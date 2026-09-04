//! A scripted display on the other end of a Unix socket: it speaks the
//! framed protobuf protocol like the firmware and records what it received.

use prost::Message;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tryx_device::Product;
use tryx_kanali::{Change, Device, TURRIS_TRANSFER_TRACK_ID};
use tryx_proto::frame;
use tryx_proto::wire::v1 as wire;
use wire::{request, response};

struct FakeDisplay {
    stream: UnixStream,
    config: wire::UserConfiguration,
    files: Vec<(String, u32)>,
    received: mpsc::Sender<String>,
    inflight: Option<(String, u32, u32)>,
    inject_event_before_next: bool,
    turris: bool,
}

impl FakeDisplay {
    fn run(mut self) {
        let mut pending = Vec::new();
        let mut chunk = [0u8; 65536];
        self.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        loop {
            let payload = loop {
                match frame::take_frame(&mut pending) {
                    Ok(Some(payload)) => break payload,
                    Ok(None) => {}
                    Err(_) => return,
                }
                match self.stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => pending.extend_from_slice(&chunk[..n]),
                }
            };
            let Ok(req) = wire::Request::decode(payload.as_slice()) else {
                return;
            };
            if !self.handle(req) {
                return;
            }
        }
    }

    fn send(&mut self, response: wire::Response) {
        let bytes = frame::encode(&response.encode_to_vec()).unwrap();
        let _ = self.stream.write_all(&bytes);
    }

    fn reply(&mut self, header: Option<wire::WireHeader>, body: response::Body) {
        if self.inject_event_before_next {
            self.inject_event_before_next = false;
            self.send(wire::Response {
                header: None,
                error: None,
                body: Some(response::Body::AsynchronousEvent(wire::AsynchronousEvent {
                    play_finished: true,
                })),
            });
        }
        self.send(wire::Response {
            header,
            error: None,
            body: Some(body),
        });
    }

    fn handle(&mut self, req: wire::Request) -> bool {
        let header = req.header;
        let track = header.as_ref().map(|h| h.track_id).unwrap_or(0);
        let echo = Some(wire::WireHeader {
            version: 1,
            track_id: track,
            payload_crc32: 0,
        });
        let bootstrap = Some(wire::WireHeader {
            version: 1,
            track_id: 0,
            payload_crc32: 0,
        });
        let Some(body) = req.body else { return true };
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
        let _ = self.received.send(format!("{name}:{track}"));
        match body {
            request::Body::Ping(ping) => self.reply(
                None,
                response::Body::Pong(wire::Pong {
                    payload: ping.payload,
                }),
            ),
            request::Body::DeviceInformationQuery(_) => {
                self.inject_event_before_next = true;
                self.reply(
                    bootstrap,
                    response::Body::DeviceInformation(wire::DeviceInformation {
                        os_name: "Linux".into(),
                        os_version: "5.10".into(),
                        firmware_version: "2.3.1".into(),
                        product_name: "PASE".into(),
                        app_version: "1.4".into(),
                        serial_number: "SN-FAKE".into(),
                        serial_number_locked: true,
                        chip_id: "rk3568".into(),
                    }),
                )
            }
            request::Body::SystemConfigurationQuery(_) => self.reply(
                bootstrap,
                response::Body::SystemConfiguration(wire::SystemConfiguration {}),
            ),
            request::Body::DeviceAuthenticationQuery(_) => self.reply(
                bootstrap,
                response::Body::DeviceAuthentication(wire::DeviceAuthentication {
                    auth: "ok".into(),
                }),
            ),
            request::Body::MediaCatalogQuery(_) => {
                let entry = |name: &str, size: u32, read_only: bool| wire::MediaEntry {
                    file_path: name.to_string(),
                    file_ext: "h264".into(),
                    file_size: size,
                    read_only,
                };
                let media_file_list = self
                    .files
                    .iter()
                    .map(|(n, s)| entry(n, *s, false))
                    .collect();
                self.reply(
                    echo,
                    response::Body::MediaCatalog(wire::MediaCatalog {
                        media_file_list,
                        preset_file_list: vec![entry("default_01.mp4.h264_2240x1080", 1000, true)],
                    }),
                )
            }
            request::Body::UserConfigurationQuery(_) => {
                let config = self.config.clone();
                self.reply(echo, response::Body::UserConfiguration(config))
            }
            request::Body::UserConfiguration(config) => {
                self.config = config;
                self.reply(
                    echo,
                    response::Body::Acknowledgement(wire::Acknowledgement {
                        dummy: String::new(),
                    }),
                )
            }
            request::Body::OverlayLayout(_) => self.reply(
                echo,
                response::Body::Acknowledgement(wire::Acknowledgement {
                    dummy: String::new(),
                }),
            ),
            request::Body::MetricBatch(_) => {}
            request::Body::TransferBegin(begin) => {
                if self.turris {
                    assert_eq!(
                        track, TURRIS_TRANSFER_TRACK_ID,
                        "Turris transfers use the fixed track id"
                    );
                }
                self.inflight = Some((
                    String::from_utf8_lossy(&begin.file_name).into_owned(),
                    begin.file_size,
                    0,
                ));
                self.reply(
                    echo,
                    response::Body::TransferBeginStatus(wire::TransferStatus { status: 0 }),
                )
            }
            request::Body::TransferChunk(chunk) => {
                let Some(inflight) = &mut self.inflight else {
                    return false;
                };
                inflight.2 += chunk.file_data.len() as u32;
                self.reply(
                    echo,
                    response::Body::TransferChunkStatus(wire::TransferStatus { status: 0 }),
                )
            }
            request::Body::TransferEnd(end) => {
                assert_eq!(end.file_type, b"media");
                let (name, declared, got) = self.inflight.take().unwrap();
                let status = if declared == got { 0 } else { 2 };
                if status == 0 {
                    self.files.push((name, declared));
                }
                self.reply(
                    echo,
                    response::Body::TransferEndStatus(wire::TransferStatus { status }),
                )
            }
            request::Body::FileRemoval(removal) => {
                self.files.retain(|(n, _)| *n != removal.file_name);
                self.reply(
                    echo,
                    response::Body::Acknowledgement(wire::Acknowledgement {
                        dummy: String::new(),
                    }),
                )
            }
            request::Body::MediaReadChunk(_) => {}
        }
        true
    }
}

fn start(product: Product) -> (Device, mpsc::Receiver<String>) {
    let (host, display) = UnixStream::pair().unwrap();
    let (tx, rx) = mpsc::channel();
    let fake = FakeDisplay {
        stream: display,
        config: wire::UserConfiguration {
            display_config: Some(wire::DisplayConfiguration {
                backlight_enable: true,
                backlight_brightness: 60,
                ..Default::default()
            }),
            work_config: Some(wire::WorkConfiguration {
                single_mode_media_file: "default_01.mp4.h264_2240x1080".into(),
                ..Default::default()
            }),
            ..Default::default()
        },
        files: vec![("old.mp4.h264_2240x1080".into(), 4096)],
        received: tx,
        inflight: None,
        inject_event_before_next: false,
        turris: product == Product::Turris620,
    };
    thread::spawn(move || fake.run());
    (Device::from_pipe(product, Box::new(host)), rx)
}

fn drain(rx: &mpsc::Receiver<String>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(item) = rx.recv_timeout(Duration::from_millis(50)) {
        out.push(item);
    }
    out
}

#[test]
fn session_bootstraps_lists_applies_uploads_and_deletes() {
    let (mut device, rx) = start(Product::PanoramaSe);
    let info = device.start_session().expect("bootstrap");
    assert_eq!(info.product_name, "PASE");
    assert_eq!(info.firmware_version, "2.3.1");
    let seen = drain(&rx);
    assert_eq!(seen[0], "device_information_query:0");
    assert_eq!(seen[1], "system_configuration_query:0");
    assert_eq!(seen[2], "device_authentication_query:0");

    let catalog = device.catalog().expect("catalog");
    assert_eq!(catalog.user.len(), 1);
    assert_eq!(catalog.presets[0].name, "default_01.mp4.h264_2240x1080");
    assert!(catalog.presets[0].read_only);

    let state = device.display_state().expect("state");
    assert_eq!(state.brightness, 60);
    assert_eq!(state.media, vec!["default_01.mp4.h264_2240x1080"]);

    let applied = device
        .apply(
            &Change {
                media: Some(vec!["old.mp4.h264_2240x1080".into()]),
                play_mode: Some("Loop".into()),
                brightness: Some(80),
                ..Change::default()
            },
            None,
        )
        .expect("apply");
    assert_eq!(applied.brightness, 80);
    assert_eq!(applied.play_mode, "Loop");
    assert_eq!(applied.media, vec!["old.mp4.h264_2240x1080"]);
    let seen = drain(&rx);
    let names: Vec<&str> = seen.iter().map(|s| s.split(':').next().unwrap()).collect();
    assert_eq!(
        names,
        vec![
            "media_catalog_query",
            "user_configuration_query",
            "user_configuration_query",
            "user_configuration",
            "overlay_layout",
            "user_configuration_query"
        ]
    );
    let tracks: Vec<&str> = seen.iter().map(|s| s.split(':').nth(1).unwrap()).collect();
    assert!(
        tracks.iter().all(|t| *t != "0"),
        "tracked requests carry a track id: {tracks:?}"
    );

    let dir = std::env::temp_dir().join(format!("tryx-kanali-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.h264");
    std::fs::write(&path, vec![7u8; 700 * 1024]).unwrap();
    let mut progress = Vec::new();
    let name = device.remote_name("clip.mp4");
    assert_eq!(name, "clip.mp4.h264_2240x1080");
    device
        .upload(&path, &name, |sent, total| progress.push((sent, total)))
        .expect("upload");
    assert_eq!(progress.len(), 3, "256 KiB chunks");
    assert_eq!(progress.last().unwrap().0, 700 * 1024);
    let seen = drain(&rx);
    assert_eq!(
        seen.iter()
            .filter(|s| s.starts_with("transfer_chunk"))
            .count(),
        3
    );
    let catalog = device.catalog().unwrap();
    assert!(
        catalog
            .user
            .iter()
            .any(|f| f.name == name && f.size == 700 * 1024)
    );

    device.delete(&name).expect("delete");
    assert!(
        !device
            .catalog()
            .unwrap()
            .user
            .iter()
            .any(|f| f.name == name)
    );
    std::fs::remove_dir_all(dir).unwrap();

    assert!(
        device
            .upload(&path, "bad name.h264_2240x1080", |_, _| {})
            .is_err()
    );
}

#[test]
fn turris_uploads_with_the_fixed_track_id_and_refuses_the_catalog() {
    let (mut device, rx) = start(Product::Turris620);
    assert!(device.catalog().is_err(), "transfer-only product");
    assert!(device.start_session().is_err());
    let dir = std::env::temp_dir().join(format!("tryx-kanali-turris-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("still.blob");
    std::fs::write(&path, vec![1u8; 10_000]).unwrap();
    device
        .upload(&path, &device.remote_name("still.png"), |_, _| {})
        .expect("turris upload");
    let seen = drain(&rx);
    assert!(
        seen.iter()
            .all(|s| s.ends_with(&format!(":{TURRIS_TRANSFER_TRACK_ID}"))),
        "{seen:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn keepalive_is_untracked_and_survives_missing_replies() {
    let (mut device, rx) = start(Product::PanoramaSe);
    device.keepalive(None).expect("keepalive");
    let seen = drain(&rx);
    assert_eq!(seen, vec!["ping:0", "overlay_layout:0"]);
}
