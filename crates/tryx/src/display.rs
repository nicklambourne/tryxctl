use crate::exit::{self, CommandResult, Failure};
use crate::{legacy, state};
use serde_json::json;

pub fn set(json: bool, session: &legacy::Session, brightness: Option<u8>) -> CommandResult {
    let Some(brightness) = brightness else {
        return Err(Failure::usage("nothing to set; pass --brightness <0-100>"));
    };
    let target = session.select()?;
    let mut client = session.open(&target)?;
    let response = client.set_brightness(brightness)?;
    let mut saved = state::load();
    saved.brightness = Some(brightness);
    if let Err(error) = state::save(&saved) {
        eprintln!("warning: could not save the display state: {error}");
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "tty": target.tty,
                "command": "brightness",
                "value": brightness,
                "status": response.status,
                "body": response.body,
                "checksum_ok": response.checksum_ok,
            }))?
        );
    } else {
        let status = if response.status.is_empty() {
            "acknowledged".to_string()
        } else {
            response.status
        };
        println!("Brightness set to {brightness} ({status})");
    }
    Ok(exit::ok())
}
