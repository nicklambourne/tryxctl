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
