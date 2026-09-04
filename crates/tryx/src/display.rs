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
}

pub fn set(json: bool, session: &legacy::Session, args: &SetArgs) -> CommandResult {
    if args.brightness.is_none()
        && args.filter.is_none()
        && args.filter_opacity.is_none()
        && args.sleep.is_none()
    {
        return Err(Failure::usage(
            "nothing to set; pass --brightness, --filter, --filter-opacity, or --sleep",
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
        saved.screen.display_in_sleep = match sleep.to_ascii_lowercase().as_str() {
            "on" | "yes" | "true" => false,
            "off" | "no" | "false" => true,
            _ => {
                return Err(Failure::usage(format!(
                    "--sleep {sleep:?} is not on or off"
                )));
            }
        };
        screen_changed = true;
    }
    let mut connection = session.connect()?;
    if screen_changed && connection.protocol() == legacy::Protocol::Kanali {
        return Err(Failure::device(
            "filters and sleep control belong to the legacy firmware; the KANALI firmware has neither",
        ));
    }
    if screen_changed && saved.screen.media.is_empty() {
        return Err(Failure::usage(
            "filters and sleep are part of the screen configuration and need media: run `tryx show` first",
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
                "Screen applied: filter {filter}, sleep with host {}",
                if saved.screen.display_in_sleep {
                    "off"
                } else {
                    "on"
                }
            );
        }
    }
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
