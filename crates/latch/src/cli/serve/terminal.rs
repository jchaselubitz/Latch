//! WebSocket relay between a client and one exclusive `latch attach` PTY.
//!
//! A remote terminal is not a second kind of surface. The spawned attach is the
//! same exclusive raw client a local terminal runs, so connecting this socket
//! steals iTerm and a later `latch attach` steals this socket back. Everything
//! here exists to make the socket a safe holder of that single surface: it must
//! not steal before it is authorised and sized, must not let a phone that has
//! stopped reading hold the surface, and must never leave the attach process
//! alive after the socket is gone.

use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use serde::Deserialize;
use tokio::io::unix::AsyncFd;
use tokio::time::timeout;

use super::pty::{PtyChild, SpawnAttachRequest};
use crate::engine::SurfaceRelease;
use crate::session::manifest::TerminalSize;
use crate::session::paths::LatchHome;

const PTY_BUFFER: usize = 32 * 1024;

/// How long one socket write may take before the peer counts as unable to
/// drain. The kernel bounds the queue behind us and evicts a raw client that
/// exceeds it, so this deadline only has to stop the gateway itself from
/// waiting on a backgrounded phone; it does not have to protect the pane.
const WRITE_DEADLINE: Duration = Duration::from_secs(5);

/// How long a socket may take to declare a usable size before it is closed.
/// Stealing the desk surface is destructive, so an unfinished handshake must
/// expire rather than sit holding a steal in reserve.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Frames accepted before a size arrives. A client that has not yet said how
/// big it is has no surface to type at, so its input is noise; bounding it
/// keeps a chatty peer from using the pre-attach phase as free buffer.
const MAX_HANDSHAKE_FRAMES: usize = 32;

/// Application close code: the session id or name does not exist.
const WS_CLOSE_SESSION_NOT_FOUND: u16 = 4404;
/// Application close code: no usable terminal size was declared in time.
const WS_CLOSE_SIZE_REQUIRED: u16 = 4400;
/// Application close code: this socket could not drain the session's output.
const WS_CLOSE_SLOW_CLIENT: u16 = 4408;
/// Application close code: another attach took the surface.
const WS_CLOSE_STOLEN: u16 = 4409;
/// Application close code: the session's program exited.
const WS_CLOSE_SESSION_EXITED: u16 = 4410;
/// Application close code: the session kernel failed to hand over a surface.
const WS_CLOSE_KERNEL_ERROR: u16 = 4500;

/// Connection inputs for one terminal socket.
pub struct TerminalConnect {
    /// Latch state root.
    pub home: LatchHome,
    /// `latch` executable.
    pub latch_bin: PathBuf,
    /// Session id or name from the URL.
    pub session: String,
    /// Initial columns from the WebSocket query string, when provided.
    pub cols: Option<u16>,
    /// Initial rows from the WebSocket query string, when provided.
    pub rows: Option<u16>,
}

/// `cols` / `rows` query parameters on `/v2/sessions/{id}/terminal`.
///
/// There is no mode parameter. A terminal connection is a control surface;
/// observing without controlling is Conversation Hub's job, not a second live
/// paint target competing for the same pane.
#[derive(Debug, Default, Deserialize)]
pub struct TerminalQuery {
    /// Initial columns. Must be paired with [`Self::rows`].
    pub cols: Option<u16>,
    /// Initial rows. Must be paired with [`Self::cols`].
    pub rows: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct ControlFrame {
    #[serde(rename = "type")]
    kind: String,
    cols: Option<u16>,
    rows: Option<u16>,
}

struct TerminalClose {
    code: u16,
    reason: &'static str,
}

impl TerminalClose {
    /// Close frame for one kernel release reason, so a mobile or SDK client
    /// distinguishes "someone else took the terminal" from "the agent exited"
    /// without reading pane bytes.
    fn from_release(release: Option<SurfaceRelease>) -> Self {
        match release {
            Some(SurfaceRelease::Normal) => Self {
                code: close_code::NORMAL,
                reason: "detached",
            },
            Some(SurfaceRelease::Stolen) => Self {
                code: WS_CLOSE_STOLEN,
                reason: "stolen",
            },
            Some(SurfaceRelease::SlowClient) => Self {
                code: WS_CLOSE_SLOW_CLIENT,
                reason: "slow_client",
            },
            Some(SurfaceRelease::SessionExited) => Self {
                code: WS_CLOSE_SESSION_EXITED,
                reason: "session_exited",
            },
            None => Self {
                code: WS_CLOSE_KERNEL_ERROR,
                reason: "kernel_error",
            },
        }
    }
}

/// Why the relay loop stopped.
enum RelayEnd {
    /// The attach process ended, or its PTY did. Its exit status is the
    /// authoritative reason, so the caller reaps it before closing the socket.
    Attach,
    /// The peer went away. Nothing needs to be told to a closed socket.
    Peer,
    /// The peer stopped draining output within [`WRITE_DEADLINE`].
    SlowClient,
}

/// Relays PTY bytes until the socket or attach process ends.
pub async fn run(mut socket: WebSocket, connect: TerminalConnect) {
    let Ok(id) = crate::cli::manage::resolve_existing(&connect.home, &connect.session) else {
        close_socket(
            &mut socket,
            TerminalClose {
                code: WS_CLOSE_SESSION_NOT_FOUND,
                reason: "session not found",
            },
        )
        .await;
        return;
    };
    // Nothing above this point has touched the session. The steal happens when
    // the attach process is spawned, so both the authorisation check in the
    // route table and this size handshake complete before any existing surface
    // is disturbed.
    let Some(size) = initial_pty_size(&mut socket, &connect).await else {
        return;
    };
    let mut pty = match PtyChild::spawn(SpawnAttachRequest {
        latch_bin: &connect.latch_bin,
        session_id: id.as_str(),
        cols: size.cols,
        rows: size.rows,
    }) {
        Ok(pty) => pty,
        Err(_) => {
            close_socket(&mut socket, TerminalClose::from_release(None)).await;
            return;
        }
    };
    let master = match pty.master.try_clone().and_then(AsyncFd::new) {
        Ok(master) => master,
        Err(_) => {
            pty.shutdown().await;
            close_socket(&mut socket, TerminalClose::from_release(None)).await;
            return;
        }
    };

    let end = relay(&mut socket, &master, &mut pty).await;
    match end {
        RelayEnd::Attach => {
            // Read the reason before killing, so an attach that already ended
            // reports why instead of being overwritten by our own signal.
            let release = pty.wait().await;
            pty.shutdown().await;
            close_socket(&mut socket, TerminalClose::from_release(release)).await;
        }
        // A socket that dropped mid-frame, or one we gave up writing to, must
        // not leave an attach process behind still counted as the live surface.
        RelayEnd::SlowClient => {
            pty.shutdown().await;
            close_socket(
                &mut socket,
                TerminalClose {
                    code: WS_CLOSE_SLOW_CLIENT,
                    reason: "slow_client",
                },
            )
            .await;
        }
        RelayEnd::Peer => pty.shutdown().await,
    }
}

async fn relay(
    socket: &mut WebSocket,
    master: &AsyncFd<std::fs::File>,
    pty: &mut PtyChild,
) -> RelayEnd {
    let mut buf = vec![0u8; PTY_BUFFER];
    loop {
        tokio::select! {
            // The attach process is watched alongside the PTY and the socket so
            // a release reason is never learned only as an anonymous EOF.
            _ = pty.wait() => return RelayEnd::Attach,
            result = read_pty(master, &mut buf) => {
                match result {
                    // EOF on the master means the attach client is gone; let
                    // the wait branch above supply the reason.
                    Ok(0) | Err(_) => return RelayEnd::Attach,
                    Ok(n) => {
                        match timeout(
                            WRITE_DEADLINE,
                            socket.send(Message::Binary(buf[..n].to_vec().into())),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) => return RelayEnd::Peer,
                            Err(_elapsed) => return RelayEnd::SlowClient,
                        }
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => return RelayEnd::Peer,
                    Some(Ok(Message::Binary(bytes))) => {
                        if write_pty(master, &bytes).await.is_err() {
                            return RelayEnd::Attach;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if apply_control(master, text.as_str()).is_err() {
                            return RelayEnd::Attach;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        match timeout(WRITE_DEADLINE, socket.send(Message::Pong(payload))).await {
                            Ok(Ok(())) => {}
                            Ok(Err(_)) => return RelayEnd::Peer,
                            Err(_elapsed) => return RelayEnd::SlowClient,
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => return RelayEnd::Peer,
                }
            }
        }
    }
}

/// Resolves the size to steal at, or closes the socket and reports `None`.
///
/// A size is mandatory. Attaching at a guessed geometry would reflow the
/// agent's pane to a size no human is looking at, and this socket is about to
/// become the only surface.
async fn initial_pty_size(
    socket: &mut WebSocket,
    connect: &TerminalConnect,
) -> Option<TerminalSize> {
    if let Some(size) = query_size(connect) {
        return Some(size);
    }
    let handshake = timeout(HANDSHAKE_DEADLINE, declared_size(socket)).await;
    match handshake {
        Ok(Some(size)) => Some(size),
        // A peer that vanished mid-handshake needs no close frame; one that
        // never declared a size is told why it is being refused.
        Ok(None) => None,
        Err(_elapsed) => {
            close_socket(
                socket,
                TerminalClose {
                    code: WS_CLOSE_SIZE_REQUIRED,
                    reason: "terminal size required",
                },
            )
            .await;
            None
        }
    }
}

async fn declared_size(socket: &mut WebSocket) -> Option<TerminalSize> {
    let mut frames = 0usize;
    loop {
        if frames >= MAX_HANDSHAKE_FRAMES {
            close_socket(
                socket,
                TerminalClose {
                    code: WS_CLOSE_SIZE_REQUIRED,
                    reason: "terminal size required",
                },
            )
            .await;
            return None;
        }
        frames += 1;
        match socket.recv().await {
            None | Some(Err(_)) => return None,
            Some(Ok(Message::Text(text))) => {
                if let Some(size) = resize_from_text(text.as_str()) {
                    return Some(size);
                }
            }
            // Input before the surface exists is discarded rather than
            // buffered: it would otherwise be replayed into whichever pane this
            // socket later stole.
            Some(Ok(Message::Binary(_))) | Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Ping(payload))) => {
                if socket.send(Message::Pong(payload)).await.is_err() {
                    return None;
                }
            }
            Some(Ok(Message::Close(_))) => return None,
        }
    }
}

fn query_size(connect: &TerminalConnect) -> Option<TerminalSize> {
    match (connect.cols, connect.rows) {
        (Some(cols), Some(rows)) => Some(TerminalSize::new(cols.max(1), rows.max(1))),
        _ => None,
    }
}

fn resize_from_text(text: &str) -> Option<TerminalSize> {
    let frame = serde_json::from_str::<ControlFrame>(text).ok()?;
    if frame.kind != "resize" {
        return None;
    }
    let cols = frame.cols?;
    let rows = frame.rows?;
    Some(TerminalSize::new(cols.max(1), rows.max(1)))
}

fn apply_control(master: &AsyncFd<std::fs::File>, text: &str) -> io::Result<()> {
    let Some(size) = resize_from_text(text) else {
        return Ok(());
    };
    set_pty_size(master, size)
}

fn set_pty_size(master: &AsyncFd<std::fs::File>, size: TerminalSize) -> io::Result<()> {
    let winsize = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `winsize` lives for the ioctl; `master` holds an open PTY fd.
    if unsafe {
        libc::ioctl(
            master.get_ref().as_raw_fd(),
            libc::TIOCSWINSZ as libc::c_ulong,
            &winsize,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

async fn close_socket(socket: &mut WebSocket, close: TerminalClose) {
    let _ = timeout(
        WRITE_DEADLINE,
        socket.send(Message::Close(Some(CloseFrame {
            code: close.code,
            reason: close.reason.into(),
        }))),
    )
    .await;
}

async fn read_pty(master: &AsyncFd<std::fs::File>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut guard = master.readable().await?;
        match guard.try_io(|inner| {
            // SAFETY: `inner` holds the PTY master; `buf` is writable for `len` bytes.
            let n = unsafe {
                libc::read(
                    inner.get_ref().as_raw_fd(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(result) => return result,
            Err(_would_block) => continue,
        }
    }
}

async fn write_pty(master: &AsyncFd<std::fs::File>, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let mut guard = master.writable().await?;
        let n = match guard.try_io(|inner| {
            // SAFETY: `inner` holds the PTY master; `buf` is the remaining write slice.
            let n = unsafe {
                libc::write(
                    inner.get_ref().as_raw_fd(),
                    buf.as_ptr() as *const libc::c_void,
                    buf.len(),
                )
            };
            if n < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(n as usize)
            }
        }) {
            Ok(result) => result?,
            Err(_would_block) => continue,
        };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "pty write returned 0",
            ));
        }
        buf = &buf[n..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tungstenite::client::IntoClientRequest;
    use tungstenite::http::HeaderValue;

    use super::*;
    use crate::session::manifest::SourceInfo;
    use crate::session::meta::{self, SessionMeta};
    use crate::session::paths::SessionId;

    const TOKEN: &str = "terminal-test-token";
    const SESSION: &str = "ses_terminaltest";

    struct Harness {
        _dir: tempfile::TempDir,
        address: SocketAddr,
        /// Written by the stub attach the moment it starts, so a test can
        /// assert a refused socket never reached the point of stealing.
        spawned: PathBuf,
        /// The stub attach's pid, so a test can prove it was reaped rather
        /// than left behind holding the surface.
        pid_file: PathBuf,
    }

    impl Harness {
        fn stole(&self) -> bool {
            self.spawned.is_file()
        }

        /// Whether the stub attach is still alive. Signal 0 checks for the
        /// process without touching it.
        fn attach_alive(&self) -> bool {
            let Ok(text) = std::fs::read_to_string(&self.pid_file) else {
                return false;
            };
            let Ok(pid) = text.trim().parse::<libc::pid_t>() else {
                return false;
            };
            // SAFETY: signal 0 performs only an existence and permission check.
            unsafe { libc::kill(pid, 0) == 0 }
        }
    }

    /// Boots the production router on loopback with `body` standing in for the
    /// attach client. The stub is a real process on a real PTY, so everything
    /// under test — spawn, byte relay, exit-code translation, reaping — is the
    /// production path; only the real daemon is absent.
    async fn harness(body: &str) -> Harness {
        harness_with(body, 0).await
    }

    /// `send_buffer`, when non-zero, shrinks the gateway listener's socket
    /// send buffer. Accepted sockets inherit it, which is what lets the
    /// slow-peer case be reached in a moment instead of after megabytes.
    async fn harness_with(body: &str, send_buffer: libc::c_int) -> Harness {
        let dir = tempfile::tempdir().expect("temp home");
        let home = LatchHome::new(dir.path());
        home.ensure().expect("home");
        let id = SessionId::parse(SESSION).expect("session id");
        let paths = home.session(&id);
        paths.ensure().expect("session dir");
        meta::write_once(
            &paths,
            &SessionMeta {
                format_version: 1,
                id: id.as_str().to_owned(),
                name: "terminal".into(),
                title: None,
                cwd: dir.path().to_path_buf(),
                command_label: "claude".into(),
                harness: None,
                created_at: "2026-08-22T00:00:00Z".into(),
                initial_size: TerminalSize::new(80, 24),
                source: SourceInfo {
                    kind: "test".into(),
                    external_run_id: None,
                },
            },
        )
        .expect("write meta");
        let token_file = dir.path().join("serve.token");
        std::fs::write(&token_file, TOKEN).expect("token");

        let spawned = dir.path().join("spawned");
        let pid_file = dir.path().join("attach.pid");
        let latch_bin = dir.path().join("stub-latch");
        std::fs::write(
            &latch_bin,
            format!(
                "#!/bin/sh\n\
                 echo $$ > {pid}\n\
                 : > {spawned}\n\
                 {body}\n",
                pid = pid_file.display(),
                spawned = spawned.display(),
            ),
        )
        .expect("stub");
        std::fs::set_permissions(&latch_bin, std::fs::Permissions::from_mode(0o755))
            .expect("stub mode");

        let hub = crate::conversation::ConversationHub::new(dir.path().join("hub")).expect("hub");
        let app = super::super::http::test_router(home, token_file, hub, latch_bin);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        if send_buffer > 0 {
            use std::os::fd::AsRawFd;
            // SAFETY: the listener owns an open socket; `send_buffer` outlives
            // the call.
            let set = unsafe {
                libc::setsockopt(
                    listener.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    std::ptr::addr_of!(send_buffer).cast(),
                    std::mem::size_of_val(&send_buffer) as libc::socklen_t,
                )
            };
            assert_eq!(set, 0, "cannot shrink the test gateway's send buffer");
        }
        let address = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        Harness {
            _dir: dir,
            address,
            spawned,
            pid_file,
        }
    }

    /// A blocking WebSocket on its own thread, so tests drive real frames.
    fn open(harness: &Harness, query: &str, grant: &str) -> tungstenite::WebSocket<TcpStream> {
        connect(harness, query, grant, 0).expect("handshake")
    }

    fn connect(
        harness: &Harness,
        query: &str,
        grant: &str,
        receive_buffer: libc::c_int,
    ) -> Result<tungstenite::WebSocket<TcpStream>, u16> {
        let url = format!(
            "ws://{}/v2/sessions/{SESSION}/terminal{query}",
            harness.address
        );
        let mut request = url.into_client_request().expect("request");
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("token header"),
        );
        request.headers_mut().insert(
            "x-latch-device-grant",
            HeaderValue::from_str(grant).expect("grant header"),
        );
        let stream = if receive_buffer > 0 {
            small_window_stream(harness.address, receive_buffer)
        } else {
            TcpStream::connect(harness.address).expect("connect")
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        tungstenite::client::client(request, stream)
            .map(|(socket, _)| socket)
            .map_err(|error| match error {
                tungstenite::HandshakeError::Failure(tungstenite::Error::Http(response)) => {
                    response.status().as_u16()
                }
                _ => 0,
            })
    }

    /// Connects with a deliberately tiny receive window, so a test reaches the
    /// state a backgrounded phone reaches — the gateway unable to hand off
    /// another byte — in a moment rather than after megabytes. The option has
    /// to be set before connect: the window scale is negotiated in the
    /// handshake and a later change does not shrink it.
    fn small_window_stream(address: SocketAddr, bytes: libc::c_int) -> TcpStream {
        use std::os::fd::FromRawFd;
        // SAFETY: each call below is checked, and the descriptor is handed to
        // `TcpStream` exactly once so ownership is never duplicated.
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
            assert!(fd >= 0, "cannot open a test socket");
            assert_eq!(
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    std::ptr::addr_of!(bytes).cast(),
                    std::mem::size_of_val(&bytes) as libc::socklen_t,
                ),
                0,
                "cannot shrink the test peer's receive buffer"
            );
            let SocketAddr::V4(v4) = address else {
                panic!("the test gateway binds IPv4 loopback");
            };
            let sockaddr = libc::sockaddr_in {
                #[cfg(any(
                    target_os = "macos",
                    target_os = "freebsd",
                    target_os = "openbsd",
                    target_os = "netbsd",
                    target_os = "dragonfly"
                ))]
                sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            assert_eq!(
                libc::connect(
                    fd,
                    std::ptr::addr_of!(sockaddr).cast(),
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                ),
                0,
                "cannot connect the test socket"
            );
            TcpStream::from_raw_fd(fd)
        }
    }

    /// Reads until the peer closes, returning the close frame and every
    /// binary byte seen before it.
    fn drain(socket: &mut tungstenite::WebSocket<TcpStream>) -> (u16, String, Vec<u8>) {
        let mut bytes = Vec::new();
        loop {
            match socket.read() {
                Ok(tungstenite::Message::Binary(chunk)) => bytes.extend_from_slice(&chunk),
                Ok(tungstenite::Message::Close(Some(frame))) => {
                    return (frame.code.into(), frame.reason.to_string(), bytes);
                }
                Ok(_) => {}
                Err(error) => panic!("socket ended without a close frame: {error}"),
            }
        }
    }

    fn wait_until(predicate: impl FnMut() -> bool, message: &str) {
        wait_until_within(predicate, message, Duration::from_secs(10));
    }

    fn wait_until_within(mut predicate: impl FnMut() -> bool, message: &str, limit: Duration) {
        let deadline = std::time::Instant::now() + limit;
        while !predicate() {
            assert!(std::time::Instant::now() < deadline, "{message}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_observe_device_cannot_take_the_terminal_surface() {
        // There is no observing terminal any more: a connection *is* the
        // session's one surface, so opening one must require control.
        let harness = harness("sleep 30").await;
        let refused = connect(&harness, "?cols=80&rows=24", "observe", 0);
        assert!(
            matches!(refused, Err(403)),
            "an observe device must be refused before anything is spawned"
        );
        assert!(!harness.stole(), "a refused socket must not steal");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_socket_that_never_declares_a_size_is_refused_before_stealing() {
        // Attaching at a guessed geometry would reflow the agent's pane to a
        // size nobody is looking at, so the steal waits for a real size.
        let harness = harness("sleep 30").await;
        let mut socket = open(&harness, "", "control");
        for _ in 0..(MAX_HANDSHAKE_FRAMES + 1) {
            socket
                .send(tungstenite::Message::Text("{\"type\":\"noise\"}".into()))
                .expect("send");
        }
        let (code, reason, bytes) = drain(&mut socket);
        assert_eq!(
            (code, reason.as_str()),
            (WS_CLOSE_SIZE_REQUIRED, "terminal size required")
        );
        assert!(bytes.is_empty());
        assert!(!harness.stole(), "an unsized socket must not steal");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_session_is_refused_without_spawning_an_attach() {
        let harness = harness("sleep 30").await;
        let url = format!(
            "ws://{}/v2/sessions/ses_missing/terminal?cols=80&rows=24",
            harness.address
        );
        let mut request = url.into_client_request().expect("request");
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("token header"),
        );
        request
            .headers_mut()
            .insert("x-latch-device-grant", HeaderValue::from_static("control"));
        let stream = TcpStream::connect(harness.address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        let (mut socket, _) = tungstenite::client::client(request, stream).expect("handshake");
        let (code, _, _) = drain(&mut socket);
        assert_eq!(code, WS_CLOSE_SESSION_NOT_FOUND);
        assert!(!harness.stole());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_kernel_release_becomes_a_stable_close_reason() {
        let cases = [
            ("exit 0", close_code::NORMAL, "detached"),
            ("exit 75", WS_CLOSE_STOLEN, "stolen"),
            ("exit 76", WS_CLOSE_SLOW_CLIENT, "slow_client"),
            ("exit 77", WS_CLOSE_SESSION_EXITED, "session_exited"),
            // Anything the kernel did not label is a failure, never a
            // hand-over: a client must not be told the surface moved on.
            ("exit 3", WS_CLOSE_KERNEL_ERROR, "kernel_error"),
        ];
        for (body, code, reason) in cases {
            let harness = harness(body).await;
            let mut socket = open(&harness, "?cols=80&rows=24", "control");
            let (actual_code, actual_reason, _) = drain(&mut socket);
            assert_eq!(
                (actual_code, actual_reason.as_str()),
                (code, reason),
                "for `{body}`"
            );
            wait_until(|| !harness.attach_alive(), "attach must be reaped");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn output_after_the_boundary_is_relayed_byte_for_byte() {
        // Deliberately not valid UTF-8 and not JSON-safe: the relay must not
        // re-encode, escape, or drop anything the pane wrote. No newline is
        // used because the PTY line discipline, not the relay, would rewrite
        // it.
        let payload: &[u8] = b"\x1b[31mred\x00\xff\xfe\x1b[0m done";
        let harness = harness(
            "printf 'red\\033[0m done' | : ; printf '\\033[31mred\\000\\377\\376\\033[0m done'",
        )
        .await;
        let mut socket = open(&harness, "?cols=80&rows=24", "control");
        let (code, _, bytes) = drain(&mut socket);
        assert_eq!(code, close_code::NORMAL);
        assert_eq!(bytes, payload);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_disappears_leaves_no_attach_behind() {
        // A ghost attach would still be counted as the session's live surface,
        // so a dropped socket has to take its attach process with it.
        let harness = harness("sleep 30").await;
        let socket = open(&harness, "?cols=80&rows=24", "control");
        wait_until(|| harness.stole(), "attach must start");
        wait_until(|| harness.attach_alive(), "attach must be running");
        drop(socket);
        wait_until(|| !harness.attach_alive(), "attach must be reaped");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_stops_reading_is_closed_and_its_attach_reaped() {
        // A backgrounded phone must not hold the surface. The kernel bounds
        // its own queue; this proves the gateway also gives up rather than
        // waiting on a socket that has stopped draining.
        let harness = harness_with("yes xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", 4096).await;
        let socket = connect(&harness, "?cols=80&rows=24", "control", 4096).expect("handshake");
        wait_until(|| harness.attach_alive(), "attach must be running");
        // Never read a single frame, so every buffer between the pane and this
        // thread fills and the gateway's write deadline is the only thing that
        // can end the connection.
        wait_until_within(
            || !harness.attach_alive(),
            "a non-reading peer must be given up on and its attach reaped",
            WRITE_DEADLINE + Duration::from_secs(20),
        );
        drop(socket);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn input_reaches_the_attach_and_a_resize_frame_is_not_typed_into_the_pane() {
        // The relay is byte-transparent for binary frames and interprets only
        // its own JSON control frames, which must never reach the pane.
        let harness = harness("head -c 6 | cat ; exit 0").await;
        let mut socket = open(&harness, "?cols=80&rows=24", "control");
        socket
            .send(tungstenite::Message::Text(
                "{\"type\":\"resize\",\"cols\":100,\"rows\":30}".into(),
            ))
            .expect("resize");
        socket
            // The newline is required by the PTY's line discipline, not by
            // the relay: without it the attach's read never completes.
            .send(tungstenite::Message::Binary(b"hello\n".to_vec().into()))
            .expect("input");
        let (_, _, bytes) = drain(&mut socket);
        // The PTY echoes input and the stub writes it back, so `hello` appears
        // twice; the resize frame must appear nowhere.
        assert!(
            String::from_utf8_lossy(&bytes).contains("hello"),
            "input must reach the attach: {bytes:?}"
        );
        assert!(
            !bytes.windows(6).any(|part| part == b"resize"),
            "a control frame must never be typed into the pane: {bytes:?}"
        );
    }
}
