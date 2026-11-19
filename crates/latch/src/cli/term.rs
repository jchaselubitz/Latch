//! Terminal geometry of the invoking terminal.

use std::os::fd::RawFd;

use crate::session::manifest::TerminalSize;

/// Reads a terminal's current character-cell geometry.
///
/// The same function supplies the child's initial PTY size and every attach or
/// resize message. Keeping that platform query here prevents creation and
/// attachment from quietly disagreeing about what the iTerm window looks like.
pub fn terminal_size(fd: RawFd) -> TerminalSize {
    let mut winsize = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: `winsize` is writable storage and the kernel validates `fd`.
    let has_size = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, winsize.as_mut_ptr()) } != -1;
    if has_size {
        // SAFETY: a successful TIOCGWINSZ initialized `winsize`.
        let winsize = unsafe { winsize.assume_init() };
        if winsize.ws_col > 0 && winsize.ws_row > 0 {
            return TerminalSize {
                cols: winsize.ws_col,
                rows: winsize.ws_row,
            };
        }
    }
    TerminalSize { cols: 80, rows: 24 }
}
