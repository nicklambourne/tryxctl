//! Hidden escape hatch for protocol exploration: one legacy command, as is.

use crate::exit::{self, CommandResult, Failure};
use crate::legacy;

pub fn run(
    session: &legacy::Session,
    command: &str,
    body: &str,
    no_wait: bool,
    every: Option<u64>,
) -> CommandResult {
    if !body.is_empty() {
        serde_json::from_str::<serde_json::Value>(body)
            .map_err(|error| Failure::usage(format!("body is not JSON: {error}")))?;
    }
    let target = session.select()?;
    let mut client = session.open(&target)?;
    loop {
        if no_wait {
            client.link_mut().send(command, body)?;
            println!("sent {command}");
        } else {
            let response = client.link_mut().request(command, body)?;
            println!("{command}: {} {}", response.status, response.body);
        }
        match every {
            Some(seconds) => std::thread::sleep(std::time::Duration::from_secs(seconds.max(1))),
            None => return Ok(exit::ok()),
        }
    }
}
