//! The in-flight marker a viewer open leaves for the session launcher.
//!
//! Overlord creates a session and opens its viewer as two separate commands,
//! so the launcher inside tmux cannot see that a terminal is on its way — it
//! can only see that none has attached yet. `latch open` stamps this marker
//! before it asks the operating system for a window; the launcher reads it to
//! tell "a viewer is starting, keep holding" apart from "nobody is coming,
//! start headlessly", instead of guessing with one fixed timeout.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::session::paths::{SessionPaths, FILE_MODE};

/// How long a stamped viewer open still counts as in flight.
///
/// A marker outlives the command that wrote it when a viewer never appears
/// (a killed `latch open`, an operating system that swallowed the request), so
/// age — not existence — decides.
pub const PENDING_TTL: Duration = Duration::from_secs(30);

/// What `latch open` recorded before spawning a viewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerOpen {
    /// Viewer that was asked for.
    pub viewer: String,
    /// Shape requested: `new-window` or `new-tab`.
    pub behavior: String,
    /// RFC 3339 timestamp of the request.
    pub at: String,
}

/// Records that a viewer open is in flight for this session.
pub fn announce(paths: &SessionPaths, viewer: &str, behavior: &str) -> anyhow::Result<()> {
    let record = ViewerOpen {
        viewer: viewer.to_owned(),
        behavior: behavior.to_owned(),
        at: crate::engine::format_rfc3339(SystemTime::now()),
    };
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(paths.viewer_open())?;
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

/// Whether a recent viewer open is still expected to produce a terminal.
pub fn is_pending(paths: &SessionPaths) -> bool {
    age(paths).is_some_and(|age| age < PENDING_TTL)
}

/// Drops the marker once the viewer has arrived or the request has failed.
pub fn clear(paths: &SessionPaths) {
    let _ = fs::remove_file(paths.viewer_open());
}

fn age(paths: &SessionPaths) -> Option<Duration> {
    let modified = fs::metadata(paths.viewer_open()).ok()?.modified().ok()?;
    // A marker stamped in the future (clock change mid-launch) is treated as
    // brand new rather than as expired.
    Some(
        SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::{announce, clear, is_pending, ViewerOpen, PENDING_TTL};
    use crate::session::paths::SessionPaths;
    use std::time::{Duration, SystemTime};

    fn session() -> (tempfile::TempDir, SessionPaths) {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(directory.path().join("ses_01JTEST"));
        paths.ensure().unwrap();
        (directory, paths)
    }

    #[test]
    fn a_fresh_marker_reads_as_pending_and_clearing_it_ends_the_wait() {
        let (_directory, paths) = session();

        assert!(!is_pending(&paths), "nothing was announced yet");
        announce(&paths, "iterm", "new-window").unwrap();
        assert!(is_pending(&paths));

        let record: ViewerOpen =
            serde_json::from_str(&std::fs::read_to_string(paths.viewer_open()).unwrap()).unwrap();
        assert_eq!(record.viewer, "iterm");
        assert_eq!(record.behavior, "new-window");
        assert!(record.at.ends_with('Z'), "{}", record.at);

        clear(&paths);
        assert!(!is_pending(&paths));
    }

    /// A `latch open` that died without clearing its marker must not hold the
    /// next launcher forever, so the marker expires on its own.
    #[test]
    fn a_stale_marker_stops_counting_as_an_open_in_flight() {
        let (_directory, paths) = session();
        announce(&paths, "iterm", "new-tab").unwrap();

        let stale = SystemTime::now() - PENDING_TTL - Duration::from_secs(5);
        let file = std::fs::File::options()
            .write(true)
            .open(paths.viewer_open())
            .unwrap();
        file.set_modified(stale).unwrap();

        assert!(!is_pending(&paths));
    }
}
