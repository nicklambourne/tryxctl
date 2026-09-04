use crate::exit::{self, CommandResult};
use crate::{legacy, output};
use serde_json::json;

pub fn run(json: bool, session: &legacy::Session) -> CommandResult {
    let mut connection = session.connect()?;
    let info = connection.info()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "transport": "legacy-serial",
                "via": connection.via(),
                "tty": connection.tty(),
                "device": info,
            }))?
        );
        return Ok(exit::ok());
    }
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
            (
                "Via",
                format!("{} ({})", connection.via(), connection.tty())
            ),
        ])
    );
    Ok(exit::ok())
}
