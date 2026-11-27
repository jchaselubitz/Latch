//! Gateway-instance references for exact, paired-device worktree operations.

use std::collections::VecDeque;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use super::{worktree, GitError, Worktree};
use crate::cli::remote_access::{control_grant_revision, random_hex};
use crate::cli::serve::contract::GitWorktreeOpen;
use crate::session::paths::LatchHome;

const REFERENCE_LIFETIME: Duration = Duration::from_secs(30 * 60);
const MAX_PER_DEVICE: usize = 32;
const MAX_REFERENCES: usize = 1024;

/// Private to one host gateway. Restarting it requires opening fresh references.
#[derive(Clone, Default)]
pub(crate) struct WorktreeReferences(Arc<Mutex<VecDeque<Arc<Binding>>>>);

pub(crate) struct Binding {
    reference: String,
    device: String,
    grant_revision: u64,
    opened: Instant,
    source: String,
    pub worktree: Worktree,
    identity: [FileIdentity; 4],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileIdentity {
    device: u64,
    inode: u64,
    // Include birth time when available to distinguish recycled inodes.
    created: Option<SystemTime>,
}

/// The identity of an accepted write. The reference is retained with the
/// receipt so a new gateway can recover it without reopening the checkout.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReceiptScope {
    pub reference: String,
    pub grant_revision: u64,
    source: String,
    identity: [FileIdentity; 4],
}

impl ReceiptScope {
    pub fn matches(&self, root: &Path, current: &Worktree) -> bool {
        current.root == root
            && worktree::canonical_folder(&self.source).is_ok_and(|source| source == root)
            && identity(current).is_ok_and(|found| found == self.identity)
    }
}

impl From<Metadata> for FileIdentity {
    fn from(metadata: Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().ok(),
        }
    }
}

fn identity(worktree: &Worktree) -> std::io::Result<[FileIdentity; 4]> {
    let directory = |path: &Path| fs::metadata(path).map(FileIdentity::from);
    Ok([
        directory(&worktree.root)?,
        fs::symlink_metadata(worktree.root.join(".git"))?.into(),
        directory(&worktree.git_dir)?,
        directory(&worktree.common_dir)?,
    ])
}

pub(crate) async fn control_revision(home: &LatchHome, device: &str) -> Result<u64, GitError> {
    let home = home.clone();
    let device = device.to_owned();
    tokio::task::spawn_blocking(move || control_grant_revision(&home, &device))
        .await
        .map_err(|_| GitError::Failed)?
        .ok_or(GitError::WorktreeAccessDenied)
}

impl WorktreeReferences {
    pub async fn open(
        &self,
        home: &LatchHome,
        device: &str,
        source: String,
    ) -> Result<GitWorktreeOpen, GitError> {
        let grant_revision = control_revision(home, device).await?;
        let folder = worktree::canonical_folder(&source)?;
        let root_identity = fs::metadata(&folder)
            .map(FileIdentity::from)
            .map_err(|_| GitError::UnavailablePath)?;
        let worktree = worktree::resolve_canonical(&folder).await?;
        if folder != worktree.root {
            return Err(GitError::WorktreeRootRequired);
        }
        let identity = identity(&worktree).map_err(|_| GitError::StaleWorktreeReference)?;
        if identity[0] != root_identity {
            return Err(GitError::StaleWorktreeReference);
        }
        let binding = Binding {
            reference: random_hex(32).map_err(|_| GitError::Failed)?,
            device: device.to_owned(),
            grant_revision,
            opened: Instant::now(),
            source,
            worktree,
            identity,
        };
        binding.validate(home).await?;
        let output = GitWorktreeOpen {
            worktree_ref: binding.reference.clone(),
            repository: binding.worktree.repository(),
        };
        let mut entries = self.0.lock().map_err(|_| GitError::Failed)?;
        entries.retain(|entry| {
            !entry.expired() && !(entry.device == device && entry.grant_revision != grant_revision)
        });
        if entries
            .iter()
            .filter(|entry| entry.device == device)
            .count()
            >= MAX_PER_DEVICE
        {
            if let Some(oldest) = entries.iter().position(|entry| entry.device == device) {
                entries.remove(oldest);
            }
        }
        if entries.len() >= MAX_REFERENCES {
            entries.pop_front();
        }
        entries.push_back(Arc::new(binding));
        Ok(output)
    }

    pub async fn resolve(
        &self,
        home: &LatchHome,
        device: &str,
        reference: &str,
    ) -> Result<Arc<Binding>, GitError> {
        // Check current permission even for an unknown reference. Never reveal
        // whether a reference from another device exists.
        let revision = control_revision(home, device).await?;
        let binding = self
            .0
            .lock()
            .map_err(|_| GitError::Failed)?
            .iter()
            .find(|entry| entry.device == device && entry.reference == reference)
            .cloned()
            .ok_or(GitError::StaleWorktreeReference)?;
        if binding.grant_revision != revision {
            return Err(GitError::StaleWorktreeReference);
        }
        binding.validate_identity().await?;
        Ok(binding)
    }
}

impl Binding {
    pub fn receipt_scope(&self) -> ReceiptScope {
        ReceiptScope {
            reference: self.reference.clone(),
            grant_revision: self.grant_revision,
            source: self.source.clone(),
            identity: self.identity.clone(),
        }
    }

    fn expired(&self) -> bool {
        self.opened.elapsed() >= REFERENCE_LIFETIME
    }

    async fn validate_identity(&self) -> Result<(), GitError> {
        let stale = GitError::StaleWorktreeReference;
        if self.expired()
            || worktree::canonical_folder(&self.source).map_err(|_| stale)? != self.worktree.root
            || identity(&self.worktree).map_err(|_| stale)? != self.identity
        {
            return Err(stale);
        }
        let current = worktree::resolve_canonical(&self.worktree.root)
            .await
            .map_err(|error| match error {
                GitError::GitUnavailable | GitError::TimedOut => error,
                _ => stale,
            })?;
        if current != self.worktree
            || worktree::canonical_folder(&self.source).map_err(|_| stale)? != current.root
            || identity(&current).map_err(|_| stale)? != self.identity
        {
            return Err(stale);
        }
        Ok(())
    }

    /// Discard a completed read if the checkout or pairing changed in flight.
    pub async fn validate(&self, home: &LatchHome) -> Result<(), GitError> {
        self.validate_identity().await?;
        if control_revision(home, &self.device).await? != self.grant_revision {
            return Err(GitError::StaleWorktreeReference);
        }
        Ok(())
    }
}
