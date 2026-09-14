//! The daemon against a fake display: it restores the screen, keeps pushing
//! metrics, carries other commands over its socket, and finds the display
//! again after it goes away. The daemon runs on Linux only.

#![cfg(target_os = "linux")]

mod common;

use common::{Rig, TTY, USB, run};
use serde_json::{Value, json};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
use tryx_testkit::cm01::SERIAL;
use tryx_testkit::{FakeCm01, Sandbox};

/// Enough for the daemon's three-second reconnect interval, twice over.
const RECONNECT: Duration = Duration::from_secs(10);

struct Daemon {
    child: Child,
    log: std::path::PathBuf,
}

impl Daemon {
    fn start(sandbox: &Sandbox) -> Daemon {
        let log = sandbox.root().join("daemon.log");
        let file = std::fs::File::create(&log).unwrap();
        let mut command = sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
        command
            .args(["daemon", "--interval", "1"])
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file));
        let child = {
            let _spawning = tryx_testkit::spawning();
            command.spawn().unwrap()
        };
        let daemon = Daemon { child, log };
        let deadline = Instant::now() + Duration::from_secs(10);
        while run(sandbox, &["daemon", "status", "--json"]).code != Some(0) {
            assert!(
                Instant::now() < deadline,
                "the daemon never answered:\n{}",
                daemon.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        daemon
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn status(sandbox: &Sandbox) -> Value {
    run(sandbox, &["daemon", "status", "--json"]).ok().json()
}

/// Waits until the daemon's status satisfies `done`.
fn wait_status(
    sandbox: &Sandbox,
    daemon: &Daemon,
    timeout: Duration,
    done: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = Instant::now() + timeout;
    loop {
        let status = status(sandbox);
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never reached the expected state: {status:#}\n--- log\n{}",
            daemon.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn the_daemon_restores_the_screen_and_keeps_pushing() {
    let rig = Rig::new();
    rig.save_state(json!({
        "screen": {"media": ["clip.mp4"], "sysinfo_display": ["CPU Usage"]},
        "cpu_name": "CPU",
        "gpu_name": "GPU",
        "fan_lcd_percent": 55,
        "rotation": 90,
    }));
    let daemon = Daemon::start(&rig.sandbox);
    assert!(rig.display.wait_for(RECONNECT, |requests| {
        requests.iter().filter(|r| r.command == "all").count() >= 2
    }));
    let commands = rig.display.commands();
    assert_eq!(
        &commands[..8],
        [
            "conn",
            "waterBlockScreenId",
            "waterBlockScreenId",
            "waterfallMode",
            "sysinfoDisplay",
            "config",
            "rotate",
            "fanLCDSet"
        ],
        "{commands:?}"
    );
    assert_eq!(rig.display.received("fanLCDSet")[0].json()["fixedMode"], 55);
    assert!(rig.display.requests().iter().all(|r| r.well_formed));

    let status = wait_status(&rig.sandbox, &daemon, RECONNECT, |s| {
        s["pushes"].as_u64() >= Some(2)
    });
    assert_eq!(status["connected"], true);
    assert_eq!(status["protocol"], "legacy");
    assert_eq!(status["tty"], rig.port());
    assert_eq!(status["interval"], 1);
    assert_eq!(status["reconnects"], 0);
    assert_eq!(status["info"]["serial"], SERIAL);
    assert_eq!(status["fans"]["lcd_fan_rpm"], 1280);
    assert_eq!(status["screen"]["media"], json!(["clip.mp4"]));
    assert_eq!(status["last_error"], Value::Null);

    run(&rig.sandbox, &["daemon", "status"])
        .ok()
        .says("Showing       clip.mp4")
        .says("Overlay       CPU Usage")
        .says("Fans          LCD fan 1280 rpm; no pump tachometer on this model")
        .says("Free storage  2.9 GiB");
    assert!(
        daemon.log().contains("restored clip.mp4 with CPU Usage"),
        "{}",
        daemon.log()
    );

    run(&rig.sandbox, &["daemon", "--interval", "1"])
        .expect(2)
        .complains("a tryxctl daemon is already running on this socket");
    run(&rig.sandbox, &["metrics", "push", "--once"])
        .expect(2)
        .complains("the tryxctl daemon is running and already pushes metrics");
}

#[test]
fn commands_go_through_the_daemon_holding_the_display() {
    let rig = Rig::new();
    let daemon = Daemon::start(&rig.sandbox);
    wait_status(&rig.sandbox, &daemon, RECONNECT, |s| {
        s["pushes"].as_u64() >= Some(1)
    });
    let socket = rig.sandbox.socket().to_string_lossy().into_owned();

    let info = rig.run(&["info", "--json"]).ok().json();
    assert_eq!(
        (info["via"].as_str(), info["link"].as_str()),
        (Some("daemon"), Some(socket.as_str()))
    );
    assert_eq!(info["device"]["serial"], SERIAL);

    // The same display by either of its names still goes through the daemon.
    let direct = rig.display.port().to_string_lossy().into_owned();
    for tty in [rig.port(), direct] {
        let json = rig
            .run(&[
                "display",
                "set",
                "--json",
                "--brightness",
                "33",
                "--tty",
                &tty,
            ])
            .ok()
            .json();
        assert_eq!(json["via"], "daemon");
    }
    assert_eq!(rig.display.received("brightness").len(), 2);

    // Another display is never served by this one's daemon.
    let conns = rig.display.received("conn").len();
    rig.run(&["info", "--tty", "/nonexistent/ttyTRYX"])
        .expect(3)
        .complains("/nonexistent/ttyTRYX");
    assert_eq!(rig.display.received("conn").len(), conns);
    // Opening the port directly while the daemon holds it fails.
    rig.run(&["info", "--direct"]).expect(3);

    assert_eq!(rig.run(&["fans", "--json"]).ok().json()["via"], "daemon");
    rig.run(&["fans", "--lcd-speed", "70"]).ok();
    assert_eq!(rig.display.received("fanLCDSet")[0].json()["fixedMode"], 70);
    rig.run(&["raw", "brightness", r#"{"value":5}"#])
        .ok()
        .says("brightness: 200 ");

    rig.run(&["show", "clip.mp4", "--play", "Loop"]).ok();
    assert_eq!(rig.state()["screen"]["media"], json!(["clip.mp4"]));
    let readback = rig.run(&["display", "get", "--json"]).ok().json();
    assert_eq!(readback["media"], json!(["clip.mp4"]));
    assert_eq!(readback["device"]["serial"], SERIAL);
    rig.run(&["display", "set", "--rotate", "270"]).ok();
    assert_eq!(
        rig.display.received("rotate").last().unwrap().json()["degree"],
        270
    );

    rig.adb.put("old.mp4", b"x");
    rig.run(&["media", "rm", "old.mp4"]).ok();
    assert_eq!(
        rig.display.received("mediaDelete")[0].json(),
        json!({"include": ["old.mp4"]})
    );
    assert!(rig.adb.names().is_empty());

    // A preset on screen is reported as such, not as nothing.
    rig.run(&["show", "preset:2"]).ok();
    run(&rig.sandbox, &["daemon", "status"])
        .ok()
        .says("Showing       Pre-set 2: Migration");
}

#[test]
fn the_daemon_finds_the_display_again_after_it_goes_away() {
    let sandbox = Sandbox::new();
    let first = FakeCm01::start();
    sandbox.plug(USB, SERIAL, TTY, first.port());
    let daemon = Daemon::start(&sandbox);
    wait_status(&sandbox, &daemon, RECONNECT, |s| {
        s["pushes"].as_u64() >= Some(1)
    });

    drop(first);
    let lost = wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == false);
    assert!(lost["last_error"].as_str().is_some(), "{lost}");
    run(&sandbox, &["display", "set", "--brightness", "10"])
        .expect(3)
        .complains("the display is disconnected; the daemon retries every 3 s");
    run(&sandbox, &["daemon", "status"])
        .ok()
        .says("(disconnected, retrying)");

    let second = FakeCm01::start();
    sandbox.point(TTY, second.port());
    let back = wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
    assert_eq!(back["reconnects"], 1);
    assert!(second.wait_for(RECONNECT, |r| r.iter().any(|r| r.command == "all")));
    assert_eq!(second.commands()[0], "conn");
    run(&sandbox, &["display", "set", "--brightness", "10"]).ok();
    assert_eq!(second.received("brightness").len(), 1);
    assert!(daemon.log().contains("reconnected to"), "{}", daemon.log());
}

#[test]
fn the_daemon_waits_for_a_display_that_is_not_there_yet() {
    let sandbox = Sandbox::new();
    let daemon = Daemon::start(&sandbox);
    let waiting = status(&sandbox);
    assert_eq!(waiting["connected"], false);
    assert_eq!(waiting["last_error"], "no TRYX display connected");
    run(&sandbox, &["info"])
        .expect(3)
        .complains("the display is disconnected");

    let display = FakeCm01::start();
    sandbox.plug(USB, SERIAL, TTY, display.port());
    let connected = wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
    assert_eq!(connected["reconnects"], 1);
    assert_eq!(connected["info"]["serial"], SERIAL);
}

#[test]
fn a_reboot_through_the_daemon_reconnects_afterwards() {
    let rig = Rig::new();
    let daemon = Daemon::start(&rig.sandbox);
    wait_status(&rig.sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
    let json = rig.run(&["display", "reboot", "--json"]).ok().json();
    assert_eq!(json["via"], "daemon");
    assert_eq!(rig.display.received("reboot").len(), 1);
    let back = wait_status(&rig.sandbox, &daemon, RECONNECT, |s| {
        s["reconnects"] == 1 && s["connected"] == true
    });
    assert_eq!(back["info"]["serial"], SERIAL);
    assert_eq!(rig.display.received("conn").len(), 2);
}

#[test]
fn a_stale_socket_does_not_stop_a_new_daemon() {
    let rig = Rig::new();
    let socket = rig.sandbox.socket();
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    // A socket left behind by a daemon that was killed.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    run(&rig.sandbox, &["daemon", "status"]).expect(3);
    let json = rig.run(&["info", "--json"]).ok().json();
    assert_eq!(json["via"], "serial");
    let daemon = Daemon::start(&rig.sandbox);
    wait_status(&rig.sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
}

#[test]
fn a_stalled_client_does_not_hold_up_the_socket() {
    let rig = Rig::new();
    let daemon = Daemon::start(&rig.sandbox);
    wait_status(&rig.sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
    // Connected, and never sends its request.
    let _stalled = std::os::unix::net::UnixStream::connect(rig.sandbox.socket()).unwrap();
    let started = Instant::now();
    status(&rig.sandbox);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "status waited {:?} behind a stalled client",
        started.elapsed()
    );
    // A request that is not one gets an answer saying so.
    let mut raw = std::os::unix::net::UnixStream::connect(rig.sandbox.socket()).unwrap();
    use std::io::{Read, Write};
    let body = br#"{"type":"launch-missiles"}"#;
    raw.write_all(&(body.len() as u32).to_le_bytes()).unwrap();
    raw.write_all(body).unwrap();
    let mut header = [0u8; 4];
    raw.read_exact(&mut header).unwrap();
    let mut reply = vec![0u8; u32::from_le_bytes(header) as usize];
    raw.read_exact(&mut reply).unwrap();
    let reply: Value = serde_json::from_slice(&reply).unwrap();
    assert_eq!(reply["ok"], false);
    assert!(
        reply["error"].as_str().unwrap().starts_with("bad request"),
        "{reply}"
    );
}

mod kanali {
    use super::*;
    use common::{KANALI_ID, KANALI_USB, KanaliRig};
    use tryx_testkit::FakeKanali;
    use tryx_testkit::kanali::{PANORAMA_SE, PRESET};

    #[test]
    fn the_daemon_keeps_a_kanali_session_and_carries_commands() {
        let rig = KanaliRig::new();
        run(&rig.sandbox, &["metrics", "set", "--labels", "cpu-usage"]).ok();
        let daemon = Daemon::start(&rig.sandbox);
        let status = wait_status(&rig.sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
        assert_eq!(status["protocol"], "kanali");
        assert_eq!(status["product"], "panorama-se");
        assert_eq!(status["tty"], KANALI_ID);
        assert_eq!(
            status["info"]["serial_number"],
            tryx_testkit::kanali::SERIAL
        );
        assert!(
            rig.display.wait_for(RECONNECT, |d| d
                .received
                .iter()
                .filter(|r| **r == "ping")
                .count()
                >= 2),
            "keepalive pings: {:?}",
            rig.display.received()
        );
        assert!(
            rig.display.wait_for(RECONNECT, |d| !d.metrics.is_empty()),
            "metric values"
        );

        let info = rig.run(&["info", "--json"]).ok().json();
        assert_eq!(info["via"], "daemon");
        rig.run(&["display", "set", "--brightness", "25"]).ok();
        assert_eq!(
            rig.display
                .with(|d| d.config.display_config.unwrap().backlight_brightness),
            25
        );
        rig.run(&["display", "set", "--rotate", "180"]).ok();
        let readback = rig.run(&["display", "get", "--json"]).ok().json();
        assert_eq!(readback["source"], "device");
        assert_eq!(readback["rotation"], 180);
        assert_eq!(readback["media"], json!([PRESET]));
        let listed = rig.run(&["media", "ls", "--json"]).ok().json();
        assert_eq!(listed["via"], "daemon");
        assert_eq!(listed["presets"][0]["name"], PRESET);
        rig.run(&["show", PRESET]).ok();
        rig.run(&["fans"])
            .expect(3)
            .complains("fan readings is not available on the KANALI firmware");
        rig.run(&["display", "reboot"])
            .expect(3)
            .complains("reboot is not available");

        if tryx_testkit::media::ffmpeg_available() {
            let clip =
                tryx_testkit::media::clip(&rig.sandbox.work().join("clip.mov"), 160, 120, 1.0);
            rig.run(&["media", "upload", clip.to_str().unwrap()]).ok();
            let name = "clip.mp4.h264_2240x1080";
            assert!(rig.display.with(|d| d.files.contains_key(name)));
            rig.run(&["media", "rm", name]).ok();
            assert!(rig.display.with(|d| d.files.is_empty()));
        }
    }

    #[test]
    fn the_daemon_finds_a_kanali_display_again_after_it_goes_away() {
        let sandbox = Sandbox::new();
        let socket = sandbox.plug_kanali(KANALI_USB, PANORAMA_SE, tryx_testkit::kanali::SERIAL);
        let first = FakeKanali::start(&socket);
        let daemon = Daemon::start(&sandbox);
        wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
        drop(first);
        let lost = wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == false);
        assert!(
            lost["last_error"]
                .as_str()
                .is_some_and(|error| error.starts_with("keepalive: ")),
            "the reason is kept: {lost}"
        );
        let second = FakeKanali::start(&socket);
        let back = wait_status(&sandbox, &daemon, RECONNECT, |s| s["connected"] == true);
        assert_eq!(back["reconnects"], 1);
        assert!(second.wait_for(RECONNECT, |d| d.received.contains(&"ping")));
    }
}
