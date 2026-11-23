//! Local handoff from the Interact-gated connector to the existing Codex
//! connection. The peer can select an offered answer, never send RPC frames.
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::conversation::{ApplyResult, ConversationItemId};
use crate::session::paths::{LatchHome, SessionId};

const MAX_ANSWER_BYTES: usize = 48 * 1024;
const LOCAL_READ_TIMEOUT: Duration = Duration::from_millis(50);
const MAX_HANDOFF_WAIT: Duration = Duration::from_secs(2);

pub(super) struct Endpoint {
    pub listener: UnixListener,
    path: PathBuf,
}

impl Endpoint {
    pub fn bind(home: &LatchHome, session: &SessionId) -> Result<Self> {
        let path = socket_path(home, session)?;
        // The kernel's 0700 per-user directory protects the socket from
        // its creation. Never unlink a pre-existing endpoint here.
        let listener = UnixListener::bind(&path)?;
        let endpoint = Self { listener, path };
        endpoint.listener.set_nonblocking(true)?;
        Ok(endpoint)
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn socket_path(home: &LatchHome, session: &SessionId) -> Result<PathBuf> {
    let mut hash = Sha256::new();
    hash.update(home.root().canonicalize()?.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(session.as_str().as_bytes());
    let digest = hash.finalize();
    Ok(latchd::paths::socket_dir()?.join(format!(
        "codex-answer-{}.sock",
        crate::hex::encode(&digest[..12])
    )))
}

pub(super) fn read_selection(stream: &mut UnixStream) -> Result<Value> {
    latchd::peer::require_same_user(stream)?;
    let end = Instant::now() + LOCAL_READ_TIMEOUT;
    let mut header = [0u8; 4];
    read_until(stream, &mut header, end)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_ANSWER_BYTES {
        bail!("Codex answer exceeds the frame limit");
    }
    let mut body = vec![0; len];
    read_until(stream, &mut body, end)?;
    let envelope: Value = serde_json::from_slice(&body)?;
    if !envelope["notAfter"]
        .as_u64()
        .is_some_and(|expiry| expiry >= uptime_ms())
    {
        bail!("Codex answer arrived after its deadline");
    }
    Ok(envelope["selection"].clone())
}

fn uptime_ms() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // Both peers use the same host monotonic clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return u64::MAX;
    }
    (time.tv_sec as u64).saturating_mul(1000) + time.tv_nsec as u64 / 1_000_000
}

fn read_until(stream: &mut UnixStream, mut bytes: &mut [u8], end: Instant) -> Result<()> {
    while !bytes.is_empty() {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("Codex answer timed out");
        }
        stream.set_read_timeout(Some(remaining))?;
        let n = stream.read(bytes)?;
        if n == 0 {
            bail!("Codex answer connection closed");
        }
        bytes = &mut bytes[n..];
    }
    Ok(())
}

fn connect(path: &Path) -> Result<UnixStream> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        bail!("invalid Codex answer endpoint");
    }
    let stream = UnixStream::connect(path)?;
    latchd::peer::require_same_user(&stream)?;
    Ok(stream)
}

pub(crate) fn submit(
    home: &LatchHome,
    session: &SessionId,
    payload: &Value,
    deadline: Duration,
) -> Result<ApplyResult> {
    let refused = |reason: &str| ApplyResult::Refused {
        reason: reason.to_owned(),
    };
    let deadline = deadline.min(Duration::from_secs(9));
    let started = Instant::now();
    let not_after = uptime_ms()
        .checked_add(deadline.min(MAX_HANDOFF_WAIT).as_millis() as u64)
        .ok_or_else(|| anyhow::anyhow!("Codex answer clock unavailable"))?;
    let end = started + deadline;
    let body = serde_json::to_vec(&json!({"notAfter":not_after,"selection":payload}))?;
    if body.len() > MAX_ANSWER_BYTES {
        return Ok(refused("This answer is too large."));
    }
    let Ok(mut stream) = connect(&socket_path(home, session)?) else {
        return Ok(refused(
            "The Codex request connection is unavailable. Answer at the Mac.",
        ));
    };
    let remaining = end.saturating_duration_since(Instant::now());
    if remaining.is_zero() || uptime_ms() > not_after {
        return Ok(refused("The local Codex answer connection timed out."));
    }
    stream.set_write_timeout(Some(remaining.min(LOCAL_READ_TIMEOUT)))?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame)?;
    let mut result = [0];
    read_until(&mut stream, &mut result, end)?;
    match result[0] {
        b'Y' => Ok(ApplyResult::Accepted {
            correlation: payload["requestId"]
                .as_str()
                .map(|id| ConversationItemId::native(format!("request:{id}"))),
        }),
        b'N' => Ok(refused("Codex no longer offers this request or answer.")),
        _ => bail!("Codex did not acknowledge the answer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_expired_selections_can_never_authorize_a_request() {
        let (mut writer, mut receiver) = UnixStream::pair().unwrap();
        writer.write_all(&[0, 0]).unwrap();
        assert!(read_selection(&mut receiver).is_err());

        let (mut writer, mut receiver) = UnixStream::pair().unwrap();
        let old = serde_json::to_vec(&json!({
            "notAfter": 0,
            "selection": {"requestId":"obsolete","optionId":"allow_once"}
        }))
        .unwrap();
        writer.write_all(&(old.len() as u32).to_be_bytes()).unwrap();
        writer.write_all(&old).unwrap();
        assert!(read_selection(&mut receiver).is_err());
    }
}
