//! The thread that talks to the display. Serial commands, adb, the host
//! monitor, and the periodic metrics push all live here so frames never
//! interleave on the port.

use crate::legacy::Target;
use crate::media::connect_adb;
use crate::metrics::pc_info;
use crate::state;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tryx_legacy::adb::{Adb, DiskUsage, MediaFile};
use tryx_legacy::{Client, DeviceInfo, ScreenConfig};
use tryx_media::check::{Options, Severity};
use tryx_media::plan::Action;
use tryx_media::target::LEGACY_PANORAMA;
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
    Info(DeviceInfo),
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
        target: Target,
        verbose: bool,
        requests: Receiver<Request>,
        events: Sender<Event>,
    ) -> Worker {
        let handle = std::thread::spawn(move || {
            let mut state = WorkerState::new(target, verbose, events);
            state.run(requests);
        });
        Worker { handle }
    }

    pub fn join(self) {
        let _ = self.handle.join();
    }
}

struct WorkerState {
    target: Target,
    verbose: bool,
    events: Sender<Event>,
    client: Option<Client>,
    adb: Option<Adb>,
    monitor: Monitor,
    pushing: bool,
    last_sample: Instant,
}

impl WorkerState {
    fn new(target: Target, verbose: bool, events: Sender<Event>) -> Self {
        WorkerState {
            target,
            verbose,
            events,
            client: None,
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
                let sample = self.monitor.sample();
                if self.pushing
                    && let Some(client) = &mut self.client
                    && let Err(error) = client.send_sysinfo(&pc_info(&sample))
                {
                    self.pushing = false;
                    self.emit(Event::Pushing(false));
                    self.emit(Event::Error(format!("metrics push stopped: {error}")));
                }
                self.emit(Event::Sample(sample));
            }
        }
    }

    fn emit(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn connect(&mut self) {
        match Client::open(&self.target.tty) {
            Ok(mut client) => {
                client.set_trace(self.verbose);
                match client.handshake() {
                    Ok(info) => self.emit(Event::Info(info)),
                    Err(error) => self.emit(Event::Error(format!("handshake failed: {error}"))),
                }
                self.client = Some(client);
            }
            Err(error) => self.emit(Event::Error(format!("serial port: {error}"))),
        }
        match connect_adb(&self.target) {
            Ok((adb, _)) => self.adb = Some(adb),
            Err(failure) => self.emit(Event::Error(format!("adb: {}", failure.message))),
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
            Request::PushMetrics(enabled) => {
                self.pushing = enabled && self.client.is_some();
                self.emit(Event::Pushing(self.pushing));
                if enabled && self.client.is_none() {
                    Err("no serial connection".to_string())
                } else {
                    Ok(())
                }
            }
            Request::Quit => Ok(()),
        };
        if let Err(message) = outcome {
            self.emit(Event::Error(message));
        }
    }

    fn client(&mut self) -> Result<&mut Client, String> {
        self.client
            .as_mut()
            .ok_or_else(|| "no serial connection".to_string())
    }

    fn adb(&self) -> Result<&Adb, String> {
        self.adb
            .as_ref()
            .ok_or_else(|| "adb is not connected".to_string())
    }

    fn refresh(&mut self) -> Result<(), String> {
        let adb = self.adb()?;
        let files = adb.list_media().map_err(|e| e.to_string())?;
        let storage = adb.free_space().ok();
        self.emit(Event::Media { files, storage });
        Ok(())
    }

    fn show(&mut self, media: Vec<String>, play: String) -> Result<(), String> {
        let mut saved = state::load();
        saved.screen.media = media.clone();
        saved.screen.play_mode = play.clone();
        crate::legacy::apply_screen(self.client()?, &mut saved).map_err(|e| e.to_string())?;
        let _ = state::save(&saved);
        self.emit(Event::Log(format!("showing {} ({play})", media.join(", "))));
        Ok(())
    }

    fn delete(&mut self, name: &str) -> Result<(), String> {
        self.client()?
            .delete_media(&[name.to_string()])
            .map_err(|e| e.to_string())?;
        self.adb()?.remove(name).map_err(|e| e.to_string())?;
        self.emit(Event::Log(format!("removed {name}")));
        self.refresh()
    }

    fn brightness(&mut self, value: u8) -> Result<(), String> {
        self.client()?
            .set_brightness(value)
            .map_err(|e| e.to_string())?;
        let mut saved = state::load();
        saved.brightness = Some(value);
        let _ = state::save(&saved);
        self.emit(Event::Log(format!("brightness {value}")));
        Ok(())
    }

    fn overlay(&mut self, screen: ScreenConfig) -> Result<(), String> {
        let mut saved = state::load();
        saved.screen = screen;
        let (cpu, gpu) = crate::legacy::hardware_names(&mut saved);
        let client = self.client()?;
        client.send_spec(&cpu, &gpu).map_err(|e| e.to_string())?;
        crate::legacy::apply_screen(client, &mut saved).map_err(|e| e.to_string())?;
        if saved.screen.sysinfo_display.is_empty() {
            client.set_sysinfo_display(&[]).map_err(|e| e.to_string())?;
        }
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
        let report =
            tryx_media::check::check(path, metadata.len(), &probe, LEGACY_PANORAMA, &options);
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
        let name = plan.name.clone();
        let adb = self.adb()?;
        if adb
            .list_media()
            .map_err(|e| e.to_string())?
            .iter()
            .any(|f| f.name == name)
        {
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
            encode::verify(&ffprobe, &plan, &staged, LEGACY_PANORAMA).map_err(|e| e.to_string())?;
            staged
        };
        self.emit(Event::UploadProgress {
            name: name.clone(),
            fraction: 1.0,
        });
        let result = adb.push(&staged, &name).map_err(|e| e.to_string());
        if staged != plan.input {
            let _ = std::fs::remove_file(&staged);
        }
        result?;
        self.emit(Event::Log(format!(
            "uploaded {name} ({})",
            plan.description
        )));
        self.refresh()
    }
}
