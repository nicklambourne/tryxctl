//! Hidden escape hatch for protocol exploration: one legacy command, as is.

use crate::exit::{self, CommandResult, Failure};
use crate::legacy;

pub fn run(
    session: &legacy::Session,
    method: &str,
    command: &str,
    body: &str,
    no_wait: bool,
    every: Option<u64>,
) -> CommandResult {
    if !body.is_empty() {
        serde_json::from_str::<serde_json::Value>(body)
            .map_err(|error| Failure::usage(format!("body is not JSON: {error}")))?;
    }
    let mut connection = session.connect()?;
    loop {
        match connection.raw(method, command, body, !no_wait)? {
            Some((status, reply)) => println!("{command}: {status} {reply}"),
            None => println!("sent {command}"),
        }
        match every {
            Some(seconds) => std::thread::sleep(std::time::Duration::from_secs(seconds.max(1))),
            None => return Ok(exit::ok()),
        }
    }
}
