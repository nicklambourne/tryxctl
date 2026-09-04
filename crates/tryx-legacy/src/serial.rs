//! The CDC ACM command channel: 115200 baud, 8N1, raw, no flow control.

use crate::LegacyError;
use crate::frame::{self, Response};
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

pub const BAUD_RATE: u32 = 115_200;
/// How long a reply may take. Upstream pauses 100 ms and then reads for
/// 500 ms; a single deadline is simpler and more tolerant.
pub const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_millis(1000);
const READ_SLICE: Duration = Duration::from_millis(50);
/// Longest wait for stale input to stop arriving before a request.
const DRAIN_WINDOW: Duration = Duration::from_millis(150);

pub struct SerialLink {
    port: Box<dyn serialport::SerialPort>,
    path: String,
    sequence: u32,
    pub response_timeout: Duration,
    /// Dump every frame on the wire to stderr as hex plus decoded text.
    pub trace: bool,
}

impl SerialLink {
    pub fn open(path: &str) -> Result<Self, LegacyError> {
        let serial_error = |source| LegacyError::Serial {
            path: path.to_string(),
            source,
        };
        let port = serialport::new(path, BAUD_RATE)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .flow_control(serialport::FlowControl::None)
            .timeout(READ_SLICE)
            .open()
            .map_err(serial_error)?;
        port.clear(serialport::ClearBuffer::All)
            .map_err(serial_error)?;
        Ok(Self::from_port(port, path))
    }

    /// Wraps an already open port, for example one half of a pty pair.
    pub fn from_port(mut port: Box<dyn serialport::SerialPort>, path: &str) -> Self {
        // Ignore a refused timeout: reads then block at most for the port's
        // own timeout, and the deadline loop still bounds the wait.
        let _ = port.set_timeout(READ_SLICE);
        SerialLink {
            port,
            path: path.to_string(),
            sequence: 0,
            response_timeout: DEFAULT_RESPONSE_TIMEOUT,
            trace: false,
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Sends `POST <command>` and waits for the reply.
    pub fn request(&mut self, command: &str, content: &str) -> Result<Response, LegacyError> {
        self.send(command, content)?;
        self.read_response(command)
    }

    /// Discards whatever the device sent that nobody read: replies to
    /// fire-and-forget commands, or a late answer to a previous process.
    /// Replies carry no correlation field, so a stale frame would otherwise
    /// be taken as the answer to the next request and shift every reply by
    /// one for the life of the link.
    pub fn drain(&mut self) -> usize {
        let deadline = Instant::now() + DRAIN_WINDOW;
        let mut discarded = 0;
        let mut chunk = [0u8; 256];
        while Instant::now() < deadline {
            match self.port.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => discarded += count,
                Err(error) if error.kind() == ErrorKind::TimedOut => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if discarded > 0 && self.trace {
            eprintln!("   discarded {discarded} stale bytes before sending");
        }
        discarded
    }

    /// Sends `POST <command>` without waiting for a reply.
    pub fn send(&mut self, command: &str, content: &str) -> Result<(), LegacyError> {
        self.drain();
        self.sequence += 1;
        let bytes = frame::build_frame("POST", command, content, "1", self.sequence)?;
        if self.trace {
            eprintln!(
                "-> {command} #{} {} bytes\n   {}",
                self.sequence,
                bytes.len(),
                hex(&bytes)
            );
        }
        self.port.write_all(&bytes)?;
        self.port.flush()?;
        Ok(())
    }

    fn read_response(&mut self, command: &str) -> Result<Response, LegacyError> {
        let deadline = Instant::now() + self.response_timeout;
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 256];
        while Instant::now() < deadline {
            match self.port.read(&mut chunk) {
                Ok(0) => {}
                Ok(count) => {
                    buffer.extend_from_slice(&chunk[..count]);
                    if let Some(bytes) = frame::take_frame(&mut buffer) {
                        let response = frame::parse_response(&bytes);
                        if self.trace {
                            eprintln!("<- {} bytes\n   {}", bytes.len(), hex(&bytes));
                            if let Some(response) = &response {
                                eprintln!(
                                    "   text: {:?} (checksum {}, length {})",
                                    response.raw,
                                    if response.checksum_ok {
                                        "ok"
                                    } else {
                                        "MISMATCH"
                                    },
                                    if response.length_ok { "ok" } else { "MISMATCH" },
                                );
                            }
                        }
                        return response.ok_or_else(|| LegacyError::MalformedResponse {
                            command: command.to_string(),
                        });
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(LegacyError::NoResponse {
            command: command.to_string(),
            timeout_ms: self.response_timeout.as_millis() as u64,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serialport::{SerialPort, TTYPort};

    fn pair() -> (TTYPort, SerialLink) {
        let (mut master, slave) = TTYPort::pair().expect("pty pair");
        master
            .set_timeout(Duration::from_millis(2000))
            .expect("master timeout");
        (master, SerialLink::from_port(Box::new(slave), "pty"))
    }

    #[test]
    fn request_round_trips_through_a_pty_pair() {
        let (mut device, mut link) = pair();
        let device_side = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 256];
            let request = loop {
                let count = device.read(&mut chunk).expect("device read");
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(bytes) = frame::take_frame(&mut buffer) {
                    break frame::parse_response(&bytes).expect("request parses");
                }
            };
            let reply = frame::wrap(b"1 OK\r\nContentType=json\r\n\r\n{\"productId\":\"cm01_se\"}")
                .unwrap();
            device.write_all(&reply).expect("device write");
            // Hand the master back: on Linux, closing it discards whatever
            // the slave has not read yet.
            (request, device)
        });

        let response = link.request("conn", "").expect("response");
        assert_eq!(response.status, "OK");
        assert_eq!(response.json.unwrap()["productId"], "cm01_se");

        let (request, _device) = device_side.join().unwrap();
        assert_eq!(request.version, "POST");
        assert_eq!(request.status, "conn");
        assert!(request.raw.contains("AckNumber=1\r\n"), "{}", request.raw);
        assert!(request.checksum_ok);
    }

    /// macOS's `poll()` reports a pty slave readable when it is not, so a
    /// read with no peer data blocks forever there. Real serial devices are
    /// unaffected; the test runs where ptys behave.
    #[test]
    #[cfg(target_os = "linux")]
    fn request_times_out_without_a_reply() {
        let (_device, mut link) = pair();
        link.response_timeout = Duration::from_millis(150);
        let started = Instant::now();
        let error = link.request("conn", "").unwrap_err();
        assert!(matches!(error, LegacyError::NoResponse { .. }), "{error}");
        assert!(started.elapsed() >= Duration::from_millis(150));
    }

    #[test]
    fn a_stale_reply_is_discarded_before_the_next_request() {
        let (mut device, mut link) = pair();
        // A reply nobody read, as after a fire-and-forget command.
        let stale =
            frame::wrap(b"1 200\r\nContentType=json\r\n\r\n{\"status\":{\"fanLCD\":\"0\"}}")
                .unwrap();
        device.write_all(&stale).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let device_side = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 256];
            let request = loop {
                let count = device.read(&mut chunk).expect("device read");
                buffer.extend_from_slice(&chunk[..count]);
                if let Some(bytes) = frame::take_frame(&mut buffer) {
                    break frame::parse_response(&bytes).expect("request parses");
                }
            };
            let reply =
                frame::wrap(b"1 200\r\nContentType=json\r\n\r\n{\"productId\":\"cm01\"}").unwrap();
            device.write_all(&reply).unwrap();
            (request, device)
        });
        let response = link.request("conn", "").expect("response");
        assert_eq!(
            response.json.unwrap()["productId"],
            "cm01",
            "got the fresh reply, not the stale one"
        );
        let (request, _device) = device_side.join().unwrap();
        assert_eq!(request.status, "conn");
    }
}
