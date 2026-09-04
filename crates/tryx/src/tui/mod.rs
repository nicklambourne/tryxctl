//! The terminal interface. One worker thread owns the display connection
//! ([`worker`]); the screen ([`app`]) only sends requests and draws events.

mod app;
mod worker;

use crate::exit::{self, CommandResult, Failure};
use crate::legacy::{self, Backend};
use app::App;
use crossterm::event::{self, Event, KeyEventKind};
use std::sync::mpsc;
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
    let worker = Worker::spawn(session.clone(), target, request_rx, event_tx);
    request_tx.send(Request::Refresh).ok();

    let mut terminal = ratatui::init();
    let mut app = App::new(request_tx.clone());
    let result = loop {
        if let Err(error) = terminal.draw(|frame| app.render(frame)) {
            break Err(Failure::environment(format!("could not draw: {error}")));
        }
        while let Ok(event) = event_rx.try_recv() {
            app.handle_event(event);
        }
        match event::poll(Duration::from_millis(100)) {
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
