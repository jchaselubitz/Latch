//! Opt-in, bounded local ICE traces. Never records application traffic.
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
static LOGGER: TraceLogger = TraceLogger(Mutex::new(None));
static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
struct TraceFile {
    path: PathBuf,
    file: File,
    bytes: u64,
}
struct TraceLogger(Mutex<Option<TraceFile>>);

fn allowed(target: &str) -> bool {
    target.starts_with("webrtc_ice")
        || target.starts_with("turn")
        || target == "latch_transport"
        || target == "latch_remote"
}

fn safe_message(message: &str) -> bool {
    // The pinned ICE version prints the peer's password in start_connectivity_checks.
    // Drop the entire record, never try to partially redact an unknown format.
    !message.contains("remotePwd") && !message.to_ascii_lowercase().contains("password")
}

impl log::Log for TraceLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        allowed(metadata.target())
    }
    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let Ok(mut slot) = self.0.lock() else {
            return;
        };
        let Some(trace) = slot.as_mut() else {
            return;
        };
        // Deleting the opt-in file stops output, even in an already running helper.
        if !trace.path.exists() {
            *slot = None;
            return;
        }
        let message = record.args().to_string();
        if !safe_message(&message) {
            return;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let line = format!(
            "{}.{:03} {:5} {} {}\n",
            now.as_secs(),
            now.subsec_millis(),
            record.level(),
            record.target(),
            message
        );
        if trace.bytes + line.len() as u64 > MAX_BYTES {
            if trace.file.set_len(0).is_err() || trace.file.rewind().is_err() {
                return;
            }
            trace.bytes = 0;
        }
        if trace.file.write_all(line.as_bytes()).is_ok() {
            trace.bytes += line.len() as u64;
        }
    }
    fn flush(&self) {
        if let Ok(mut slot) = self.0.lock() {
            if let Some(trace) = slot.as_mut() {
                let _ = trace.file.flush();
            }
        }
    }
}

/// Enables a trace at a caller-owned local path, or disables it with `None`.
/// The trace is capped at 8 MiB and contains network addresses, never Noise or
/// application records. The caller decides whether to create the opt-in file.
pub fn configure(path: Option<&Path>) -> Result<(), String> {
    INSTALLED
        .get_or_init(|| log::set_logger(&LOGGER).map_err(|e| e.to_string()))
        .clone()?;
    let trace = if let Some(path) = path {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        let bytes = file.metadata().map_err(|e| e.to_string())?.len();
        Some(TraceFile {
            path: path.to_owned(),
            file,
            bytes,
        })
    } else {
        None
    };
    *LOGGER.0.lock().map_err(|e| e.to_string())? = trace;
    log::set_max_level(if path.is_some() {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Off
    });
    log::info!(target: "latch_transport", "ICE diagnostics enabled; build={} lifecycle-v2; local only; bounded to 8 MiB", env!("CARGO_PKG_VERSION"));
    Ok(())
}

/// A stable, non-secret correlation token. Never pass a password or key here.
pub fn fingerprint(value: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(value.as_bytes())[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_and_unrelated_protocols_are_excluded() {
        assert!(!safe_message("Started agent: remotePwd: secret"));
        assert!(!safe_message("password=secret"));
        assert!(safe_message("Nominatable pair found, nominating"));
        assert!(!allowed("webrtc_dtls"));
        assert!(!allowed("latch::noise"));
        assert!(allowed("webrtc_ice::agent::agent_selector"));
    }
    #[test]
    fn trace_is_bounded_and_drops_secret_records() {
        use log::Log;
        let path = std::env::temp_dir().join(format!("latch-trace-test-{}", std::process::id()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .unwrap();
        file.set_len(MAX_BYTES).unwrap();
        let logger = TraceLogger(Mutex::new(Some(TraceFile {
            path: path.clone(),
            file,
            bytes: MAX_BYTES,
        })));
        logger.log(
            &log::Record::builder()
                .target("webrtc_ice")
                .args(format_args!("remotePwd: secret"))
                .build(),
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), MAX_BYTES);
        logger.log(
            &log::Record::builder()
                .target("webrtc_ice")
                .args(format_args!("Nominatable pair found"))
                .build(),
        );
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Nominatable pair found"));
        assert!(!contents.contains("secret"));
        assert!(contents.len() < 1024);
        std::fs::remove_file(path).unwrap();
    }
}
