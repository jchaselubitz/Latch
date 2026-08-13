//! Nesting refusal: `LATCH_SESSION_ID` makes a session within a session refuse.
//!
//! With an iTerm profile pointed at `latch`, every new window is a session, and
//! running `latch` inside one is a daily occurrence rather than an edge case.
//! The worker exports this variable into the child; the CLI reads it before
//! creating anything and either attaches to the named session or declines.

pub use crate::session::paths::SESSION_ID_ENV;

/// What to do when `latch` is invoked and a session may already enclose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NestingDecision {
    /// No enclosing session; create (or attach to a named target) as asked.
    Allow,
    /// Inside a session; attach to it rather than create a nested one.
    AttachToEnclosing {
        /// The enclosing session's id, taken from [`SESSION_ID_ENV`].
        session_id: String,
    },
    /// Inside a session and the invocation cannot be satisfied by attaching.
    ///
    /// For example `latch run -- something` names a different command than the
    /// enclosing session is already running. Decline rather than nest.
    Decline {
        /// Why nesting was refused, for a human-readable error.
        reason: String,
    },
}

/// Decides whether this invocation may create a session.
///
/// `enclosing` is the value of [`SESSION_ID_ENV`] in this process's environment,
/// or `None` when unset. `wants_create` is true for bare `latch`, `shell`,
/// `run`, and `create` — the commands that would otherwise start a new session.
pub fn nesting_decision(enclosing: Option<&str>, wants_create: bool) -> NestingDecision {
    let Some(session_id) = enclosing.map(str::trim).filter(|id| !id.is_empty()) else {
        return NestingDecision::Allow;
    };
    if !wants_create {
        return NestingDecision::Allow;
    }
    NestingDecision::AttachToEnclosing {
        session_id: session_id.to_owned(),
    }
}
