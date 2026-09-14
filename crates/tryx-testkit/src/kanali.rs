//! A KANALI display on a Unix socket in the sandbox's device tree, which
//! tryx-kanali connects to in place of the USB pipe. It speaks the framed
//! protobuf protocol like the firmware, keeps its configuration and files
//! from one connection to the next, and records what it received.

use prost::Message;
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tryx_proto::frame;
use tryx_proto::wire::v1 as wire;
use wire::{request, response};

/// USB product id of the Panorama SE.
pub const PANORAMA_SE: u16 = 0x1021;
/// USB product id of the Turris 620, which takes uploads and nothing else.
pub const TURRIS_620: u16 = 0x2011;
/// The serial number the display reports.
pub const SERIAL: &str = "KNL0000000000001";
/// The factory animation every display ships with.
pub const PRESET: &str = "default_01.mp4.h264_2240x1080";

/// What the display holds and has been sent; see [`FakeKanali::with`].
#[derive(Debug, Clone)]
pub struct Display {
    pub config: wire::UserConfiguration,
    /// User files by name, with the bytes received for each.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Request names in arrival order, such as `user_configuration`.
    pub received: Vec<&'static str>,
    /// Every overlay layout, tracked or leased, in arrival order.
    pub layouts: Vec<wire::OverlayLayout>,
    pub metrics: Vec<wire::MetricBatch>,
    /// The track id of every transfer begun.
    pub transfer_tracks: Vec<u64>,
    /// The status transfers end with: 0 accepts them, 1 is out of space.
    pub transfer_status: i32,
}

struct Shared {
    display: Mutex<Display>,
    stop: AtomicBool,
    clients: Mutex<Vec<JoinHandle<()>>>,
}

pub struct FakeKanali {
    socket: PathBuf,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl FakeKanali {
    /// A display answering at `socket`, as [`crate::Sandbox::plug_kanali`]
    /// returns it.
    pub fn start(socket: &Path) -> FakeKanali {
        let _ = std::fs::remove_file(socket);
        let listener = UnixListener::bind(socket).expect("the display's socket");
        listener
            .set_nonblocking(true)
            .expect("a non-blocking listener");
        let config = wire::UserConfiguration {
            display_config: Some(wire::DisplayConfiguration {
                backlight_enable: true,
                backlight_brightness: 60,
                ..Default::default()
            }),
            work_config: Some(wire::WorkConfiguration {
                single_mode_media_file: PRESET.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let shared = Arc::new(Shared {
            display: Mutex::new(Display {
                config,
                files: BTreeMap::new(),
                received: Vec::new(),
                layouts: Vec::new(),
                metrics: Vec::new(),
                transfer_tracks: Vec::new(),
                transfer_status: 0,
            }),
            stop: AtomicBool::new(false),
            clients: Mutex::new(Vec::new()),
        });
        let accept = {
            let shared = shared.clone();
            std::thread::spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let client = {
                                let shared = shared.clone();
                                std::thread::spawn(move || serve(stream, &shared))
                            };
                            shared.clients.lock().unwrap().push(client);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            })
        };
        FakeKanali {
            socket: socket.to_path_buf(),
            shared,
            accept: Some(accept),
        }
    }

    /// Reads or changes the display's state.
    pub fn with<R>(&self, f: impl FnOnce(&mut Display) -> R) -> R {
        f(&mut self.shared.display.lock().unwrap())
    }

    /// The request names so far.
    pub fn received(&self) -> Vec<&'static str> {
        self.with(|display| display.received.clone())
    }

    /// How many requests named `name` arrived.
    pub fn count(&self, name: &str) -> usize {
        self.with(|display| display.received.iter().filter(|r| **r == name).count())
    }

    /// Waits until the display's state satisfies `done`; false on timeout.
    pub fn wait_for(&self, timeout: Duration, done: impl Fn(&Display) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.with(|display| done(display)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for FakeKanali {
    /// Unplugs the display: every connection closes and new ones are refused.
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        for client in self.shared.clients.lock().unwrap().drain(..) {
            let _ = client.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn serve(mut stream: UnixStream, shared: &Shared) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut pending = Vec::new();
    let mut chunk = vec![0u8; 1 << 16];
    let mut inflight: Option<(String, usize, Vec<u8>)> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        loop {
            match frame::take_frame(&mut pending) {
                Ok(Some(payload)) => {
                    let Ok(request) = wire::Request::decode(payload.as_slice()) else {
                        return;
                    };
                    handle(&mut stream, shared, &mut inflight, request);
                }
                Ok(None) => break,
                Err(_) => return,
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(count) => pending.extend_from_slice(&chunk[..count]),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
    }
}

fn send(stream: &mut UnixStream, header: Option<wire::WireHeader>, body: response::Body) {
    let response = wire::Response {
        header,
        error: None,
        body: Some(body),
    };
    if let Some(bytes) = frame::encode(&response.encode_to_vec()) {
        let _ = stream.write_all(&bytes);
    }
}

fn header(track: u64) -> Option<wire::WireHeader> {
    Some(wire::WireHeader {
        version: 1,
        track_id: track,
        payload_crc32: 0,
    })
}

fn acknowledgement() -> response::Body {
    response::Body::Acknowledgement(wire::Acknowledgement {
        dummy: String::new(),
    })
}

fn status(status: i32) -> wire::TransferStatus {
    wire::TransferStatus { status }
}

fn handle(
    stream: &mut UnixStream,
    shared: &Shared,
    inflight: &mut Option<(String, usize, Vec<u8>)>,
    request: wire::Request,
) {
    let track = request.header.as_ref().map(|h| h.track_id).unwrap_or(0);
    let Some(body) = request.body else { return };
    let name = match &body {
        request::Body::Ping(_) => "ping",
        request::Body::DeviceInformationQuery(_) => "device_information_query",
        request::Body::SystemConfigurationQuery(_) => "system_configuration_query",
        request::Body::DeviceAuthenticationQuery(_) => "device_authentication_query",
        request::Body::MediaCatalogQuery(_) => "media_catalog_query",
        request::Body::UserConfigurationQuery(_) => "user_configuration_query",
        request::Body::UserConfiguration(_) => "user_configuration",
        request::Body::OverlayLayout(_) => "overlay_layout",
        request::Body::MetricBatch(_) => "metric_batch",
        request::Body::TransferBegin(_) => "transfer_begin",
        request::Body::TransferChunk(_) => "transfer_chunk",
        request::Body::TransferEnd(_) => "transfer_end",
        request::Body::FileRemoval(_) => "file_removal",
        request::Body::MediaReadChunk(_) => "media_read_chunk",
    };
    let mut display = shared.display.lock().unwrap();
    display.received.push(name);
    let (reply_header, reply) = match body {
        request::Body::Ping(ping) => (
            None,
            response::Body::Pong(wire::Pong {
                payload: ping.payload,
            }),
        ),
        request::Body::DeviceInformationQuery(_) => (
            header(0),
            response::Body::DeviceInformation(wire::DeviceInformation {
                os_name: "Linux".into(),
                os_version: "5.10".into(),
                firmware_version: "2.3.1".into(),
                product_name: "Panorama SE".into(),
                app_version: "1.4".into(),
                serial_number: SERIAL.into(),
                serial_number_locked: true,
                chip_id: "rk3568".into(),
            }),
        ),
        request::Body::SystemConfigurationQuery(_) => (
            header(0),
            response::Body::SystemConfiguration(wire::SystemConfiguration {}),
        ),
        request::Body::DeviceAuthenticationQuery(_) => (
            header(0),
            response::Body::DeviceAuthentication(wire::DeviceAuthentication { auth: "ok".into() }),
        ),
        request::Body::MediaCatalogQuery(_) => {
            let entry = |name: &str, size: usize, read_only: bool| wire::MediaEntry {
                file_path: name.to_string(),
                file_ext: "h264".into(),
                file_size: size as u32,
                read_only,
            };
            (
                header(track),
                response::Body::MediaCatalog(wire::MediaCatalog {
                    media_file_list: display
                        .files
                        .iter()
                        .map(|(name, bytes)| entry(name, bytes.len(), false))
                        .collect(),
                    preset_file_list: vec![entry(PRESET, 1_048_576, true)],
                }),
            )
        }
        request::Body::UserConfigurationQuery(_) => (
            header(track),
            response::Body::UserConfiguration(display.config.clone()),
        ),
        request::Body::UserConfiguration(config) => {
            display.config = config;
            (header(track), acknowledgement())
        }
        request::Body::OverlayLayout(layout) => {
            display.layouts.push(layout);
            (header(track), acknowledgement())
        }
        request::Body::MetricBatch(batch) => {
            display.metrics.push(batch);
            return;
        }
        request::Body::TransferBegin(begin) => {
            display.transfer_tracks.push(track);
            *inflight = Some((
                String::from_utf8_lossy(&begin.file_name).into_owned(),
                begin.file_size as usize,
                Vec::new(),
            ));
            (
                header(track),
                response::Body::TransferBeginStatus(status(0)),
            )
        }
        request::Body::TransferChunk(chunk) => {
            if let Some((_, _, bytes)) = inflight.as_mut() {
                bytes.extend_from_slice(&chunk.file_data);
            }
            (
                header(track),
                response::Body::TransferChunkStatus(status(0)),
            )
        }
        request::Body::TransferEnd(_) => {
            let outcome = match inflight.take() {
                Some((name, declared, bytes)) if bytes.len() == declared => {
                    if display.transfer_status == 0 {
                        display.files.insert(name, bytes);
                    }
                    display.transfer_status
                }
                _ => 3,
            };
            (
                header(track),
                response::Body::TransferEndStatus(status(outcome)),
            )
        }
        request::Body::FileRemoval(removal) => {
            display.files.remove(&removal.file_name);
            (header(track), acknowledgement())
        }
        request::Body::MediaReadChunk(_) => return,
    };
    drop(display);
    send(stream, reply_header, reply);
}
