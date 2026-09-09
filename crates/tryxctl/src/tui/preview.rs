//! Pictures for the interface: a frame of a file on the display, or of a
//! local file as the display would get it. Rendered by ffmpeg to small
//! PNGs, decoded for the image widget, and cached on disk by name and size.

use crate::ops;
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::process::Command;
use tryx_legacy::adb::Adb;
use tryx_media::Target;
use tryx_media::check::Kind;
use tryx_media::preview;
use tryx_media::transform::Transform;

/// Bytes fetched before falling back to pulling the whole file.
const PREFIX_BYTES: u64 = 6 * 1024 * 1024;
/// Width of the cached thumbnails and wizard previews.
const WIDTH: u32 = 640;

fn thumbs_dir() -> Option<PathBuf> {
    ops::cache_dir().and_then(|dir| dir.parent().map(|parent| parent.join("thumbs")))
}

fn decode(path: &Path) -> Result<DynamicImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// One frame, about a second in when the input allows, scaled to [`WIDTH`].
fn render_small(ffmpeg: &Path, input: &Path, output: &Path) -> Result<(), String> {
    for seek in [Some("1"), None] {
        let mut command = Command::new(ffmpeg);
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
        if let Some(seconds) = seek {
            command.args(["-ss", seconds]);
        }
        let result = command
            .arg("-i")
            .arg(input)
            .args(["-map", "0:v:0", "-frames:v", "1", "-vf"])
            .arg(format!("scale={WIDTH}:-2"))
            .args(["-f", "image2", "-c:v", "png"])
            .arg(output)
            .output()
            .map_err(|e| e.to_string())?;
        if result.status.success() && output.is_file() {
            return Ok(());
        }
        if seek.is_none() {
            return Err(String::from_utf8_lossy(&result.stderr).trim().to_string());
        }
    }
    Err("no frame".to_string())
}

/// A thumbnail of a file on a legacy display, from the file's first
/// megabytes when its index is at the front, otherwise from one pull of the
/// whole file. Cached as a PNG.
pub fn device_thumbnail(
    ffmpeg: &Path,
    adb: &Adb,
    name: &str,
    size: u64,
) -> Result<DynamicImage, String> {
    let dir = thumbs_dir().ok_or("no cache directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let png = dir.join(format!("{name}-{size}.png"));
    if png.is_file() {
        return decode(&png);
    }
    let part = dir.join(format!("{name}.part"));
    let prefix = adb
        .read_prefix(name, PREFIX_BYTES)
        .map_err(|e| e.to_string())?;
    std::fs::write(&part, &prefix).map_err(|e| e.to_string())?;
    let mut rendered = render_small(ffmpeg, &part, &png);
    if rendered.is_err() && u64::try_from(prefix.len()).unwrap_or(0) < size {
        // The index sits at the end (no faststart): fetch it all, once.
        let _ = std::fs::remove_file(&part);
        adb.pull(name, &part).map_err(|e| e.to_string())?;
        rendered = render_small(ffmpeg, &part, &png);
    }
    let _ = std::fs::remove_file(&part);
    rendered.map_err(|why| format!("no frame could be decoded: {why}"))?;
    decode(&png)
}

/// A local file as the display would get it, through the transform.
pub fn local_preview(
    ffmpeg: &Path,
    path: &Path,
    kind: Kind,
    transform: &Transform,
    target: Target,
    at: Option<f64>,
) -> Result<DynamicImage, String> {
    let dir = thumbs_dir().ok_or("no cache directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let output = dir.join(format!("preview-{}.png", std::process::id()));
    preview::render_frame(ffmpeg, path, kind, transform, target, at, &output)
        .map_err(|e| e.to_string())?;
    let image = decode(&output)?;
    let _ = std::fs::remove_file(&output);
    Ok(image.thumbnail(WIDTH, WIDTH))
}
