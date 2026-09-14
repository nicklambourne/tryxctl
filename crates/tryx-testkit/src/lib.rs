//! Test doubles for tryxctl, so its commands can be tested end to end
//! without a cooler: a sandbox standing in for everything the tool reads
//! from its environment ([`sandbox`]), a cm01 display on a pseudo-terminal
//! ([`cm01`]), a fake `adb` ([`adb`]), and small media files ([`media`]).

pub mod adb;
#[cfg(unix)]
pub mod cm01;
pub mod media;
#[cfg(unix)]
pub mod pty;
pub mod sandbox;
#[cfg(unix)]
pub mod terminal;

pub use adb::FakeAdb;
#[cfg(unix)]
pub use cm01::FakeCm01;
#[cfg(unix)]
pub use pty::spawning;
pub use sandbox::{Sandbox, isolated};
