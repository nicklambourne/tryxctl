//! Pseudo-terminals that no child process inherits.
//!
//! `serialport` opens both ends of a pair without close-on-exec, so every
//! process a test starts would keep a copy of every display's end of its
//! port. A display could then never be unplugged while any of those ran:
//! its port stays open as long as one copy does. The descriptors are marked
//! close-on-exec under a lock that starting a child also takes, so no child
//! can inherit one in the moment between.

use serialport::TTYPort;
use std::os::fd::AsRawFd;
use std::sync::{RwLock, RwLockReadGuard};

static DESCRIPTORS: RwLock<()> = RwLock::new(());

/// A pseudo-terminal pair: the master end, then the slave end.
pub fn pair() -> (TTYPort, TTYPort) {
    let _creating = DESCRIPTORS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (master, slave) = TTYPort::pair().expect("a pseudo-terminal pair");
    for port in [&master, &slave] {
        // SAFETY: F_SETFD on a descriptor this process owns.
        let marked = unsafe { libc::fcntl(port.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
        assert_eq!(
            marked,
            0,
            "close-on-exec: {}",
            std::io::Error::last_os_error()
        );
    }
    (master, slave)
}

/// Hold while starting a child process, so it cannot inherit a
/// pseudo-terminal that is being created at that moment.
pub fn spawning() -> RwLockReadGuard<'static, ()> {
    DESCRIPTORS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
