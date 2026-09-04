//! Media validation, planning, and conversion for TRYX displays.
//!
//! The pipeline is probe → classify → check → plan → encode → verify. Every
//! check yields a [`check::Finding`] with a severity, so a caller can show
//! what would happen before anything is converted.

pub mod check;
pub mod encode;
pub mod plan;
pub mod probe;
pub mod target;
pub mod transform;

pub use check::{Finding, Kind, Report, Severity};
pub use plan::Plan;
pub use probe::Probe;
pub use target::Target;
pub use transform::{Mode, Transform};

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("ffprobe is not installed")]
    FfprobeMissing,
    #[error("ffmpeg is not installed")]
    FfmpegMissing,
    #[error("{path}: {message}")]
    Unreadable { path: PathBuf, message: String },
    #[error("{path}: ffprobe output is not valid JSON: {source}")]
    ProbeJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("ffmpeg failed ({status}): {stderr}")]
    Encode { status: String, stderr: String },
    #[error("{path}: {message}")]
    Verify { path: PathBuf, message: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
