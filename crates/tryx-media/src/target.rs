//! What a display expects prepared media to look like.

use serde::Serialize;

/// The file the display consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Format {
    /// H.264 in an MP4 container (legacy cm01 firmware).
    Mp4,
    /// A raw Annex-B H.264 elementary stream (KANALI Panorama firmware).
    RawH264,
    /// A raw H.264 stream behind the Turris media header.
    Mxhd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Target {
    pub id: &'static str,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub format: Format,
}

/// Panorama coolers on the original cm01 firmware. Measured from the vendor
/// app's own upload: 1920×960 (2:1, not the panel's 2240×1080), 30 fps H.264
/// High 4.1 in MP4, no audio.
pub const LEGACY_PANORAMA: Target = Target {
    id: "legacy-panorama",
    label: "Panorama (cm01 firmware)",
    width: 1920,
    height: 960,
    fps: 30,
    format: Format::Mp4,
};

/// Panorama and Panorama SE on KANALI firmware: the full 2240×1080 panel,
/// fed a raw H.264 stream; still images become a 60 s loop.
pub const KANALI_PANORAMA: Target = Target {
    id: "kanali-panorama",
    label: "Panorama (KANALI firmware)",
    width: 2240,
    height: 1080,
    fps: 30,
    format: Format::RawH264,
};

/// Turris 620: 1280×720 Main 4.1 at 12 Mbps behind the MXHD header.
pub const KANALI_TURRIS: Target = Target {
    id: "kanali-turris",
    label: "Turris 620",
    width: 1280,
    height: 720,
    fps: 30,
    format: Format::Mxhd,
};

impl Target {
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height)
    }
}
