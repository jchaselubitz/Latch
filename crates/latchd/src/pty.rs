//! PTY allocation and child spawn.
//!
//! The child is a session leader with the PTY slave as its controlling
//! terminal, in its own process group, so signalling `-pid` reaches the whole
//! job for lifecycle and signal operations.

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

/// Exit status the child reports when it cannot enter the requested cwd.
///
/// A program that ends up running somewhere other than where it was asked to
/// is a hazard, not a convenience, so the launch fails instead of falling
/// through to the daemon's own directory.
pub const EXIT_BAD_CWD: i32 = 126;

/// Signals whose disposition and mask the child resets before exec.
///
/// `SIG_IGN` and a blocked mask both survive `exec`, so whatever the daemon's
/// ancestors left behind would otherwise reach the user's shell: a shell that
/// cannot be interrupted, or a job that never sees `SIGCHLD`.
const RESET_SIGNALS: [libc::c_int; 13] = [
    libc::SIGHUP,
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGPIPE,
    libc::SIGALRM,
    libc::SIGTERM,
    libc::SIGUSR1,
    libc::SIGUSR2,
    libc::SIGCHLD,
    libc::SIGTSTP,
    libc::SIGTTIN,
    libc::SIGTTOU,
    libc::SIGWINCH,
];

/// Spawns `argv` in a fresh PTY of the given size.
///
/// `env` replaces the child's environment entirely when `Some`; `None`
/// inherits the daemon's. `cwd` is entered before exec; if it cannot be, the
/// child exits with [`EXIT_BAD_CWD`] after saying so on the terminal.
///
/// Must be called while the daemon is single-threaded: the child runs
/// between `fork` and `exec`, where only async-signal-safe calls are legal.
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
            libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY as _, 0);
            libc::dup2(slave.as_raw_fd(), 0);
            libc::dup2(slave.as_raw_fd(), 1);
            libc::dup2(slave.as_raw_fd(), 2);
            if slave.as_raw_fd() > 2 {
                libc::close(slave.as_raw_fd());
            }
            libc::close(master.as_raw_fd());
            // Start the program with a clean signal slate: default
            // dispositions and nothing blocked, whatever the daemon or its
            // ancestors had.
            for signal in RESET_SIGNALS {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigprocmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
            if libc::chdir(cwd.as_ptr()) != 0 {
                const MESSAGE: &[u8] = b"latchd: cannot enter the session directory\r\n";
                let _ = libc::write(2, MESSAGE.as_ptr().cast(), MESSAGE.len());
                libc::_exit(EXIT_BAD_CWD);
            }
            if env_strings.is_some() {
                libc::execve(program.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr());
            } else {
                libc::execv(program.as_ptr(), argv_ptrs.as_ptr());
            }
            const FAILED: &[u8] = b"latchd: exec failed\r\n";
            let _ = libc::write(2, FAILED.as_ptr().cast(), FAILED.len());
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
///
/// `pid` must be a child this daemon spawned and has not yet reaped: once a
/// pid is reaped it can be reissued to an unrelated process of the same user,
/// and `kill(-pid)` would then reach that process's whole group.
pub fn signal_group(pid: i32, signal: i32) -> io::Result<()> {
    if pid <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refusing to signal a non-positive pid",
        ));
    }
    if signal < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "signal numbers are non-negative",
        ));
    }
    // SAFETY: signalling a pid this daemon spawned and still owns.
    if unsafe { libc::kill(-pid, signal) } == 0 {
        return Ok(());
    }
    if unsafe { libc::kill(pid, signal) } == 0 {
        return Ok(());
    }
    Err(io::Error::last_os_error())
}

/// How a child ended: `(exit status, terminating signal)`.
pub type ExitStatus = (Option<i32>, Option<i32>);

/// Blocks until `pid` has ended and reports how, **without reaping it**.
///
/// The child stays a zombie — its pid cannot be reissued — until [`reap`]
/// runs, which lets the daemon record the exit before anything could confuse
/// a recycled pid for the session. Falls back to a reaping wait where
/// `waitid` is unavailable.
pub fn wait_exit(pid: i32) -> ExitStatus {
    loop {
        // SAFETY: waitid writes a siginfo we own for a child this daemon spawned.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            let (code, status) = child_info(&info);
            return match code {
                libc::CLD_EXITED => (Some(status), None),
                libc::CLD_KILLED | libc::CLD_DUMPED => (None, Some(status)),
                _ => (None, None),
            };
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        return reap(pid);
    }
}

#[cfg(target_os = "linux")]
fn child_info(info: &libc::siginfo_t) -> (i32, i32) {
    // SAFETY: si_status is valid for a CLD_* siginfo, which is what waitid
    // with WEXITED produces.
    (info.si_code, unsafe { info.si_status() })
}

#[cfg(not(target_os = "linux"))]
fn child_info(info: &libc::siginfo_t) -> (i32, i32) {
    (info.si_code, info.si_status)
}

/// Reaps `pid` and returns `(status, signal)`; blocks if it is still running.
pub fn reap(pid: i32) -> ExitStatus {
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

/// Blocks until `pid` ends, reaps it, and returns `(status, signal)`.
pub fn wait(pid: i32) -> ExitStatus {
    let exit = wait_exit(pid);
    let _ = reap(pid);
    exit
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_exit_reports_before_reaping() {
        let child = spawn(
            &["/bin/sh".into(), "-c".into(), "exit 7".into()],
            Path::new("/"),
            None,
            10,
            2,
        )
        .unwrap();
        assert_eq!(wait_exit(child.pid), (Some(7), None));
        // Still a zombie: signal 0 finds it, and a second look agrees.
        // SAFETY: signal zero only asks whether the pid exists.
        assert_eq!(unsafe { libc::kill(child.pid, 0) }, 0);
        assert_eq!(wait_exit(child.pid), (Some(7), None));
        assert_eq!(reap(child.pid), (Some(7), None));
    }

    #[test]
    fn wait_exit_reports_a_signal() {
        let child = spawn(
            &["/bin/sh".into(), "-c".into(), "kill -TERM $$".into()],
            Path::new("/"),
            None,
            10,
            2,
        )
        .unwrap();
        assert_eq!(wait(child.pid), (None, Some(libc::SIGTERM)));
    }

    #[test]
    fn a_missing_cwd_fails_the_launch_instead_of_running_elsewhere() {
        let child = spawn(
            &["/bin/sh".into(), "-c".into(), "pwd".into()],
            Path::new("/definitely/not/a/directory"),
            None,
            10,
            2,
        )
        .unwrap();
        assert_eq!(wait(child.pid), (Some(EXIT_BAD_CWD), None));
    }

    #[test]
    fn signal_group_refuses_pids_that_could_be_anyone() {
        assert!(signal_group(0, 0).is_err());
        assert!(signal_group(-1, 0).is_err());
        assert!(signal_group(std::process::id() as i32, -1).is_err());
    }
}
