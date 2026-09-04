//! Selecting a legacy-firmware display and talking to it, either through
//! the daemon's socket when one is running or directly over the serial port.

use crate::exit::Failure;
use crate::ipc::{self, DaemonStatus, Request};
use crate::metrics::pc_info;
use crate::state::DisplayState;
use tryx_device::discovery::{Access, LegacyDevice};
use tryx_legacy::{Client, DeviceInfo, FanStatus, LegacyError, Response};
use tryx_monitor::Monitor;

/// Brightness the vendor app assumes when nothing was ever set.
pub const DEFAULT_BRIGHTNESS: u8 = 75;

/// Global options that pick and configure the display connection.
#[derive(Debug, Clone)]
pub struct Session {
    pub tty: Option<String>,
    pub verbose: bool,
    /// Bypass a running daemon and open the port directly.
    pub direct: bool,
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

/// Where commands go.
pub enum Connection {
    Daemon,
    Direct {
        client: Box<Client>,
        target: Box<Target>,
    },
}

impl Session {
    /// Finds the display and checks the port, without opening it.
    pub fn select_direct(&self) -> Result<Target, Failure> {
        select(self.tty.as_deref())
    }

    pub fn open(&self, target: &Target) -> Result<Client, Failure> {
        let mut client = Client::open(&target.tty)?;
        client.set_trace(self.verbose);
        Ok(client)
    }

    /// The daemon when it is listening (unless `--direct`), else the port.
    pub fn connect(&self) -> Result<Connection, Failure> {
        if !self.direct && ipc::available() {
            return Ok(Connection::Daemon);
        }
        let target = self.select_direct()?;
        let client = self.open(&target)?;
        Ok(Connection::Direct {
            client: Box::new(client),
            target: Box::new(target),
        })
    }
}

fn status_of(reply: &ipc::Reply) -> String {
    reply.value["status"].as_str().unwrap_or("200").to_string()
}

impl Connection {
    pub fn via(&self) -> &'static str {
        match self {
            Connection::Daemon => "daemon",
            Connection::Direct { .. } => "serial",
        }
    }

    pub fn tty(&self) -> String {
        match self {
            Connection::Daemon => ipc::socket_path().display().to_string(),
            Connection::Direct { target, .. } => target.tty.clone(),
        }
    }

    pub fn info(&mut self) -> Result<DeviceInfo, Failure> {
        match self {
            Connection::Daemon => Ok(serde_json::from_value(ipc::expect(&Request::Info)?.value)?),
            Connection::Direct { client, .. } => Ok(client.handshake()?),
        }
    }

    /// Applies the saved screen (media, overlay, filter, sleep) and returns
    /// the device's status word.
    pub fn apply(&mut self, saved: &mut DisplayState) -> Result<String, Failure> {
        match self {
            Connection::Daemon => Ok(status_of(&ipc::expect(&Request::Apply {
                state: Box::new(saved.clone()),
            })?)),
            Connection::Direct { client, .. } => Ok(apply_screen(client, saved)?.status),
        }
    }

    pub fn brightness(&mut self, value: u8) -> Result<String, Failure> {
        match self {
            Connection::Daemon => Ok(status_of(&ipc::expect(&Request::Brightness { value })?)),
            Connection::Direct { client, .. } => Ok(client.set_brightness(value)?.status),
        }
    }

    pub fn delete_media(&mut self, names: &[String]) -> Result<(), Failure> {
        match self {
            Connection::Daemon => ipc::expect(&Request::DeleteMedia {
                names: names.to_vec(),
            })
            .map(drop),
            Connection::Direct { client, .. } => Ok(client.delete_media(names).map(drop)?),
        }
    }

    pub fn fan_lcd(&mut self, percent: u8) -> Result<(), Failure> {
        match self {
            Connection::Daemon => ipc::expect(&Request::FanLcd { percent }).map(drop),
            Connection::Direct { client, .. } => Ok(client.set_fan_lcd(percent).map(drop)?),
        }
    }

    pub fn reboot(&mut self) -> Result<(), Failure> {
        match self {
            Connection::Daemon => ipc::expect(&Request::Reboot).map(drop),
            Connection::Direct { client, .. } => Ok(client.reboot().map(drop)?),
        }
    }

    /// One raw command; `None` when sent without waiting.
    pub fn raw(
        &mut self,
        command: &str,
        body: &str,
        wait: bool,
    ) -> Result<Option<(String, String)>, Failure> {
        match self {
            Connection::Daemon => {
                let reply = ipc::expect(&Request::Raw {
                    command: command.to_string(),
                    body: body.to_string(),
                    wait,
                })?;
                Ok(wait.then(|| {
                    (
                        reply.value["status"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        reply.value["body"].as_str().unwrap_or_default().to_string(),
                    )
                }))
            }
            Connection::Direct { client, .. } => {
                if wait {
                    let response = client.link_mut().request(command, body)?;
                    Ok(Some((response.status, response.body)))
                } else {
                    client.link_mut().send(command, body)?;
                    Ok(None)
                }
            }
        }
    }

    /// Fan readings: the daemon's latest, or one sysinfo exchange carrying a
    /// fresh host sample so the overlay is not fed zeros.
    pub fn fans(&mut self) -> Result<FanStatus, Failure> {
        match self {
            Connection::Daemon => Ok(self.daemon_status()?.fans),
            Connection::Direct { client, .. } => {
                let mut sample = if Monitor::supported() {
                    let mut monitor = Monitor::new();
                    monitor.sample();
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    monitor.sample()
                } else {
                    Default::default()
                };
                sample.timestamp_ms += tryx_legacy::local_utc_offset_ms();
                Ok(client.send_sysinfo(&pc_info(&sample))?)
            }
        }
    }

    pub fn daemon_status(&self) -> Result<DaemonStatus, Failure> {
        Ok(serde_json::from_value(
            ipc::expect(&Request::Status)?.value,
        )?)
    }
}

/// Sends the saved screen configuration the way the vendor app does:
/// `waterBlockScreenId` (twice, with the waterfall mode), the overlay
/// labels, and then the full `config` with `waterBlockScreen.enable`, the
/// brightness, the sleep flag, and the hardware names. The last step is what
/// makes the firmware actually switch away from its built-in content.
pub fn apply_screen(
    client: &mut Client,
    saved: &mut DisplayState,
) -> Result<Response, LegacyError> {
    let (cpu, gpu) = hardware_names(saved);
    client.set_screen_config(&saved.screen)?;
    client.set_sysinfo_display(&saved.screen.sysinfo_display)?;
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
