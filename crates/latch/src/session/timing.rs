//! Per-session launch phase timings.
//!
//! Launch latency is split across three processes — `latch create`, `latch
//! open`, and the internal launcher under tmux — so no single exit status or
//! log line explains a slow start. Each process appends the phases it owns to
//! one JSONL sidecar in the session directory, which `latch inspect --json`
//! reads back. Appends are single small writes to an `O_APPEND` file, so the
//! three writers need no lock between them.
//!
//! Recording is telemetry: a failure here must never fail a launch, so
//! [`record`] swallows its own errors.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::engine;
use crate::session::paths::{SessionPaths, FILE_MODE};

/// One measured launch phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchPhase {
    /// Dotted phase name, e.g. `create.tmux_new_session`, `open.viewer`.
    pub phase: String,
    /// How long the phase took, in milliseconds.
    pub ms: u64,
    /// RFC 3339 timestamp at which the phase finished.
    pub at: String,
    /// How the phase ended, when the duration alone is ambiguous — the
    /// difference between "waited 3 s and a viewer arrived" and "waited 3 s and
    /// gave up" is the whole diagnosis.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

/// Successive segments of one process's work.
///
/// [`lap`](Stopwatch::lap) returns the time since the previous lap, so a
/// caller measures a sequence of phases without arithmetic at each step.
#[derive(Debug)]
pub struct Stopwatch {
    started: Instant,
    lap: Instant,
}

impl Default for Stopwatch {
    fn default() -> Self {
        Self::start()
    }
}

impl Stopwatch {
    /// Begins measuring.
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            lap: now,
        }
    }

    /// Time since the previous lap (or since the start).
    pub fn lap(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.lap);
        self.lap = now;
        elapsed
    }

    /// Time since the stopwatch started, leaving the lap point alone.
    pub fn total(&self) -> Duration {
        Instant::now().saturating_duration_since(self.started)
    }
}

/// Appends one phase to the session's timing sidecar.
///
/// Best effort by design: a session whose directory is gone, read-only, or
/// already removed still launches.
pub fn record(paths: &SessionPaths, phase: &str, elapsed: Duration, outcome: Option<&str>) {
    let record = LaunchPhase {
        phase: phase.to_owned(),
        ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        at: engine::format_rfc3339(std::time::SystemTime::now()),
        outcome: outcome.map(str::to_owned),
    };
    let _ = append(paths, &record);
}

fn append(paths: &SessionPaths, record: &LaunchPhase) -> anyhow::Result<()> {
    let mut line = serde_json::to_vec(record)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(paths.launch_timings())?;
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

/// Reads back every phase recorded for a session, oldest first.
///
/// Malformed lines are skipped rather than failing the read: the sidecar is a
/// diagnostic, and a truncated final record must not hide the phases before it.
pub fn read(paths: &SessionPaths) -> Vec<LaunchPhase> {
    let Ok(raw) = std::fs::read_to_string(paths.launch_timings()) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| serde_json::from_str::<LaunchPhase>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{read, record, LaunchPhase, Stopwatch};
    use crate::session::paths::SessionPaths;
    use std::time::Duration;

    #[test]
    fn phases_round_trip_in_the_order_they_were_recorded() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(directory.path().join("ses_01JTEST"));
        paths.ensure().unwrap();

        record(&paths, "create.total", Duration::from_millis(12), None);
        record(
            &paths,
            "launch.first_viewer_wait",
            Duration::from_millis(1_500),
            Some("attached"),
        );

        let phases = read(&paths);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].phase, "create.total");
        assert_eq!(phases[0].ms, 12);
        assert_eq!(phases[0].outcome, None);
        assert_eq!(phases[1].phase, "launch.first_viewer_wait");
        assert_eq!(phases[1].outcome.as_deref(), Some("attached"));
        assert!(!phases[1].at.is_empty());
    }

    /// The sidecar is appended by three processes; a partial line from one of
    /// them must not cost a reader the records that are intact.
    #[test]
    fn a_truncated_record_does_not_hide_the_phases_before_it() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(directory.path().join("ses_01JTEST"));
        paths.ensure().unwrap();
        record(&paths, "create.total", Duration::from_millis(3), None);
        std::fs::write(
            paths.launch_timings(),
            format!(
                "{}{{\"phase\":\"open.viewer\"",
                std::fs::read_to_string(paths.launch_timings()).unwrap()
            ),
        )
        .unwrap();

        let phases = read(&paths);

        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].phase, "create.total");
    }

    #[test]
    fn recording_into_a_missing_session_directory_is_not_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let paths = SessionPaths::new(directory.path().join("gone").join("ses_01JTEST"));

        record(&paths, "create.total", Duration::from_millis(1), None);

        assert!(read(&paths).is_empty());
    }

    #[test]
    fn laps_measure_successive_segments_and_the_total_keeps_running() {
        let mut watch = Stopwatch::start();
        std::thread::sleep(Duration::from_millis(20));
        let first = watch.lap();
        std::thread::sleep(Duration::from_millis(20));
        let second = watch.lap();

        assert!(first >= Duration::from_millis(15), "{first:?}");
        assert!(second >= Duration::from_millis(15), "{second:?}");
        assert!(watch.total() >= first + second);
    }

    #[test]
    fn a_phase_document_keeps_its_field_names() {
        let phase = LaunchPhase {
            phase: "open.viewer".to_owned(),
            ms: 42,
            at: "2026-08-18T12:00:00Z".to_owned(),
            outcome: Some("pending".to_owned()),
        };

        assert_eq!(
            serde_json::to_string(&phase).unwrap(),
            "{\"phase\":\"open.viewer\",\"ms\":42,\"at\":\"2026-08-18T12:00:00Z\",\"outcome\":\"pending\"}"
        );
    }
}
