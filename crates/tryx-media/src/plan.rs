//! Turning a report into ffmpeg work.

use crate::check::{Kind, Options, Report};
use crate::target::{Format, Target};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// The source is compliant and goes to the display unchanged.
    Passthrough,
    /// The video stream is compliant; only the container or audio change.
    Remux,
    /// Re-encode through the transform filter graph.
    Encode,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub target: Target,
    pub action: Action,
    pub kind: Kind,
    pub input: PathBuf,
    /// File name on the display.
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// ffmpeg arguments between the program and the output path; empty for
    /// a passthrough.
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_bytes: Option<u64>,
    pub description: String,
}

/// What the legacy encoder settings converge on, for size estimates.
const LEGACY_VIDEO_BITRATE: f64 = 13.5e6;
/// The Turris profile's constant bitrate.
const TURRIS_VIDEO_BITRATE: f64 = 12e6;
const TURRIS_X264_PARAMS: &str = "aud=1:repeat-headers=1:open-gop=0:force-cfr=1:fullrange=on:colorprim=bt709:transfer=bt709:colormatrix=bt709";

/// BT.2020 PQ/HLG to BT.709 through zimg, prepended to the transform graph.
const TONEMAP_PREFIX: &str = "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=tonemap=hable:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p,";

impl Plan {
    /// `None` when the report found no usable stream.
    pub fn from_report(report: &Report, options: &Options) -> Option<Plan> {
        let kind = report.kind?;
        let target = report.target;
        let input = report.path.clone();
        let name = report.name.clone();
        let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let loop_image = kind == Kind::Image && target.format != Format::Mp4;
        if loop_image {
            args.extend(
                ["-loop", "1", "-framerate", &target.fps.to_string()]
                    .iter()
                    .map(|s| s.to_string()),
            );
            if target.format == Format::RawH264 {
                args.extend(["-t", "60"].iter().map(|s| s.to_string()));
            }
        }
        args.push("-i".to_string());
        args.push(input.to_string_lossy().into_owned());

        let plan = match kind {
            Kind::Image if !report.requirements.re_encode => Plan {
                target,
                action: Action::Passthrough,
                kind,
                input,
                name,
                filter: None,
                args: Vec::new(),
                estimated_bytes: Some(report.source.size),
                description: "send the image as is".to_string(),
            },
            Kind::Image if target.format == Format::Mp4 => {
                let filter = options.transform.image_filter(target.width, target.height);
                args.extend(
                    [
                        "-map",
                        "0:v:0",
                        "-frames:v",
                        "1",
                        "-vf",
                        &filter,
                        "-f",
                        "image2",
                        "-c:v",
                        "png",
                    ]
                    .iter()
                    .map(|s| s.to_string()),
                );
                Plan {
                    target,
                    action: Action::Encode,
                    kind,
                    input,
                    name,
                    filter: Some(filter),
                    args,
                    estimated_bytes: None,
                    description: format!(
                        "convert to a {}×{} PNG ({})",
                        target.width, target.height, options.transform.mode
                    ),
                }
            }
            Kind::Image | Kind::Video | Kind::AnimatedImage if report.requirements.re_encode => {
                let mut filter =
                    options
                        .transform
                        .video_filter(target.width, target.height, target.fps);
                if report.requirements.tonemap {
                    filter = format!("{TONEMAP_PREFIX}{filter}");
                }
                if target.format == Format::Mxhd {
                    filter.push_str(",scale=in_range=auto:out_range=full:out_color_matrix=bt709");
                }
                args.extend(
                    ["-map", "0:v:0", "-an", "-vf", &filter, "-c:v", "libx264"]
                        .iter()
                        .map(|s| s.to_string()),
                );
                let fps = target.fps.to_string();
                let (estimated_bytes, description) = match target.format {
                    Format::Mp4 => {
                        args.extend(
                            [
                                "-profile:v",
                                "high",
                                "-level:v",
                                "4.1",
                                "-pix_fmt",
                                "yuv420p",
                                "-r",
                                &fps,
                                "-fps_mode",
                                "cfr",
                                "-g",
                                "12",
                                "-keyint_min",
                                "12",
                                "-bf",
                                "0",
                                "-sc_threshold",
                                "0",
                                "-crf",
                                "18",
                                "-maxrate",
                                "14M",
                                "-bufsize",
                                "28M",
                                "-movflags",
                                "+faststart",
                                "-f",
                                "mp4",
                            ]
                            .iter()
                            .map(|s| s.to_string()),
                        );
                        (
                            report
                                .source
                                .duration
                                .map(|seconds| (seconds * LEGACY_VIDEO_BITRATE / 8.0) as u64),
                            format!(
                                "re-encode to {}×{} H.264 MP4 at {} fps ({})",
                                target.width, target.height, target.fps, options.transform.mode
                            ),
                        )
                    }
                    Format::RawH264 => {
                        args.extend(
                            [
                                "-preset",
                                "veryfast",
                                "-crf",
                                "23",
                                "-pix_fmt",
                                "yuv420p",
                                "-r",
                                &fps,
                                "-fps_mode",
                                "cfr",
                                "-f",
                                "h264",
                            ]
                            .iter()
                            .map(|s| s.to_string()),
                        );
                        (
                            None,
                            if loop_image {
                                format!(
                                    "encode a 60 s {}×{} H.264 loop of the image ({})",
                                    target.width, target.height, options.transform.mode
                                )
                            } else {
                                format!(
                                    "re-encode to a raw {}×{} H.264 stream at {} fps ({})",
                                    target.width, target.height, target.fps, options.transform.mode
                                )
                            },
                        )
                    }
                    Format::Mxhd => {
                        if loop_image {
                            args.extend(
                                ["-preset", "medium", "-crf", "18"]
                                    .iter()
                                    .map(|s| s.to_string()),
                            );
                        } else {
                            args.extend(
                                [
                                    "-preset", "fast", "-b:v", "12M", "-maxrate", "12M",
                                    "-bufsize", "24M",
                                ]
                                .iter()
                                .map(|s| s.to_string()),
                            );
                        }
                        let (level, gop) = if loop_image {
                            ("4.0", "30")
                        } else {
                            ("4.1", "60")
                        };
                        args.extend(
                            [
                                "-pix_fmt",
                                "yuv420p",
                                "-r",
                                &fps,
                                "-fps_mode",
                                "cfr",
                                "-profile:v",
                                "main",
                                "-level:v",
                                level,
                                "-g",
                                gop,
                                "-keyint_min",
                                gop,
                                "-sc_threshold",
                                "0",
                                "-bf",
                                "0",
                                "-flags",
                                "+cgop",
                                "-color_range",
                                "pc",
                                "-colorspace",
                                "bt709",
                                "-color_primaries",
                                "bt709",
                                "-color_trc",
                                "bt709",
                                "-x264-params",
                                TURRIS_X264_PARAMS,
                            ]
                            .iter()
                            .map(|s| s.to_string()),
                        );
                        if loop_image {
                            args.extend(["-frames:v", "1"].iter().map(|s| s.to_string()));
                        }
                        args.extend(["-f", "h264"].iter().map(|s| s.to_string()));
                        (
                            if loop_image {
                                None
                            } else {
                                report
                                    .source
                                    .duration
                                    .map(|seconds| (seconds * TURRIS_VIDEO_BITRATE / 8.0) as u64)
                            },
                            if loop_image {
                                format!(
                                    "encode a single {}×{} H.264 frame for the Turris header ({})",
                                    target.width, target.height, options.transform.mode
                                )
                            } else {
                                format!(
                                    "re-encode to {}×{} H.264 Main 4.1 at 12 Mbps for the Turris header ({})",
                                    target.width, target.height, options.transform.mode
                                )
                            },
                        )
                    }
                };
                Plan {
                    target,
                    action: Action::Encode,
                    kind,
                    input,
                    name,
                    filter: Some(filter),
                    args,
                    estimated_bytes,
                    description,
                }
            }
            Kind::Video | Kind::AnimatedImage if report.requirements.remux => {
                args.extend(
                    [
                        "-map",
                        "0:v:0",
                        "-an",
                        "-c:v",
                        "copy",
                        "-movflags",
                        "+faststart",
                        "-f",
                        "mp4",
                    ]
                    .iter()
                    .map(|s| s.to_string()),
                );
                Plan {
                    target,
                    action: Action::Remux,
                    kind,
                    input,
                    name,
                    filter: None,
                    args,
                    estimated_bytes: Some(report.source.size),
                    description: "rewrap the video stream as MP4 without audio".to_string(),
                }
            }
            Kind::Image | Kind::Video | Kind::AnimatedImage => Plan {
                target,
                action: Action::Passthrough,
                kind,
                input,
                name,
                filter: None,
                args: Vec::new(),
                estimated_bytes: Some(report.source.size),
                description: "send the video as is".to_string(),
            },
        };
        Some(plan)
    }

    /// The full ffmpeg argument list writing to `output`.
    pub fn ffmpeg_args(&self, output: &Path) -> Vec<String> {
        let mut args = self.args.clone();
        args.push(output.to_string_lossy().into_owned());
        args
    }

    /// A copy-pasteable rendering of the ffmpeg command line.
    pub fn command_line(&self, output: &Path) -> String {
        std::iter::once("ffmpeg".to_string())
            .chain(self.ffmpeg_args(output))
            .map(|arg| {
                if arg.is_empty()
                    || arg
                        .chars()
                        .any(|c| c.is_whitespace() || "'\"$`\\()[]{};&|<>*?!".contains(c))
                {
                    format!("'{}'", arg.replace('\'', "'\\''"))
                } else {
                    arg
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{Kind, Options, Requirements, Source};
    use crate::target::{KANALI_PANORAMA, KANALI_TURRIS, LEGACY_PANORAMA, Target};
    use crate::transform::{Mode, Transform};

    fn report(kind: Kind, requirements: Requirements, duration: Option<f64>) -> Report {
        report_for(LEGACY_PANORAMA, kind, requirements, duration)
    }

    fn report_for(
        target: Target,
        kind: Kind,
        requirements: Requirements,
        duration: Option<f64>,
    ) -> Report {
        Report {
            path: PathBuf::from("/in/clip one.mkv"),
            target,
            kind: Some(kind),
            source: Source {
                size: 1000,
                duration,
                ..Source::default()
            },
            findings: Vec::new(),
            requirements,
            name: "clip-one.mp4".to_string(),
        }
    }

    #[test]
    fn re_encode_uses_the_measured_vendor_settings() {
        let requirements = Requirements {
            re_encode: true,
            ..Requirements::default()
        };
        let plan = Plan::from_report(
            &report(Kind::Video, requirements, Some(60.0)),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(plan.action, Action::Encode);
        let args = plan.ffmpeg_args(Path::new("/out/clip-one.mp4"));
        let text = args.join(" ");
        for expected in [
            "-c:v libx264",
            "-profile:v high",
            "-level:v 4.1",
            "-pix_fmt yuv420p",
            "-r 30 -fps_mode cfr",
            "-g 12 -keyint_min 12 -bf 0 -sc_threshold 0",
            "-crf 18 -maxrate 14M -bufsize 28M",
            "-movflags +faststart -f mp4 /out/clip-one.mp4",
            "-map 0:v:0 -an -vf",
        ] {
            assert!(text.contains(expected), "{expected} missing in {text}");
        }
        assert!(
            plan.filter
                .as_deref()
                .unwrap()
                .ends_with("setsar=1,format=yuv420p,fps=30")
        );
        assert_eq!(plan.estimated_bytes, Some((60.0 * 13.5e6 / 8.0) as u64));
        assert!(
            plan.command_line(Path::new("/out/x.mp4"))
                .contains("'/in/clip one.mkv'")
        );
    }

    #[test]
    fn tonemap_prefixes_the_filter_graph() {
        let requirements = Requirements {
            re_encode: true,
            tonemap: true,
            remux: false,
        };
        let plan = Plan::from_report(
            &report(Kind::Video, requirements, None),
            &Options::default(),
        )
        .unwrap();
        assert!(plan.filter.unwrap().starts_with("zscale=t=linear"));
        assert_eq!(plan.estimated_bytes, None);
    }

    #[test]
    fn remux_copies_the_stream_and_passthrough_has_no_args() {
        let remux = Plan::from_report(
            &report(
                Kind::Video,
                Requirements {
                    remux: true,
                    ..Requirements::default()
                },
                None,
            ),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(remux.action, Action::Remux);
        assert!(remux.args.join(" ").contains("-c:v copy"));
        let pass = Plan::from_report(
            &report(Kind::Video, Requirements::default(), None),
            &Options::default(),
        )
        .unwrap();
        assert_eq!(pass.action, Action::Passthrough);
        assert!(pass.args.is_empty());
        assert_eq!(pass.estimated_bytes, Some(1000));
    }

    #[test]
    fn images_become_a_single_png_frame() {
        let options = Options {
            transform: Transform {
                mode: Mode::Fill,
                ..Transform::default()
            },
            transform_explicit: true,
            ..Options::default()
        };
        let plan = Plan::from_report(
            &report(
                Kind::Image,
                Requirements {
                    re_encode: true,
                    ..Requirements::default()
                },
                None,
            ),
            &options,
        )
        .unwrap();
        assert_eq!(plan.action, Action::Encode);
        let text = plan.args.join(" ");
        assert!(text.contains("-frames:v 1"));
        assert!(text.contains("-c:v png"));
        assert!(plan.filter.unwrap().ends_with("setsar=1,format=rgb24"));
        assert!(plan.description.contains("Fill"));
    }

    #[test]
    fn kanali_panorama_encodes_raw_h264_and_loops_images() {
        let re_encode = Requirements {
            re_encode: true,
            ..Requirements::default()
        };
        let video = Plan::from_report(
            &report_for(KANALI_PANORAMA, Kind::Video, re_encode, Some(10.0)),
            &Options::default(),
        )
        .unwrap();
        let text = video.args.join(" ");
        assert!(
            text.starts_with("-hide_banner -nostdin -y -i /in/clip one.mkv -map 0:v:0 -an -vf "),
            "{text}"
        );
        assert!(
            text.ends_with(
                "-c:v libx264 -preset veryfast -crf 23 -pix_fmt yuv420p -r 30 -fps_mode cfr -f h264"
            ),
            "{text}"
        );
        assert!(
            !text.contains("-profile:v"),
            "PASE leaves the profile to x264"
        );
        assert_eq!(video.estimated_bytes, None);
        let image = Plan::from_report(
            &report_for(KANALI_PANORAMA, Kind::Image, re_encode, None),
            &Options::default(),
        )
        .unwrap();
        let text = image.args.join(" ");
        assert!(
            text.starts_with("-hide_banner -nostdin -y -loop 1 -framerate 30 -t 60 -i "),
            "{text}"
        );
        assert!(
            image.filter.as_deref().unwrap().ends_with("fps=30"),
            "images go through the video graph"
        );
        assert!(text.ends_with("-f h264"));
        assert!(!text.contains("-frames:v"));
    }

    #[test]
    fn turris_uses_the_strict_main_profile_and_single_frame_images() {
        let re_encode = Requirements {
            re_encode: true,
            ..Requirements::default()
        };
        let video = Plan::from_report(
            &report_for(KANALI_TURRIS, Kind::Video, re_encode, Some(10.0)),
            &Options::default(),
        )
        .unwrap();
        let text = video.args.join(" ");
        for expected in [
            "-preset fast -b:v 12M -maxrate 12M -bufsize 24M",
            "-profile:v main -level:v 4.1 -g 60 -keyint_min 60 -sc_threshold 0 -bf 0 -flags +cgop",
            "-color_range pc -colorspace bt709 -color_primaries bt709 -color_trc bt709 -x264-params aud=1:repeat-headers=1:open-gop=0:force-cfr=1:fullrange=on:colorprim=bt709:transfer=bt709:colormatrix=bt709 -f h264",
        ] {
            assert!(text.contains(expected), "{expected} missing in {text}");
        }
        assert!(
            video
                .filter
                .unwrap()
                .ends_with(",scale=in_range=auto:out_range=full:out_color_matrix=bt709")
        );
        assert_eq!(video.estimated_bytes, Some((10.0 * 12e6 / 8.0) as u64));
        let image = Plan::from_report(
            &report_for(KANALI_TURRIS, Kind::Image, re_encode, None),
            &Options::default(),
        )
        .unwrap();
        let text = image.args.join(" ");
        assert!(text.contains("-loop 1 -framerate 30 -i"), "{text}");
        assert!(
            !text.contains("-t 60"),
            "Turris images are a single frame, not a loop"
        );
        assert!(text.contains("-preset medium -crf 18"));
        assert!(text.contains("-level:v 4.0 -g 30 -keyint_min 30"));
        assert!(text.ends_with("-frames:v 1 -f h264"), "{text}");
    }
}
