//! Screen state, key handling, and drawing.

use super::worker::{Event, Request};
use crate::legacy::Info;
use crate::metrics::LABELS;
use crate::{output, state};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Gauge, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tryx_legacy::adb::{DiskUsage, MediaFile};
use tryx_legacy::{FanStatus, ScreenConfig};
use tryx_monitor::Sample;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Library,
    Overlay,
    Display,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Library, Tab::Overlay, Tab::Display];

    fn title(self) -> &'static str {
        match self {
            Tab::Library => "Library",
            Tab::Overlay => "Overlay",
            Tab::Display => "Display",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Prompt {
    UploadPath(String),
    ConfirmDelete(String),
}

const POSITIONS: [&str; 3] = ["Top", "Center", "Bottom"];
const ALIGNMENTS: [&str; 3] = ["Left", "Center", "Right"];

pub struct App {
    requests: Sender<Request>,
    tab: Tab,
    info: Option<Info>,
    files: Vec<MediaFile>,
    storage: Option<DiskUsage>,
    list: ListState,
    sample: Option<Sample>,
    fans: FanStatus,
    via: &'static str,
    screen: ScreenConfig,
    label_cursor: usize,
    brightness: u8,
    pushing: bool,
    upload: Option<(String, f64)>,
    prompt: Option<Prompt>,
    status: String,
    error: Option<String>,
}

impl App {
    pub fn new(requests: Sender<Request>) -> Self {
        let saved = state::load();
        let mut list = ListState::default();
        list.select(Some(0));
        App {
            requests,
            tab: Tab::Library,
            info: None,
            files: Vec::new(),
            storage: None,
            list,
            sample: None,
            fans: FanStatus::default(),
            via: "connecting",
            screen: saved.screen,
            label_cursor: 0,
            brightness: saved.brightness.unwrap_or(75),
            pushing: false,
            upload: None,
            prompt: None,
            status: "connecting…".to_string(),
            error: None,
        }
    }

    fn send(&self, request: Request) {
        let _ = self.requests.send(request);
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
                self.list
                    .select(Some(selected.min(self.files.len().saturating_sub(1))));
            }
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
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab => self.tab = next(&Tab::ALL, self.tab, 1),
            KeyCode::BackTab => self.tab = next(&Tab::ALL, self.tab, -1),
            KeyCode::Char('1') => self.tab = Tab::Library,
            KeyCode::Char('2') => self.tab = Tab::Overlay,
            KeyCode::Char('3') => self.tab = Tab::Display,
            KeyCode::Char('r') => self.send(Request::Refresh),
            KeyCode::Char('m') => self.send(Request::PushMetrics(!self.pushing)),
            _ => match self.tab {
                Tab::Library => self.handle_library_key(key),
                Tab::Overlay => self.handle_overlay_key(key),
                Tab::Display => self.handle_display_key(key),
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
                    self.status = format!("uploading {}…", path.display());
                    self.send(Request::Upload(path));
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

    fn selected_file(&self) -> Option<&MediaFile> {
        self.list.selected().and_then(|index| self.files.get(index))
    }

    fn handle_library_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Enter => {
                if let Some(name) = self.selected_file().map(|file| file.name.clone()) {
                    self.screen.media = vec![name.clone()];
                    self.send(Request::Show {
                        media: vec![name],
                        play: "Single".to_string(),
                    });
                }
            }
            KeyCode::Char('l') => {
                let names: Vec<String> = self.files.iter().map(|f| f.name.clone()).collect();
                if !names.is_empty() {
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
            KeyCode::Char('u') => self.prompt = Some(Prompt::UploadPath(String::new())),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let current = self.list.selected().unwrap_or(0) as isize;
        let next = (current + delta).rem_euclid(self.files.len() as isize) as usize;
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
                if self.screen.media.is_empty() {
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
            KeyCode::Enter => self.send(Request::Brightness(self.brightness)),
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
        match self.tab {
            Tab::Library => self.render_library(frame, body),
            Tab::Overlay => self.render_overlay(frame, body),
            Tab::Display => self.render_display(frame, body),
        }
        self.render_footer(frame, footer);
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
        let showing = if self.screen.media.is_empty() {
            "nothing selected".to_string()
        } else {
            self.screen.media.join(", ")
        };
        let text = vec![
            Line::from(vec![Span::raw("Display  ").dim(), Span::raw(device)]),
            Line::from(vec![
                Span::raw("Showing  ").dim(),
                Span::raw(showing),
                Span::raw("   ").dim(),
                Span::raw(storage).dim(),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" tryx ")),
            area,
        );
    }

    fn render_library(&mut self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .files
            .iter()
            .map(|file| {
                let showing = self.screen.media.contains(&file.name);
                let marker = if showing { "▶ " } else { "  " };
                ListItem::new(Line::from(vec![
                    Span::raw(marker).fg(Color::Green),
                    Span::raw(format!("{:<40}", file.name)),
                    Span::raw(output::human_bytes(file.size)).dim(),
                ]))
            })
            .collect();
        let title = if self.files.is_empty() {
            " Library (empty · u uploads a file) "
        } else {
            " Library · Enter show · l loop all · d delete · u upload "
        };
        let list = List::new(items)
            .block(Block::bordered().title(title))
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
        frame.render_stateful_widget(list, area, &mut self.list);
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
        let [gauge_area, help_area] =
            Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        frame.render_widget(
            Gauge::default()
                .block(Block::bordered().title(" Brightness · ←/→ adjust · Enter apply "))
                .gauge_style(Style::new().fg(Color::Yellow))
                .ratio(f64::from(self.brightness) / 100.0)
                .label(format!("{}%", self.brightness)),
            gauge_area,
        );
        let info = match &self.info {
            Some(info) => info
                .fields()
                .into_iter()
                .map(|(key, value)| Line::from(format!("{key:<11} {value}")))
                .collect(),
            None => vec![Line::from("No device information yet.")],
        };
        frame.render_widget(
            Paragraph::new(info).block(Block::bordered().title(" Device ")),
            help_area,
        );
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
                "Upload path: {text}▏  (Enter to upload, Esc to cancel)"
            ))
            .fg(Color::Cyan),
            (Some(Prompt::ConfirmDelete(name)), _, _) => {
                Line::from(format!("Delete {name} from the display? y/n")).fg(Color::Yellow)
            }
            (None, Some((name, fraction)), _) => {
                Line::from(format!("encoding {name} {:>3}%", (fraction * 100.0) as u32))
                    .fg(Color::Cyan)
            }
            (None, None, Some(error)) => Line::from(format!("error: {error}")).fg(Color::Red),
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
        let mut app = App::new(tx);
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
    fn library_renders_files_storage_and_device() {
        let (mut app, _rx) = app_with_files();
        let text = rendered(&mut app, 100, 24);
        assert!(
            text.contains("cm01 firmware V1.0.3 · serial XYZ1"),
            "{text}"
        );
        assert!(text.contains("vendor.mp4"), "{text}");
        assert!(text.contains("289.5 MiB"), "{text}");
        assert!(text.contains("3.0 GiB free of 3.7 GiB"), "{text}");
        assert!(text.contains("1 Library"), "{text}");
    }

    #[test]
    fn keys_drive_selection_show_and_delete_confirmation() {
        let (mut app, rx) = app_with_files();
        app.handle_key(KeyEvent::from(KeyCode::Down));
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
        assert!(app.handle_key(KeyEvent::from(KeyCode::Char('q'))));
    }

    #[test]
    fn overlay_limits_labels_to_three_and_applies() {
        let (mut app, rx) = app_with_files();
        app.screen.sysinfo_display.clear();
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
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
}
