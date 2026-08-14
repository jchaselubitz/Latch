//! Per-client PTY wrapping `latch attach`.

use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::engine::DEFAULT_TERMINAL;

#[cfg(target_os = "linux")]
#[link(name = "util")]
extern "C" {}

/// Inputs for spawning one attach client.
pub struct SpawnAttachRequest<'a> {
    /// Path to the `latch` binary.
    pub latch_bin: &'a Path,
    /// Session id passed to `latch attach`.
    pub session_id: &'a str,
    /// Initial columns.
    pub cols: u16,
    /// Initial rows.
    pub rows: u16,
}

/// A live attach process sitting on a PTY master.
pub struct PtyChild {
    /// PTY master. Bytes written here are keystrokes; bytes read are output.
    pub master: File,
    child: Child,
}

impl PtyChild {
    /// Spawns `latch attach <session>` with the PTY as its controlling terminal.
    pub fn spawn(request: SpawnAttachRequest<'_>) -> io::Result<Self> {
        let mut master_fd: libc::c_int = -1;
        let mut slave_fd: libc::c_int = -1;
        let mut size = libc::winsize {
            ws_row: request.rows.max(1),
            ws_col: request.cols.max(1),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // The winsize parameter is `*mut winsize` on Apple platforms and
        // `*const winsize` on glibc. A raw `*mut` satisfies both — it coerces
        // to `*const` — where a `&mut` borrow reads as gratuitous on Linux.
        let size_ptr: *mut libc::winsize = &mut size;
        // SAFETY: `openpty` writes the two file descriptors and optional name
        // and termios. We pass null for the unused outputs and stack storage
        // for the fds and winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                size_ptr,
            )
        };
        if opened != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `openpty` succeeded, so both descriptors are open and uniquely
        // owned by this function.
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        set_cloexec(master.as_raw_fd())?;
        set_nonblocking(master.as_raw_fd())?;

        set_cloexec(slave.as_raw_fd())?;
        let mut command = Command::new(request.latch_bin);
        command
            .arg("attach")
            .arg(request.session_id)
            .env("TERM", DEFAULT_TERMINAL)
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave.try_clone()?));
        // SAFETY: the closure runs after fork in the child. `setsid` and
        // `ioctl(TIOCSCTTY)` are async-signal-safe and make the slave the
        // controlling terminal before exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(slave);
        Ok(Self { master, child })
    }

    /// Asks the attach process to exit.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        self.kill();
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is an open descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is an open descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
