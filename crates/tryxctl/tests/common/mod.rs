//! Running tryxctl confined to a sandbox, with a fake display plugged in.

#![allow(dead_code)] // each test binary uses a different part

use std::path::PathBuf;
use tryx_testkit::cm01::SERIAL;
use tryx_testkit::{FakeAdb, FakeCm01, FakeKanali, Sandbox};

/// The sysfs name of the fake display's USB port.
pub const USB: &str = "3-12";
/// The fake display's command port.
pub const TTY: &str = "ttyACM0";

/// What one run of tryxctl did.
#[derive(Debug)]
pub struct Run {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// Asserts the exit code, showing the output when it differs.
    #[track_caller]
    pub fn expect(&self, code: i32) -> &Run {
        assert_eq!(
            self.code,
            Some(code),
            "\n--- stdout\n{}\n--- stderr\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    #[track_caller]
    pub fn ok(&self) -> &Run {
        self.expect(0)
    }

    /// Stdout parsed as JSON.
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("{error}: {}\n--- stderr\n{}", self.stdout, self.stderr))
    }

    /// Asserts that stderr mentions `text`.
    #[track_caller]
    pub fn complains(&self, text: &str) -> &Run {
        assert!(
            self.stderr.contains(text),
            "stderr lacks {text:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.stdout,
            self.stderr
        );
        self
    }

    /// Asserts that stdout mentions `text`.
    #[track_caller]
    pub fn says(&self, text: &str) -> &Run {
        assert!(
            self.stdout.contains(text),
            "stdout lacks {text:?}\n--- stdout\n{}\n--- stderr\n{}",
            self.stdout,
            self.stderr
        );
        self
    }
}

/// Runs tryxctl in `sandbox`.
pub fn run(sandbox: &Sandbox, args: &[&str]) -> Run {
    let mut command = sandbox.command(env!("CARGO_BIN_EXE_tryxctl"));
    command.args(args);
    let output = tryx_testkit::sandbox::output(command);
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A sandbox with a fake cm01 display plugged in and a fake adb serving its
/// media.
pub struct Rig {
    pub sandbox: Sandbox,
    pub display: FakeCm01,
    pub adb: FakeAdb,
}

impl Rig {
    pub fn new() -> Rig {
        let sandbox = Sandbox::new();
        let display = FakeCm01::start();
        sandbox.plug(USB, SERIAL, TTY, display.port());
        let adb = FakeAdb::install(&sandbox, SERIAL, USB);
        Rig {
            sandbox,
            display,
            adb,
        }
    }

    pub fn run(&self, args: &[&str]) -> Run {
        run(&self.sandbox, args)
    }

    /// The port tryxctl discovers the display on.
    pub fn port(&self) -> String {
        self.sandbox.port(TTY).to_string_lossy().into_owned()
    }

    /// The saved display state.
    pub fn state(&self) -> serde_json::Value {
        read_json(self.sandbox.state_file())
    }

    /// Replaces the saved display state.
    pub fn save_state(&self, state: serde_json::Value) {
        let path = self.sandbox.state_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, state.to_string()).unwrap();
    }

    /// The transfer journal.
    pub fn journal(&self) -> Vec<serde_json::Value> {
        match read_json(self.sandbox.journal()) {
            serde_json::Value::Array(records) => records,
            _ => Vec::new(),
        }
    }
}

/// The sysfs name of the fake KANALI display's USB port.
pub const KANALI_USB: &str = "3-4";
/// The id tryxctl gives the fake KANALI display.
pub const KANALI_ID: &str = "usb:003-4";

/// A sandbox with a fake KANALI display plugged in.
pub struct KanaliRig {
    pub sandbox: Sandbox,
    pub display: FakeKanali,
}

impl KanaliRig {
    /// A Panorama SE.
    pub fn new() -> KanaliRig {
        KanaliRig::with_product(tryx_testkit::kanali::PANORAMA_SE)
    }

    pub fn with_product(product_id: u16) -> KanaliRig {
        let sandbox = Sandbox::new();
        let socket = sandbox.plug_kanali(KANALI_USB, product_id, tryx_testkit::kanali::SERIAL);
        let display = FakeKanali::start(&socket);
        KanaliRig { sandbox, display }
    }

    pub fn run(&self, args: &[&str]) -> Run {
        run(&self.sandbox, args)
    }

    /// The saved display state.
    pub fn state(&self) -> serde_json::Value {
        read_json(self.sandbox.state_file())
    }

    /// The media the display is set to show.
    pub fn showing(&self) -> String {
        self.display.with(|display| {
            display
                .config
                .work_config
                .as_ref()
                .map(|work| work.single_mode_media_file.clone())
                .unwrap_or_default()
        })
    }
}

pub fn read_json(path: PathBuf) -> serde_json::Value {
    std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(serde_json::Value::Null)
}

/// Whether the files tryxctl stages in its temporary directory were cleaned
/// up.
pub fn nothing_staged(sandbox: &Sandbox) -> bool {
    std::fs::read_dir(sandbox.tmp())
        .map(|entries| entries.count() == 0)
        .unwrap_or(true)
}
