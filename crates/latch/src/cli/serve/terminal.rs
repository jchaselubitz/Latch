//! WebSocket relay between a client and one `latch attach` PTY.

use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use serde::Deserialize;
use tokio::io::unix::AsyncFd;

use super::pty::{PtyChild, SpawnAttachRequest};
use crate::session::manifest::TerminalSize;
use crate::session::paths::LatchHome;

const PTY_BUFFER: usize = 32 * 1024;
/// Application close code: the session id or name does not exist.
const WS_CLOSE_SESSION_NOT_FOUND: u16 = 4404;

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

/// `cols` / `rows` query parameters on `/v1/sessions/{id}/terminal`.
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
            close_socket(
                &mut socket,
                TerminalClose {
                    code: close_code::ERROR,
                    reason: "attach failed",
                },
            )
            .await;
            return;
        }
    };
    let Ok(file) = pty.master.try_clone() else {
        pty.kill();
        close_socket(
            &mut socket,
            TerminalClose {
                code: close_code::ERROR,
                reason: "attach failed",
            },
        )
        .await;
        return;
    };
    let Ok(master) = AsyncFd::new(file) else {
        pty.kill();
        close_socket(
            &mut socket,
            TerminalClose {
                code: close_code::ERROR,
                reason: "attach failed",
            },
        )
        .await;
        return;
    };

    let mut buf = vec![0u8; PTY_BUFFER];
    loop {
        tokio::select! {
            result = read_pty(&master, &mut buf) => {
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if socket
                            .send(Message::Binary(buf[..n].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Binary(bytes))) => {
                        if write_pty(&master, &bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        if apply_control(&master, text.as_str()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => break,
                }
            }
        }
    }
    pty.kill();
}

async fn initial_pty_size(
    socket: &mut WebSocket,
    connect: &TerminalConnect,
) -> Option<TerminalSize> {
    if let Some(size) = query_size(connect) {
        return Some(size);
    }
    loop {
        match socket.recv().await {
            None | Some(Err(_)) => return None,
            Some(Ok(Message::Text(text))) => {
                if let Some(size) = resize_from_text(text.as_str()) {
                    return Some(size);
                }
            }
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
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close.code,
            reason: close.reason.into(),
        })))
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
