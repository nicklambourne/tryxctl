//! The terminal interface. One worker thread owns the display connection
//! ([`worker`]); the screen ([`app`]) only sends requests and draws events.

mod app;
mod player;
mod preview;
mod worker;

use crate::exit::{self, CommandResult, Failure};
use crate::legacy::{self, Backend};
use app::App;
use crossterm::event::{self, Event, KeyEventKind};
use ratatui_image::picker::{Picker, ProtocolType};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use worker::{Request, Worker};

pub fn run(session: &legacy::Session) -> CommandResult {
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        return Err(Failure::usage("the interface needs a terminal"));
    }
    let target = match session.select_backend()? {
        Backend::Legacy(target) => Some(target),
        Backend::Kanali { .. } => None,
    };
    let (request_tx, request_rx) = mpsc::channel::<Request>();
    let (event_tx, event_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker = Worker::spawn(
        session.clone(),
        target,
        cancel.clone(),
        request_rx,
        event_tx,
    );
    request_tx.send(Request::Refresh).ok();

    let picker = graphics();
    let mut terminal = ratatui::init();
    let mut app = App::new(request_tx.clone(), cancel, picker);
    let result = loop {
        if let Err(error) = terminal.draw(|frame| app.render(frame)) {
            break Err(Failure::environment(format!("could not draw: {error}")));
        }
        while let Ok(event) = event_rx.try_recv() {
            app.handle_event(event);
        }
        match event::poll(Duration::from_millis(app.tick_ms())) {
            Ok(true) => match event::read() {
                Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                    if app.handle_key(key) {
                        break Ok(());
                    }
                }
                Ok(_) => {}
                Err(error) => break Err(Failure::environment(format!("input failed: {error}"))),
            },
            Ok(false) => {}
            Err(error) => break Err(Failure::environment(format!("input failed: {error}"))),
        }
    };
    ratatui::restore();
    request_tx.send(Request::Quit).ok();
    worker.join();
    result.map(|()| exit::ok())
}

/// How pictures are drawn. `TRYXCTL_GRAPHICS` (kitty, iterm2, sixel,
/// halfblocks) settles it outright. Inside tmux nothing is queried: the
/// pane only forwards graphics when passthrough is on, so the answer would
/// not be trusted anyway. Elsewhere the terminal is asked, before the
/// alternate screen, and half-blocks are the fallback. The query must not
/// run where the terminal may never answer: its reader thread would then
/// keep stdin and swallow every key.
fn graphics() -> Picker {
    let forced = std::env::var("TRYXCTL_GRAPHICS").ok();
    let protocol = forced
        .as_deref()
        .map(|name| match name.to_ascii_lowercase().as_str() {
            "kitty" => ProtocolType::Kitty,
            "iterm2" | "iterm" => ProtocolType::Iterm2,
            "sixel" => ProtocolType::Sixel,
            _ => ProtocolType::Halfblocks,
        });
    let mut picker = if protocol.is_none() && std::env::var_os("TMUX").is_none() {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize(cell_size()))
    } else {
        Picker::from_fontsize(cell_size())
    };
    if let Some(protocol) = protocol {
        picker.set_protocol_type(protocol);
    }
    picker
}

/// The terminal cell in pixels from the window size, or a 1:2 guess.
fn cell_size() -> (u16, u16) {
    #[cfg(unix)]
    {
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ fills a winsize struct for a terminal fd.
        let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
        if ok && size.ws_col > 0 && size.ws_row > 0 && size.ws_xpixel > 0 && size.ws_ypixel > 0 {
            return (size.ws_xpixel / size.ws_col, size.ws_ypixel / size.ws_row);
        }
    }
    (8, 16)
}
