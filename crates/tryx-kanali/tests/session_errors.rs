//! How a session copes with a display that misbehaves: replies meant for
//! other requests, events in between, rejections, garbage on the pipe,
//! refused transfers, and a pipe that closes.

use prost::Message;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;
use tryx_device::Product;
use tryx_kanali::transport::TransportError;
use tryx_kanali::{Change, Device, KanaliError};
use tryx_proto::frame;
use tryx_proto::wire::v1 as wire;
use wire::{request, response};

/// What the display sends back for one request: nothing, or raw bytes.
type Script = Box<dyn FnMut(&wire::Request) -> Option<Vec<u8>> + Send>;

/// A display that answers each request with whatever `script` returns, and
/// closes the pipe once the script returns `None` for a request.
fn scripted(product: Product, mut script: Script) -> Device {
    let (host, mut display) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        let mut pending = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            while let Ok(Some(payload)) = frame::take_frame(&mut pending) {
                let request = wire::Request::decode(payload.as_slice()).unwrap();
                match script(&request) {
                    Some(bytes) => {
                        if display.write_all(&bytes).is_err() {
                            return;
                        }
                    }
                    None => return,
                }
            }
            match display.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(count) => pending.extend_from_slice(&chunk[..count]),
            }
        }
    });
    Device::from_pipe(product, Box::new(host))
}

fn track(request: &wire::Request) -> u64 {
    request.header.as_ref().map(|h| h.track_id).unwrap_or(0)
}

fn encode(track: u64, error: Option<wire::ProtocolError>, body: response::Body) -> Vec<u8> {
    let response = wire::Response {
        header: Some(wire::WireHeader {
            version: 1,
            track_id: track,
            payload_crc32: 0,
        }),
        error,
        body: Some(body),
    };
    frame::encode(&response.encode_to_vec()).unwrap()
}

fn catalog() -> response::Body {
    response::Body::MediaCatalog(wire::MediaCatalog {
        media_file_list: vec![wire::MediaEntry {
            file_path: "clip.mp4.h264_2240x1080".into(),
            file_ext: "h264".into(),
            file_size: 42,
            read_only: false,
        }],
        preset_file_list: Vec::new(),
    })
}

#[test]
fn frames_meant_for_something_else_are_skipped() {
    let mut device = scripted(
        Product::PanoramaSe,
        Box::new(|request| {
            let mut bytes = Vec::new();
            // Line noise, an event, a stray pong, and a late reply to an
            // earlier request, before the real answer.
            bytes.extend_from_slice(b"\x00\x13noise");
            bytes.extend(encode(
                0,
                None,
                response::Body::AsynchronousEvent(wire::AsynchronousEvent {
                    play_finished: true,
                }),
            ));
            bytes.extend(encode(
                0,
                None,
                response::Body::Pong(wire::Pong {
                    payload: b"hello?".to_vec(),
                }),
            ));
            bytes.extend(encode(track(request) ^ 1, None, catalog()));
            bytes.extend(encode(track(request), None, catalog()));
            Some(bytes)
        }),
    );
    let catalog = device.catalog().unwrap();
    assert_eq!(catalog.user[0].name, "clip.mp4.h264_2240x1080");
    assert_eq!(catalog.user[0].size, 42);
}

#[test]
fn a_rejection_carries_the_display_s_reason() {
    let mut device = scripted(
        Product::PanoramaSe,
        Box::new(|request| {
            Some(encode(
                track(request),
                Some(wire::ProtocolError {
                    code: wire::protocol_error::Code::Failure as i32,
                    why: "storage is busy".into(),
                }),
                catalog(),
            ))
        }),
    );
    match device.catalog() {
        Err(KanaliError::Rejected { request, why }) => {
            assert_eq!(request, "media_catalog_query");
            assert_eq!(why, "storage is busy");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_reply_of_the_wrong_kind_or_no_kind_is_invalid() {
    let mut device = scripted(
        Product::PanoramaSe,
        Box::new(|request| match &request.body {
            Some(request::Body::MediaCatalogQuery(_)) => Some(encode(
                track(request),
                None,
                response::Body::Acknowledgement(wire::Acknowledgement::default()),
            )),
            _ => Some(frame::encode(b"\xff\xff\xff not protobuf").unwrap()),
        }),
    );
    assert!(matches!(
        device.catalog(),
        Err(KanaliError::InvalidResponse {
            request: "media_catalog_query",
            ..
        })
    ));
    match device.display_state() {
        Err(KanaliError::InvalidResponse { detail, .. }) => {
            assert!(detail.contains("not a protobuf response"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_closed_pipe_is_a_disconnected_device() {
    let mut device = scripted(Product::PanoramaSe, Box::new(|_| None));
    let error = device.catalog().unwrap_err();
    assert!(
        matches!(error, KanaliError::Transport(TransportError::Disconnected)),
        "{error:?}"
    );
}

#[test]
fn refused_transfers_say_why() {
    let dir = std::env::temp_dir().join(format!("tryx-kanali-refused-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("clip.h264");
    std::fs::write(&path, [1u8; 1000]).unwrap();
    for (status, reason) in [
        (1, "not enough space on the device"),
        (2, "the device could not write the file"),
        (3, "checksum failure"),
        (9, "unknown status 9"),
    ] {
        let mut device = scripted(
            Product::PanoramaSe,
            Box::new(move |request| {
                let body = match &request.body {
                    Some(request::Body::TransferBegin(_)) => {
                        response::Body::TransferBeginStatus(wire::TransferStatus { status })
                    }
                    _ => return None,
                };
                Some(encode(track(request), None, body))
            }),
        );
        match device.upload(&path, "clip.mp4.h264_2240x1080", |_, _| {}) {
            Err(KanaliError::Transfer(message)) => assert_eq!(message, reason),
            other => panic!("status {status}: {other:?}"),
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_change_the_display_does_not_keep_is_reported() {
    // The display acknowledges the new configuration but keeps the old one.
    let config = wire::UserConfiguration {
        display_config: Some(wire::DisplayConfiguration {
            backlight_brightness: 60,
            ..Default::default()
        }),
        work_config: Some(wire::WorkConfiguration::default()),
        ..Default::default()
    };
    let mut device = scripted(
        Product::PanoramaSe,
        Box::new(move |request| {
            let body = match &request.body {
                Some(request::Body::UserConfigurationQuery(_)) => {
                    response::Body::UserConfiguration(config.clone())
                }
                Some(request::Body::UserConfiguration(_)) => {
                    response::Body::Acknowledgement(wire::Acknowledgement::default())
                }
                // The activating layout wants no answer.
                _ => return Some(Vec::new()),
            };
            Some(encode(track(request), None, body))
        }),
    );
    match device.set_brightness(80) {
        Err(KanaliError::InvalidResponse { detail, .. }) => {
            assert_eq!(detail, "device readback does not match: brightness");
        }
        other => panic!("{other:?}"),
    }
    // Invalid changes never reach the display.
    for change in [
        Change::default(),
        Change {
            brightness: Some(101),
            ..Change::default()
        },
        Change {
            rotation: Some(45),
            ..Change::default()
        },
        Change {
            media: Some(vec!["a b.mp4".into()]),
            ..Change::default()
        },
        Change {
            media: Some(vec!["one.mp4".into()]),
            split_screen: true,
            ..Change::default()
        },
    ] {
        assert!(
            matches!(device.apply(&change, None), Err(KanaliError::Invalid(_))),
            "{change:?}"
        );
    }
}

#[test]
fn a_bootstrap_reply_must_carry_the_bootstrap_header() {
    let mut device = scripted(
        Product::PanoramaSe,
        Box::new(|request| {
            let info = || {
                response::Body::DeviceInformation(wire::DeviceInformation {
                    product_name: "PASE".into(),
                    serial_number: "SN".into(),
                    ..Default::default()
                })
            };
            match &request.body {
                Some(request::Body::DeviceInformationQuery(_)) => {
                    // A tracked copy first, which the bootstrap must skip.
                    let mut bytes = encode(7, None, info());
                    bytes.extend(encode(0, None, info()));
                    Some(bytes)
                }
                Some(request::Body::SystemConfigurationQuery(_)) => Some(encode(
                    0,
                    None,
                    response::Body::SystemConfiguration(wire::SystemConfiguration {}),
                )),
                Some(request::Body::DeviceAuthenticationQuery(_)) => Some(encode(
                    0,
                    None,
                    response::Body::DeviceAuthentication(wire::DeviceAuthentication {
                        auth: "ok".into(),
                    }),
                )),
                _ => None,
            }
        }),
    );
    let info = device.start_session().unwrap();
    assert_eq!(info.serial_number, "SN");
    // Products without a session refuse the calls that need one.
    let mut turris = scripted(Product::Turris620, Box::new(|_| None));
    assert!(matches!(
        turris.display_state(),
        Err(KanaliError::Unsupported(_))
    ));
    assert!(matches!(
        turris.delete("x"),
        Err(KanaliError::Unsupported(_))
    ));
    turris
        .keepalive(None)
        .expect("transfer-only products skip the keepalive");
}
