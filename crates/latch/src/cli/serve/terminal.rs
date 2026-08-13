//! WebSocket relay between a client and one `latch attach` PTY.

use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use axum::extract::ws::{Message, WebSocket};
use serde::Deserialize;
use tokio::io::unix::AsyncFd;

use super::pty::{PtyChild, SpawnAttachRequest};
use crate::session::paths::LatchHome;

const DEFAULT_COLS: u16 = 80;
const DEFAULT_ROWS: u16 = 24;
const PTY_BUFFER: usize = 32 * 1024;

/// Connection inputs for one terminal socket.
pub struct TerminalConnect {
    /// Latch state root.
    pub home: LatchHome,
    /// `latch` executable.
    pub latch_bin: PathBuf,
    /// Session id or name from the URL.
    pub session: String,
}

#[derive(Debug, Deserialize)]
struct ControlFrame {
    #[serde(rename = "type")]
    kind: String,
    cols: Option<u16>,
    rows: Option<u16>,
}

/// Relays PTY bytes until the socket or attach process ends.
pub async fn run(mut socket: WebSocket, connect: TerminalConnect) {
    let Ok(id) = crate::cli::manage::resolve_existing(&connect.home, &connect.session) else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    let mut pty = match PtyChild::spawn(SpawnAttachRequest {
        latch_bin: &connect.latch_bin,
        session_id: id.as_str(),
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
    }) {
        Ok(pty) => pty,
        Err(_) => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    let Ok(file) = pty.master.try_clone() else {
        pty.kill();
        return;
    };
    let Ok(master) = AsyncFd::new(file) else {
        pty.kill();
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

fn apply_control(master: &AsyncFd<std::fs::File>, text: &str) -> io::Result<()> {
    let Ok(frame) = serde_json::from_str::<ControlFrame>(text) else {
        return Ok(());
    };
    if frame.kind != "resize" {
        return Ok(());
    }
    let (Some(cols), Some(rows)) = (frame.cols, frame.rows) else {
        return Ok(());
    };
    let size = libc::winsize {
        ws_row: rows.max(1),
        ws_col: cols.max(1),
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `size` lives for the ioctl; `master` holds an open PTY fd.
    if unsafe {
        libc::ioctl(
            master.get_ref().as_raw_fd(),
            libc::TIOCSWINSZ as libc::c_ulong,
            &size,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
