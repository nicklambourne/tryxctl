//! Test doubles for tryxctl, so its commands can be tested end to end
//! without a cooler: a sandbox standing in for everything the tool reads
//! from its environment ([`sandbox`]), a cm01 display on a pseudo-terminal
//! ([`cm01`]) with a fake `adb` ([`adb`]), a KANALI display on a socket
//! ([`kanali`]), and small media files ([`media`]).

pub mod adb;
#[cfg(unix)]
pub mod cm01;
#[cfg(unix)]
pub mod kanali;
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
pub use kanali::FakeKanali;
#[cfg(unix)]
pub use pty::spawning;
pub use sandbox::{Sandbox, isolated};
