use assert_cmd::Command;

fn tryx() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tryx"))
}

#[test]
fn help_lists_commands() {
    let output = tryx().arg("--help").assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(stdout.contains("doctor"), "{stdout}");
    assert!(stdout.contains("devices"), "{stdout}");
}

#[test]
fn devices_json_has_both_device_lists() {
    let output = tryx().args(["devices", "--json"]).assert().success();
    let value: serde_json::Value = serde_json::from_slice(&output.get_output().stdout).unwrap();
    assert!(value["printer_devices"].is_array());
    assert!(value["legacy_devices"].is_array());
}

#[test]
fn doctor_json_reports_checks_and_matches_exit_status() {
    let output = tryx().args(["doctor", "--json"]).output().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let names: Vec<&str> = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"ffmpeg"), "{names:?}");
    assert!(names.contains(&"ffprobe"), "{names:?}");
    assert_eq!(value["ok"].as_bool().unwrap(), output.status.success());
    if !output.status.success() {
        assert_eq!(output.status.code(), Some(4));
    }
}

#[test]
fn info_on_a_missing_port_is_a_device_failure() {
    let output = tryx()
        .args(["info", "--tty", "/nonexistent/ttyTRYX"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("/nonexistent/ttyTRYX"), "{stderr}");
}

#[test]
fn display_set_without_a_setting_is_a_usage_error() {
    let output = tryx().args(["display", "set"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--brightness"));
}

#[test]
fn media_ls_needs_adb_and_a_display() {
    let output = tryx()
        .args(["media", "ls", "--tty", "/nonexistent/ttyTRYX"])
        .output()
        .unwrap();
    // 4 without adb installed, 3 when adb runs but sees no display.
    assert!(
        matches!(output.status.code(), Some(3) | Some(4)),
        "{output:?}"
    );
}

#[test]
fn show_rejects_unsafe_media_names_before_touching_a_device() {
    let output = tryx()
        .args(["show", "../etc/passwd", "--tty", "/nonexistent/ttyTRYX"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("not safe"));
}

/// Writes a small test image with ffmpeg, or returns None when ffmpeg is
/// not installed (the check commands then fail with exit code 4 instead).
fn sample_png(name: &str) -> Option<std::path::PathBuf> {
    let ffmpeg = which::which("ffmpeg").ok()?;
    let dir = std::env::temp_dir().join(format!("tryx-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    let status = std::process::Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x32:rate=1:duration=1",
            "-frames:v",
            "1",
        ])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

#[test]
fn media_check_reports_findings_and_strictness() {
    let Some(png) = sample_png("tiny.png") else {
        let output = tryx()
            .args(["media", "check", "/nonexistent.png"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(4));
        return;
    };
    let output = tryx().args(["media", "check"]).arg(&png).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("TRYX-M-RESOLUTION"), "{stdout}");
    assert!(
        stdout.contains("plan: convert to a 1920×960 PNG"),
        "{stdout}"
    );

    let strict = tryx()
        .args(["media", "check", "--strict"])
        .arg(&png)
        .output()
        .unwrap();
    assert_eq!(strict.status.code(), Some(5));

    let json = tryx()
        .args(["media", "check", "--json"])
        .arg(&png)
        .output()
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(value[0]["report"]["kind"], "image");
    assert_eq!(value[0]["plan"]["action"], "encode");

    let missing = tryx()
        .args(["media", "check", "/nonexistent/file.mp4"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&missing.stdout).contains("TRYX-M-UNREADABLE"));
}

#[test]
fn media_convert_dry_run_prints_the_command() {
    let Some(png) = sample_png("dry.png") else {
        return;
    };
    let output = tryx()
        .args(["media", "convert", "--dry-run", "--mode", "fill"])
        .arg(&png)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("command: ffmpeg"), "{stdout}");
    assert!(stdout.contains("-frames:v 1"), "{stdout}");
    let bad = tryx()
        .args(["media", "convert", "--dry-run", "--mode", "squash"])
        .arg(&png)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn media_upload_dry_run_never_touches_a_device() {
    let Some(png) = sample_png("upload.png") else {
        return;
    };
    let output = tryx()
        .args([
            "media",
            "upload",
            "--dry-run",
            "--tty",
            "/nonexistent/ttyTRYX",
        ])
        .arg(&png)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn metrics_set_validates_before_touching_a_device() {
    let too_many = tryx()
        .args([
            "metrics",
            "set",
            "--labels",
            "cpu-temp,gpu-temp,cpu-usage,gpu-usage",
            "--tty",
            "/nonexistent/ttyTRYX",
        ])
        .output()
        .unwrap();
    assert_eq!(too_many.status.code(), Some(2));
    let bad_color = tryx()
        .args([
            "metrics",
            "set",
            "--labels",
            "cpu-temp",
            "--color",
            "red",
            "--media",
            "a.mp4",
            "--tty",
            "/nonexistent/ttyTRYX",
        ])
        .output()
        .unwrap();
    assert_eq!(bad_color.status.code(), Some(2));
}

#[test]
fn metrics_status_is_linux_only() {
    let output = tryx()
        .args(["metrics", "status", "--json"])
        .output()
        .unwrap();
    if cfg!(target_os = "linux") {
        assert!(output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(value["cpu"].is_object());
    } else {
        assert_eq!(output.status.code(), Some(4));
    }
}
