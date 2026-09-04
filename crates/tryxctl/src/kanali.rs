//! Selecting a KANALI-firmware display and mapping the shared display
//! state onto its configuration and overlay.

use crate::exit::Failure;
use crate::state::DisplayState;
use tryx_device::Product;
use tryx_device::discovery::{Access, InterfaceStatus, PrinterDevice};
use tryx_device::product::IdleMode;
use tryx_kanali::overlay::{METRICS, MetricValue, OverlayArea, OverlayConfig};
use tryx_kanali::{Change, Device, DeviceInfo, KanaliError};
use tryx_legacy::commands::{DisplaySettings, SCREEN_SPLITTING};
use tryx_media::Target;
use tryx_media::target::{KANALI_PANORAMA, KANALI_TURRIS};
use tryx_monitor::Sample;

/// An open USB session with the display.
pub struct Link {
    pub id: String,
    pub device: Device,
    /// `None` on transfer-only products such as the Turris.
    pub info: Option<DeviceInfo>,
    /// The overlay the keepalive leases; `None` shows plain media.
    pub overlay: Option<OverlayConfig>,
    pub fahrenheit: bool,
}

pub fn select(id_override: Option<&str>) -> Result<(String, Product), Failure> {
    let discovery = tryx_device::discover().map_err(|error| Failure::device(error.to_string()))?;
    let mut candidates: Vec<PrinterDevice> = discovery
        .printer_devices
        .into_iter()
        .filter(|device| device.product.is_some())
        .collect();
    if let Some(id) = id_override {
        let device = candidates
            .into_iter()
            .find(|device| device.id == id)
            .ok_or_else(|| {
                Failure::device(format!(
                    "no KANALI display with id {id}; see `tryxctl devices`"
                ))
            })?;
        return ready(device);
    }
    match candidates.len() {
        0 => Err(Failure::device("no KANALI display connected")),
        1 => ready(candidates.remove(0)),
        _ => Err(Failure::device(
            "several KANALI displays are connected; choose one with --device ID",
        )),
    }
}

fn ready(device: PrinterDevice) -> Result<(String, Product), Failure> {
    match device.access {
        Access::PermissionDenied => {
            return Err(Failure::device(format!(
                "{}: permission denied; install packaging/udev/99-tryx-printer.rules (or join the lp group), then replug the display",
                device.id
            )));
        }
        Access::Busy => {
            return Err(Failure::device(format!(
                "{}: busy; another program holds the printer interface",
                device.id
            )));
        }
        Access::Error { message } => {
            return Err(Failure::device(format!("{}: {message}", device.id)));
        }
        Access::Accessible => {}
    }
    if !matches!(device.interface, InterfaceStatus::Found { .. }) {
        return Err(Failure::device(format!(
            "{}: printer interface {}",
            device.id, device.interface
        )));
    }
    Ok((
        device.id,
        device.product.expect("filtered to known products"),
    ))
}

/// Opens the display and runs the readiness handshake.
pub fn open(id: Option<&str>, verbose: bool) -> Result<Link, Failure> {
    let (id, product) = select(id)?;
    let mut device = Device::open(&id)?;
    device.set_trace(verbose);
    let info = if product.idle_mode() == IdleMode::OverlayLayout {
        Some(device.start_session()?)
    } else {
        None
    };
    Ok(Link {
        id,
        device,
        info,
        overlay: None,
        fahrenheit: false,
    })
}

pub fn media_target(product: Product) -> Target {
    match product {
        Product::PanoramaSe | Product::Panorama => KANALI_PANORAMA,
        Product::Turris620 => KANALI_TURRIS,
    }
}

/// The overlay label for a shared label name, when the firmware has one.
pub fn supported_label(label: &str) -> Option<&'static str> {
    let wanted = match label {
        "Memory Utilization" => "Memory Usage",
        other => other,
    };
    METRICS
        .iter()
        .map(|metric| metric.name)
        .find(|name| name.eq_ignore_ascii_case(wanted))
}

pub fn supported_labels() -> Vec<&'static str> {
    METRICS.iter().map(|metric| metric.name).collect()
}

fn parse_color(text: &str) -> u32 {
    u32::from_str_radix(text.trim_start_matches('#'), 16).unwrap_or(0x00DC_DCDC)
}

fn area_from(labels: &[String], settings: &DisplaySettings) -> Result<OverlayArea, Failure> {
    let mut metrics = Vec::new();
    for label in labels {
        let name = supported_label(label).ok_or_else(|| {
            Failure::usage(format!(
                "the KANALI overlay has no {label:?}; choose from {}",
                supported_labels().join(", ")
            ))
        })?;
        metrics.push(name.to_string());
    }
    Ok(OverlayArea {
        metrics,
        badges: settings.badges.clone(),
        alignment: settings.align.clone(),
        text_color: parse_color(&settings.color),
        vertical_placement: settings.position.clone(),
    })
}

/// The overlay implied by the shared state, `None` when nothing is shown.
pub fn overlay_from(saved: &DisplayState) -> Result<Option<OverlayConfig>, Failure> {
    let screen = &saved.screen;
    let dual = screen.screen_mode == SCREEN_SPLITTING;
    let left = area_from(&screen.sysinfo_display, &screen.settings)?;
    let right = if dual {
        area_from(&screen.sysinfo_display2, &screen.settings2)?
    } else {
        OverlayArea::default()
    };
    let empty = |area: &OverlayArea| area.metrics.is_empty() && area.badges.is_empty();
    if empty(&left) && empty(&right) {
        return Ok(None);
    }
    Ok(Some(OverlayConfig {
        left,
        right,
        dual,
        waterfall: screen.waterfall_mode,
        cpu_badge: saved.cpu_name.clone().unwrap_or_default(),
        gpu_badge: saved.gpu_name.clone().unwrap_or_default(),
    }))
}

/// The configuration change implied by the shared state.
pub fn change_from(saved: &DisplayState) -> Change {
    let screen = &saved.screen;
    Change {
        media: (!screen.media.is_empty()).then(|| screen.media.clone()),
        split_screen: screen.screen_mode == SCREEN_SPLITTING,
        play_mode: Some(screen.play_mode.clone()),
        brightness: saved.brightness.map(u32::from),
        backlight: None,
    }
}

/// Live values in the firmware's label names and formats.
pub fn metric_values(sample: &Sample, fahrenheit: bool) -> Vec<MetricValue> {
    let mut values = Vec::new();
    let mut push = |name: &str, value: Option<f64>, unit: &str, decimals: usize| {
        if let Some(value) = value {
            values.push(MetricValue {
                name: name.to_string(),
                value: format!("{value:.decimals$}"),
                unit: unit.to_string(),
            });
        }
    };
    let temperature =
        |celsius: Option<f64>| celsius.map(|c| if fahrenheit { c * 9.0 / 5.0 + 32.0 } else { c });
    let unit = if fahrenheit { "°F" } else { "°C" };
    push(
        "CPU Temperature",
        temperature(sample.cpu.temperature_c),
        unit,
        0,
    );
    push("CPU Frequency", sample.cpu.frequency_mhz, "MHZ", 0);
    push("CPU Usage", sample.cpu.usage_percent, "%", 0);
    push("CPU Power", sample.cpu.power_w, "W", 1);
    push(
        "GPU Temperature",
        temperature(sample.gpu.temperature_c),
        unit,
        0,
    );
    push("GPU Frequency", sample.gpu.frequency_mhz, "MHZ", 0);
    push("GPU Usage", sample.gpu.usage_percent, "%", 0);
    push("GPU Power", sample.gpu.power_w, "W", 1);
    push("Memory Usage", sample.memory.usage_percent, "%", 0);
    values
}

impl Link {
    pub fn target(&self) -> Target {
        media_target(self.device.product())
    }

    /// Sends the media selection, brightness, and overlay from the shared
    /// state, then keeps leasing that overlay.
    pub fn apply_state(&mut self, saved: &DisplayState) -> Result<String, Failure> {
        let overlay = overlay_from(saved)?;
        let change = change_from(saved);
        if change.media.is_some() || change.brightness.is_some() {
            self.device.apply(&change, overlay.as_ref())?;
        } else if let Some(overlay) = &overlay {
            self.device.configure_overlay(overlay)?;
        } else {
            // An empty layout lease clears whatever overlay was showing.
            self.device.keepalive(None)?;
        }
        self.overlay = overlay;
        self.fahrenheit = saved.temperature_unit.as_deref() == Some("Fahrenheit");
        Ok("applied".to_string())
    }

    /// Adopts the saved overlay for the lease without touching the device.
    pub fn adopt_state(&mut self, saved: &DisplayState) -> Result<(), Failure> {
        self.overlay = overlay_from(saved)?;
        self.fahrenheit = saved.temperature_unit.as_deref() == Some("Fahrenheit");
        Ok(())
    }

    pub fn keepalive(&mut self) -> Result<(), KanaliError> {
        self.device.keepalive(self.overlay.as_ref())
    }

    pub fn push(&mut self, sample: &Sample) -> Result<(), KanaliError> {
        match &self.overlay {
            Some(overlay) => self
                .device
                .send_metrics(overlay, &metric_values(sample, self.fahrenheit)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tryx_legacy::ScreenConfig;
    use tryx_monitor::{CpuSample, GpuSample};

    #[test]
    fn shared_state_maps_onto_the_kanali_overlay_and_change() {
        let saved = DisplayState {
            screen: ScreenConfig {
                media: vec!["clip.mp4.h264_2240x1080".into()],
                play_mode: "Loop".into(),
                sysinfo_display: vec!["CPU Temperature".into(), "Memory Utilization".into()],
                settings: DisplaySettings {
                    position: "Bottom".into(),
                    color: "#FF8800".into(),
                    align: "Right".into(),
                    badges: vec!["CPU Badge".into()],
                    ..DisplaySettings::default()
                },
                ..ScreenConfig::default()
            },
            brightness: Some(40),
            cpu_name: Some("AMD Ryzen".into()),
            gpu_name: None,
            temperature_unit: Some("Fahrenheit".into()),
            fan_lcd_percent: None,
        };
        let overlay = overlay_from(&saved).unwrap().unwrap();
        assert_eq!(
            overlay.left.metrics,
            vec!["CPU Temperature", "Memory Usage"]
        );
        assert_eq!(overlay.left.text_color, 0x00FF8800);
        assert_eq!(overlay.left.alignment, "Right");
        assert_eq!(overlay.left.vertical_placement, "Bottom");
        assert_eq!(overlay.cpu_badge, "AMD Ryzen");
        assert!(!overlay.dual);
        let change = change_from(&saved);
        assert_eq!(
            change.media.as_deref(),
            Some(&["clip.mp4.h264_2240x1080".to_string()][..])
        );
        assert_eq!(change.play_mode.as_deref(), Some("Loop"));
        assert_eq!(change.brightness, Some(40));
        assert!(overlay_from(&DisplayState::default()).unwrap().is_none());
        let mut unsupported = saved.clone();
        unsupported.screen.sysinfo_display = vec!["CPU Voltage".into()];
        assert!(overlay_from(&unsupported).is_err());
    }

    #[test]
    fn metric_values_use_the_firmware_formats() {
        let sample = Sample {
            cpu: CpuSample {
                temperature_c: Some(54.6),
                power_w: Some(65.25),
                ..CpuSample::default()
            },
            gpu: GpuSample {
                usage_percent: Some(3.2),
                ..GpuSample::default()
            },
            ..Sample::default()
        };
        let values = metric_values(&sample, false);
        let find = |name: &str| {
            values
                .iter()
                .find(|v| v.name == name)
                .map(|v| (v.value.clone(), v.unit.clone()))
        };
        assert_eq!(find("CPU Temperature"), Some(("55".into(), "°C".into())));
        assert_eq!(find("CPU Power"), Some(("65.2".into(), "W".into())));
        assert_eq!(find("GPU Usage"), Some(("3".into(), "%".into())));
        assert_eq!(find("GPU Temperature"), None);
        let f = metric_values(&sample, true);
        assert_eq!(
            f.iter()
                .find(|v| v.name == "CPU Temperature")
                .map(|v| v.value.as_str()),
            Some("130")
        );
    }
}
