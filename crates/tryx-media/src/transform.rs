//! Fit, Fill, Crop, and Stretch transforms and their ffmpeg filter graph.
//!
//! The filter graph is reproduced from DXVSI/Tryx-Linux-GUI
//! `src/mediatransform.cpp` so that both projects prepare pixels
//! identically; only the target size and frame rate are parameters.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Mode {
    /// Scale to fit inside the target and pad with a background colour.
    Fit,
    /// Scale to cover the target and crop the overflow, centred.
    Fill,
    /// Like Fill with a zoom factor and a focus point.
    Crop,
    /// Scale to the target size ignoring the aspect ratio.
    Stretch,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text.to_ascii_lowercase().as_str() {
            "fit" => Some(Mode::Fit),
            "fill" => Some(Mode::Fill),
            "crop" => Some(Mode::Crop),
            "stretch" => Some(Mode::Stretch),
            _ => None,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Mode::Fit => "Fit",
            Mode::Fill => "Fill",
            Mode::Crop => "Crop",
            Mode::Stretch => "Stretch",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Transform {
    pub mode: Mode,
    /// Clockwise quarter turns, 0 to 3.
    pub rotation_quarter_turns: u32,
    /// Crop zoom, 1000 (100 %) to 4000 (400 %).
    pub zoom_permille: u32,
    /// Crop focus, 0 to 10000 across each axis; 5000 is the centre.
    pub focus_x: u32,
    pub focus_y: u32,
    /// Fit background as 0xRRGGBB.
    pub background_rgb: u32,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            mode: Mode::Fit,
            rotation_quarter_turns: 0,
            zoom_permille: 1000,
            focus_x: 5000,
            focus_y: 5000,
            background_rgb: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidTransform {
    #[error("rotation must be 0, 90, 180, or 270 degrees")]
    Rotation,
    #[error("zoom must be between 100 % and 400 %")]
    Zoom,
    #[error("focus must be between 0 % and 100 %")]
    Focus,
    #[error("background colour must be #RRGGBB")]
    Background,
    #[error("zoom and focus apply to Crop mode only")]
    ZoomOutsideCrop,
    #[error("a background colour applies to Fit mode only")]
    BackgroundOutsideFit,
}

impl Transform {
    pub fn validate(&self) -> Result<(), InvalidTransform> {
        if self.rotation_quarter_turns > 3 {
            return Err(InvalidTransform::Rotation);
        }
        if !(1000..=4000).contains(&self.zoom_permille) {
            return Err(InvalidTransform::Zoom);
        }
        if self.focus_x > 10000 || self.focus_y > 10000 {
            return Err(InvalidTransform::Focus);
        }
        if self.background_rgb > 0x00FF_FFFF {
            return Err(InvalidTransform::Background);
        }
        if self.mode != Mode::Crop && !self.is_neutral_viewport() {
            return Err(InvalidTransform::ZoomOutsideCrop);
        }
        if self.mode != Mode::Fit && self.background_rgb != 0 {
            return Err(InvalidTransform::BackgroundOutsideFit);
        }
        Ok(())
    }

    fn is_neutral_viewport(&self) -> bool {
        self.zoom_permille == 1000 && self.focus_x == 5000 && self.focus_y == 5000
    }

    /// Whether this is the default Fit transform.
    pub fn is_default(&self) -> bool {
        *self == Transform::default()
    }

    /// Stable text form, hashed into [`Transform::fingerprint`].
    pub fn canonical(&self) -> String {
        format!(
            "v=1;mode={};rotation={};zoom={};focus-x={};focus-y={};background={:06x}",
            self.mode,
            self.rotation_quarter_turns,
            self.zoom_permille,
            self.focus_x,
            self.focus_y,
            self.background_rgb
        )
        .to_lowercase()
    }

    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(self.canonical().as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// The ffmpeg `-vf` graph for a video target: rotation, square pixels,
    /// the mode, then `setsar=1,format=yuv420p,fps=N`.
    pub fn video_filter(&self, width: u32, height: u32, fps: u32) -> String {
        format!(
            "{}setsar=1,format=yuv420p,fps={fps}",
            self.geometry_filter(width, height)
        )
    }

    /// The graph for a still image target, without frame-rate conversion.
    pub fn image_filter(&self, width: u32, height: u32) -> String {
        format!(
            "{}setsar=1,format=rgb24",
            self.geometry_filter(width, height)
        )
    }

    fn geometry_filter(&self, width: u32, height: u32) -> String {
        let rotation = match self.rotation_quarter_turns {
            1 => "transpose=clock,",
            2 => "hflip,vflip,",
            3 => "transpose=cclock,",
            _ => "",
        };
        let square_pixels = "scale='if(lte(sar,0),iw,max(1,round(iw*sar)))':ih,setsar=1,";
        let body = match self.mode {
            Mode::Fit => format!(
                "scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=0x{:06x},",
                self.background_rgb
            ),
            Mode::Fill => cover_viewport(width, height, 1000, 5000, 5000),
            Mode::Stretch => format!("scale={width}:{height},"),
            Mode::Crop => cover_viewport(
                width,
                height,
                self.zoom_permille,
                self.focus_x,
                self.focus_y,
            ),
        };
        format!("{rotation}{square_pixels}{body}")
    }
}

fn cover_viewport(width: u32, height: u32, zoom: u32, focus_x: u32, focus_y: u32) -> String {
    format!(
        "scale={width}:{height}:force_original_aspect_ratio=increase:force_divisible_by=2,\
         scale='trunc(iw*{zoom}/1000/2)*2':'trunc(ih*{zoom}/1000/2)*2',\
         crop={width}:{height}:'trunc((iw-{width})*{focus_x}/10000/2)*2':'trunc((ih-{height})*{focus_y}/10000/2)*2',"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "scale='if(lte(sar,0),iw,max(1,round(iw*sar)))':ih,setsar=1,";
    const SUFFIX: &str = "setsar=1,format=yuv420p,fps=30";

    #[test]
    fn fit_matches_upstream_filter() {
        let filter = Transform::default().video_filter(1920, 960, 30);
        assert_eq!(
            filter,
            format!(
                "{PREFIX}scale=1920:960:force_original_aspect_ratio=decrease,pad=1920:960:(ow-iw)/2:(oh-ih)/2:color=0x000000,{SUFFIX}"
            )
        );
    }

    #[test]
    fn fill_and_crop_share_the_cover_graph() {
        let fill = Transform {
            mode: Mode::Fill,
            ..Transform::default()
        };
        assert_eq!(
            fill.video_filter(2240, 1080, 30),
            format!(
                "{PREFIX}scale=2240:1080:force_original_aspect_ratio=increase:force_divisible_by=2,scale='trunc(iw*1000/1000/2)*2':'trunc(ih*1000/1000/2)*2',crop=2240:1080:'trunc((iw-2240)*5000/10000/2)*2':'trunc((ih-1080)*5000/10000/2)*2',{SUFFIX}"
            )
        );
        let crop = Transform {
            mode: Mode::Crop,
            zoom_permille: 1500,
            focus_x: 2500,
            focus_y: 7500,
            ..Transform::default()
        };
        assert!(crop.video_filter(2240, 1080, 30).contains(
            "scale='trunc(iw*1500/1000/2)*2':'trunc(ih*1500/1000/2)*2',crop=2240:1080:'trunc((iw-2240)*2500/10000/2)*2':'trunc((ih-1080)*7500/10000/2)*2',"
        ));
    }

    #[test]
    fn stretch_rotation_and_background() {
        let transform = Transform {
            mode: Mode::Stretch,
            rotation_quarter_turns: 1,
            ..Transform::default()
        };
        assert_eq!(
            transform.video_filter(1280, 720, 30),
            format!("transpose=clock,{PREFIX}scale=1280:720,{SUFFIX}")
        );
        let flipped = Transform {
            rotation_quarter_turns: 2,
            background_rgb: 0x112233,
            ..Transform::default()
        };
        let filter = flipped.video_filter(1920, 960, 30);
        assert!(filter.starts_with("hflip,vflip,"));
        assert!(filter.contains("color=0x112233,"));
        assert!(
            Transform {
                rotation_quarter_turns: 3,
                ..Transform::default()
            }
            .video_filter(1920, 960, 30)
            .starts_with("transpose=cclock,")
        );
    }

    #[test]
    fn image_filter_skips_frame_rate_conversion() {
        let filter = Transform::default().image_filter(1920, 960);
        assert!(filter.ends_with("setsar=1,format=rgb24"));
        assert!(!filter.contains("fps="));
    }

    #[test]
    fn validation_mirrors_upstream_rules() {
        assert_eq!(Transform::default().validate(), Ok(()));
        let zoom_in_fit = Transform {
            zoom_permille: 2000,
            ..Transform::default()
        };
        assert_eq!(
            zoom_in_fit.validate(),
            Err(InvalidTransform::ZoomOutsideCrop)
        );
        let bg_in_fill = Transform {
            mode: Mode::Fill,
            background_rgb: 1,
            ..Transform::default()
        };
        assert_eq!(
            bg_in_fill.validate(),
            Err(InvalidTransform::BackgroundOutsideFit)
        );
        assert_eq!(
            Transform {
                zoom_permille: 999,
                mode: Mode::Crop,
                ..Transform::default()
            }
            .validate(),
            Err(InvalidTransform::Zoom)
        );
        assert_eq!(
            Transform {
                rotation_quarter_turns: 4,
                ..Transform::default()
            }
            .validate(),
            Err(InvalidTransform::Rotation)
        );
    }

    #[test]
    fn canonical_form_matches_upstream() {
        assert_eq!(
            Transform::default().canonical(),
            "v=1;mode=fit;rotation=0;zoom=1000;focus-x=5000;focus-y=5000;background=000000"
        );
        assert_eq!(Transform::default().fingerprint().len(), 64);
    }
}
