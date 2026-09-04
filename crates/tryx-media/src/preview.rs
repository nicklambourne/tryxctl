//! Frames rendered through the same filter graph the encoder uses, so what
//! the preview shows is what the display will get.

use crate::MediaError;
use crate::check::Kind;
use crate::target::Target;
use crate::transform::Transform;
use std::path::Path;
use std::process::Command;

/// Renders one frame of `input` at `at` seconds (ignored for images) to a
/// PNG at the target size.
pub fn render_frame(
    ffmpeg: &Path,
    input: &Path,
    kind: Kind,
    transform: &Transform,
    target: Target,
    at: Option<f64>,
    output: &Path,
) -> Result<(), MediaError> {
    let mut command = Command::new(ffmpeg);
    command.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    if kind != Kind::Image
        && let Some(seconds) = at
    {
        command.args(["-ss", &format!("{seconds:.3}")]);
    }
    command
        .arg("-i")
        .arg(input)
        .args(["-map", "0:v:0", "-frames:v", "1", "-vf"])
        .arg(transform.image_filter(target.width, target.height))
        .args(["-f", "image2", "-c:v", "png"])
        .arg(output);
    run(command)
}

/// Renders a 3×3 sheet of frames spread over `duration` seconds.
pub fn render_sheet(
    ffmpeg: &Path,
    input: &Path,
    transform: &Transform,
    target: Target,
    duration: f64,
    output: &Path,
) -> Result<(), MediaError> {
    let interval = (duration / 9.0).max(0.1);
    let filter = format!(
        "fps=1/{interval:.4},{},scale=iw/3:ih/3,tile=3x3",
        transform.image_filter(target.width, target.height)
    );
    let mut command = Command::new(ffmpeg);
    command
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-f",
            "image2",
            "-c:v",
            "png",
        ])
        .arg(output);
    run(command)
}

fn run(mut command: Command) -> Result<(), MediaError> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(MediaError::Encode {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(())
}
