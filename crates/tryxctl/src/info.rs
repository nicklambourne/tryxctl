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
                "transport": connection.protocol().label(),
                "via": connection.via(),
                "link": connection.tty(),
                "device": info,
            }))?
        );
        return Ok(exit::ok());
    }
    let mut rows = info.fields();
    rows.push((
        "Via",
        format!("{} ({})", connection.via(), connection.tty()),
    ));
    print!("{}", output::key_values(&rows));
    Ok(exit::ok())
}
