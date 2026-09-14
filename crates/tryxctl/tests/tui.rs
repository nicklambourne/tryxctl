//! The interface on a pseudo-terminal: it starts, draws the display's files,
//! answers keys, and gives the terminal back when it quits.

#![cfg(unix)]

mod common;

use common::Rig;
use std::time::Duration;
use tryx_testkit::terminal::Terminal;

fn start(rig: &Rig) -> Terminal {
    let mut command = rig.sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
    command
        .arg("tui")
        .env("TERM", "xterm-256color")
        .env("TRYXCTL_GRAPHICS", "halfblocks");
    Terminal::spawn(command, 120, 36)
}

#[test]
fn the_interface_draws_the_library_and_quits_cleanly() {
    let rig = Rig::new();
    rig.adb.put("sunset.mp4", b"not really a video");
    let mut terminal = start(&rig);
    assert!(
        terminal.wait_for("sunset.mp4", Duration::from_secs(20)),
        "the library never listed the file:\n{}",
        terminal.screen()
    );
    terminal.type_keys("5");
    assert!(
        terminal.wait_for("Operations (no transfers yet)", Duration::from_secs(5)),
        "{}",
        terminal.screen()
    );
    // Dropping kept encodes reports in the status line, and prints nothing
    // over the interface.
    terminal.type_keys("c");
    assert!(
        terminal.wait_for("kept encodes removed", Duration::from_secs(5)),
        "{}",
        terminal.screen()
    );
    assert!(
        !terminal.screen().contains("removed_encodes"),
        "{}",
        terminal.screen()
    );
    terminal.type_keys("1");
    assert!(
        terminal.wait_for("usb:003-12", Duration::from_secs(5)),
        "the devices tab lists the display:\n{}",
        terminal.screen()
    );
    terminal.type_keys("q");
    let status = terminal
        .wait(Duration::from_secs(10))
        .unwrap_or_else(|| panic!("q did not quit:\n{}", terminal.screen()));
    assert!(status.success(), "{status}\n{}", terminal.screen());
    assert!(!terminal.alternate_screen(), "the terminal was given back");
    let output = terminal.output();
    let entered = output
        .find("\x1b[?1049h")
        .expect("entered the alternate screen");
    let left = output
        .rfind("\x1b[?1049l")
        .expect("left the alternate screen");
    assert!(left > entered);
}

#[test]
fn without_a_display_the_interface_does_not_start() {
    let rig = Rig::new();
    rig.sandbox.unplug(common::USB, common::TTY);
    let mut terminal = start(&rig);
    let status = terminal.wait(Duration::from_secs(10)).expect("it exits");
    assert_eq!(status.code(), Some(3));
    assert!(
        terminal.wait_for("no TRYX display connected", Duration::from_secs(2)),
        "{}",
        terminal.screen()
    );
    assert!(
        !terminal.output().contains("\x1b[?1049h"),
        "never took over the screen"
    );
}
