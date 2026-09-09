//! The thread that talks to the display. Serial or USB commands, adb, the
//! host monitor, and the periodic metrics push all live here so frames never
//! interleave on the link.

use crate::ipc::{self, Request as IpcRequest};
use crate::legacy::{Connection, Info, Readback, Session, Target};
use crate::media::{self, TransformArgs, connect_adb, finish_stage};
use crate::metrics::pc_info;
use crate::ops::{self, Outcome, Pending, Record};
use crate::state;
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
    Upload {
        path: PathBuf,
        transform: Box<TransformArgs>,
    },
    Retry(String),
    ClearCache,
    PushMetrics(bool),
    Quit,
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
        image: Box<DynamicImage>,
    },
    PreviewFailed {
        key: String,
        reason: String,
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
                    Ok(image) => self.emit(Event::Preview {
                        key,
                        image: Box::new(image),
                    }),
                    Err(reason) => self.emit(Event::PreviewFailed { key, reason }),
                }
                Ok(())
            }
            Request::Preview {
                key,
                path,
                transform,
            } => {
                match self.preview(&path, &transform) {
                    Ok(image) => self.emit(Event::Preview {
                        key,
                        image: Box::new(image),
                    }),
                    Err(reason) => self.emit(Event::PreviewFailed { key, reason }),
                }
                Ok(())
            }
            Request::Upload { path, transform } => self.upload(&path, &transform, None),
            Request::Retry(id) => self.retry(&id),
            Request::ClearCache => {
                let _ = crate::ops::clear(true, false);
                self.operations();
                self.emit(Event::Log("kept encodes removed".to_string()));
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
        self.adb()?.pull(name, &path).map_err(|e| e.to_string())?;
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

    fn thumbnail(&mut self, name: &str, size: u64) -> Result<DynamicImage, String> {
        if self.target.is_none() {
            return Err("no preview on this firmware: media pull is not implemented".to_string());
        }
        let (ffmpeg, _) = encode::tools().map_err(|e| e.to_string())?;
        let adb = self.adb()?;
        pictures::device_thumbnail(&ffmpeg, adb, name, size)
    }

    fn preview(&mut self, path: &Path, transform: &TransformArgs) -> Result<DynamicImage, String> {
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
        pictures::local_preview(&ffmpeg, path, kind, &options.transform, target, at)
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
