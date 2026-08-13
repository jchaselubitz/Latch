//! Attach a terminal to a session through tmux.

use std::time::Duration;

use anyhow::{bail, Context};

use crate::engine;
use crate::session::meta;
use crate::session::paths::{LatchHome, SessionId};

/// How an attach was requested.
#[derive(Debug, Clone)]
pub struct AttachOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Session id or display name; omitted selects the newest session.
    pub session: Option<String>,
    /// Retained for CLI compatibility. tmux attachments are shared.
    pub watch: bool,
    /// Retained for CLI compatibility. Attach no longer has exclusive control.
    pub steal: bool,
}

/// How an attach ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
    /// The tmux client detached.
    Detached,
    /// The hosted process exited while attached.
    SessionExited {
        /// Exit status.
        code: Option<i32>,
    },
    /// The session was already showing a retained dead pane.
    AlreadyExited {
        /// Exit status.
        code: Option<i32>,
    },
}

/// Compatibility retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts after the initial attach.
    pub attempts: u32,
    /// First retry delay.
    pub initial_backoff: Duration,
    /// Retry delay ceiling.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 5,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(1),
        }
    }
}

impl RetryPolicy {
    /// No retries.
    pub const NONE: Self = Self {
        attempts: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
    };

    /// Delay before a 1-based retry attempt.
    pub fn backoff(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        self.initial_backoff
            .checked_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX))
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff)
    }

    /// Total retry delay budget.
    pub fn budget(&self) -> Duration {
        (1..=self.attempts)
            .map(|attempt| self.backoff(attempt))
            .sum()
    }
}

/// Legacy connection-state type kept for library compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// Attached.
    Connected {
        /// Session id.
        session: String,
        /// Legacy watcher flag.
        watching: bool,
    },
    /// Attach failed.
    Lost {
        /// Session id.
        session: String,
        /// Failure detail.
        reason: String,
    },
}

impl ConnectionState {
    /// Human-readable state.
    pub fn line(&self) -> String {
        match self {
            Self::Connected { session, .. } => format!("attached to {session}"),
            Self::Lost { session, reason } => {
                format!("connection to {session} failed: {reason}")
            }
        }
    }
}

/// Attaches once.
pub fn attach(options: AttachOptions) -> anyhow::Result<AttachOutcome> {
    attach_with_retry(options, RetryPolicy::NONE)
}

/// Attaches, retrying only if tmux itself fails transiently.
pub fn attach_with_retry(
    options: AttachOptions,
    policy: RetryPolicy,
) -> anyhow::Result<AttachOutcome> {
    let id = match options.session.as_deref() {
        Some(session) => resolve_session(&options.home, session)?,
        None => most_recent_session(&options.home)?,
    };
    let before = engine::inspect(&options.home, &id)?
        .with_context(|| format!("session {id} is not available in the Latch server"))?;
    let was_exited = before.state == engine::SessionState::Exited;

    let mut attempt = 0;
    loop {
        match engine::attach(&options.home, &id) {
            Ok(()) => break,
            Err(error) if attempt < policy.attempts => {
                attempt += 1;
                std::thread::sleep(policy.backoff(attempt));
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }

    let after = engine::inspect(&options.home, &id)?;
    let code = after.as_ref().and_then(|info| info.exit_status);
    if was_exited {
        Ok(AttachOutcome::AlreadyExited { code })
    } else if after.is_some_and(|info| info.state == engine::SessionState::Exited) {
        Ok(AttachOutcome::SessionExited { code })
    } else {
        Ok(AttachOutcome::Detached)
    }
}

/// Resolves an id or unique display name.
pub fn resolve_session(home: &LatchHome, session: &str) -> anyhow::Result<SessionId> {
    if let Ok(id) = SessionId::parse(session) {
        return Ok(id);
    }
    let matches = home
        .session_ids()?
        .into_iter()
        .filter_map(|id| {
            let metadata = meta::read(&home.session(&id)).ok()?;
            (metadata.name == session).then_some(id)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok(id.clone()),
        [] => bail!("no session named `{session}`"),
        _ => bail!("session name `{session}` is ambiguous; use its session id"),
    }
}

fn most_recent_session(home: &LatchHome) -> anyhow::Result<SessionId> {
    let live = engine::list(home)?;
    home.session_ids()?
        .into_iter()
        .rev()
        .find(|id| live.iter().any(|session| session.id == id.as_str()))
        .context("no sessions are available to attach")
}
