//! A program on a pseudo-terminal, for testing the interface: keys are
//! typed into it, and what it draws is read back through a terminal emulator,
//! since a redraw only rewrites the cells that changed.

use serialport::TTYPort;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub struct Terminal {
    master: TTYPort,
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    screen: Arc<Mutex<vt100::Parser>>,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
}

impl Terminal {
    /// Starts `command` in a new session whose controlling terminal is a
    /// `cols`×`rows` pseudo-terminal, which is also its stdin, stdout, and
    /// stderr.
    pub fn spawn(mut command: Command, cols: u16, rows: u16) -> Terminal {
        let (master, slave) = crate::pty::pair();
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: cols * 8,
            ws_ypixel: rows * 16,
        };
        // SAFETY: TIOCSWINSZ reads a winsize struct for a terminal fd.
        unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        let stdio = || {
            // SAFETY: the duplicate is a new descriptor this OwnedFd then
            // owns; close-on-exec keeps it out of every other child.
            let fd = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            assert!(fd >= 0, "a duplicate of the terminal");
            Stdio::from(unsafe { OwnedFd::from_raw_fd(fd) })
        };
        command.stdin(stdio()).stdout(stdio()).stderr(stdio());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                // The request's type differs between platforms.
                #[allow(clippy::useless_conversion)]
                libc::ioctl(0, libc::TIOCSCTTY.into(), 0);
                Ok(())
            });
        }
        let child = {
            let _spawning = crate::pty::spawning();
            command.spawn().expect("the program starts")
        };
        drop(slave);

        let output = Arc::new(Mutex::new(Vec::new()));
        let screen = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let mut master = master.try_clone_native().expect("a reading handle");
            let output = output.clone();
            let screen = screen.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                while !stop.load(Ordering::Relaxed) {
                    match master.read(&mut chunk) {
                        Ok(0) => std::thread::sleep(Duration::from_millis(10)),
                        Ok(count) => {
                            output.lock().unwrap().extend_from_slice(&chunk[..count]);
                            screen.lock().unwrap().process(&chunk[..count]);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
                        // The program exited and closed the terminal.
                        Err(_) => std::thread::sleep(Duration::from_millis(10)),
                    }
                }
            })
        };
        Terminal {
            master,
            child,
            output,
            screen,
            stop,
            reader: Some(reader),
        }
    }

    /// Types `keys` as they are; `\r` is Enter and `\x1b` starts an escape.
    pub fn type_keys(&mut self, keys: &str) {
        self.master.write_all(keys.as_bytes()).expect("typing");
        self.master.flush().expect("typing");
    }

    /// Every byte written so far, escape sequences and all.
    pub fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    /// The text on the screen now, row by row.
    pub fn screen(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    /// Whether the program has switched to the alternate screen.
    pub fn alternate_screen(&self) -> bool {
        self.screen.lock().unwrap().screen().alternate_screen()
    }

    /// Waits until the screen shows `needle`; false on timeout.
    pub fn wait_for(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.screen().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.screen().contains(needle)
    }

    /// Waits for the program to exit; `None` when it is still running after
    /// `timeout`, in which case it is killed.
    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        None
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.stop.store(true, Ordering::Relaxed);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
