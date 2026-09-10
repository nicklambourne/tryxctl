//! Pictures for the interface: a short clip of a file on the display, or of
//! a local file as the display would get it. Rendered by ffmpeg to small
//! PNG frames, decoded for the image widget, and cached on disk by name
//! and size. A still image is a clip of one frame.

use crate::ops;
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tryx_legacy::adb::Adb;
use tryx_media::Target;
use tryx_media::check::Kind;
use tryx_media::preview;
use tryx_media::transform::Transform;

/// Bytes fetched before falling back to pulling the whole file.
const PREFIX_BYTES: u64 = 6 * 1024 * 1024;
/// Width of the cached frames.
const WIDTH: u32 = 480;
/// How much of a video a preview shows, looping.
const CLIP_SECONDS: f64 = 4.0;
const CLIP_FPS: u32 = 6;

/// Frames to cycle through, and how long each stays.
pub struct Clip {
    pub frames: Vec<DynamicImage>,
    pub interval: Duration,
}

fn interval() -> Duration {
    Duration::from_millis(1000 / u64::from(CLIP_FPS))
}

fn thumbs_dir() -> Option<PathBuf> {
    ops::cache_dir().and_then(|dir| dir.parent().map(|parent| parent.join("thumbs")))
}

fn decode(path: &Path) -> Result<DynamicImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

fn decode_dir(dir: &Path) -> Result<Clip, String> {
    let frames = preview::frames_in(dir)
        .map_err(|e| e.to_string())?
        .iter()
        .map(|path| decode(path))
        .collect::<Result<Vec<_>, _>>()?;
    if frames.is_empty() {
        return Err("no frame".to_string());
    }
    Ok(Clip {
        frames,
        interval: interval(),
    })
}

/// A few seconds from about a second in when the input allows, scaled to
/// [`WIDTH`], as numbered PNGs in `dir`. Whatever frames ffmpeg manages
/// from a truncated input count as success.
fn render_small_clip(ffmpeg: &Path, input: &Path, dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut last_error = String::new();
    for seek in [Some("1"), None] {
        let mut command = Command::new(ffmpeg);
        command.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
        if let Some(seconds) = seek {
            command.args(["-ss", seconds]);
        }
        let result = command
            .arg("-i")
            .arg(input)
            .args(["-map", "0:v:0", "-t", &format!("{CLIP_SECONDS:.1}"), "-vf"])
            .arg(format!("fps={CLIP_FPS},scale={WIDTH}:-2"))
            .args(["-f", "image2", "-c:v", "png"])
            .arg(dir.join("%03d.png"))
            .output()
            .map_err(|e| e.to_string())?;
        let produced = preview::frames_in(dir).map(|f| f.len()).unwrap_or(0);
        if produced > 0 {
            return Ok(());
        }
        last_error = String::from_utf8_lossy(&result.stderr).trim().to_string();
    }
    Err(if last_error.is_empty() {
        "no frame".to_string()
    } else {
        last_error
    })
}

/// A clip of a file on a legacy display, from the file's first megabytes
/// when its index is at the front, otherwise from one pull of the whole
/// file. Cached as PNG frames.
pub fn device_clip(ffmpeg: &Path, adb: &Adb, name: &str, size: u64) -> Result<Clip, String> {
    let dir = thumbs_dir().ok_or("no cache directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let frames_dir = dir.join(format!("{name}-{size}"));
    if let Ok(clip) = decode_dir(&frames_dir) {
        return Ok(clip);
    }
    let part = dir.join(format!("{name}.part"));
    let prefix = adb
        .read_prefix(name, PREFIX_BYTES)
        .map_err(|e| e.to_string())?;
    std::fs::write(&part, &prefix).map_err(|e| e.to_string())?;
    let mut rendered = render_small_clip(ffmpeg, &part, &frames_dir);
    if rendered.is_err() && u64::try_from(prefix.len()).unwrap_or(0) < size {
        // The index sits at the end (no faststart): fetch it all, once.
        let _ = std::fs::remove_file(&part);
        adb.pull(name, &part).map_err(|e| e.to_string())?;
        rendered = render_small_clip(ffmpeg, &part, &frames_dir);
    }
    let _ = std::fs::remove_file(&part);
    if let Err(why) = rendered {
        let _ = std::fs::remove_dir_all(&frames_dir);
        return Err(format!("no frame could be decoded: {why}"));
    }
    decode_dir(&frames_dir)
}

/// A clip of a local file as the display would get it, through the
/// transform.
pub fn local_clip(
    ffmpeg: &Path,
    path: &Path,
    kind: Kind,
    transform: &Transform,
    target: Target,
    start: Option<f64>,
) -> Result<Clip, String> {
    let dir = thumbs_dir().ok_or("no cache directory")?;
    let frames_dir = dir.join(format!("preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&frames_dir);
    let rendered = preview::render_clip(
        ffmpeg,
        path,
        kind,
        transform,
        target,
        start,
        CLIP_SECONDS,
        CLIP_FPS,
        WIDTH,
        &frames_dir,
    )
    .map_err(|e| e.to_string());
    let clip = rendered.and_then(|_| decode_dir(&frames_dir));
    let _ = std::fs::remove_dir_all(&frames_dir);
    clip
}
