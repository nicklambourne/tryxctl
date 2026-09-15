//! Commands that need no display, and the checks every command makes before
//! it looks for one. Each run is confined to a sandbox, so neither the
//! host's displays nor a daemon it runs can change the outcome.

mod common;

use common::run;
use tryx_testkit::Sandbox;
use tryx_testkit::media::{self, ffmpeg_available};

#[test]
fn help_lists_commands() {
    let sandbox = Sandbox::new();
    let help = run(&sandbox, &["--help"]);
    help.ok().says("doctor").says("devices").says("media");
    run(&sandbox, &["--version"])
        .ok()
        .says(env!("CARGO_PKG_VERSION"));
    run(&sandbox, &["no-such-command"]).expect(2);
}

#[test]
fn devices_lists_nothing_in_an_empty_device_tree() {
    let sandbox = Sandbox::new();
    let json = run(&sandbox, &["devices", "--json"]).ok().json();
    assert_eq!(json["printer_devices"], serde_json::json!([]));
    assert_eq!(json["legacy_devices"], serde_json::json!([]));
    run(&sandbox, &["devices"])
        .ok()
        .says("No TRYX displays found.");
}

#[test]
fn doctor_json_reports_checks_and_matches_exit_status() {
    let sandbox = Sandbox::new();
    let output = run(&sandbox, &["doctor", "--json"]);
    let value = output.json();
    let checks = value["checks"].as_array().unwrap();
    let names: Vec<&str> = checks
        .iter()
        .map(|check| check["name"].as_str().unwrap())
        .collect();
    for name in ["ffmpeg", "ffprobe", "adb", "devices"] {
        assert!(names.contains(&name), "{names:?}");
    }
    assert_eq!(value["ok"].as_bool().unwrap(), output.code == Some(0));
    if output.code != Some(0) {
        output.expect(4);
    }
    let devices = checks.iter().find(|c| c["name"] == "devices").unwrap();
    assert_eq!(devices["status"], "warn");
    assert_eq!(devices["detail"], "no TRYX display connected");

    let human = run(&sandbox, &["doctor"]);
    assert_eq!(human.code, output.code);
    assert!(
        human.stdout.contains("All checks passed.")
            || human.stdout.contains("One or more checks failed."),
        "{}",
        human.stdout
    );
}

#[test]
fn doctor_fails_without_ffmpeg() {
    let sandbox = Sandbox::new();
    let mut command = sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
    command
        .args(["doctor", "--json"])
        .env("PATH", sandbox.bin());
    let output = tryx_testkit::sandbox::output(command);
    assert_eq!(output.status.code(), Some(4));
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = |name: &str| {
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == name)
            .map(|check| check["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status("ffmpeg").as_deref(), Some("fail"));
    assert_eq!(status("libx264 encoder").as_deref(), Some("skip"));
    assert_eq!(status("ffprobe").as_deref(), Some("fail"));
    assert_eq!(status("adb").as_deref(), Some("warn"));
}

#[test]
fn doctor_hints_give_the_distribution_s_commands() {
    // Runs doctor with nothing on PATH, so ffmpeg and adb are missing.
    let doctor = |os_release: Option<&str>, args: &[&str]| {
        let sandbox = Sandbox::new();
        if let Some(contents) = os_release {
            std::fs::write(sandbox.os_release(), contents).unwrap();
        }
        let mut command = sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
        command.args(args).env("PATH", sandbox.bin());
        let output = tryx_testkit::sandbox::output(command);
        String::from_utf8(output.stdout).unwrap()
    };
    let checks = |os_release: Option<&str>| {
        let json: serde_json::Value =
            serde_json::from_str(&doctor(os_release, &["doctor", "--json"])).unwrap();
        move |name: &str| {
            json["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|check| check["name"] == name)
                .unwrap_or_else(|| panic!("no {name} check"))
                .clone()
        }
    };
    let hint = |check: serde_json::Value| check["hint"].as_str().unwrap_or_default().to_string();

    for (os_release, distribution, ffmpeg, adb) in [
        (
            "PRETTY_NAME=\"Ubuntu 24.04.3 LTS\"\nNAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n",
            "Ubuntu 24.04.3 LTS",
            "sudo apt install ffmpeg",
            "sudo apt install adb",
        ),
        (
            "NAME=\"Fedora Linux\"\nID=fedora\nPRETTY_NAME=\"Fedora Linux 42 (Workstation Edition)\"\n",
            "Fedora Linux 42 (Workstation Edition)",
            "sudo dnf install --allowerasing ffmpeg",
            "sudo dnf install android-tools",
        ),
        (
            "NAME=\"EndeavourOS\"\nPRETTY_NAME=\"EndeavourOS\"\nID=\"endeavouros\"\nID_LIKE=\"arch\"\n",
            "EndeavourOS",
            "sudo pacman -S --needed ffmpeg",
            "sudo pacman -S --needed android-tools",
        ),
        (
            "NAME=\"openSUSE Tumbleweed\"\nID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\nPRETTY_NAME=\"openSUSE Tumbleweed\"\n",
            "openSUSE Tumbleweed",
            "sudo zypper install --from packman ffmpeg",
            "sudo zypper install android-tools",
        ),
        (
            "NAME=NixOS\nID=nixos\nPRETTY_NAME=\"NixOS 25.05 (Warbler)\"\n",
            "NixOS 25.05 (Warbler)",
            "nix profile install nixpkgs#ffmpeg",
            "nix profile install nixpkgs#android-tools",
        ),
    ] {
        let check = checks(Some(os_release));
        assert_eq!(check("distribution")["status"], "ok");
        assert_eq!(check("distribution")["detail"], distribution);
        let ffmpeg_hint = hint(check("ffmpeg"));
        assert!(
            ffmpeg_hint.contains(ffmpeg),
            "{distribution}: {ffmpeg_hint}"
        );
        let ffprobe_hint = hint(check("ffprobe"));
        assert!(
            ffprobe_hint.contains(ffmpeg),
            "{distribution}: {ffprobe_hint}"
        );
        let adb_hint = hint(check("adb"));
        assert!(adb_hint.contains(adb), "{distribution}: {adb_hint}");
    }

    // A distribution without its own commands, and none at all.
    let gentoo = checks(Some(
        "NAME=Gentoo\nID=gentoo\nPRETTY_NAME=\"Gentoo Linux\"\n",
    ));
    assert_eq!(
        gentoo("distribution")["detail"],
        "Gentoo Linux, which the hints have no specific commands for"
    );
    assert!(hint(gentoo("ffmpeg")).starts_with("Install ffmpeg with the libx264 encoder"));
    let unknown = checks(None);
    assert_eq!(unknown("distribution")["status"], "skip");
    assert!(!hint(unknown("adb")).contains("sudo"));

    // Each step of a hint is on its own line in the report.
    let report = doctor(
        Some("ID=fedora\nPRETTY_NAME=\"Fedora Linux 42\"\n"),
        &["doctor"],
    );
    assert!(
        report.contains(
            "\n       Fedora's own ffmpeg-free lacks libx264, so take ffmpeg from RPM Fusion:\n       sudo dnf install https://mirrors.rpmfusion.org/"
        ),
        "{report}"
    );
    assert!(
        report.contains("\n       sudo dnf install --allowerasing ffmpeg\n"),
        "{report}"
    );
}

#[test]
fn commands_needing_a_display_fail_without_one() {
    let sandbox = Sandbox::new();
    for args in [
        &["info"][..],
        &["display", "get"],
        &["display", "set", "--brightness", "40"],
        &["display", "reboot"],
        &["show", "clip.mp4"],
        &["fans"],
        &["media", "ls"],
        &["media", "rm", "clip.mp4"],
        &["media", "export", "clip.mp4"],
        &["raw", "conn"],
    ] {
        run(&sandbox, args)
            .expect(3)
            .complains("no TRYX display connected");
    }
}

#[test]
fn info_on_a_missing_port_is_a_device_failure() {
    let sandbox = Sandbox::new();
    run(&sandbox, &["info", "--tty", "/nonexistent/ttyTRYX"])
        .expect(3)
        .complains("/nonexistent/ttyTRYX");
    run(
        &sandbox,
        &["info", "--tty", "/dev/null", "--device", "usb:001-1"],
    )
    .expect(2);
    run(&sandbox, &["info", "--device", "usb:001-1"])
        .expect(3)
        .complains("no KANALI display with id usb:001-1");
}

#[test]
fn a_port_without_a_display_has_no_files_to_reach() {
    let sandbox = Sandbox::new();
    // Not even an adb that could answer for some other Android device is
    // consulted: without a discovered display there is nothing to match.
    for args in [
        &["media", "ls", "--tty", "/nonexistent/ttyTRYX"][..],
        &[
            "media",
            "export",
            "clip.mp4",
            "--tty",
            "/nonexistent/ttyTRYX",
        ],
        &["media", "rm", "clip.mp4", "--tty", "/nonexistent/ttyTRYX"],
    ] {
        run(&sandbox, args)
            .expect(3)
            .complains("no TRYX display found at /nonexistent/ttyTRYX");
    }
}

#[test]
fn settings_are_validated_before_touching_a_device() {
    let sandbox = Sandbox::new();
    let tty = ["--tty", "/nonexistent/ttyTRYX"];
    let usage = |args: &[&str], message: &str| {
        let mut all = args.to_vec();
        all.extend(tty);
        let output = run(&sandbox, &all);
        output.expect(2);
        if !message.is_empty() {
            output.complains(message);
        }
    };
    usage(&["display", "set"], "--brightness");
    usage(&["display", "set", "--brightness", "101"], "");
    usage(
        &["display", "set", "--filter", "fog"],
        "not none, smoke, or rain",
    );
    usage(&["display", "set", "--sleep", "maybe"], "is not on or off");
    usage(
        &["display", "set", "--mode", "diagonal"],
        "not full or split",
    );
    usage(
        &["display", "set", "--waterfall", "perhaps"],
        "is not on or off",
    );
    usage(&["display", "set", "--rotate", "45"], "");
    usage(&["show", "../etc/passwd"], "not safe");
    usage(&["show", "preset:9"], "preset:1 to preset:6");
    usage(&["show", "clip.mp4", "preset:1"], "on their own");
    usage(&["show", "clip.mp4", "--play", "sideways"], "");
    usage(&["fans", "--lcd-speed", "101"], "is not 0 to 100 or auto");
    usage(&["fans", "--lcd-speed", "fast"], "is not 0 to 100 or auto");
    usage(&["raw", "brightness", "{not json"], "body is not JSON");
    usage(
        &[
            "metrics",
            "set",
            "--labels",
            "cpu-temp,gpu-temp,cpu-usage,gpu-usage",
        ],
        "at most 3",
    );
    usage(
        &["metrics", "set", "--labels", "cpu-watts"],
        "unknown metric",
    );
    usage(
        &["metrics", "set", "--labels", "cpu-temp", "--clear"],
        "exclusive",
    );
    usage(&["metrics", "set", "--area", "middle"], "left or right");
    usage(
        &["metrics", "set", "--position", "middle"],
        "--position must be",
    );
    usage(&["metrics", "set", "--align", "justify"], "--align must be");
    usage(&["metrics", "set", "--color", "red"], "is not #RRGGBB");
    usage(&["metrics", "set", "--badges", "npu"], "unknown badge");
    usage(&["metrics", "set", "--media", "a b.mp4"], "not safe");
    usage(&["metrics", "set", "--play", "sideways"], "--play must be");
    usage(&["media", "rm", "../x"], "not safe");
    usage(&["media", "export", "../x"], "not safe");
    usage(&["daemon", "--interval", "0"], "");
}

#[test]
fn play_modes_are_accepted_in_any_case() {
    let sandbox = Sandbox::new();
    for play in ["loop", "Loop", "SHUFFLE", "single"] {
        // Past the usage checks, to the missing display.
        run(
            &sandbox,
            &[
                "show",
                "clip.mp4",
                "--play",
                play,
                "--tty",
                "/nonexistent/ttyTRYX",
            ],
        )
        .expect(3);
    }
}

#[test]
fn metrics_status_is_linux_only() {
    let sandbox = Sandbox::new();
    let output = run(&sandbox, &["metrics", "status", "--json"]);
    if cfg!(target_os = "linux") {
        let value = output.ok().json();
        assert!(value["cpu"].is_object());
        assert!(value["memory"].is_object());
        run(&sandbox, &["metrics", "status"]).ok().says("CPU usage");
    } else {
        output.expect(4).complains("Linux only");
    }
}

#[test]
fn completions_and_manpage_render() {
    let sandbox = Sandbox::new();
    for shell in ["bash", "zsh", "fish"] {
        run(&sandbox, &["completions", shell]).ok().says("tryxctl");
    }
    run(&sandbox, &["manpage"])
        .ok()
        .says(".TH tryxctl")
        .says("doctor");
}

#[test]
fn the_interface_needs_a_terminal() {
    let sandbox = Sandbox::new();
    run(&sandbox, &["tui"])
        .expect(2)
        .complains("the interface needs a terminal");
}

#[test]
fn daemon_status_without_a_daemon_is_a_device_failure() {
    let sandbox = Sandbox::new();
    run(&sandbox, &["daemon", "status"])
        .expect(3)
        .complains("no daemon is listening on")
        .complains(&sandbox.socket().to_string_lossy());
    if !cfg!(target_os = "linux") {
        run(&sandbox, &["daemon"]).expect(4).complains("Linux only");
        run(&sandbox, &["daemon", "install"]).expect(4);
        run(&sandbox, &["metrics", "push", "--once"]).expect(4);
    }
}

#[test]
fn the_journal_starts_empty() {
    let sandbox = Sandbox::new();
    run(&sandbox, &["op", "ls"])
        .ok()
        .says("No transfers recorded.");
    assert_eq!(
        run(&sandbox, &["op", "ls", "--json"]).ok().json(),
        serde_json::json!([])
    );
    run(&sandbox, &["op", "retry", "abc123"])
        .expect(2)
        .complains("no transfer abc123");
    run(&sandbox, &["op", "clear"])
        .ok()
        .says("removed 0 kept encode(s)");
    let cleared = run(&sandbox, &["op", "clear", "--journal", "--json"])
        .ok()
        .json();
    assert_eq!(cleared["journal_cleared"], true);
}

#[test]
fn media_check_reports_findings_and_strictness() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        run(&sandbox, &["media", "check", "/nonexistent.png"]).expect(4);
        return;
    }
    let png = media::picture(&sandbox.work().join("tiny.png"), 64, 32);
    let png = png.to_str().unwrap();
    run(&sandbox, &["media", "check", png])
        .ok()
        .says("TRYX-M-RESOLUTION")
        .says("plan: convert to a 1920×960 PNG");
    run(&sandbox, &["media", "check", "--strict", png])
        .expect(5)
        .complains("under --strict");

    let value = run(&sandbox, &["media", "check", "--json", png])
        .ok()
        .json();
    assert_eq!(value[0]["report"]["kind"], "image");
    assert_eq!(value[0]["plan"]["action"], "encode");

    run(&sandbox, &["media", "check", png, "/nonexistent/file.mp4"])
        .expect(5)
        .says("TRYX-M-UNREADABLE")
        .complains("1 of 2 file(s)");
    let text = sandbox.work().join("notes.txt");
    std::fs::write(&text, "not media").unwrap();
    run(&sandbox, &["media", "check", text.to_str().unwrap()])
        .expect(5)
        .says("TRYX-M-UNREADABLE");
    run(
        &sandbox,
        &["media", "check", sandbox.work().to_str().unwrap()],
    )
    .expect(5)
    .says("not a regular file");
}

#[test]
fn media_options_are_validated() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        return;
    }
    let png = media::picture(&sandbox.work().join("opts.png"), 64, 32);
    let png = png.to_str().unwrap();
    for (flag, value, message) in [
        ("--mode", "squash", "unknown mode"),
        ("--rotate", "45", "is not 0, 90, 180, or 270"),
        ("--focus", "50", "is not X,Y percentages"),
        ("--bg", "red", "is not #RRGGBB"),
        ("--trim", "abc", "is not A-B, A-, or -B"),
        ("--target", "kanali-watch", "unknown target"),
        ("--zoom", "50", ""),
    ] {
        let output = run(&sandbox, &["media", "check", flag, value, png]);
        output.expect(2);
        if !message.is_empty() {
            output.complains(message);
        }
    }
    for target in ["legacy-panorama", "kanali-panorama", "kanali-turris"] {
        let value = run(
            &sandbox,
            &["media", "check", "--json", "--target", target, png],
        )
        .ok()
        .json();
        assert_eq!(value[0]["report"]["target"]["id"], target);
    }
}

#[test]
fn media_convert_writes_what_the_display_expects() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        return;
    }
    let png = media::picture(&sandbox.work().join("dry.png"), 64, 32);
    let png = png.to_str().unwrap();
    run(
        &sandbox,
        &["media", "convert", "--dry-run", "--mode", "fill", png],
    )
    .ok()
    .says("command: ffmpeg")
    .says("-frames:v 1");
    let dry = run(&sandbox, &["media", "convert", "--dry-run", "--json", png])
        .ok()
        .json();
    assert_eq!(dry["plan"]["name"], "dry.png");
    assert!(dry["command"].as_str().unwrap().starts_with("ffmpeg "));
    run(
        &sandbox,
        &["media", "convert", "--dry-run", "--mode", "squash", png],
    )
    .expect(2);

    let out = sandbox.work().join("converted.png");
    let converted = run(
        &sandbox,
        &[
            "media",
            "convert",
            "--json",
            "--name",
            "renamed",
            "-o",
            out.to_str().unwrap(),
            png,
        ],
    )
    .ok()
    .json();
    assert_eq!(converted["name"], "renamed.png");
    assert_eq!(converted["size"], std::fs::metadata(&out).unwrap().len());
    assert_eq!(converted["sha256"].as_str().unwrap().len(), 64);
    // The output is a compliant PNG now: checking it plans no work.
    let again = run(
        &sandbox,
        &["media", "check", "--json", out.to_str().unwrap()],
    )
    .ok()
    .json();
    assert_eq!(again[0]["plan"]["action"], "passthrough");

    // Without -o the display name is used, in the working directory.
    run(&sandbox, &["media", "convert", png])
        .ok()
        .says("wrote dry.png");
    assert!(sandbox.work().join("dry.png").is_file());
    run(&sandbox, &["media", "convert", "/nonexistent.png"]).expect(5);
}

#[test]
fn media_convert_encodes_a_video() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        return;
    }
    let clip = media::clip(&sandbox.work().join("clip.mov"), 320, 240, 1.0);
    let out = sandbox.work().join("clip.mp4");
    let converted = run(
        &sandbox,
        &[
            "media",
            "convert",
            "--json",
            "-o",
            out.to_str().unwrap(),
            clip.to_str().unwrap(),
        ],
    )
    .ok()
    .json();
    assert_eq!(converted["action"], "encode");
    let checked = run(
        &sandbox,
        &[
            "media",
            "check",
            "--strict",
            "--json",
            out.to_str().unwrap(),
        ],
    )
    .ok()
    .json();
    assert_eq!(checked[0]["plan"]["action"], "passthrough");
    assert_eq!(checked[0]["report"]["source"]["display_width"], 1920);
}

#[test]
fn media_upload_dry_run_never_touches_a_device() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        return;
    }
    let png = media::picture(&sandbox.work().join("upload.png"), 64, 32);
    run(
        &sandbox,
        &[
            "media",
            "upload",
            "--dry-run",
            "--tty",
            "/nonexistent/ttyTRYX",
            png.to_str().unwrap(),
        ],
    )
    .ok()
    .says("command: ffmpeg");
    let json = run(
        &sandbox,
        &[
            "media",
            "upload",
            "--dry-run",
            "--json",
            "--tty",
            "/nonexistent/ttyTRYX",
            png.to_str().unwrap(),
        ],
    )
    .ok()
    .json();
    assert_eq!(json["plan"]["action"], "encode");
    run(
        &sandbox,
        &[
            "media",
            "upload",
            "--strict",
            "--tty",
            "/nonexistent/ttyTRYX",
            png.to_str().unwrap(),
        ],
    )
    .expect(5);
}

#[test]
fn media_preview_writes_a_png_when_not_on_a_terminal() {
    let sandbox = Sandbox::new();
    if !ffmpeg_available() {
        return;
    }
    let png = media::picture(&sandbox.work().join("preview.png"), 64, 32);
    let png = png.to_str().unwrap();
    let out = sandbox.work().join("preview-out.png");
    run(
        &sandbox,
        &[
            "media",
            "preview",
            "--mode",
            "stretch",
            "-o",
            out.to_str().unwrap(),
            png,
        ],
    )
    .ok();
    assert!(out.is_file());
    run(&sandbox, &["media", "preview", png])
        .ok()
        .says("wrote preview-preview.png");
    run(&sandbox, &["media", "preview", "--sheet", png])
        .expect(2)
        .complains("--sheet needs a video");

    let clip = media::clip(&sandbox.work().join("clip.mp4"), 160, 90, 2.0);
    let clip = clip.to_str().unwrap();
    let frame = run(
        &sandbox,
        &["media", "preview", "--json", "--at", "99", clip],
    )
    .ok()
    .json();
    assert!(
        frame["at"].as_f64().unwrap() <= 2.1,
        "clamped to the clip: {frame}"
    );
    let sheet_out = sandbox.work().join("sheet.png");
    let sheet = run(
        &sandbox,
        &[
            "media",
            "preview",
            "--sheet",
            "--json",
            "-o",
            sheet_out.to_str().unwrap(),
            clip,
        ],
    )
    .ok()
    .json();
    assert_eq!(sheet["sheet"], true);
    assert!(sheet_out.is_file());

    let notes = sandbox.work().join("notes.txt");
    std::fs::write(&notes, "text").unwrap();
    run(&sandbox, &["media", "preview", notes.to_str().unwrap()])
        .expect(5)
        .complains("no picture to preview");
}
