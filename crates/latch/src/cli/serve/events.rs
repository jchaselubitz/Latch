//! WebSocket relay of `latch events` NDJSON as one HarnessEvent per message.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket};
use serde::Deserialize;

use crate::harness::{self, InteractionOptions};
use crate::session::paths::LatchHome;

/// Application close code: the session id or name does not exist.
const WS_CLOSE_SESSION_NOT_FOUND: u16 = 4404;
/// Application close code: this session has no harness connector.
const WS_CLOSE_NO_CONNECTOR: u16 = 4408;
/// Application close code: the cursor is invalid for this connector epoch.
const WS_CLOSE_STALE_CURSOR: u16 = 4422;

/// Connection inputs for one events socket.
pub struct EventsConnect {
    /// Latch state root.
    pub home: LatchHome,
    /// `latch` executable.
    pub latch_bin: PathBuf,
    /// Session id or name from the URL.
    pub session: String,
    /// Event index to resume after. Zero is the start of the stream.
    pub cursor: usize,
}

/// `cursor` query parameter on `/v1/sessions/{id}/events`.
#[derive(Debug, Default, Deserialize)]
pub struct EventsQuery {
    /// Resume after this many deterministic events.
    pub cursor: Option<usize>,
}

struct EventsClose {
    code: u16,
    reason: &'static str,
}

/// Relays `latch events --json --from <cursor>` until the socket or child ends.
pub async fn run(mut socket: WebSocket, connect: EventsConnect) {
    let Ok(id) = crate::cli::manage::resolve_existing(&connect.home, &connect.session) else {
        close_socket(
            &mut socket,
            EventsClose {
                code: WS_CLOSE_SESSION_NOT_FOUND,
                reason: "session not found",
            },
        )
        .await;
        return;
    };
    let availability = match harness::events_availability(InteractionOptions {
        home: connect.home.clone(),
        session: id.to_string(),
    }) {
        Ok(availability) => availability,
        Err(_) => {
            close_socket(
                &mut socket,
                EventsClose {
                    code: close_code::ERROR,
                    reason: "events failed",
                },
            )
            .await;
            return;
        }
    };
    if !availability.ok {
        close_socket(
            &mut socket,
            EventsClose {
                code: WS_CLOSE_NO_CONNECTOR,
                reason: "no harness connector",
            },
        )
        .await;
        return;
    }

    let mut child = match spawn_events(SpawnEvents {
        latch_bin: &connect.latch_bin,
        home: &connect.home,
        session_id: id.as_str(),
        cursor: connect.cursor,
    }) {
        Ok(child) => EventsChild { child },
        Err(_) => {
            close_socket(
                &mut socket,
                EventsClose {
                    code: close_code::ERROR,
                    reason: "events failed",
                },
            )
            .await;
            return;
        }
    };

    let Some(stdout) = child.child.stdout.take() else {
        close_socket(
            &mut socket,
            EventsClose {
                code: close_code::ERROR,
                reason: "events failed",
            },
        )
        .await;
        return;
    };
    let stderr_buf = Arc::new(Mutex::new(String::new()));
    let mut stderr_join = child.child.stderr.take().map(|stderr| {
        let stderr_buf = Arc::clone(&stderr_buf);
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut text);
            if let Ok(mut slot) = stderr_buf.lock() {
                *slot = text;
            }
        })
    });

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    loop {
        tokio::select! {
            incoming = rx.recv() => {
                match incoming {
                    Some(Ok(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        if socket.send(Message::Text(line.into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Err(_)) | None => {
                        let failure = classify_child_exit(ClassifyExit {
                            child: &mut child.child,
                            stderr_buf: &stderr_buf,
                            stderr_join: stderr_join.take(),
                        });
                        if let Some(close) = failure {
                            close_socket(&mut socket, close).await;
                        } else {
                            close_socket(
                                &mut socket,
                                EventsClose {
                                    code: close_code::NORMAL,
                                    reason: "stream ended",
                                },
                            )
                            .await;
                        }
                        return;
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Ping(payload))) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_) | Message::Binary(_) | Message::Text(_))) => {}
                    Some(Ok(Message::Close(_))) => break,
                }
            }
        }
    }
}

struct EventsChild {
    child: Child,
}

impl Drop for EventsChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct SpawnEvents<'a> {
    latch_bin: &'a std::path::Path,
    home: &'a LatchHome,
    session_id: &'a str,
    cursor: usize,
}

fn spawn_events(request: SpawnEvents<'_>) -> std::io::Result<Child> {
    Command::new(request.latch_bin)
        .arg("events")
        .arg(request.session_id)
        .arg("--json")
        .arg("--from")
        .arg(request.cursor.to_string())
        .env("LATCH_HOME", request.home.root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
}

struct ClassifyExit<'a> {
    child: &'a mut Child,
    stderr_buf: &'a Arc<Mutex<String>>,
    stderr_join: Option<std::thread::JoinHandle<()>>,
}

fn classify_child_exit(request: ClassifyExit<'_>) -> Option<EventsClose> {
    let status = request.child.wait().ok()?;
    if let Some(join) = request.stderr_join {
        let _ = join.join();
    }
    if status.success() {
        return None;
    }
    let stderr = request
        .stderr_buf
        .lock()
        .ok()
        .map(|text| text.clone())
        .unwrap_or_default();
    Some(classify_events_failure(&stderr))
}

fn classify_events_failure(stderr: &str) -> EventsClose {
    let text = stderr.to_ascii_lowercase();
    if text.contains("restart from cursor 0")
        || (text.contains("cursor") && text.contains("beyond"))
    {
        return EventsClose {
            code: WS_CLOSE_STALE_CURSOR,
            reason: "stale cursor",
        };
    }
    if text.contains("no claude code transcript") {
        return EventsClose {
            code: close_code::AGAIN,
            reason: "transcript not found",
        };
    }
    EventsClose {
        code: close_code::ERROR,
        reason: "events failed",
    }
}

async fn close_socket(socket: &mut WebSocket, close: EventsClose) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close.code,
            reason: close.reason.into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_beyond_and_epoch_restart_are_stale() {
        let beyond = classify_events_failure("cursor 9 is beyond the session's 2 persisted events");
        assert_eq!(beyond.code, WS_CLOSE_STALE_CURSOR);
        let restart = classify_events_failure(
            "Claude Code changed the active transcript branch; restart from cursor 0 for connector epoch 1",
        );
        assert_eq!(restart.code, WS_CLOSE_STALE_CURSOR);
    }

    #[test]
    fn missing_transcript_is_retryable() {
        let close = classify_events_failure(
            "no Claude Code transcript found for Latch session ses_one under /tmp/projects",
        );
        assert_eq!(close.code, close_code::AGAIN);
        assert_eq!(close.reason, "transcript not found");
    }
}
