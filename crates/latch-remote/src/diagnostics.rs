//! Opt-in helper trace using the shared, credential-filtered diagnostic writer.
use std::path::Path;
/// Opt-in marker and output filename.
pub const ICE_LOG_FILE: &str = "ice-debug.log";
/// Installs the filtered shared trace only when the local marker exists.
pub fn install_if_requested(remote_access_dir: &Path) -> anyhow::Result<bool> {
    let path = remote_access_dir.join(ICE_LOG_FILE);
    if !path.exists() {
        return Ok(false);
    }
    latch_transport::diagnostics::configure(Some(&path)).map_err(anyhow::Error::msg)?;
    Ok(true)
}
