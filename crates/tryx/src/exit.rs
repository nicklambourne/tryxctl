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
/// A media file was rejected or could not be prepared.
pub const MEDIA: u8 = 5;

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

    pub fn media(message: impl Into<String>) -> Self {
        Failure {
            code: ExitCode::from(MEDIA),
            message: message.into(),
        }
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::environment(error.to_string())
    }
}

impl From<tryx_media::MediaError> for Failure {
    fn from(error: tryx_media::MediaError) -> Self {
        use tryx_media::MediaError;
        match error {
            MediaError::FfmpegMissing | MediaError::FfprobeMissing => Failure::environment(
                format!("{error}; the nix shell provides ffmpeg and ffprobe"),
            ),
            MediaError::Io(error) => Failure::environment(error.to_string()),
            other => Failure::media(other.to_string()),
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
