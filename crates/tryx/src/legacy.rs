//! Selecting and opening a display that runs the legacy cm01 firmware.

use crate::exit::Failure;
use crate::state::DisplayState;
use tryx_device::discovery::{Access, LegacyDevice};
use tryx_legacy::{Client, LegacyError, Response};
use tryx_monitor::Monitor;

/// Brightness the vendor app assumes when nothing was ever set.
pub const DEFAULT_BRIGHTNESS: u8 = 75;

/// Sends the saved screen configuration the way the vendor app does:
/// `waterBlockScreenId` (twice, with the waterfall mode), the overlay
/// labels, and then the full `config` with `waterBlockScreen.enable`, the
/// brightness, and the hardware names. The last step is what makes the
/// firmware actually switch away from its built-in content.
pub fn apply_screen(
    client: &mut Client,
    saved: &mut DisplayState,
) -> Result<Response, LegacyError> {
    let (cpu, gpu) = hardware_names(saved);
    client.set_screen_config(&saved.screen)?;
    if !saved.screen.sysinfo_display.is_empty() {
        client.set_sysinfo_display(&saved.screen.sysinfo_display)?;
    }
    let brightness = saved.brightness.unwrap_or(DEFAULT_BRIGHTNESS);
    let unit = saved.temperature_unit.as_deref().unwrap_or("Celsius");
    client.send_full_config(&saved.screen, &cpu, &gpu, brightness, unit)
}

/// CPU and GPU names for the badges: saved, or detected once and saved.
pub fn hardware_names(saved: &mut DisplayState) -> (String, String) {
    if let (Some(cpu), Some(gpu)) = (&saved.cpu_name, &saved.gpu_name) {
        return (cpu.clone(), gpu.clone());
    }
    let detected = if Monitor::supported() {
        Monitor::new().sample()
    } else {
        Default::default()
    };
    let cpu = saved
        .cpu_name
        .clone()
        .or(detected.cpu.name)
        .unwrap_or_else(|| "CPU".to_string());
    let gpu = saved
        .gpu_name
        .clone()
        .or(detected.gpu.name)
        .unwrap_or_else(|| "GPU".to_string());
    saved.cpu_name = Some(cpu.clone());
    saved.gpu_name = Some(gpu.clone());
    (cpu, gpu)
}

/// Global options that pick and configure the display connection.
pub struct Session {
    pub tty: Option<String>,
    pub verbose: bool,
}

impl Session {
    pub fn select(&self) -> Result<Target, Failure> {
        select(self.tty.as_deref())
    }

    pub fn open(&self, target: &Target) -> Result<Client, Failure> {
        let mut client = Client::open(&target.tty)?;
        client.set_trace(self.verbose);
        Ok(client)
    }
}

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
