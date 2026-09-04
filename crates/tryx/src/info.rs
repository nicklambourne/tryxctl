use crate::exit::{self, CommandResult};
use crate::{legacy, output};
use serde_json::json;

pub fn run(json: bool, session: &legacy::Session) -> CommandResult {
    let target = session.select()?;
    let mut client = session.open(&target)?;
    let info = client.handshake()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "transport": "legacy-serial",
                "tty": target.tty,
                "usb": target.device,
                "device": info,
            }))?
        );
        return Ok(exit::ok());
    }
    let port = match &target.device {
        Some(device) => format!("{} ({})", target.tty, device.id),
        None => target.tty.clone(),
    };
    print!(
        "{}",
        output::key_values(&[
            ("Product", info.product_id),
            ("Firmware", info.firmware),
            ("App", info.app_version),
            ("Hardware", info.hardware),
            ("OS", info.os),
            ("Serial", info.serial),
            ("Attributes", info.attributes.join(", ")),
            ("Port", port),
        ])
    );
    Ok(exit::ok())
}
