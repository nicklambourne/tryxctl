use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, state};
use serde_json::json;

#[derive(clap::Args, Debug, Clone, Default)]
pub struct SetArgs {
    /// Backlight brightness, 0 to 100.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    pub brightness: Option<u8>,
    /// Firmware-rendered effect over the media: none, smoke, or rain.
    #[arg(long, value_name = "EFFECT")]
    pub filter: Option<String>,
    /// Effect opacity, 0 to 100.
    #[arg(long, value_name = "PERCENT", value_parser = clap::value_parser!(u8).range(0..=100))]
    pub filter_opacity: Option<u8>,
    /// Whether the panel may sleep when the host does: on or off.
    #[arg(long, value_name = "on|off")]
    pub sleep: Option<String>,
    /// Screen layout: full, or split into a left and a right half.
    #[arg(long, value_name = "full|split")]
    pub mode: Option<String>,
    /// Portrait (waterfall) orientation: on or off.
    #[arg(long, value_name = "on|off")]
    pub waterfall: Option<String>,
    /// Rotate the media by 0, 90, 180, or 270 degrees.
    #[arg(long, value_name = "DEGREES", value_parser = ["0", "90", "180", "270"])]
    pub rotate: Option<String>,
}

fn on_off(flag: &str, value: &str) -> Result<bool, Failure> {
    match value.to_ascii_lowercase().as_str() {
        "on" | "yes" | "true" => Ok(true),
        "off" | "no" | "false" => Ok(false),
        _ => Err(Failure::usage(format!("{flag} {value:?} is not on or off"))),
    }
}

pub fn set(json: bool, session: &legacy::Session, args: &SetArgs) -> CommandResult {
    if args.brightness.is_none()
        && args.filter.is_none()
        && args.filter_opacity.is_none()
        && args.sleep.is_none()
        && args.mode.is_none()
        && args.waterfall.is_none()
        && args.rotate.is_none()
    {
        return Err(Failure::usage(
            "nothing to set; pass --brightness, --filter, --filter-opacity, --sleep, --mode, --waterfall, or --rotate",
        ));
    }
    let mut saved = state::load();
    let mut screen_changed = false;
    if let Some(filter) = &args.filter {
        saved.screen.settings.filter = match filter.to_ascii_lowercase().as_str() {
            "none" | "off" => String::new(),
            "smoke" => "Smoke".to_string(),
            "rain" => "Rain".to_string(),
            _ => {
                return Err(Failure::usage(format!(
                    "--filter {filter:?} is not none, smoke, or rain"
                )));
            }
        };
        screen_changed = true;
    }
    if let Some(opacity) = args.filter_opacity {
        saved.screen.settings.filter_opacity = u32::from(opacity);
        screen_changed = true;
    }
    if let Some(sleep) = &args.sleep {
        // "sleep on" lets the panel sleep with the host, which the firmware
        // expresses as displayInSleep=false.
        saved.screen.display_in_sleep = !on_off("--sleep", sleep)?;
        screen_changed = true;
    }
    let legacy_only = screen_changed;
    if let Some(mode) = &args.mode {
        saved.screen.screen_mode = match mode.to_ascii_lowercase().as_str() {
            "full" => tryx_legacy::commands::SCREEN_FULL,
            "split" => tryx_legacy::commands::SCREEN_SPLITTING,
            _ => {
                return Err(Failure::usage(format!(
                    "--mode {mode:?} is not full or split"
                )));
            }
        }
        .to_string();
        screen_changed = true;
    }
    if let Some(waterfall) = &args.waterfall {
        saved.screen.waterfall_mode = on_off("--waterfall", waterfall)?;
        screen_changed = true;
    }
    let rotation: Option<u16> = args
        .rotate
        .as_deref()
        .map(|text| text.parse().expect("validated by clap"));
    let mut connection = session.connect()?;
    if legacy_only && connection.protocol() == legacy::Protocol::Kanali {
        return Err(Failure::device(
            "filters and sleep control belong to the legacy firmware; the KANALI firmware has neither",
        ));
    }
    if screen_changed && saved.screen.media.is_empty() {
        return Err(Failure::usage(
            "filters and sleep are part of the screen configuration and need media: run `tryxctl show` first",
        ));
    }
    let mut statuses = serde_json::Map::new();
    if let Some(brightness) = args.brightness {
        let status = connection.brightness(brightness)?;
        saved.brightness = Some(brightness);
        statuses.insert("brightness".into(), json!(status));
    }
    if screen_changed {
        let status = connection.apply(&mut saved)?;
        statuses.insert("screen".into(), json!(status));
    }
    if let Some(degrees) = rotation {
        connection.rotate(degrees)?;
        saved.rotation = Some(degrees);
        statuses.insert("rotation".into(), json!("applied"));
    }
    if let Err(error) = state::save(&saved) {
        eprintln!("warning: could not save the display state: {error}");
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "via": connection.via(),
                "brightness": saved.brightness,
                "filter": saved.screen.settings.filter,
                "filter_opacity": saved.screen.settings.filter_opacity,
                "sleep": !saved.screen.display_in_sleep,
                "mode": saved.screen.screen_mode,
                "waterfall": saved.screen.waterfall_mode,
                "rotation": saved.rotation,
                "statuses": statuses,
            }))?
        );
    } else {
        if let Some(brightness) = args.brightness {
            println!("Brightness set to {brightness}");
        }
        if screen_changed {
            let filter = if saved.screen.settings.filter.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "{} at {}%",
                    saved.screen.settings.filter.to_lowercase(),
                    saved.screen.settings.filter_opacity
                )
            };
            println!(
                "Screen applied: {}, waterfall {}, filter {filter}, sleep with host {}",
                saved.screen.screen_mode.to_lowercase(),
                if saved.screen.waterfall_mode {
                    "on"
                } else {
                    "off"
                },
                if saved.screen.display_in_sleep {
                    "off"
                } else {
                    "on"
                }
            );
        }
        if let Some(degrees) = rotation {
            println!("Media rotated by {degrees} degrees");
        }
    }
    Ok(exit::ok())
}

/// `display get`: what the panel shows, as far as the firmware can say.
pub fn get(json: bool, session: &legacy::Session) -> CommandResult {
    let mut connection = session.connect()?;
    let readback = connection.readback()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&readback)?);
        return Ok(exit::ok());
    }
    let showing = match &readback.preset {
        Some(preset) => preset.clone(),
        None if readback.media.is_empty() => "nothing selected".to_string(),
        None => readback.media.join(", "),
    };
    let overlay = |labels: &[String]| {
        if labels.is_empty() {
            "off".to_string()
        } else {
            labels.join(", ")
        }
    };
    let mut rows = vec![
        (
            "Source",
            match readback.source.as_str() {
                "device" => "read from the display".to_string(),
                _ => "last applied by tryxctl (the cm01 firmware answers no queries)".to_string(),
            },
        ),
        (
            "Device",
            readback
                .device
                .as_ref()
                .map(legacy::Info::summary)
                .unwrap_or_else(|| "not identified".into()),
        ),
        ("Showing", format!("{showing} ({})", readback.play_mode)),
        (
            "Layout",
            format!(
                "{}, waterfall {}, rotation {}",
                readback.screen_mode.to_lowercase(),
                if readback.waterfall { "on" } else { "off" },
                readback
                    .rotation
                    .map(|d| format!("{d}°"))
                    .unwrap_or_else(|| "unset".into())
            ),
        ),
        (
            "Brightness",
            readback
                .brightness
                .map(|b| format!("{b}%"))
                .unwrap_or_else(|| "unset".into()),
        ),
        ("Overlay", overlay(&readback.overlay)),
    ];
    if readback.screen_mode == tryx_legacy::commands::SCREEN_SPLITTING {
        rows.push(("Overlay right", overlay(&readback.overlay_right)));
    }
    rows.push((
        "Badges",
        if readback.badges.is_empty() {
            "none".into()
        } else {
            readback.badges.join(", ")
        },
    ));
    if readback.protocol == legacy::Protocol::Legacy {
        rows.push((
            "Filter",
            if readback.filter.is_empty() {
                "none".into()
            } else {
                format!(
                    "{} at {}%",
                    readback.filter.to_lowercase(),
                    readback.filter_opacity
                )
            },
        ));
        rows.push((
            "Sleep with host",
            if readback.sleep_with_host {
                "on"
            } else {
                "off"
            }
            .into(),
        ));
        rows.push((
            "LCD fan",
            match (readback.fan_lcd_percent, readback.fans.lcd_fan_rpm) {
                (Some(percent), Some(rpm)) => format!("fixed {percent}%, {rpm} rpm"),
                (Some(percent), None) => format!("fixed {percent}%"),
                (None, Some(rpm)) => format!("smart mode, {rpm} rpm"),
                (None, None) => "smart mode".into(),
            },
        ));
        rows.push((
            "Pump",
            match (
                readback.fans.pump_rpm,
                readback.device.as_ref().map(legacy::Info::has_pump),
            ) {
                (Some(rpm), _) => format!("{rpm} rpm"),
                (None, Some(false)) => "not reported by this model".into(),
                (None, _) => "no reading".into(),
            },
        ));
        if let Some(bytes) = readback.fans.available_storage {
            rows.push(("Free storage", crate::output::human_bytes(bytes)));
        }
        if !readback.fans.warnings.is_empty() {
            rows.push((
                "Health",
                readback
                    .fans
                    .warnings
                    .iter()
                    .map(|w| format!("{}: {}", w.kind, w.description))
                    .collect::<Vec<_>>()
                    .join("; "),
            ));
        }
    }
    print!("{}", crate::output::key_values(&rows));
    Ok(exit::ok())
}

pub fn reboot(json: bool, session: &legacy::Session) -> CommandResult {
    let mut connection = session.connect()?;
    connection.reboot()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"rebooting": true, "via": connection.via()}))?
        );
    } else {
        println!(
            "Reboot requested; the panel shows its built-in animation until the daemon restores the screen."
        );
    }
    Ok(exit::ok())
}
