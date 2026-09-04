//! Process exit codes and the failure type that carries them. Codes are
//! stable; scripts may rely on them. Clap reports usage errors with 2.

use std::process::ExitCode;

/// A display was required and is missing, inaccessible, or rejected the
/// request.
pub const DEVICE: u8 = 3;
/// The host environment is unusable: missing tools, permissions, or services.
pub const ENVIRONMENT: u8 = 4;
/// Command-line usage error, matching clap's own code.
pub const USAGE: u8 = 2;

pub fn ok() -> ExitCode {
    ExitCode::SUCCESS
}

pub fn environment() -> ExitCode {
    ExitCode::from(ENVIRONMENT)
}

/// A command failure with the exit code it maps to.
#[derive(Debug)]
pub struct Failure {
    pub code: ExitCode,
    pub message: String,
}

impl Failure {
    pub fn device(message: impl Into<String>) -> Self {
        Failure {
            code: ExitCode::from(DEVICE),
            message: message.into(),
        }
    }

    pub fn environment(message: impl Into<String>) -> Self {
        Failure {
            code: ExitCode::from(ENVIRONMENT),
            message: message.into(),
        }
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Failure {
            code: ExitCode::from(USAGE),
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure::environment(format!("{error:#}"))
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Failure::environment(error.to_string())
    }
}

impl From<tryx_legacy::LegacyError> for Failure {
    fn from(error: tryx_legacy::LegacyError) -> Self {
        use tryx_legacy::LegacyError;
        match error {
            LegacyError::AdbMissing => Failure::environment(
                "adb is not installed; it transfers media on the legacy firmware (the nix shell provides android-tools on Linux)",
            ),
            other => Failure::device(other.to_string()),
        }
    }
}

pub type CommandResult = Result<ExitCode, Failure>;
