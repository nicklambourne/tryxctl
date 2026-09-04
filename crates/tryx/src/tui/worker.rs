//! The thread that talks to the display. Serial or USB commands, adb, the
//! host monitor, and the periodic metrics push all live here so frames never
//! interleave on the link.

use crate::ipc::{self, Request as IpcRequest};
use crate::legacy::{Connection, Info, Session, Target};
use crate::media::{connect_adb, finish_stage};
use crate::metrics::pc_info;
use crate::state;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tryx_legacy::FanStatus;
use tryx_legacy::ScreenConfig;
use tryx_legacy::adb::{Adb, DiskUsage, MediaFile};
use tryx_media::check::{Options, Severity};
use tryx_media::plan::Action;
use tryx_media::{Plan, Probe, encode};
use tryx_monitor::{Monitor, Sample};

pub enum Request {
    Refresh,
    Show { media: Vec<String>, play: String },
    Delete(String),
    Brightness(u8),
    Overlay(Box<ScreenConfig>),
    Upload(PathBuf),
    PushMetrics(bool),
    Quit,
}

pub enum Event {
    Info(Info),
    Fans(FanStatus),
    Via(&'static str),
    Media {
        files: Vec<MediaFile>,
        storage: Option<DiskUsage>,
    },
    Sample(Sample),
    Pushing(bool),
    UploadProgress {
        name: String,
        fraction: f64,
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
        requests: Receiver<Request>,
        events: Sender<Event>,
    ) -> Worker {
        let handle = std::thread::spawn(move || {
            let mut state = WorkerState::new(session, target, events);
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
    events: Sender<Event>,
    connection: Option<Connection>,
    adb: Option<Adb>,
    monitor: Monitor,
    pushing: bool,
    last_sample: Instant,
}

impl WorkerState {
    fn new(session: Session, target: Option<Target>, events: Sender<Event>) -> Self {
        WorkerState {
            session,
            target,
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
            Request::Refresh => self.refresh(),
            Request::Show { media, play } => self.show(media, play),
            Request::Delete(name) => self.delete(&name),
            Request::Brightness(value) => self.brightness(value),
            Request::Overlay(screen) => self.overlay(*screen),
            Request::Upload(path) => self.upload(&path),
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

    fn show(&mut self, media: Vec<String>, play: String) -> Result<(), String> {
        let mut saved = state::load();
        saved.screen.media = media.clone();
        saved.screen.play_mode = play.clone();
        self.connection()?
            .apply(&mut saved)
            .map_err(|f| f.message)?;
        let _ = state::save(&saved);
        self.emit(Event::Log(format!("showing {} ({play})", media.join(", "))));
        Ok(())
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

    fn upload(&mut self, path: &PathBuf) -> Result<(), String> {
        let (ffmpeg, ffprobe) = encode::tools().map_err(|e| e.to_string())?;
        let metadata = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let probe = Probe::read(&ffprobe, path).map_err(|e| e.to_string())?;
        let options = Options::default();
        let target = self.connection()?.media_target();
        let report = tryx_media::check::check(path, metadata.len(), &probe, target, &options);
        if !report.acceptable(false) {
            let reason = report
                .findings
                .iter()
                .find(|f| f.severity == Severity::Fatal)
                .map(|f| f.message.clone())
                .unwrap_or_else(|| "rejected".to_string());
            return Err(format!("{}: {reason}", path.display()));
        }
        let plan = Plan::from_report(&report, &options).ok_or("nothing to upload")?;
        let name = self.connection()?.remote_name(&plan.name);
        if self.list()?.0.iter().any(|f| f.name == name) {
            return Err(format!("{name} already exists on the display"));
        }
        let staged = if plan.action == Action::Passthrough {
            plan.input.clone()
        } else {
            let staged =
                std::env::temp_dir().join(format!("tryx-tui-{}-{}", std::process::id(), name));
            let events = self.events.clone();
            let progress_name = name.clone();
            encode::run(
                &ffmpeg,
                &plan,
                &staged,
                report.source.duration,
                |progress| {
                    if let Some(fraction) = progress.fraction {
                        let _ = events.send(Event::UploadProgress {
                            name: progress_name.clone(),
                            fraction,
                        });
                    }
                },
            )
            .map_err(|e| e.to_string())?;
            finish_stage(&ffprobe, &plan, &staged).map_err(|f| f.message)?;
            staged
        };
        self.emit(Event::UploadProgress {
            name: name.clone(),
            fraction: 1.0,
        });
        let result = if self.target.is_some() {
            self.adb()?.push(&staged, &name).map_err(|e| e.to_string())
        } else {
            let events = self.events.clone();
            let progress_name = name.clone();
            self.connection()?
                .upload(&staged, &name, |sent, total| {
                    if total > 0 {
                        let _ = events.send(Event::UploadProgress {
                            name: progress_name.clone(),
                            fraction: sent as f64 / total as f64,
                        });
                    }
                })
                .map_err(|f| f.message)
        };
        if staged != plan.input {
            let _ = std::fs::remove_file(&staged);
        }
        result?;
        self.emit(Event::UploadProgress {
            name: name.clone(),
            fraction: 1.0,
        });
        self.emit(Event::Log(format!(
            "uploaded {name} ({})",
            plan.description
        )));
        self.refresh()
    }
}
