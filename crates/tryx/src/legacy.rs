//! Selecting and opening a display that runs the legacy cm01 firmware.

use crate::exit::Failure;
use tryx_device::discovery::{Access, LegacyDevice};
use tryx_legacy::Client;

pub struct Target {
    pub tty: String,
    /// The discovered device, when the port came from discovery.
    pub device: Option<LegacyDevice>,
}

impl Target {
    pub fn usb_serial(&self) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.serial.as_deref())
    }

    /// The sysfs device name, e.g. `3-12`, which adb reports as `usb:3-12`.
    pub fn sysfs_name(&self) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.sysfs_path.rsplit('/').next())
    }
}

pub fn select(tty_override: Option<&str>) -> Result<Target, Failure> {
    if let Some(tty) = tty_override {
        let device = tryx_device::discover().ok().and_then(|discovery| {
            discovery
                .legacy_devices
                .into_iter()
                .find(|device| device.tty.as_deref() == Some(tty))
        });
        return Ok(Target {
            tty: tty.to_string(),
            device,
        });
    }

    let discovery = tryx_device::discover().map_err(|error| Failure::device(error.to_string()))?;
    let mut legacy = discovery.legacy_devices;
    match legacy.len() {
        0 if discovery.printer_devices.is_empty() => {
            Err(Failure::device("no TRYX display connected"))
        }
        0 => Err(Failure::device(
            "the connected display runs the printer-class (KANALI) firmware, which is not supported yet",
        )),
        1 => {
            let device = legacy.remove(0);
            let tty = device.tty.clone().ok_or_else(|| {
                Failure::device(format!(
                    "{}: no serial command port exposed; replug the display",
                    device.id
                ))
            })?;
            if device.tty_access == Some(Access::PermissionDenied) {
                return Err(Failure::device(format!(
                    "{tty}: permission denied; join the dialout group, then log in again"
                )));
            }
            Ok(Target {
                tty,
                device: Some(device),
            })
        }
        _ => Err(Failure::device(
            "several legacy displays are connected; choose one with --tty",
        )),
    }
}

pub fn open(target: &Target) -> Result<Client, Failure> {
    Ok(Client::open(&target.tty)?)
}
