//! Small helpers shared across the CLI, the gateway, and the observers.

use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn since_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_secs() -> u64 {
    since_epoch().as_secs()
}

/// Nanoseconds since the Unix epoch, for names that must not collide.
pub(crate) fn unix_nanos() -> u128 {
    since_epoch().as_nanos()
}

/// Wraps a value as a single POSIX shell word.
///
/// The escape for an embedded single quote is `'"'"'` — close the quoted run,
/// emit one double-quoted apostrophe, reopen. It has to be exactly that, with
/// no backslashes: `open` quotes the session id, then quotes the whole attach
/// command again, so any error here is applied twice and reaches `latch attach`
/// as extra characters in the session id rather than as a syntax error.
pub(crate) fn shell_quote(value: impl AsRef<OsStr>) -> String {
    use std::os::unix::ffi::OsStrExt;

    let text = String::from_utf8_lossy(value.as_ref().as_bytes());
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}
