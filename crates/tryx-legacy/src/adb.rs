//! Media transfer through the `adb` binary, as the vendor app does. The
//! firmware keeps user media in one flat directory.

use crate::LegacyError;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const MEDIA_DIR: &str = "/sdcard/pcMedia/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    /// `device`, `unauthorized`, `offline`, ...
    pub state: String,
    /// Transport qualifier such as `usb:3-12`.
    pub usb: Option<String>,
    pub product: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MediaFile {
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DiskUsage {
    pub total_kib: u64,
    pub used_kib: u64,
    pub available_kib: u64,
}

/// Parses `adb devices -l`.
pub fn parse_devices(output: &str) -> Vec<AdbDevice> {
    output
        .lines()
        .filter(|line| !line.starts_with("List of devices") && !line.starts_with('*'))
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            let serial = tokens.next()?.to_string();
            // "no permissions (missing udev rules? ...)" spans several
            // tokens; keep the first word so callers can match on it.
            let state = tokens.next()?.to_string();
            let mut device = AdbDevice {
                serial,
                state,
                usb: None,
                product: None,
                model: None,
            };
            for token in tokens {
                match token.split_once(':') {
                    Some(("usb", value)) => device.usb = Some(value.to_string()),
                    Some(("product", value)) => device.product = Some(value.to_string()),
                    Some(("model", value)) => device.model = Some(value.to_string()),
                    _ => {}
                }
            }
            Some(device)
        })
        .collect()
}

/// Chooses the ADB transport that belongs to a USB device, by serial first,
/// then by the `usb:<bus>-<ports>` qualifier, then by being the only device.
pub fn select<'a>(
    devices: &'a [AdbDevice],
    usb_serial: Option<&str>,
    sysfs_name: Option<&str>,
) -> Option<&'a AdbDevice> {
    if let Some(serial) = usb_serial
        && let Some(device) = devices.iter().find(|d| d.serial == serial)
    {
        return Some(device);
    }
    if let Some(name) = sysfs_name
        && let Some(device) = devices.iter().find(|d| d.usb.as_deref() == Some(name))
    {
        return Some(device);
    }
    match devices {
        [only] => Some(only),
        _ => None,
    }
}

/// Parses `stat -c '%s %n'` output: one `size path` line per file.
pub fn parse_stat_listing(output: &str) -> Vec<MediaFile> {
    output
        .lines()
        .filter_map(|line| {
            let (size, path) = line.trim_end().split_once(' ')?;
            let size = size.parse().ok()?;
            let name = path.rsplit('/').next()?;
            (!name.is_empty()).then(|| MediaFile {
                name: name.to_string(),
                size,
            })
        })
        .collect()
}

/// Parses `df -k <path>`: the last line carries the numbers in KiB.
pub fn parse_df(output: &str) -> Option<DiskUsage> {
    let line = output.lines().rev().find(|line| !line.trim().is_empty())?;
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 4 {
        return None;
    }
    Some(DiskUsage {
        total_kib: tokens[1].parse().ok()?,
        used_kib: tokens[2].parse().ok()?,
        available_kib: tokens[3].parse().ok()?,
    })
}

/// Names the firmware directory accepts and that need no quoting through
/// `adb shell`: ASCII letters, digits, `.`, `_`, `-`, no leading dot.
pub fn is_safe_media_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub struct Adb {
    program: PathBuf,
    serial: Option<String>,
}

impl Adb {
    pub fn new() -> Result<Self, LegacyError> {
        let program = which::which("adb").map_err(|_| LegacyError::AdbMissing)?;
        Ok(Adb {
            program,
            serial: None,
        })
    }

    pub fn with_serial(mut self, serial: impl Into<String>) -> Self {
        self.serial = Some(serial.into());
        self
    }

    pub fn devices(&self) -> Result<Vec<AdbDevice>, LegacyError> {
        Ok(parse_devices(&self.run(&["devices", "-l"], false)?))
    }

    pub fn list_media(&self) -> Result<Vec<MediaFile>, LegacyError> {
        let command = format!("stat -c '%s %n' {MEDIA_DIR}* 2>/dev/null; true");
        Ok(parse_stat_listing(&self.run(&["shell", &command], true)?))
    }

    pub fn free_space(&self) -> Result<DiskUsage, LegacyError> {
        let output = self.run(&["shell", "df -k /sdcard"], true)?;
        parse_df(&output).ok_or_else(|| LegacyError::Adb {
            args: "shell df -k /sdcard".to_string(),
            message: format!("unexpected output: {}", output.trim()),
        })
    }

    pub fn push(&self, local: &Path, remote_name: &str) -> Result<(), LegacyError> {
        let remote = self.remote_path(remote_name)?;
        let local = local.to_string_lossy();
        self.run(&["push", &local, &remote], true).map(drop)
    }

    pub fn pull(&self, remote_name: &str, local: &Path) -> Result<(), LegacyError> {
        let remote = self.remote_path(remote_name)?;
        let local = local.to_string_lossy();
        self.run(&["pull", &remote, &local], true).map(drop)
    }

    pub fn remove(&self, remote_name: &str) -> Result<(), LegacyError> {
        let remote = self.remote_path(remote_name)?;
        let command = format!("rm -- {remote}");
        let output = self.run(&["shell", &command], true)?;
        // Old adbd versions report every shell command as successful, so
        // the message is the only signal.
        if output.contains("No such file") || output.contains("rm:") {
            return Err(LegacyError::Adb {
                args: format!("shell {command}"),
                message: output.trim().to_string(),
            });
        }
        Ok(())
    }

    fn remote_path(&self, name: &str) -> Result<String, LegacyError> {
        if !is_safe_media_name(name) {
            return Err(LegacyError::UnsafeMediaName(name.to_string()));
        }
        Ok(format!("{MEDIA_DIR}{name}"))
    }

    fn run(&self, args: &[&str], device_scoped: bool) -> Result<String, LegacyError> {
        let mut command = Command::new(&self.program);
        if device_scoped && let Some(serial) = &self.serial {
            command.args(["-s", serial]);
        }
        command.args(args);
        let output = command.output().map_err(|error| LegacyError::Adb {
            args: args.join(" "),
            message: error.to_string(),
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(LegacyError::Adb {
                args: args.join(" "),
                message: format!("{}{}", stdout.trim(), stderr.trim()),
            });
        }
        Ok(stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEVICES: &str = "List of devices attached\n\
        XYZ000000000000001     device usb:3-12 product:cm01_se model:cm01_se device:cm01\n\
        emulator-5554          offline\n";

    #[test]
    fn parses_device_listing_with_qualifiers() {
        let devices = parse_devices(DEVICES);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "XYZ000000000000001");
        assert_eq!(devices[0].state, "device");
        assert_eq!(devices[0].usb.as_deref(), Some("3-12"));
        assert_eq!(devices[0].product.as_deref(), Some("cm01_se"));
        assert_eq!(devices[1].state, "offline");
        assert!(devices[1].usb.is_none());
        assert!(
            parse_devices("* daemon started successfully\nList of devices attached\n\n").is_empty()
        );
    }

    #[test]
    fn selects_by_serial_then_usb_path_then_uniqueness() {
        let devices = parse_devices(DEVICES);
        assert_eq!(
            select(&devices, Some("XYZ000000000000001"), None).map(|d| &d.serial),
            Some(&"XYZ000000000000001".to_string())
        );
        assert_eq!(
            select(&devices, Some("other"), Some("3-12")).map(|d| &d.serial),
            Some(&"XYZ000000000000001".to_string())
        );
        assert_eq!(select(&devices, Some("other"), Some("1-1")), None);
        assert_eq!(
            select(&devices[1..], None, None).map(|d| &d.serial),
            Some(&"emulator-5554".to_string())
        );
    }

    #[test]
    fn parses_stat_listing_and_df() {
        let files = parse_stat_listing(
            "1048576 /sdcard/pcMedia/clip one.mp4\n2048 /sdcard/pcMedia/a.png\nstat: bad\n",
        );
        assert_eq!(
            files,
            vec![
                MediaFile {
                    name: "clip one.mp4".into(),
                    size: 1_048_576
                },
                MediaFile {
                    name: "a.png".into(),
                    size: 2048
                },
            ]
        );
        assert!(parse_stat_listing("").is_empty());
        let usage = parse_df("Filesystem     1K-blocks    Used Available Use% Mounted on\n/dev/fuse       11681792 4218880   7462912  37% /storage/emulated\n").unwrap();
        assert_eq!(
            usage,
            DiskUsage {
                total_kib: 11_681_792,
                used_kib: 4_218_880,
                available_kib: 7_462_912
            }
        );
        assert_eq!(parse_df("df: /sdcard: No such file or directory\n"), None);
    }

    #[test]
    fn media_names_that_need_quoting_are_rejected() {
        assert!(is_safe_media_name("clip_01.mp4"));
        assert!(!is_safe_media_name("clip one.mp4"));
        assert!(!is_safe_media_name(".hidden"));
        assert!(!is_safe_media_name("../etc"));
        assert!(!is_safe_media_name("a;rm -rf.mp4"));
        assert!(!is_safe_media_name(""));
    }
}
