//! Commands over the serial port of a fake cm01 display: what each one sends,
//! what it prints, and what it remembers. Opening a pseudo-terminal as a
//! serial port works on Linux only.

#![cfg(target_os = "linux")]

mod common;

use common::{Rig, TTY, USB, run};
use serde_json::json;
use std::time::Duration;
use tryx_testkit::cm01::SERIAL;
use tryx_testkit::{FakeCm01, Sandbox};

#[test]
fn info_identifies_the_discovered_display() {
    let rig = Rig::new();
    let json = rig.run(&["info", "--json"]).ok().json();
    assert_eq!(json["transport"], "legacy-serial");
    assert_eq!(json["via"], "serial");
    assert_eq!(json["link"], rig.port());
    assert_eq!(json["device"]["protocol"], "legacy");
    assert_eq!(json["device"]["serial"], SERIAL);
    assert_eq!(json["device"]["firmware"], "V1.0.3");

    rig.run(&["info"])
        .ok()
        .says("Firmware    V1.0.3")
        .says(&format!("Via         serial ({})", rig.port()));
    assert_eq!(rig.display.commands(), ["conn", "conn"]);
    assert!(rig.display.requests().iter().all(|r| r.well_formed));
}

#[test]
fn a_named_port_is_used_as_given() {
    let rig = Rig::new();
    let direct = rig.display.port().to_string_lossy().into_owned();
    // The pseudo-terminal itself, not the discovered link to it.
    let json = rig.run(&["info", "--json", "--tty", &direct]).ok().json();
    assert_eq!(json["link"], direct);
    assert_eq!(json["device"]["serial"], SERIAL);
}

#[test]
fn verbose_traces_every_frame() {
    let rig = Rig::new();
    rig.run(&["info", "--verbose"])
        .ok()
        .complains("-> conn #1")
        .complains("<- ")
        .complains("(checksum ok, length ok)");
}

#[test]
fn display_set_sends_each_setting_and_remembers_it() {
    let rig = Rig::new();
    let json = rig
        .run(&[
            "display",
            "set",
            "--json",
            "--brightness",
            "40",
            "--rotate",
            "180",
        ])
        .ok()
        .json();
    assert_eq!(json["via"], "serial");
    assert_eq!(json["brightness"], 40);
    assert_eq!(json["rotation"], 180);
    let brightness = rig.display.received("brightness");
    assert_eq!(brightness[0].json(), json!({"value": 40}));
    assert_eq!(
        rig.display.received("rotate")[0].json(),
        json!({"degree": 180})
    );
    assert_eq!(rig.state()["brightness"], 40);
    assert_eq!(rig.state()["rotation"], 180);

    // Screen settings need media to show: the firmware applies them as part
    // of the screen configuration.
    rig.run(&["display", "set", "--filter", "smoke"])
        .expect(2)
        .complains("need media");
    rig.save_state(json!({
        "screen": {"media": ["clip.mp4"]},
        "brightness": 40,
        "rotation": 180,
        "cpu_name": "CPU",
        "gpu_name": "GPU",
    }));
    rig.display.clear();
    rig.run(&[
        "display",
        "set",
        "--filter",
        "rain",
        "--filter-opacity",
        "70",
        "--sleep",
        "off",
        "--mode",
        "split",
        "--waterfall",
        "on",
    ])
    .ok()
    .says(
        "Screen applied: screen splitting, waterfall on, filter rain at 70%, sleep with host off",
    );
    assert_eq!(
        rig.display.commands(),
        [
            "waterBlockScreenId",
            "waterBlockScreenId",
            "waterfallMode",
            "sysinfoDisplay",
            "config",
            "rotate"
        ],
        "the saved rotation is applied again with the screen"
    );
    let config = rig.display.received("config")[0].json();
    let screen = &config["waterBlockScreen"];
    assert_eq!(screen["brightness"], 40);
    assert_eq!(screen["displayInSleep"], true);
    assert_eq!(screen["waterfallMode"], true);
    assert_eq!(screen["id"]["screenMode"], "Screen Splitting");
    assert_eq!(
        screen["id"]["settings"][0]["filter"],
        json!({"value": "Rain", "opacity": 70})
    );
    let state = rig.state();
    assert_eq!(state["screen"]["settings"]["filter"], "Rain");
    assert_eq!(state["screen"]["display_in_sleep"], true);
    assert_eq!(state["screen"]["media"], json!(["clip.mp4"]));
}

#[test]
fn display_get_reports_what_was_last_applied() {
    let rig = Rig::new();
    rig.save_state(json!({
        "screen": {
            "media": ["clip.mp4"],
            "play_mode": "Loop",
            "sysinfo_display": ["CPU Temperature"],
            "settings": {"badges": ["CPU Badge"], "filter": "Smoke", "filter_opacity": 30},
        },
        "brightness": 60,
        "fan_lcd_percent": 45,
    }));
    let json = rig.run(&["display", "get", "--json"]).ok().json();
    assert_eq!(json["source"], "last-applied");
    assert_eq!(json["media"], json!(["clip.mp4"]));
    assert_eq!(json["brightness"], 60);
    assert_eq!(json["fans"]["lcd_fan_rpm"], 1280);
    assert_eq!(json["fans"]["available_storage"], 3_111_497_728u64);
    assert_eq!(json["device"]["serial"], SERIAL);

    rig.run(&["display", "get"])
        .ok()
        .says("Showing          clip.mp4 (Loop)")
        .says("Brightness       60%")
        .says("Overlay          CPU Temperature")
        .says("Filter           smoke at 30%")
        .says("LCD fan          fixed 45%, 1280 rpm")
        .says("Pump             not reported by this model")
        .says("Free storage     2.9 GiB")
        .says("Health           Fan LCD: No ERROR");
    // Reading back sends nothing that changes the display.
    for command in rig.display.commands() {
        assert!(command == "conn" || command == "all", "{command}");
    }
}

#[test]
fn show_plays_media_or_a_preset() {
    let rig = Rig::new();
    let json = rig
        .run(&[
            "show",
            "--json",
            "clip.mp4",
            "still.png",
            "--play",
            "shuffle",
        ])
        .ok()
        .json();
    assert_eq!(json["media"], json!(["clip.mp4", "still.png"]));
    assert_eq!(json["play_mode"], "Shuffle");
    assert_eq!(json["status"], "200");
    let screen = rig.display.received("waterBlockScreenId")[0].json();
    assert_eq!(screen["Type"], "Custom");
    assert_eq!(screen["media"], json!(["clip.mp4", "still.png"]));
    assert_eq!(screen["playMode"], "Shuffle");
    assert_eq!(rig.state()["screen"]["play_mode"], "Shuffle");

    rig.display.clear();
    rig.run(&["show", "preset:3", "--play", "Loop"])
        .ok()
        .says("Showing Pre-set 3: Quantum time capsule (Loop, 200)");
    let screen = rig.display.received("waterBlockScreenId")[0].json();
    assert_eq!(screen["Type"], "Pre-set");
    assert_eq!(screen["id"], "Pre-set 3: Quantum time capsule");
    let state = rig.state();
    assert_eq!(
        state["screen"]["preset_id"],
        "Pre-set 3: Quantum time capsule"
    );
    assert_eq!(
        state["screen"]["media"],
        json!(["clip.mp4", "still.png"]),
        "a preset leaves the media list for later"
    );
}

#[test]
fn fans_read_the_tachometers_and_set_the_lcd_fan() {
    let rig = Rig::new();
    let json = rig.run(&["fans", "--json"]).ok().json();
    assert_eq!(json["fans"]["lcd_fan_rpm"], 1280);
    assert_eq!(json["has_pump"], false);
    assert_eq!(json["via"], "serial");
    rig.run(&["fans"]).ok().says(
        "lcd fan 1280 rpm · pump not reported by this model · fan lcd: no error · 2.9 GiB free",
    );

    rig.run(&["fans", "--lcd-speed", "60"])
        .ok()
        .says("LCD fan set to a fixed 60%");
    let set = rig.display.received("fanLCDSet");
    assert_eq!(set[0].json()["mode"], "Fixed Mode");
    assert_eq!(set[0].json()["fixedMode"], 60);
    assert_eq!(rig.state()["fan_lcd_percent"], 60);

    rig.run(&["fans", "--lcd-speed", "auto"])
        .ok()
        .says("returned to the firmware's smart curve");
    assert_eq!(
        rig.display.received("fanLCDSet")[1].json()["mode"],
        "Smart Mode"
    );
    assert_eq!(rig.state()["fan_lcd_percent"], serde_json::Value::Null);

    rig.display.set(|firmware| {
        firmware.identity["attribute"] = json!(["Status", "Turbo Pump"]);
        firmware.status = json!({"status": {"fanLCD": "900", "turboPump": "2100"}});
    });
    let json = rig.run(&["fans", "--json"]).ok().json();
    assert_eq!(json["has_pump"], true);
    assert_eq!(json["fans"]["pump_rpm"], 2100);
}

#[test]
fn metrics_set_configures_the_overlay_over_the_saved_media() {
    let rig = Rig::new();
    rig.run(&["metrics", "set", "--labels", "cpu-temp"])
        .expect(2)
        .complains("needs media");
    let json = rig
        .run(&[
            "metrics",
            "set",
            "--json",
            "--labels",
            "cpu-temp,gpu-usage,clock",
            "--position",
            "bottom",
            "--align",
            "right",
            "--color",
            "ff8800",
            "--badges",
            "cpu,gpu",
            "--media",
            "clip.mp4",
            "--play",
            "loop",
            "--cpu-name",
            "Ryzen 9",
            "--gpu-name",
            "Radeon",
            "--fahrenheit",
        ])
        .ok()
        .json();
    assert_eq!(json["temperature_unit"], "Fahrenheit");
    assert_eq!(
        json["screen"]["sysinfo_display"],
        json!(["CPU Temperature", "GPU Usage", "Date&Time"])
    );
    let labels = rig.display.received("sysinfoDisplay");
    assert_eq!(
        labels[0].json(),
        json!({"items": ["CPU Temperature", "GPU Usage", "Date&Time"]})
    );
    let config = rig.display.received("config")[0].json();
    assert_eq!(config["temperature"], "Fahrenheit");
    assert_eq!(config["spec"], json!({"cpu": "Ryzen 9", "gpu": "Radeon"}));
    let settings = &config["waterBlockScreen"]["id"]["settings"];
    assert_eq!(settings["color"], "#FF8800");
    assert_eq!(settings["position"], "Bottom");
    assert_eq!(settings["align"], "Right");
    assert_eq!(settings["badges"], json!(["CPU Badge", "GPU Badge"]));
    assert_eq!(config["waterBlockScreen"]["id"]["playMode"], "Loop");

    // The right half in split mode keeps the left as it was.
    rig.run(&["display", "set", "--mode", "split"]).ok();
    rig.run(&["metrics", "set", "--area", "right", "--labels", "mem-usage"])
        .ok()
        .says("right half: overlay shows Memory Utilization");
    let state = rig.state();
    assert_eq!(
        state["screen"]["sysinfo_display2"],
        json!(["Memory Utilization"])
    );
    assert_eq!(
        state["screen"]["sysinfo_display"],
        json!(["CPU Temperature", "GPU Usage", "Date&Time"])
    );

    rig.run(&["metrics", "set", "--clear"])
        .ok()
        .says("overlay cleared");
    assert_eq!(rig.state()["screen"]["sysinfo_display"], json!([]));
    // The unit set earlier stays until it is changed.
    assert_eq!(rig.state()["temperature_unit"], "Fahrenheit");
    assert_eq!(
        rig.display.received("config").last().unwrap().json()["temperature"],
        "Fahrenheit"
    );
    rig.run(&["metrics", "set", "--celsius"]).ok();
    assert_eq!(rig.state()["temperature_unit"], "Celsius");
    rig.run(&["metrics", "set", "--celsius", "--fahrenheit"])
        .expect(2);
    rig.run(&["metrics", "set", "--labels", "cpu-power"])
        .expect(2)
        .complains("only shown by the KANALI firmware");
}

#[test]
fn metrics_push_once_restores_the_screen_then_sends_a_sample() {
    let rig = Rig::new();
    rig.save_state(
        json!({"screen": {"media": ["clip.mp4"]}, "cpu_name": "CPU", "gpu_name": "GPU"}),
    );
    rig.run(&["metrics", "push", "--once"])
        .ok()
        .says("restored clip.mp4 with no overlay")
        .says("lcd fan 1280 rpm");
    let commands = rig.display.commands();
    assert_eq!(
        commands.first().map(String::as_str),
        Some("waterBlockScreenId")
    );
    assert_eq!(commands.last().map(String::as_str), Some("all"));
    let sample = rig.display.received("all")[0].json();
    assert!(
        sample["cpu"]["load"].is_i64(),
        "whole numbers only: {sample}"
    );

    rig.display.clear();
    let json = rig
        .run(&["metrics", "push", "--once", "--no-apply", "--json"])
        .ok()
        .json();
    assert_eq!(json["fans"]["lcd_fan_rpm"], 1280);
    assert_eq!(rig.display.commands(), ["all"]);
}

#[test]
fn raw_sends_one_command_as_is() {
    let rig = Rig::new();
    rig.run(&["raw", "brightness", r#"{"value":10}"#])
        .ok()
        .says("brightness: 200 ");
    assert_eq!(
        rig.display.received("brightness")[0].json(),
        json!({"value": 10})
    );
    rig.run(&["raw", "conn"])
        .ok()
        .says(&format!("\"sn\":\"{SERIAL}\""));
    rig.run(&["raw", "--method", "STATE", "all"])
        .ok()
        .says("fanLCD");
    assert_eq!(rig.display.received("all")[0].method, "STATE");
    rig.run(&["raw", "--no-wait", "sysinfoDisplay", r#"{"items":[]}"#])
        .ok()
        .says("sent sysinfoDisplay");
}

#[test]
fn display_reboot_asks_the_firmware_to_restart() {
    let rig = Rig::new();
    let json = rig.run(&["display", "reboot", "--json"]).ok().json();
    assert_eq!(json, json!({"rebooting": true, "via": "serial"}));
    assert_eq!(rig.display.commands(), ["reboot"]);
}

#[test]
fn an_unresponsive_display_is_a_device_failure() {
    let rig = Rig::new();
    rig.display.set(|firmware| firmware.silent = true);
    rig.run(&["info"])
        .expect(3)
        .complains("no response to `conn` within 1000 ms");
    rig.run(&["display", "set", "--brightness", "10"])
        .expect(3)
        .complains("no response to `brightness`");
}

#[test]
fn discovery_picks_the_one_display_or_asks() {
    let sandbox = Sandbox::new();
    let first = FakeCm01::start();
    let second = FakeCm01::start();
    sandbox.plug(USB, SERIAL, TTY, first.port());
    sandbox.plug("1-4", "XYZ000000000000002", "ttyACM1", second.port());
    run(&sandbox, &["info"])
        .expect(3)
        .complains("several legacy displays are connected; choose one with --tty");
    let port = sandbox.port("ttyACM1");
    let json = run(
        &sandbox,
        &["info", "--json", "--tty", port.to_str().unwrap()],
    )
    .ok()
    .json();
    assert_eq!(json["link"], port.to_str().unwrap());
    assert!(first.requests().is_empty());
    assert_eq!(second.commands(), ["conn"]);
    run(&sandbox, &["devices"])
        .ok()
        .says("usb:001-4")
        .says("usb:003-12")
        .says("cm01_se");
}

#[test]
fn a_port_without_permission_is_explained() {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new();
    if nix_is_root() {
        return; // root opens anything
    }
    let node = sandbox.root().join("locked-tty");
    std::fs::write(&node, b"").unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o000)).unwrap();
    sandbox.plug(USB, SERIAL, TTY, &node);
    run(&sandbox, &["info"])
        .expect(3)
        .complains("permission denied; join the dialout group");
    let doctor = run(&sandbox, &["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "device usb:003-12")
        .cloned()
        .unwrap();
    assert_eq!(check["status"], "fail");
    assert!(
        check["detail"]
            .as_str()
            .unwrap()
            .contains("permission denied"),
        "{check}"
    );
}

#[test]
fn a_display_without_a_command_port_is_explained() {
    let sandbox = Sandbox::new();
    let display = FakeCm01::start();
    sandbox.plug(USB, SERIAL, TTY, display.port());
    // The interface lost its tty: the firmware is still booting.
    std::fs::remove_dir_all(sandbox.root().join(format!("sys/{USB}/{USB}:1.0/tty"))).unwrap();
    run(&sandbox, &["info"])
        .expect(3)
        .complains("usb:003-12: no serial command port exposed; replug the display");
    let doctor = run(&sandbox, &["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "device usb:003-12")
        .cloned()
        .unwrap();
    assert_eq!(check["status"], "warn");
}

#[test]
fn doctor_and_devices_describe_a_healthy_display() {
    let rig = Rig::new();
    let doctor = rig.run(&["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "device usb:003-12")
        .cloned()
        .unwrap();
    assert_eq!(check["status"], "ok");
    let detail = check["detail"].as_str().unwrap();
    assert!(
        detail.contains("accessible, ADB interface present"),
        "{detail}"
    );
    assert!(
        rig.display.requests().is_empty(),
        "doctor never opens the port"
    );

    let devices = rig.run(&["devices", "--json"]).ok().json();
    let device = &devices["legacy_devices"][0];
    assert_eq!(device["id"], "usb:003-12");
    assert_eq!(device["serial"], SERIAL);
    assert_eq!(device["tty"], rig.port());
    assert_eq!(device["tty_access"]["state"], "accessible");
    assert_eq!(device["adb_interface"], true);
    rig.run(&["devices"])
        .ok()
        .says("Legacy cm01 firmware (serial + ADB protocol):")
        .says(&rig.port());
}

#[test]
fn daemon_install_writes_a_user_service() {
    let rig = Rig::new();
    let log = rig.sandbox.root().join("systemctl.log");
    for tool in ["systemctl", "loginctl"] {
        let script = rig.sandbox.bin().join(tool);
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho \"{tool} $*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let port = rig.port();
    let json = rig
        .run(&[
            "daemon",
            "install",
            "--json",
            "--interval",
            "7",
            "--tty",
            &port,
        ])
        .ok()
        .json();
    assert_eq!(json["interval"], 7);
    assert_eq!(json["linger"], true);
    let unit = rig
        .sandbox
        .root()
        .join("config/systemd/user/tryxctl.service");
    assert_eq!(json["unit"], unit.to_str().unwrap());
    let text = std::fs::read_to_string(&unit).unwrap();
    assert!(
        text.contains(&format!(
            "ExecStart={} daemon --interval 7 --quiet --tty {port}",
            env!("CARGO_BIN_EXE_tryxctl")
        )),
        "{text}"
    );
    assert!(text.contains("Restart=always"), "{text}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls,
        "systemctl --user daemon-reload\nsystemctl --user enable --now tryxctl.service\nloginctl enable-linger\n"
    );

    rig.run(&["daemon", "uninstall"]).ok().says("removed");
    assert!(!unit.exists());
    rig.run(&["daemon", "uninstall", "--json"]).ok();
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .contains("systemctl --user disable --now tryxctl.service")
    );
}

#[test]
fn a_display_that_waits_for_its_reply_still_gets_one() {
    // The port holds a reply nobody read, from a command sent without
    // waiting: the next command must not take it as its own answer.
    let rig = Rig::new();
    rig.run(&["raw", "--no-wait", "conn"]).ok();
    std::thread::sleep(Duration::from_millis(100));
    rig.run(&["raw", "brightness", r#"{"value":20}"#])
        .ok()
        .says("brightness: 200 \n");
}

fn nix_is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}
