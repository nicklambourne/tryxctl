use crate::exit::{self, CommandResult, Failure};
use crate::legacy::{self, Backend, Target as DeviceTarget};
use crate::ops::{self, Outcome, Pending};
use crate::output;
use owo_colors::{OwoColorize, Stream};
use serde_json::json;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use tryx_legacy::adb::{self, Adb, is_safe_media_name};
use tryx_media::check::{self, Finding, Kind, Options, Report, Severity, Source, Trim};
use tryx_media::plan::Action;
use tryx_media::target::{Format, KANALI_PANORAMA, KANALI_TURRIS, LEGACY_PANORAMA, Target};
use tryx_media::transform::{Mode, Transform};
use tryx_media::{Plan, Probe, encode, mxhd, preview};

/// Room to leave on the display's storage after an upload.
const FREE_SPACE_MARGIN: u64 = 16 * 1024 * 1024;

#[derive(clap::Args, Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TransformArgs {
    /// How to map the source onto the display: fit, fill, crop, or stretch.
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,
    /// Rotate clockwise by 90, 180, or 270 degrees.
    #[arg(long, value_name = "DEGREES", default_value_t = 0)]
    pub rotate: u32,
    /// Crop zoom in percent, 100 to 400 (crop mode only).
    #[arg(long, value_name = "PERCENT")]
    pub zoom: Option<u32>,
    /// Crop focus as X,Y percentages; 50,50 is the centre (crop mode only).
    #[arg(long, value_name = "X,Y")]
    pub focus: Option<String>,
    /// Background colour for fit mode as #RRGGBB.
    #[arg(long, value_name = "#RRGGBB")]
    pub bg: Option<String>,
    /// Refuse HDR sources instead of tone-mapping them.
    #[arg(long)]
    pub no_tonemap: bool,
    /// Display to prepare for when none is connected: legacy-panorama,
    /// kanali-panorama, or kanali-turris (upload detects it).
    #[arg(long, value_name = "DISPLAY")]
    pub target: Option<String>,
    /// Keep only part of a video, in seconds: A-B, A-, or -B.
    #[arg(long, value_name = "A-B")]
    pub trim: Option<String>,
}

impl TransformArgs {
    pub fn target(&self) -> Result<Target, Failure> {
        match self.target.as_deref() {
            None => Ok(LEGACY_PANORAMA),
            Some(id) => [LEGACY_PANORAMA, KANALI_PANORAMA, KANALI_TURRIS]
                .into_iter()
                .find(|target| target.id == id)
                .ok_or_else(|| {
                    Failure::usage(format!(
                        "unknown target {id:?}; use legacy-panorama, kanali-panorama, or kanali-turris"
                    ))
                }),
        }
    }

    pub fn options(&self, name: Option<String>) -> Result<Options, Failure> {
        let mut transform = Transform::default();
        if let Some(mode) = &self.mode {
            transform.mode = Mode::parse(mode).ok_or_else(|| {
                Failure::usage(format!(
                    "unknown mode {mode:?}; use fit, fill, crop, or stretch"
                ))
            })?;
        }
        transform.rotation_quarter_turns = match self.rotate {
            0 => 0,
            90 => 1,
            180 => 2,
            270 => 3,
            other => {
                return Err(Failure::usage(format!(
                    "--rotate {other} is not 0, 90, 180, or 270"
                )));
            }
        };
        if let Some(zoom) = self.zoom {
            transform.zoom_permille = zoom * 10;
        }
        if let Some(focus) = &self.focus {
            let (x, y) = focus
                .split_once(',')
                .and_then(|(x, y)| {
                    Some((x.trim().parse::<u32>().ok()?, y.trim().parse::<u32>().ok()?))
                })
                .ok_or_else(|| {
                    Failure::usage(format!("--focus {focus:?} is not X,Y percentages"))
                })?;
            transform.focus_x = x * 100;
            transform.focus_y = y * 100;
        }
        if let Some(bg) = &self.bg {
            let hex = bg.strip_prefix('#').unwrap_or(bg);
            transform.background_rgb = (hex.len() == 6)
                .then(|| u32::from_str_radix(hex, 16).ok())
                .flatten()
                .ok_or_else(|| Failure::usage(format!("--bg {bg:?} is not #RRGGBB")))?;
        }
        transform
            .validate()
            .map_err(|error| Failure::usage(error.to_string()))?;
        let trim = match &self.trim {
            Some(text) => Some(Trim::parse(text).ok_or_else(|| {
                Failure::usage(format!("--trim {text:?} is not A-B, A-, or -B in seconds"))
            })?),
            None => None,
        };
        Ok(Options {
            transform,
            transform_explicit: self.mode.is_some(),
            tonemap: !self.no_tonemap,
            name,
            trim,
        })
    }
}

pub(crate) struct Analysis {
    pub(crate) report: Report,
    pub(crate) plan: Option<Plan>,
}

pub(crate) fn analyse(ffprobe: &Path, path: &Path, options: &Options, target: Target) -> Analysis {
    let unreadable = |message: String| Analysis {
        report: Report {
            path: path.to_path_buf(),
            target,
            kind: None,
            source: Source::default(),
            findings: vec![Finding {
                code: "TRYX-M-UNREADABLE",
                severity: Severity::Fatal,
                message,
                action: None,
            }],
            requirements: Default::default(),
            name: String::new(),
        },
        plan: None,
    };
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => metadata,
        Ok(_) => return unreadable("not a regular file".to_string()),
        Err(error) => return unreadable(error.to_string()),
    };
    let probe = match Probe::read(ffprobe, path) {
        Ok(probe) => probe,
        Err(error) => return unreadable(error.to_string()),
    };
    let report = check::check(path, metadata.len(), &probe, target, options);
    let plan = Plan::from_report(&report, options);
    Analysis { report, plan }
}

fn badge(severity: Severity) -> String {
    match severity {
        Severity::Ok => format!(
            "[{}]",
            " OK ".if_supports_color(Stream::Stdout, |t| t.green())
        ),
        Severity::Auto => format!(
            "[{}]",
            "FIX ".if_supports_color(Stream::Stdout, |t| t.cyan())
        ),
        Severity::Decide => format!(
            "[{}]",
            "ASK ".if_supports_color(Stream::Stdout, |t| t.yellow())
        ),
        Severity::Fatal => format!(
            "[{}]",
            "FAIL".if_supports_color(Stream::Stdout, |t| t.red())
        ),
    }
}

fn summary_line(report: &Report) -> String {
    let source = &report.source;
    let kind = match report.kind {
        Some(check::Kind::Image) => "image",
        Some(check::Kind::AnimatedImage) => "animated image",
        Some(check::Kind::Video) => "video",
        None => "unknown",
    };
    let mut parts = vec![kind.to_string()];
    let codec = match &source.profile {
        Some(profile) => format!("{} {profile}", source.codec),
        None => source.codec.clone(),
    };
    parts.push(codec);
    if let Some(pix_fmt) = &source.pix_fmt {
        parts.push(pix_fmt.clone());
    }
    parts.push(format!(
        "{}×{}",
        source.display_width, source.display_height
    ));
    if let Some(fps) = source
        .frame_rate
        .filter(|_| report.kind != Some(check::Kind::Image))
    {
        parts.push(format!("{fps:.3} fps"));
    }
    if let Some(seconds) = source
        .duration
        .filter(|_| report.kind != Some(check::Kind::Image))
    {
        parts.push(format!("{seconds:.1} s"));
    }
    parts.push(output::human_bytes(source.size));
    let containers: Vec<&str> = source.container.split(',').collect();
    parts.push(
        containers
            .iter()
            .find(|name| **name == "mp4")
            .or(containers.first())
            .unwrap_or(&"")
            .to_string(),
    );
    parts.join(" · ")
}

/// The findings and the plan as plain lines, for the interface.
pub(crate) fn finding_lines(analysis: &Analysis) -> Vec<String> {
    let mut lines: Vec<String> = analysis
        .report
        .findings
        .iter()
        .map(|finding| {
            let badge = match finding.severity {
                Severity::Ok => " ok ",
                Severity::Auto => "fix ",
                Severity::Decide => "ask ",
                Severity::Fatal => "FAIL",
            };
            match &finding.action {
                Some(action) => format!("[{badge}] {} → {action}", finding.message),
                None => format!("[{badge}] {}", finding.message),
            }
        })
        .collect();
    if let Some(plan) = &analysis.plan {
        lines.push(format!("plan: {} → {}", plan.description, plan.name));
    }
    lines
}

fn print_report(analysis: &Analysis) {
    let report = &analysis.report;
    println!(
        "{}",
        report
            .path
            .display()
            .if_supports_color(Stream::Stdout, |t| t.bold())
    );
    if report.kind.is_some() {
        println!("  {}", output::dim(&summary_line(report)));
    }
    for finding in &report.findings {
        let action = finding
            .action
            .as_deref()
            .map(|action| format!(" → {action}"))
            .unwrap_or_default();
        println!(
            "  {} {:<18} {}{action}",
            badge(finding.severity),
            finding.code,
            finding.message
        );
    }
    if let Some(plan) = &analysis.plan {
        let size = plan
            .estimated_bytes
            .map(|bytes| match plan.action {
                Action::Encode => format!(" (about {})", output::human_bytes(bytes)),
                _ => format!(" ({})", output::human_bytes(bytes)),
            })
            .unwrap_or_default();
        println!("  plan: {} → {}{size}", plan.description, plan.name);
    }
}

fn analysis_json(analysis: &Analysis) -> serde_json::Value {
    json!({"report": analysis.report, "plan": analysis.plan})
}

pub fn check(
    json: bool,
    files: &[PathBuf],
    strict: bool,
    transform: &TransformArgs,
    name: Option<String>,
) -> CommandResult {
    let (_, ffprobe) = encode::tools()?;
    let options = transform.options(name)?;
    let target = transform.target()?;
    let analyses: Vec<Analysis> = files
        .iter()
        .map(|file| analyse(&ffprobe, file, &options, target))
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&analyses.iter().map(analysis_json).collect::<Vec<_>>())?
        );
    } else {
        for (index, analysis) in analyses.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_report(analysis);
        }
    }
    let rejected = analyses
        .iter()
        .filter(|a| !a.report.acceptable(strict))
        .count();
    if rejected > 0 {
        return Err(Failure::media(format!(
            "{rejected} of {} file(s) cannot be prepared for the display{}",
            analyses.len(),
            if strict { " under --strict" } else { "" }
        )));
    }
    Ok(exit::ok())
}

fn run_plan(
    ffmpeg: &Path,
    ffprobe: &Path,
    plan: &Plan,
    output: &Path,
    duration: Option<f64>,
    quiet: bool,
) -> Result<(), Failure> {
    let quiet = quiet || output::quiet();
    let label = match plan.action {
        Action::Encode => "encoding",
        Action::Remux => "rewrapping",
        Action::Passthrough => "copying",
    };
    encode::run(ffmpeg, plan, output, duration, |progress| {
        if quiet || (progress.fraction.is_none() && progress.seconds == 0.0 && progress.bytes == 0)
        {
            return;
        }
        match progress.fraction {
            Some(fraction) => eprint!(
                "\r  {label} {:>3}%  {}   ",
                (fraction * 100.0) as u32,
                output::human_bytes(progress.bytes)
            ),
            None => eprint!(
                "\r  {label} {:.1} s  {}   ",
                progress.seconds,
                output::human_bytes(progress.bytes)
            ),
        }
    })?;
    if !quiet && plan.action != Action::Passthrough {
        eprintln!();
    }
    finish_stage(ffprobe, plan, output)
}

/// Verifies a finished encode and wraps Turris media in its header.
pub fn finish_stage(ffprobe: &Path, plan: &Plan, output: &Path) -> Result<(), Failure> {
    encode::verify(ffprobe, plan, output, plan.target)?;
    if plan.target.format == Format::Mxhd && plan.action != Action::Passthrough {
        let raw = output.with_extension("raw.h264");
        std::fs::rename(output, &raw)?;
        let kind = if plan.kind == Kind::Image {
            mxhd::MediaKind::Image
        } else {
            mxhd::MediaKind::Video
        };
        let wrapped = mxhd::wrap(&raw, output, kind);
        let _ = std::fs::remove_file(&raw);
        wrapped?;
    }
    Ok(())
}

pub fn convert(
    json: bool,
    file: &Path,
    output: Option<PathBuf>,
    dry_run: bool,
    transform: &TransformArgs,
    name: Option<String>,
) -> CommandResult {
    let (ffmpeg, ffprobe) = encode::tools()?;
    let options = transform.options(name)?;
    let analysis = analyse(&ffprobe, file, &options, transform.target()?);
    if !analysis.report.acceptable(false) {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&analysis_json(&analysis))?
            );
        } else {
            print_report(&analysis);
        }
        return Err(Failure::media(
            "the file cannot be prepared for the display",
        ));
    }
    let plan = analysis
        .plan
        .as_ref()
        .expect("an acceptable report has a plan");
    let output = output.unwrap_or_else(|| PathBuf::from(&plan.name));
    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "report": analysis.report,
                    "plan": plan,
                    "output": output,
                    "command": plan.command_line(&output),
                }))?
            );
        } else {
            print_report(&analysis);
            println!("  output: {}", output.display());
            if plan.action != Action::Passthrough {
                println!("  command: {}", plan.command_line(&output));
            }
        }
        return Ok(exit::ok());
    }
    if !json {
        print_report(&analysis);
    }
    run_plan(&ffmpeg, &ffprobe, plan, &output, plan.duration, json)?;
    let size = std::fs::metadata(&output)?.len();
    let sha256 = encode::sha256_file(&output)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "output": output,
                "size": size,
                "sha256": sha256,
                "action": plan.action,
                "name": plan.name,
            }))?
        );
    } else {
        println!(
            "  wrote {} ({}, sha256 {}…)",
            output.display(),
            output::human_bytes(size),
            &sha256[..12]
        );
    }
    Ok(exit::ok())
}

pub fn connect_adb(target: &DeviceTarget) -> Result<(Adb, String), Failure> {
    // Without a discovered device there is no serial or USB port to find the
    // right adb transport by, and guessing could reach another Android device.
    if target.device.is_none() {
        return Err(Failure::device(format!(
            "no TRYX display found at {}, so its files cannot be reached over adb; `tryxctl devices` lists the connected displays",
            target.tty
        )));
    }
    let adb = Adb::new()?;
    let devices = adb.devices()?;
    let selected = adb::select(&devices, target.usb_serial(), target.sysfs_name()).ok_or_else(|| {
        Failure::device(if devices.is_empty() {
            "adb sees no devices; install packaging/udev/71-tryx-legacy.rules and replug the display"
        } else {
            "adb sees devices, but none matches the display's serial or USB port"
        })
    })?;
    if selected.state != "device" {
        let hint = if selected.state.starts_with("no") {
            "; the adb server probably started before the udev rule was installed, run `adb kill-server` and retry"
        } else {
            ""
        };
        return Err(Failure::device(format!(
            "adb reports the display ({}) as {}{hint}",
            selected.serial, selected.state
        )));
    }
    let serial = selected.serial.clone();
    Ok((adb.with_serial(serial.clone()), serial))
}

pub fn ls(json: bool, session: &legacy::Session) -> CommandResult {
    let target = match session.select_backend()? {
        Backend::Legacy(target) => target,
        Backend::Kanali { .. } => return ls_kanali(json, session),
    };
    let (adb, serial) = connect_adb(&target)?;
    let files = adb.list_media()?;
    let storage = adb.free_space()?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "adb_serial": serial,
                "directory": adb::MEDIA_DIR,
                "files": files,
                "storage": storage,
                "presets": tryx_legacy::commands::PRESETS,
            }))?
        );
        return Ok(exit::ok());
    }
    if files.is_empty() {
        println!("No user media on the display.");
    } else {
        let rows: Vec<Vec<String>> = files
            .iter()
            .map(|file| vec![file.name.clone(), output::human_bytes(file.size)])
            .collect();
        print!("{}", output::table(&["NAME", "SIZE"], &rows));
    }
    let used: u64 = files.iter().map(|file| file.size).sum();
    println!(
        "{} file(s), {} in {}; {} free of {} on the display",
        files.len(),
        output::human_bytes(used),
        adb::MEDIA_DIR,
        output::human_bytes(storage.available_kib * 1024),
        output::human_bytes(storage.total_kib * 1024),
    );
    println!(
        "Built-in animations: {}",
        tryx_legacy::commands::PRESETS
            .iter()
            .enumerate()
            .map(|(index, name)| format!(
                "preset:{} {}",
                index + 1,
                name.split(": ").nth(1).unwrap_or(name)
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(exit::ok())
}

fn ls_kanali(json: bool, session: &legacy::Session) -> CommandResult {
    let mut connection = session.connect()?;
    let catalog = connection.catalog()?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "via": connection.via(),
                "files": catalog.user,
                "presets": catalog.presets,
            }))?
        );
        return Ok(exit::ok());
    }
    if catalog.user.is_empty() {
        println!("No user media on the display.");
    } else {
        let rows: Vec<Vec<String>> = catalog
            .user
            .iter()
            .map(|file| vec![file.name.clone(), output::human_bytes(u64::from(file.size))])
            .collect();
        print!("{}", output::table(&["NAME", "SIZE"], &rows));
    }
    let used: u64 = catalog.user.iter().map(|file| u64::from(file.size)).sum();
    println!(
        "{} user file(s), {}; {} factory preset(s): {}",
        catalog.user.len(),
        output::human_bytes(used),
        catalog.presets.len(),
        if catalog.presets.is_empty() {
            "none".to_string()
        } else {
            catalog
                .presets
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    Ok(exit::ok())
}

/// A staged file on its way to the display, journalled, and kept for a
/// retry when the transfer fails after the encode.
struct Transfer {
    record_id: String,
    key: Option<String>,
    staged: PathBuf,
    /// The staged file is ours to delete (not the user's input).
    owned: bool,
}

impl Transfer {
    /// Runs the plan, or picks up the encode a failed attempt left behind.
    fn stage(
        ffmpeg: &Path,
        ffprobe: &Path,
        plan: &Plan,
        json: bool,
        pending: &Pending,
        remote: &str,
    ) -> Result<Transfer, Failure> {
        let record = ops::begin(pending, remote, plan.target.id);
        if plan.action == Action::Passthrough {
            return Ok(Transfer {
                record_id: record.id,
                key: None,
                staged: plan.input.clone(),
                owned: false,
            });
        }
        let key = ops::cache_key(&plan.input, &pending.transform, plan.target.id, remote)?;
        if let Some(cached) = ops::cached(&key) {
            if !json && !output::quiet() {
                eprintln!("  reusing the encode kept from a previous attempt");
            }
            return Ok(Transfer {
                record_id: record.id,
                key: Some(key),
                staged: cached,
                owned: true,
            });
        }
        let staged = std::env::temp_dir().join(format!(
            "tryxctl-{}-{}-{}",
            pending.kind,
            std::process::id(),
            remote
        ));
        if let Err(failure) = run_plan(ffmpeg, ffprobe, plan, &staged, plan.duration, json) {
            let _ = std::fs::remove_file(&staged);
            ops::finish(
                &record.id,
                Outcome::Failed,
                Some(failure.message.clone()),
                None,
                None,
                None,
            );
            return Err(failure);
        }
        Ok(Transfer {
            record_id: record.id,
            key: Some(key),
            staged,
            owned: true,
        })
    }

    /// Records the outcome; keeps the encode when the transfer failed.
    fn done(self, result: Result<(u64, String), Failure>) -> Result<(u64, String), Failure> {
        match &result {
            Ok((size, sha256)) => {
                if self.owned {
                    let _ = std::fs::remove_file(&self.staged);
                }
                if let Some(key) = &self.key {
                    ops::discard(key);
                }
                ops::finish(
                    &self.record_id,
                    Outcome::Ok,
                    None,
                    None,
                    Some(*size),
                    Some(sha256.clone()),
                );
            }
            Err(failure) => {
                let cached = match (&self.key, self.owned) {
                    (Some(key), true) => ops::keep(key, &self.staged).ok(),
                    _ => None,
                };
                if cached.is_some() && !output::quiet() {
                    eprintln!(
                        "  the encode is kept; `tryxctl op retry {}` sends it without re-encoding",
                        self.record_id
                    );
                }
                ops::finish(
                    &self.record_id,
                    Outcome::Failed,
                    Some(failure.message.clone()),
                    cached,
                    None,
                    None,
                );
            }
        }
        result
    }
}

#[allow(clippy::too_many_arguments)]
pub fn upload(
    json: bool,
    session: &legacy::Session,
    file: &Path,
    name: Option<String>,
    show: bool,
    replace: bool,
    dry_run: bool,
    strict: bool,
    transform: &TransformArgs,
) -> CommandResult {
    let (ffmpeg, ffprobe) = encode::tools()?;
    let options = transform.options(name.clone())?;
    let pending = Pending {
        kind: "upload",
        source: file.to_path_buf(),
        name,
        transform: transform.clone(),
        show,
        replace,
    };
    // The connected display decides the target; --target is for offline use.
    let backend = session.select_backend()?;
    let target = match &backend {
        Backend::Legacy(_) => LEGACY_PANORAMA,
        Backend::Kanali { product, .. } => crate::kanali::media_target(*product),
    };
    let analysis = analyse(&ffprobe, file, &options, target);
    if !analysis.report.acceptable(strict) {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&analysis_json(&analysis))?
            );
        } else {
            print_report(&analysis);
        }
        return Err(Failure::media(format!(
            "the file cannot be prepared for the display{}",
            if strict { " under --strict" } else { "" }
        )));
    }
    let plan = analysis
        .plan
        .as_ref()
        .expect("an acceptable report has a plan");
    if dry_run {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&analysis_json(&analysis))?
            );
        } else {
            print_report(&analysis);
            if plan.action != Action::Passthrough {
                println!("  command: {}", plan.command_line(Path::new(&plan.name)));
            }
        }
        return Ok(exit::ok());
    }

    let target = match backend {
        Backend::Legacy(target) => target,
        Backend::Kanali { .. } => {
            return upload_kanali(
                json, session, &analysis, &ffmpeg, &ffprobe, show, replace, &pending,
            );
        }
    };
    // Fail on device problems before spending time on ffmpeg.
    let (adb, _) = connect_adb(&target)?;
    let existing = adb.list_media()?;
    if existing.iter().any(|entry| entry.name == plan.name) && !replace {
        return Err(Failure::media(format!(
            "{} already exists on the display; pass --replace to overwrite it or --name to rename",
            plan.name
        )));
    }

    if !json {
        print_report(&analysis);
    }
    let transfer = Transfer::stage(&ffmpeg, &ffprobe, plan, json, &pending, &plan.name)?;
    let result = push_and_show(session, &target, &adb, plan, &transfer.staged, show);
    let (size, sha256) = transfer.done(result)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": plan.name,
                "size": size,
                "sha256": sha256,
                "action": plan.action,
                "shown": show,
            }))?
        );
    } else {
        println!(
            "  uploaded {} ({}, sha256 {}…)",
            plan.name,
            output::human_bytes(size),
            &sha256[..12]
        );
        if show {
            println!("  showing {}", plan.name);
        }
    }
    Ok(exit::ok())
}

#[allow(clippy::too_many_arguments)]
fn upload_kanali(
    json: bool,
    session: &legacy::Session,
    analysis: &Analysis,
    ffmpeg: &Path,
    ffprobe: &Path,
    show: bool,
    replace: bool,
    pending: &Pending,
) -> CommandResult {
    let plan = analysis
        .plan
        .as_ref()
        .expect("an acceptable report has a plan");
    let mut connection = session.connect()?;
    let remote = connection.remote_name(&plan.name);
    // Transfer-only products have no catalog to consult.
    let catalog = connection.catalog().unwrap_or_default();
    if catalog.presets.iter().any(|entry| entry.name == remote) {
        return Err(Failure::media(format!(
            "{remote} is a factory preset; pass --name to choose another name"
        )));
    }
    if catalog.user.iter().any(|entry| entry.name == remote) && !replace {
        return Err(Failure::media(format!(
            "{remote} already exists on the display; pass --replace to overwrite it or --name to rename"
        )));
    }
    if !json {
        print_report(analysis);
    }
    let transfer = Transfer::stage(ffmpeg, ffprobe, plan, json, pending, &remote)?;
    let result = (|| -> Result<(u64, String), Failure> {
        let size = std::fs::metadata(&transfer.staged)?.len();
        let sha256 = encode::sha256_file(&transfer.staged)?;
        connection.upload(&transfer.staged, &remote, |sent, total| {
            if !json && total > 0 {
                eprint!(
                    "\r  uploading {:>3}%  {}   ",
                    sent * 100 / total,
                    output::human_bytes(sent)
                );
            }
        })?;
        if !json {
            eprintln!();
        }
        Ok((size, sha256))
    })();
    let (size, sha256) = transfer.done(result)?;
    if show {
        let mut saved = crate::state::load();
        saved.screen.media = vec![remote.clone()];
        connection.apply(&mut saved)?;
        if let Err(error) = crate::state::save(&saved) {
            eprintln!("warning: could not save the display state: {error}");
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": remote,
                "size": size,
                "sha256": sha256,
                "action": plan.action,
                "target": plan.target,
                "shown": show,
            }))?
        );
    } else {
        println!(
            "  uploaded {remote} ({}, sha256 {}…)",
            output::human_bytes(size),
            &sha256[..12]
        );
        if show {
            println!("  showing {remote}");
        }
    }
    Ok(exit::ok())
}

fn push_and_show(
    session: &legacy::Session,
    _target: &DeviceTarget,
    adb: &Adb,
    plan: &Plan,
    staged: &Path,
    show: bool,
) -> Result<(u64, String), Failure> {
    let size = std::fs::metadata(staged)?.len();
    let storage = adb.free_space()?;
    let available = storage.available_kib * 1024;
    if available < size + FREE_SPACE_MARGIN {
        return Err(Failure::media(format!(
            "not enough space on the display: {} free, {} needed",
            output::human_bytes(available),
            output::human_bytes(size + FREE_SPACE_MARGIN)
        )));
    }
    let sha256 = encode::sha256_file(staged)?;
    adb.push(staged, &plan.name)?;
    if show {
        let mut saved = crate::state::load();
        saved.screen.media = vec![plan.name.clone()];
        let mut connection = session.connect()?;
        connection.apply(&mut saved)?;
        if let Err(error) = crate::state::save(&saved) {
            eprintln!("warning: could not save the display state: {error}");
        }
    }
    Ok((size, sha256))
}

pub fn rm(json: bool, session: &legacy::Session, names: &[String]) -> CommandResult {
    if let Some(name) = names.iter().find(|name| !is_safe_media_name(name)) {
        return Err(Failure::usage(format!("media name {name:?} is not safe")));
    }
    let target = match session.select_backend()? {
        Backend::Legacy(target) => target,
        Backend::Kanali { .. } => return rm_kanali(json, session, names),
    };
    let (adb, _) = connect_adb(&target)?;
    let existing = adb.list_media()?;
    if let Some(name) = names
        .iter()
        .find(|name| !existing.iter().any(|entry| &entry.name == *name))
    {
        return Err(Failure::media(format!("{name} is not on the display")));
    }
    let mut connection = session.connect()?;
    let mut removed = Vec::new();
    for name in names {
        // The firmware deletes the file itself; adb only mops up if it did not.
        connection.delete_media(std::slice::from_ref(name))?;
        adb.remove(name)?;
        removed.push(name.clone());
        if !json {
            println!("removed {name}");
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"removed": removed}))?
        );
    }
    Ok(exit::ok())
}

fn rm_kanali(json: bool, session: &legacy::Session, names: &[String]) -> CommandResult {
    let mut connection = session.connect()?;
    let catalog = connection.catalog()?;
    for name in names {
        if catalog.presets.iter().any(|entry| &entry.name == name) {
            return Err(Failure::media(format!(
                "{name} is a factory preset and cannot be removed"
            )));
        }
        if !catalog.user.iter().any(|entry| &entry.name == name) {
            return Err(Failure::media(format!("{name} is not on the display")));
        }
    }
    let mut removed = Vec::new();
    for name in names {
        connection.delete_media(std::slice::from_ref(name))?;
        removed.push(name.clone());
        if !json {
            println!("removed {name}");
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({"removed": removed}))?
        );
    }
    Ok(exit::ok())
}

/// `media export`: a copy of a file on the display, pulled over adb.
pub fn export(
    json: bool,
    session: &legacy::Session,
    name: &str,
    output: Option<PathBuf>,
    force: bool,
) -> CommandResult {
    if !is_safe_media_name(name) {
        return Err(Failure::usage(format!("media name {name:?} is not safe")));
    }
    let target = match session.select_backend()? {
        Backend::Legacy(target) => target,
        Backend::Kanali { .. } => {
            return Err(Failure::device(
                "pulling media from a KANALI display is not implemented",
            ));
        }
    };
    let (adb, _) = connect_adb(&target)?;
    let files = adb.list_media()?;
    let entry = files
        .iter()
        .find(|file| file.name == name)
        .ok_or_else(|| Failure::media(format!("{name} is not on the display")))?;
    let path = output.unwrap_or_else(|| PathBuf::from(name));
    if path.exists() && !force {
        return Err(Failure::media(format!(
            "{} exists; pass --force to overwrite it",
            path.display()
        )));
    }
    adb.pull(name, &path)?;
    let size = std::fs::metadata(&path)?.len();
    if size != entry.size {
        let _ = std::fs::remove_file(&path);
        return Err(Failure::device(format!(
            "pulled {} of {} bytes; the copy was removed",
            size, entry.size
        )));
    }
    let sha256 = encode::sha256_file(&path)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": name,
                "path": path,
                "size": size,
                "sha256": sha256,
            }))?
        );
    } else {
        println!(
            "exported {name} to {} ({}, sha256 {}…)",
            path.display(),
            output::human_bytes(size),
            &sha256[..12]
        );
    }
    Ok(exit::ok())
}

/// `media replace`: a new file under an existing name. The prepared file is
/// pushed under a temporary name, checked, and renamed over the old one,
/// so an interruption leaves either the old file or the new one.
pub fn replace(
    json: bool,
    session: &legacy::Session,
    name: &str,
    file: &Path,
    transform: &TransformArgs,
) -> CommandResult {
    if !is_safe_media_name(name) {
        return Err(Failure::usage(format!("media name {name:?} is not safe")));
    }
    let (ffmpeg, ffprobe) = encode::tools()?;
    let backend = session.select_backend()?;
    let (target, base_name) = match &backend {
        Backend::Legacy(_) => (LEGACY_PANORAMA, name.to_string()),
        Backend::Kanali { product, .. } => {
            let suffix = product.media_name_suffix();
            let base = name.strip_suffix(suffix).ok_or_else(|| {
                Failure::usage(format!("names on this display end with {suffix}"))
            })?;
            (crate::kanali::media_target(*product), base.to_string())
        }
    };
    let pending = Pending {
        kind: "replace",
        source: file.to_path_buf(),
        name: Some(base_name.clone()),
        transform: transform.clone(),
        show: false,
        replace: true,
    };
    let options = transform.options(Some(base_name))?;
    let analysis = analyse(&ffprobe, file, &options, target);
    if !analysis.report.acceptable(false) {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&analysis_json(&analysis))?
            );
        } else {
            print_report(&analysis);
        }
        return Err(Failure::media(
            "the file cannot be prepared for the display",
        ));
    }
    let plan = analysis
        .plan
        .as_ref()
        .expect("an acceptable report has a plan");
    let target_device = match backend {
        Backend::Legacy(target) => target,
        Backend::Kanali { .. } => {
            let connection = session.connect()?;
            if connection.remote_name(&plan.name) != name {
                return Err(Failure::usage(format!(
                    "the prepared file would be called {}, not {name}; remove and upload instead",
                    connection.remote_name(&plan.name)
                )));
            }
            drop(connection);
            return upload_kanali(
                json, session, &analysis, &ffmpeg, &ffprobe, false, true, &pending,
            );
        }
    };
    if plan.name != name {
        return Err(Failure::usage(format!(
            "the prepared file would be called {}, not {name}; remove and upload instead",
            plan.name
        )));
    }
    let (adb, _) = connect_adb(&target_device)?;
    if !adb.list_media()?.iter().any(|entry| entry.name == name) {
        return Err(Failure::media(format!(
            "{name} is not on the display; use `media upload`"
        )));
    }
    if !json {
        print_report(&analysis);
    }
    let transfer = Transfer::stage(&ffmpeg, &ffprobe, plan, json, &pending, name)?;
    let temporary = format!("{name}.replacing-{}", std::process::id());
    let result = (|| -> Result<(u64, String), Failure> {
        let size = std::fs::metadata(&transfer.staged)?.len();
        let sha256 = encode::sha256_file(&transfer.staged)?;
        adb.push(&transfer.staged, &temporary)?;
        let pushed = adb
            .list_media()?
            .into_iter()
            .find(|entry| entry.name == temporary)
            .map(|entry| entry.size);
        if pushed != Some(size) {
            let _ = adb.remove(&temporary);
            return Err(Failure::device(format!(
                "the pushed copy is {} bytes, expected {size}; nothing was replaced",
                pushed
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "missing".into())
            )));
        }
        adb.rename(&temporary, name)?;
        Ok((size, sha256))
    })();
    let (size, sha256) = transfer.done(result)?;
    // The firmware keeps playing the old frames until told again.
    let mut saved = crate::state::load();
    let showing = saved.screen.media.iter().any(|entry| entry == name);
    if showing {
        let mut connection = session.connect()?;
        connection.apply(&mut saved)?;
        let _ = crate::state::save(&saved);
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": name,
                "size": size,
                "sha256": sha256,
                "action": plan.action,
                "reloaded": showing,
            }))?
        );
    } else {
        println!(
            "  replaced {name} ({}, sha256 {}…){}",
            output::human_bytes(size),
            &sha256[..12],
            if showing {
                "; the display reloaded it"
            } else {
                ""
            }
        );
    }
    Ok(exit::ok())
}

pub fn preview(
    json: bool,
    file: &Path,
    at: Option<f64>,
    sheet: bool,
    output: Option<PathBuf>,
    transform: &TransformArgs,
) -> CommandResult {
    let target = transform.target()?;
    let (ffmpeg, ffprobe) = encode::tools()?;
    let options = transform.options(None)?;
    let analysis = analyse(&ffprobe, file, &options, target);
    let Some(kind) = analysis.report.kind else {
        print_report(&analysis);
        return Err(Failure::media("the file has no picture to preview"));
    };
    let duration = analysis.report.source.duration;
    if sheet && kind == check::Kind::Image {
        return Err(Failure::usage("--sheet needs a video"));
    }
    let at = at.map(|seconds| match duration {
        Some(total) if seconds > total => total.max(0.0),
        _ => seconds.max(0.0),
    });

    let inline = output.is_none() && std::io::stdout().is_terminal();
    let path = match &output {
        Some(path) => path.clone(),
        None if inline => {
            std::env::temp_dir().join(format!("tryxctl-preview-{}.png", std::process::id()))
        }
        None => {
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "media".into());
            PathBuf::from(format!("{stem}-preview.png"))
        }
    };
    if sheet {
        let total = duration
            .ok_or_else(|| Failure::media("cannot make a sheet: the duration is unknown"))?;
        preview::render_sheet(&ffmpeg, file, &options.transform, target, total, &path)?;
    } else {
        preview::render_frame(&ffmpeg, file, kind, &options.transform, target, at, &path)?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": path,
                "kind": kind,
                "at": at,
                "sheet": sheet,
                "transform": options.transform,
                "target": target,
            }))?
        );
        return Ok(exit::ok());
    }
    if inline {
        let (columns, _) = viuer::terminal_size();
        let config = viuer::Config {
            width: Some(u32::from(columns.saturating_sub(2).max(20))),
            truecolor: true,
            use_kitty: true,
            use_iterm: true,
            ..Default::default()
        };
        let shown = viuer::print_from_file(&path, &config);
        let _ = std::fs::remove_file(&path);
        shown.map_err(|error| {
            Failure::environment(format!("could not draw the preview: {error}"))
        })?;
        println!(
            "{}",
            output::dim(&format!(
                "{} · {}×{} · {}{}",
                analysis.report.name,
                target.width,
                target.height,
                options.transform.mode,
                at.map(|s| format!(" · at {s:.1} s")).unwrap_or_default()
            ))
        );
    } else {
        println!("wrote {}", path.display());
    }
    Ok(exit::ok())
}
