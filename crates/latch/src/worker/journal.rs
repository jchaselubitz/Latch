//! The bounded output journal.
//!
//! The journal is a byte log of what the child wrote, capped and owner-only. It
//! is *not* how a client reattaches — that is the screen model's snapshot, and
//! the difference is the whole reason the screen model exists. The journal's job
//! is history: scrollback that predates an attachment, and the raw record M2
//! decides how much of to ship alongside a snapshot.
//!
//! Being bounded is the interesting property. An agent session left running
//! overnight can emit gigabytes; a log that grows without limit fills the user's
//! disk with terminal output they never asked to keep. The cap is enforced
//! oldest-first, because the recent tail is the part anyone wants.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::worker::paths::FILE_MODE;

/// Default cap for one session's journal, in bytes.
///
/// Ten megabytes is roughly an hour of a chatty agent and costs nothing to keep
/// for a session that lives a day.
pub const DEFAULT_JOURNAL_CAP_BYTES: u64 = 10 * 1024 * 1024;

/// How a journal is bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalConfig {
    /// Maximum bytes retained. Older bytes are discarded first.
    pub cap_bytes: u64,
}

impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            cap_bytes: DEFAULT_JOURNAL_CAP_BYTES,
        }
    }
}

/// Anything that can go wrong with the journal.
///
/// A journal failure must never take the session down: the child is the thing
/// with value, and losing history is not a reason to lose work. Callers are
/// expected to report and continue, which is why the error carries enough to log
/// and nothing that demands handling.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// The journal file could not be opened, written, or trimmed.
    #[error("{path}: {source}")]
    Io {
        /// The journal path.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The configured cap is unusable.
    #[error("journal cap of {cap} bytes is too small; the minimum is {minimum}")]
    CapTooSmall {
        /// Requested cap.
        cap: u64,
        /// Smallest cap this build accepts.
        minimum: u64,
    },
}

/// Smallest journal cap this build accepts, in bytes.
///
/// A small cap is useful in constrained deployments and tests; zero is the only
/// value that would make every append a full truncation.
pub const MIN_JOURNAL_CAP_BYTES: u64 = 1024;

/// How full the journal is left after an append crosses the hard cap.
///
/// Compacting to the cap exactly makes the next PTY read compact again. Under
/// sustained output that turns every 8 KiB read into a rewrite and `fsync` of
/// the whole 10 MiB journal, starving the worker's control loop. Leaving some
/// headroom keeps the hard on-disk bound while amortizing compaction.
const COMPACTION_FILL_PERCENT: u64 = 75;

/// A capped, owner-only byte log.
pub struct Journal {
    path: PathBuf,
    cap_bytes: u64,
    len_bytes: u64,
    dropped_bytes: u64,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal").finish_non_exhaustive()
    }
}

impl Journal {
    /// Opens or creates the journal at `path`, mode `0600`.
    ///
    /// An existing journal longer than the cap is trimmed on open rather than at
    /// the first append, so a cap lowered between runs takes effect immediately.
    pub fn open(path: &Path, config: JournalConfig) -> Result<Self, JournalError> {
        if config.cap_bytes < MIN_JOURNAL_CAP_BYTES {
            return Err(JournalError::CapTooSmall {
                cap: config.cap_bytes,
                minimum: MIN_JOURNAL_CAP_BYTES,
            });
        }
        let mut journal = Self {
            path: path.to_owned(),
            cap_bytes: config.cap_bytes,
            len_bytes: 0,
            dropped_bytes: 0,
        };
        journal.ensure_file()?;
        let current = fs::metadata(path)
            .map_err(|source| journal.io(source))?
            .len();
        journal.len_bytes = current;
        journal.trim_to(config.cap_bytes)?;
        Ok(journal)
    }

    /// Appends output, discarding the oldest bytes if the cap is exceeded.
    ///
    /// Never fails for being over the cap: over-cap is the normal case, and the
    /// caller is the PTY read loop, which has nothing useful to do with a
    /// refusal.
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), JournalError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.path)
            .map_err(|source| self.io(source))?;
        file.write_all(bytes).map_err(|source| self.io(source))?;
        self.len_bytes = self.len_bytes.saturating_add(bytes.len() as u64);
        if self.len_bytes > self.cap_bytes {
            let target = self.cap_bytes.saturating_mul(COMPACTION_FILL_PERCENT) / 100;
            self.trim_to(target)
        } else {
            Ok(())
        }
    }

    /// Bytes currently retained. Never exceeds [`Journal::cap_bytes`].
    pub fn len_bytes(&self) -> u64 {
        self.len_bytes
    }

    /// The configured cap.
    pub fn cap_bytes(&self) -> u64 {
        self.cap_bytes
    }

    /// Bytes discarded over this journal's life.
    ///
    /// A client that reads the journal needs to know history was truncated
    /// rather than silently shorter than it expected.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }

    /// Everything currently retained, oldest first.
    pub fn read_all(&self) -> Result<Vec<u8>, JournalError> {
        fs::read(&self.path).map_err(|source| self.io(source))
    }

    /// The most recent `limit` bytes.
    pub fn read_tail(&self, limit: u64) -> Result<Vec<u8>, JournalError> {
        let start = self.len_bytes.saturating_sub(limit);
        let mut file = fs::File::open(&self.path).map_err(|source| self.io(source))?;
        file.seek(SeekFrom::Start(start))
            .map_err(|source| self.io(source))?;
        let mut bytes = Vec::with_capacity((self.len_bytes - start) as usize);
        file.read_to_end(&mut bytes)
            .map_err(|source| self.io(source))?;
        Ok(bytes)
    }

    /// Flushes buffered appends to the file.
    pub fn sync(&mut self) -> Result<(), JournalError> {
        OpenOptions::new()
            .write(true)
            .open(&self.path)
            .map_err(|source| self.io(source))?
            .sync_all()
            .map_err(|source| self.io(source))
    }

    fn ensure_file(&self) -> Result<(), JournalError> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .mode(FILE_MODE)
            .open(&self.path)
            .map_err(|source| self.io(source))?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(FILE_MODE))
            .map_err(|source| self.io(source))
    }

    fn trim_to(&mut self, target_bytes: u64) -> Result<(), JournalError> {
        if self.len_bytes <= target_bytes {
            return Ok(());
        }
        let drop_count = self.len_bytes - target_bytes;
        let mut source = fs::File::open(&self.path).map_err(|error| self.io(error))?;
        source
            .seek(SeekFrom::Start(drop_count))
            .map_err(|error| self.io(error))?;
        let mut kept = Vec::with_capacity(self.cap_bytes as usize);
        source
            .read_to_end(&mut kept)
            .map_err(|error| self.io(error))?;
        let temp = self.path.with_extension("journal.tmp");
        let mut replacement = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temp)
            .map_err(|error| self.io(error))?;
        replacement
            .write_all(&kept)
            .map_err(|error| self.io(error))?;
        replacement.sync_all().map_err(|error| self.io(error))?;
        drop(replacement);
        fs::rename(&temp, &self.path).map_err(|error| self.io(error))?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(FILE_MODE))
            .map_err(|error| self.io(error))?;
        self.len_bytes = kept.len() as u64;
        self.dropped_bytes = self.dropped_bytes.saturating_add(drop_count);
        Ok(())
    }

    fn io(&self, source: std::io::Error) -> JournalError {
        JournalError::Io {
            path: self.path.clone(),
            source,
        }
    }
}
