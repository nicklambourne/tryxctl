//! Commands against a fake KANALI display: the printer-class protocol, over
//! the socket that stands in for its USB pipe.

#![cfg(unix)]

mod common;

use common::{KANALI_ID, KanaliRig, run};
use serde_json::json;
use std::time::Duration;
use tryx_testkit::Sandbox;
use tryx_testkit::kanali::{PRESET, SERIAL, TURRIS_620};
use tryx_testkit::media::{self, ffmpeg_available};

#[test]
fn info_runs_the_session_handshake() {
    let rig = KanaliRig::new();
    let json = rig.run(&["info", "--json"]).ok().json();
    assert_eq!(json["transport"], "kanali-usb");
    assert_eq!(json["via"], "usb");
    assert_eq!(json["link"], KANALI_ID);
    assert_eq!(json["device"]["protocol"], "kanali");
    assert_eq!(json["device"]["serial_number"], SERIAL);
    assert_eq!(json["device"]["firmware_version"], "2.3.1");
    assert_eq!(
        rig.display.received(),
        [
            "device_information_query",
            "system_configuration_query",
            "device_authentication_query"
        ]
    );
    rig.run(&["info"])
        .ok()
        .says(&format!("Serial    {SERIAL} (locked)"))
        .says("Chip      rk3568")
        .says(&format!("Via       usb ({KANALI_ID})"));
    // Named by its id, and by an id that is not there.
    rig.run(&["info", "--device", KANALI_ID]).ok();
    rig.run(&["info", "--device", "usb:009-9"])
        .expect(3)
        .complains("no KANALI display with id usb:009-9");
}

#[test]
fn devices_and_doctor_list_printer_class_displays() {
    let rig = KanaliRig::new();
    rig.sandbox.plug_kanali("1-1", 0x0006, "");
    let devices = rig.run(&["devices", "--json"]).ok().json();
    let printers = devices["printer_devices"].as_array().unwrap();
    assert_eq!(printers.len(), 2);
    assert_eq!(printers[1]["id"], KANALI_ID);
    assert_eq!(printers[1]["product"], "panorama-se");
    assert_eq!(printers[0]["transitional"], true);
    rig.run(&["devices"])
        .ok()
        .says("Rockchip gadget (booting)")
        .says("Panorama SE")
        .says("391a:1021");

    let doctor = rig.run(&["doctor", "--json"]).json();
    let check = |name: &str| {
        doctor["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("no {name} check: {doctor:#}"))
    };
    assert_eq!(check("device usb:003-4")["status"], "ok");
    assert!(
        check("device usb:003-4")["detail"]
            .as_str()
            .unwrap()
            .contains("Panorama SE (391a:1021) accessible, printer interface 0")
    );
    assert_eq!(check("device usb:001-1")["status"], "warn");
    assert!(rig.display.received().is_empty(), "listing opens nothing");
}

#[test]
fn a_display_that_cannot_be_opened_is_explained() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let rig = KanaliRig::new();
    let socket = rig.sandbox.root().join("sys/3-4/socket");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).unwrap();
    rig.run(&["info"])
        .expect(3)
        .complains("usb:003-4: permission denied; install packaging/udev/99-tryx-printer.rules");
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Plugged in, but not answering: the socket is gone.
    let KanaliRig { sandbox, display } = rig;
    drop(display);
    run(&sandbox, &["info"]).expect(3).complains("usb:003-4");
}

#[test]
fn display_settings_go_through_the_user_configuration() {
    let rig = KanaliRig::new();
    let readback = rig.run(&["display", "get", "--json"]).ok().json();
    assert_eq!(readback["source"], "device");
    assert_eq!(readback["protocol"], "kanali");
    assert_eq!(readback["media"], json!([PRESET]));
    assert_eq!(readback["brightness"], 60);
    rig.run(&["display", "get"])
        .ok()
        .says("read from the display")
        .says(&format!("Showing     {PRESET} (Single)"));

    let set = rig
        .run(&[
            "display",
            "set",
            "--json",
            "--brightness",
            "35",
            "--rotate",
            "180",
        ])
        .ok()
        .json();
    assert_eq!(set["via"], "usb");
    assert_eq!(set["statuses"]["brightness"], "applied");
    let display = rig.display.with(|d| d.config.display_config.unwrap());
    assert_eq!(display.backlight_brightness, 35);
    assert_eq!(display.media_rotation, 180);
    assert_eq!(rig.state()["brightness"], 35);
    assert_eq!(
        rig.run(&["display", "get", "--json"]).ok().json()["rotation"],
        180
    );

    rig.run(&["display", "set", "--filter", "smoke"])
        .expect(3)
        .complains("filters and sleep control belong to the legacy firmware");
    // Split screen needs two files, which the saved state does not have.
    rig.run(&["show", "a.mp4.h264_2240x1080"]).ok();
    rig.run(&["display", "set", "--mode", "split"])
        .expect(3)
        .complains("this screen mode needs 2 media file(s)");
    for (args, what) in [
        (&["fans"][..], "fan readings"),
        (&["display", "reboot"], "reboot"),
        (&["raw", "conn"], "raw legacy commands"),
    ] {
        rig.run(args)
            .expect(3)
            .complains(&format!("{what} is not available on the KANALI firmware"));
    }
}

#[test]
fn show_selects_media_and_presets() {
    let rig = KanaliRig::new();
    rig.run(&["show", "clip.mp4.h264_2240x1080", "--play", "loop"])
        .ok()
        .says("Showing clip.mp4.h264_2240x1080 (Loop, applied)");
    assert_eq!(rig.showing(), "clip.mp4.h264_2240x1080");
    let work = rig.display.with(|d| d.config.work_config.clone().unwrap());
    assert_eq!(work.loop_mode, 1, "loop all");
    // The configuration is activated with an overlay layout.
    assert!(rig.display.count("overlay_layout") >= 1);

    rig.run(&["show", "preset:3"]).ok();
    assert_eq!(rig.showing(), "default_04.mp4.h264_2240x1080");
}

#[test]
fn metrics_set_leases_the_overlay() {
    let rig = KanaliRig::new();
    rig.run(&[
        "metrics",
        "set",
        "--labels",
        "cpu-temp,gpu-usage",
        "--badges",
        "cpu",
    ])
    .ok()
    .says("overlay shows CPU Temperature, GPU Usage");
    // The layout goes out without waiting for an answer, so the command can
    // finish before the display has read it.
    assert!(
        rig.display
            .wait_for(Duration::from_secs(10), |d| !d.layouts.is_empty()),
        "the overlay layout never arrived"
    );
    let layout = rig.display.with(|d| d.layouts[0].clone());
    assert!(!layout.label_groups.is_empty(), "{layout:?}");
    rig.run(&["metrics", "set", "--labels", "cpu-voltage"])
        .expect(2)
        .complains("the KANALI overlay has no \"CPU Voltage\"");
    // CPU power is shown by this firmware alone.
    rig.run(&["metrics", "set", "--labels", "cpu-power"]).ok();
    rig.run(&["metrics", "set", "--clear"])
        .ok()
        .says("overlay cleared");
}

#[test]
fn media_is_listed_uploaded_replaced_and_removed() {
    let rig = KanaliRig::new();
    rig.run(&["media", "ls"])
        .ok()
        .says("No user media on the display.")
        .says(&format!(
            "0 user file(s), 0 B; 1 factory preset(s): {PRESET}"
        ));
    rig.run(&["media", "export", "x.mp4.h264_2240x1080"])
        .expect(3)
        .complains("pulling media from a KANALI display is not implemented");
    rig.run(&["media", "rm", PRESET])
        .expect(5)
        .complains("is a factory preset and cannot be removed");
    rig.run(&["media", "rm", "missing.mp4.h264_2240x1080"])
        .expect(5)
        .complains("is not on the display");
    if !ffmpeg_available() {
        return;
    }
    let clip = media::clip(&rig.sandbox.work().join("clip.mov"), 320, 240, 1.0);
    let clip = clip.to_str().unwrap();
    let json = rig
        .run(&["media", "upload", "--json", "--show", clip])
        .ok()
        .json();
    let name = "clip.mp4.h264_2240x1080";
    assert_eq!(json["name"], name);
    assert_eq!(json["target"]["id"], "kanali-panorama");
    assert_eq!(json["shown"], true);
    let uploaded = rig
        .display
        .with(|d| d.files.get(name).cloned())
        .expect("uploaded");
    assert_eq!(json["size"], uploaded.len());
    assert!(uploaded.starts_with(&[0, 0, 0, 1]), "a raw H.264 stream");
    assert_eq!(rig.showing(), name);
    assert_eq!(rig.state()["screen"]["media"], json!([name]));

    let listed = rig.run(&["media", "ls", "--json"]).ok().json();
    assert_eq!(listed["files"][0]["name"], name);
    rig.run(&["media", "upload", clip])
        .expect(5)
        .complains("already exists on the display; pass --replace");
    rig.run(&["media", "upload", "--name", "default_01", clip])
        .expect(5)
        .complains("is a factory preset; pass --name to choose another name");

    let second = media::clip(&rig.sandbox.work().join("second.mov"), 160, 120, 1.0);
    rig.run(&["media", "replace", name, second.to_str().unwrap()])
        .ok()
        .says(&format!("uploaded {name}"));
    assert_ne!(rig.display.with(|d| d.files[name].clone()), uploaded);
    rig.run(&["media", "replace", "clip.mp4", second.to_str().unwrap()])
        .expect(2)
        .complains("names on this display end with .h264_2240x1080");

    let removed = rig.run(&["media", "rm", "--json", name]).ok().json();
    assert_eq!(removed, json!({"removed": [name]}));
    assert!(rig.display.with(|d| d.files.is_empty()));
}

#[test]
fn a_refused_upload_keeps_its_encode() {
    let rig = KanaliRig::new();
    if !ffmpeg_available() {
        return;
    }
    rig.display.with(|d| d.transfer_status = 1);
    let clip = media::clip(&rig.sandbox.work().join("clip.mov"), 160, 120, 1.0);
    rig.run(&["media", "upload", clip.to_str().unwrap()])
        .expect(3)
        .complains("not enough space on the device")
        .complains("`tryxctl op retry ");
    let journal = common::read_json(rig.sandbox.journal());
    assert_eq!(journal[0]["outcome"], "failed");
    assert_eq!(journal[0]["target"], "kanali-panorama");
    rig.display.with(|d| d.transfer_status = 0);
    let id = journal[0]["id"].as_str().unwrap();
    rig.run(&["op", "retry", id])
        .ok()
        .complains("reusing the encode kept from a previous attempt");
    assert!(
        rig.display
            .with(|d| d.files.contains_key("clip.mp4.h264_2240x1080"))
    );
}

#[test]
fn the_turris_only_takes_uploads() {
    let rig = KanaliRig::with_product(TURRIS_620);
    rig.run(&["info"])
        .expect(3)
        .complains("device information on this product is not available");
    rig.run(&["media", "ls"])
        .expect(3)
        .complains("the media catalog");
    assert!(
        rig.display.received().is_empty(),
        "nothing reached the display"
    );
    if !ffmpeg_available() {
        return;
    }
    let picture = media::picture(&rig.sandbox.work().join("still.png"), 320, 180);
    let json = rig
        .run(&["media", "upload", "--json", picture.to_str().unwrap()])
        .ok()
        .json();
    assert_eq!(json["name"], "still.png.h264_1280x720");
    assert_eq!(json["target"]["id"], "kanali-turris");
    let (bytes, tracks) = rig.display.with(|d| {
        (
            d.files["still.png.h264_1280x720"].clone(),
            d.transfer_tracks.clone(),
        )
    });
    let header = tryx_media::mxhd::read_header(&bytes).expect("the Turris media header");
    assert_eq!(header.kind, tryx_media::mxhd::MediaKind::Image);
    assert_eq!((header.width, header.height, header.frames), (1280, 720, 1));
    assert_eq!(tracks, [981_521], "the fixed transfer track id");
}

#[test]
fn a_legacy_display_is_preferred_to_ask_about() {
    // With both kinds connected, commands take the legacy display unless a
    // KANALI one is named.
    let sandbox = Sandbox::new();
    let socket = sandbox.plug_kanali("3-4", tryx_testkit::kanali::PANORAMA_SE, SERIAL);
    let kanali = tryx_testkit::FakeKanali::start(&socket);
    let legacy_node = sandbox.root().join("not-a-tty");
    std::fs::write(&legacy_node, b"").unwrap();
    sandbox.plug("3-12", "LEGACY0000000001", "ttyACM0", &legacy_node);
    // The legacy port is a plain file, so opening it fails: that it was
    // tried at all shows the choice.
    run(&sandbox, &["info"]).expect(3).complains("dev/ttyACM0");
    assert!(kanali.received().is_empty());
    let json = run(&sandbox, &["info", "--json", "--device", KANALI_ID])
        .ok()
        .json();
    assert_eq!(json["device"]["serial_number"], SERIAL);
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn metrics_push_keeps_the_session_and_sends_values() {
        let rig = KanaliRig::new();
        rig.run(&["metrics", "set", "--labels", "cpu-usage,mem-usage"])
            .ok();
        rig.run(&["metrics", "push", "--once", "--json"]).ok();
        assert!(
            rig.display
                .wait_for(Duration::from_secs(10), |d| d.received.contains(&"ping")),
            "{:?}",
            rig.display.received()
        );
        assert!(
            rig.display
                .wait_for(Duration::from_secs(2), |d| !d.metrics.is_empty()),
            "{:?}",
            rig.display.received()
        );
    }
}
