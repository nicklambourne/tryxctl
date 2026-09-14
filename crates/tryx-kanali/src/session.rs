//! Framed request/response exchange with track ids.
//!
//! Tracked requests carry a random `track_id`; the matching reply echoes it
//! and carries the expected body. Bootstrap replies carry track id zero.
//! Unrelated frames (pongs, asynchronous events, stale tracked replies)
//! are skipped within bounds, as upstream does.

use crate::transport::{Pipe, TransportError};
use crate::{KanaliError, encode_request};
use prost::Message;
use std::time::{Duration, Instant};
use tryx_proto::frame;
use tryx_proto::wire::v1 as wire;
use wire::{request, response};

const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(5);
const FILE_TRANSMIT_WRITE_TIMEOUT: Duration = Duration::from_secs(15);
const FILE_TRANSMIT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const BOOTSTRAP_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const READ_SLICE: Duration = Duration::from_millis(200);
const MAX_SKIPPED_FRAMES: usize = 256;
const MAX_SKIPPED_BYTES: usize = 4 * 1024 * 1024;
const READ_BUFFER: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Pong,
    DeviceInformation,
    DeviceAuthentication,
    SystemConfiguration,
    MediaCatalog,
    UserConfiguration,
    Acknowledgement,
    TransferBeginStatus,
    TransferChunkStatus,
    TransferEndStatus,
    MediaReadChunk,
}

impl Expect {
    fn matches(self, body: &Option<response::Body>) -> bool {
        matches!(
            (self, body),
            (Expect::Pong, Some(response::Body::Pong(_)))
                | (
                    Expect::DeviceInformation,
                    Some(response::Body::DeviceInformation(_))
                )
                | (
                    Expect::DeviceAuthentication,
                    Some(response::Body::DeviceAuthentication(_))
                )
                | (
                    Expect::SystemConfiguration,
                    Some(response::Body::SystemConfiguration(_))
                )
                | (Expect::MediaCatalog, Some(response::Body::MediaCatalog(_)))
                | (
                    Expect::UserConfiguration,
                    Some(response::Body::UserConfiguration(_))
                )
                | (
                    Expect::Acknowledgement,
                    Some(response::Body::Acknowledgement(_))
                )
                | (
                    Expect::TransferBeginStatus,
                    Some(response::Body::TransferBeginStatus(_))
                )
                | (
                    Expect::TransferChunkStatus,
                    Some(response::Body::TransferChunkStatus(_))
                )
                | (
                    Expect::TransferEndStatus,
                    Some(response::Body::TransferEndStatus(_))
                )
                | (
                    Expect::MediaReadChunk,
                    Some(response::Body::MediaReadChunk(_))
                )
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Normal,
    FileTransmit,
}

pub struct Session {
    pipe: Box<dyn Pipe>,
    pending: Vec<u8>,
    track: u64,
    pub trace: bool,
}

fn request_name(request: &wire::Request) -> &'static str {
    match &request.body {
        Some(request::Body::Ping(_)) => "ping",
        Some(request::Body::DeviceInformationQuery(_)) => "device_information_query",
        Some(request::Body::DeviceAuthenticationQuery(_)) => "device_authentication_query",
        Some(request::Body::SystemConfigurationQuery(_)) => "system_configuration_query",
        Some(request::Body::MediaCatalogQuery(_)) => "media_catalog_query",
        Some(request::Body::UserConfigurationQuery(_)) => "user_configuration_query",
        Some(request::Body::UserConfiguration(_)) => "user_configuration",
        Some(request::Body::OverlayLayout(_)) => "overlay_layout",
        Some(request::Body::MetricBatch(_)) => "metric_batch",
        Some(request::Body::TransferBegin(_)) => "transfer_begin",
        Some(request::Body::TransferChunk(_)) => "transfer_chunk",
        Some(request::Body::TransferEnd(_)) => "transfer_end",
        Some(request::Body::FileRemoval(_)) => "file_removal",
        Some(request::Body::MediaReadChunk(_)) => "media_read_chunk",
        None => "empty",
    }
}

impl Session {
    pub fn new(pipe: Box<dyn Pipe>) -> Session {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
            ^ (std::process::id() as u64) << 32;
        Session {
            pipe,
            pending: Vec::new(),
            track: seed | 1,
            trace: false,
        }
    }

    fn next_track(&mut self) -> u64 {
        // xorshift64*: never zero, which bootstrap replies reserve.
        let mut x = self.track;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.track = x;
        let id = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        if id == 0 { 1 } else { id }
    }

    fn write_all(&mut self, bytes: &[u8], timeout: Duration) -> Result<(), KanaliError> {
        let deadline = Instant::now() + timeout;
        let mut written = 0;
        while written < bytes.len() {
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(1));
            written += self.pipe.write(&bytes[written..], remaining)?;
        }
        Ok(())
    }

    /// One complete frame's payload, or `Timeout`.
    fn read_frame(&mut self, timeout: Duration) -> Result<Vec<u8>, KanaliError> {
        let deadline = Instant::now() + timeout;
        let mut chunk = vec![0u8; READ_BUFFER];
        loop {
            // Garbage goes first: the codec clears the whole buffer at a bad
            // header, which would also lose a good frame read along with it.
            let dropped = frame::discard_bytes_before_plausible_frame(&mut self.pending);
            if dropped > 0 && self.trace {
                eprintln!("   dropping {dropped} bytes of malformed input");
            }
            match frame::take_frame(&mut self.pending) {
                Ok(Some(payload)) => return Ok(payload),
                Ok(None) => {}
                Err(malformed) => {
                    if self.trace {
                        eprintln!("   dropping malformed input: {malformed}");
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(TransportError::Timeout.into());
            }
            match self.pipe.read(&mut chunk, remaining.min(READ_SLICE)) {
                Ok(count) => self.pending.extend_from_slice(&chunk[..count]),
                Err(TransportError::Timeout) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }

    /// Discards frames that arrive within `window`.
    pub fn drain(&mut self, window: Duration) -> usize {
        let deadline = Instant::now() + window;
        let mut discarded = 0;
        let mut chunk = vec![0u8; READ_BUFFER];
        while Instant::now() < deadline {
            match self.pipe.read(
                &mut chunk,
                deadline
                    .saturating_duration_since(Instant::now())
                    .max(Duration::from_millis(1)),
            ) {
                Ok(count) => discarded += count,
                Err(_) => break,
            }
        }
        self.pending.clear();
        discarded
    }

    fn stamp(&mut self, request: &mut wire::Request, fixed_track: Option<u64>) -> u64 {
        let track = fixed_track.unwrap_or_else(|| self.next_track());
        request.header = Some(wire::WireHeader {
            version: 1,
            track_id: track,
            payload_crc32: 0,
        });
        track
    }

    /// Writes a request with a header and no wait for the reply.
    pub fn write_only(&mut self, request: &wire::Request) -> Result<(), KanaliError> {
        let bytes = encode_request(request)?;
        if self.trace {
            eprintln!(
                "-> {} ({} bytes, untracked)",
                request_name(request),
                bytes.len()
            );
        }
        self.write_all(&bytes, TRANSACTION_TIMEOUT)
    }

    /// Writes a tracked request without waiting for its acknowledgement.
    pub fn write_tracked_only(&mut self, request: &mut wire::Request) -> Result<u64, KanaliError> {
        let track = self.stamp(request, None);
        let bytes = encode_request(request)?;
        if self.trace {
            eprintln!(
                "-> {} ({} bytes, track {track}, no wait)",
                request_name(request),
                bytes.len()
            );
        }
        self.write_all(&bytes, TRANSACTION_TIMEOUT)?;
        Ok(track)
    }

    /// Sends a tracked request and returns the matching reply.
    pub fn execute(
        &mut self,
        request: &mut wire::Request,
        expected: Expect,
        profile: Profile,
        fixed_track: Option<u64>,
    ) -> Result<wire::Response, KanaliError> {
        let name = request_name(request);
        let track = self.stamp(request, fixed_track);
        let bytes = encode_request(request)?;
        let (write_timeout, response_timeout) = match profile {
            Profile::Normal => (TRANSACTION_TIMEOUT, TRANSACTION_TIMEOUT),
            Profile::FileTransmit => (FILE_TRANSMIT_WRITE_TIMEOUT, FILE_TRANSMIT_RESPONSE_TIMEOUT),
        };
        self.drain(Duration::from_millis(20));
        if self.trace {
            eprintln!("-> {name} ({} bytes, track {track})", bytes.len());
        }
        self.write_all(&bytes, write_timeout)?;

        let deadline = Instant::now() + response_timeout;
        let mut skipped_frames = 0;
        let mut skipped_bytes = 0;
        while skipped_frames <= MAX_SKIPPED_FRAMES && skipped_bytes <= MAX_SKIPPED_BYTES {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(KanaliError::NoResponse {
                    request: name,
                    timeout_ms: response_timeout.as_millis() as u64,
                });
            }
            let payload = match self.read_frame(remaining) {
                Ok(payload) => payload,
                Err(KanaliError::Transport(TransportError::Timeout)) => {
                    return Err(KanaliError::NoResponse {
                        request: name,
                        timeout_ms: response_timeout.as_millis() as u64,
                    });
                }
                Err(error) => return Err(error),
            };
            let Ok(parsed) = wire::Response::decode(payload.as_slice()) else {
                return Err(KanaliError::InvalidResponse {
                    request: name,
                    detail: "reply is not a protobuf response".into(),
                });
            };
            if self.trace {
                eprintln!("<- {} bytes: {}", payload.len(), describe(&parsed));
            }
            let is_event = matches!(parsed.body, Some(response::Body::AsynchronousEvent(_)));
            let is_stray_pong =
                matches!(parsed.body, Some(response::Body::Pong(_))) && expected != Expect::Pong;
            if is_event || is_stray_pong {
                skipped_frames += 1;
                skipped_bytes += payload.len() + 8;
                continue;
            }
            let Some(header) = &parsed.header else {
                return Err(KanaliError::InvalidResponse {
                    request: name,
                    detail: "tracked reply without a header".into(),
                });
            };
            if header.track_id != track {
                skipped_frames += 1;
                skipped_bytes += payload.len() + 8;
                continue;
            }
            if let Some(error) = &parsed.error
                && error.code != wire::protocol_error::Code::Success as i32
            {
                return Err(KanaliError::Rejected {
                    request: name,
                    why: error.why.clone(),
                });
            }
            if !expected.matches(&parsed.body) {
                return Err(KanaliError::InvalidResponse {
                    request: name,
                    detail: describe(&parsed),
                });
            }
            return Ok(parsed);
        }
        Err(KanaliError::InvalidResponse {
            request: name,
            detail: "too many unrelated frames".into(),
        })
    }

    /// The readiness loop: `device_information_query` until an exact
    /// `DeviceInformation` arrives, retrying only on a confirmed unsent
    /// request, then the one-shot system and authentication queries.
    pub fn bootstrap(&mut self) -> Result<wire::DeviceInformation, KanaliError> {
        let deadline = Instant::now() + crate::READINESS_DEADLINE;
        let mut backoff = Duration::from_millis(500);
        let info = loop {
            let request = wire::Request {
                header: Some(wire::WireHeader {
                    version: 1,
                    ..Default::default()
                }),
                body: Some(request::Body::DeviceInformationQuery(wire::QueryToken {
                    dummy: "NA".into(),
                })),
            };
            let bytes = encode_request(&request)?;
            if self.trace {
                eprintln!(
                    "-> device_information_query ({} bytes, bootstrap)",
                    bytes.len()
                );
            }
            match self.pipe.write(&bytes, BOOTSTRAP_WRITE_TIMEOUT) {
                Ok(0) | Err(TransportError::Timeout) => {
                    if Instant::now() + backoff >= deadline {
                        return Err(KanaliError::NoResponse {
                            request: "device_information_query",
                            timeout_ms: crate::READINESS_DEADLINE.as_millis() as u64,
                        });
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(2));
                    continue;
                }
                Ok(written) if written < bytes.len() => {
                    self.write_all(&bytes[written..], BOOTSTRAP_WRITE_TIMEOUT)?;
                }
                Ok(_) => {}
                Err(error) => return Err(error.into()),
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .max(Duration::from_secs(1));
            match self.read_bootstrap(Expect::DeviceInformation, remaining)? {
                Some(response::Body::DeviceInformation(info)) => break info,
                _ => {
                    return Err(KanaliError::InvalidResponse {
                        request: "device_information_query",
                        detail: "no device information".into(),
                    });
                }
            }
        };
        for (body, expected, name) in [
            (
                request::Body::SystemConfigurationQuery(wire::QueryToken { dummy: "NA".into() }),
                Expect::SystemConfiguration,
                "system_configuration_query",
            ),
            (
                request::Body::DeviceAuthenticationQuery(wire::DeviceAuthQuery { key: 1 }),
                Expect::DeviceAuthentication,
                "device_authentication_query",
            ),
        ] {
            let request = wire::Request {
                header: Some(wire::WireHeader {
                    version: 1,
                    ..Default::default()
                }),
                body: Some(body),
            };
            let bytes = encode_request(&request)?;
            if self.trace {
                eprintln!("-> {name} ({} bytes, bootstrap)", bytes.len());
            }
            self.write_all(&bytes, BOOTSTRAP_WRITE_TIMEOUT)?;
            let _ = self.read_bootstrap(expected, TRANSACTION_TIMEOUT);
        }
        Ok(info)
    }

    fn read_bootstrap(
        &mut self,
        expected: Expect,
        timeout: Duration,
    ) -> Result<Option<response::Body>, KanaliError> {
        let deadline = Instant::now() + timeout;
        let mut skipped_frames = 0;
        let mut skipped_bytes = 0;
        while skipped_frames <= MAX_SKIPPED_FRAMES && skipped_bytes <= MAX_SKIPPED_BYTES {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(KanaliError::NoResponse {
                    request: "bootstrap",
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            let payload = match self.read_frame(remaining) {
                Ok(payload) => payload,
                Err(KanaliError::Transport(TransportError::Timeout)) => {
                    return Err(KanaliError::NoResponse {
                        request: "bootstrap",
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
                Err(error) => return Err(error),
            };
            let Ok(parsed) = wire::Response::decode(payload.as_slice()) else {
                return Err(KanaliError::InvalidResponse {
                    request: "bootstrap",
                    detail: "reply is not a protobuf response".into(),
                });
            };
            if self.trace {
                eprintln!("<- {} bytes: {}", payload.len(), describe(&parsed));
            }
            let bootstrap_header = parsed
                .header
                .as_ref()
                .is_some_and(|h| h.version == 1 && h.track_id == 0 && h.payload_crc32 == 0);
            if bootstrap_header && expected.matches(&parsed.body) {
                return Ok(parsed.body);
            }
            skipped_frames += 1;
            skipped_bytes += payload.len() + 8;
        }
        Err(KanaliError::InvalidResponse {
            request: "bootstrap",
            detail: "too many unrelated frames".into(),
        })
    }
}

fn describe(response: &wire::Response) -> String {
    let body = match &response.body {
        Some(response::Body::Pong(_)) => "pong",
        Some(response::Body::DeviceInformation(_)) => "device_information",
        Some(response::Body::DeviceAuthentication(_)) => "device_authentication",
        Some(response::Body::SystemConfiguration(_)) => "system_configuration",
        Some(response::Body::MediaCatalog(_)) => "media_catalog",
        Some(response::Body::UserConfiguration(_)) => "user_configuration",
        Some(response::Body::Acknowledgement(_)) => "acknowledgement",
        Some(response::Body::TransferBeginStatus(_)) => "transfer_begin_status",
        Some(response::Body::TransferChunkStatus(_)) => "transfer_chunk_status",
        Some(response::Body::TransferEndStatus(_)) => "transfer_end_status",
        Some(response::Body::MediaReadChunk(_)) => "media_read_chunk",
        Some(response::Body::AsynchronousEvent(_)) => "asynchronous_event",
        None => "no body",
    };
    let track = response.header.as_ref().map(|h| h.track_id).unwrap_or(0);
    let error = response
        .error
        .as_ref()
        .filter(|e| e.code != 0)
        .map(|e| format!(", error {} {:?}", e.code, e.why))
        .unwrap_or_default();
    format!("{body} (track {track}{error})")
}
