use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, output, state};
use serde_json::json;
use std::thread;
use std::time::Duration;
use tryx_legacy::commands::{CpuInfo, DiskInfo, GpuInfo, MemoryInfo, PcInfo};
use tryx_monitor::{Monitor, Sample};

/// Overlay labels the legacy firmware understands, with CLI aliases.
pub const LABELS: [(&str, &[&str]); 15] = [
    ("CPU Temperature", &["cpu-temp", "cpu-temperature"]),
    ("CPU Frequency", &["cpu-freq", "cpu-frequency"]),
    ("CPU Usage", &["cpu-usage", "cpu-load"]),
    ("CPU Voltage", &["cpu-voltage"]),
    ("CPU Power", &["cpu-power"]),
    ("GPU Temperature", &["gpu-temp", "gpu-temperature"]),
    ("GPU Frequency", &["gpu-freq", "gpu-frequency"]),
    ("GPU Usage", &["gpu-usage", "gpu-load"]),
    ("GPU Voltage", &["gpu-voltage"]),
    ("GPU Power", &["gpu-power"]),
    ("Hard Disk Temperature", &["disk-temp", "disk-temperature"]),
    (
        "Motherboard Temperature",
        &["mb-temp", "motherboard-temperature"],
    ),
    ("Memory Frequency", &["mem-freq", "memory-frequency"]),
    (
        "Memory Utilization",
        &["mem-usage", "memory-usage", "memory-utilization"],
    ),
    ("Date&Time", &["date-time", "datetime", "clock"]),
];
/// The overlay shows at most this many labels.
pub const MAX_LABELS: usize = 3;
/// Labels only the KANALI firmware renders.
pub const KANALI_ONLY_LABELS: [&str; 2] = ["CPU Power", "GPU Power"];

pub fn resolve_label(text: &str) -> Option<&'static str> {
    let wanted = text.trim();
    LABELS.iter().find_map(|(canonical, aliases)| {
        (canonical.eq_ignore_ascii_case(wanted)
            || aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(wanted)))
        .then_some(*canonical)
    })
}

/// Parses a comma-separated label list.
pub fn parse_labels(text: &str) -> Result<Vec<String>, Failure> {
    let mut labels: Vec<String> = Vec::new();
    for item in text
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let label = resolve_label(item).ok_or_else(|| {
            Failure::usage(format!(
                "unknown metric {item:?}; choose from {}",
                LABELS
                    .iter()
                    .map(|(_, aliases)| aliases[0])
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        if labels.iter().any(|existing| existing == label) {
            return Err(Failure::usage(format!("metric {label:?} given twice")));
        }
        labels.push(label.to_string());
    }
    if labels.len() > MAX_LABELS {
        return Err(Failure::usage(format!(
            "the overlay shows at most {MAX_LABELS} metrics"
        )));
    }
    Ok(labels)
}

fn title_case_choice(value: &str, choices: &[&'static str], flag: &str) -> Result<String, Failure> {
    choices
        .iter()
        .find(|choice| choice.eq_ignore_ascii_case(value))
        .map(|choice| choice.to_string())
        .ok_or_else(|| {
            Failure::usage(format!(
                "{flag} must be one of {}",
                choices.join(", ").to_lowercase()
            ))
        })
}

fn parse_badges(text: &str) -> Result<Vec<String>, Failure> {
    let mut badges = Vec::new();
    for item in text
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let badge = match item.to_ascii_lowercase().as_str() {
            "cpu" => "CPU Badge",
            "gpu" => "GPU Badge",
            _ => {
                return Err(Failure::usage(format!(
                    "unknown badge {item:?}; use cpu and/or gpu"
                )));
            }
        };
        if !badges.iter().any(|existing| existing == badge) {
            badges.push(badge.to_string());
        }
    }
    Ok(badges)
}

pub fn require_linux() -> Result<(), Failure> {
    if Monitor::supported() {
        Ok(())
    } else {
        Err(Failure::environment(
            "host metrics are read from /proc and /sys; Linux only",
        ))
    }
}

/// Two samples a moment apart, so rates are available.
fn warm_sample(monitor: &mut Monitor) -> Sample {
    monitor.sample();
    thread::sleep(Duration::from_millis(500));
    monitor.sample()
}

fn fmt(value: Option<f64>, unit: &str, decimals: usize) -> String {
    match value {
        Some(value) => format!("{value:.decimals$}{unit}"),
        None => "n/a".to_string(),
    }
}

pub fn status(json: bool, watch: Option<u64>) -> CommandResult {
    require_linux()?;
    let mut monitor = Monitor::new();
    let sample = warm_sample(&mut monitor);
    if let Some(seconds) = watch {
        let mut sample = sample;
        loop {
            if json {
                println!("{}", serde_json::to_string(&sample)?);
            } else {
                println!("{}", summary_line(&sample));
            }
            thread::sleep(Duration::from_secs(seconds.max(1)));
            sample = monitor.sample();
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&sample)?);
        return Ok(exit::ok());
    }
    print!(
        "{}",
        output::key_values(&[
            (
                "CPU",
                sample.cpu.name.clone().unwrap_or_else(|| "unknown".into())
            ),
            ("CPU temperature", fmt(sample.cpu.temperature_c, " °C", 0)),
            ("CPU usage", fmt(sample.cpu.usage_percent, " %", 0)),
            ("CPU frequency", fmt(sample.cpu.frequency_mhz, " MHz", 0)),
            ("CPU power", fmt(sample.cpu.power_w, " W", 1)),
            (
                "GPU",
                sample
                    .gpu
                    .name
                    .clone()
                    .unwrap_or_else(|| "none detected".into())
            ),
            ("GPU temperature", fmt(sample.gpu.temperature_c, " °C", 0)),
            ("GPU usage", fmt(sample.gpu.usage_percent, " %", 0)),
            ("GPU frequency", fmt(sample.gpu.frequency_mhz, " MHz", 0)),
            ("GPU power", fmt(sample.gpu.power_w, " W", 1)),
            ("Memory usage", fmt(sample.memory.usage_percent, " %", 0)),
            ("Disk temperature", fmt(sample.disk.temperature_c, " °C", 0)),
            ("Disk usage", fmt(sample.disk.usage_percent, " %", 0)),
        ])
    );
    Ok(exit::ok())
}

#[derive(clap::Args, Debug, Clone, Default)]
pub struct SetArgs {
    /// Comma-separated metrics, up to three, e.g. cpu-temp,gpu-temp,cpu-usage.
    #[arg(long, value_name = "LIST")]
    pub labels: Option<String>,
    /// Remove all metrics from the overlay.
    #[arg(long)]
    pub clear: bool,
    /// Vertical placement: top, center, or bottom.
    #[arg(long, value_name = "WHERE")]
    pub position: Option<String>,
    /// Text alignment: left, center, or right.
    #[arg(long, value_name = "HOW")]
    pub align: Option<String>,
    /// Text colour as #RRGGBB.
    #[arg(long, value_name = "#RRGGBB")]
    pub color: Option<String>,
    /// Hardware name badges: cpu and/or gpu, comma-separated.
    #[arg(long, value_name = "LIST")]
    pub badges: Option<String>,
    /// Media to show under the overlay; defaults to what `tryxctl show` last used.
    #[arg(long, value_name = "NAME")]
    pub media: Vec<String>,
    /// Playback mode: single, loop, or shuffle.
    #[arg(long, value_name = "MODE")]
    pub play: Option<String>,
    /// CPU name for the badge; detected when omitted.
    #[arg(long, value_name = "NAME")]
    pub cpu_name: Option<String>,
    /// GPU name for the badge; detected when omitted.
    #[arg(long, value_name = "NAME")]
    pub gpu_name: Option<String>,
    /// Show temperatures in Fahrenheit.
    #[arg(long)]
    pub fahrenheit: bool,
    /// Show temperatures in Celsius, the default.
    #[arg(long, conflicts_with = "fahrenheit")]
    pub celsius: bool,
    /// Which half the labels and layout apply to in split mode: left or right.
    #[arg(long, value_name = "left|right", default_value = "left")]
    pub area: String,
}

pub fn set(json: bool, session: &legacy::Session, args: &SetArgs) -> CommandResult {
    let labels = match (&args.labels, args.clear) {
        (Some(_), true) => return Err(Failure::usage("--labels and --clear are exclusive")),
        (Some(text), false) => Some(parse_labels(text)?),
        (None, true) => Some(Vec::new()),
        (None, false) => None,
    };
    let right = match args.area.to_ascii_lowercase().as_str() {
        "left" => false,
        "right" => true,
        _ => return Err(Failure::usage("--area must be left or right")),
    };
    let mut saved = state::load();
    let screen = &mut saved.screen;
    let (display, settings) = if right {
        (&mut screen.sysinfo_display2, &mut screen.settings2)
    } else {
        (&mut screen.sysinfo_display, &mut screen.settings)
    };
    if let Some(labels) = &labels {
        *display = labels.clone();
    }
    if let Some(position) = &args.position {
        settings.position =
            title_case_choice(position, &["Top", "Center", "Bottom"], "--position")?;
    }
    if let Some(align) = &args.align {
        settings.align = title_case_choice(align, &["Left", "Center", "Right"], "--align")?;
    }
    if let Some(color) = &args.color {
        let hex = color.strip_prefix('#').unwrap_or(color);
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(Failure::usage(format!("--color {color:?} is not #RRGGBB")));
        }
        settings.color = format!("#{}", hex.to_ascii_uppercase());
    }
    if let Some(badges) = &args.badges {
        settings.badges = parse_badges(badges)?;
    }
    if !args.media.is_empty() {
        if let Some(name) = args
            .media
            .iter()
            .find(|name| !tryx_legacy::adb::is_safe_media_name(name))
        {
            return Err(Failure::usage(format!("media name {name:?} is not safe")));
        }
        screen.media = args.media.clone();
    }
    if let Some(play) = &args.play {
        screen.play_mode = title_case_choice(play, &["Single", "Loop", "Shuffle"], "--play")?;
    }
    if let Some(cpu) = &args.cpu_name {
        saved.cpu_name = Some(cpu.clone());
    }
    if let Some(gpu) = &args.gpu_name {
        saved.gpu_name = Some(gpu.clone());
    }
    // Unchanged unless asked, like every other setting.
    if args.fahrenheit || args.celsius {
        saved.temperature_unit = Some(
            if args.fahrenheit {
                "Fahrenheit"
            } else {
                "Celsius"
            }
            .to_string(),
        );
    }
    let unit = saved
        .temperature_unit
        .clone()
        .unwrap_or_else(|| "Celsius".to_string());
    legacy::hardware_names(&mut saved);

    let mut connection = session.connect()?;
    if connection.protocol() == legacy::Protocol::Legacy {
        if saved.screen.media.is_empty() && saved.screen.preset_id.is_empty() {
            return Err(Failure::usage(
                "the overlay is part of the screen configuration and needs media: pass --media NAME or run `tryxctl show` first",
            ));
        }
        if let Some(label) = saved
            .screen
            .sysinfo_display
            .iter()
            .chain(saved.screen.sysinfo_display2.iter())
            .find(|label| KANALI_ONLY_LABELS.contains(&label.as_str()))
        {
            return Err(Failure::usage(format!(
                "{label} is only shown by the KANALI firmware"
            )));
        }
    }
    let status = connection.apply(&mut saved)?;
    let names = (
        saved.cpu_name.clone().unwrap_or_default(),
        saved.gpu_name.clone().unwrap_or_default(),
    );
    if let Err(error) = state::save(&saved) {
        eprintln!("warning: could not save the display state: {error}");
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "screen": saved.screen,
                "cpu_name": names.0,
                "gpu_name": names.1,
                "temperature_unit": unit,
                "status": status,
            }))?
        );
    } else {
        let (labels, settings) = if right {
            (&saved.screen.sysinfo_display2, &saved.screen.settings2)
        } else {
            (&saved.screen.sysinfo_display, &saved.screen.settings)
        };
        let area = if right { "right half: " } else { "" };
        if labels.is_empty() {
            println!("{area}overlay cleared ({status})");
        } else {
            println!(
                "{area}overlay shows {} at {} {} in {} ({})",
                labels.join(", "),
                settings.position.to_lowercase(),
                settings.align.to_lowercase(),
                settings.color,
                status
            );
        }
        println!(
            "Badges: {}; CPU {:?}, GPU {:?}",
            if saved.screen.settings.badges.is_empty() {
                "none".to_string()
            } else {
                saved.screen.settings.badges.join(", ")
            },
            names.0,
            names.1
        );
    }
    Ok(exit::ok())
}

pub fn pc_info(sample: &Sample) -> PcInfo {
    let or_zero = |value: Option<f64>| value.unwrap_or(0.0);
    let gib = |bytes: Option<u64>| {
        bytes
            .map(|b| b as f64 / (1024.0 * 1024.0 * 1024.0))
            .unwrap_or(0.0)
    };
    PcInfo {
        cpu: CpuInfo {
            load: or_zero(sample.cpu.usage_percent),
            temperature: or_zero(sample.cpu.temperature_c),
            speed_average: or_zero(sample.cpu.frequency_mhz),
            power: or_zero(sample.cpu.power_w),
            ..CpuInfo::default()
        },
        gpu: GpuInfo {
            load: or_zero(sample.gpu.usage_percent),
            temperature: sample
                .gpu
                .temperature_c
                .map(|t| format!("{t:.0}"))
                .unwrap_or_else(|| "0".to_string()),
            speed: or_zero(sample.gpu.frequency_mhz),
            power: or_zero(sample.gpu.power_w),
            ..GpuInfo::default()
        },
        memory: MemoryInfo {
            load: or_zero(sample.memory.usage_percent),
            total: gib(sample.memory.total_bytes),
            used: gib(sample.memory.used_bytes),
            ..MemoryInfo::default()
        },
        disk: DiskInfo {
            load: or_zero(sample.disk.usage_percent),
            temperature: or_zero(sample.disk.temperature_c),
            ..DiskInfo::default()
        },
        timestamp_ms: sample.timestamp_ms,
        ..PcInfo::default()
    }
}

fn summary_line(sample: &Sample) -> String {
    format!(
        "cpu {} {} {} {} · gpu {} {} {} · mem {} · disk {}",
        fmt(sample.cpu.temperature_c, "°C", 0),
        fmt(sample.cpu.usage_percent, "%", 0),
        fmt(sample.cpu.frequency_mhz.map(|m| m / 1000.0), "GHz", 2),
        fmt(sample.cpu.power_w, "W", 0),
        fmt(sample.gpu.temperature_c, "°C", 0),
        fmt(sample.gpu.usage_percent, "%", 0),
        fmt(sample.gpu.power_w, "W", 0),
        fmt(sample.memory.usage_percent, "%", 0),
        fmt(sample.disk.temperature_c, "°C", 0),
    )
}

pub fn push(
    json: bool,
    session: &legacy::Session,
    interval: u64,
    once: bool,
    quiet: bool,
    apply: bool,
) -> CommandResult {
    require_linux()?;
    if !session.direct && crate::ipc::available() {
        return Err(Failure::usage(
            "the tryxctl daemon is running and already pushes metrics; see `tryxctl daemon status`",
        ));
    }
    let target = match session.select_backend()? {
        legacy::Backend::Legacy(target) => target,
        legacy::Backend::Kanali { id, .. } => {
            return push_kanali(json, &id, session.verbose, interval, once, quiet, apply);
        }
    };
    let mut client = session.open(&target)?;
    if apply {
        // The display blanks when the host goes quiet, so restore the saved
        // screen before the first sample.
        let mut saved = state::load();
        if !saved.screen.media.is_empty() {
            legacy::apply_screen(&mut client, &mut saved)?;
            let _ = state::save(&saved);
            if !quiet && !json {
                let overlay = if saved.screen.sysinfo_display.is_empty() {
                    "no overlay".to_string()
                } else {
                    saved.screen.sysinfo_display.join(", ")
                };
                println!("restored {} with {overlay}", saved.screen.media.join(", "));
            }
        }
    }
    let mut monitor = Monitor::new();
    let mut sample = warm_sample(&mut monitor);
    loop {
        sample.timestamp_ms += tryx_legacy::local_utc_offset_ms();
        let fans = client.send_sysinfo(&pc_info(&sample))?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&json!({"sample": sample, "fans": fans}))?
            );
        } else if !quiet {
            println!("{}{}", summary_line(&sample), fans_suffix(&fans));
        }
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(interval));
        sample = monitor.sample();
    }
    Ok(exit::ok())
}

/// The KANALI loop: the 2 s ping and overlay lease, with values in between.
fn push_kanali(
    json: bool,
    id: &str,
    verbose: bool,
    interval: u64,
    once: bool,
    quiet: bool,
    apply: bool,
) -> CommandResult {
    let mut link = crate::kanali::open(Some(id), verbose)?;
    let saved = state::load();
    if apply {
        link.apply_state(&saved)?;
        if !quiet && !json {
            let overlay = if saved.screen.sysinfo_display.is_empty() {
                "no overlay".to_string()
            } else {
                saved.screen.sysinfo_display.join(", ")
            };
            println!("applied the saved screen with {overlay}");
        }
    } else {
        link.adopt_state(&saved)?;
    }
    let mut monitor = Monitor::new();
    let mut sample = warm_sample(&mut monitor);
    loop {
        link.keepalive()?;
        link.push(&sample)?;
        if json {
            println!("{}", serde_json::to_string(&json!({"sample": sample}))?);
        } else if !quiet {
            println!("{}", summary_line(&sample));
        }
        if once {
            break;
        }
        thread::sleep(Duration::from_secs(interval.clamp(1, 2)));
        sample = monitor.sample();
    }
    Ok(exit::ok())
}

fn fans_suffix(fans: &tryx_legacy::FanStatus) -> String {
    let mut parts = Vec::new();
    if let Some(rpm) = fans.lcd_fan_rpm {
        parts.push(format!("lcd fan {rpm} rpm"));
    }
    if let Some(rpm) = fans.pump_rpm {
        parts.push(format!("pump {rpm} rpm"));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" · {}", parts.join(" · "))
    }
}

/// `tryxctl fans`: read the fan tachometers, optionally set the LCD fan speed.
pub fn fans(
    json: bool,
    session: &legacy::Session,
    watch: Option<u64>,
    lcd_speed: Option<String>,
) -> CommandResult {
    let lcd_speed = lcd_speed
        .map(|speed| {
            if speed.eq_ignore_ascii_case("auto") {
                return Ok(None);
            }
            speed
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= 100)
                .map(Some)
                .ok_or_else(|| {
                    Failure::usage(format!("--lcd-speed {speed:?} is not 0 to 100 or auto"))
                })
        })
        .transpose()?;
    let mut connection = session.connect()?;
    if let Some(fixed) = lcd_speed {
        connection.fan_lcd(fixed)?;
        let mut saved = state::load();
        saved.fan_lcd_percent = fixed;
        if let Err(error) = state::save(&saved) {
            eprintln!("warning: could not save the display state: {error}");
        }
        if !json {
            match fixed {
                Some(percent) => println!("LCD fan set to a fixed {percent}%"),
                None => println!("LCD fan returned to the firmware's smart curve"),
            }
        }
    }
    let has_pump = connection.info().ok().map(|info| info.has_pump());
    loop {
        let fans = connection.fans()?;
        if json {
            println!(
                "{}",
                serde_json::to_string(
                    &json!({"via": connection.via(), "fans": fans, "has_pump": has_pump})
                )?
            );
        } else {
            let mut parts = Vec::new();
            if let Some(rpm) = fans.lcd_fan_rpm {
                parts.push(format!("lcd fan {rpm} rpm"));
            }
            match (fans.pump_rpm, has_pump) {
                (Some(rpm), _) => parts.push(format!("pump {rpm} rpm")),
                (None, Some(false)) => parts.push("pump not reported by this model".to_string()),
                (None, _) => {}
            }
            for warning in &fans.warnings {
                parts.push(format!(
                    "{}: {}",
                    warning.kind.to_lowercase(),
                    warning.description.to_lowercase()
                ));
            }
            if let Some(bytes) = fans.available_storage {
                parts.push(format!("{} free", output::human_bytes(bytes)));
            }
            println!(
                "{}",
                if parts.is_empty() {
                    "no fan readings reported".to_string()
                } else {
                    parts.join(" · ")
                }
            );
        }
        match watch {
            Some(seconds) => thread::sleep(Duration::from_secs(seconds.max(1))),
            None => return Ok(exit::ok()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_resolve_by_name_or_alias_with_limits() {
        assert_eq!(
            parse_labels("cpu-temp, GPU Temperature ,memory-usage").unwrap(),
            vec!["CPU Temperature", "GPU Temperature", "Memory Utilization"]
        );
        assert_eq!(parse_labels("").unwrap(), Vec::<String>::new());
        assert!(
            parse_labels("cpu-temp,cpu-temperature").is_err(),
            "duplicates"
        );
        assert!(
            parse_labels("cpu-temp,gpu-temp,cpu-usage,gpu-usage").is_err(),
            "more than three"
        );
        assert!(parse_labels("cpu-watts").is_err(), "unknown");
    }

    #[test]
    fn badges_and_choices_parse_case_insensitively() {
        assert_eq!(
            parse_badges("GPU,cpu,gpu").unwrap(),
            vec!["GPU Badge", "CPU Badge"]
        );
        assert!(parse_badges("npu").is_err());
        assert_eq!(
            title_case_choice("bottom", &["Top", "Center", "Bottom"], "--position").unwrap(),
            "Bottom"
        );
        assert!(title_case_choice("middle", &["Top", "Center", "Bottom"], "--position").is_err());
    }

    #[test]
    fn pc_info_maps_the_sample_and_keeps_gpu_temperature_textual() {
        let sample = Sample {
            cpu: tryx_monitor::CpuSample {
                temperature_c: Some(61.4),
                usage_percent: Some(12.5),
                frequency_mhz: Some(4200.0),
                ..Default::default()
            },
            gpu: tryx_monitor::GpuSample {
                temperature_c: Some(41.6),
                ..Default::default()
            },
            memory: tryx_monitor::MemorySample {
                usage_percent: Some(50.0),
                total_bytes: Some(64 * 1024 * 1024 * 1024),
                used_bytes: Some(32 * 1024 * 1024 * 1024),
            },
            ..Sample::default()
        };
        let info = pc_info(&sample);
        assert_eq!(info.cpu.temperature, 61.4);
        assert_eq!(info.cpu.speed_average, 4200.0);
        assert_eq!(info.gpu.temperature, "42");
        assert_eq!(info.memory.total, 64.0);
        assert_eq!(info.gpu.load, 0.0);
    }
}
