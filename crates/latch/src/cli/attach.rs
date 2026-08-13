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
}

/// Retry policy for `latch attach --retry`.
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
}

/// Attaches once.
pub fn attach(options: AttachOptions) -> anyhow::Result<()> {
    attach_session(options, None)
}

/// Attaches, retrying only transient failures (server not yet up, or a still-
/// running session whose attach client dropped). Permanent errors — missing
/// session, exited pane, missing tmux binary — fail immediately with the last
/// error.
pub fn attach_with_retry(options: AttachOptions, policy: RetryPolicy) -> anyhow::Result<()> {
    attach_session(options, Some(policy))
}

fn attach_session(options: AttachOptions, policy: Option<RetryPolicy>) -> anyhow::Result<()> {
    let id = match options.session.as_deref() {
        Some(session) => resolve_session(&options.home, session)?,
        None => most_recent_session(&options.home)?,
    };

    let extra_attempts = policy.map(|policy| policy.attempts).unwrap_or(0);
    let mut attempt = 0;
    loop {
        match attach_once(&options.home, &id) {
            Ok(()) => return Ok(()),
            Err(_)
                if attempt < extra_attempts && engine::attach_is_retryable(&options.home, &id) =>
            {
                attempt += 1;
                if let Some(policy) = policy {
                    std::thread::sleep(policy.backoff(attempt));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn attach_once(home: &LatchHome, id: &SessionId) -> anyhow::Result<()> {
    match engine::inspect(home, id)? {
        Some(_) => engine::attach(home, id),
        None if engine::tmux_server_is_absent(home) => {
            bail!("no Latch tmux server is running")
        }
        None => bail!(SessionLookupError::NotInServer {
            session: id.to_string(),
        }),
    }
}

/// Why a session id or display name could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum SessionLookupError {
    /// No metadata session exists for this id or name.
    #[error("no session `{session}`")]
    NotFound {
        /// Id or name the caller asked for.
        session: String,
    },
    /// The name is not used by any metadata session.
    #[error("no session named `{session}`")]
    UnknownName {
        /// Display name the caller asked for.
        session: String,
    },
    /// More than one session has this display name.
    #[error("session name `{session}` is ambiguous; use its session id")]
    Ambiguous {
        /// Display name that matched multiple sessions.
        session: String,
    },
    /// Metadata exists but the private tmux server does not have the session.
    #[error("session {session} is not available in the Latch server")]
    NotInServer {
        /// Session id.
        session: String,
    },
}

impl SessionLookupError {
    /// True when the HTTP gateway should answer 404 rather than 500.
    pub fn is_absent(&self) -> bool {
        !matches!(self, Self::Ambiguous { .. })
    }
}

/// Resolves an id or unique display name.
pub fn resolve_session(home: &LatchHome, session: &str) -> anyhow::Result<SessionId> {
    if let Ok(id) = SessionId::parse(session) {
        return Ok(id);
    }
    match session_ids_named(home, session)?.as_slice() {
        [id] => Ok(id.clone()),
        [] => bail!(SessionLookupError::UnknownName {
            session: session.to_owned(),
        }),
        _ => bail!(SessionLookupError::Ambiguous {
            session: session.to_owned(),
        }),
    }
}

/// Metadata session ids whose sanitized display name equals `name`.
pub(crate) fn session_ids_named(home: &LatchHome, name: &str) -> anyhow::Result<Vec<SessionId>> {
    Ok(home
        .session_ids()?
        .into_iter()
        .filter_map(|id| {
            let metadata = meta::read(&home.session(&id)).ok()?;
            (metadata.name == name).then_some(id)
        })
        .collect())
}

fn most_recent_session(home: &LatchHome) -> anyhow::Result<SessionId> {
    let known = home.session_ids()?;
    let session = engine::list(home)?
        .into_iter()
        .filter(|session| known.iter().any(|id| id.as_str() == session.id))
        .max_by(|left, right| {
            left.activity
                .cmp(&right.activity)
                .then_with(|| left.id.cmp(&right.id))
        })
        .context("no sessions are available to attach")?;
    Ok(SessionId::parse(&session.id)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_backoff_doubles_until_the_ceiling() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.backoff(0), Duration::ZERO);
        assert_eq!(policy.backoff(1), Duration::from_millis(100));
        assert_eq!(policy.backoff(2), Duration::from_millis(200));
        assert_eq!(policy.backoff(5), Duration::from_millis(1000));
        assert_eq!(policy.backoff(6), Duration::from_secs(1));
    }

    #[test]
    fn lookup_errors_keep_cli_wording_and_classify_absence() {
        let missing = SessionLookupError::NotFound {
            session: "ses_1".to_owned(),
        };
        assert_eq!(missing.to_string(), "no session `ses_1`");
        assert!(missing.is_absent());

        let named = SessionLookupError::UnknownName {
            session: "agent".to_owned(),
        };
        assert_eq!(named.to_string(), "no session named `agent`");
        assert!(named.is_absent());

        let ambiguous = SessionLookupError::Ambiguous {
            session: "agent".to_owned(),
        };
        assert_eq!(
            ambiguous.to_string(),
            "session name `agent` is ambiguous; use its session id"
        );
        assert!(!ambiguous.is_absent());

        let gone = SessionLookupError::NotInServer {
            session: "ses_1".to_owned(),
        };
        assert_eq!(
            gone.to_string(),
            "session ses_1 is not available in the Latch server"
        );
        assert!(gone.is_absent());
    }
}
