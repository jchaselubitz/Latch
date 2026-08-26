//! PTY allocation and child spawn.
//!
//! The child is a session leader with the PTY slave as its controlling
//! terminal, in its own process group, so signalling `-pid` reaches the whole
//! job the way tmux's `pane_pid` does.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

/// A spawned child and the master side of its terminal.
pub struct PtyChild {
    /// Master fd. Reads are child output; writes are child input.
    pub master: OwnedFd,
    /// Child pid; also its process group id.
    pub pid: i32,
}

/// Spawns `argv` in a fresh PTY of the given size.
///
/// `env` replaces the child's environment entirely when `Some`; `None`
/// inherits the daemon's. `cwd` is entered before exec.
pub fn spawn(
    argv: &[String],
    cwd: &Path,
    env: Option<&[(String, String)]>,
    cols: u16,
    rows: u16,
) -> io::Result<PtyChild> {
    if argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    #[cfg(target_os = "linux")]
    let winsize = winsize(cols, rows);
    #[cfg(not(target_os = "linux"))]
    let mut winsize = winsize(cols, rows);
    // libc follows each platform's openpty declaration: Linux takes a const
    // winsize pointer while Darwin exposes a mutable one. Preserve that
    // distinction so the Linux -D warnings gate does not need a lint waiver.
    #[cfg(target_os = "linux")]
    let winsize_ptr: *const libc::winsize = &winsize;
    #[cfg(not(target_os = "linux"))]
    let winsize_ptr: *mut libc::winsize = &mut winsize;
    // SAFETY: openpty writes two fds and reads a winsize we own.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            winsize_ptr,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both fds were just returned by openpty and are owned here.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    set_cloexec(master.as_raw_fd())?;

    let program = CString::new(argv[0].as_str())?;
    let args: Vec<CString> = argv
        .iter()
        .map(|arg| CString::new(arg.as_str()))
        .collect::<Result<_, _>>()?;
    let mut argv_ptrs: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(std::ptr::null());
    let env_strings: Option<Vec<CString>> = env
        .map(|pairs| {
            pairs
                .iter()
                .map(|(k, v)| CString::new(format!("{k}={v}")))
                .collect::<Result<_, _>>()
        })
        .transpose()?;
    let mut env_ptrs: Vec<*const libc::c_char> = env_strings
        .as_ref()
        .map(|list| list.iter().map(|e| e.as_ptr()).collect())
        .unwrap_or_default();
    env_ptrs.push(std::ptr::null());
    let cwd = CString::new(cwd.as_os_str().as_encoded_bytes())?;

    // SAFETY: between fork and exec the child only calls async-signal-safe
    // functions on fds and C strings prepared above.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid == 0 {
        // Child.
        unsafe {
            libc::setsid();
            #[cfg(target_os = "macos")]
            libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
            #[cfg(not(target_os = "macos"))]
            libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
            libc::dup2(slave.as_raw_fd(), 0);
            libc::dup2(slave.as_raw_fd(), 1);
            libc::dup2(slave.as_raw_fd(), 2);
            if slave.as_raw_fd() > 2 {
                libc::close(slave.as_raw_fd());
            }
            libc::close(master.as_raw_fd());
            // Reset signal dispositions the daemon may have changed.
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGHUP, libc::SIG_DFL);
            if libc::chdir(cwd.as_ptr()) != 0 {
                let _ = libc::write(2, b"latchd: cannot enter cwd\n".as_ptr().cast(), 25);
            }
            if env_strings.is_some() {
                libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr());
            } else {
                libc::execv(program.as_ptr(), argv_ptrs.as_ptr());
            }
            let _ = libc::write(2, b"latchd: exec failed\n".as_ptr().cast(), 20);
            libc::_exit(127);
        }
    }
    drop(slave);
    Ok(PtyChild { master, pid })
}

fn set_cloexec(fd: RawFd) -> io::Result<()> {
    // SAFETY: plain fcntl on an fd we own.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// Sets the terminal size on a master fd. The kernel raises `SIGWINCH` in the
/// child's foreground process group.
pub fn resize(master: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let size = winsize(cols, rows);
    // SAFETY: TIOCSWINSZ reads a winsize we own.
    if unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Reads the current terminal size of `fd`.
pub fn size_of(fd: RawFd) -> io::Result<(u16, u16)> {
    let mut size = winsize(0, 0);
    // SAFETY: TIOCGWINSZ writes a winsize we own.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((size.ws_col, size.ws_row))
}

/// Signals a process group, falling back to the process itself.
pub fn signal_group(pid: i32, signal: i32) -> io::Result<()> {
    // SAFETY: signalling a pid this daemon spawned.
    if unsafe { libc::kill(-pid, signal) } == 0 {
        return Ok(());
    }
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

/// Blocks until `pid` ends and returns `(status, signal)`.
pub fn wait(pid: i32) -> (Option<i32>, Option<i32>) {
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: waiting on a child this daemon spawned.
        let rc = unsafe { libc::waitpid(pid, &mut status, 0) };
        if rc == pid {
            break;
        }
        if rc < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return (None, None);
    }
    if libc::WIFEXITED(status) {
        (Some(libc::WEXITSTATUS(status)), None)
    } else if libc::WIFSIGNALED(status) {
        (None, Some(libc::WTERMSIG(status)))
    } else {
        (None, None)
    }
}

/// Wraps a raw master fd as a `File` for reading without taking ownership.
///
/// The returned file must not outlive the owner; callers use it for one
/// thread's read loop while the [`OwnedFd`] in the daemon keeps the fd alive.
pub fn dup_file(fd: RawFd) -> io::Result<File> {
    // SAFETY: dup returns a fresh descriptor we then own.
    let duplicate = unsafe { libc::dup(fd) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    set_cloexec(duplicate)?;
    Ok(unsafe { File::from_raw_fd(duplicate) })
}
