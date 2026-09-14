//! Moving media to and from a fake display over a fake adb, and the journal
//! that lets a failed transfer be retried without encoding again.

#![cfg(unix)]

mod common;

use common::{Rig, nothing_staged};
use serde_json::json;
use tryx_testkit::cm01::SERIAL;
use tryx_testkit::media::{self, ffmpeg_available};

#[test]
fn media_ls_lists_files_space_and_presets() {
    let rig = Rig::new();
    rig.run(&["media", "ls"])
        .ok()
        .says("No user media on the display.");
    rig.adb.put("clip.mp4", &[0u8; 3 * 1024 * 1024]);
    rig.adb.put("still.png", &[0u8; 2048]);
    rig.run(&["media", "ls"])
        .ok()
        .says("clip.mp4   3.0 MiB")
        .says("still.png  2.0 KiB")
        .says("2 file(s), 3.0 MiB in /sdcard/pcMedia/; 7.1 GiB free of 11.1 GiB on the display")
        .says("preset:3 Quantum time capsule");
    let json = rig.run(&["media", "ls", "--json"]).ok().json();
    assert_eq!(json["adb_serial"], SERIAL);
    assert_eq!(
        json["files"][0],
        json!({"name": "clip.mp4", "size": 3 * 1024 * 1024})
    );
    assert_eq!(json["storage"]["available_kib"], 7_462_912);
    assert_eq!(json["presets"].as_array().unwrap().len(), 6);
    // Discovery found the display, so adb is asked for its transport only.
    assert!(
        rig.adb
            .calls()
            .iter()
            .any(|call| call.starts_with(&format!("-s {SERIAL} shell")))
    );
    assert!(rig.display.requests().is_empty(), "no serial traffic");
}

#[test]
fn a_port_named_by_another_path_still_reaches_its_files() {
    let rig = Rig::new();
    rig.adb.put("clip.mp4", b"x");
    // The pseudo-terminal itself rather than the discovered link to it, as
    // with /dev/ttyACM0 against /dev/serial/by-id/....
    let direct = rig.display.port().to_string_lossy().into_owned();
    let json = rig
        .run(&["media", "ls", "--json", "--tty", &direct])
        .ok()
        .json();
    assert_eq!(json["adb_serial"], SERIAL);
    assert_eq!(json["files"][0]["name"], "clip.mp4");
}

#[test]
fn adb_trouble_is_explained() {
    let rig = Rig::new();
    rig.adb.set_state("unauthorized");
    rig.run(&["media", "ls"]).expect(3).complains(&format!(
        "adb reports the display ({SERIAL}) as unauthorized"
    ));
    rig.adb
        .set_state("no permissions (missing udev rules? user is in the plugdev group)");
    rig.run(&["media", "ls"])
        .expect(3)
        .complains("run `adb kill-server` and retry");
    rig.adb.set_listing(Some(""));
    rig.run(&["media", "ls"])
        .expect(3)
        .complains("adb sees no devices");
    // A phone is plugged in as well, and the display's transport is missing.
    rig.adb.set_listing(Some(
        "R5CT1234567\tdevice usb:1-1 product:beyond model:SM_G973F\n",
    ));
    rig.run(&["media", "ls"])
        .expect(3)
        .complains("none matches the display's serial or USB port");
    assert!(
        !rig.adb
            .calls()
            .iter()
            .any(|call| call.starts_with("-s R5CT1234567")),
        "the phone was never used: {:?}",
        rig.adb.calls()
    );
    // Found by USB port when the serial differs.
    rig.adb
        .set_listing(Some("0123456789ABCDEF\tdevice usb:3-12 product:cm01_se\n"));
    rig.run(&["media", "ls"])
        .expect(3)
        .complains("device '0123456789ABCDEF' not found");

    let mut command = rig.sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
    command
        .args(["media", "ls"])
        .env("PATH", rig.sandbox.root().join("empty"));
    let output = tryx_testkit::sandbox::output(command);
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("adb is not installed"));
}

#[test]
fn media_export_copies_a_file_off_the_display() {
    let rig = Rig::new();
    let bytes: Vec<u8> = (0..=255).cycle().take(10_000).collect();
    rig.adb.put("clip.mp4", &bytes);
    rig.run(&["media", "export", "clip.mp4"])
        .ok()
        .says("exported clip.mp4 to clip.mp4 (9.8 KiB, sha256 ");
    assert_eq!(
        std::fs::read(rig.sandbox.work().join("clip.mp4")).unwrap(),
        bytes
    );

    rig.run(&["media", "export", "clip.mp4"])
        .expect(5)
        .complains("clip.mp4 exists; pass --force to overwrite it");
    rig.run(&["media", "export", "clip.mp4", "--force"]).ok();
    rig.run(&["media", "export", "missing.mp4"])
        .expect(5)
        .complains("missing.mp4 is not on the display");

    // Into a directory: the file keeps its name there.
    let dir = rig.sandbox.work().join("exports");
    std::fs::create_dir(&dir).unwrap();
    let json = rig
        .run(&[
            "media",
            "export",
            "--json",
            "clip.mp4",
            "-o",
            dir.to_str().unwrap(),
        ])
        .ok()
        .json();
    assert_eq!(json["path"], dir.join("clip.mp4").to_str().unwrap());
    assert_eq!(json["size"], 10_000);
    assert_eq!(std::fs::read(dir.join("clip.mp4")).unwrap(), bytes);
    rig.run(&["media", "export", "clip.mp4", "-o", dir.to_str().unwrap()])
        .expect(5)
        .complains("exists; pass --force");

    // A pull that stops short leaves nothing behind.
    rig.adb.truncate_pulls(Some(4096));
    let short = rig.sandbox.work().join("short.mp4");
    rig.run(&["media", "export", "clip.mp4", "-o", short.to_str().unwrap()])
        .expect(3)
        .complains("pulled 4096 of 10000 bytes; the copy was removed");
    assert!(!short.exists());
}

#[test]
fn media_upload_encodes_pushes_and_journals() {
    let rig = Rig::new();
    if !ffmpeg_available() {
        return;
    }
    let picture = media::picture(&rig.sandbox.work().join("Holiday Photo.jpg"), 320, 240);
    let json = rig
        .run(&["media", "upload", "--json", picture.to_str().unwrap()])
        .ok()
        .json();
    assert_eq!(json["name"], "Holiday-Photo.png");
    assert_eq!(json["action"], "encode");
    assert_eq!(json["shown"], false);
    let pushed = rig
        .adb
        .read("Holiday-Photo.png")
        .expect("the upload is on the display");
    assert_eq!(json["size"], pushed.len());
    assert!(
        nothing_staged(&rig.sandbox),
        "the staged encode was removed"
    );

    let journal = rig.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["kind"], "upload");
    assert_eq!(journal[0]["outcome"], "ok");
    assert_eq!(journal[0]["remote"], "Holiday-Photo.png");
    assert_eq!(journal[0]["size"], pushed.len());
    assert_eq!(journal[0]["sha256"], json["sha256"]);

    rig.run(&["media", "upload", picture.to_str().unwrap()])
        .expect(5)
        .complains("Holiday-Photo.png already exists on the display; pass --replace");
    rig.run(&[
        "media",
        "upload",
        "--replace",
        "--name",
        "second",
        picture.to_str().unwrap(),
    ])
    .ok()
    .says("uploaded second.png");
    assert_eq!(rig.adb.names(), ["Holiday-Photo.png", "second.png"]);
    rig.run(&["op", "ls"])
        .ok()
        .says("upload  Holiday-Photo.png  ok")
        .says("upload  second.png         ok");
    assert!(
        rig.display.requests().is_empty(),
        "no serial traffic without --show"
    );
}

#[test]
fn a_failed_upload_keeps_its_encode_for_a_retry() {
    let rig = Rig::new();
    if !ffmpeg_available() {
        return;
    }
    let clip = media::clip(&rig.sandbox.work().join("clip.mov"), 320, 240, 1.0);
    rig.adb.set_available_kib(1024);
    rig.run(&["media", "upload", clip.to_str().unwrap()])
        .expect(5)
        .complains("not enough space on the display")
        .complains("`tryxctl op retry ");
    assert!(rig.adb.names().is_empty());
    let journal = rig.journal();
    assert_eq!(journal[0]["outcome"], "failed");
    let cached = journal[0]["cached"]
        .as_str()
        .expect("an encode is kept")
        .to_string();
    assert!(std::path::Path::new(&cached).is_file());
    assert!(cached.starts_with(rig.sandbox.encodes().to_str().unwrap()));
    assert!(nothing_staged(&rig.sandbox));
    let id = journal[0]["id"].as_str().unwrap().to_string();
    rig.run(&["op", "ls"])
        .ok()
        .says("failed")
        .says("kept")
        .says("not enough space");

    // Only failed transfers can be retried, by a unique id or prefix.
    rig.run(&["op", "retry", ""]).expect(2);
    rig.adb.set_available_kib(7_462_912);
    rig.adb.fail(
        "push",
        Some("adb: error: failed to copy 'x' to '/sdcard/pcMedia/clip.mp4': remote Broken pipe"),
    );
    rig.run(&["op", "retry", &id])
        .expect(3)
        .complains("reusing the encode kept from a previous attempt")
        .complains("Broken pipe");
    let journal = rig.journal();
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[1]["outcome"], "failed");
    assert_ne!(
        journal[0]["id"], journal[1]["id"],
        "each attempt has its own id"
    );
    assert!(std::path::Path::new(&cached).is_file(), "still kept");

    rig.adb.fail("push", None);
    let retry_id = journal[1]["id"].as_str().unwrap().to_string();
    rig.run(&["op", "retry", &retry_id[..6]])
        .ok()
        .says("retrying upload of")
        .complains("reusing the encode kept from a previous attempt")
        .says("uploaded clip.mp4");
    assert!(rig.adb.read("clip.mp4").is_some());
    assert!(
        !std::path::Path::new(&cached).exists(),
        "the kept encode was used up"
    );
    let journal = rig.journal();
    assert_eq!(journal[2]["outcome"], "ok");
    rig.run(&["op", "retry", journal[2]["id"].as_str().unwrap()])
        .expect(2)
        .complains("is complete; only failed ones can be retried");

    rig.run(&["op", "clear", "--journal"])
        .ok()
        .says("and the journal");
    assert!(rig.journal().is_empty());
}

#[test]
fn op_clear_removes_kept_encodes() {
    let rig = Rig::new();
    if !ffmpeg_available() {
        return;
    }
    let picture = media::picture(&rig.sandbox.work().join("a.png"), 64, 32);
    rig.adb.fail("push", Some("adb: error: closed"));
    rig.run(&["media", "upload", picture.to_str().unwrap()])
        .expect(3);
    assert_eq!(std::fs::read_dir(rig.sandbox.encodes()).unwrap().count(), 1);
    let json = rig.run(&["op", "clear", "--json"]).ok().json();
    assert_eq!(
        json,
        json!({"removed_encodes": 1, "journal_cleared": false})
    );
    let journal = rig.journal();
    assert_eq!(journal.len(), 1, "the journal stays");
    assert_eq!(journal[0]["cached"], serde_json::Value::Null);
}

#[test]
fn media_replace_swaps_a_file_in_place() {
    let rig = Rig::new();
    if !ffmpeg_available() {
        return;
    }
    rig.adb.put("clip.png", b"old");
    let picture = media::picture(&rig.sandbox.work().join("new.png"), 64, 32);
    let picture = picture.to_str().unwrap();
    let json = rig
        .run(&["media", "replace", "--json", "clip.png", picture])
        .ok()
        .json();
    assert_eq!(json["name"], "clip.png");
    assert_eq!(json["reloaded"], false);
    let replaced = rig.adb.read("clip.png").unwrap();
    assert_ne!(replaced, b"old");
    assert_eq!(json["size"], replaced.len());
    assert_eq!(rig.adb.names(), ["clip.png"], "no temporary file is left");
    let pushes: Vec<String> = rig
        .adb
        .calls()
        .into_iter()
        .filter(|c| c.contains(" push "))
        .collect();
    assert_eq!(pushes.len(), 1);
    assert!(
        !pushes[0].ends_with("/sdcard/pcMedia/clip.png"),
        "pushed under a temporary name first: {}",
        pushes[0]
    );
    assert_eq!(rig.journal()[0]["kind"], "replace");
    assert_eq!(rig.journal()[0]["outcome"], "ok");
    assert!(nothing_staged(&rig.sandbox));

    rig.run(&["media", "replace", "clip.mp4", picture])
        .expect(2)
        .complains("the prepared file would be called clip.mp4.png, not clip.mp4");
    rig.run(&["media", "replace", "gone.png", picture])
        .expect(5)
        .complains("gone.png is not on the display; use `media upload`");

    // A copy that arrives short replaces nothing and leaves nothing behind.
    rig.adb.truncate_pushes(Some(10));
    rig.run(&["media", "replace", "clip.png", picture])
        .expect(3)
        .complains("the pushed copy is 10 bytes, expected")
        .complains("nothing was replaced");
    assert_eq!(rig.adb.names(), ["clip.png"]);
    assert_eq!(rig.adb.read("clip.png").unwrap(), replaced);
    assert_eq!(rig.journal().last().unwrap()["outcome"], "failed");
}

#[test]
fn media_upload_rejects_what_cannot_be_prepared() {
    let rig = Rig::new();
    if !ffmpeg_available() {
        return;
    }
    let notes = rig.sandbox.work().join("notes.txt");
    std::fs::write(&notes, "not media").unwrap();
    rig.run(&["media", "upload", notes.to_str().unwrap()])
        .expect(5)
        .says("TRYX-M-UNREADABLE")
        .complains("the file cannot be prepared for the display");
    rig.run(&["media", "replace", "notes.png", notes.to_str().unwrap()])
        .expect(5);
    assert!(rig.adb.calls().iter().all(|call| !call.contains("push")));
    assert!(rig.journal().is_empty(), "nothing was attempted");
}

#[cfg(target_os = "linux")]
mod with_the_serial_port {
    use super::*;

    #[test]
    fn upload_show_plays_the_new_file() {
        let rig = Rig::new();
        if !ffmpeg_available() {
            return;
        }
        rig.save_state(json!({"screen": {"sysinfo_display": ["CPU Usage"]}, "cpu_name": "CPU", "gpu_name": "GPU"}));
        let picture = media::picture(&rig.sandbox.work().join("still.png"), 64, 32);
        rig.run(&["media", "upload", "--show", picture.to_str().unwrap()])
            .ok()
            .says("showing still.png");
        let screen = rig.display.received("waterBlockScreenId")[0].json();
        assert_eq!(screen["media"], json!(["still.png"]));
        assert_eq!(rig.state()["screen"]["media"], json!(["still.png"]));
        assert_eq!(
            rig.state()["screen"]["sysinfo_display"],
            json!(["CPU Usage"])
        );
    }

    #[test]
    fn replacing_the_file_on_screen_reloads_it() {
        let rig = Rig::new();
        if !ffmpeg_available() {
            return;
        }
        rig.adb.put("still.png", b"old");
        rig.save_state(
            json!({"screen": {"media": ["still.png"]}, "cpu_name": "CPU", "gpu_name": "GPU"}),
        );
        let picture = media::picture(&rig.sandbox.work().join("still.png"), 64, 32);
        rig.run(&["media", "replace", "still.png", picture.to_str().unwrap()])
            .ok()
            .says("the display reloaded it");
        assert_eq!(rig.display.received("config").len(), 1);
    }

    #[test]
    fn media_rm_asks_the_firmware_then_cleans_up() {
        let rig = Rig::new();
        rig.adb.put("a.png", b"a");
        rig.adb.put("b.mp4", b"b");
        rig.adb.put("c.mp4", b"c");
        rig.run(&["media", "rm", "a.png", "b.mp4"])
            .ok()
            .says("removed a.png")
            .says("removed b.mp4");
        assert_eq!(rig.adb.names(), ["c.mp4"]);
        let deletes = rig.display.received("mediaDelete");
        assert_eq!(deletes.len(), 2);
        assert_eq!(deletes[0].json(), json!({"include": ["a.png"]}));
        rig.run(&["media", "rm", "c.mp4", "missing.mp4"])
            .expect(5)
            .complains("missing.mp4 is not on the display");
        assert_eq!(
            rig.adb.names(),
            ["c.mp4"],
            "nothing is removed when a name is wrong"
        );
        let json = rig.run(&["media", "rm", "--json", "c.mp4"]).ok().json();
        assert_eq!(json, json!({"removed": ["c.mp4"]}));
    }
}
