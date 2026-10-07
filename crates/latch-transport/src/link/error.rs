//! Fail-closed remote-link errors.

/// Fail-closed remote-link errors.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// Invalid local inputs.
    #[error("invalid remote-link configuration: {0}")]
    Configuration(&'static str),
    /// Authentication or protocol mismatch.
    #[error("remote-link authentication failed: {0}")]
    Authentication(String),
    /// The peer or transport exceeded a fixed bound.
    #[error("remote-link limit exceeded: {0}")]
    Limit(&'static str),
    /// The operation exceeded its deadline.
    #[error("remote-link operation timed out")]
    Timeout,
    /// The relay never reported the opposite endpoint within the wait bound.
    /// This says nothing about the peer's own connectivity: it was not
    /// reachable through the relay, which is all the relay can report.
    #[error("the paired endpoint was not reachable through the relay")]
    PeerUnavailable,
    /// The link has closed.
    #[error("remote link is closed")]
    Closed,
    /// Underlying I/O failed.
    #[error("remote-link I/O failed: {0}")]
    Io(String),
}

pub(super) fn auth_error(error: snow::Error) -> LinkError {
    LinkError::Authentication(error.to_string())
}

pub(super) fn io_error(error: std::io::Error) -> LinkError {
    LinkError::Io(error.to_string())
}
