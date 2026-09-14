//! Small media files made from ffmpeg's test sources.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Whether ffmpeg and ffprobe are installed. Without them the media tests
/// have nothing to run and say so; with `TRYXCTL_REQUIRE_FFMPEG` set, as in
/// CI, their absence fails the test instead of skipping it.
pub fn ffmpeg_available() -> bool {
    let found = ["ffmpeg", "ffprobe"].iter().all(|tool| {
        Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    });
    if !found {
        assert!(
            std::env::var_os("TRYXCTL_REQUIRE_FFMPEG").is_none(),
            "ffmpeg and ffprobe must be installed: TRYXCTL_REQUIRE_FFMPEG is set"
        );
        eprintln!("skipped: ffmpeg or ffprobe is not installed");
    }
    found
}

fn ffmpeg(args: &[&str], output: &Path) -> PathBuf {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).expect("a directory for the sample");
    }
    let mut command = Command::new("ffmpeg");
    command
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .arg(output);
    let result = crate::sandbox::output(command);
    assert!(
        result.status.success(),
        "ffmpeg could not make {}: {}",
        output.display(),
        String::from_utf8_lossy(&result.stderr)
    );
    output.to_path_buf()
}

/// A `width`×`height` still of the test pattern, in the format `path`'s
/// extension names.
pub fn picture(path: &Path, width: u32, height: u32) -> PathBuf {
    let source = format!("testsrc2=size={width}x{height}:rate=1:duration=1");
    ffmpeg(&["-f", "lavfi", "-i", &source, "-frames:v", "1"], path)
}

/// `seconds` of the moving test pattern with a tone, as H.264 and AAC in
/// MP4: a typical clip that the legacy firmware cannot take as it is.
pub fn clip(path: &Path, width: u32, height: u32, seconds: f64) -> PathBuf {
    let video = format!("testsrc2=size={width}x{height}:rate=25:duration={seconds}");
    let audio = format!("sine=frequency=440:duration={seconds}");
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &video,
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-shortest",
        ],
        path,
    )
}
