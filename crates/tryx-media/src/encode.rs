//! Running ffmpeg with progress, verifying its output, and hashing files.

use crate::MediaError;
use crate::check::Kind;
use crate::plan::{Action, Plan};
use crate::probe::Probe;
use crate::target::{Format, Target};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// Output position in seconds.
    pub seconds: f64,
    /// Bytes written so far.
    pub bytes: u64,
    /// Fraction done when the source duration is known.
    pub fraction: Option<f64>,
}

/// Locates ffmpeg and ffprobe on PATH.
pub fn tools() -> Result<(PathBuf, PathBuf), MediaError> {
    let ffmpeg = which::which("ffmpeg").map_err(|_| MediaError::FfmpegMissing)?;
    let ffprobe = which::which("ffprobe").map_err(|_| MediaError::FfprobeMissing)?;
    Ok((ffmpeg, ffprobe))
}

/// Executes the plan, writing to `output` and reporting progress.
pub fn run(
    ffmpeg: &Path,
    plan: &Plan,
    output: &Path,
    duration: Option<f64>,
    mut on_progress: impl FnMut(Progress),
) -> Result<(), MediaError> {
    if plan.action == Action::Passthrough {
        std::fs::copy(&plan.input, output)?;
        return Ok(());
    }
    let mut child = Command::new(ffmpeg)
        .args(["-progress", "pipe:1", "-nostats", "-loglevel", "error"])
        .args(plan.ffmpeg_args(output))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stderr = child.stderr.take().expect("stderr is piped");
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });

    let stdout = child.stdout.take().expect("stdout is piped");
    let mut progress = Progress::default();
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "out_time_us" => {
                if let Ok(micros) = value.trim().parse::<i64>() {
                    progress.seconds = micros.max(0) as f64 / 1e6;
                }
            }
            "total_size" => progress.bytes = value.trim().parse().unwrap_or(progress.bytes),
            "progress" => {
                progress.fraction = duration
                    .filter(|seconds| *seconds > 0.0)
                    .map(|seconds| (progress.seconds / seconds).clamp(0.0, 1.0));
                if value.trim() == "end" {
                    progress.fraction = duration.map(|_| 1.0);
                }
                on_progress(progress);
            }
            _ => {}
        }
    }

    let status = child.wait()?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(MediaError::Encode {
            status: status.to_string(),
            stderr: stderr.trim().to_string(),
        });
    }
    Ok(())
}

/// Probes the finished file and checks it is what the plan promised.
pub fn verify(
    ffprobe: &Path,
    plan: &Plan,
    output: &Path,
    target: Target,
) -> Result<Probe, MediaError> {
    let fail = |message: String| MediaError::Verify {
        path: output.to_path_buf(),
        message,
    };
    if std::fs::metadata(output)?.len() == 0 {
        return Err(fail("the output is empty".to_string()));
    }
    let probe = Probe::read(ffprobe, output)?;
    let video = probe
        .video()
        .ok_or_else(|| fail("the output has no video stream".to_string()))?;
    let dimensions = (video.width.unwrap_or(0), video.height.unwrap_or(0));
    let container = match target.format {
        Format::Mp4 => "mp4",
        Format::RawH264 | Format::Mxhd => "h264",
    };
    let expect_stream = |probe: &Probe, video: &crate::probe::Stream| -> Result<(), MediaError> {
        if video.codec_name != "h264" || !probe.format.is_container(container) {
            return Err(fail(format!(
                "expected an H.264 {}",
                container.to_uppercase()
            )));
        }
        Ok(())
    };
    match (plan.kind, plan.action) {
        (_, Action::Passthrough) => {}
        (Kind::Image, _) if target.format == Format::Mp4 => {
            if video.codec_name != "png" {
                return Err(fail(format!("expected a PNG, found {}", video.codec_name)));
            }
            if dimensions != (target.width, target.height) {
                return Err(fail(format!(
                    "expected {}×{}, found {}×{}",
                    target.width, target.height, dimensions.0, dimensions.1
                )));
            }
        }
        (_, Action::Remux) => expect_stream(&probe, video)?,
        (kind, Action::Encode) => {
            expect_stream(&probe, video)?;
            if dimensions != (target.width, target.height) {
                return Err(fail(format!(
                    "expected {}×{}, found {}×{}",
                    target.width, target.height, dimensions.0, dimensions.1
                )));
            }
            // Elementary streams expose x264's field-rate tick (60/1 for
            // 30 fps), not the frame rate, so only containers are checked.
            let _ = kind;
            if target.format == Format::Mp4
                && video
                    .frame_rate()
                    .is_none_or(|fps| (fps - f64::from(target.fps)).abs() > 0.05)
            {
                return Err(fail(format!(
                    "expected {} fps, found {:?}",
                    target.fps, video.r_frame_rate
                )));
            }
            if probe.has_audio() {
                return Err(fail("the output still has audio".to_string()));
            }
        }
    }
    Ok(probe)
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_files_like_sha256sum() {
        let dir = std::env::temp_dir().join(format!("tryx-media-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
