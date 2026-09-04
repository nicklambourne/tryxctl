use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, state};
use serde_json::json;
use tryx_legacy::adb::is_safe_media_name;

pub fn run(json: bool, session: &legacy::Session, media: &[String], play: &str) -> CommandResult {
    if let Some(name) = media.iter().find(|name| !is_safe_media_name(name)) {
        return Err(Failure::usage(format!("media name {name:?} is not safe")));
    }
    let play_mode = match play.to_ascii_lowercase().as_str() {
        "single" => "Single",
        "loop" => "Loop",
        "shuffle" => "Shuffle",
        _ => return Err(Failure::usage("--play must be single, loop, or shuffle")),
    };
    let mut saved = state::load();
    saved.screen.media = media.to_vec();
    saved.screen.play_mode = play_mode.to_string();
    let mut connection = session.connect()?;
    let status = connection.apply(&mut saved)?;
    if let Err(error) = state::save(&saved) {
        eprintln!("warning: could not save the display state: {error}");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "via": connection.via(),
                "media": media,
                "play_mode": play_mode,
                "status": status,
            }))?
        );
    } else {
        println!("Showing {} ({play_mode}, {status})", media.join(", "));
    }
    Ok(exit::ok())
}
