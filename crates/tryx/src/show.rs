use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, state};
use serde_json::json;
use tryx_legacy::adb::is_safe_media_name;

/// Selects media already stored on the display and starts playing it.
pub fn run(
    json: bool,
    session: &legacy::Session,
    media: &[String],
    play_mode: &str,
) -> CommandResult {
    if let Some(name) = media.iter().find(|name| !is_safe_media_name(name)) {
        return Err(Failure::usage(format!(
            "media name {name:?} is not safe: use ASCII letters, digits, '.', '_' or '-' and no leading dot"
        )));
    }
    let mut saved = state::load();
    saved.screen.media = media.to_vec();
    saved.screen.play_mode = play_mode.to_string();
    let target = session.select()?;
    let mut client = session.open(&target)?;
    let response = client.set_screen_config(&saved.screen)?;
    if let Err(error) = state::save(&saved) {
        eprintln!("warning: could not save the display state: {error}");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "tty": target.tty,
                "command": "waterBlockScreenId",
                "media": media,
                "play_mode": play_mode,
                "status": response.status,
                "body": response.body,
            }))?
        );
    } else {
        let status = if response.status.is_empty() {
            "acknowledged".to_string()
        } else {
            response.status
        };
        println!("Showing {} ({play_mode}, {status})", media.join(", "));
    }
    Ok(exit::ok())
}
