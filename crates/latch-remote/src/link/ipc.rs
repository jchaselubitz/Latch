//! Desktop-to-helper IPC lines on stdin and the content-free JSON lines the
//! helper prints on stdout.

use std::io::{BufRead, Write};

use anyhow::Context;
use latch::session::paths::LatchHome;
use serde::Deserialize;
use tokio::sync::mpsc;

/// Desktop-to-helper IPC lines. The initial stdin line is the
/// [`RemoteLinkHostConfig`](super::RemoteLinkHostConfig); every later line is one of these.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum HelperCommand {
    /// A control-plane-signed extension for the current relay lease.
    #[serde(rename_all = "camelCase")]
    LeaseExtension {
        version: u8,
        lease_id: String,
        claim: String,
    },
    /// A fresh single-use relay admission after the previous socket closed.
    #[serde(rename_all = "camelCase")]
    Admission {
        version: u8,
        relay_url: String,
        admission: String,
    },
    /// Stop serving and exit cleanly.
    Shutdown { version: u8 },
}

/// Helper-owned link lifecycle statuses printed as content-free JSON lines.
#[derive(Clone, Copy)]
pub(super) enum HostStatus {
    LanReady,
    Connecting,
    WaitingForPeer,
    Authenticating,
    Ready,
    LinkClosed,
    Offline,
}

impl HostStatus {
    fn name(self) -> &'static str {
        match self {
            Self::LanReady => "lan_ready",
            Self::Connecting => "connecting",
            Self::WaitingForPeer => "waiting_for_peer",
            Self::Authenticating => "authenticating",
            Self::Ready => "ready",
            Self::LinkClosed => "link_closed",
            Self::Offline => "offline",
        }
    }
}

/// The home the helper serves, so status transitions can also land in the
/// Mac's content-free audit trail for field evidence.
pub(super) static AUDIT_HOME: std::sync::OnceLock<LatchHome> = std::sync::OnceLock::new();

pub(super) fn emit_status(status: HostStatus, extra: serde_json::Value) {
    let mut value = serde_json::json!({ "type": "status", "version": 1, "status": status.name() });
    if let (Some(object), Some(more)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in more {
            object.insert(key.clone(), item.clone());
        }
    }
    let _ = emit_json(&value);
    if let Some(home) = AUDIT_HOME.get() {
        let detail = extra
            .get("reason")
            .or_else(|| extra.get("carrier"))
            .and_then(|item| item.as_str())
            .unwrap_or("ok");
        let _ = latch::cli::remote_access::record_link_status(home, status.name(), detail);
    }
}

pub(super) fn request_admission(reason: &str) {
    let _ = emit_json(&serde_json::json!({
        "type": "admission_needed", "version": 1, "reason": reason,
    }));
}

pub(super) fn ipc_reader(name: &str) -> anyhow::Result<mpsc::Receiver<String>> {
    let (send, receive) = mpsc::channel::<String>(4);
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) if !line.is_empty() => {
                        if send.blocking_send(line).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        })
        .context("cannot start helper enrollment IPC reader")?;
    Ok(receive)
}

pub(super) fn emit_json(value: &serde_json::Value) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub(super) async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}
