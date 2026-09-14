//! The client against a fake cm01 display on a pseudo-terminal: what goes on
//! the wire for each command, and how replies come back.

#![cfg(unix)]

use serde_json::json;
use tryx_legacy::commands::{PcInfo, ScreenConfig, preset_id};
use tryx_legacy::{Client, FanStatus, FanWarning, SerialLink};
use tryx_testkit::FakeCm01;
use tryx_testkit::cm01::SERIAL;

fn connect(display: &FakeCm01) -> Client {
    Client::from_link(SerialLink::from_port(Box::new(display.open()), "fake"))
}

#[test]
fn the_handshake_identifies_the_display() {
    let display = FakeCm01::start();
    let mut client = connect(&display);
    let info = client.handshake().unwrap();
    assert_eq!(info.product_id, "cm01");
    assert_eq!(info.firmware, "V1.0.3");
    assert_eq!(info.hardware, "V1.1");
    assert_eq!(info.serial, SERIAL);
    assert_eq!(info.attributes[2], "Fan LCD|rw");
    assert!(!info.has_pump());
    let requests = display.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        (requests[0].method.as_str(), requests[0].body.as_str()),
        ("POST", "")
    );
    assert!(requests[0].well_formed);
}

#[test]
fn settings_go_out_as_the_vendor_app_sends_them() {
    let display = FakeCm01::start();
    let mut client = connect(&display);
    assert_eq!(client.set_brightness(150).unwrap().status, "200");
    client.set_rotation(270).unwrap();
    client.set_waterfall_mode(true).unwrap();
    client.set_temperature_unit("Fahrenheit").unwrap();
    client.send_spec("Ryzen 9", "Radeon").unwrap();
    client.set_fan_lcd(Some(60)).unwrap();
    client.set_fan_lcd(None).unwrap();
    client
        .delete_media(&["a.mp4".to_string(), "Z[1].png".to_string()])
        .unwrap();
    client.reboot().unwrap();

    let requests = display.requests();
    let sent: Vec<(&str, serde_json::Value)> = requests
        .iter()
        .map(|request| (request.command.as_str(), request.json()))
        .collect();
    assert_eq!(sent[0], ("brightness", json!({"value": 100})), "clamped");
    assert_eq!(sent[1], ("rotate", json!({"degree": 270})));
    assert_eq!(sent[2], ("waterfallMode", json!({"enable": true})));
    assert_eq!(sent[3], ("temperature", json!({"value": "Fahrenheit"})));
    assert_eq!(
        sent[4],
        ("spec", json!({"cpu": "Ryzen 9", "gpu": "Radeon"}))
    );
    assert_eq!(sent[5].0, "fanLCDSet");
    assert_eq!(sent[5].1["mode"], "Fixed Mode");
    assert_eq!(sent[5].1["fixedMode"], 60);
    assert_eq!(sent[6].1["mode"], "Smart Mode");
    // Marker and escape bytes in a name survive the byte stuffing.
    assert_eq!(
        sent[7],
        ("mediaDelete", json!({"include": ["a.mp4", "Z[1].png"]}))
    );
    assert_eq!(sent[8], ("reboot", serde_json::Value::Null));
    assert!(requests.iter().all(|request| request.well_formed));
}

#[test]
fn the_screen_config_is_sent_twice_then_the_waterfall_mode() {
    let display = FakeCm01::start();
    let mut client = connect(&display);
    let config = ScreenConfig {
        preset_id: preset_id(4).unwrap().to_string(),
        waterfall_mode: true,
        ..ScreenConfig::default()
    };
    client.set_screen_config(&config).unwrap();
    client
        .set_sysinfo_display(&["CPU Usage".to_string()])
        .unwrap();
    // The reply to the fire-and-forget overlay labels is drained, so the
    // next request gets its own answer rather than that one.
    let response = client
        .send_full_config(&config, "CPU", "GPU", 120, "Celsius")
        .unwrap();
    assert_eq!(response.status, "200");
    assert_eq!(client.handshake().unwrap().serial, SERIAL);

    let requests = display.requests();
    let commands: Vec<&str> = requests.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(
        commands,
        [
            "waterBlockScreenId",
            "waterBlockScreenId",
            "waterfallMode",
            "sysinfoDisplay",
            "config",
            "conn"
        ]
    );
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(requests[0].json()["id"], "Pre-set 4: Exo-Ecologies");
    assert_eq!(requests[2].json(), json!({"enable": true}));
    assert_eq!(requests[3].json(), json!({"items": ["CPU Usage"]}));
    let full = requests[4].json();
    assert_eq!(full["waterBlockScreen"]["brightness"], 100, "clamped");
    assert_eq!(full["waterBlockScreen"]["waterfallMode"], true);
    assert_eq!(full["spec"], json!({"cpu": "CPU", "gpu": "GPU"}));
}

#[test]
fn sysinfo_carries_the_metrics_and_returns_the_fans() {
    let display = FakeCm01::start();
    display.set(|firmware| {
        firmware.status = json!({
            "status": {"fanLCD": 1310, "turboPump": "2400"},
            "availableStorage": "1048576",
            "warning": "[{\"description\":\"No ERROR\",\"type\":\"Fan LCD\"},{\"description\":\"Stalled\",\"type\":\"Turbo Pump\"}]",
        });
    });
    let mut client = connect(&display);
    let info = PcInfo {
        timestamp_ms: 1_700_000_000_000,
        ..PcInfo::default()
    };
    let fans = client.send_sysinfo(&info).unwrap();
    assert_eq!(
        fans,
        FanStatus {
            lcd_fan_rpm: Some(1310),
            pump_rpm: Some(2400),
            available_storage: Some(1_048_576),
            warnings: vec![
                FanWarning {
                    kind: "Fan LCD".into(),
                    description: "No ERROR".into(),
                },
                FanWarning {
                    kind: "Turbo Pump".into(),
                    description: "Stalled".into(),
                },
            ],
        }
    );
    let request = &display.received("all")[0];
    assert_eq!(request.method, "STATE");
    assert_eq!(request.json()["timestamp"], 1_700_000_000_000_i64);
    assert_eq!(request.json()["gpu"]["temperature"], "0");
}

/// macOS reports a pseudo-terminal readable when it is not, so a read that
/// gets no reply blocks there instead of timing out.
#[cfg(target_os = "linux")]
mod unanswered {
    use super::*;
    use std::time::Duration;
    use tryx_legacy::LegacyError;

    #[test]
    fn a_command_without_a_reply_times_out() {
        let display = FakeCm01::start();
        display.set(|firmware| firmware.unanswered = vec!["brightness".into()]);
        let mut client = connect(&display);
        client.link_mut().response_timeout = Duration::from_millis(200);
        let error = client.set_brightness(10).unwrap_err();
        assert!(
            matches!(error, LegacyError::NoResponse { ref command, timeout_ms: 200 } if command == "brightness"),
            "{error}"
        );
        // The link still works for the next command.
        assert_eq!(client.set_rotation(0).unwrap().status, "200");
    }

    #[test]
    fn a_silent_display_reports_no_fans_rather_than_an_error() {
        let display = FakeCm01::start();
        display.set(|firmware| firmware.silent = true);
        let mut client = connect(&display);
        client.link_mut().response_timeout = Duration::from_millis(200);
        assert_eq!(
            client.send_sysinfo(&PcInfo::default()).unwrap(),
            FanStatus::default()
        );
        assert!(matches!(
            client.handshake(),
            Err(LegacyError::NoResponse { .. })
        ));
    }

    #[test]
    fn an_unplugged_display_fails_with_an_io_error() {
        let display = FakeCm01::start();
        let mut client = connect(&display);
        client.handshake().unwrap();
        drop(display);
        client.link_mut().response_timeout = Duration::from_millis(200);
        let mut failure = None;
        // The kernel may buffer the first write; the port fails soon after.
        for _ in 0..5 {
            if let Err(error) = client.set_brightness(50) {
                failure = Some(error);
                if matches!(failure, Some(LegacyError::Io(_))) {
                    break;
                }
            }
        }
        assert!(matches!(failure, Some(LegacyError::Io(_))), "{failure:?}");
    }
}
