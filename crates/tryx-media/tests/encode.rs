//! Running a plan through ffmpeg: progress, cancellation, failure, and the
//! checks on what comes out.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use tryx_media::check::{self, Options};
use tryx_media::encode::{self, Progress};
use tryx_media::target::{KANALI_PANORAMA, LEGACY_PANORAMA};
use tryx_media::{MediaError, Plan, Probe, Target};
use tryx_testkit::Sandbox;
use tryx_testkit::media::{self, ffmpeg_available};

fn plan(ffprobe: &Path, path: &Path, target: Target) -> Plan {
    let probe = Probe::read(ffprobe, path).unwrap();
    let size = std::fs::metadata(path).unwrap().len();
    let options = Options::default();
    let report = check::check(path, size, &probe, target, &options);
    Plan::from_report(&report, &options).expect("a plan")
}

#[test]
fn an_encode_reports_progress_and_passes_verification() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let (ffmpeg, ffprobe) = encode::tools().unwrap();
    let clip = media::clip(&sandbox.work().join("clip.mov"), 320, 240, 1.0);
    let plan = plan(&ffprobe, &clip, LEGACY_PANORAMA);
    let output = sandbox.work().join("out.mp4");
    let mut updates: Vec<Progress> = Vec::new();
    encode::run(&ffmpeg, &plan, &output, plan.duration, |p| updates.push(p)).unwrap();
    assert!(!updates.is_empty());
    assert_eq!(
        updates.last().unwrap().fraction,
        Some(1.0),
        "the last update is the end"
    );
    assert!(
        updates
            .iter()
            .all(|p| p.fraction.is_none_or(|f| (0.0..=1.0).contains(&f)))
    );
    let probe = encode::verify(&ffprobe, &plan, &output, LEGACY_PANORAMA).unwrap();
    assert_eq!(probe.video().unwrap().width, Some(1920));
    assert_eq!(
        encode::sha256_file(&output).unwrap().len(),
        64,
        "a hex digest"
    );

    // The same output does not pass for another display.
    assert!(matches!(
        encode::verify(&ffprobe, &plan, &output, KANALI_PANORAMA),
        Err(MediaError::Verify { .. })
    ));
    let empty = sandbox.work().join("empty.mp4");
    std::fs::write(&empty, b"").unwrap();
    match encode::verify(&ffprobe, &plan, &empty, LEGACY_PANORAMA) {
        Err(MediaError::Verify { message, .. }) => assert_eq!(message, "the output is empty"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_cancelled_encode_stops_and_leaves_no_output() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let (ffmpeg, ffprobe) = encode::tools().unwrap();
    let clip = media::clip(&sandbox.work().join("long.mov"), 320, 240, 20.0);
    let plan = plan(&ffprobe, &clip, LEGACY_PANORAMA);
    let output = sandbox.work().join("out.mp4");
    let cancel = AtomicBool::new(true);
    let result = encode::run_cancellable(
        &ffmpeg,
        &plan,
        &output,
        plan.duration,
        Some(&cancel),
        |_| {},
    );
    assert!(matches!(result, Err(MediaError::Cancelled)), "{result:?}");
    assert!(!output.exists());
}

#[test]
fn a_failed_encode_carries_ffmpeg_s_complaint() {
    if !ffmpeg_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let (ffmpeg, ffprobe) = encode::tools().unwrap();
    let clip = media::clip(&sandbox.work().join("gone.mov"), 160, 120, 1.0);
    let plan = plan(&ffprobe, &clip, LEGACY_PANORAMA);
    std::fs::remove_file(&clip).unwrap();
    match encode::run(
        &ffmpeg,
        &plan,
        &sandbox.work().join("out.mp4"),
        None,
        |_| {},
    ) {
        Err(MediaError::Encode { stderr, .. }) => {
            assert!(stderr.contains("gone.mov"), "{stderr}");
        }
        other => panic!("{other:?}"),
    }
}
