//! A cm01 display on a pseudo-terminal. It answers the legacy protocol the
//! way a Panorama SE on firmware V1.0.3 does and records every request.
//!
//! The frame codec here is written independently of `tryx-legacy`, so a
//! mistake in one cannot hide the same mistake in the other.

use serde_json::{Value, json};
use serialport::{SerialPort, TTYPort};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The serial number the display reports, over USB and in `conn`.
pub const SERIAL: &str = "XYZ000000000000001";

const MARKER: u8 = 0x5A;
const ESCAPE: u8 = 0x5B;

/// One request as the display received it.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: String,
    pub command: String,
    pub body: String,
    /// The length prefix, the checksum, and `ContentLength` all agreed.
    pub well_formed: bool,
}

impl Request {
    /// The body as JSON; `Null` when it is empty or not JSON.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// How the display behaves; change it while it runs with [`FakeCm01::set`].
#[derive(Debug, Clone)]
pub struct Firmware {
    /// The reply to `conn`.
    pub identity: Value,
    /// The reply to `all`: fan readings, their health, and free storage.
    pub status: Value,
    /// Commands the display leaves unanswered.
    pub unanswered: Vec<String>,
    /// Answer nothing at all, like a display that hung.
    pub silent: bool,
}

impl Default for Firmware {
    fn default() -> Self {
        Firmware {
            identity: json!({
                "attribute": ["Status", "Water Block Screen", "Fan LCD|rw"],
                "OS": "Android",
                "productId": "cm01",
                "version": {"app": "1.0", "firmware": "V1.0.3", "hardware": "V1.1"},
                "sn": SERIAL,
            }),
            status: json!({
                "status": {"fanLCD": "1280"},
                "availableStorage": 3_111_497_728u64,
                "warning": "[{\"description\":\"No ERROR\",\"type\":\"Fan LCD\"}]",
            }),
            unanswered: Vec::new(),
            silent: false,
        }
    }
}

struct Shared {
    firmware: Mutex<Firmware>,
    requests: Mutex<Vec<Request>>,
    stop: AtomicBool,
}

pub struct FakeCm01 {
    port: PathBuf,
    /// Held so the port stays usable between clients.
    slave: TTYPort,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl Default for FakeCm01 {
    fn default() -> Self {
        FakeCm01::start()
    }
}

impl FakeCm01 {
    pub fn start() -> FakeCm01 {
        FakeCm01::with_firmware(Firmware::default())
    }

    pub fn with_firmware(firmware: Firmware) -> FakeCm01 {
        let (mut master, slave) = crate::pty::pair();
        let port = PathBuf::from(slave.name().expect("the pseudo-terminal's name"));
        master
            .set_timeout(Duration::from_millis(20))
            .expect("a read timeout");
        let shared = Arc::new(Shared {
            firmware: Mutex::new(firmware),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let worker = {
            let shared = shared.clone();
            std::thread::spawn(move || serve(master, &shared))
        };
        FakeCm01 {
            port,
            slave,
            shared,
            worker: Some(worker),
        }
    }

    /// The port's device node, for opening by path. Opening a
    /// pseudo-terminal as a serial port works on Linux only.
    pub fn port(&self) -> &Path {
        &self.port
    }

    /// Another handle on the port, for a client in the same process.
    pub fn open(&self) -> TTYPort {
        self.slave.try_clone_native().expect("a second port handle")
    }

    /// Every request so far, oldest first.
    pub fn requests(&self) -> Vec<Request> {
        self.shared.requests.lock().unwrap().clone()
    }

    /// The command names of every request so far.
    pub fn commands(&self) -> Vec<String> {
        self.requests().into_iter().map(|r| r.command).collect()
    }

    /// The requests for `command`, oldest first.
    pub fn received(&self, command: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| request.command == command)
            .collect()
    }

    /// Forgets the requests so far.
    pub fn clear(&self) {
        self.shared.requests.lock().unwrap().clear();
    }

    /// Changes how the display behaves from its next request on.
    pub fn set(&self, change: impl FnOnce(&mut Firmware)) {
        change(&mut self.shared.firmware.lock().unwrap());
    }

    /// Waits until the requests satisfy `done`; false when `timeout` passes
    /// first.
    pub fn wait_for(&self, timeout: Duration, done: impl Fn(&[Request]) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done(&self.shared.requests.lock().unwrap()) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for FakeCm01 {
    /// Closes the display's end of the port, as unplugging it would: a client
    /// still holding the port sees it fail.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(mut master: TTYPort, shared: &Shared) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    while !shared.stop.load(Ordering::Relaxed) {
        match master.read(&mut chunk) {
            Ok(0) => std::thread::sleep(Duration::from_millis(5)),
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                while let Some(raw) = take_frame(&mut buffer) {
                    if let Some(reply) = answer(shared, &raw) {
                        let _ = master.write_all(&reply);
                        let _ = master.flush();
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::Interrupted | ErrorKind::WouldBlock
                ) => {}
            // No client holds the port right now.
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// Records the request in `raw` and returns the reply to send, if any.
fn answer(shared: &Shared, raw: &[u8]) -> Option<Vec<u8>> {
    let request = parse(raw)?;
    let firmware = shared.firmware.lock().unwrap().clone();
    let command = request.command.clone();
    // Recorded before the reply goes out, so a client that has its answer
    // always finds its request recorded.
    shared.requests.lock().unwrap().push(request);
    if firmware.silent || firmware.unanswered.contains(&command) {
        return None;
    }
    let body = match command.as_str() {
        "conn" => firmware.identity.to_string(),
        "all" => firmware.status.to_string(),
        _ => String::new(),
    };
    // The device orders its headers differently from requests.
    let text = format!(
        "1 200\r\nAckNumber=0\r\nContentLength={}\r\nContentType=json\r\n\r\n{body}",
        body.len()
    );
    Some(encode(text.as_bytes()))
}

/// The wire bytes of a frame carrying `text`.
pub fn encode(text: &[u8]) -> Vec<u8> {
    let length = u16::try_from(text.len() + 5).expect("a frame under 64 KiB");
    let mut raw = length.to_be_bytes().to_vec();
    raw.extend_from_slice(text);
    raw.push(raw.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    let mut wire = vec![MARKER];
    for byte in raw {
        match byte {
            MARKER => wire.extend([ESCAPE, 0x01]),
            ESCAPE => wire.extend([ESCAPE, 0x02]),
            other => wire.push(other),
        }
    }
    wire.push(MARKER);
    wire
}

/// Removes the first delimited frame from `buffer` and returns its contents
/// unescaped: the length prefix, the text, and the checksum.
fn take_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buffer.iter().position(|&byte| byte == MARKER)?;
    let end = start + 1 + buffer[start + 1..].iter().position(|&b| b == MARKER)?;
    let inner: Vec<u8> = buffer.drain(..=end).skip(start + 1).collect();
    let mut raw = Vec::with_capacity(inner.len());
    let mut bytes = inner[..inner.len() - 1].iter().copied();
    while let Some(byte) = bytes.next() {
        if byte != ESCAPE {
            raw.push(byte);
            continue;
        }
        match bytes.next() {
            Some(0x01) => raw.push(MARKER),
            Some(0x02) => raw.push(ESCAPE),
            Some(other) => raw.extend([ESCAPE, other]),
            None => raw.push(ESCAPE),
        }
    }
    Some(raw)
}

fn parse(raw: &[u8]) -> Option<Request> {
    if raw.len() < 3 {
        return None;
    }
    let (payload, checksum) = raw.split_at(raw.len() - 1);
    let declared = usize::from(u16::from_be_bytes([payload[0], payload[1]]));
    let text = &payload[2..];
    let sum = payload
        .iter()
        .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
    let text_str = String::from_utf8_lossy(text);
    let (head, body) = text_str.split_once("\r\n\r\n")?;
    let mut lines = head.split("\r\n");
    let mut words = lines.next()?.split(' ');
    let method = words.next()?.to_string();
    let command = words.next()?.to_string();
    let content_length = lines
        .find_map(|line| line.strip_prefix("ContentLength="))
        .and_then(|value| value.parse::<usize>().ok());
    Some(Request {
        method,
        command,
        body: body.to_string(),
        well_formed: sum == checksum[0]
            && declared == text.len() + 5
            && content_length == Some(body.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_through_the_codec() {
        let text = b"POST mediaDelete 1\r\nContentType=json\r\nContentLength=21\r\nAckNumber=7\r\n\r\n{\"include\":[\"Z.mp4\"]}";
        let mut buffer = vec![0x00, 0x11];
        buffer.extend(encode(text));
        buffer.extend(encode(b"POST conn 1\r\nContentLength=0\r\n\r\n"));
        let first = take_frame(&mut buffer).unwrap();
        let request = parse(&first).unwrap();
        assert_eq!(request.command, "mediaDelete");
        assert_eq!(request.json()["include"][0], "Z.mp4");
        assert!(request.well_formed);
        let second = parse(&take_frame(&mut buffer).unwrap()).unwrap();
        assert_eq!(second.command, "conn");
        assert!(buffer.is_empty());
    }
}
