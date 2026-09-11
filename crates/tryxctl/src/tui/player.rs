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
use std::collections::VecDeque;
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

/// How a kitty frame's pixels travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// zlib-compressed raw RGB, base64 in the escape.
    Zlib,
    /// A PNG in the escape: smaller, a little more to encode.
    Png,
    /// Raw RGB in a POSIX shared-memory object; only its name travels.
    /// Local kitty and Ghostty only.
    Shm,
}

/// How frames reach the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    Halfblocks,
    /// Kitty graphics, with the terminal's cell size in pixels.
    Kitty {
        cell: (u16, u16),
        transfer: Transfer,
    },
}

impl Sink {
    pub fn name(self) -> &'static str {
        match self {
            Sink::Halfblocks => "half-blocks",
            Sink::Kitty {
                transfer: Transfer::Zlib,
                ..
            } => "kitty · zlib",
            Sink::Kitty {
                transfer: Transfer::Png,
                ..
            } => "kitty · png",
            Sink::Kitty {
                transfer: Transfer::Shm,
                ..
            } => "kitty · shared memory",
        }
    }

    /// The frame size to ask for, for a pane of `cols` by `rows` cells.
    pub fn frame_size(self, cols: u16, rows: u16) -> (u32, u32) {
        match self {
            Sink::Halfblocks => (u32::from(cols.max(1)), u32::from(rows.max(1)) * 2),
            Sink::Kitty { cell: (cw, ch), .. } => {
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
    /// Shared-memory objects handed to the terminal, oldest first; the
    /// terminal unlinks them after reading, and we unlink stragglers.
    shm_names: VecDeque<String>,
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
            shm_names: VecDeque::new(),
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

    /// Draws the newest frame into `area`. For kitty the cells only carry
    /// placeholders, which never change; the returned transmit escape has
    /// to be written to the terminal after the draw.
    pub fn draw(&mut self, area: Rect, buf: &mut Buffer) -> Option<String> {
        let kitty_id = self.kitty_id;
        let sink = self.sink;
        let frame = self.frame()?;
        match sink {
            Sink::Halfblocks => {
                draw_halfblocks(&frame, area, buf);
                None
            }
            Sink::Kitty { transfer, .. } => {
                let rows = area.height.min(DIACRITICS.len() as u16);
                place_kitty(kitty_id, area, buf);
                match transfer {
                    Transfer::Zlib | Transfer::Png => {
                        Some(kitty_transmit(&frame, kitty_id, area.width, rows, transfer))
                    }
                    Transfer::Shm => {
                        let name = format!(
                            "/tryxctl-{}-{}",
                            std::process::id() % 100_000,
                            frame.sequence % 16
                        );
                        shm_publish(&name, &frame.rgb).ok()?;
                        self.shm_names.push_back(name.clone());
                        while self.shm_names.len() > 8 {
                            if let Some(old) = self.shm_names.pop_front() {
                                shm_unlink(&old);
                            }
                        }
                        Some(kitty_transmit_shm(
                            &frame, kitty_id, area.width, rows, &name,
                        ))
                    }
                }
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        for name in self.shm_names.drain(..) {
            shm_unlink(&name);
        }
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

/// The pixel data of a frame as it goes into a direct transmit, with the
/// format keys describing it.
fn encode_direct(frame: &RawFrame, transfer: Transfer) -> (&'static str, Vec<u8>) {
    match transfer {
        Transfer::Png | Transfer::Shm => {
            let mut png = Vec::with_capacity(frame.rgb.len() / 3);
            let encoder = image::codecs::png::PngEncoder::new_with_quality(
                &mut png,
                image::codecs::png::CompressionType::Fast,
                image::codecs::png::FilterType::Adaptive,
            );
            let _ = image::ImageEncoder::write_image(
                encoder,
                &frame.rgb,
                frame.width,
                frame.height,
                image::ExtendedColorType::Rgb8,
            );
            ("f=100", png)
        }
        Transfer::Zlib => {
            let mut encoder =
                ZlibEncoder::new(Vec::with_capacity(frame.rgb.len() / 2), Compression::fast());
            let _ = encoder.write_all(&frame.rgb);
            ("f=24,o=z", encoder.finish().unwrap_or_default())
        }
    }
}

/// The kitty transmit command for `frame` under `id`, base64 in 4 KiB
/// chunks, as a virtual placement of `cols`×`rows` cells.
pub fn kitty_transmit(
    frame: &RawFrame,
    id: u32,
    cols: u16,
    rows: u16,
    transfer: Transfer,
) -> String {
    let (format, data) = encode_direct(frame, transfer);
    let payload = base64::engine::general_purpose::STANDARD.encode(&data);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(4096).collect();
    let mut out = String::with_capacity(payload.len() + chunks.len() * 64);
    for (index, chunk) in chunks.iter().enumerate() {
        out.push_str("\x1b_Gq=2,");
        if index == 0 {
            write!(
                out,
                "i={id},a=T,U=1,{format},t=d,s={},v={},c={cols},r={rows},",
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

/// A transmit whose pixels sit in the shared-memory object `name`: only
/// the name travels. The terminal unlinks the object once read.
pub fn kitty_transmit_shm(frame: &RawFrame, id: u32, cols: u16, rows: u16, name: &str) -> String {
    let payload = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
    format!(
        "\x1b_Gq=2,i={id},a=T,U=1,f=24,t=s,S={},s={},v={},c={cols},r={rows};{payload}\x1b\\",
        frame.rgb.len(),
        frame.width,
        frame.height
    )
}

/// Creates the shared-memory object `name` holding `data`.
#[cfg(unix)]
pub fn shm_publish(name: &str, data: &[u8]) -> std::io::Result<()> {
    use std::ffi::CString;
    let cname = CString::new(name).map_err(|_| std::io::Error::other("bad name"))?;
    // SAFETY: plain POSIX calls on a fresh object we own until unlinked.
    unsafe {
        libc::shm_unlink(cname.as_ptr());
        let fd = libc::shm_open(
            cname.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600,
        );
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            if libc::ftruncate(fd, data.len() as libc::off_t) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let map = libc::mmap(
                std::ptr::null_mut(),
                data.len(),
                libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            );
            if map == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            std::ptr::copy_nonoverlapping(data.as_ptr(), map as *mut u8, data.len());
            libc::munmap(map, data.len());
            Ok(())
        })();
        libc::close(fd);
        if result.is_err() {
            libc::shm_unlink(cname.as_ptr());
        }
        result
    }
}

#[cfg(unix)]
pub fn shm_unlink(name: &str) {
    if let Ok(cname) = std::ffi::CString::new(name) {
        // SAFETY: unlinking a name we created; a missing object is fine.
        unsafe {
            libc::shm_unlink(cname.as_ptr());
        }
    }
}

#[cfg(all(unix, test))]
fn shm_read(name: &str, len: usize) -> std::io::Result<Vec<u8>> {
    use std::ffi::CString;
    let cname = CString::new(name).map_err(|_| std::io::Error::other("bad name"))?;
    // SAFETY: read-only mapping of an object this process created.
    unsafe {
        let fd = libc::shm_open(cname.as_ptr(), libc::O_RDONLY, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let map = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd,
            0,
        );
        libc::close(fd);
        if map == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let data = std::slice::from_raw_parts(map as *const u8, len).to_vec();
        libc::munmap(map, len);
        Ok(data)
    }
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

/// Lays the placeholder cells the way ratatui-image does for stills: every
/// row starts with a placeholder carrying its row diacritic, the image id
/// is carried by the foreground colour, and the cells never change between
/// frames, so ratatui writes them once. The pixels travel separately.
pub fn place_kitty(id: u32, area: Rect, buf: &mut Buffer) {
    let rows = area.height.min(DIACRITICS.len() as u16);
    let cols = area.width;
    if rows == 0 || cols == 0 {
        return;
    }
    let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
    let id_color = format!("\x1b[38;2;{id_r};{id_g};{id_b}m");
    for y in 0..rows {
        let mut symbol = String::new();
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

    fn payload_of(text: &str) -> Vec<u8> {
        let payload = text
            .trim_start_matches(|c| c != ';')
            .trim_start_matches(';')
            .trim_end_matches("\x1b\\");
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap()
    }

    #[test]
    fn frame_sizes_follow_the_pane() {
        assert_eq!(Sink::Halfblocks.frame_size(50, 20), (50, 40));
        let kitty = Sink::Kitty {
            cell: (10, 20),
            transfer: Transfer::Png,
        };
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
    fn zlib_transmit_round_trips_the_pixels() {
        let frame = frame(4, 2);
        let text = kitty_transmit(&frame, 0x00E0_1234, 4, 2, Transfer::Zlib);
        assert!(
            text.starts_with("\x1b_Gq=2,i=14684724,a=T,U=1,f=24,o=z,t=d,s=4,v=2,c=4,r=2,m=0;"),
            "{text}"
        );
        let compressed = payload_of(&text);
        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut rgb = Vec::new();
        decoder.read_to_end(&mut rgb).unwrap();
        assert_eq!(rgb, frame.rgb);
    }

    #[test]
    fn png_transmit_round_trips_the_pixels_and_is_smaller() {
        let frame = frame(64, 32);
        let text = kitty_transmit(&frame, 7, 8, 4, Transfer::Png);
        assert!(
            text.starts_with("\x1b_Gq=2,i=7,a=T,U=1,f=100,t=d,s=64,v=32,c=8,r=4,"),
            "{text}"
        );
        let png = payload_of(&text);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        assert_eq!(decoded.as_raw(), &frame.rgb);
        let zlib = kitty_transmit(&frame, 7, 8, 4, Transfer::Zlib);
        assert!(
            text.len() < zlib.len(),
            "png {} vs zlib {}",
            text.len(),
            zlib.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_memory_transmit_names_an_object_holding_the_pixels() {
        let frame = frame(6, 3);
        let name = format!("/tryxctl-test-{}", std::process::id() % 100_000);
        shm_publish(&name, &frame.rgb).unwrap();
        assert_eq!(shm_read(&name, frame.rgb.len()).unwrap(), frame.rgb);
        let text = kitty_transmit_shm(&frame, 9, 6, 2, &name);
        assert!(
            text.starts_with("\x1b_Gq=2,i=9,a=T,U=1,f=24,t=s,S=54,s=6,v=3,c=6,r=2;"),
            "{text}"
        );
        assert_eq!(payload_of(&text), name.as_bytes());
        shm_unlink(&name);
        assert!(shm_read(&name, frame.rgb.len()).is_err(), "unlinked");
    }

    #[test]
    fn placeholders_are_laid_once_and_never_carry_pixels() {
        let mut buf = Buffer::empty(Rect::new(2, 1, 4, 2));
        place_kitty(0x00E0_1234, Rect::new(2, 1, 4, 2), &mut buf);
        let first = buf[(2, 1)].symbol().to_string();
        assert!(!first.contains("_G"), "no transmit in the cells");
        assert!(
            first.contains("\u{10EEEE}\u{305}\u{305}"),
            "row 0 placeholder with row and column diacritics"
        );
        let second = buf[(2, 2)].symbol().to_string();
        assert!(second.contains("\u{10EEEE}\u{30D}"), "row 1 diacritic");
        assert!(buf[(3, 1)].skip, "cells under the placeholders are skipped");
        let mut again = Buffer::empty(Rect::new(2, 1, 4, 2));
        place_kitty(0x00E0_1234, Rect::new(2, 1, 4, 2), &mut again);
        assert_eq!(buf, again, "identical every frame, so nothing is re-sent");
    }
}
