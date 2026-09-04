//! `ffprobe` output, parsed leniently: ffprobe reports most numbers as
//! strings and omits what it does not know.

use crate::MediaError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Probe {
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Format {
    #[serde(default)]
    pub format_name: String,
    #[serde(default)]
    duration: Value,
    #[serde(default)]
    size: Value,
    #[serde(default)]
    bit_rate: Value,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Stream {
    #[serde(default)]
    pub index: u32,
    #[serde(default)]
    pub codec_type: String,
    #[serde(default)]
    pub codec_name: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    level: Value,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub pix_fmt: Option<String>,
    #[serde(default)]
    pub r_frame_rate: Option<String>,
    #[serde(default)]
    pub avg_frame_rate: Option<String>,
    #[serde(default)]
    nb_frames: Value,
    #[serde(default)]
    duration: Value,
    #[serde(default)]
    bit_rate: Value,
    #[serde(default)]
    has_b_frames: Value,
    #[serde(default)]
    pub sample_aspect_ratio: Option<String>,
    #[serde(default)]
    pub color_range: Option<String>,
    #[serde(default)]
    pub color_space: Option<String>,
    #[serde(default)]
    pub color_transfer: Option<String>,
    #[serde(default)]
    pub color_primaries: Option<String>,
    #[serde(default)]
    pub side_data_list: Vec<SideData>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default)]
    pub disposition: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SideData {
    #[serde(default)]
    pub side_data_type: String,
    #[serde(default)]
    rotation: Value,
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn as_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn ratio(text: &str) -> Option<f64> {
    let (numerator, denominator) = text.split_once(['/', ':'])?;
    let numerator: f64 = numerator.trim().parse().ok()?;
    let denominator: f64 = denominator.trim().parse().ok()?;
    (denominator > 0.0).then_some(numerator / denominator)
}

impl Format {
    pub fn duration(&self) -> Option<f64> {
        as_f64(&self.duration)
    }

    pub fn size(&self) -> Option<u64> {
        as_u64(&self.size)
    }

    pub fn bit_rate(&self) -> Option<u64> {
        as_u64(&self.bit_rate)
    }

    /// Whether the container is one of the names ffprobe lists, e.g. `mp4`
    /// inside `mov,mp4,m4a,3gp,3g2,mj2`.
    pub fn is_container(&self, name: &str) -> bool {
        self.format_name.split(',').any(|entry| entry == name)
    }
}

impl Stream {
    pub fn is_video(&self) -> bool {
        self.codec_type == "video"
    }

    pub fn is_audio(&self) -> bool {
        self.codec_type == "audio"
    }

    /// Cover art and similar attachments are video streams too.
    pub fn is_attached_picture(&self) -> bool {
        self.disposition.get("attached_pic").copied().unwrap_or(0) != 0
    }

    pub fn level(&self) -> Option<i64> {
        as_u64(&self.level).map(|level| level as i64)
    }

    pub fn nb_frames(&self) -> Option<u64> {
        as_u64(&self.nb_frames)
    }

    pub fn duration(&self) -> Option<f64> {
        as_f64(&self.duration)
    }

    pub fn bit_rate(&self) -> Option<u64> {
        as_u64(&self.bit_rate)
    }

    pub fn has_b_frames(&self) -> bool {
        as_u64(&self.has_b_frames).unwrap_or(0) > 0
    }

    /// The nominal frame rate (`r_frame_rate`).
    pub fn frame_rate(&self) -> Option<f64> {
        self.r_frame_rate
            .as_deref()
            .and_then(ratio)
            .filter(|rate| *rate > 0.0)
    }

    /// The measured average rate; differs from the nominal one for
    /// variable-rate sources.
    pub fn average_frame_rate(&self) -> Option<f64> {
        self.avg_frame_rate
            .as_deref()
            .and_then(ratio)
            .filter(|rate| *rate > 0.0)
    }

    /// Sample aspect ratio, `None` when square or unknown.
    pub fn sample_aspect(&self) -> Option<f64> {
        let sar = ratio(self.sample_aspect_ratio.as_deref()?)?;
        ((sar - 1.0).abs() > 0.001).then_some(sar)
    }

    /// Display rotation in degrees (0, 90, 180, 270) from the display matrix
    /// or the legacy `rotate` tag.
    pub fn rotation(&self) -> u32 {
        let raw = self
            .side_data_list
            .iter()
            .find(|side| side.side_data_type == "Display Matrix")
            .and_then(|side| as_f64(&side.rotation))
            .or_else(|| self.tags.get("rotate").and_then(|text| text.parse().ok()))
            .unwrap_or(0.0);
        (((raw.round() as i64 % 360) + 360) % 360) as u32
    }

    /// Whether the transfer or primaries mark HDR content.
    pub fn is_hdr(&self) -> bool {
        matches!(
            self.color_transfer.as_deref(),
            Some("smpte2084") | Some("arib-std-b67")
        ) || self.color_primaries.as_deref() == Some("bt2020")
    }
}

impl Probe {
    /// The first real video stream, ignoring attached pictures.
    pub fn video(&self) -> Option<&Stream> {
        self.streams
            .iter()
            .find(|stream| stream.is_video() && !stream.is_attached_picture())
    }

    pub fn has_audio(&self) -> bool {
        self.streams.iter().any(Stream::is_audio)
    }

    /// Runs `ffprobe` on `path`.
    pub fn read(ffprobe: &Path, path: &Path) -> Result<Probe, MediaError> {
        let output = Command::new(ffprobe)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
                "--",
            ])
            .arg(path)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(MediaError::Unreadable {
                path: path.to_path_buf(),
                message: stderr
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("ffprobe failed")
                    .to_string(),
            });
        }
        serde_json::from_slice(&output.stdout).map_err(|source| MediaError::ProbeJson {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VENDOR: &str = include_str!("../tests/fixtures/vendor-panorama-se-legacy.probe.json");

    #[test]
    fn parses_the_vendor_file_probe() {
        let probe: Probe = serde_json::from_str(VENDOR).unwrap();
        assert!(probe.format.is_container("mp4"));
        assert_eq!(probe.format.size(), Some(303_549_636));
        assert!((probe.format.duration().unwrap() - 180.166_667).abs() < 0.001);
        assert!(!probe.has_audio());
        let video = probe.video().unwrap();
        assert_eq!(video.codec_name, "h264");
        assert_eq!(video.profile.as_deref(), Some("High"));
        assert_eq!(video.level(), Some(41));
        assert_eq!((video.width, video.height), (Some(1920), Some(960)));
        assert_eq!(video.pix_fmt.as_deref(), Some("yuv420p"));
        assert_eq!(video.frame_rate(), Some(30.0));
        assert!((video.average_frame_rate().unwrap() - 30.0055).abs() < 0.001);
        assert_eq!(video.nb_frames(), Some(5406));
        assert!(!video.has_b_frames());
        assert_eq!(video.rotation(), 0);
        assert_eq!(video.sample_aspect(), None);
        assert!(!video.is_hdr());
    }

    #[test]
    fn reads_rotation_from_display_matrix_or_tag() {
        let matrix: Stream = serde_json::from_str(
            r#"{"codec_type":"video","side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]}"#,
        )
        .unwrap();
        assert_eq!(matrix.rotation(), 270);
        let tag: Stream =
            serde_json::from_str(r#"{"codec_type":"video","tags":{"rotate":"180"}}"#).unwrap();
        assert_eq!(tag.rotation(), 180);
    }

    #[test]
    fn numeric_strings_and_ratios_parse() {
        let stream: Stream = serde_json::from_str(
            r#"{"codec_type":"video","nb_frames":"12","has_b_frames":2,"sample_aspect_ratio":"4:3","r_frame_rate":"0/0","avg_frame_rate":"24000/1001","level":"31"}"#,
        )
        .unwrap();
        assert_eq!(stream.nb_frames(), Some(12));
        assert!(stream.has_b_frames());
        assert!((stream.sample_aspect().unwrap() - 4.0 / 3.0).abs() < 1e-9);
        assert_eq!(stream.frame_rate(), None);
        assert!((stream.average_frame_rate().unwrap() - 23.976).abs() < 0.001);
        assert_eq!(stream.level(), Some(31));
    }
}
