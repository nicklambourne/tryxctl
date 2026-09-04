//! Process exit codes. These are stable; scripts may rely on them.

use std::process::ExitCode;

/// Success.
pub fn ok() -> ExitCode {
    ExitCode::SUCCESS
}

/// The host environment is unusable: missing tools, permissions, or services.
pub fn environment() -> ExitCode {
    ExitCode::from(4)
}
