//! Screen state, key handling, and drawing.

use super::worker::{DeviceRow, Event, Request};
use crate::legacy::{Info, Readback};
use crate::media::TransformArgs;
use crate::metrics::LABELS;
use crate::ops::{Outcome, Record};
use crate::{output, state};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::DynamicImage;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Gauge, List, ListItem, ListState, Paragraph, Row, Table, Tabs, Wrap,
};
use ratatui_image::StatefulImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use tryx_legacy::adb::{DiskUsage, MediaFile};
use tryx_legacy::commands::{PRESETS, SCREEN_FULL, SCREEN_SPLITTING, preset_number};
use tryx_legacy::{FanStatus, ScreenConfig};
use tryx_monitor::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Devices,
    Library,
    Overlay,
    Display,
    Operations,
}

impl Tab {
    const ALL: [Tab; 5] = [
        Tab::Devices,
        Tab::Library,
        Tab::Overlay,
        Tab::Display,
        Tab::Operations,
    ];

    fn title(self) -> &'static str {
        match self {
            Tab::Devices => "Devices",
            Tab::Library => "Library",
            Tab::Overlay => "Overlay",
            Tab::Display => "Display",
            Tab::Operations => "Operations",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    UploadPath(String),
    ConfirmDelete(String),
}

/// The upload wizard: a file, the transform chosen for it, and what the
/// analysis says about that choice.
#[derive(Debug, Clone)]
struct Wizard {
    path: PathBuf,
    transform: TransformArgs,
    lines: Vec<String>,
    acceptable: bool,
}

/// A row of the Library: the six built-in animations, then the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Preset(u8),
    File(usize),
}

const POSITIONS: [&str; 3] = ["Top", "Center", "Bottom"];
const ALIGNMENTS: [&str; 3] = ["Left", "Center", "Right"];
const MODES: [&str; 4] = ["fit", "fill", "crop", "stretch"];
const ROTATIONS: [u32; 4] = [0, 90, 180, 270];
const DEGREES: [u16; 4] = [0, 90, 180, 270];
/// Pictures kept ready to draw.
const PREVIEW_CACHE: usize = 12;

fn protocol_name(protocol: ProtocolType) -> &'static str {
    match protocol {
        ProtocolType::Kitty => "kitty graphics",
        ProtocolType::Iterm2 => "iTerm2 images",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Halfblocks => "half-blocks",
    }
}

pub struct App {
    requests: Sender<Request>,
    cancel: Arc<AtomicBool>,
    tab: Tab,
    info: Option<Info>,
    files: Vec<MediaFile>,
    storage: Option<DiskUsage>,
    devices: Vec<DeviceRow>,
    list: ListState,
    sample: Option<Sample>,
    fans: FanStatus,
    via: &'static str,
    screen: ScreenConfig,
    rotation: u16,
    label_cursor: usize,
    brightness: u8,
    pushing: bool,
    upload: Option<(String, f64)>,
    prompt: Option<Prompt>,
    wizard: Option<Wizard>,
    picker: Picker,
    previews: HashMap<String, StatefulProtocol>,
    preview_order: VecDeque<String>,
    preview_failures: HashMap<String, String>,
    preview_requested: HashSet<String>,
    readback: Option<Readback>,
    operations: Vec<Record>,
    op_list: ListState,
    status: String,
    error: Option<String>,
}

impl App {
    pub fn new(requests: Sender<Request>, cancel: Arc<AtomicBool>, picker: Picker) -> Self {
        let saved = state::load();
        let mut list = ListState::default();
        list.select(Some(0));
        App {
            requests,
            cancel,
            tab: Tab::Library,
            info: None,
            files: Vec::new(),
            storage: None,
            devices: Vec::new(),
            list,
            sample: None,
            fans: FanStatus::default(),
            via: "connecting",
            screen: saved.screen,
            rotation: saved.rotation.unwrap_or(0),
            label_cursor: 0,
            brightness: saved.brightness.unwrap_or(75),
            pushing: false,
            upload: None,
            prompt: None,
            wizard: None,
            picker,
            previews: HashMap::new(),
            preview_order: VecDeque::new(),
            preview_failures: HashMap::new(),
            preview_requested: HashSet::new(),
            readback: None,
            operations: Vec::new(),
            op_list: ListState::default(),
            status: "connecting…".to_string(),
            error: None,
        }
    }

    fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }

    fn store_preview(&mut self, key: String, image: DynamicImage) {
        let protocol = self.picker.new_resize_protocol(image);
        if self.previews.insert(key.clone(), protocol).is_none() {
            self.preview_order.push_back(key);
        }
        while self.preview_order.len() > PREVIEW_CACHE {
            if let Some(old) = self.preview_order.pop_front() {
                self.previews.remove(&old);
                self.preview_requested.remove(&old);
            }
        }
    }

    /// Asks the worker for a picture once; later renders find it ready.
    fn ensure_preview(&mut self, key: &str, request: impl FnOnce() -> Request) {
        if self.previews.contains_key(key)
            || self.preview_failures.contains_key(key)
            || self.preview_requested.contains(key)
        {
            return;
        }
        self.preview_requested.insert(key.to_string());
        self.send(request());
    }

    fn wizard_key(wizard: &Wizard) -> String {
        format!(
            "wizard:{}|{}",
            wizard.path.display(),
            serde_json::to_string(&wizard.transform).unwrap_or_default()
        )
    }

    /// Draws the picture for `key`, or says why there is none.
    fn render_preview(&mut self, frame: &mut Frame, area: Rect, key: &str, waiting: &str) {
        let title = format!(" Preview · {} ", protocol_name(self.picker.protocol_type()));
        let block = Block::bordered().title(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(protocol) = self.previews.get_mut(key) {
            frame.render_stateful_widget(StatefulImage::default(), inner, protocol);
            return;
        }
        let text = match self.preview_failures.get(key) {
            Some(reason) => reason.clone(),
            None => waiting.to_string(),
        };
        frame.render_widget(
            Paragraph::new(Line::from(text).dim()).wrap(Wrap { trim: true }),
            inner,
        );
    }

    pub fn handle_event(&mut self, event: Event) {
        match event {
            Event::Info(info) => {
                self.status = format!("connected to {}", info.short());
                self.info = Some(info);
            }
            Event::Media { files, storage } => {
                self.files = files;
                self.storage = storage;
                let selected = self.list.selected().unwrap_or(0);
                let count = self.entries().len();
                self.list
                    .select(Some(selected.min(count.saturating_sub(1))));
            }
            Event::Devices(devices) => self.devices = devices,
            Event::Sample(sample) => self.sample = Some(sample),
            Event::Fans(fans) => self.fans = fans,
            Event::Via(via) => self.via = via,
            Event::Pushing(pushing) => self.pushing = pushing,
            Event::UploadProgress { name, fraction } => {
                self.upload = if fraction >= 1.0 {
                    None
                } else {
                    Some((name, fraction))
                };
            }
            Event::Analysis {
                path,
                lines,
                acceptable,
            } => {
                if let Some(wizard) = &mut self.wizard
                    && wizard.path == path
                {
                    wizard.lines = lines;
                    wizard.acceptable = acceptable;
                }
            }
            Event::Readback(readback) => self.readback = Some(*readback),
            Event::Preview { key, image } => self.store_preview(key, *image),
            Event::PreviewFailed { key, reason } => {
                self.preview_failures.insert(key, reason);
            }
            Event::Operations(records) => {
                self.operations = records;
                let count = self.operations.len();
                self.op_list.select(if count == 0 {
                    None
                } else {
                    Some(self.op_list.selected().unwrap_or(count - 1).min(count - 1))
                });
            }
            Event::Log(message) => {
                self.error = None;
                self.status = message;
            }
            Event::Error(message) => {
                self.upload = None;
                self.error = Some(message);
            }
        }
    }

    /// Returns true when the interface should exit.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if let Some(prompt) = self.prompt.clone() {
            self.handle_prompt_key(prompt, key);
            return false;
        }
        if self.wizard.is_some() {
            self.handle_wizard_key(key);
            return false;
        }
        if self.upload.is_some() && key.code == KeyCode::Char('x') {
            self.cancel.store(true, Ordering::Relaxed);
            self.status = "cancelling the encode…".to_string();
            return false;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab => self.tab = next(&Tab::ALL, self.tab, 1),
            KeyCode::BackTab => self.tab = next(&Tab::ALL, self.tab, -1),
            KeyCode::Char('1') => self.tab = Tab::Devices,
            KeyCode::Char('2') => self.tab = Tab::Library,
            KeyCode::Char('3') => self.tab = Tab::Overlay,
            KeyCode::Char('4') => self.tab = Tab::Display,
            KeyCode::Char('5') => self.tab = Tab::Operations,
            KeyCode::Char('r') => self.send(Request::Refresh),
            KeyCode::Char('m') => self.send(Request::PushMetrics(!self.pushing)),
            _ => match self.tab {
                Tab::Devices => {}
                Tab::Library => self.handle_library_key(key),
                Tab::Overlay => self.handle_overlay_key(key),
                Tab::Display => self.handle_display_key(key),
                Tab::Operations => self.handle_operations_key(key),
            },
        }
        false
    }

    fn handle_prompt_key(&mut self, prompt: Prompt, key: KeyEvent) {
        match prompt {
            Prompt::UploadPath(mut text) => match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    self.prompt = None;
                    let path = PathBuf::from(text.trim());
                    if path.as_os_str().is_empty() {
                        return;
                    }
                    self.open_wizard(path);
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.prompt = Some(Prompt::UploadPath(text));
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    self.prompt = Some(Prompt::UploadPath(text));
                }
                _ => self.prompt = Some(Prompt::UploadPath(text)),
            },
            Prompt::ConfirmDelete(name) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.prompt = None;
                    self.send(Request::Delete(name));
                }
                _ => self.prompt = None,
            },
        }
    }

    fn open_wizard(&mut self, path: PathBuf) {
        let wizard = Wizard {
            path: path.clone(),
            transform: TransformArgs::default(),
            lines: vec!["analysing…".to_string()],
            acceptable: false,
        };
        self.send(Request::Analyse {
            path,
            transform: Box::new(wizard.transform.clone()),
        });
        self.wizard = Some(wizard);
        self.request_wizard_preview();
    }

    fn request_wizard_preview(&mut self) {
        let Some(wizard) = &self.wizard else {
            return;
        };
        let key = Self::wizard_key(wizard);
        let (path, transform) = (wizard.path.clone(), wizard.transform.clone());
        let request_key = key.clone();
        self.ensure_preview(&key, move || Request::Preview {
            key: request_key,
            path,
            transform: Box::new(transform),
        });
    }

    fn handle_wizard_key(&mut self, key: KeyEvent) {
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let mut changed = true;
        match key.code {
            KeyCode::Esc => {
                self.wizard = None;
                return;
            }
            KeyCode::Char('m') => {
                let current = wizard.transform.mode.as_deref().unwrap_or("fit");
                wizard.transform.mode = Some(next(&MODES, current, 1).to_string());
            }
            KeyCode::Char('r') => {
                wizard.transform.rotate = next(&ROTATIONS, wizard.transform.rotate, 1);
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                let zoom = wizard.transform.zoom.unwrap_or(100);
                wizard.transform.zoom = Some((zoom + 25).min(400));
            }
            KeyCode::Char('-') => {
                let zoom = wizard.transform.zoom.unwrap_or(100);
                wizard.transform.zoom = Some(zoom.saturating_sub(25).max(100));
            }
            KeyCode::Enter => {
                if !wizard.acceptable {
                    self.error = Some("the file cannot be prepared as it is".to_string());
                    return;
                }
                let (path, transform) = (wizard.path.clone(), wizard.transform.clone());
                self.wizard = None;
                self.status = format!("uploading {}…", path.display());
                self.send(Request::Upload {
                    path,
                    transform: Box::new(transform),
                });
                return;
            }
            _ => changed = false,
        }
        if changed {
            wizard.lines = vec!["analysing…".to_string()];
            let (path, transform) = (wizard.path.clone(), wizard.transform.clone());
            self.send(Request::Analyse {
                path,
                transform: Box::new(transform),
            });
            self.request_wizard_preview();
        }
    }

    fn entries(&self) -> Vec<Entry> {
        (1..=PRESETS.len() as u8)
            .map(Entry::Preset)
            .chain((0..self.files.len()).map(Entry::File))
            .collect()
    }

    fn selected_entry(&self) -> Option<Entry> {
        self.list
            .selected()
            .and_then(|index| self.entries().get(index).copied())
    }

    fn selected_file(&self) -> Option<&MediaFile> {
        match self.selected_entry()? {
            Entry::File(index) => self.files.get(index),
            Entry::Preset(_) => None,
        }
    }

    fn handle_library_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => match self.selected_entry() {
                Some(Entry::Preset(number)) => {
                    self.screen.preset_id = PRESETS[usize::from(number - 1)].to_string();
                    self.send(Request::Show {
                        media: vec![format!("preset:{number}")],
                        play: "Single".to_string(),
                    });
                }
                Some(Entry::File(index)) => {
                    let name = self.files[index].name.clone();
                    self.screen.preset_id.clear();
                    self.screen.media = vec![name.clone()];
                    self.send(Request::Show {
                        media: vec![name],
                        play: "Single".to_string(),
                    });
                }
                None => {}
            },
            KeyCode::Char('l') => {
                let names: Vec<String> = self.files.iter().map(|f| f.name.clone()).collect();
                if !names.is_empty() {
                    self.screen.preset_id.clear();
                    self.screen.media = names.clone();
                    self.send(Request::Show {
                        media: names,
                        play: "Loop".to_string(),
                    });
                }
            }
            KeyCode::Char('d') => {
                if let Some(name) = self.selected_file().map(|file| file.name.clone()) {
                    self.prompt = Some(Prompt::ConfirmDelete(name));
                }
            }
            KeyCode::Char('e') => {
                if let Some(name) = self.selected_file().map(|file| file.name.clone()) {
                    self.status = format!("exporting {name}…");
                    self.send(Request::Export(name));
                }
            }
            KeyCode::Char('u') => self.prompt = Some(Prompt::UploadPath(String::new())),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.entries().len();
        if count == 0 {
            return;
        }
        let current = self.list.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(count as isize) as usize;
        self.list.select(Some(next));
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.label_cursor = (self.label_cursor + 1) % LABELS.len()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.label_cursor = (self.label_cursor + LABELS.len() - 1) % LABELS.len()
            }
            KeyCode::Char(' ') => {
                let label = LABELS[self.label_cursor].0.to_string();
                if let Some(index) = self.screen.sysinfo_display.iter().position(|l| *l == label) {
                    self.screen.sysinfo_display.remove(index);
                } else if self.screen.sysinfo_display.len() < crate::metrics::MAX_LABELS {
                    self.screen.sysinfo_display.push(label);
                } else {
                    self.error = Some(format!("at most {} metrics", crate::metrics::MAX_LABELS));
                }
            }
            KeyCode::Char('p') => {
                self.screen.settings.position =
                    next(&POSITIONS, self.screen.settings.position.as_str(), 1).to_string()
            }
            KeyCode::Char('a') => {
                self.screen.settings.align =
                    next(&ALIGNMENTS, self.screen.settings.align.as_str(), 1).to_string()
            }
            KeyCode::Char('c') => toggle_badge(&mut self.screen.settings.badges, "CPU Badge"),
            KeyCode::Char('g') => toggle_badge(&mut self.screen.settings.badges, "GPU Badge"),
            KeyCode::Char('x') => self.screen.sysinfo_display.clear(),
            KeyCode::Enter => {
                if self.screen.media.is_empty() && self.screen.preset_id.is_empty() {
                    self.error = Some("pick media in the Library first".to_string());
                } else {
                    self.send(Request::Overlay(Box::new(self.screen.clone())));
                }
            }
            _ => {}
        }
    }

    fn handle_display_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                self.brightness = self.brightness.saturating_sub(5)
            }
            KeyCode::Right | KeyCode::Char('l') => self.brightness = (self.brightness + 5).min(100),
            KeyCode::Char('s') => {
                self.screen.screen_mode = if self.screen.screen_mode == SCREEN_SPLITTING {
                    SCREEN_FULL.to_string()
                } else {
                    SCREEN_SPLITTING.to_string()
                }
            }
            KeyCode::Char('w') => self.screen.waterfall_mode = !self.screen.waterfall_mode,
            KeyCode::Char('o') => self.rotation = next(&DEGREES, self.rotation, 1),
            KeyCode::Char('g') => self.send(Request::Readback),
            KeyCode::Enter => {
                self.send(Request::Brightness(self.brightness));
                if self.screen.media.is_empty() && self.screen.preset_id.is_empty() {
                    self.error = Some("pick media in the Library first".to_string());
                } else {
                    self.send(Request::Layout {
                        screen: Box::new(self.screen.clone()),
                        rotation: Some(self.rotation),
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_operations_key(&mut self, key: KeyEvent) {
        let count = self.operations.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') if count > 0 => {
                let current = self.op_list.selected().unwrap_or(0);
                self.op_list.select(Some((current + 1) % count));
            }
            KeyCode::Up | KeyCode::Char('k') if count > 0 => {
                let current = self.op_list.selected().unwrap_or(0);
                self.op_list.select(Some((current + count - 1) % count));
            }
            KeyCode::Enter => {
                if let Some(record) = self
                    .op_list
                    .selected()
                    .and_then(|index| self.operations.get(index))
                {
                    if record.outcome == Outcome::Failed {
                        self.status = format!("retrying {}…", record.remote);
                        self.send(Request::Retry(record.id.clone()));
                    } else {
                        self.error = Some("only failed transfers can be retried".to_string());
                    }
                }
            }
            KeyCode::Char('c') => self.send(Request::ClearCache),
            _ => {}
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [header, tabs, body, footer] = Layout::vertical([
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(4),
        ])
        .areas(frame.area());
        self.render_header(frame, header);
        let titles: Vec<Line> = Tab::ALL
            .iter()
            .enumerate()
            .map(|(i, tab)| Line::from(format!(" {} {} ", i + 1, tab.title())))
            .collect();
        let selected = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        frame.render_widget(
            Tabs::new(titles)
                .select(selected)
                .highlight_style(Style::new().bold().fg(Color::Cyan)),
            tabs,
        );
        if self.wizard.is_some() {
            self.render_wizard(frame, body);
        } else {
            match self.tab {
                Tab::Devices => self.render_devices(frame, body),
                Tab::Library => self.render_library(frame, body),
                Tab::Overlay => self.render_overlay(frame, body),
                Tab::Display => self.render_display(frame, body),
                Tab::Operations => self.render_operations(frame, body),
            }
        }
        self.render_footer(frame, footer);
    }

    fn showing(&self) -> String {
        if preset_number(&self.screen.preset_id).is_some() {
            self.screen.preset_id.clone()
        } else if self.screen.media.is_empty() {
            "nothing selected".to_string()
        } else {
            self.screen.media.join(", ")
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let device = match &self.info {
            Some(info) => info.summary(),
            None => "not connected".to_string(),
        };
        let storage = match &self.storage {
            Some(usage) => format!(
                "{} free of {}",
                output::human_bytes(usage.available_kib * 1024),
                output::human_bytes(usage.total_kib * 1024)
            ),
            None => "storage unknown".to_string(),
        };
        let text = vec![
            Line::from(vec![Span::raw("Display  ").dim(), Span::raw(device)]),
            Line::from(vec![
                Span::raw("Showing  ").dim(),
                Span::raw(self.showing()),
                Span::raw("   ").dim(),
                Span::raw(storage).dim(),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" tryxctl ")),
            area,
        );
    }

    fn render_devices(&self, frame: &mut Frame, area: Rect) {
        let rows: Vec<Row> = self
            .devices
            .iter()
            .map(|device| {
                Row::new(vec![
                    device.id.clone(),
                    device.product.clone(),
                    device.usb_id.clone(),
                    device.serial.clone(),
                    device.access.clone(),
                    device.protocol.clone(),
                ])
            })
            .collect();
        let title = if self.devices.is_empty() {
            " Devices (none found · r rescans) "
        } else {
            " Devices · r rescans "
        };
        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(14),
                Constraint::Length(10),
                Constraint::Length(20),
                Constraint::Length(18),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(vec![
                "ID", "PRODUCT", "USB ID", "SERIAL", "ACCESS", "PROTOCOL",
            ])
            .dim(),
        )
        .block(Block::bordered().title(title));
        frame.render_widget(table, area);
    }

    fn render_library(&mut self, frame: &mut Frame, area: Rect) {
        let [list_area, preview_area] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .areas(area);
        let selected = self.selected_entry();
        if let Some(Entry::File(index)) = selected
            && let Some(file) = self.files.get(index)
        {
            let (name, size) = (file.name.clone(), file.size);
            let key = format!("thumb:{name}");
            let request_name = name.clone();
            self.ensure_preview(&key, move || Request::Thumbnail {
                name: request_name,
                size,
            });
        }
        let items: Vec<ListItem> = self
            .entries()
            .into_iter()
            .map(|entry| match entry {
                Entry::Preset(number) => {
                    let id = PRESETS[usize::from(number - 1)];
                    let showing = self.screen.preset_id == id;
                    let marker = if showing { "▶ " } else { "  " };
                    ListItem::new(Line::from(vec![
                        Span::raw(marker).fg(Color::Green),
                        Span::raw(format!("{:<44}", format!("preset:{number}  {id}"))),
                        Span::raw("built-in").dim(),
                    ]))
                }
                Entry::File(index) => {
                    let file = &self.files[index];
                    let showing =
                        self.screen.preset_id.is_empty() && self.screen.media.contains(&file.name);
                    let marker = if showing { "▶ " } else { "  " };
                    ListItem::new(Line::from(vec![
                        Span::raw(marker).fg(Color::Green),
                        Span::raw(format!("{:<44}", file.name)),
                        Span::raw(output::human_bytes(file.size)).dim(),
                    ]))
                }
            })
            .collect();
        let title = " Library · Enter show · l loop files · d delete · e export · u upload ";
        let list = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, list_area, &mut self.list);
        match selected {
            Some(Entry::File(index)) => {
                let key = format!("thumb:{}", self.files[index].name);
                self.render_preview(
                    frame,
                    preview_area,
                    &key,
                    "generating the preview… (a file without a front index is pulled once)",
                );
            }
            Some(Entry::Preset(_)) => {
                let block = Block::bordered().title(" Preview ");
                let inner = block.inner(preview_area);
                frame.render_widget(block, preview_area);
                frame.render_widget(
                    Paragraph::new(Line::from("a built-in animation; no preview").dim()),
                    inner,
                );
            }
            None => {}
        }
    }

    fn render_wizard(&mut self, frame: &mut Frame, area: Rect) {
        let Some(wizard) = self.wizard.clone() else {
            return;
        };
        let [findings_area, right_area] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(area);
        let [settings_area, preview_area] =
            Layout::vertical([Constraint::Length(7), Constraint::Min(4)]).areas(right_area);
        let lines: Vec<Line> = wizard
            .lines
            .iter()
            .map(|line| {
                let styled = Line::from(line.clone());
                if line.starts_with("[FAIL]") {
                    styled.fg(Color::Red)
                } else if line.starts_with("[ask ]") {
                    styled.fg(Color::Yellow)
                } else {
                    styled
                }
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(format!(" {} ", wizard.path.display()))),
            findings_area,
        );
        let transform = &wizard.transform;
        let text = vec![
            Line::from(format!(
                "Mode       {}   (m)",
                transform.mode.as_deref().unwrap_or("fit")
            )),
            Line::from(format!("Rotate     {}°   (r)", transform.rotate)),
            Line::from(format!(
                "Zoom       {}%   (+/-, crop mode)",
                transform.zoom.unwrap_or(100)
            )),
            Line::from(""),
            Line::from("Enter uploads with these settings; Esc goes back.").dim(),
        ];
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Transform ")),
            settings_area,
        );
        let key = Self::wizard_key(&wizard);
        self.render_preview(frame, preview_area, &key, "rendering the preview…");
    }

    fn render_overlay(&self, frame: &mut Frame, area: Rect) {
        let [labels_area, settings_area] =
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                .areas(area);
        let items: Vec<ListItem> = LABELS
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let chosen = self.screen.sysinfo_display.iter().any(|l| l == label);
                let cursor = if index == self.label_cursor {
                    "›"
                } else {
                    " "
                };
                let mark = if chosen { "[x]" } else { "[ ]" };
                let line = Line::from(format!("{cursor} {mark} {label}"));
                ListItem::new(if chosen { line.fg(Color::Green) } else { line })
            })
            .collect();
        frame.render_widget(
            List::new(items)
                .block(Block::bordered().title(" Metrics · Space toggles (max 3) · x clears ")),
            labels_area,
        );
        let badges = if self.screen.settings.badges.is_empty() {
            "none".to_string()
        } else {
            self.screen.settings.badges.join(", ")
        };
        let text = vec![
            Line::from(format!(
                "Position   {}   (p)",
                self.screen.settings.position
            )),
            Line::from(format!("Align      {}   (a)", self.screen.settings.align)),
            Line::from(format!("Colour     {}", self.screen.settings.color)),
            Line::from(format!("Badges     {badges}   (c cpu, g gpu)")),
            Line::from(""),
            Line::from("Enter applies the overlay to the display.").dim(),
            Line::from(format!(
                "Live values are sent while metrics push is on (m): {}",
                if self.pushing { "on" } else { "off" }
            ))
            .dim(),
        ];
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Layout ")),
            settings_area,
        );
    }

    fn render_display(&self, frame: &mut Frame, area: Rect) {
        let [gauge_area, rest] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let [layout_area, readback_area] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(rest);
        frame.render_widget(
            Gauge::default()
                .block(Block::bordered().title(" Brightness · ←/→ adjust · Enter applies all "))
                .gauge_style(Style::new().fg(Color::Yellow))
                .ratio(f64::from(self.brightness) / 100.0)
                .label(format!("{}%", self.brightness)),
            gauge_area,
        );
        let layout = vec![
            Line::from(format!(
                "Screen     {}   (s)",
                if self.screen.screen_mode == SCREEN_SPLITTING {
                    "split"
                } else {
                    "full"
                }
            )),
            Line::from(format!(
                "Waterfall  {}   (w)",
                if self.screen.waterfall_mode {
                    "on"
                } else {
                    "off"
                }
            )),
            Line::from(format!("Rotation   {}°   (o)", self.rotation)),
            Line::from(""),
            Line::from("Enter applies brightness and layout.").dim(),
            Line::from("g reads the display back.").dim(),
        ];
        frame.render_widget(
            Paragraph::new(layout).block(Block::bordered().title(" Layout ")),
            layout_area,
        );
        let readback: Vec<Line> = match &self.readback {
            Some(readback) => {
                let mut lines = vec![
                    Line::from(format!(
                        "Source     {}",
                        if readback.source == "device" {
                            "read from the display"
                        } else {
                            "last applied (firmware answers no queries)"
                        }
                    )),
                    Line::from(format!(
                        "Showing    {}",
                        readback
                            .preset
                            .clone()
                            .unwrap_or_else(|| readback.media.join(", "))
                    )),
                    Line::from(format!(
                        "Layout     {}, waterfall {}, rotation {}",
                        readback.screen_mode.to_lowercase(),
                        if readback.waterfall { "on" } else { "off" },
                        readback
                            .rotation
                            .map(|d| format!("{d}°"))
                            .unwrap_or_else(|| "unset".into())
                    )),
                    Line::from(format!(
                        "Brightness {}",
                        readback
                            .brightness
                            .map(|b| format!("{b}%"))
                            .unwrap_or_else(|| "unset".into())
                    )),
                    Line::from(format!("Overlay    {}", readback.overlay.join(", "))),
                ];
                if !readback.filter.is_empty() {
                    lines.push(Line::from(format!(
                        "Filter     {} at {}%",
                        readback.filter.to_lowercase(),
                        readback.filter_opacity
                    )));
                }
                if let Some(bytes) = readback.fans.available_storage {
                    lines.push(Line::from(format!(
                        "Storage    {} free",
                        output::human_bytes(bytes)
                    )));
                }
                lines
            }
            None => vec![Line::from("g reads the display back.").dim()],
        };
        frame.render_widget(
            Paragraph::new(readback)
                .wrap(Wrap { trim: true })
                .block(Block::bordered().title(" Display ")),
            readback_area,
        );
    }

    fn render_operations(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .operations
            .iter()
            .map(|record| {
                let outcome = match record.outcome {
                    Outcome::Ok => "ok".to_string(),
                    Outcome::Failed => "failed".to_string(),
                    Outcome::Running => "running".to_string(),
                };
                let detail = match record.outcome {
                    Outcome::Ok => record.size.map(output::human_bytes).unwrap_or_default(),
                    Outcome::Failed => record.error.clone().unwrap_or_default(),
                    Outcome::Running => String::new(),
                };
                let kept = if record.cached.as_ref().is_some_and(|p| p.is_file()) {
                    " (encode kept)"
                } else {
                    ""
                };
                let line = Line::from(format!(
                    "{:<8} {:<26} {:<7} {detail}{kept}",
                    record.kind, record.remote, outcome
                ));
                ListItem::new(match record.outcome {
                    Outcome::Failed => line.fg(Color::Red),
                    Outcome::Ok => line,
                    Outcome::Running => line.fg(Color::Yellow),
                })
            })
            .collect();
        let title = if self.operations.is_empty() {
            " Operations (no transfers yet) "
        } else {
            " Operations · Enter retries a failed transfer · c drops kept encodes "
        };
        let list = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.op_list);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let fan = match (self.fans.lcd_fan_rpm, self.fans.pump_rpm) {
            (Some(fan), Some(pump)) => format!(" · fan {fan} rpm · pump {pump} rpm"),
            (Some(fan), None) => format!(" · fan {fan} rpm"),
            (None, Some(pump)) => format!(" · pump {pump} rpm"),
            (None, None) => String::new(),
        };
        let push = match (self.via, self.pushing) {
            ("daemon", _) => "push via daemon".to_string(),
            (_, true) => "push on (m)".to_string(),
            (_, false) => "push off (m)".to_string(),
        };
        let metrics = match &self.sample {
            Some(sample) => format!(
                "cpu {} {} · gpu {} {} · mem {}{fan}   {push}",
                fmt(sample.cpu.temperature_c, "°C"),
                fmt(sample.cpu.usage_percent, "%"),
                fmt(sample.gpu.temperature_c, "°C"),
                fmt(sample.gpu.usage_percent, "%"),
                fmt(sample.memory.usage_percent, "%"),
            ),
            None => format!("host metrics unavailable   {push}"),
        };
        let second = match (&self.prompt, &self.upload, &self.error) {
            (Some(Prompt::UploadPath(text)), _, _) => Line::from(format!(
                "Upload path: {text}▏  (Enter to continue, Esc to cancel)"
            ))
            .fg(Color::Cyan),
            (Some(Prompt::ConfirmDelete(name)), _, _) => {
                Line::from(format!("Delete {name} from the display? y/n")).fg(Color::Yellow)
            }
            (None, Some((name, fraction)), _) => Line::from(format!(
                "encoding {name} {:>3}%   (x cancels)",
                (fraction * 100.0) as u32
            ))
            .fg(Color::Cyan),
            (None, None, Some(error)) => Line::from(format!("error: {error}")).fg(Color::Red),
            (None, None, None) if self.wizard.is_some() => {
                Line::from("m mode · r rotate · +/- zoom · Enter upload · Esc back").dim()
            }
            (None, None, None) => Line::from(format!(
                "{}   q quits · Tab switches · r refreshes",
                self.status
            ))
            .dim(),
        };
        frame.render_widget(
            Paragraph::new(vec![Line::from(metrics), second]).block(Block::bordered()),
            area,
        );
    }
}

fn fmt(value: Option<f64>, unit: &str) -> String {
    value
        .map(|v| format!("{v:.0}{unit}"))
        .unwrap_or_else(|| "n/a".into())
}

fn next<T: PartialEq + Copy>(items: &[T], current: T, delta: isize) -> T {
    let index = items.iter().position(|item| *item == current).unwrap_or(0) as isize;
    items[(index + delta).rem_euclid(items.len() as isize) as usize]
}

fn toggle_badge(badges: &mut Vec<String>, badge: &str) {
    if let Some(index) = badges.iter().position(|b| b == badge) {
        badges.remove(index);
    } else {
        badges.push(badge.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::mpsc;

    fn rendered(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn app_with_files() -> (App, mpsc::Receiver<Request>) {
        let (tx, rx) = mpsc::channel();
        let mut app = App::new(
            tx,
            Arc::new(AtomicBool::new(false)),
            Picker::from_fontsize((8, 16)),
        );
        app.handle_event(Event::Info(Info::Legacy(tryx_legacy::DeviceInfo {
            product_id: "cm01".into(),
            os: "Android".into(),
            serial: "XYZ1".into(),
            app_version: "1.0".into(),
            firmware: "V1.0.3".into(),
            hardware: "V1.1".into(),
            attributes: vec![],
        })));
        app.handle_event(Event::Media {
            files: vec![
                MediaFile {
                    name: "vendor.mp4".into(),
                    size: 303_549_636,
                },
                MediaFile {
                    name: "clip.mp4".into(),
                    size: 4_500_000,
                },
            ],
            storage: Some(DiskUsage {
                total_kib: 3_840_000,
                used_kib: 612_480,
                available_kib: 3_096_448,
            }),
        });
        (app, rx)
    }

    #[test]
    fn library_renders_presets_files_storage_and_device() {
        let (mut app, _rx) = app_with_files();
        let text = rendered(&mut app, 130, 30);
        assert!(
            text.contains("cm01 firmware V1.0.3 · serial XYZ1"),
            "{text}"
        );
        assert!(
            text.contains("preset:1  Pre-set 1: Cooling delivery"),
            "{text}"
        );
        assert!(text.contains("vendor.mp4"), "{text}");
        assert!(text.contains("289.5 MiB"), "{text}");
        assert!(text.contains("3.0 GiB free of 3.7 GiB"), "{text}");
        assert!(text.contains("2 Library"), "{text}");
    }

    #[test]
    fn keys_drive_selection_show_and_delete_confirmation() {
        let (mut app, rx) = app_with_files();
        // Six presets come first; the second file is entry 7.
        for _ in 0..7 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        match rx.try_recv().unwrap() {
            Request::Show { media, play } => {
                assert_eq!(media, vec!["clip.mp4"]);
                assert_eq!(play, "Single");
            }
            _ => panic!("expected a show request"),
        }
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(matches!(app.prompt, Some(Prompt::ConfirmDelete(ref name)) if name == "clip.mp4"));
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(app.prompt.is_none());
        assert!(rx.try_recv().is_err(), "declined delete sends nothing");
        app.handle_key(KeyEvent::from(KeyCode::Up));
        app.handle_key(KeyEvent::from(KeyCode::Up));
        app.handle_key(KeyEvent::from(KeyCode::Up));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        match rx.try_recv().unwrap() {
            Request::Show { media, .. } => assert_eq!(media, vec!["preset:5"]),
            _ => panic!("expected a preset show request"),
        }
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
    }

    #[test]
    fn overlay_limits_labels_to_three_and_applies() {
        let (mut app, rx) = app_with_files();
        app.screen.sysinfo_display.clear();
        app.handle_key(KeyEvent::from(KeyCode::Char('3')));
        for _ in 0..4 {
            app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        assert_eq!(app.screen.sysinfo_display.len(), 3);
        assert!(app.error.as_deref().unwrap().contains("at most 3"));
        app.screen.media = vec!["vendor.mp4".into()];
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(rx.try_recv().unwrap(), Request::Overlay(_)));
        let text = rendered(&mut app, 100, 24);
        assert!(text.contains("[x] CPU Temperature"), "{text}");
    }

    #[test]
    fn library_asks_for_a_thumbnail_once_and_draws_it() {
        let (mut app, rx) = app_with_files();
        for _ in 0..6 {
            app.handle_key(KeyEvent::from(KeyCode::Down));
        }
        let text = rendered(&mut app, 120, 30);
        assert!(text.contains("generating the preview"), "{text}");
        assert!(text.contains("Preview · half-blocks"), "{text}");
        match rx.try_recv().unwrap() {
            Request::Thumbnail { name, size } => {
                assert_eq!(name, "vendor.mp4");
                assert_eq!(size, 303_549_636);
            }
            _ => panic!("expected a thumbnail request"),
        }
        rendered(&mut app, 120, 30);
        assert!(rx.try_recv().is_err(), "the thumbnail is requested once");
        // Two tones per cell, or the renderer would draw plain background.
        let mut image = image::RgbImage::new(64, 32);
        for (_, y, pixel) in image.enumerate_pixels_mut() {
            *pixel = if y % 2 == 0 {
                image::Rgb([200, 40, 40])
            } else {
                image::Rgb([40, 40, 200])
            };
        }
        app.handle_event(Event::Preview {
            key: "thumb:vendor.mp4".into(),
            image: Box::new(DynamicImage::ImageRgb8(image)),
        });
        let text = rendered(&mut app, 120, 30);
        assert!(!text.contains("generating the preview"), "{text}");
        assert!(
            text.contains('▀') || text.contains('▄'),
            "half-block cells drawn: {text}"
        );
        app.handle_event(Event::PreviewFailed {
            key: "thumb:clip.mp4".into(),
            reason: "no frame could be decoded".into(),
        });
        app.handle_key(KeyEvent::from(KeyCode::Down));
        let text = rendered(&mut app, 120, 30);
        assert!(text.contains("no frame could be decoded"), "{text}");
    }

    #[test]
    fn wizard_collects_a_transform_then_uploads() {
        let (mut app, rx) = app_with_files();
        app.handle_key(KeyEvent::from(KeyCode::Char('u')));
        for c in "/tmp/a.mp4".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(rx.try_recv().unwrap(), Request::Analyse { .. }));
        assert!(matches!(rx.try_recv().unwrap(), Request::Preview { .. }));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        app.handle_key(KeyEvent::from(KeyCode::Char('r')));
        assert!(
            matches!(rx.try_recv().unwrap(), Request::Analyse { ref transform, .. } if transform.mode.as_deref() == Some("fill"))
        );
        assert!(
            matches!(rx.try_recv().unwrap(), Request::Preview { ref transform, .. } if transform.mode.as_deref() == Some("fill"))
        );
        assert!(
            matches!(rx.try_recv().unwrap(), Request::Analyse { ref transform, .. } if transform.rotate == 90)
        );
        assert!(
            matches!(rx.try_recv().unwrap(), Request::Preview { ref transform, .. } if transform.rotate == 90)
        );
        app.handle_event(Event::Analysis {
            path: PathBuf::from("/tmp/a.mp4"),
            lines: vec!["[ ok ] fine".into(), "plan: send as is → a.mp4".into()],
            acceptable: true,
        });
        let text = rendered(&mut app, 100, 24);
        assert!(text.contains("Mode       fill"), "{text}");
        assert!(text.contains("plan: send as is"), "{text}");
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        match rx.try_recv().unwrap() {
            Request::Upload { path, transform } => {
                assert_eq!(path, PathBuf::from("/tmp/a.mp4"));
                assert_eq!(transform.rotate, 90);
            }
            _ => panic!("expected an upload"),
        }
        assert!(app.wizard.is_none());
    }

    #[test]
    fn display_tab_applies_layout_and_operations_retry_failed_transfers() {
        let (mut app, rx) = app_with_files();
        app.handle_key(KeyEvent::from(KeyCode::Char('4')));
        app.handle_key(KeyEvent::from(KeyCode::Char('s')));
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        app.handle_key(KeyEvent::from(KeyCode::Char('o')));
        app.screen.media = vec!["vendor.mp4".into()];
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(rx.try_recv().unwrap(), Request::Brightness(_)));
        match rx.try_recv().unwrap() {
            Request::Layout { screen, rotation } => {
                assert_eq!(screen.screen_mode, SCREEN_SPLITTING);
                assert!(screen.waterfall_mode);
                assert_eq!(rotation, Some(90));
            }
            _ => panic!("expected a layout request"),
        }
        app.handle_event(Event::Operations(vec![Record {
            id: "abc".into(),
            kind: "upload".into(),
            started_unix: 0,
            finished_unix: Some(1),
            source: PathBuf::from("/tmp/a.mp4"),
            name: None,
            remote: "a.mp4".into(),
            target: "legacy-panorama".into(),
            transform: TransformArgs::default(),
            show: false,
            replace: false,
            outcome: Outcome::Failed,
            error: Some("push refused".into()),
            cached: None,
            size: None,
            sha256: None,
        }]));
        app.handle_key(KeyEvent::from(KeyCode::Char('5')));
        let text = rendered(&mut app, 100, 24);
        assert!(text.contains("push refused"), "{text}");
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(rx.try_recv().unwrap(), Request::Retry(ref id) if id == "abc"));
        app.handle_event(Event::Devices(vec![DeviceRow {
            id: "usb:003-12".into(),
            product: "cm01_se".into(),
            usb_id: "18d1:2d04".into(),
            serial: "XYZ1".into(),
            access: "ok".into(),
            protocol: "legacy (/dev/ttyACM0)".into(),
        }]));
        app.handle_key(KeyEvent::from(KeyCode::Char('1')));
        let text = rendered(&mut app, 100, 24);
        assert!(text.contains("usb:003-12"), "{text}");
    }
}
