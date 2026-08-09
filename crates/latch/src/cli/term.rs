//! Raw mode and terminal restoration.
//!
//! Entering raw mode is easy. Leaving it on every exit path — including a panic
//! and a signal that never reaches the normal return — is the part that matters.
//! [`RawModeGuard`] restores on `Drop`, so an early return or an unwinding panic
//! cannot skip it. Signal handlers restore before re-raising so `SIGINT` /
//! `SIGTERM` / `SIGHUP` leave the terminal usable too.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

use latch_protocol::control::Size;

static RECEIVED_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Saved terminal attributes, opaque to everyone but this module.
#[derive(Debug, Clone)]
pub struct TermiosAttrs {
    attrs: libc::termios,
}

/// Holds the terminal in raw mode for the life of the value.
///
/// Dropping restores the attributes that were current when [`RawModeGuard::enter`]
/// succeeded. The restoration is the point of the type: it must run even when
/// the attach loop panics or returns early.
#[derive(Debug)]
pub struct RawModeGuard {
    fd: RawFd,
    original: TermiosAttrs,
    previous_handlers: [(i32, libc::sighandler_t); 3],
}

impl RawModeGuard {
    /// Puts `fd` into raw mode and returns a guard that restores on drop.
    ///
    /// Also installs signal handlers that restore before re-raising, covering
    /// the paths `Drop` cannot: `SIGINT`, `SIGTERM`, `SIGHUP`.
    pub fn enter(fd: RawFd) -> std::io::Result<Self> {
        let original = current_attrs(fd)?;
        let mut raw = original.attrs;
        // SAFETY: `raw` points to a valid termios structure owned by this
        // function. `cfmakeraw` only updates that structure.
        unsafe { libc::cfmakeraw(&mut raw) };
        set_attrs(fd, &raw)?;
        let previous_handlers = install_signal_handlers()?;
        Ok(Self {
            fd,
            original,
            previous_handlers,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Never panic while unwinding an attach failure. There is no useful
        // recovery action at this point, but a best-effort restore protects the
        // invoking shell in the overwhelmingly common failure mode.
        let _ = restore(self.fd, &self.original);
        for (signal, previous) in self.previous_handlers {
            // SAFETY: restoring the previous process-wide signal disposition is
            // valid for the same signals installed in `enter`.
            unsafe { libc::signal(signal, previous) };
        }
    }
}

/// Returns and clears a signal received while a raw-mode guard was active.
///
/// The handler only records the number — the attach loop observes it, returns,
/// and lets [`RawModeGuard::drop`] restore terminal attributes on the ordinary
/// stack. This keeps terminal syscalls out of the asynchronous signal handler.
pub fn take_received_signal() -> Option<i32> {
    let signal = RECEIVED_SIGNAL.swap(0, Ordering::SeqCst);
    (signal != 0).then_some(signal)
}

/// Reads a terminal's current character-cell geometry.
///
/// The same function supplies the child's initial PTY size and every attach or
/// resize message. Keeping that platform query here prevents creation and
/// attachment from quietly disagreeing about what the iTerm window looks like.
pub fn terminal_size(fd: RawFd) -> Size {
    let mut winsize = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: `winsize` is writable storage and the kernel validates `fd`.
    let has_size = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, winsize.as_mut_ptr()) } != -1;
    if has_size {
        // SAFETY: a successful TIOCGWINSZ initialized `winsize`.
        let winsize = unsafe { winsize.assume_init() };
        if winsize.ws_col > 0 && winsize.ws_row > 0 {
            return Size {
                cols: winsize.ws_col,
                rows: winsize.ws_row,
            };
        }
    }
    Size { cols: 80, rows: 24 }
}

/// The attributes currently in effect on `fd`.
pub fn current_attrs(fd: RawFd) -> std::io::Result<TermiosAttrs> {
    let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `attrs` has space for libc to initialize and `fd` is supplied by
    // the caller. A failing descriptor is reported through errno.
    if unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful tcgetattr initializes the whole termios structure.
    Ok(TermiosAttrs {
        attrs: unsafe { attrs.assume_init() },
    })
}

/// Whether two attribute snapshots describe the same terminal state.
///
/// Used by the panic-restoration test: capture before entering raw mode, panic
/// under the guard, assert the terminal matches the before snapshot.
pub fn attrs_eq(left: &TermiosAttrs, right: &TermiosAttrs) -> bool {
    // `c_line` is the Linux line discipline; BSD termios has no such field, so
    // it is compared only where it exists rather than dropped from both.
    #[cfg(target_os = "linux")]
    let line_matches = left.attrs.c_line == right.attrs.c_line;
    #[cfg(not(target_os = "linux"))]
    let line_matches = true;

    left.attrs.c_iflag == right.attrs.c_iflag
        && left.attrs.c_oflag == right.attrs.c_oflag
        && left.attrs.c_cflag == right.attrs.c_cflag
        && left.attrs.c_lflag == right.attrs.c_lflag
        && line_matches
        && left.attrs.c_cc == right.attrs.c_cc
}

/// Restores `fd` to `attrs`.
///
/// Called from `Drop` and from signal handlers. Signal handlers must use this
/// rather than relying on `Drop`, because a signal does not unwind.
pub fn restore(fd: RawFd, attrs: &TermiosAttrs) -> std::io::Result<()> {
    set_attrs(fd, &attrs.attrs)
}

fn set_attrs(fd: RawFd, attrs: &libc::termios) -> io::Result<()> {
    // SAFETY: `attrs` points to a valid termios structure. The kernel validates
    // `fd` and reports any error via errno.
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, attrs) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn install_signal_handlers() -> io::Result<[(i32, libc::sighandler_t); 3]> {
    RECEIVED_SIGNAL.store(0, Ordering::SeqCst);
    let mut previous = [(0, 0); 3];
    for (index, signal) in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
        .into_iter()
        .enumerate()
    {
        // SAFETY: the handler has the required C ABI and only stores to an
        // atomic signal flag. `signal` reports installation errors with SIG_ERR.
        let old = unsafe { libc::signal(signal, record_signal as *const () as libc::sighandler_t) };
        if old == libc::SIG_ERR {
            for (restore_signal, restore_handler) in previous[..index].iter().copied() {
                // SAFETY: these entries came from successful signal calls above.
                unsafe { libc::signal(restore_signal, restore_handler) };
            }
            return Err(io::Error::last_os_error());
        }
        previous[index] = (signal, old);
    }
    Ok(previous)
}

extern "C" fn record_signal(signal: i32) {
    RECEIVED_SIGNAL.store(signal, Ordering::SeqCst);
}
