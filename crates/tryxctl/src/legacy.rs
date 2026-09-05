//! Selecting a display and talking to it: through the daemon's socket when
//! one is running, directly over the serial port (legacy firmware), or over
//! USB (KANALI firmware).

use crate::exit::Failure;
use crate::ipc::{self, DaemonStatus, Request};
use crate::kanali;
use crate::metrics::pc_info;
use crate::state::DisplayState;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;
use tryx_device::Product;
use tryx_device::discovery::{Access, LegacyDevice};
use tryx_kanali::Catalog;
use tryx_legacy::{Client, DeviceInfo, FanStatus, LegacyError, Response};
use tryx_media::Target as MediaTarget;
use tryx_media::target::LEGACY_PANORAMA;
use tryx_monitor::Monitor;

/// Brightness the vendor app assumes when nothing was ever set.
pub const DEFAULT_BRIGHTNESS: u8 = 75;

/// The wire protocol a display speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    /// JSON over the CDC ACM serial port plus ADB for files (cm01 firmware).
    #[default]
    Legacy,
    /// Protobuf frames over the printer-class USB interface.
    Kanali,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Legacy => "legacy-serial",
            Protocol::Kanali => "kanali-usb",
        }
    }
}

/// What a display says about itself, per protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "protocol", rename_all = "kebab-case")]
pub enum Info {
    Legacy(DeviceInfo),
    Kanali(tryx_kanali::DeviceInfo),
}

impl Info {
    pub fn short(&self) -> String {
        match self {
            Info::Legacy(info) => format!("{} firmware {}", info.product_id, info.firmware),
            Info::Kanali(info) => {
                format!("{} firmware {}", info.product_name, info.firmware_version)
            }
        }
    }

    pub fn serial(&self) -> &str {
        match self {
            Info::Legacy(info) => &info.serial,
            Info::Kanali(info) => &info.serial_number,
        }
    }

    /// Whether the firmware reports a pump tachometer at all.
    pub fn has_pump(&self) -> bool {
        match self {
            Info::Legacy(info) => info.has_pump(),
            Info::Kanali(_) => false,
        }
    }

    pub fn summary(&self) -> String {
        format!("{} · serial {}", self.short(), self.serial())
    }

    pub fn fields(&self) -> Vec<(&'static str, String)> {
        match self {
            Info::Legacy(info) => vec![
                ("Product", info.product_id.clone()),
                ("Firmware", info.firmware.clone()),
                ("App", info.app_version.clone()),
                ("Hardware", info.hardware.clone()),
                ("OS", info.os.clone()),
                ("Serial", info.serial.clone()),
                ("Attributes", info.attributes.join(", ")),
            ],
            Info::Kanali(info) => vec![
                ("Product", info.product_name.clone()),
                ("Firmware", info.firmware_version.clone()),
                ("App", info.app_version.clone()),
                ("OS", format!("{} {}", info.os_name, info.os_version)),
                ("Chip", info.chip_id.clone()),
                (
                    "Serial",
                    if info.serial_number_locked {
                        format!("{} (locked)", info.serial_number)
                    } else {
                        info.serial_number.clone()
                    },
                ),
            ],
        }
    }
}

/// Global options that pick and configure the display connection.
#[derive(Debug, Clone)]
pub struct Session {
    pub tty: Option<String>,
    pub device: Option<String>,
    pub verbose: bool,
    /// Bypass a running daemon and open the display directly.
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

/// The display discovery settled on, before anything is opened.
pub enum Backend {
    Legacy(Target),
    Kanali { id: String, product: Product },
}

/// Where commands go.
pub enum Connection {
    Daemon {
        protocol: Protocol,
        product: Option<Product>,
    },
    Direct {
        client: Box<Client>,
        target: Box<Target>,
    },
    Kanali(Box<kanali::Link>),
}

impl Session {
    /// Which display and protocol discovery points at.
    pub fn select_backend(&self) -> Result<Backend, Failure> {
        if self.tty.is_some() {
            return Ok(Backend::Legacy(select(self.tty.as_deref())?));
        }
        if self.device.is_some() {
            let (id, product) = kanali::select(self.device.as_deref())?;
            return Ok(Backend::Kanali { id, product });
        }
        let discovery =
            tryx_device::discover().map_err(|error| Failure::device(error.to_string()))?;
        if discovery.legacy_devices.is_empty()
            && discovery
                .printer_devices
                .iter()
                .any(|device| device.product.is_some())
        {
            let (id, product) = kanali::select(None)?;
            return Ok(Backend::Kanali { id, product });
        }
        Ok(Backend::Legacy(select(None)?))
    }

    pub fn open(&self, target: &Target) -> Result<Client, Failure> {
        let mut client = Client::open(&target.tty)?;
        client.set_trace(self.verbose);
        Ok(client)
    }

    /// The daemon when it is listening (unless `--direct`), else the display.
    pub fn connect(&self) -> Result<Connection, Failure> {
        if !self.direct
            && let Some(status) = ipc::status()?
        {
            return Ok(Connection::Daemon {
                protocol: status.protocol,
                product: status.product,
            });
        }
        match self.select_backend()? {
            Backend::Legacy(target) => {
                let client = self.open(&target)?;
                Ok(Connection::Direct {
                    client: Box::new(client),
                    target: Box::new(target),
                })
            }
            Backend::Kanali { id, .. } => Ok(Connection::Kanali(Box::new(kanali::open(
                Some(&id),
                self.verbose,
            )?))),
        }
    }
}

fn status_of(reply: &ipc::Reply) -> String {
    reply.value["status"].as_str().unwrap_or("200").to_string()
}

fn unsupported(what: &str) -> Failure {
    Failure::device(format!("{what} is not available on the KANALI firmware"))
}

fn kanali_only(what: &str) -> Failure {
    Failure::device(format!(
        "{what} needs the KANALI firmware; this display speaks the legacy protocol"
    ))
}

impl Connection {
    pub fn protocol(&self) -> Protocol {
        match self {
            Connection::Daemon { protocol, .. } => *protocol,
            Connection::Direct { .. } => Protocol::Legacy,
            Connection::Kanali(_) => Protocol::Kanali,
        }
    }

    pub fn via(&self) -> &'static str {
        match self {
            Connection::Daemon { .. } => "daemon",
            Connection::Direct { .. } => "serial",
            Connection::Kanali(_) => "usb",
        }
    }

    pub fn tty(&self) -> String {
        match self {
            Connection::Daemon { .. } => ipc::socket_path().display().to_string(),
            Connection::Direct { target, .. } => target.tty.clone(),
            Connection::Kanali(link) => link.id.clone(),
        }
    }

    pub fn info(&mut self) -> Result<Info, Failure> {
        match self {
            Connection::Daemon { .. } => {
                Ok(serde_json::from_value(ipc::expect(&Request::Info)?.value)?)
            }
            Connection::Direct { client, .. } => Ok(Info::Legacy(client.handshake()?)),
            Connection::Kanali(link) => link
                .info
                .clone()
                .map(Info::Kanali)
                .ok_or_else(|| unsupported("device information on this product")),
        }
    }

    /// Applies the saved screen (media, overlay, filter, sleep) and returns
    /// the device's status word.
    pub fn apply(&mut self, saved: &mut DisplayState) -> Result<String, Failure> {
        match self {
            Connection::Daemon { .. } => Ok(status_of(&ipc::expect(&Request::Apply {
                state: Box::new(saved.clone()),
            })?)),
            Connection::Direct { client, .. } => Ok(apply_screen(client, saved)?.status),
            Connection::Kanali(link) => link.apply_state(saved),
        }
    }

    pub fn brightness(&mut self, value: u8) -> Result<String, Failure> {
        match self {
            Connection::Daemon { .. } => {
                Ok(status_of(&ipc::expect(&Request::Brightness { value })?))
            }
            Connection::Direct { client, .. } => Ok(client.set_brightness(value)?.status),
            Connection::Kanali(link) => {
                link.device.set_brightness(u32::from(value))?;
                Ok("applied".to_string())
            }
        }
    }

    pub fn delete_media(&mut self, names: &[String]) -> Result<(), Failure> {
        match self {
            Connection::Daemon { .. } => ipc::expect(&Request::DeleteMedia {
                names: names.to_vec(),
            })
            .map(drop),
            Connection::Direct { client, .. } => Ok(client.delete_media(names).map(drop)?),
            Connection::Kanali(link) => {
                for name in names {
                    link.device.delete(name)?;
                }
                Ok(())
            }
        }
    }

    pub fn fan_lcd(&mut self, percent: u8) -> Result<(), Failure> {
        match self {
            Connection::Daemon {
                protocol: Protocol::Legacy,
                ..
            } => ipc::expect(&Request::FanLcd { percent }).map(drop),
            Connection::Direct { client, .. } => Ok(client.set_fan_lcd(percent).map(drop)?),
            Connection::Daemon { .. } | Connection::Kanali(_) => Err(unsupported("fan control")),
        }
    }

    pub fn reboot(&mut self) -> Result<(), Failure> {
        match self {
            Connection::Daemon {
                protocol: Protocol::Legacy,
                ..
            } => ipc::expect(&Request::Reboot).map(drop),
            Connection::Direct { client, .. } => Ok(client.reboot().map(drop)?),
            Connection::Daemon { .. } | Connection::Kanali(_) => Err(unsupported("reboot")),
        }
    }

    /// One raw command; `None` when sent without waiting.
    pub fn raw(
        &mut self,
        method: &str,
        command: &str,
        body: &str,
        wait: bool,
    ) -> Result<Option<(String, String)>, Failure> {
        match self {
            Connection::Daemon {
                protocol: Protocol::Legacy,
                ..
            } => {
                let reply = ipc::expect(&Request::Raw {
                    method: method.to_string(),
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
                    let response = client.link_mut().request_with(method, command, body)?;
                    Ok(Some((response.status, response.body)))
                } else {
                    client.link_mut().send_with(method, command, body)?;
                    Ok(None)
                }
            }
            Connection::Daemon { .. } | Connection::Kanali(_) => {
                Err(unsupported("raw legacy commands"))
            }
        }
    }

    /// Fan readings: the daemon's latest, or one sysinfo exchange carrying a
    /// fresh host sample so the overlay is not fed zeros.
    pub fn fans(&mut self) -> Result<FanStatus, Failure> {
        match self {
            Connection::Daemon {
                protocol: Protocol::Legacy,
                ..
            } => Ok(self.daemon_status()?.fans),
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
            Connection::Daemon { .. } | Connection::Kanali(_) => Err(unsupported("fan readings")),
        }
    }

    /// Rotates the media by 0, 90, 180, or 270 degrees.
    pub fn rotate(&mut self, degrees: u16) -> Result<(), Failure> {
        match self {
            Connection::Daemon { .. } => ipc::expect(&Request::Rotate { degrees }).map(drop),
            Connection::Direct { client, .. } => Ok(client.set_rotation(degrees).map(drop)?),
            Connection::Kanali(link) => {
                let change = tryx_kanali::Change {
                    rotation: Some(u32::from(degrees)),
                    ..Default::default()
                };
                link.device.apply(&change, link.overlay.as_ref())?;
                Ok(())
            }
        }
    }

    /// What the display shows, from the device where the firmware can say
    /// (KANALI) and from the last applied state where it cannot (legacy).
    pub fn readback(&mut self) -> Result<Readback, Failure> {
        match self {
            Connection::Daemon { .. } => Ok(serde_json::from_value(
                ipc::expect(&Request::Readback)?.value,
            )?),
            Connection::Direct { .. } => {
                let saved = crate::state::load();
                let info = self.info().ok();
                let fans = self.fans().unwrap_or_default();
                Ok(Readback::last_applied(&saved, info, fans))
            }
            Connection::Kanali(link) => {
                let saved = crate::state::load();
                let state = link.device.display_state()?;
                Ok(Readback::from_kanali(
                    &state,
                    &saved,
                    link.info.clone().map(Info::Kanali),
                ))
            }
        }
    }

    pub fn daemon_status(&self) -> Result<DaemonStatus, Failure> {
        Ok(serde_json::from_value(
            ipc::expect(&Request::Status)?.value,
        )?)
    }

    /// The media on a KANALI display.
    pub fn catalog(&mut self) -> Result<Catalog, Failure> {
        match self {
            Connection::Kanali(link) => Ok(link.device.catalog()?),
            Connection::Daemon {
                protocol: Protocol::Kanali,
                ..
            } => Ok(serde_json::from_value(
                ipc::expect(&Request::Catalog)?.value,
            )?),
            _ => Err(kanali_only("the media catalog")),
        }
    }

    /// Sends a prepared file to a KANALI display under `name`.
    pub fn upload(
        &mut self,
        path: &Path,
        name: &str,
        progress: impl FnMut(u64, u64),
    ) -> Result<(), Failure> {
        match self {
            Connection::Kanali(link) => Ok(link.device.upload(path, name, progress)?),
            Connection::Daemon {
                protocol: Protocol::Kanali,
                ..
            } => ipc::expect_with_timeout(
                &Request::Upload {
                    path: path.to_path_buf(),
                    name: name.to_string(),
                },
                Duration::from_secs(30 * 60),
            )
            .map(drop),
            _ => Err(kanali_only("USB media upload")),
        }
    }

    /// What prepared media must look like for this display.
    pub fn media_target(&self) -> MediaTarget {
        match self {
            Connection::Kanali(link) => link.target(),
            Connection::Daemon {
                protocol: Protocol::Kanali,
                product: Some(product),
            } => kanali::media_target(*product),
            _ => LEGACY_PANORAMA,
        }
    }

    /// The name a prepared file gets on the display.
    pub fn remote_name(&self, local_name: &str) -> String {
        match self {
            Connection::Kanali(link) => link.device.remote_name(local_name),
            Connection::Daemon {
                protocol: Protocol::Kanali,
                product: Some(product),
            } => format!("{local_name}{}", product.media_name_suffix()),
            _ => local_name.to_string(),
        }
    }
}

/// What the display shows, as far as it can be known.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Readback {
    /// `device` when read from the firmware, `last-applied` when it is what
    /// this tool last sent (the legacy firmware answers no queries).
    pub source: String,
    pub protocol: Protocol,
    pub device: Option<Info>,
    pub media: Vec<String>,
    pub preset: Option<String>,
    pub play_mode: String,
    pub screen_mode: String,
    pub waterfall: bool,
    pub rotation: Option<u16>,
    pub brightness: Option<u8>,
    pub overlay: Vec<String>,
    pub overlay_right: Vec<String>,
    pub badges: Vec<String>,
    pub filter: String,
    pub filter_opacity: u32,
    pub sleep_with_host: bool,
    pub fan_lcd_percent: Option<u8>,
    pub fans: FanStatus,
}

impl Readback {
    pub fn last_applied(saved: &DisplayState, device: Option<Info>, fans: FanStatus) -> Readback {
        let screen = &saved.screen;
        Readback {
            source: "last-applied".to_string(),
            protocol: Protocol::Legacy,
            device,
            media: screen.media.clone(),
            preset: (!screen.preset_id.is_empty()).then(|| screen.preset_id.clone()),
            play_mode: screen.play_mode.clone(),
            screen_mode: screen.screen_mode.clone(),
            waterfall: screen.waterfall_mode,
            rotation: saved.rotation,
            brightness: saved.brightness,
            overlay: screen.sysinfo_display.clone(),
            overlay_right: screen.sysinfo_display2.clone(),
            badges: screen.settings.badges.clone(),
            filter: screen.settings.filter.clone(),
            filter_opacity: screen.settings.filter_opacity,
            sleep_with_host: !screen.display_in_sleep,
            fan_lcd_percent: saved.fan_lcd_percent,
            fans,
        }
    }

    pub fn from_kanali(
        state: &tryx_kanali::DisplayState,
        saved: &DisplayState,
        device: Option<Info>,
    ) -> Readback {
        let screen = &saved.screen;
        Readback {
            source: "device".to_string(),
            protocol: Protocol::Kanali,
            device,
            media: state.media.clone(),
            preset: None,
            play_mode: state.play_mode.clone(),
            screen_mode: state.screen_mode.clone(),
            waterfall: state.waterfall,
            rotation: Some(if state.mirror { 180 } else { 0 }),
            brightness: Some(state.brightness.min(100) as u8),
            overlay: screen.sysinfo_display.clone(),
            overlay_right: screen.sysinfo_display2.clone(),
            badges: screen.settings.badges.clone(),
            filter: String::new(),
            filter_opacity: 0,
            sleep_with_host: state.standby_enabled,
            fan_lcd_percent: None,
            fans: FanStatus::default(),
        }
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
    let response = client.send_full_config(&saved.screen, &cpu, &gpu, brightness, unit)?;
    if let Some(degrees) = saved.rotation {
        client.set_rotation(degrees)?;
    }
    Ok(response)
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
            "the connected display runs the KANALI firmware; this command needs the legacy serial protocol",
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
