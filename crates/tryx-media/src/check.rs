//! Findings about a source file measured against a target.

use crate::probe::{Probe, Stream};
use crate::target::{Format, Target};
use crate::transform::{Mode, Transform};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Sources larger than this are refused before any work starts.
pub const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Aspect ratios within this fraction of the target count as matching.
const ASPECT_TOLERANCE: f64 = 0.01;
/// Image containers as ffprobe names them.
const IMAGE_CONTAINERS: [&str; 8] = [
    "image2",
    "png_pipe",
    "jpeg_pipe",
    "webp_pipe",
    "bmp_pipe",
    "tiff_pipe",
    "gif",
    "apng",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Compliant as is.
    Ok,
    /// Fixed by the plan without asking.
    Auto,
    /// A default policy applied; a flag changes it.
    Decide,
    /// Cannot be used.
    Fatal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Image,
    AnimatedImage,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// What the plan must do, derived from the findings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Requirements {
    pub re_encode: bool,
    pub remux: bool,
    pub tonemap: bool,
}

/// The measured facts a plan and a listing need.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Source {
    pub container: String,
    pub codec: String,
    pub profile: Option<String>,
    pub pix_fmt: Option<String>,
    /// Stored dimensions.
    pub width: u32,
    pub height: u32,
    /// Dimensions after rotation and pixel-aspect correction.
    pub display_width: u32,
    pub display_height: u32,
    pub rotation: u32,
    pub frame_rate: Option<f64>,
    pub duration: Option<f64>,
    pub size: u64,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub path: PathBuf,
    pub target: Target,
    pub kind: Option<Kind>,
    pub source: Source,
    pub findings: Vec<Finding>,
    pub requirements: Requirements,
    /// Name the file will have on the display.
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub transform: Transform,
    /// The user chose the mode; an aspect mismatch is then not a decision.
    pub transform_explicit: bool,
    /// Convert HDR sources instead of refusing them.
    pub tonemap: bool,
    pub name: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            transform: Transform::default(),
            transform_explicit: false,
            tonemap: true,
            name: None,
        }
    }
}

impl Report {
    pub fn worst(&self) -> Severity {
        self.findings
            .iter()
            .map(|finding| finding.severity)
            .max()
            .unwrap_or(Severity::Ok)
    }

    /// Whether the file can be used; under `strict` anything the plan would
    /// silently change also blocks.
    pub fn acceptable(&self, strict: bool) -> bool {
        match self.worst() {
            Severity::Fatal => false,
            Severity::Ok => true,
            Severity::Auto | Severity::Decide => !strict,
        }
    }
}

/// Classifies by container first: single images arrive through ffmpeg's
/// image demuxers, animations through `gif`/`apng` or an animated `webp`.
pub fn classify(probe: &Probe) -> Option<Kind> {
    let video = probe.video()?;
    let is_image_container = IMAGE_CONTAINERS
        .iter()
        .any(|name| probe.format.is_container(name));
    if !is_image_container {
        return Some(Kind::Video);
    }
    let animated = video.nb_frames().is_some_and(|frames| frames > 1)
        || (matches!(video.codec_name.as_str(), "gif" | "apng" | "webp")
            && probe
                .format
                .duration()
                .is_some_and(|seconds| seconds > 0.15));
    Some(if animated {
        Kind::AnimatedImage
    } else {
        Kind::Image
    })
}

pub fn check(path: &Path, size: u64, probe: &Probe, target: Target, options: &Options) -> Report {
    let mut findings = Vec::new();
    let mut requirements = Requirements::default();
    let mut source = Source {
        container: probe.format.format_name.clone(),
        size,
        has_audio: probe.has_audio(),
        ..Source::default()
    };

    if size == 0 {
        findings.push(fatal("TRYX-M-EMPTY", "the file is empty"));
    } else if size > MAX_SOURCE_BYTES {
        findings.push(fatal("TRYX-M-SIZE", "the file is larger than 8 GiB"));
    }

    let kind = classify(probe);
    let Some(video) = probe.video() else {
        findings.push(fatal("TRYX-M-STREAM", "no video or image stream found"));
        return Report {
            path: path.to_path_buf(),
            target,
            kind,
            source,
            findings,
            requirements,
            name: derive_name(path, options.name.as_deref(), kind, false, &mut Vec::new()),
        };
    };
    let kind = kind.expect("a video stream implies a kind");
    fill_source(&mut source, video, probe);
    if options.transform_explicit || !options.transform.is_default() {
        requirements.re_encode = true;
        findings.push(ok(
            "TRYX-M-TRANSFORM",
            format!("{} transform requested", options.transform.mode),
        ));
    }

    match kind {
        Kind::Image => check_image(
            video,
            &source,
            target,
            options,
            &mut findings,
            &mut requirements,
        ),
        Kind::AnimatedImage | Kind::Video => check_video(
            video,
            &source,
            target,
            options,
            &mut findings,
            &mut requirements,
        ),
    }

    if target.format != Format::Mp4 {
        requirements.re_encode = true;
        requirements.remux = false;
        findings.push(auto(
            "TRYX-M-FORMAT",
            format!("{} plays raw H.264 streams", target.label),
            match kind {
                Kind::Image if target.format == Format::RawH264 => "encode as a 60 s H.264 loop",
                Kind::Image => "encode as a single H.264 frame",
                _ => "encode as a raw H.264 stream",
            },
        ));
    }

    let passthrough = !requirements.re_encode && !requirements.remux;
    let name = derive_name(
        path,
        options.name.as_deref(),
        Some(kind),
        passthrough,
        &mut findings,
    );
    Report {
        path: path.to_path_buf(),
        target,
        kind: Some(kind),
        source,
        findings,
        requirements,
        name,
    }
}

fn fill_source(source: &mut Source, video: &Stream, probe: &Probe) {
    source.codec = video.codec_name.clone();
    source.profile = video.profile.clone();
    source.pix_fmt = video.pix_fmt.clone();
    source.width = video.width.unwrap_or(0);
    source.height = video.height.unwrap_or(0);
    source.rotation = video.rotation();
    source.frame_rate = video.frame_rate();
    source.duration = probe.format.duration().or_else(|| video.duration());
    let sar = video.sample_aspect().unwrap_or(1.0);
    let (width, height) = if matches!(source.rotation, 90 | 270) {
        (source.height, source.width)
    } else {
        (source.width, source.height)
    };
    source.display_width = (f64::from(width) * sar).round() as u32;
    source.display_height = height;
}

fn check_video(
    video: &Stream,
    source: &Source,
    target: Target,
    options: &Options,
    findings: &mut Vec<Finding>,
    requirements: &mut Requirements,
) {
    let compliant_codec = video.codec_name == "h264"
        && video.profile.as_deref().is_some_and(|profile| {
            matches!(
                profile,
                "Baseline" | "Constrained Baseline" | "Main" | "High"
            )
        })
        && video.level().is_none_or(|level| level <= 41);
    if compliant_codec {
        findings.push(ok(
            "TRYX-M-CODEC",
            format!(
                "H.264 {} level {}",
                video.profile.as_deref().unwrap_or("?"),
                level_text(video.level())
            ),
        ));
    } else {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-CODEC",
            format!("codec is {}{}", video.codec_name, profile_suffix(video)),
            "re-encode as H.264 High 4.1",
        ));
    }

    check_pixels(video, options.tonemap, findings, requirements);
    check_geometry(source, target, options, findings, requirements);

    match (video.frame_rate(), video.average_frame_rate()) {
        (Some(nominal), average) if (nominal - f64::from(target.fps)).abs() < 0.05 => {
            if average.is_some_and(|average| (average - nominal).abs() > 0.5) {
                requirements.re_encode = true;
                findings.push(auto(
                    "TRYX-M-FPS",
                    format!(
                        "variable frame rate (nominal {nominal:.3}, average {:.3})",
                        average.unwrap()
                    ),
                    format!("resample to a constant {} fps", target.fps),
                ));
            } else {
                findings.push(ok("TRYX-M-FPS", format!("{} fps", target.fps)));
            }
        }
        (Some(nominal), _) => {
            requirements.re_encode = true;
            findings.push(auto(
                "TRYX-M-FPS",
                format!("{nominal:.3} fps"),
                format!("resample to {} fps", target.fps),
            ));
        }
        (None, _) => {
            requirements.re_encode = true;
            findings.push(auto(
                "TRYX-M-FPS",
                "frame rate unknown",
                format!("resample to {} fps", target.fps),
            ));
        }
    }

    if video.has_b_frames() {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-BFRAMES",
            "the stream uses B-frames",
            "re-encode without B-frames",
        ));
    }

    if target.format == Format::Mp4 {
        if source.container.split(',').any(|name| name == "mp4") {
            findings.push(ok("TRYX-M-CONTAINER", "MP4 container"));
        } else {
            requirements.remux = true;
            findings.push(auto(
                "TRYX-M-CONTAINER",
                format!("container is {}", source.container),
                "rewrap as MP4",
            ));
        }

        if source.has_audio {
            requirements.remux = true;
            findings.push(auto(
                "TRYX-M-AUDIO",
                "the file has an audio track",
                "drop it; the display has no speaker",
            ));
        }
    }
}

fn check_image(
    video: &Stream,
    source: &Source,
    target: Target,
    options: &Options,
    findings: &mut Vec<Finding>,
    requirements: &mut Requirements,
) {
    if matches!(video.codec_name.as_str(), "png" | "mjpeg") {
        findings.push(ok(
            "TRYX-M-CODEC",
            if video.codec_name == "png" {
                "PNG image"
            } else {
                "JPEG image"
            },
        ));
    } else {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-CODEC",
            format!("image codec is {}", video.codec_name),
            "convert to PNG",
        ));
    }
    check_pixels(video, options.tonemap, findings, requirements);
    check_geometry(source, target, options, findings, requirements);
}

fn check_pixels(
    video: &Stream,
    tonemap: bool,
    findings: &mut Vec<Finding>,
    requirements: &mut Requirements,
) {
    let pix_fmt = video.pix_fmt.as_deref().unwrap_or("unknown");
    let has_alpha = pix_fmt.contains('a') && pix_fmt != "yuv420p" && !pix_fmt.starts_with("gray")
        || pix_fmt == "pal8";
    if has_alpha {
        requirements.re_encode = true;
        findings.push(decide(
            "TRYX-M-ALPHA",
            format!("pixel format {pix_fmt} carries transparency"),
            "flatten onto the background colour (black unless --bg is given)",
        ));
    } else if video.codec_name == "png" || video.codec_name == "mjpeg" {
        findings.push(ok("TRYX-M-PIXFMT", pix_fmt));
    } else if pix_fmt == "yuv420p" {
        findings.push(ok("TRYX-M-PIXFMT", "yuv420p"));
    } else {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-PIXFMT",
            format!("pixel format is {pix_fmt}"),
            "convert to 8-bit yuv420p",
        ));
    }
    if video.is_hdr() {
        requirements.re_encode = true;
        let message = format!(
            "HDR content ({} / {})",
            video.color_transfer.as_deref().unwrap_or("?"),
            video.color_primaries.as_deref().unwrap_or("?")
        );
        if tonemap {
            requirements.tonemap = true;
            findings.push(decide(
                "TRYX-M-HDR",
                message,
                "tone-map to BT.709 (needs ffmpeg with zscale); --no-tonemap refuses HDR instead",
            ));
        } else {
            findings.push(fatal(
                "TRYX-M-HDR",
                format!("{message}; refused because of --no-tonemap"),
            ));
        }
    }
}

fn check_geometry(
    source: &Source,
    target: Target,
    options: &Options,
    findings: &mut Vec<Finding>,
    requirements: &mut Requirements,
) {
    if source.rotation != 0 {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-ROTATION",
            format!("stored with a {}° rotation flag", source.rotation),
            "bake the rotation into the pixels",
        ));
    }
    if source.display_width != source.width && source.rotation == 0 {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-SAR",
            format!(
                "non-square pixels (displays as {}×{})",
                source.display_width, source.display_height
            ),
            "resample to square pixels",
        ));
    }
    if source.display_width == 0 || source.display_height == 0 {
        findings.push(fatal(
            "TRYX-M-GEOMETRY",
            "the stream has no usable dimensions",
        ));
        return;
    }
    let aspect = f64::from(source.display_width) / f64::from(source.display_height);
    let aspect_matches = ((aspect - target.aspect()) / target.aspect()).abs() <= ASPECT_TOLERANCE;
    if source.display_width == target.width && source.display_height == target.height {
        findings.push(ok(
            "TRYX-M-RESOLUTION",
            format!("{}×{}", target.width, target.height),
        ));
    } else if aspect_matches {
        requirements.re_encode = true;
        findings.push(auto(
            "TRYX-M-RESOLUTION",
            format!("{}×{}", source.display_width, source.display_height),
            format!("scale to {}×{}", target.width, target.height),
        ));
    } else {
        requirements.re_encode = true;
        let message = format!(
            "{}×{} is {:.3}:1, the display is {:.3}:1",
            source.display_width,
            source.display_height,
            aspect,
            target.aspect()
        );
        if options.transform_explicit {
            findings.push(ok(
                "TRYX-M-ASPECT",
                format!("{message}; {} applied", options.transform.mode),
            ));
        } else {
            findings.push(decide(
                "TRYX-M-ASPECT",
                message,
                match options.transform.mode {
                    Mode::Fit => {
                        "letterbox onto the background (--mode fill, crop, or stretch to change)"
                    }
                    _ => "apply the requested mode",
                },
            ));
        }
    }
}

fn derive_name(
    path: &Path,
    requested: Option<&str>,
    kind: Option<Kind>,
    passthrough: bool,
    findings: &mut Vec<Finding>,
) -> String {
    let source_extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let extension = match kind {
        Some(Kind::Image)
            if passthrough && matches!(source_extension.as_str(), "png" | "jpg" | "jpeg") =>
        {
            source_extension.clone()
        }
        Some(Kind::Image) => "png".to_string(),
        _ => "mp4".to_string(),
    };
    let suffix = format!(".{extension}");
    let stem = match requested {
        Some(name) => name.strip_suffix(&suffix).unwrap_or(name).to_string(),
        None => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let safe = sanitize_stem(&stem);
    let name = format!("{safe}{suffix}");
    if safe != stem {
        findings.push(auto(
            "TRYX-M-NAME",
            format!("{stem:?} is not a safe device file name"),
            format!("store it as {name}"),
        ));
    }
    name
}

/// Reduces a stem to the device-safe charset, ASCII letters, digits, `.`,
/// `_`, `-`, without a leading dot.
pub fn sanitize_stem(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut last_dash = false;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches(|ch| ch == '-' || ch == '.').to_string();
    let trimmed: String = trimmed.chars().take(100).collect();
    if trimmed.is_empty() {
        "media".to_string()
    } else {
        trimmed
    }
}

fn level_text(level: Option<i64>) -> String {
    match level {
        Some(level) => format!("{}.{}", level / 10, level % 10),
        None => "?".to_string(),
    }
}

fn profile_suffix(video: &Stream) -> String {
    match (&video.profile, video.level()) {
        (Some(profile), Some(level)) => format!(" {profile} level {}", level_text(Some(level))),
        (Some(profile), None) => format!(" {profile}"),
        _ => String::new(),
    }
}

fn ok(code: &'static str, message: impl Into<String>) -> Finding {
    Finding {
        code,
        severity: Severity::Ok,
        message: message.into(),
        action: None,
    }
}

fn auto(code: &'static str, message: impl Into<String>, action: impl Into<String>) -> Finding {
    Finding {
        code,
        severity: Severity::Auto,
        message: message.into(),
        action: Some(action.into()),
    }
}

fn decide(code: &'static str, message: impl Into<String>, action: impl Into<String>) -> Finding {
    Finding {
        code,
        severity: Severity::Decide,
        message: message.into(),
        action: Some(action.into()),
    }
}

fn fatal(code: &'static str, message: impl Into<String>) -> Finding {
    Finding {
        code,
        severity: Severity::Fatal,
        message: message.into(),
        action: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::LEGACY_PANORAMA;

    const VENDOR: &str = include_str!("../tests/fixtures/vendor-panorama-se-legacy.probe.json");

    fn vendor_probe() -> Probe {
        serde_json::from_str(VENDOR).unwrap()
    }

    fn codes(report: &Report, severity: Severity) -> Vec<&'static str> {
        report
            .findings
            .iter()
            .filter(|finding| finding.severity == severity)
            .map(|finding| finding.code)
            .collect()
    }

    #[test]
    fn the_vendor_file_passes_through_untouched() {
        let probe = vendor_probe();
        let report = check(
            Path::new("/x/2025-12-09_19-48-54-100.mp4"),
            303_549_636,
            &probe,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, Some(Kind::Video));
        assert_eq!(report.worst(), Severity::Ok, "{:?}", report.findings);
        assert_eq!(report.requirements, Requirements::default());
        assert_eq!(report.name, "2025-12-09_19-48-54-100.mp4");
        assert!(report.acceptable(true));
    }

    #[test]
    fn a_requested_transform_forces_a_re_encode() {
        let report = check(
            Path::new("/x/v.mp4"),
            10,
            &vendor_probe(),
            LEGACY_PANORAMA,
            &Options {
                transform: Transform {
                    mode: Mode::Fill,
                    ..Transform::default()
                },
                transform_explicit: true,
                ..Options::default()
            },
        );
        assert!(report.requirements.re_encode);
        assert!(codes(&report, Severity::Ok).contains(&"TRYX-M-TRANSFORM"));
    }

    fn probe_json(format: &str, stream: &str) -> Probe {
        serde_json::from_str(&format!(r#"{{"format":{format},"streams":[{stream}]}}"#)).unwrap()
    }

    #[test]
    fn a_16_9_hevc_clip_with_audio_needs_decisions_and_fixes() {
        let probe = probe_json(
            r#"{"format_name":"matroska,webm","duration":"12.5"}"#,
            r#"{"codec_type":"video","codec_name":"hevc","profile":"Main 10","width":3840,"height":2160,"pix_fmt":"yuv420p10le","r_frame_rate":"24000/1001","avg_frame_rate":"24000/1001","has_b_frames":2,"color_transfer":"smpte2084","color_primaries":"bt2020"},{"codec_type":"audio","codec_name":"aac"}"#,
        );
        let report = check(
            Path::new("/x/My Movie (1).mkv"),
            5_000_000,
            &probe,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, Some(Kind::Video));
        assert!(report.requirements.re_encode);
        let auto = codes(&report, Severity::Auto);
        for code in [
            "TRYX-M-CODEC",
            "TRYX-M-PIXFMT",
            "TRYX-M-FPS",
            "TRYX-M-BFRAMES",
            "TRYX-M-CONTAINER",
            "TRYX-M-AUDIO",
            "TRYX-M-NAME",
        ] {
            assert!(auto.contains(&code), "{code} missing from {auto:?}");
        }
        assert_eq!(
            codes(&report, Severity::Decide),
            vec!["TRYX-M-HDR", "TRYX-M-ASPECT"]
        );
        assert_eq!(report.name, "My-Movie-1.mp4");
        assert!(report.acceptable(false));
        assert!(!report.acceptable(true));
    }

    #[test]
    fn rotated_phone_video_swaps_its_dimensions() {
        let probe = probe_json(
            r#"{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"3"}"#,
            r#"{"codec_type":"video","codec_name":"h264","profile":"High","level":40,"width":1920,"height":1080,"pix_fmt":"yuv420p","r_frame_rate":"30/1","avg_frame_rate":"30/1","side_data_list":[{"side_data_type":"Display Matrix","rotation":-90}]}"#,
        );
        let report = check(
            Path::new("/x/clip.mp4"),
            10,
            &probe,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(
            (report.source.display_width, report.source.display_height),
            (1080, 1920)
        );
        assert!(codes(&report, Severity::Auto).contains(&"TRYX-M-ROTATION"));
        assert!(codes(&report, Severity::Decide).contains(&"TRYX-M-ASPECT"));
    }

    #[test]
    fn a_target_sized_png_passes_through_and_keeps_its_extension() {
        let probe = probe_json(
            r#"{"format_name":"png_pipe"}"#,
            r#"{"codec_type":"video","codec_name":"png","width":1920,"height":960,"pix_fmt":"rgb24"}"#,
        );
        let report = check(
            Path::new("/x/Poster.PNG"),
            10,
            &probe,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, Some(Kind::Image));
        assert_eq!(report.worst(), Severity::Ok, "{:?}", report.findings);
        assert_eq!(report.name, "Poster.png");
    }

    #[test]
    fn an_animated_gif_is_a_video_and_a_small_alpha_png_is_flattened() {
        let gif = probe_json(
            r#"{"format_name":"gif","duration":"2.4"}"#,
            r#"{"codec_type":"video","codec_name":"gif","width":480,"height":240,"pix_fmt":"bgra","r_frame_rate":"100/1","avg_frame_rate":"25/2","nb_frames":"30"}"#,
        );
        let report = check(
            Path::new("/x/loop.gif"),
            10,
            &gif,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, Some(Kind::AnimatedImage));
        assert_eq!(report.name, "loop.mp4");
        assert!(report.requirements.re_encode);

        let png = probe_json(
            r#"{"format_name":"png_pipe"}"#,
            r#"{"codec_type":"video","codec_name":"png","width":800,"height":400,"pix_fmt":"rgba"}"#,
        );
        let report = check(
            Path::new("/x/logo.png"),
            10,
            &png,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, Some(Kind::Image));
        assert_eq!(codes(&report, Severity::Decide), vec!["TRYX-M-ALPHA"]);
        assert!(codes(&report, Severity::Auto).contains(&"TRYX-M-RESOLUTION"));
    }

    #[test]
    fn empty_or_streamless_files_are_fatal() {
        let audio_only = probe_json(
            r#"{"format_name":"mp3"}"#,
            r#"{"codec_type":"audio","codec_name":"mp3"}"#,
        );
        let report = check(
            Path::new("/x/song.mp3"),
            10,
            &audio_only,
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert_eq!(report.kind, None);
        assert_eq!(codes(&report, Severity::Fatal), vec!["TRYX-M-STREAM"]);
        let report = check(
            Path::new("/x/v.mp4"),
            0,
            &vendor_probe(),
            LEGACY_PANORAMA,
            &Options::default(),
        );
        assert!(codes(&report, Severity::Fatal).contains(&"TRYX-M-EMPTY"));
    }

    #[test]
    fn requested_names_are_accepted_with_or_without_the_extension() {
        for requested in ["tryx-fit-169", "tryx-fit-169.mp4"] {
            let options = Options {
                name: Some(requested.to_string()),
                ..Options::default()
            };
            let report = check(
                Path::new("/x/My Movie.mp4"),
                10,
                &vendor_probe(),
                LEGACY_PANORAMA,
                &options,
            );
            assert_eq!(report.name, "tryx-fit-169.mp4");
            assert!(
                !codes(&report, Severity::Auto).contains(&"TRYX-M-NAME"),
                "{requested}: {:?}",
                report.findings
            );
        }
        let options = Options {
            name: Some("my clip!".to_string()),
            ..Options::default()
        };
        let report = check(
            Path::new("/x/v.mp4"),
            10,
            &vendor_probe(),
            LEGACY_PANORAMA,
            &options,
        );
        assert_eq!(report.name, "my-clip.mp4");
        assert!(codes(&report, Severity::Auto).contains(&"TRYX-M-NAME"));
    }

    #[test]
    fn stems_are_sanitised() {
        assert_eq!(sanitize_stem("My Movie (1)"), "My-Movie-1");
        assert_eq!(sanitize_stem("..hidden"), "hidden");
        assert_eq!(sanitize_stem("ünïcode ok"), "n-code-ok");
        assert_eq!(sanitize_stem("!!!"), "media");
        assert_eq!(
            sanitize_stem("2025-12-09_19-48-54-100"),
            "2025-12-09_19-48-54-100"
        );
    }
}
