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
