//! Client side of the kernel protocol.
//!
//! `latch` links this to create, observe, drive, and attach to sessions. The
//! attach path ([`attach_tty`]) is the human surface: it puts the calling
//! terminal in raw mode, paints the one snapshot the daemon sends, and then
//! splices bytes in both directions until the daemon closes the connection.

use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::protocol::{
    self, Event, ReleaseReason, Reply, Request, Response, SnapshotFormat, Stat, PROTOCOL_VERSION,
};
use crate::pty;

/// A client-side failure.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The socket could not be reached.
    #[error("cannot reach the session kernel: {0}")]
    Connect(#[source] io::Error),
    /// The connection broke mid-exchange.
    #[error("session kernel connection failed: {0}")]
    Io(#[from] io::Error),
    /// The daemon answered with an error.
    #[error("{0}")]
    Kernel(String),
    /// The daemon closed without answering.
    #[error("session kernel closed the connection")]
    Closed,
}

/// One control connection.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connects to a session socket.
    pub fn connect(socket: &Path) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(socket).map_err(ClientError::Connect)?;
        Ok(Self { stream })
    }

    /// Connects with a deadline on every later exchange.
    pub fn connect_with_timeout(socket: &Path, timeout: Duration) -> Result<Self, ClientError> {
        let client = Self::connect(socket)?;
        client.stream.set_read_timeout(Some(timeout))?;
        client.stream.set_write_timeout(Some(timeout))?;
        Ok(client)
    }

    /// Sends one request and reads its response.
    pub fn call(&mut self, request: &Request) -> Result<Reply, ClientError> {
        protocol::write_frame(&mut self.stream, request)?;
        let response: Response =
            protocol::read_frame(&mut self.stream)?.ok_or(ClientError::Closed)?;
        response.into_result().map_err(ClientError::Kernel)
    }

    /// Live facts.
    pub fn stat(&mut self) -> Result<Stat, ClientError> {
        self.call(&Request::Stat)?
            .stat
            .ok_or_else(|| ClientError::Kernel("stat reply carried no facts".into()))
    }

    /// The current frame in `format`.
    pub fn snapshot(
        &mut self,
        format: SnapshotFormat,
        scrollback_lines: u32,
    ) -> Result<Reply, ClientError> {
        self.call(&Request::Snapshot {
            format,
            scrollback_lines,
        })
    }

    /// Turns this connection into an event stream.
    pub fn subscribe(mut self) -> Result<Subscription, ClientError> {
        self.call(&Request::Subscribe)?;
        Ok(Subscription {
            stream: self.stream,
        })
    }
}

/// An event stream.
pub struct Subscription {
    stream: UnixStream,
}

impl Subscription {
    /// The next event, or `None` when the daemon closed the stream.
    pub fn recv(&mut self) -> Result<Option<Event>, ClientError> {
        Ok(protocol::read_frame(&mut self.stream)?)
    }
}

/// One-shot control call.
pub fn call(socket: &Path, request: &Request) -> Result<Reply, ClientError> {
    Client::connect(socket)?.call(request)
}

/// One-shot control call with a deadline.
pub fn call_with_timeout(
    socket: &Path,
    request: &Request,
    timeout: Duration,
) -> Result<Reply, ClientError> {
    Client::connect_with_timeout(socket, timeout)?.call(request)
}

/// One-shot `stat`.
pub fn stat(socket: &Path) -> Result<Stat, ClientError> {
    Client::connect(socket)?.stat()
}

/// Why surface `surface` ended, asked after its connection closed.
pub fn release_reason(socket: &Path, surface: u64) -> Result<ReleaseReason, ClientError> {
    call(socket, &Request::ReleaseReason { surface })?
        .reason
        .ok_or_else(|| ClientError::Kernel("release reply carried no reason".into()))
}

/// A surface connection after the handshake.
pub struct Surface {
    /// Raw stream: child output arrives, child input departs.
    pub stream: UnixStream,
    /// Surface id, for [`release_reason`].
    pub id: u64,
    /// The current frame to paint before any live byte.
    pub snapshot: Vec<u8>,
}

/// Takes the surface at the given size.
pub fn attach(socket: &Path, cols: u16, rows: u16) -> Result<Surface, ClientError> {
    let mut stream = UnixStream::connect(socket).map_err(ClientError::Connect)?;
    protocol::write_frame(
        &mut stream,
        &Request::Attach {
            cols,
            rows,
            protocol: PROTOCOL_VERSION,
        },
    )?;
    let response: Response = protocol::read_frame(&mut stream)?.ok_or(ClientError::Closed)?;
    let reply = response.into_result().map_err(ClientError::Kernel)?;
    let id = reply
        .surface
        .ok_or_else(|| ClientError::Kernel("attach reply carried no surface id".into()))?;
    let mut snapshot = vec![0u8; reply.snapshot_len.unwrap_or(0)];
    stream.read_exact(&mut snapshot)?;
    Ok(Surface {
        stream,
        id,
        snapshot,
    })
}

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}

struct RawGuard {
    fd: i32,
    saved: libc::termios,
}

impl RawGuard {
    fn enter(fd: i32) -> io::Result<Self> {
        // SAFETY: termios structs are plain data the calls fill in.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, saved })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: restoring attributes captured by `enter`.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// Attaches the calling terminal as the session's surface and blocks until
/// the daemon releases it. Returns why.
///
/// Stdin must be a terminal. The terminal is restored on every return path.
pub fn attach_tty(socket: &Path) -> Result<ReleaseReason, ClientError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let in_fd = stdin.as_raw_fd();
    let out_fd = stdout.as_raw_fd();
    let (cols, rows) = pty::size_of(out_fd).or_else(|_| pty::size_of(in_fd))?;

    // Take the surface before touching the tty, so a failed attach leaves the
    // terminal exactly as it was.
    let surface = attach(socket, cols, rows)?;
    let id = surface.id;
    let _raw = RawGuard::enter(in_fd)?;
    // SAFETY: installing a handler that only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGWINCH,
            on_winch as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }

    let mut out = stdout.lock();
    out.write_all(&surface.snapshot)?;
    out.flush()?;

    let stop = Arc::new(AtomicBool::new(false));
    let input_thread = {
        let stop = Arc::clone(&stop);
        let mut to_kernel = surface.stream.try_clone()?;
        let socket = socket.to_path_buf();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let mut poll = libc::pollfd {
                fd: in_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            while !stop.load(Ordering::Relaxed) {
                if WINCH.swap(false, Ordering::Relaxed) {
                    if let Ok((cols, rows)) = pty::size_of(out_fd) {
                        let _ = call(
                            &socket,
                            &Request::Resize {
                                cols,
                                rows,
                                pin: false,
                            },
                        );
                    }
                }
                // SAFETY: polling one fd we own with a bounded timeout.
                let ready = unsafe { libc::poll(&mut poll, 1, 100) };
                if ready <= 0 {
                    continue;
                }
                // SAFETY: reading into a buffer we own.
                let n = unsafe { libc::read(in_fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    if n < 0 && io::Error::last_os_error().kind() == ErrorKind::Interrupted {
                        continue;
                    }
                    break;
                }
                if to_kernel.write_all(&buf[..n as usize]).is_err() {
                    break;
                }
            }
            let _ = to_kernel.shutdown(std::net::Shutdown::Write);
        })
    };

    let mut from_kernel = surface.stream;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match from_kernel.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = out.flush();
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    let _ = from_kernel.shutdown(std::net::Shutdown::Both);
    let _ = input_thread.join();
    drop(out);

    // The daemon records the reason at release; a daemon that is already
    // gone (killed under us) counts as the session ending.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match release_reason(socket, id) {
            Ok(reason) => return Ok(reason),
            Err(ClientError::Connect(_)) => return Ok(ReleaseReason::SessionExited),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(error),
        }
    }
}
