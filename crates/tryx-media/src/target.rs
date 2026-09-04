//! What a display expects prepared media to look like.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Target {
    pub id: &'static str,
    pub label: &'static str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
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
};

impl Target {
    pub fn aspect(&self) -> f64 {
        f64::from(self.width) / f64::from(self.height)
    }
}
