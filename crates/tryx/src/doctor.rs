use crate::exit;
use crate::output::{self, Status};
use serde::Serialize;
use std::path::Path;
use std::process::{Command, ExitCode};
use tryx_device::discovery::{Access, Discovery, InterfaceStatus};

#[derive(Debug, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    ok: bool,
    checks: Vec<Check>,
}

pub fn run(json: bool) -> anyhow::Result<ExitCode> {
    let mut checks = Vec::new();
    check_ffmpeg(&mut checks);
    check_ffprobe(&mut checks);
    check_permissions(&mut checks);
    check_devices(&mut checks);

    let ok = !checks.iter().any(|check| check.status == Status::Fail);
    if json {
        println!("{}", serde_json::to_string_pretty(&Report { ok, checks })?);
    } else {
        for check in &checks {
            println!("{} {}: {}", check.status.badge(), check.name, check.detail);
            if let Some(hint) = &check.hint {
                println!("       {}", output::dim(hint));
            }
        }
        println!();
        println!(
            "{}",
            if ok {
                "All checks passed."
            } else {
                "One or more checks failed."
            }
        );
    }
    Ok(if ok { exit::ok() } else { exit::environment() })
}

fn check(name: &str, status: Status, detail: impl Into<String>, hint: Option<&str>) -> Check {
    Check {
        name: name.to_string(),
        status,
        detail: detail.into(),
        hint: hint.map(str::to_string),
    }
}

fn check_ffmpeg(checks: &mut Vec<Check>) {
    let Ok(path) = which::which("ffmpeg") else {
        checks.push(check(
            "ffmpeg",
            Status::Fail,
            "not found on PATH",
            Some("Install ffmpeg with the libx264 encoder (the nix shell provides it)."),
        ));
        checks.push(check(
            "libx264 encoder",
            Status::Skip,
            "ffmpeg is missing",
            None,
        ));
        return;
    };
    let version = command_stdout(&path, &["-version"])
        .as_deref()
        .and_then(parse_ffmpeg_version)
        .unwrap_or_else(|| "unknown version".to_string());
    checks.push(check(
        "ffmpeg",
        Status::Ok,
        format!("{version} at {}", path.display()),
        None,
    ));
    match command_stdout(&path, &["-hide_banner", "-encoders"]) {
        Some(encoders) if has_video_encoder(&encoders, "libx264") => {
            checks.push(check("libx264 encoder", Status::Ok, "available", None));
        }
        Some(_) => checks.push(check(
            "libx264 encoder",
            Status::Fail,
            "this ffmpeg build cannot encode H.264",
            Some(
                "Fedora's ffmpeg-free lacks libx264: enable RPM Fusion and install the full ffmpeg package.",
            ),
        )),
        None => checks.push(check(
            "libx264 encoder",
            Status::Fail,
            "could not list ffmpeg encoders",
            None,
        )),
    }
}

fn check_ffprobe(checks: &mut Vec<Check>) {
    match which::which("ffprobe") {
        Ok(path) => checks.push(check(
            "ffprobe",
            Status::Ok,
            path.display().to_string(),
            None,
        )),
        Err(_) => checks.push(check(
            "ffprobe",
            Status::Fail,
            "not found on PATH",
            Some("ffprobe ships with ffmpeg; install the same package."),
        )),
    }
}

#[cfg(target_os = "linux")]
fn check_permissions(checks: &mut Vec<Check>) {
    match find_udev_rule() {
        Some(path) => checks.push(check(
            "udev rule",
            Status::Ok,
            format!("{} covers vendor 391a", path.display()),
            None,
        )),
        None => checks.push(check(
            "udev rule",
            Status::Warn,
            "no udev rule mentions vendor 391a",
            Some("Copy packaging/udev/*.rules into /etc/udev/rules.d and replug the display."),
        )),
    }
    push_group_check(
        checks,
        "lp group",
        &["lp"],
        "printer-class USB access without a seat ACL (SSH sessions have none)",
    );
    push_group_check(
        checks,
        "serial group",
        &["dialout", "uucp"],
        "the legacy firmware's /dev/ttyACM* command port",
    );
}

#[cfg(target_os = "linux")]
fn push_group_check(checks: &mut Vec<Check>, name: &str, groups: &[&str], purpose: &str) {
    match user_in_any_group(groups) {
        Ok(Some(group)) => checks.push(check(
            name,
            Status::Ok,
            format!("current user is in {group}"),
            None,
        )),
        Ok(None) => checks.push(check(
            name,
            Status::Warn,
            format!(
                "current user is in none of {}; needed for {purpose}",
                groups.join("/")
            ),
            Some(&format!(
                "sudo usermod -aG {} $USER, then log in again.",
                groups[0]
            )),
        )),
        Err(error) => checks.push(check(
            name,
            Status::Warn,
            format!("could not determine membership: {error}"),
            None,
        )),
    }
}

#[cfg(target_os = "linux")]
fn user_in_any_group(names: &[&str]) -> anyhow::Result<Option<String>> {
    let memberships = nix::unistd::getgroups()?;
    for name in names {
        if let Some(group) = nix::unistd::Group::from_name(name)?
            && memberships.contains(&group.gid)
        {
            return Ok(Some(name.to_string()));
        }
    }
    Ok(None)
}

#[cfg(target_os = "linux")]
fn find_udev_rule() -> Option<std::path::PathBuf> {
    const RULE_DIRS: [&str; 4] = [
        "/etc/udev/rules.d",
        "/run/udev/rules.d",
        "/usr/lib/udev/rules.d",
        "/lib/udev/rules.d",
    ];
    for dir in RULE_DIRS {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rules"))
            .collect();
        paths.sort();
        for path in paths {
            if std::fs::read_to_string(&path).is_ok_and(|text| text.contains("391a")) {
                return Some(path);
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn check_permissions(checks: &mut Vec<Check>) {
    for name in ["udev rule", "lp group", "serial group"] {
        checks.push(check(name, Status::Skip, "Linux only", None));
    }
}

fn check_devices(checks: &mut Vec<Check>) {
    let discovery = match tryx_device::discover() {
        Ok(discovery) => discovery,
        Err(error) => {
            checks.push(check(
                "usb enumeration",
                Status::Fail,
                error.to_string(),
                None,
            ));
            return;
        }
    };
    if discovery.is_empty() {
        checks.push(check(
            "devices",
            Status::Warn,
            "no TRYX display connected",
            Some("Connect the cooler's USB header, then run `tryx devices`."),
        ));
        return;
    }
    push_device_checks(checks, &discovery);
}

fn push_device_checks(checks: &mut Vec<Check>, discovery: &Discovery) {
    for device in &discovery.printer_devices {
        let name = format!("device {}", device.id);
        let label = match device.product {
            Some(product) => format!("{} ({})", product.name(), device.usb_id),
            None => format!("unknown TRYX product ({})", device.usb_id),
        };
        if device.transitional {
            checks.push(check(
                &name,
                Status::Warn,
                format!(
                    "Rockchip gadget ({}): the display is booting or updating",
                    device.usb_id
                ),
                Some("Wait for it to re-enumerate as a printer-class device."),
            ));
            continue;
        }
        match (&device.access, &device.interface) {
            (Access::Accessible, InterfaceStatus::Found { .. }) => checks.push(check(
                &name,
                Status::Ok,
                format!("{label} accessible, printer interface {}", device.interface),
                None,
            )),
            (Access::Accessible, interface) => checks.push(check(
                &name,
                Status::Fail,
                format!("{label} accessible but its printer interface is {interface}"),
                Some("The device must expose one printer-class interface with one bulk IN and one bulk OUT endpoint."),
            )),
            (Access::PermissionDenied, _) => checks.push(check(
                &name,
                Status::Fail,
                format!("{label}: permission denied"),
                Some("Install the udev rules or join the lp group, then replug the display."),
            )),
            (Access::Busy, _) => checks.push(check(
                &name,
                Status::Warn,
                format!("{label}: busy, another process holds it"),
                Some("Stop the upstream tryx-panorama-runtime if it is running."),
            )),
            (Access::Error { message }, _) => {
                checks.push(check(&name, Status::Fail, format!("{label}: {message}"), None));
            }
        }
    }
    for device in &discovery.legacy_devices {
        let name = format!("device {}", device.id);
        let label = format!(
            "{} ({}) on legacy cm01 firmware",
            device.product_string, device.usb_id
        );
        match (&device.tty, &device.tty_access) {
            (Some(tty), Some(Access::Accessible)) => checks.push(check(
                &name,
                Status::Ok,
                format!(
                    "{label}: command port {tty} accessible, ADB interface {}",
                    yes_no(device.adb_interface)
                ),
                None,
            )),
            (Some(tty), Some(Access::PermissionDenied)) => checks.push(check(
                &name,
                Status::Fail,
                format!("{label}: permission denied on {tty}"),
                Some("Join the dialout (or uucp) group, then log in again."),
            )),
            (Some(tty), Some(access)) => checks.push(check(
                &name,
                Status::Fail,
                format!("{label}: {tty} {access}"),
                None,
            )),
            _ => checks.push(check(
                &name,
                Status::Warn,
                format!("{label}: no CDC ACM command port exposed"),
                Some("Replug the display; the legacy firmware should expose /dev/ttyACM*."),
            )),
        }
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "present" } else { "absent" }
}

fn command_stdout(program: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Extracts `7.1` from the first line of `ffmpeg -version`.
fn parse_ffmpeg_version(output: &str) -> Option<String> {
    output
        .lines()
        .next()?
        .strip_prefix("ffmpeg version ")?
        .split_whitespace()
        .next()
        .map(str::to_string)
}

/// Whether `ffmpeg -encoders` lists a video encoder called `name`.
fn has_video_encoder(output: &str, name: &str) -> bool {
    output.lines().any(|line| {
        let mut tokens = line.split_whitespace();
        matches!(
            (tokens.next(), tokens.next()),
            (Some(flags), Some(encoder)) if encoder == name && flags.starts_with('V')
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENCODERS: &str = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)\n A....D aac                  AAC (Advanced Audio Coding)\n";

    #[test]
    fn parses_ffmpeg_version_line() {
        assert_eq!(
            parse_ffmpeg_version("ffmpeg version 7.1.1 Copyright (c) 2000-2025\nbuilt with gcc"),
            Some("7.1.1".to_string())
        );
        assert_eq!(parse_ffmpeg_version("garbage"), None);
    }

    #[test]
    fn detects_video_encoders_only() {
        assert!(has_video_encoder(ENCODERS, "libx264"));
        assert!(!has_video_encoder(ENCODERS, "aac"));
        assert!(!has_video_encoder(ENCODERS, "libx265"));
        assert!(!has_video_encoder("", "libx264"));
    }
}
