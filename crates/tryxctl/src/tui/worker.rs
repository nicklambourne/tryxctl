//! The thread that talks to the display. Serial or USB commands, adb, the
//! host monitor, and the periodic metrics push all live here so frames never
//! interleave on the link.

use crate::ipc::{self, Request as IpcRequest};
use crate::legacy::{Connection, Info, Readback, Session, Target};
use crate::media::{self, TransformArgs, connect_adb, finish_stage};
use crate::metrics::pc_info;
use crate::ops::{self, Outcome, Pending, Record};
use crate::state;
use crate::tui::player::{Player, Sink};
use crate::tui::preview as pictures;
use image::DynamicImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tryx_legacy::FanStatus;
use tryx_legacy::ScreenConfig;
use tryx_legacy::adb::{Adb, DiskUsage, MediaFile};
use tryx_legacy::commands::{parse_preset, preset_id};
use tryx_media::check::Severity;
use tryx_media::encode;
use tryx_media::plan::Action;
use tryx_monitor::{Monitor, Sample};

pub enum Request {
    Refresh,
    Show {
        media: Vec<String>,
        play: String,
    },
    Delete(String),
    Export(String),
    Brightness(u8),
    Overlay(Box<ScreenConfig>),
    Layout {
        screen: Box<ScreenConfig>,
        rotation: Option<u16>,
    },
    Readback,
    Analyse {
        path: PathBuf,
        transform: Box<TransformArgs>,
    },
    /// A picture of a file on the display, by name and size.
    Thumbnail {
        name: String,
        size: u64,
    },
    /// A picture of a local file as the display would get it.
    Preview {
        key: String,
        path: PathBuf,
        transform: Box<TransformArgs>,
    },
    /// Real-time playback in a pane of `cols`×`rows` cells.
    Play {
        key: String,
        source: PlaySource,
        cols: u16,
        rows: u16,
        sink: Sink,
    },
    Upload {
        path: PathBuf,
        transform: Box<TransformArgs>,
    },
    Retry(String),
    ClearCache,
    PushMetrics(bool),
    Quit,
}

/// What to play: a file on the display, or a local file through a transform.
pub enum PlaySource {
    Device {
        name: String,
        size: u64,
    },
    Local {
        path: PathBuf,
        transform: Box<TransformArgs>,
    },
}

/// One connected display, as `tryxctl devices` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub product: String,
    pub usb_id: String,
    pub serial: String,
    pub access: String,
    pub protocol: String,
}

pub enum Event {
    Info(Info),
    Fans(FanStatus),
    Via(&'static str),
    Media {
        files: Vec<MediaFile>,
        storage: Option<DiskUsage>,
    },
    Devices(Vec<DeviceRow>),
    Sample(Sample),
    Pushing(bool),
    UploadProgress {
        name: String,
        fraction: f64,
    },
    Analysis {
        path: PathBuf,
        lines: Vec<String>,
        acceptable: bool,
    },
    Readback(Box<Readback>),
    Operations(Vec<Record>),
    Preview {
        key: String,
        frames: Vec<DynamicImage>,
        interval_ms: u64,
    },
    PreviewFailed {
        key: String,
        reason: String,
    },
    Playing {
        key: String,
        player: Box<Player>,
    },
    Log(String),
    Error(String),
}

const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

pub struct Worker {
    handle: JoinHandle<()>,
}

impl Worker {
    pub fn spawn(
        session: Session,
        target: Option<Target>,
        cancel: Arc<AtomicBool>,
        requests: Receiver<Request>,
        events: Sender<Event>,
    ) -> Worker {
        let handle = std::thread::spawn(move || {
            let mut state = WorkerState::new(session, target, cancel, events);
            state.run(requests);
        });
        Worker { handle }
    }

    pub fn join(self) {
        let _ = self.handle.join();
    }
}

struct WorkerState {
    session: Session,
    /// The legacy serial target; `None` on KANALI displays.
    target: Option<Target>,
    cancel: Arc<AtomicBool>,
    events: Sender<Event>,
    connection: Option<Connection>,
    adb: Option<Adb>,
    monitor: Monitor,
    pushing: bool,
    last_sample: Instant,
}

impl WorkerState {
    fn new(
        session: Session,
        target: Option<Target>,
        cancel: Arc<AtomicBool>,
        events: Sender<Event>,
    ) -> Self {
        WorkerState {
            session,
            target,
            cancel,
            events,
            connection: None,
            adb: None,
            monitor: Monitor::new(),
            pushing: false,
            last_sample: Instant::now() - SAMPLE_INTERVAL,
        }
    }

    fn run(&mut self, requests: Receiver<Request>) {
        self.connect();
        loop {
            match requests.recv_timeout(Duration::from_millis(250)) {
                Ok(Request::Quit) => break,
                Ok(request) => self.handle(request),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.last_sample.elapsed() >= SAMPLE_INTERVAL && Monitor::supported() {
                self.last_sample = Instant::now();
                self.tick();
            }
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// Every two seconds: a host sample for the footer, plus the push when
    /// this worker owns the link. With the daemon, its status carries both.
    fn tick(&mut self) {
        match &mut self.connection {
            Some(Connection::Daemon { .. }) => match ipc::call(&IpcRequest::Status) {
                Ok(Some(reply)) if reply.ok => {
                    if let Ok(status) = serde_json::from_value::<ipc::DaemonStatus>(reply.value) {
                        if let Some(sample) = status.sample {
                            self.emit(Event::Sample(sample));
                        }
                        self.emit(Event::Fans(status.fans));
                    }
                }
                _ => self.emit(Event::Error("lost the daemon".to_string())),
            },
            Some(Connection::Direct { client, .. }) => {
                let mut sample = self.monitor.sample();
                sample.timestamp_ms += tryx_legacy::local_utc_offset_ms();
                if self.pushing {
                    match client.send_sysinfo(&pc_info(&sample)) {
                        Ok(fans) => self.emit(Event::Fans(fans)),
                        Err(error) => {
                            self.pushing = false;
                            self.emit(Event::Pushing(false));
                            self.emit(Event::Error(format!("metrics push stopped: {error}")));
                        }
                    }
                }
                self.emit(Event::Sample(sample));
            }
            Some(Connection::Kanali(link)) => {
                let sample = self.monitor.sample();
                if self.pushing {
                    let result = link.keepalive().and_then(|()| link.push(&sample));
                    if let Err(error) = result {
                        self.pushing = false;
                        self.emit(Event::Pushing(false));
                        self.emit(Event::Error(format!("keepalive stopped: {error}")));
                    }
                }
                self.emit(Event::Sample(sample));
            }
            None => {
                let sample = self.monitor.sample();
                self.emit(Event::Sample(sample));
            }
        }
    }

    fn connect(&mut self) {
        match self.session.connect() {
            Ok(mut connection) => {
                self.emit(Event::Via(connection.via()));
                match connection.info() {
                    Ok(info) => self.emit(Event::Info(info)),
                    Err(failure) => self.emit(Event::Error(format!(
                        "handshake failed: {}",
                        failure.message
                    ))),
                }
                if let Connection::Kanali(link) = &mut connection
                    && let Err(failure) = link.adopt_state(&state::load())
                {
                    self.emit(Event::Error(failure.message));
                }
                if matches!(
                    connection,
                    Connection::Daemon { .. } | Connection::Kanali(_)
                ) {
                    self.pushing = true;
                    self.emit(Event::Pushing(true));
                }
                self.connection = Some(connection);
            }
            Err(failure) => self.emit(Event::Error(failure.message)),
        }
        if let Some(target) = &self.target {
            match connect_adb(target) {
                Ok((adb, _)) => self.adb = Some(adb),
                Err(failure) => self.emit(Event::Error(format!("adb: {}", failure.message))),
            }
        }
    }

    fn handle(&mut self, request: Request) {
        let outcome = match request {
            Request::Refresh => self.refresh().and_then(|()| {
                self.devices();
                self.operations();
                self.readback()
            }),
            Request::Show { media, play } => self.show(media, play),
            Request::Delete(name) => self.delete(&name),
            Request::Export(name) => self.export(&name),
            Request::Brightness(value) => self.brightness(value),
            Request::Overlay(screen) => self.overlay(*screen),
            Request::Layout { screen, rotation } => self.layout(*screen, rotation),
            Request::Readback => self.readback(),
            Request::Analyse { path, transform } => self.analyse(&path, &transform),
            Request::Thumbnail { name, size } => {
                let key = format!("thumb:{name}");
                match self.thumbnail(&name, size) {
                    Ok(clip) => self.emit(Event::Preview {
                        key,
                        frames: clip.frames,
                        interval_ms: clip.interval.as_millis() as u64,
                    }),
                    Err(reason) => self.emit(Event::PreviewFailed { key, reason }),
                }
                Ok(())
            }
            Request::Play {
                key,
                source,
                cols,
                rows,
                sink,
            } => match self.play(source, cols, rows, sink) {
                Ok(player) => {
                    self.emit(Event::Playing {
                        key,
                        player: Box::new(player),
                    });
                    Ok(())
                }
                Err(reason) => Err(format!("playback: {reason}")),
            },
            Request::Preview {
                key,
                path,
                transform,
            } => {
                match self.preview(&path, &transform) {
                    Ok(clip) => self.emit(Event::Preview {
                        key,
                        frames: clip.frames,
                        interval_ms: clip.interval.as_millis() as u64,
                    }),
                    Err(reason) => self.emit(Event::PreviewFailed { key, reason }),
                }
                Ok(())
            }
            Request::Upload { path, transform } => self.upload(&path, &transform, None),
            Request::Retry(id) => self.retry(&id),
            Request::ClearCache => {
                // Not `op clear`, which prints over the interface.
                let removed = ops::remove_kept(false);
                self.operations();
                self.emit(Event::Log(format!("kept encodes removed ({removed})")));
                Ok(())
            }
            Request::PushMetrics(enabled) => match &self.connection {
                Some(Connection::Daemon { .. }) => {
                    self.emit(Event::Pushing(true));
                    Err("the daemon pushes metrics; stop it to push from here".to_string())
                }
                Some(Connection::Direct { .. } | Connection::Kanali(_)) => {
                    self.pushing = enabled;
                    self.emit(Event::Pushing(enabled));
                    Ok(())
                }
                None => Err("not connected".to_string()),
            },
            Request::Quit => Ok(()),
        };
        if let Err(message) = outcome {
            self.emit(Event::Error(message));
        }
    }

    fn connection(&mut self) -> Result<&mut Connection, String> {
        self.connection
            .as_mut()
            .ok_or_else(|| "not connected".to_string())
    }

    fn adb(&self) -> Result<&Adb, String> {
        self.adb
            .as_ref()
            .ok_or_else(|| "adb is not connected".to_string())
    }

    /// The files on the display and, over adb, the free space.
    fn list(&mut self) -> Result<(Vec<MediaFile>, Option<DiskUsage>), String> {
        if self.target.is_some() {
            let adb = self.adb()?;
            let files = adb.list_media().map_err(|e| e.to_string())?;
            return Ok((files, adb.free_space().ok()));
        }
        let catalog = self.connection()?.catalog().map_err(|f| f.message)?;
        let files = catalog
            .user
            .into_iter()
            .map(|entry| MediaFile {
                name: entry.name,
                size: u64::from(entry.size),
            })
            .collect();
        Ok((files, None))
    }

    fn refresh(&mut self) -> Result<(), String> {
        let (files, storage) = self.list()?;
        self.emit(Event::Media { files, storage });
        Ok(())
    }

    fn devices(&mut self) {
        let rows = match tryx_device::discover() {
            Ok(discovery) => {
                let mut rows: Vec<DeviceRow> = discovery
                    .printer_devices
                    .iter()
                    .map(|device| DeviceRow {
                        id: device.id.clone(),
                        product: device
                            .product
                            .map(|p| p.name().to_string())
                            .unwrap_or_else(|| "unknown TRYX product".into()),
                        usb_id: device.usb_id.clone(),
                        serial: device.serial.clone().unwrap_or_else(|| "-".into()),
                        access: device.access.to_string(),
                        protocol: "KANALI".to_string(),
                    })
                    .collect();
                rows.extend(discovery.legacy_devices.iter().map(|device| {
                    DeviceRow {
                        id: device.id.clone(),
                        product: device.product_string.clone(),
                        usb_id: device.usb_id.clone(),
                        serial: device.serial.clone().unwrap_or_else(|| "-".into()),
                        access: device
                            .tty_access
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| "-".into()),
                        protocol: format!(
                            "legacy ({})",
                            device.tty.clone().unwrap_or_else(|| "no port".into())
                        ),
                    }
                }));
                rows
            }
            Err(error) => {
                self.emit(Event::Error(format!("discovery: {error}")));
                Vec::new()
            }
        };
        self.emit(Event::Devices(rows));
    }

    fn operations(&mut self) {
        self.emit(Event::Operations(ops::load()));
    }

    fn readback(&mut self) -> Result<(), String> {
        let readback = self.connection()?.readback().map_err(|f| f.message)?;
        self.emit(Event::Readback(Box::new(readback)));
        Ok(())
    }

    fn show(&mut self, media: Vec<String>, play: String) -> Result<(), String> {
        let mut saved = state::load();
        let preset = match media.as_slice() {
            [only] => parse_preset(only).and_then(preset_id),
            _ => None,
        };
        match preset {
            Some(id) => saved.screen.preset_id = id.to_string(),
            None => {
                saved.screen.preset_id.clear();
                saved.screen.media = media.clone();
            }
        }
        saved.screen.play_mode = play.clone();
        self.connection()?
            .apply(&mut saved)
            .map_err(|f| f.message)?;
        let _ = state::save(&saved);
        self.emit(Event::Log(format!(
            "showing {} ({play})",
            preset
                .map(str::to_string)
                .unwrap_or_else(|| media.join(", "))
        )));
        self.readback()
    }

    fn delete(&mut self, name: &str) -> Result<(), String> {
        self.connection()?
            .delete_media(&[name.to_string()])
            .map_err(|f| f.message)?;
        if let Some(adb) = &self.adb {
            adb.remove(name).map_err(|e| e.to_string())?;
        }
        self.emit(Event::Log(format!("removed {name}")));
        self.refresh()
    }

    fn export(&mut self, name: &str) -> Result<(), String> {
        if self.target.is_none() {
            return Err("pulling media from a KANALI display is not implemented".to_string());
        }
        let path = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(name);
        if path.exists() {
            return Err(format!("{} exists already", path.display()));
        }
        let adb = self.adb()?;
        let size = adb
            .list_media()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|file| file.name == name)
            .map(|file| file.size)
            .ok_or_else(|| format!("{name} is not on the display"))?;
        media::pull_whole(adb, name, size, &path).map_err(|f| f.message)?;
        self.emit(Event::Log(format!("exported {name} to {}", path.display())));
        Ok(())
    }

    fn brightness(&mut self, value: u8) -> Result<(), String> {
        self.connection()?
            .brightness(value)
            .map_err(|f| f.message)?;
        let mut saved = state::load();
        saved.brightness = Some(value);
        let _ = state::save(&saved);
        self.emit(Event::Log(format!("brightness {value}")));
        Ok(())
    }

    fn overlay(&mut self, screen: ScreenConfig) -> Result<(), String> {
        let mut saved = state::load();
        saved.screen = screen;
        self.connection()?
            .apply(&mut saved)
            .map_err(|f| f.message)?;
        let _ = state::save(&saved);
        self.emit(Event::Log(if saved.screen.sysinfo_display.is_empty() {
            "overlay cleared".to_string()
        } else {
            format!("overlay: {}", saved.screen.sysinfo_display.join(", "))
        }));
        Ok(())
    }

    fn layout(&mut self, screen: ScreenConfig, rotation: Option<u16>) -> Result<(), String> {
        let mut saved = state::load();
        saved.screen = screen;
        self.connection()?
            .apply(&mut saved)
            .map_err(|f| f.message)?;
        if let Some(degrees) = rotation {
            self.connection()?.rotate(degrees).map_err(|f| f.message)?;
            saved.rotation = Some(degrees);
        }
        let _ = state::save(&saved);
        self.emit(Event::Log(format!(
            "layout: {}, waterfall {}, rotation {}°",
            saved.screen.screen_mode.to_lowercase(),
            if saved.screen.waterfall_mode {
                "on"
            } else {
                "off"
            },
            saved.rotation.unwrap_or(0)
        )));
        self.readback()
    }

    fn analyse(&mut self, path: &Path, transform: &TransformArgs) -> Result<(), String> {
        let (_, ffprobe) = encode::tools().map_err(|e| e.to_string())?;
        let options = transform.options(None).map_err(|f| f.message)?;
        let target = self.connection()?.media_target();
        let analysis = media::analyse(&ffprobe, path, &options, target);
        self.emit(Event::Analysis {
            path: path.to_path_buf(),
            lines: media::finding_lines(&analysis),
            acceptable: analysis.report.acceptable(false),
        });
        Ok(())
    }

    fn thumbnail(&mut self, name: &str, size: u64) -> Result<pictures::Clip, String> {
        if self.target.is_none() {
            return Err("no preview on this firmware: media pull is not implemented".to_string());
        }
        let (ffmpeg, _) = encode::tools().map_err(|e| e.to_string())?;
        let adb = self.adb()?;
        pictures::device_clip(&ffmpeg, adb, name, size)
    }

    fn preview(
        &mut self,
        path: &Path,
        transform: &TransformArgs,
    ) -> Result<pictures::Clip, String> {
        let (ffmpeg, ffprobe) = encode::tools().map_err(|e| e.to_string())?;
        let options = transform.options(None).map_err(|f| f.message)?;
        let target = self.connection()?.media_target();
        let analysis = media::analyse(&ffprobe, path, &options, target);
        let kind = analysis.report.kind.ok_or("not a media file")?;
        // A frame a little way in, where a clip has settled.
        let at = analysis
            .report
            .source
            .duration
            .map(|duration| (duration / 3.0).min(2.0));
        pictures::local_clip(&ffmpeg, path, kind, &options.transform, target, at)
    }

    fn play(
        &mut self,
        source: PlaySource,
        cols: u16,
        rows: u16,
        sink: Sink,
    ) -> Result<Player, String> {
        let (ffmpeg, ffprobe) = encode::tools().map_err(|e| e.to_string())?;
        match source {
            PlaySource::Device { name, size } => {
                if self.target.is_none() {
                    return Err(
                        "no playback on this firmware: media pull is not implemented".to_string(),
                    );
                }
                let adb = self.adb()?;
                let path = pictures::device_file(adb, &name, size)?;
                Player::start(&ffmpeg, &path, None, None, cols, rows, sink)
            }
            PlaySource::Local { path, transform } => {
                let options = transform.options(None).map_err(|f| f.message)?;
                let target = self.connection()?.media_target();
                let analysis = media::analyse(&ffprobe, &path, &options, target);
                match analysis.report.kind {
                    Some(tryx_media::check::Kind::Image) => {
                        return Err("a still image; the preview already shows it".to_string());
                    }
                    Some(_) => {}
                    None => return Err("not a media file".to_string()),
                }
                let geometry = options.transform.image_filter(target.width, target.height);
                Player::start(&ffmpeg, &path, Some(&geometry), None, cols, rows, sink)
            }
        }
    }

    fn retry(&mut self, id: &str) -> Result<(), String> {
        let record = ops::load()
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| format!("no transfer {id}"))?;
        if record.outcome != Outcome::Failed {
            return Err("only failed transfers can be retried".to_string());
        }
        if record.kind != "upload" {
            return Err(format!("retry a {} from the command line", record.kind));
        }
        self.upload(&record.source, &record.transform, record.name.clone())
    }

    fn upload(
        &mut self,
        path: &Path,
        transform: &TransformArgs,
        name: Option<String>,
    ) -> Result<(), String> {
        let (ffmpeg, ffprobe) = encode::tools().map_err(|e| e.to_string())?;
        let options = transform.options(name.clone()).map_err(|f| f.message)?;
        let target = self.connection()?.media_target();
        let analysis = media::analyse(&ffprobe, path, &options, target);
        if !analysis.report.acceptable(false) {
            let reason = analysis
                .report
                .findings
                .iter()
                .find(|f| f.severity == Severity::Fatal)
                .map(|f| f.message.clone())
                .unwrap_or_else(|| "rejected".to_string());
            return Err(format!("{}: {reason}", path.display()));
        }
        let plan = analysis.plan.ok_or("nothing to upload")?;
        let remote = self.connection()?.remote_name(&plan.name);
        if self.list()?.0.iter().any(|f| f.name == remote) {
            return Err(format!("{remote} already exists on the display"));
        }
        let pending = Pending {
            kind: "upload",
            source: path.to_path_buf(),
            name,
            transform: transform.clone(),
            show: false,
            replace: false,
        };
        let record = ops::begin(&pending, &remote, plan.target.id);
        let fail = |id: &str, message: String, cached: Option<PathBuf>| {
            ops::finish(
                id,
                Outcome::Failed,
                Some(message.clone()),
                cached,
                None,
                None,
            );
            message
        };
        let key = if plan.action == Action::Passthrough {
            None
        } else {
            Some(
                ops::cache_key(&plan.input, transform, plan.target.id, &remote)
                    .map_err(|f| fail(&record.id, f.message, None))?,
            )
        };
        let (staged, owned) = if plan.action == Action::Passthrough {
            (plan.input.clone(), false)
        } else if let Some(cached) = key.as_deref().and_then(ops::cached) {
            self.emit(Event::Log(format!("reusing the encode kept for {remote}")));
            (cached, true)
        } else {
            let staged =
                std::env::temp_dir().join(format!("tryxctl-tui-{}-{}", std::process::id(), remote));
            let events = self.events.clone();
            let progress_name = remote.clone();
            self.cancel.store(false, Ordering::Relaxed);
            let encoded = encode::run_cancellable(
                &ffmpeg,
                &plan,
                &staged,
                plan.duration,
                Some(&self.cancel),
                |progress| {
                    if let Some(fraction) = progress.fraction {
                        let _ = events.send(Event::UploadProgress {
                            name: progress_name.clone(),
                            fraction,
                        });
                    }
                },
            )
            .map_err(|e| e.to_string())
            .and_then(|()| finish_stage(&ffprobe, &plan, &staged).map_err(|f| f.message));
            if let Err(message) = encoded {
                let _ = std::fs::remove_file(&staged);
                self.emit(Event::UploadProgress {
                    name: remote.clone(),
                    fraction: 1.0,
                });
                return Err(fail(&record.id, message, None));
            }
            (staged, true)
        };
        self.emit(Event::UploadProgress {
            name: remote.clone(),
            fraction: 1.0,
        });
        let size = std::fs::metadata(&staged).map(|m| m.len()).unwrap_or(0);
        let sha256 = encode::sha256_file(&staged).unwrap_or_default();
        let result = if self.target.is_some() {
            self.adb()?
                .push(&staged, &remote)
                .map_err(|e| e.to_string())
        } else {
            let events = self.events.clone();
            let progress_name = remote.clone();
            self.connection()?
                .upload(&staged, &remote, |sent, total| {
                    if total > 0 {
                        let _ = events.send(Event::UploadProgress {
                            name: progress_name.clone(),
                            fraction: sent as f64 / total as f64,
                        });
                    }
                })
                .map_err(|f| f.message)
        };
        self.emit(Event::UploadProgress {
            name: remote.clone(),
            fraction: 1.0,
        });
        match result {
            Ok(()) => {
                if owned {
                    let _ = std::fs::remove_file(&staged);
                }
                if let Some(key) = &key {
                    ops::discard(key);
                }
                ops::finish(
                    &record.id,
                    Outcome::Ok,
                    None,
                    None,
                    Some(size),
                    Some(sha256),
                );
            }
            Err(message) => {
                let cached = match (&key, owned) {
                    (Some(key), true) => ops::keep(key, &staged).ok(),
                    _ => None,
                };
                let note = if cached.is_some() {
                    "; the encode is kept, retry it from Operations"
                } else {
                    ""
                };
                let message = fail(&record.id, format!("{message}{note}"), cached);
                self.operations();
                return Err(message);
            }
        }
        self.emit(Event::Log(format!(
            "uploaded {remote} ({})",
            plan.description
        )));
        self.operations();
        self.refresh()
    }
}

/// The worker against a fake display and a fake adb. Each test runs in its
/// own process confined to a sandbox, since the worker reads the saved
/// state, the journal, and the device tree from its environment.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::legacy::{Connection, Session};
    use std::sync::mpsc;
    use tryx_legacy::{Client, SerialLink};
    use tryx_testkit::cm01::SERIAL;
    use tryx_testkit::media::{self as samples, ffmpeg_available};
    use tryx_testkit::{FakeAdb, FakeCm01, Sandbox, isolated};

    struct Rig {
        display: FakeCm01,
        adb: FakeAdb,
        worker: WorkerState,
        events: Receiver<Event>,
    }

    impl Rig {
        /// A worker holding the fake display's port directly, as it would
        /// after connecting without a daemon.
        fn new(sandbox: &Sandbox) -> Rig {
            let display = FakeCm01::start();
            sandbox.plug("3-12", SERIAL, "ttyACM0", display.port());
            let adb = FakeAdb::install(sandbox, SERIAL, "3-12");
            let session = Session {
                tty: None,
                device: None,
                verbose: false,
                direct: true,
            };
            let (events_tx, events) = mpsc::channel();
            let target = crate::legacy::select(None).expect("the plugged display");
            let mut worker = WorkerState::new(
                session,
                Some(target),
                Arc::new(AtomicBool::new(false)),
                events_tx,
            );
            let client = Client::from_link(SerialLink::from_port(Box::new(display.open()), "fake"));
            worker.connection = Some(Connection::Direct {
                client: Box::new(client),
                target: Box::new(crate::legacy::select(None).unwrap()),
            });
            worker.adb = Some(connect_adb(worker.target.as_ref().unwrap()).unwrap().0);
            Rig {
                display,
                adb,
                worker,
                events,
            }
        }

        /// Handles `request` and describes the events it produced.
        fn handle(&mut self, request: Request) -> Vec<String> {
            self.worker.handle(request);
            self.events
                .try_iter()
                .map(|event| describe(&event))
                .collect()
        }
    }

    fn describe(event: &Event) -> String {
        match event {
            Event::Info(info) => format!("info {}", info.serial()),
            Event::Fans(fans) => format!("fans {:?}", fans.lcd_fan_rpm),
            Event::Via(via) => format!("via {via}"),
            Event::Media { files, storage } => format!(
                "media [{}]{}",
                files
                    .iter()
                    .map(|f| f.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                if storage.is_some() {
                    " with storage"
                } else {
                    ""
                }
            ),
            Event::Devices(rows) => format!(
                "devices [{}]",
                rows.iter()
                    .map(|r| format!("{} {}", r.id, r.protocol))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Event::Sample(_) => "sample".to_string(),
            Event::Pushing(on) => format!("pushing {on}"),
            Event::UploadProgress { name, .. } => format!("progress {name}"),
            Event::Analysis {
                acceptable, lines, ..
            } => {
                format!("analysis acceptable={acceptable} {}", lines.join(" | "))
            }
            Event::Readback(readback) => format!("readback [{}]", readback.media.join(", ")),
            Event::Operations(records) => format!("operations {}", records.len()),
            Event::Preview { key, frames, .. } => {
                format!("preview {key} {} frame(s)", frames.len())
            }
            Event::PreviewFailed { key, reason } => format!("no preview {key}: {reason}"),
            Event::Playing { key, .. } => format!("playing {key}"),
            Event::Log(text) => format!("log {text}"),
            Event::Error(text) => format!("error {text}"),
        }
    }

    #[test]
    fn refresh_reports_files_devices_transfers_and_the_screen() {
        isolated(
            "tui::worker::tests::refresh_reports_files_devices_transfers_and_the_screen",
            |sandbox| {
                let mut rig = Rig::new(sandbox);
                rig.adb.put("clip.mp4", &[0; 64]);
                let port = sandbox.port("ttyACM0");
                assert_eq!(
                    rig.handle(Request::Refresh),
                    [
                        "media [clip.mp4] with storage".to_string(),
                        format!("devices [usb:003-12 legacy ({})]", port.display()),
                        "operations 0".to_string(),
                        "readback []".to_string(),
                    ]
                );
            },
        );
    }

    #[test]
    fn show_and_settings_reach_the_display_and_are_remembered() {
        isolated(
            "tui::worker::tests::show_and_settings_reach_the_display_and_are_remembered",
            |sandbox| {
                let mut rig = Rig::new(sandbox);
                assert_eq!(
                    rig.handle(Request::Show {
                        media: vec!["clip.mp4".into()],
                        play: "Loop".into()
                    }),
                    ["log showing clip.mp4 (Loop)", "readback [clip.mp4]"]
                );
                assert_eq!(rig.display.received("waterBlockScreenId").len(), 2);
                assert_eq!(state::load().screen.play_mode, "Loop");
                assert_eq!(
                    rig.handle(Request::Show {
                        media: vec!["preset:4".into()],
                        play: "Single".into()
                    })[0],
                    "log showing Pre-set 4: Exo-Ecologies (Single)"
                );

                assert_eq!(rig.handle(Request::Brightness(40)), ["log brightness 40"]);
                assert_eq!(rig.display.received("brightness")[0].json()["value"], 40);
                assert_eq!(state::load().brightness, Some(40));

                let screen = ScreenConfig {
                    media: vec!["clip.mp4".into()],
                    sysinfo_display: vec!["CPU Usage".into()],
                    ..ScreenConfig::default()
                };
                assert_eq!(
                    rig.handle(Request::Overlay(Box::new(screen.clone()))),
                    ["log overlay: CPU Usage"]
                );
                assert_eq!(
                    rig.display
                        .received("sysinfoDisplay")
                        .last()
                        .unwrap()
                        .json()["items"][0],
                    "CPU Usage"
                );
                let split = ScreenConfig {
                    screen_mode: tryx_legacy::commands::SCREEN_SPLITTING.into(),
                    ..screen
                };
                assert_eq!(
                    rig.handle(Request::Layout {
                        screen: Box::new(split),
                        rotation: Some(90)
                    }),
                    [
                        "log layout: screen splitting, waterfall off, rotation 90°",
                        "readback [clip.mp4]"
                    ]
                );
                assert_eq!(rig.display.received("rotate")[0].json()["degree"], 90);
                assert_eq!(state::load().rotation, Some(90));

                rig.display.set(|firmware| firmware.silent = true);
                if let Some(Connection::Direct { client, .. }) = rig.worker.connection.as_mut() {
                    client.link_mut().response_timeout = Duration::from_millis(100);
                }
                let failed = rig.handle(Request::Brightness(10));
                assert_eq!(failed, ["error no response to `brightness` within 100 ms"]);
                assert_eq!(
                    state::load().brightness,
                    Some(40),
                    "a failed change is not saved"
                );
            },
        );
    }

    #[test]
    fn delete_and_export_work_on_the_display_files() {
        isolated(
            "tui::worker::tests::delete_and_export_work_on_the_display_files",
            |sandbox| {
                let mut rig = Rig::new(sandbox);
                rig.adb.put("a.png", b"a");
                let bytes: Vec<u8> = (0..=255).cycle().take(5000).collect();
                rig.adb.put("clip.mp4", &bytes);
                assert_eq!(
                    rig.handle(Request::Delete("a.png".into())),
                    ["log removed a.png", "media [clip.mp4] with storage"]
                );
                assert_eq!(
                    rig.display.received("mediaDelete")[0].json()["include"][0],
                    "a.png"
                );
                assert_eq!(rig.adb.names(), ["clip.mp4"]);

                let path = sandbox.work().join("clip.mp4");
                assert_eq!(
                    rig.handle(Request::Export("clip.mp4".into())),
                    [format!("log exported clip.mp4 to {}", path.display())]
                );
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                assert_eq!(
                    rig.handle(Request::Export("clip.mp4".into())),
                    [format!("error {} exists already", path.display())]
                );
                assert_eq!(
                    rig.handle(Request::Export("gone.mp4".into())),
                    ["error gone.mp4 is not on the display"]
                );
                // A pull that stops short leaves nothing that passes for the file.
                rig.adb.put("second.mp4", &bytes);
                rig.adb.truncate_pulls(Some(100));
                assert_eq!(
                    rig.handle(Request::Export("second.mp4".into())),
                    ["error pulled 100 of 5000 bytes; the copy was removed"]
                );
                assert!(!sandbox.work().join("second.mp4").exists());
            },
        );
    }

    #[test]
    fn uploads_are_journalled_and_a_failed_one_retries_from_its_kept_encode() {
        isolated(
            "tui::worker::tests::uploads_are_journalled_and_a_failed_one_retries_from_its_kept_encode",
            |sandbox| {
                if !ffmpeg_available() {
                    return;
                }
                let mut rig = Rig::new(sandbox);
                let still = samples::picture(&sandbox.work().join("still.png"), 64, 32);
                let plain = || Box::new(TransformArgs::default());
                let events = rig.handle(Request::Upload {
                    path: still.clone(),
                    transform: plain(),
                });
                assert!(
                    events.contains(&"progress still.png".to_string()),
                    "{events:?}"
                );
                assert!(
                    events.iter().any(|e| e.starts_with("log uploaded still.png (convert to a 1920×960 PNG")),
                    "{events:?}"
                );
                assert_eq!(events.last().unwrap(), "media [still.png] with storage");
                assert!(rig.adb.read("still.png").is_some());
                assert_eq!(
                    rig.handle(Request::Upload {
                        path: still,
                        transform: plain()
                    }),
                    ["error still.png already exists on the display"]
                );

                // Two transfers begun by one process within a second each keep
                // their own record.
                let other = samples::picture(&sandbox.work().join("other.png"), 64, 32);
                rig.adb.fail("push", Some("adb: error: closed"));
                let events = rig.handle(Request::Upload {
                    path: other,
                    transform: plain(),
                });
                let error = events
                    .iter()
                    .find(|e| e.starts_with("error "))
                    .expect("an error");
                assert!(
                    error.ends_with("; the encode is kept, retry it from Operations"),
                    "{error}"
                );
                let records = ops::load();
                assert_eq!(records.len(), 2);
                assert_ne!(records[0].id, records[1].id);
                assert_eq!(
                    (records[0].remote.as_str(), records[0].outcome),
                    ("still.png", Outcome::Ok)
                );
                assert_eq!(
                    (records[1].remote.as_str(), records[1].outcome),
                    ("other.png", Outcome::Failed)
                );
                let kept = records[1].cached.clone().expect("the encode is kept");
                assert!(kept.is_file());

                rig.adb.fail("push", None);
                let events = rig.handle(Request::Retry(records[1].id.clone()));
                assert!(
                    events.contains(&"log reusing the encode kept for other.png".to_string()),
                    "{events:?}"
                );
                assert!(
                    events
                        .iter()
                        .any(|e| e.starts_with("log uploaded other.png")),
                    "{events:?}"
                );
                assert!(!kept.exists(), "the kept encode was sent and removed");
                assert_eq!(ops::load().last().unwrap().outcome, Outcome::Ok);

                assert_eq!(
                    rig.handle(Request::Retry(records[0].id.clone())),
                    ["error only failed transfers can be retried"]
                );
                assert_eq!(
                    rig.handle(Request::Retry("nope".into())),
                    ["error no transfer nope"]
                );
                assert_eq!(
                    rig.handle(Request::ClearCache),
                    ["operations 3", "log kept encodes removed (0)"]
                );
            },
        );
    }

    #[test]
    fn analysis_and_previews_describe_local_and_display_files() {
        isolated(
            "tui::worker::tests::analysis_and_previews_describe_local_and_display_files",
            |sandbox| {
                if !ffmpeg_available() {
                    return;
                }
                let mut rig = Rig::new(sandbox);
                let clip = samples::clip(&sandbox.work().join("clip.mp4"), 320, 180, 2.0);
                let events = rig.handle(Request::Analyse {
                    path: clip.clone(),
                    transform: Box::new(TransformArgs::default()),
                });
                assert_eq!(events.len(), 1);
                assert!(
                    events[0].starts_with("analysis acceptable=true "),
                    "{events:?}"
                );
                assert!(
                    events[0].contains("plan: re-encode to 1920×960 H.264 MP4"),
                    "{events:?}"
                );

                let events = rig.handle(Request::Preview {
                    key: "local".into(),
                    path: clip.clone(),
                    transform: Box::new(TransformArgs::default()),
                });
                assert!(events[0].starts_with("preview local "), "{events:?}");
                assert!(!events[0].starts_with("preview local 0 "), "{events:?}");

                // A file on the display is previewed from its first megabytes
                // over adb, once, and then from the cache.
                let bytes = std::fs::read(&clip).unwrap();
                rig.adb.put("clip.mp4", &bytes);
                let size = bytes.len() as u64;
                let thumb = |rig: &mut Rig| {
                    rig.handle(Request::Thumbnail {
                        name: "clip.mp4".into(),
                        size,
                    })
                };
                let events = thumb(&mut rig);
                assert!(
                    events[0].starts_with("preview thumb:clip.mp4 "),
                    "{events:?}"
                );
                let first = rig_calls(&rig.adb, "exec-out");
                assert_eq!(first, 1);
                assert_eq!(thumb(&mut rig).len(), 1);
                assert_eq!(
                    rig_calls(&rig.adb, "exec-out"),
                    first,
                    "the second preview came from the cache"
                );

                let notes = sandbox.work().join("notes.txt");
                std::fs::write(&notes, "text").unwrap();
                let events = rig.handle(Request::Preview {
                    key: "notes".into(),
                    path: notes,
                    transform: Box::new(TransformArgs::default()),
                });
                assert_eq!(events, ["no preview notes: not a media file"]);
            },
        );
    }

    fn rig_calls(adb: &FakeAdb, verb: &str) -> usize {
        adb.calls()
            .iter()
            .filter(|call| call.contains(verb))
            .count()
    }

    #[test]
    fn metrics_pushes_follow_the_toggle() {
        isolated(
            "tui::worker::tests::metrics_pushes_follow_the_toggle",
            |sandbox| {
                let mut rig = Rig::new(sandbox);
                assert_eq!(rig.handle(Request::PushMetrics(true)), ["pushing true"]);
                rig.worker.tick();
                let events: Vec<String> = rig.events.try_iter().map(|e| describe(&e)).collect();
                assert_eq!(events, ["fans Some(1280)", "sample"]);
                assert_eq!(rig.display.received("all").len(), 1);
                assert_eq!(rig.handle(Request::PushMetrics(false)), ["pushing false"]);
                rig.worker.tick();
                let events: Vec<String> = rig.events.try_iter().map(|e| describe(&e)).collect();
                assert_eq!(events, ["sample"], "the footer still gets samples");
                assert_eq!(rig.display.received("all").len(), 1, "no push while off");

                rig.worker.connection = None;
                assert_eq!(
                    rig.handle(Request::PushMetrics(true)),
                    ["error not connected"]
                );
                assert_eq!(rig.handle(Request::Brightness(1)), ["error not connected"]);
            },
        );
    }

    /// Connecting by path opens a pseudo-terminal as a serial port, which
    /// works on Linux only.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_worker_thread_connects_and_stops_when_asked() {
        isolated(
            "tui::worker::tests::the_worker_thread_connects_and_stops_when_asked",
            |sandbox| {
                let display = FakeCm01::start();
                sandbox.plug("3-12", SERIAL, "ttyACM0", display.port());
                let _adb = FakeAdb::install(sandbox, SERIAL, "3-12");
                let session = Session {
                    tty: None,
                    device: None,
                    verbose: false,
                    direct: false,
                };
                let target = crate::legacy::select(None).unwrap();
                let (requests_tx, requests) = mpsc::channel();
                let (events_tx, events) = mpsc::channel();
                let worker = Worker::spawn(
                    session,
                    Some(target),
                    Arc::new(AtomicBool::new(false)),
                    requests,
                    events_tx,
                );
                requests_tx.send(Request::Refresh).unwrap();
                let mut seen = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(10);
                while !seen.iter().any(|e: &String| e.starts_with("readback")) {
                    assert!(Instant::now() < deadline, "{seen:?}");
                    if let Ok(event) = events.recv_timeout(Duration::from_millis(100)) {
                        seen.push(describe(&event));
                    }
                }
                assert_eq!(seen[0], "via serial");
                assert_eq!(seen[1], format!("info {SERIAL}"));
                requests_tx.send(Request::Quit).unwrap();
                worker.join();
            },
        );
    }
}
