//! The socket between the daemon and everything else. Frames are a 4-byte
//! little-endian length followed by JSON.

use crate::exit::Failure;
use crate::legacy::{Info, Protocol};
use crate::state::DisplayState;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;
use tryx_device::Product;
use tryx_legacy::{FanStatus, ScreenConfig};
use tryx_monitor::Sample;

const MAX_FRAME: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Request {
    Status,
    Info,
    Apply {
        state: Box<DisplayState>,
    },
    Brightness {
        value: u8,
    },
    DeleteMedia {
        names: Vec<String>,
    },
    FanLcd {
        percent: u8,
    },
    Reboot,
    Raw {
        command: String,
        body: String,
        wait: bool,
    },
    /// KANALI only: the media on the display.
    Catalog,
    /// KANALI only: send a prepared local file as `name`.
    Upload {
        path: PathBuf,
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub value: serde_json::Value,
}

impl Reply {
    pub fn ok(value: impl Serialize) -> Reply {
        Reply {
            ok: true,
            error: None,
            value: serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn error(message: impl Into<String>) -> Reply {
        Reply {
            ok: false,
            error: Some(message.into()),
            value: serde_json::Value::Null,
        }
    }
}

/// What the daemon knows right now.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub protocol: Protocol,
    pub product: Option<Product>,
    /// The port or USB id the daemon owns.
    pub tty: String,
    pub started_unix: i64,
    pub interval: u64,
    pub pushes: u64,
    pub info: Option<Info>,
    pub sample: Option<Sample>,
    pub fans: FanStatus,
    pub screen: ScreenConfig,
    pub last_error: Option<String>,
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| std::env::temp_dir().join(format!("tryxctl-{}", uid())));
    dir.join("tryxctl").join("daemon.sock")
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions.
        unsafe { libc_getuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

pub fn write_frame(stream: &mut impl Write, bytes: &[u8]) -> std::io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()
}

pub fn read_frame(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut bytes = vec![0u8; len];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// Whether a daemon is listening.
pub fn available() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// The daemon's status, or `None` when no daemon answers.
pub fn status() -> Result<Option<DaemonStatus>, Failure> {
    match call(&Request::Status)? {
        Some(reply) if reply.ok => Ok(serde_json::from_value(reply.value).ok()),
        _ => Ok(None),
    }
}

/// Sends one request. `Ok(None)` means no daemon is listening.
pub fn call(request: &Request) -> Result<Option<Reply>, Failure> {
    call_with_timeout(request, Duration::from_secs(60))
}

/// Like [`call`], waiting up to `timeout` for the reply.
pub fn call_with_timeout(request: &Request, timeout: Duration) -> Result<Option<Reply>, Failure> {
    let Ok(mut stream) = UnixStream::connect(socket_path()) else {
        return Ok(None);
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let bytes = serde_json::to_vec(request)?;
    write_frame(&mut stream, &bytes).map_err(|e| Failure::device(format!("daemon socket: {e}")))?;
    let reply =
        read_frame(&mut stream).map_err(|e| Failure::device(format!("daemon socket: {e}")))?;
    let reply: Reply = serde_json::from_slice(&reply)?;
    Ok(Some(reply))
}

/// Sends one request and turns a daemon-side error into a device failure.
pub fn expect(request: &Request) -> Result<Reply, Failure> {
    expect_with_timeout(request, Duration::from_secs(60))
}

pub fn expect_with_timeout(request: &Request, timeout: Duration) -> Result<Reply, Failure> {
    match call_with_timeout(request, timeout)? {
        Some(reply) if reply.ok => Ok(reply),
        Some(reply) => Err(Failure::device(
            reply.error.unwrap_or_else(|| "daemon error".into()),
        )),
        None => Err(Failure::device("the daemon is not running")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn frames_round_trip_over_a_socket() {
        let dir = std::env::temp_dir().join(format!("tryxctl-ipc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request: Request =
                serde_json::from_slice(&read_frame(&mut stream).unwrap()).unwrap();
            let reply = match request {
                Request::Brightness { value } => {
                    Reply::ok(serde_json::json!({"brightness": value}))
                }
                _ => Reply::error("unexpected"),
            };
            write_frame(&mut stream, &serde_json::to_vec(&reply).unwrap()).unwrap();
        });
        let mut client = UnixStream::connect(&path).unwrap();
        write_frame(
            &mut client,
            &serde_json::to_vec(&Request::Brightness { value: 42 }).unwrap(),
        )
        .unwrap();
        let reply: Reply = serde_json::from_slice(&read_frame(&mut client).unwrap()).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.value["brightness"], 42);
        server.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn requests_serialise_with_a_type_tag() {
        let text = serde_json::to_string(&Request::Raw {
            command: "conn".into(),
            body: String::new(),
            wait: true,
        })
        .unwrap();
        assert!(text.contains(r#""type":"raw""#), "{text}");
        let back: Request = serde_json::from_str(&text).unwrap();
        assert!(matches!(back, Request::Raw { ref command, .. } if command == "conn"));
    }
}
