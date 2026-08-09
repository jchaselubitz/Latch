//! The PTY and the child's process group.
//!
//! The worker allocates a PTY, puts the child in a process group of its own, and
//! keeps the master side. Two consequences carry most of the product:
//!
//! **The child is in its own group, and the worker knows the group id because it
//! created it.** Stopping a session is `killpg` on that number. It is never read
//! from a file, so there is no window in which a recycled PID belongs to somebody
//! else's process — the structural version of the rule, rather than a discipline
//! to remember.
//!
//! **The child's fate is not tied to any client.** The PTY's foreground process
//! group is the child's, not the launching terminal's, so a `SIGHUP` to the
//! window that started the session reaches nothing. That is what makes closing a
//! terminal a detach rather than a kill.

use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use latch_protocol::control::Size;

use crate::worker::manifest::LaunchSpec;

/// A signal this worker sends.
///
/// A closed set rather than a raw `i32`: the only signals the worker has any
/// business sending are the two halves of a stop, and naming them is what lets
/// `exit.json` record `SIGTERM` instead of `15`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// `SIGHUP`.
    Hangup,
    /// `SIGINT`.
    Interrupt,
    /// `SIGTERM` — the graceful half of a stop.
    Terminate,
    /// `SIGKILL` — the forced half, which cannot be caught.
    Kill,
}

impl Signal {
    /// The platform signal number.
    pub fn as_raw(self) -> i32 {
        match self {
            Self::Hangup => libc::SIGHUP,
            Self::Interrupt => libc::SIGINT,
            Self::Terminate => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }

    /// The name recorded in `exit.json`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Hangup => "SIGHUP",
            Self::Interrupt => "SIGINT",
            Self::Terminate => "SIGTERM",
            Self::Kill => "SIGKILL",
        }
    }

    /// Recognizes a platform signal number, for reporting a child's death.
    pub fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            n if n == libc::SIGHUP => Some(Self::Hangup),
            n if n == libc::SIGINT => Some(Self::Interrupt),
            n if n == libc::SIGTERM => Some(Self::Terminate),
            n if n == libc::SIGKILL => Some(Self::Kill),
            _ => None,
        }
    }
}

/// A process group identifier.
///
/// A distinct type from a PID because the two are never interchangeable here: a
/// stop signals a group, and passing a PID where a group is meant would signal
/// one process out of a pipeline and call the session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessGroup(i32);

impl ProcessGroup {
    /// Wraps a group id the caller obtained by creating the group.
    pub fn from_raw(pgid: i32) -> Self {
        Self(pgid)
    }

    /// The raw group id.
    pub fn as_raw(self) -> i32 {
        self.0
    }
}

impl std::fmt::Display for ProcessGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How the child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExit {
    /// Exit status, or `None` when the child was signalled.
    pub code: Option<i32>,
    /// The signal that ended it, or `None` on a normal exit.
    pub signal: Option<Signal>,
}

impl ChildExit {
    /// A normal exit with `code`.
    pub const fn code(code: i32) -> Self {
        Self {
            code: Some(code),
            signal: None,
        }
    }

    /// A death by `signal`.
    pub const fn signalled(signal: Signal) -> Self {
        Self {
            code: None,
            signal: Some(signal),
        }
    }

    /// A process status for the worker itself to exit with.
    ///
    /// A signalled child becomes `128 + signal`, the shell convention, so a
    /// caller that only looks at the worker's status sees the same thing it
    /// would have seen running the command directly.
    pub fn status_code(&self) -> i32 {
        match (self.code, self.signal) {
            (Some(code), _) => code,
            (None, Some(signal)) => 128 + signal.as_raw(),
            (None, None) => 0,
        }
    }
}

/// How a stop escalates.
///
/// A graceful stop that never escalates is a session that cannot be stopped,
/// which is worse than a forceful one: an agent that traps `SIGTERM` and blocks
/// would leave `latch stop` hanging forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopPolicy {
    /// Sent first, to the child's process group.
    pub graceful: Signal,
    /// How long the child is given to act on it.
    pub grace: Duration,
    /// Sent to the group when the interval elapses.
    pub forced: Signal,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            graceful: Signal::Terminate,
            grace: Duration::from_millis(crate::worker::manifest::DEFAULT_STOP_GRACE_MS),
            forced: Signal::Kill,
        }
    }
}

/// Anything that can go wrong with the PTY or the child.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// A PTY could not be allocated or configured.
    #[error("cannot allocate a pty: {0}")]
    Pty(#[source] io::Error),
    /// The child could not be started.
    #[error("cannot start {program}: {source}")]
    Spawn {
        /// The program that could not be run.
        program: String,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
    /// The working directory is unusable.
    #[error("cannot use {cwd} as a working directory: {source}")]
    Cwd {
        /// The directory.
        cwd: PathBuf,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
    /// A signal could not be delivered to the child's process group.
    #[error("cannot signal process group {group}: {source}")]
    Signal {
        /// The group being signalled.
        group: ProcessGroup,
        /// Underlying failure.
        #[source]
        source: io::Error,
    },
    /// Reading, writing, or resizing the PTY failed.
    #[error("pty io failed: {0}")]
    Io(#[from] io::Error),
}

/// The child process, its process group, and the master side of its PTY.
///
/// Everything about the child that the worker can act on is reachable from this
/// value and from nowhere else. There is no accessor that returns a PID, because
/// the only operations the worker performs on the child are group operations.
pub struct ChildProcess {
    master: File,
    child_pid: i32,
    group: ProcessGroup,
}

impl std::fmt::Debug for ChildProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildProcess").finish_non_exhaustive()
    }
}

impl ChildProcess {
    /// Allocates a PTY and starts `spec` on it, in a new process group.
    ///
    /// The child calls `setsid` and acquires the PTY slave as its controlling
    /// terminal, so it leads its own session and its own group. Nothing about the
    /// worker's own group or controlling terminal is inherited by it.
    pub fn spawn(spec: &LaunchSpec) -> Result<Self, ProcessError> {
        if !spec.cwd.is_dir() {
            return Err(ProcessError::Cwd {
                cwd: spec.cwd.clone(),
                source: io::Error::new(io::ErrorKind::NotFound, "not a directory"),
            });
        }
        let mut master = -1;
        let mut slave = -1;
        let mut window = libc::winsize {
            ws_row: spec.size.rows,
            ws_col: spec.size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Darwin declares the trailing `openpty` parameters `*mut`, Linux
        // `*const`. Mutable pointers satisfy both: `*mut T` weakens to `*const
        // T`, so the call compiles on either without a `cfg`. `openpty` reads
        // through them and does not write.
        let termp = std::ptr::null_mut::<libc::termios>();
        // SAFETY: `openpty` initializes the supplied fd pointers and copies the
        // window size; all pointers remain valid for the call.
        if unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                termp,
                &mut window as *mut _,
            )
        } == -1
        {
            return Err(ProcessError::Pty(io::Error::last_os_error()));
        }
        // SAFETY: fork creates an isolated child; only async-signal-safe libc
        // calls are made before exec in that branch.
        let pid = unsafe { libc::fork() };
        if pid == -1 {
            // SAFETY: these descriptors were returned by openpty above.
            unsafe {
                libc::close(master);
                libc::close(slave);
            }
            return Err(ProcessError::Spawn {
                program: spec.launch_program(),
                source: io::Error::last_os_error(),
            });
        }
        if pid == 0 {
            // SAFETY: all calls use fds supplied by openpty or byte strings with
            // static NUL terminators. Failure cannot be reported to the parent
            // safely, so the child exits with the shell's conventional 127.
            unsafe {
                libc::close(master);
                if libc::setsid() == -1
                    || libc::ioctl(slave, libc::TIOCSCTTY as _, 0) == -1
                    || libc::dup2(slave, libc::STDIN_FILENO) == -1
                    || libc::dup2(slave, libc::STDOUT_FILENO) == -1
                    || libc::dup2(slave, libc::STDERR_FILENO) == -1
                {
                    libc::_exit(127);
                }
                if slave > libc::STDERR_FILENO {
                    libc::close(slave);
                }
                if !spec.inherit_env {
                    clear_environment();
                }
                for (key, value) in &spec.env {
                    let Ok(key) = CString::new(key.as_bytes()) else {
                        libc::_exit(127);
                    };
                    let Ok(value) = CString::new(value.as_bytes()) else {
                        libc::_exit(127);
                    };
                    if libc::setenv(key.as_ptr(), value.as_ptr(), 1) == -1 {
                        libc::_exit(127);
                    }
                }
                let Ok(term) = CString::new(spec.term.as_bytes()) else {
                    libc::_exit(127);
                };
                if libc::setenv(c"TERM".as_ptr(), term.as_ptr(), 1) == -1 {
                    libc::_exit(127);
                }
                let Ok(cwd) = CString::new(spec.cwd.as_os_str().as_bytes()) else {
                    libc::_exit(127);
                };
                if libc::chdir(cwd.as_ptr()) == -1 {
                    libc::_exit(127);
                }
                let Ok(argv) = spec
                    .argv
                    .iter()
                    .map(|arg| CString::new(arg.as_bytes()))
                    .collect::<Result<Vec<_>, _>>()
                else {
                    libc::_exit(127);
                };
                let mut argv_ptrs: Vec<*const libc::c_char> =
                    argv.iter().map(|arg| arg.as_ptr()).collect();
                argv_ptrs.push(std::ptr::null());
                libc::execvp(argv[0].as_ptr(), argv_ptrs.as_ptr());
                libc::_exit(127);
            }
        }
        // SAFETY: parent owns the master and no longer needs the slave.
        unsafe { libc::close(slave) };
        // SAFETY: master is uniquely owned by this process after closing slave.
        let master = unsafe { File::from_raw_fd(master) };
        // SAFETY: master is a valid owned descriptor. Nonblocking reads let the
        // worker poll the control socket without ever waiting on PTY output.
        let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == -1
        {
            return Err(ProcessError::Pty(io::Error::last_os_error()));
        }
        Ok(Self {
            master,
            child_pid: pid,
            group: ProcessGroup::from_raw(pid),
        })
    }

    /// The child's process group — the only thing a stop ever signals.
    pub fn process_group(&self) -> ProcessGroup {
        self.group
    }

    /// The master PTY descriptor, for waiting on it alongside the socket.
    ///
    /// Borrowed for the duration of a `poll`, never stored. Exposed so the
    /// worker can sleep until something actually happens instead of waking on a
    /// timer: a machine with thirty sessions open has thirty of these processes,
    /// and thirty spinning loops is a laptop fan.
    pub fn pty_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    /// Reads whatever the child has written.
    ///
    /// Returns `Ok(0)` when the PTY reaches end of file, which on every platform
    /// Latch targets is how the child's exit is first noticed.
    pub fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match io::Read::read(&mut self.master, buf) {
            Err(error) if error.raw_os_error() == Some(libc::EIO) => Ok(0),
            result => result,
        }
    }

    /// Writes client input to the child.
    pub fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        io::Write::write(&mut self.master, bytes)
    }

    /// Sets the PTY window size and delivers `SIGWINCH` to the child's group.
    pub fn resize(&mut self, size: Size) -> Result<(), ProcessError> {
        let window = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: master is a valid PTY fd and `window` is valid for the call.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &window) } == -1 {
            return Err(ProcessError::Io(io::Error::last_os_error()));
        }
        self.signal_group_raw(libc::SIGWINCH)
    }

    /// Reports the child's exit if it has already exited, without blocking.
    pub fn try_wait(&mut self) -> Result<Option<ChildExit>, ProcessError> {
        let mut status = 0;
        // SAFETY: `child_pid` was returned by fork and status points to valid storage.
        let result = unsafe { libc::waitpid(self.child_pid, &mut status, libc::WNOHANG) };
        if result == 0 {
            Ok(None)
        } else if result == self.child_pid {
            Ok(Some(decode_exit(status)))
        } else {
            Err(ProcessError::Io(io::Error::last_os_error()))
        }
    }

    /// Blocks until the child exits, or until `timeout` elapses.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ChildExit>, ProcessError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(exit) = self.try_wait()? {
                return Ok(Some(exit));
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Sends `signal` to the child's process group.
    ///
    /// The group, never the process: a shell that started a pipeline has
    /// children of its own, and signalling only the leader leaves them running
    /// while reporting the session stopped.
    pub fn signal_group(&self, signal: Signal) -> Result<(), ProcessError> {
        self.signal_group_raw(signal.as_raw())
    }

    /// Stops the child: the graceful signal, then the forced one after the
    /// policy's interval, returning how it actually ended.
    pub fn stop(&mut self, policy: StopPolicy) -> Result<ChildExit, ProcessError> {
        self.signal_group(policy.graceful)?;
        if let Some(exit) = self.wait_timeout(policy.grace)? {
            return Ok(exit);
        }
        self.signal_group(policy.forced)?;
        loop {
            if let Some(exit) = self.wait_timeout(Duration::from_secs(1))? {
                return Ok(exit);
            }
        }
    }

    fn signal_group_raw(&self, signal: i32) -> Result<(), ProcessError> {
        // SAFETY: a negative PID addresses the process group created for this child.
        if unsafe { libc::kill(-self.group.as_raw(), signal) } == -1 {
            let error = io::Error::last_os_error();
            // A group that is not there to be signalled has already satisfied
            // the request. `ESRCH` is the portable spelling of that; Darwin
            // answers `EPERM` once every member has exited and been reaped,
            // where Linux answers `ESRCH`.
            //
            // Treating `EPERM` as done is right under either reading of it. If
            // the group is gone, there is nothing to signal. If the number has
            // since been recycled onto processes that are not this worker's,
            // then not signalling them is exactly the rule this module is
            // built on — the one place a stop could reach the wrong process
            // group is the one place it must not.
            //
            // The stop path does not take this on trust: it goes on to reap the
            // child, so "signalled nothing" and "stopped" stay distinguishable.
            if matches!(error.raw_os_error(), Some(libc::ESRCH) | Some(libc::EPERM)) {
                return Ok(());
            }
            return Err(ProcessError::Signal {
                group: self.group,
                source: error,
            });
        }
        Ok(())
    }
}

/// Empties the environment in the post-`fork` child, before it is rebuilt from
/// the launch spec.
///
/// `clearenv` is a glibc extension that Darwin does not provide. Darwin instead
/// reaches `environ` through `_NSGetEnviron`, so the equivalent is pointing it
/// at an already-terminated empty vector. Both forms leave the `setenv` calls
/// that follow building the child's environment from nothing.
///
/// # Safety
///
/// Must be called only in the child of a `fork`, which is the one context where
/// replacing the process-wide environment races with nothing.
#[cfg(target_os = "linux")]
unsafe fn clear_environment() {
    // SAFETY: the caller guarantees this is the child of a `fork`, so replacing
    // the process-wide environment races with nothing.
    unsafe { libc::clearenv() };
}

#[cfg(not(target_os = "linux"))]
unsafe fn clear_environment() {
    extern "C" {
        /// Darwin exposes `environ` to a linked image through this accessor
        /// rather than as a directly linkable symbol.
        fn _NSGetEnviron() -> *mut *mut *mut libc::c_char;
    }
    static mut EMPTY: [*mut libc::c_char; 1] = [std::ptr::null_mut()];
    // SAFETY: `_NSGetEnviron` always returns a valid pointer to the process's
    // `environ` slot, and `EMPTY` is a `'static` NUL-terminated vector, so the
    // environment stays well-formed for the `setenv` calls that follow. The
    // caller guarantees this is the child of a `fork`, so the write races with
    // nothing.
    unsafe { *_NSGetEnviron() = std::ptr::addr_of_mut!(EMPTY).cast() };
}

fn decode_exit(status: i32) -> ChildExit {
    if libc::WIFEXITED(status) {
        ChildExit::code(libc::WEXITSTATUS(status))
    } else if libc::WIFSIGNALED(status) {
        ChildExit::signalled(Signal::from_raw(libc::WTERMSIG(status)).unwrap_or(Signal::Kill))
    } else {
        ChildExit::code(0)
    }
}

trait LaunchProgram {
    fn launch_program(&self) -> String;
}

impl LaunchProgram for LaunchSpec {
    fn launch_program(&self) -> String {
        self.argv.first().cloned().unwrap_or_default()
    }
}
