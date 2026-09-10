//! Playback in the preview pane. ffmpeg streams raw frames at the file's
//! own pace, a thread keeps only the newest one, and every redraw shows it:
//! as half-blocks anywhere, or in kitty-protocol terminals as a fresh image
//! transmitted under the same id behind unicode placeholders, so only the
//! pixel data travels each frame. Frames the redraw never gets to are
//! dropped rather than queued.

use base64::Engine;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::fmt::Write as _;
use std::io::{Read, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// Widest frame requested from ffmpeg for a pixel-protocol terminal.
const MAX_WIDTH: u32 = 640;
/// Frames per second asked of ffmpeg.
pub const FPS: u32 = 30;

/// One decoded frame, packed RGB.
pub struct RawFrame {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
    pub sequence: u64,
}

/// How frames reach the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Halfblocks,
    /// Kitty graphics, with the terminal's cell size in pixels.
    Kitty {
        cell: (u16, u16),
    },
}

impl Sink {
    pub fn name(self) -> &'static str {
        match self {
            Sink::Halfblocks => "half-blocks",
            Sink::Kitty { .. } => "kitty graphics",
        }
    }

    /// The frame size to ask for, for a pane of `cols` by `rows` cells.
    pub fn frame_size(self, cols: u16, rows: u16) -> (u32, u32) {
        match self {
            Sink::Halfblocks => (u32::from(cols.max(1)), u32::from(rows.max(1)) * 2),
            Sink::Kitty { cell: (cw, ch) } => {
                let full_w = u32::from(cols.max(1)) * u32::from(cw.max(1));
                let full_h = u32::from(rows.max(1)) * u32::from(ch.max(1));
                let width = full_w.min(MAX_WIDTH);
                let height = (full_h * width / full_w).max(2);
                (width, height)
            }
        }
    }
}

struct Shared {
    stop: AtomicBool,
    latest: Mutex<Option<Arc<RawFrame>>>,
    produced: AtomicU64,
    error: Mutex<Option<String>>,
}

pub struct Player {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    started: Instant,
    shown: u64,
    last_sequence: u64,
    pub sink: Sink,
    kitty_id: u32,
}

/// The ffmpeg filter chain producing `width`×`height` frames at [`FPS`],
/// letterboxed, after the optional geometry `prefix`.
pub fn filter(prefix: Option<&str>, width: u32, height: u32) -> String {
    let mut chain = String::new();
    if let Some(prefix) = prefix.filter(|p| !p.is_empty()) {
        chain.push_str(prefix);
        chain.push(',');
    }
    write!(
        chain,
        "fps={FPS},scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black"
    )
    .unwrap();
    chain
}

impl Player {
    /// Starts ffmpeg on `input`, looping it, at the pace of the file.
    pub fn start(
        ffmpeg: &Path,
        input: &Path,
        prefix: Option<&str>,
        start: Option<f64>,
        cols: u16,
        rows: u16,
        sink: Sink,
    ) -> Result<Player, String> {
        let (width, height) = sink.frame_size(cols, rows);
        let mut command = Command::new(ffmpeg);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-re",
            "-stream_loop",
            "-1",
        ]);
        if let Some(seconds) = start {
            command.args(["-ss", &format!("{seconds:.3}")]);
        }
        command
            .arg("-i")
            .arg(input)
            .args(["-map", "0:v:0", "-vf"])
            .arg(filter(prefix, width, height))
            .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| format!("ffmpeg: {e}"))?;
        let stdout = child.stdout.take().expect("piped");
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            latest: Mutex::new(None),
            produced: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let pumping = shared.clone();
        let thread = std::thread::spawn(move || {
            pump(stdout, width, height, &pumping);
            let _ = child.kill();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let _ = child.wait();
            let stderr = stderr.trim();
            if !stderr.is_empty() && !pumping.stop.load(Ordering::Relaxed) {
                *pumping.error.lock().unwrap() = Some(stderr.to_string());
            }
        });
        Ok(Player {
            shared,
            thread: Some(thread),
            started: Instant::now(),
            shown: 0,
            last_sequence: 0,
            sink,
            kitty_id: 0x00E0_0000 | (std::process::id() & 0xFFFF),
        })
    }

    /// The newest frame; counts it as shown when it is new.
    fn frame(&mut self) -> Option<Arc<RawFrame>> {
        let frame = self.shared.latest.lock().unwrap().clone()?;
        if frame.sequence != self.last_sequence {
            self.last_sequence = frame.sequence;
            self.shown += 1;
        }
        Some(frame)
    }

    /// Frames shown per second, frames produced, frames dropped.
    pub fn stats(&self) -> (f64, u64, u64) {
        let produced = self.shared.produced.load(Ordering::Relaxed);
        let seconds = self.started.elapsed().as_secs_f64().max(0.001);
        (
            self.shown as f64 / seconds,
            produced,
            produced.saturating_sub(self.shown),
        )
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Whether ffmpeg is still feeding frames.
    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// Draws the newest frame into `area`.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) {
        let kitty_id = self.kitty_id;
        let sink = self.sink;
        let Some(frame) = self.frame() else {
            return;
        };
        match sink {
            Sink::Halfblocks => draw_halfblocks(&frame, area, buf),
            Sink::Kitty { .. } => draw_kitty(&frame, kitty_id, area, buf),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        // The reader notices when ffmpeg's pipe closes; kill through it.
        if let Some(thread) = self.thread.take() {
            // ffmpeg dies with the reader's kill; waiting here is short.
            let _ = thread.join();
        }
    }
}

/// Reads fixed-size frames until the stream ends or `stop` is set, keeping
/// only the newest.
fn pump(mut source: impl Read, width: u32, height: u32, shared: &Shared) {
    let frame_len = (width * height * 3) as usize;
    let mut sequence = 0u64;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        let mut rgb = vec![0u8; frame_len];
        if read_exact_or_stop(&mut source, &mut rgb, &shared.stop).is_err() {
            return;
        }
        sequence += 1;
        *shared.latest.lock().unwrap() = Some(Arc::new(RawFrame {
            width,
            height,
            rgb,
            sequence,
        }));
        shared.produced.fetch_add(1, Ordering::Relaxed);
    }
}

fn read_exact_or_stop(
    source: &mut impl Read,
    buffer: &mut [u8],
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        if stop.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("stopped"));
        }
        match source.read(&mut buffer[filled..]) {
            Ok(0) => return Err(std::io::Error::other("end of stream")),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Two pixels per cell: the upper one as the foreground of `▀`, the lower
/// one as the background.
pub fn draw_halfblocks(frame: &RawFrame, area: Rect, buf: &mut Buffer) {
    let pixel = |x: u32, y: u32| -> Option<Color> {
        if x >= frame.width || y >= frame.height {
            return None;
        }
        let at = ((y * frame.width + x) * 3) as usize;
        Some(Color::Rgb(
            frame.rgb[at],
            frame.rgb[at + 1],
            frame.rgb[at + 2],
        ))
    };
    for y in 0..area.height {
        for x in 0..area.width {
            let (upper, lower) = (
                pixel(u32::from(x), u32::from(y) * 2),
                pixel(u32::from(x), u32::from(y) * 2 + 1),
            );
            let (Some(upper), Some(lower)) = (upper, lower) else {
                continue;
            };
            if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                cell.set_char('▀').set_fg(upper).set_bg(lower);
            }
        }
    }
}

/// The kitty transmit command for `frame` under `id`: zlib-compressed RGB,
/// base64 in 4 KiB chunks, as a virtual placement of `cols`×`rows` cells.
pub fn kitty_transmit(frame: &RawFrame, id: u32, cols: u16, rows: u16) -> String {
    let mut encoder =
        ZlibEncoder::new(Vec::with_capacity(frame.rgb.len() / 2), Compression::fast());
    let _ = encoder.write_all(&frame.rgb);
    let compressed = encoder.finish().unwrap_or_default();
    let payload = base64::engine::general_purpose::STANDARD.encode(&compressed);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(4096).collect();
    let mut out = String::with_capacity(payload.len() + chunks.len() * 64);
    for (index, chunk) in chunks.iter().enumerate() {
        out.push_str("\x1b_Gq=2,");
        if index == 0 {
            write!(
                out,
                "i={id},a=T,U=1,f=24,o=z,t=d,s={},v={},c={cols},r={rows},",
                frame.width, frame.height
            )
            .unwrap();
        }
        let more = u8::from(index + 1 < chunks.len());
        write!(out, "m={more};").unwrap();
        out.push_str(std::str::from_utf8(chunk).expect("base64 is ascii"));
        out.push_str("\x1b\\");
    }
    out
}

/// The row diacritics of the kitty unicode-placeholder scheme, in order.
const DIACRITICS: [char; 74] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
    '\u{598}', '\u{599}', '\u{59C}', '\u{59D}', '\u{59E}', '\u{59F}', '\u{5A0}', '\u{5A1}',
    '\u{5A8}', '\u{5A9}', '\u{5AB}', '\u{5AC}', '\u{5AF}', '\u{5C4}', '\u{610}', '\u{611}',
    '\u{612}', '\u{613}', '\u{614}', '\u{615}', '\u{616}', '\u{617}', '\u{657}', '\u{658}',
    '\u{659}', '\u{65A}', '\u{65B}', '\u{65D}', '\u{65E}', '\u{6D6}', '\u{6D7}', '\u{6D8}',
    '\u{6D9}', '\u{6DA}',
];

/// Transmits the frame and lays the placeholder cells, the way
/// ratatui-image does for stills: the escape rides in the first cell's
/// symbol, every row starts with a placeholder carrying its row diacritic,
/// and the image id is carried by the foreground colour.
pub fn draw_kitty(frame: &RawFrame, id: u32, area: Rect, buf: &mut Buffer) {
    let rows = area.height.min(DIACRITICS.len() as u16);
    let cols = area.width;
    if rows == 0 || cols == 0 {
        return;
    }
    let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
    let id_color = format!("\x1b[38;2;{id_r};{id_g};{id_b}m");
    let mut transmit = Some(kitty_transmit(frame, id, cols, rows));
    for y in 0..rows {
        let mut symbol = transmit.take().unwrap_or_default();
        write!(
            symbol,
            "\x1b[s{id_color}\u{10EEEE}{}{}{}",
            DIACRITICS[usize::from(y)],
            DIACRITICS[0],
            DIACRITICS[usize::from(id_extra).min(DIACRITICS.len() - 1)]
        )
        .unwrap();
        symbol.extend(std::iter::repeat_n('\u{10EEEE}', usize::from(cols) - 1));
        for x in 1..cols {
            if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                cell.set_skip(true);
            }
        }
        write!(symbol, "\x1b[u\x1b[{}C\x1b[{}B", cols - 1, area.height - 1).unwrap();
        if let Some(cell) = buf.cell_mut((area.x, area.y + y)) {
            cell.set_symbol(&symbol);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frame(width: u32, height: u32) -> RawFrame {
        let mut rgb = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgb.extend_from_slice(&[(x * 40) as u8, (y * 90) as u8, 7]);
            }
        }
        RawFrame {
            width,
            height,
            rgb,
            sequence: 1,
        }
    }

    #[test]
    fn frame_sizes_follow_the_pane() {
        assert_eq!(Sink::Halfblocks.frame_size(50, 20), (50, 40));
        let kitty = Sink::Kitty { cell: (10, 20) };
        assert_eq!(kitty.frame_size(40, 20), (400, 400));
        assert_eq!(kitty.frame_size(100, 20), (640, 256), "capped, aspect kept");
        assert!(filter(Some("hflip"), 64, 32).starts_with("hflip,fps=30,scale=64:32:"));
        assert!(filter(None, 64, 32).starts_with("fps=30,"));
    }

    #[test]
    fn the_pump_keeps_only_the_newest_frame_and_counts() {
        let shared = Shared {
            stop: AtomicBool::new(false),
            latest: Mutex::new(None),
            produced: AtomicU64::new(0),
            error: Mutex::new(None),
        };
        let mut stream = Vec::new();
        for n in 1..=3u8 {
            stream.extend(std::iter::repeat_n(n, 2 * 2 * 3));
        }
        stream.extend_from_slice(&[9, 9]); // a torn tail is discarded
        pump(Cursor::new(stream), 2, 2, &shared);
        let latest = shared.latest.lock().unwrap().clone().unwrap();
        assert_eq!(latest.sequence, 3);
        assert!(latest.rgb.iter().all(|b| *b == 3));
        assert_eq!(shared.produced.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn halfblocks_pair_two_pixels_per_cell() {
        let frame = frame(3, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 3, 2));
        draw_halfblocks(&frame, Rect::new(0, 0, 3, 2), &mut buf);
        let cell = &buf[(1, 1)];
        assert_eq!(cell.symbol(), "▀");
        assert_eq!(cell.fg, Color::Rgb(40, 180, 7), "pixel (1,2) on top");
        assert_eq!(
            cell.bg,
            Color::Rgb(40, 14, 7),
            "pixel (1,3) below, 270 wraps to 14"
        );
    }

    #[test]
    fn kitty_transmit_round_trips_the_pixels_and_places_the_rows() {
        let frame = frame(4, 2);
        let text = kitty_transmit(&frame, 0x00E0_1234, 4, 2);
        assert!(
            text.starts_with("\x1b_Gq=2,i=14684724,a=T,U=1,f=24,o=z,t=d,s=4,v=2,c=4,r=2,m=0;"),
            "{text}"
        );
        let payload = text
            .trim_start_matches(|c| c != ';')
            .trim_start_matches(';')
            .trim_end_matches("\x1b\\");
        let compressed = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut rgb = Vec::new();
        decoder.read_to_end(&mut rgb).unwrap();
        assert_eq!(rgb, frame.rgb);

        let mut buf = Buffer::empty(Rect::new(2, 1, 4, 2));
        draw_kitty(&frame, 0x00E0_1234, Rect::new(2, 1, 4, 2), &mut buf);
        let first = buf[(2, 1)].symbol().to_string();
        assert!(
            first.starts_with("\x1b_Gq=2,i=14684724"),
            "the transmit rides in the first cell"
        );
        assert!(
            first.contains("\u{10EEEE}\u{305}\u{305}"),
            "row 0 placeholder with row and column diacritics"
        );
        let second = buf[(2, 2)].symbol().to_string();
        assert!(!second.contains("_G"), "later rows carry placeholders only");
        assert!(second.contains("\u{10EEEE}\u{30D}"), "row 1 diacritic");
        assert!(buf[(3, 1)].skip, "cells under the placeholders are skipped");
    }
}
