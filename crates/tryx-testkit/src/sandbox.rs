//! A throwaway directory standing in for everything tryxctl reads from its
//! environment: the XDG directories, the temporary directory, `PATH`, the
//! USB device tree, and os-release.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// Set in a process [`isolated`] started, naming the sandbox it runs in.
const CHILD: &str = "TRYX_TESTKIT_SANDBOX";

pub struct Sandbox {
    root: PathBuf,
    /// Whether dropping the sandbox removes it.
    owned: bool,
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

impl Sandbox {
    /// A new, empty sandbox under the host's temporary directory. The path is
    /// kept short: the daemon's socket lives inside it, and socket paths are
    /// limited to about a hundred bytes.
    pub fn new() -> Sandbox {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "tryxctl-t{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // Left over from a crashed run with the same process id.
        let _ = std::fs::remove_dir_all(&root);
        for dir in [
            "bin", "run", "state", "cache", "config", "home", "tmp", "sys", "dev", "work",
        ] {
            std::fs::create_dir_all(root.join(dir)).expect("a sandbox directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // XDG_RUNTIME_DIR must be private to its user.
            std::fs::set_permissions(root.join("run"), std::fs::Permissions::from_mode(0o700))
                .expect("runtime directory permissions");
        }
        Sandbox { root, owned: true }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Put first on `PATH`: fake tools installed here win over the host's.
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// The working directory of commands run in the sandbox.
    pub fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    /// `$TMPDIR`, where tryxctl stages encodes and previews.
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// The saved display state.
    pub fn state_file(&self) -> PathBuf {
        self.root.join("state/tryxctl/display.json")
    }

    /// The transfer journal.
    pub fn journal(&self) -> PathBuf {
        self.root.join("state/tryxctl/operations.json")
    }

    /// Where failed transfers keep their encodes.
    pub fn encodes(&self) -> PathBuf {
        self.root.join("cache/tryxctl/encodes")
    }

    /// The daemon's socket.
    pub fn socket(&self) -> PathBuf {
        self.root.join("run/tryxctl/daemon.sock")
    }

    /// The device node tryxctl finds for the port named `tty`.
    pub fn port(&self, tty: &str) -> PathBuf {
        self.root.join("dev").join(tty)
    }

    /// The os-release tryxctl reads to tell which distribution it runs on.
    /// There is none until a test writes one.
    pub fn os_release(&self) -> PathBuf {
        self.root.join("os-release")
    }

    /// The variables that confine tryxctl to the sandbox. The host's `PATH`
    /// follows the sandbox's own bin directory, for ffmpeg.
    pub fn vars(&self) -> Vec<(&'static str, OsString)> {
        let mut path = OsString::from(self.bin());
        if let Some(host) = std::env::var_os("PATH") {
            path.push(":");
            path.push(host);
        }
        let dir = |name: &str| self.root.join(name).into_os_string();
        vec![
            ("HOME", dir("home")),
            ("XDG_RUNTIME_DIR", dir("run")),
            ("XDG_STATE_HOME", dir("state")),
            ("XDG_CACHE_HOME", dir("cache")),
            ("XDG_CONFIG_HOME", dir("config")),
            ("TMPDIR", dir("tmp")),
            ("TRYXCTL_SYSFS_USB_DEVICES", dir("sys")),
            ("TRYXCTL_DEV_DIR", dir("dev")),
            ("TRYXCTL_OS_RELEASE", self.os_release().into_os_string()),
            ("PATH", path),
        ]
    }

    /// A command confined to the sandbox, run from its work directory.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.envs(self.vars()).current_dir(self.work());
        // Outside influences on how tryxctl draws or finds a display.
        for name in ["TMUX", "TRYXCTL_GRAPHICS", "NO_COLOR"] {
            command.env_remove(name);
        }
        command
    }

    fn write(&self, relative: &str, name: &str, value: &str) {
        let dir = self.root.join("sys").join(relative);
        std::fs::create_dir_all(&dir).expect("a device tree directory");
        std::fs::write(dir.join(name), format!("{value}\n")).expect("a device attribute");
    }

    /// Lists a cm01 display in the device tree: sysfs name `name` (such as
    /// `3-12`, which adb reports as `usb:3-12`), USB serial `serial`, and a
    /// command port `tty` whose device node points at `node`.
    #[cfg(unix)]
    pub fn plug(&self, name: &str, serial: &str, tty: &str, node: &Path) {
        self.write(name, "idVendor", "18d1");
        self.write(name, "idProduct", "2d04");
        self.write(name, "product", "cm01_se");
        self.write(name, "manufacturer", "rockchip");
        self.write(name, "serial", serial);
        self.write(name, "devnum", "4");
        let interface = |number: u8, class: [&str; 3]| {
            let relative = format!("{name}/{name}:1.{number}");
            self.write(&relative, "bInterfaceClass", class[0]);
            self.write(&relative, "bInterfaceSubClass", class[1]);
            self.write(&relative, "bInterfaceProtocol", class[2]);
            relative
        };
        let control = interface(0, ["02", "02", "01"]);
        interface(2, ["ff", "42", "01"]);
        std::fs::create_dir_all(self.root.join("sys").join(control).join("tty").join(tty))
            .expect("the port's tty directory");
        self.point(tty, node);
    }

    /// Lists a KANALI display of USB product `product_id` in the device tree
    /// under sysfs name `name`, and returns the socket where
    /// [`crate::FakeKanali`] must answer for it.
    pub fn plug_kanali(&self, name: &str, product_id: u16, serial: &str) -> PathBuf {
        self.write(name, "idVendor", "391a");
        self.write(name, "idProduct", &format!("{product_id:04x}"));
        self.write(name, "manufacturer", "TRYX");
        self.write(name, "serial", serial);
        self.write(name, "devnum", "9");
        self.root.join("sys").join(name).join("socket")
    }

    /// Points the device node of `tty` at `node`, as a replugged display
    /// that came back on a different pseudo-terminal.
    #[cfg(unix)]
    pub fn point(&self, tty: &str, node: &Path) {
        let link = self.port(tty);
        if link != node {
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(node, &link).expect("the port's device node");
        }
    }

    /// Removes a display and its port from the device tree.
    pub fn unplug(&self, name: &str, tty: &str) {
        let _ = std::fs::remove_dir_all(self.root.join("sys").join(name));
        let _ = std::fs::remove_file(self.port(tty));
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        if self.owned && std::env::var_os("TRYX_TESTKIT_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Runs `body` in a new process whose environment points into a fresh
/// sandbox, so code that reads the XDG directories, `PATH`, the temporary
/// directory, or the device tree from its own environment can be tested in
/// process without touching the host's files or racing other tests. `name`
/// is the test's path as `cargo test` lists it, such as
/// `tui::worker::tests::uploads`; the test re-runs itself under that name.
pub fn isolated(name: &str, body: impl FnOnce(&Sandbox)) {
    if let Some(root) = std::env::var_os(CHILD) {
        let sandbox = Sandbox {
            root: PathBuf::from(root),
            owned: false,
        };
        body(&sandbox);
        return;
    }
    let sandbox = Sandbox::new();
    let mut command = sandbox.command(std::env::current_exe().expect("the test binary"));
    command
        .args([name, "--exact", "--nocapture", "--test-threads", "1"])
        .env(CHILD, sandbox.root());
    let output = output(command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A name that matches no test passes with nothing run.
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "isolated test {name} failed ({})\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
        output.status
    );
}

/// Runs `command` to completion, capturing its output. On Unix the child is
/// started under [`crate::spawning`], so it inherits no display's port.
pub fn output(mut command: Command) -> std::process::Output {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = {
        #[cfg(unix)]
        let _spawning = crate::pty::spawning();
        command.spawn().expect("the command starts")
    };
    child.wait_with_output().expect("the command's output")
}
