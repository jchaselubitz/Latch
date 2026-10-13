//! The one path every Git mutation takes.
//!
//! 1. The request id is looked up among this device's receipts. The same id
//!    with the same arguments returns the first receipt and never runs Git
//!    again; with different arguments it is refused.
//! 2. The folder is resolved, and a pending receipt is written and flushed
//!    before anything else happens.
//! 3. The work runs in its own task, so a phone that disconnects cannot
//!    cancel Git half-way. Under a per-worktree lock the folder is resolved
//!    again, the repository is observed afresh and checked against what the
//!    phone reviewed; only then does Git run.
//! 4. The receipt is rewritten with the outcome.
//!
//! The lock serializes Latch's own mutations of one working tree. It cannot
//! stop a terminal or an agent on the Mac, which is why every mutation
//! rechecks its preconditions under the lock rather than trusting the order.
//! A receipt still pending when nothing in this gateway is running it (the
//! gateway restarted) is settled by reading the repository, and stays
//! uncertain whenever the repository cannot say what happened.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::command::{self, ProgressSink};
use super::ops::{self, Interruption, Mutation, Outcome};
use super::receipts::{self, Before, Claim, Stored};
use super::{observe, worktree, GitError};
use crate::cli::serve::contract::{
    GitMutationReceipt, GitPreconditions, GitProgress, GitReceiptState,
};
use crate::session::paths::LatchHome;

/// What a submission came to.
#[derive(Debug)]
pub(crate) enum Submitted {
    /// Completed, rejected or uncertain.
    Settled(GitMutationReceipt),
    /// Still running (a repeat of a request that has not finished).
    Pending(GitMutationReceipt),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MutationError {
    /// The folder could not be resolved; nothing was accepted.
    Git(GitError),
    /// This device already used the id for different arguments.
    RequestIdConflict,
    /// The receipt could not be read or written; nothing ran.
    Storage,
}

/// Serializes claiming an id with registering it as running, so a lookup
/// never mistakes a request that is about to start for an abandoned one.
static CLAIMS: Mutex<()> = Mutex::new(());

/// Receipt files of requests this gateway is running now.
static RUNNING: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Git's latest progress line for each request this gateway is running.
static PROGRESS: LazyLock<Mutex<HashMap<PathBuf, GitProgress>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How long a long-running mutation's submission waits before answering
/// with the pending receipt. Git keeps running; the phone reads the receipt.
const EARLY_ANSWER: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 20 });

/// Characters of a progress line kept.
const PROGRESS_CHARS: usize = 500;

fn progress_sink(key: PathBuf) -> ProgressSink {
    std::sync::Arc::new(move |line: &str| {
        let message = line.chars().take(PROGRESS_CHARS).collect::<String>();
        let progress = GitProgress {
            percent: command::percent(&message),
            message,
            updated_at: crate::engine::format_rfc3339(SystemTime::now()),
        };
        PROGRESS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.clone(), progress);
    })
}

/// A pending receipt with the latest progress this gateway has seen.
fn with_progress(mut receipt: GitMutationReceipt, key: &PathBuf) -> GitMutationReceipt {
    if receipt.state == GitReceiptState::Pending {
        receipt.progress = PROGRESS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned();
    }
    receipt
}

/// One lock per working tree root.
static WORKTREE_LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_for(root: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = WORKTREE_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks.entry(root.to_owned()).or_default().clone()
}

fn running_key(home: &LatchHome, device: Option<&str>, request_id: &str) -> PathBuf {
    home.root()
        .join(device.unwrap_or("\0local"))
        .join(request_id.to_ascii_lowercase())
}

fn is_running(key: &PathBuf) -> bool {
    RUNNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(key)
}

/// Digest of everything a request asks for. The request id itself is not
/// part of it; the folder is as the phone sent it.
fn digest(path: &str, expected: &GitPreconditions, mutation: &Mutation) -> String {
    let bytes = serde_json::to_vec(&(path, expected, mutation)).unwrap_or_default();
    super::status::hex(&Sha256::digest(bytes))
}

/// Accepts and runs one mutation, or answers a repeat of it.
pub(crate) async fn submit(
    home: &LatchHome,
    device: Option<String>,
    request_id: &str,
    path: &str,
    expected: GitPreconditions,
    mutation: Mutation,
) -> Result<Submitted, MutationError> {
    let digest = digest(path, &expected, &mutation);
    let device_ref = device.as_deref();
    if let Some(existing) =
        receipts::load(home, device_ref, request_id).map_err(|_| MutationError::Storage)?
    {
        return repeat(home, existing, &digest).await;
    }

    let worktree = worktree::resolve(path).await.map_err(MutationError::Git)?;
    let receipt = GitMutationReceipt {
        request_id: request_id.to_owned(),
        operation: mutation.kind(),
        root: worktree.root_str().to_owned(),
        state: GitReceiptState::Pending,
        accepted_at: crate::engine::format_rfc3339(SystemTime::now()),
        finished_at: None,
        outcome: None,
        started_at: None,
        progress: None,
    };
    let answers_early = mutation.answers_early();
    let stored = Stored::new(device.clone(), digest.clone(), mutation, receipt);

    let key = running_key(home, device_ref, request_id);
    let claim = {
        let _claims = CLAIMS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let claim = receipts::claim(home, &stored).map_err(|_| MutationError::Storage)?;
        if matches!(claim, Claim::Claimed) {
            RUNNING
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(key.clone());
        }
        claim
    };
    if let Claim::Exists(existing) = claim {
        return repeat(home, *existing, &digest).await;
    }

    let task_home = home.clone();
    let task_key = key.clone();
    let mut running = tokio::spawn(async move {
        let receipt = run(&task_home, stored, worktree, expected, task_key.clone()).await;
        RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&task_key);
        PROGRESS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&task_key);
        receipt
    });
    let joined = if answers_early {
        match tokio::time::timeout(EARLY_ANSWER, &mut running).await {
            Ok(joined) => joined,
            // Still running: answer with the receipt as it stands. The task
            // goes on without anyone waiting for it.
            Err(_) => {
                return match receipts::load(home, device_ref, request_id) {
                    Ok(Some(stored)) => Ok(Submitted::Pending(with_progress(stored.receipt, &key))),
                    _ => Err(MutationError::Storage),
                };
            }
        }
    } else {
        running.await
    };
    match joined {
        Ok(receipt) => Ok(Submitted::Settled(receipt)),
        // The task panicked. Its receipt says how far it got; a lookup
        // reconciles it.
        Err(_) => match receipts::load(home, device_ref, request_id) {
            Ok(Some(stored)) => Ok(Submitted::Pending(stored.receipt)),
            _ => Err(MutationError::Storage),
        },
    }
}

/// A request id this device already used.
async fn repeat(
    home: &LatchHome,
    existing: Stored,
    digest: &str,
) -> Result<Submitted, MutationError> {
    if existing.digest != digest {
        return Err(MutationError::RequestIdConflict);
    }
    let settled = settle(home, existing).await?;
    Ok(match settled.state {
        GitReceiptState::Pending => Submitted::Pending(settled),
        _ => Submitted::Settled(settled),
    })
}

/// `GET /v2/git/receipts/{id}`: this device's receipt, settled first if no
/// one is running it any more.
pub(crate) async fn receipt(
    home: &LatchHome,
    device: Option<&str>,
    request_id: &str,
) -> Result<Option<GitMutationReceipt>, MutationError> {
    match receipts::load(home, device, request_id).map_err(|_| MutationError::Storage)? {
        None => Ok(None),
        Some(stored) => settle(home, stored).await.map(Some),
    }
}

/// Returns a settled receipt as it is. A pending one that this gateway is
/// running is returned as pending; one nothing is running is reconciled.
async fn settle(home: &LatchHome, stored: Stored) -> Result<GitMutationReceipt, MutationError> {
    if stored.receipt.state != GitReceiptState::Pending {
        return Ok(stored.receipt);
    }
    let key = running_key(home, stored.device.as_deref(), &stored.receipt.request_id);
    {
        let _claims = CLAIMS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if is_running(&key) {
            return Ok(with_progress(stored.receipt, &key));
        }
        RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.clone());
    }
    let task_home = home.clone();
    let reconciled = tokio::spawn(async move {
        let receipt = reconcile_abandoned(&task_home, stored).await;
        RUNNING
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&key);
        receipt
    })
    .await
    .map_err(|_| MutationError::Storage)?;
    reconciled
}

/// Settles a receipt left pending by a gateway that stopped.
async fn reconcile_abandoned(
    home: &LatchHome,
    stored: Stored,
) -> Result<GitMutationReceipt, MutationError> {
    let root = PathBuf::from(&stored.receipt.root);
    let lock = lock_for(&root);
    let _guard = lock.lock().await;
    // Reread under the lock: whoever held it may have settled it.
    let mut stored = receipts::load(home, stored.device.as_deref(), &stored.receipt.request_id)
        .map_err(|_| MutationError::Storage)?
        .unwrap_or(stored);
    if stored.receipt.state != GitReceiptState::Pending {
        return Ok(stored.receipt);
    }
    if !stored.mutation.pushes()
        && stored.before.is_some()
        && receipts::git_may_be_running(home, stored.device.as_deref(), &stored.receipt.request_id)
    {
        return finish(home, &mut stored, Outcome::uncertain(
            "interrupted", "Git from the previous gateway may still be running. Check on your Mac before starting another change."
        ).reconciled());
    }
    let outcome = match worktree::resolve_canonical(&root).await {
        Ok(current) if current.root == root => match observe(&current).await {
            Ok(observed) => {
                ops::reconcile(
                    &stored.mutation,
                    stored.before.as_ref(),
                    &current,
                    &observed,
                    Interruption::Interrupted,
                )
                .await
            }
            Err(_) => unknown(&stored),
        },
        _ => unknown(&stored),
    };
    finish(home, &mut stored, outcome)
}

/// The repository cannot be read: if Git never started nothing happened,
/// otherwise nobody can say.
fn unknown(stored: &Stored) -> Outcome {
    if stored.before.is_none() {
        Outcome::rejected(
            "not_applied",
            "Latch stopped before running Git for this request. Nothing was changed.",
        )
        .reconciled()
    } else {
        Outcome::uncertain(
            "interrupted",
            "Latch stopped while Git ran, and the repository can't be read now to see what changed.",
        )
        .reconciled()
    }
}

async fn run(
    home: &LatchHome,
    mut stored: Stored,
    accepted: worktree::Worktree,
    expected: GitPreconditions,
    key: PathBuf,
) -> GitMutationReceipt {
    let lock = lock_for(&accepted.root);
    let _guard = lock.lock().await;
    let outcome = attempt(home, &mut stored, &accepted, &expected, key).await;
    // Git has exited or been killed with its whole group by now.
    receipts::clear_pid(home, stored.device.as_deref(), &stored.receipt.request_id);
    match finish(home, &mut stored, outcome) {
        Ok(receipt) => receipt,
        // The outcome could not be recorded; the phone still gets it, and
        // a later lookup reconciles what the file says.
        Err(_) => stored.receipt,
    }
}

async fn attempt(
    home: &LatchHome,
    stored: &mut Stored,
    accepted: &worktree::Worktree,
    expected: &GitPreconditions,
    key: PathBuf,
) -> Outcome {
    // The folder is resolved again under the lock: it may have been moved,
    // replaced or made bare since the request was accepted.
    let current = match worktree::resolve_canonical(&accepted.root).await {
        Ok(current) if current == *accepted => current,
        Ok(_) => {
            return Outcome::refresh_required(
                "The repository changed shape on your Mac. Refresh and open it again.",
            )
        }
        Err(error) => return ops::from_git_error(error),
    };
    let observed = match observe(&current).await {
        Ok(observed) => observed,
        Err(error) => return ops::from_git_error(error),
    };
    let plan = match ops::check(&stored.mutation, expected, &current, &observed).await {
        Ok(plan) => plan,
        Err(refusal) => {
            let now = &observed.snapshot.preconditions;
            let mut refusal = refusal;
            refusal.outcome.head = now.head.clone();
            refusal.outcome.branch = now.branch.clone();
            return refusal;
        }
    };
    let before = Before {
        head: observed.snapshot.preconditions.head.clone(),
        branch: observed.snapshot.preconditions.branch.clone(),
        targets: plan
            .targets
            .iter()
            .map(|path| super::status::hex(path))
            .collect(),
        source: plan.source.clone(),
        fetch_head: plan.fetch_head.clone(),
        operation: observed.operation.clone(),
        destination: plan.destination.clone(),
    };
    stored.before = Some(before.clone());
    stored.receipt.started_at = Some(crate::engine::format_rfc3339(SystemTime::now()));
    // From here a crash means Git may have run; the receipt must say so
    // before Git starts.
    if receipts::save(home, stored).is_err() {
        stored.before = None;
        stored.receipt.started_at = None;
        return Outcome::rejected(
            "receipt_unavailable",
            "Latch could not record this request on your Mac, so it did not run Git.",
        );
    }
    let mut plan = plan;
    {
        // Every mutation records its process group, so restart recovery
        // never treats a surviving Git/hook/helper as a stopped operation.
        let (home, device, request_id) = (
            home.clone(),
            stored.device.clone(),
            stored.receipt.request_id.clone(),
        );
        plan.extras.on_spawn = Some(Arc::new(move |pid| {
            receipts::record_pid(&home, device.as_deref(), &request_id, pid);
        }));
    }
    ops::execute(
        &stored.mutation,
        plan,
        &current,
        &before,
        Some(progress_sink(key)),
    )
    .await
}

fn finish(
    home: &LatchHome,
    stored: &mut Stored,
    outcome: Outcome,
) -> Result<GitMutationReceipt, MutationError> {
    stored.receipt.state = outcome.state;
    stored.receipt.progress = None;
    stored.receipt.outcome = Some(*outcome.outcome);
    stored.receipt.finished_at = Some(crate::engine::format_rfc3339(SystemTime::now()));
    receipts::save(home, stored).map_err(|_| MutationError::Storage)?;
    Ok(stored.receipt.clone())
}

/// Why a push could not be checked against the remote.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PushCheckError {
    /// This device has no receipt with that id.
    NotFound,
    /// The receipt is not for a push.
    NotAPush,
    /// Git is still running for it here; keep reading the receipt.
    StillRunning,
    /// Another Latch change holds this working tree; try again shortly.
    Busy,
    Storage,
}

/// What `POST /v2/git/push/reconcile` found.
#[derive(Debug)]
pub(crate) struct PushCheck {
    pub receipt: GitMutationReceipt,
    pub remote_checked: bool,
    pub remote_oid: Option<String>,
    pub message: String,
}

/// How long a check waits for the working tree's lock before answering busy.
const CHECK_LOCK_WAIT: Duration = Duration::from_secs(20);

/// Settles this device's uncertain push by asking the remote, only because
/// the user asked. Never pushes. The receipt changes only when the evidence
/// decides it; otherwise it stays uncertain and the answer says why.
pub(crate) async fn check_push(
    home: &LatchHome,
    device: Option<&str>,
    request_id: &str,
) -> Result<PushCheck, PushCheckError> {
    let stored = receipts::load(home, device, request_id)
        .map_err(|_| PushCheckError::Storage)?
        .ok_or(PushCheckError::NotFound)?;
    if !stored.mutation.pushes() {
        return Err(PushCheckError::NotAPush);
    }
    // A pending receipt nothing is running is reconciled locally first.
    let current = settle(home, stored)
        .await
        .map_err(|_| PushCheckError::Storage)?;
    let unchanged = |receipt: GitMutationReceipt, message: String| PushCheck {
        receipt,
        remote_checked: false,
        remote_oid: None,
        message,
    };
    match current.state {
        GitReceiptState::Pending => return Err(PushCheckError::StillRunning),
        GitReceiptState::Completed | GitReceiptState::Rejected => {
            let message = current
                .outcome
                .as_ref()
                .map(|outcome| outcome.message.clone())
                .unwrap_or_default();
            return Ok(unchanged(current, message));
        }
        GitReceiptState::Uncertain => {}
    }
    let root = PathBuf::from(&current.root);
    let lock = lock_for(&root);
    let Ok(_guard) = tokio::time::timeout(CHECK_LOCK_WAIT, lock.lock()).await else {
        return Err(PushCheckError::Busy);
    };
    let mut stored = receipts::load(home, device, request_id)
        .map_err(|_| PushCheckError::Storage)?
        .ok_or(PushCheckError::NotFound)?;
    if stored.receipt.state != GitReceiptState::Uncertain {
        let message = stored
            .receipt
            .outcome
            .as_ref()
            .map(|outcome| outcome.message.clone())
            .unwrap_or_default();
        return Ok(unchanged(stored.receipt, message));
    }
    if is_running(&running_key(home, device, request_id)) {
        return Err(PushCheckError::StillRunning);
    }
    let git_stopped = !receipts::git_may_be_running(home, device, request_id)
        && !super::push::too_soon(receipts::written_at(home, device, request_id));
    let worktree = match worktree::resolve_canonical(&root).await {
        Ok(found) if found.root == root => found,
        _ => {
            return Ok(unchanged(
                stored.receipt,
                "The repository is no longer where it was on your Mac, so Latch can't ask its remote. The push is still unconfirmed.".to_owned(),
            ))
        }
    };
    let verdict = super::push::verdict(
        &stored.mutation,
        stored.before.as_ref(),
        &worktree,
        git_stopped,
    )
    .await;
    if let Some(outcome) = verdict.outcome {
        let outcome = outcome.at(
            ops::rev(&worktree, "HEAD").await,
            ops::current_branch(&worktree).await,
        );
        finish(home, &mut stored, outcome).map_err(|_| PushCheckError::Storage)?;
        receipts::clear_pid(home, device, request_id);
    }
    Ok(PushCheck {
        receipt: stored.receipt,
        remote_checked: verdict.remote_checked,
        remote_oid: verdict.remote_oid,
        message: verdict.message,
    })
}
