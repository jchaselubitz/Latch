//! Bounded, Latch-owned persistence for ConversationHub.
//!
//! The cache is never an authority: a bad cache is discarded and the connector
//! is asked to rebuild from its agent-owned source.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::{CheckpointDelta, ConversationSnapshot, StampedMutation, MAX_CONVERSATION_BATCH_BYTES};

pub const MAX_JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_JOURNAL_RECORDS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheBatch {
    pub mutations: Vec<StampedMutation>,
    /// Source offsets and active-branch changes observed with these mutations.
    /// Keeping them in the same JSONL record makes a torn final write harmless.
    #[serde(default)]
    pub checkpoint_delta: Option<CheckpointDelta>,
    pub operation_records: Vec<serde_json::Value>,
}

/// A restored cache: the compact base, its operation ledger, and the
/// append-only batches recorded after it.
pub type RestoredCache = (
    ConversationSnapshot,
    Vec<serde_json::Value>,
    Vec<u8>,
    Vec<CacheBatch>,
);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CompactCache {
    snapshot: ConversationSnapshot,
    operations: Vec<serde_json::Value>,
    #[serde(default)]
    connector_checkpoint: Vec<u8>,
    journal_generation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalHeader {
    journal_generation: String,
}

pub struct GatewayLock {
    file: File,
}
impl GatewayLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let path = root.join("conversation-gateway.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        // An advisory kernel lock survives neither process exit nor a crash.
        // A create-new sentinel would leave the gateway permanently wedged
        // after SIGKILL, precisely when restart recovery is most important.
        let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if locked != 0 {
            bail!(
                "another latch serve process already owns the Conversation Hub cache at {}",
                path.display()
            );
        }
        file.set_len(0)?;
        writeln!(file, "{}", std::process::id())?;
        file.sync_data()?;
        Ok(Self { file })
    }
}
impl Drop for GatewayLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[derive(Clone, Debug)]
pub struct ConversationCache {
    dir: PathBuf,
    journal_generation: Arc<Mutex<Option<String>>>,
}
impl ConversationCache {
    pub fn new(root: impl Into<PathBuf>, session: &str) -> Self {
        Self {
            dir: root.into().join("conversations").join(session),
            journal_generation: Arc::new(Mutex::new(None)),
        }
    }
    fn compact_path(&self) -> PathBuf {
        self.dir.join("compact.json")
    }
    fn journal_path(&self) -> PathBuf {
        self.dir.join("transitions.jsonl")
    }
    pub fn load(&self) -> Result<Option<RestoredCache>> {
        let path = self.compact_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        if bytes.len() > MAX_JOURNAL_BYTES as usize {
            bail!("conversation compact cache exceeds bound");
        }
        let cached: CompactCache =
            serde_json::from_slice(&bytes).context("invalid conversation compact cache")?;
        *self
            .journal_generation
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cache generation lock poisoned"))? =
            Some(cached.journal_generation.clone());
        let journal = self.load_journal(&cached.journal_generation)?;
        Ok(Some((
            cached.snapshot,
            cached.operations,
            cached.connector_checkpoint,
            journal,
        )))
    }
    pub fn append(&self, batch: &CacheBatch) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let payload = serde_json::to_vec(batch)?;
        if payload.len() > MAX_CONVERSATION_BATCH_BYTES {
            bail!("conversation transition batch exceeds bound");
        }
        let generation = self
            .journal_generation
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cache generation lock poisoned"))?
            .clone()
            .context("conversation cache has no compact base")?;
        let path = self.journal_path();
        let needs_header = !path.exists() || fs::metadata(&path)?.len() == 0;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if needs_header {
            serde_json::to_writer(
                &mut file,
                &JournalHeader {
                    journal_generation: generation,
                },
            )?;
            file.write_all(b"\n")?;
        }
        file.write_all(&payload)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }
    pub fn compact(
        &self,
        snapshot: &ConversationSnapshot,
        operations: &[serde_json::Value],
        connector_checkpoint: &[u8],
    ) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let journal_generation = fresh_generation();
        let payload = serde_json::to_vec(&CompactCache {
            snapshot: snapshot.clone(),
            operations: operations.to_vec(),
            connector_checkpoint: connector_checkpoint.to_vec(),
            journal_generation: journal_generation.clone(),
        })?;
        if payload.len() > MAX_JOURNAL_BYTES as usize {
            bail!("conversation compact cache exceeds bound");
        }
        let nonce = fresh_generation();
        let compact_temp = self.dir.join(format!("compact-{nonce}.tmp"));
        let journal_temp = self.dir.join(format!("journal-{nonce}.tmp"));
        fs::write(&compact_temp, payload)?;
        File::open(&compact_temp)?.sync_all()?;
        let mut journal = File::create(&journal_temp)?;
        serde_json::to_writer(
            &mut journal,
            &JournalHeader {
                journal_generation: journal_generation.clone(),
            },
        )?;
        journal.write_all(b"\n")?;
        journal.sync_all()?;

        // Publish the compact base first. If the process dies before the
        // journal rename, load sees the old journal generation and ignores it;
        // it can never replay pre-compaction operations onto the new snapshot.
        fs::rename(compact_temp, self.compact_path())?;
        fs::rename(journal_temp, self.journal_path())?;
        *self
            .journal_generation
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cache generation lock poisoned"))? =
            Some(journal_generation);
        Ok(())
    }
    pub fn journal_is_bounded(&self) -> Result<bool> {
        let path = self.journal_path();
        if !path.exists() {
            return Ok(true);
        }
        if fs::metadata(&path)?.len() > MAX_JOURNAL_BYTES {
            return Ok(false);
        }
        Ok(fs::read(path)?
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            <= MAX_JOURNAL_RECORDS + 1)
    }
    pub fn discard(&self) -> Result<()> {
        if self.dir.exists() {
            fs::remove_dir_all(&self.dir)?;
        }
        *self
            .journal_generation
            .lock()
            .map_err(|_| anyhow::anyhow!("conversation cache generation lock poisoned"))? = None;
        Ok(())
    }

    fn load_journal(&self, expected_generation: &str) -> Result<Vec<CacheBatch>> {
        let path = self.journal_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&path)?;
        if bytes.len() > MAX_JOURNAL_BYTES as usize {
            bail!("conversation transition journal exceeds bound");
        }
        let complete = bytes.ends_with(b"\n");
        let mut lines = bytes.split(|byte| *byte == b'\n');
        let header: JournalHeader = serde_json::from_slice(
            lines
                .next()
                .filter(|line| !line.is_empty())
                .context("conversation transition journal has no header")?,
        )
        .context("invalid conversation transition journal header")?;
        if header.journal_generation != expected_generation {
            // The compact rename won a crash race with journal replacement.
            // The compact snapshot already contains those old transitions.
            return Ok(Vec::new());
        }
        let remaining: Vec<_> = lines.collect();
        let mut batches = Vec::new();
        for (index, line) in remaining.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            if index + 1 == remaining.len() && !complete {
                // Only an interrupted final record is disposable. Corruption
                // in the middle must rebuild and rotate operationEpoch.
                break;
            }
            if batches.len() >= MAX_JOURNAL_RECORDS {
                bail!("conversation transition journal exceeds record bound");
            }
            batches.push(
                serde_json::from_slice(line)
                    .context("invalid conversation transition journal record")?,
            );
        }
        Ok(batches)
    }
}

fn fresh_generation() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_lock_is_exclusive_and_recoverable_after_drop() {
        let temp = tempfile::tempdir().unwrap();
        let first = GatewayLock::acquire(temp.path()).unwrap();
        assert!(GatewayLock::acquire(temp.path()).is_err());
        drop(first);
        GatewayLock::acquire(temp.path()).unwrap();
    }
}
