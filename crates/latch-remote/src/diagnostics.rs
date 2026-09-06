//! Opt-in ICE diagnostics for the helper.
//!
//! The helper normally logs nothing: a transport failure is counted in the
//! audit trail and that is all. When a failure has to be understood rather
//! than counted, the person creates `ice-debug.log` in the remote-access
//! directory and restarts remote access; the helper then appends the ICE
//! stack's own trace to it — every check sent and received, every pair's
//! state — alongside a content-free summary of each gather and answer. The
//! file is opened only if it already exists, so nothing is written unless
//! it was asked for, and it is never uploaded: it carries addresses and
//! ports, which the audit trail deliberately does not.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// The marker and log file, relative to the remote-access directory.
pub const ICE_LOG_FILE: &str = "ice-debug.log";

struct FileLogger {
    file: Mutex<std::fs::File>,
}

impl log::Log for FileLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let target = metadata.target();
        let ceiling = if target.starts_with("webrtc_ice") || target.starts_with("turn") {
            log::Level::Trace
        } else if target.starts_with("latch_transport") || target.starts_with("latch_remote") {
            log::Level::Debug
        } else {
            log::Level::Info
        };
        metadata.level() <= ceiling
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(
                file,
                "{}.{:03} {:5} {} {}",
                now.as_secs(),
                now.subsec_millis(),
                record.level(),
                record.target(),
                record.args()
            );
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            let _ = file.flush();
        }
    }
}

/// Installs the file logger when the marker file exists.
///
/// Returns whether diagnostics are on. A missing file is the ordinary case
/// and not an error; a file that exists but cannot be opened is.
pub fn install_if_requested(remote_access_dir: &Path) -> anyhow::Result<bool> {
    let path = remote_access_dir.join(ICE_LOG_FILE);
    if !path.exists() {
        return Ok(false);
    }
    let file = OpenOptions::new().append(true).mode(0o600).open(&path)?;
    log::set_boxed_logger(Box::new(FileLogger {
        file: Mutex::new(file),
    }))?;
    log::set_max_level(log::LevelFilter::Trace);
    log::info!(
        target: "latch_remote",
        "ICE diagnostics on: this file carries addresses and ports and is never uploaded; delete it to stop"
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stack_is_traced_and_everything_else_is_kept_quiet() {
        let logger = FileLogger {
            file: Mutex::new(tempfile::tempfile().unwrap()),
        };
        let level = |target: &str, level: log::Level| {
            log::Log::enabled(
                &logger,
                &log::Metadata::builder().target(target).level(level).build(),
            )
        };
        assert!(level(
            "webrtc_ice::agent::agent_internal",
            log::Level::Trace
        ));
        assert!(level("turn::client", log::Level::Trace));
        assert!(level("latch_transport", log::Level::Debug));
        assert!(!level("latch_transport", log::Level::Trace));
        assert!(level("mdns_sd", log::Level::Info));
        assert!(!level("mdns_sd", log::Level::Debug));
    }
}
