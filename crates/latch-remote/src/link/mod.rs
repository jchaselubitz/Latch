//! Host ownership for Remote Link v1.
//!
//! - [`config`]: the versioned host admission read from stdin.
//! - [`session`]: the paired-session loop that owns the current link.
//! - [`acceptors`]: the LAN and WSS carrier acceptors and LAN advertisement.
//! - [`enrollment`]: the provisional, QR-bound enrollment flow.
//! - [`ipc`]: Desktop-to-helper commands and helper-to-Desktop JSON lines.

use std::path::PathBuf;

use latch::cli::remote_access::SharedGatewayOwner;
use latch::session::paths::LatchHome;
use latch_transport::link::LinkPurpose;

mod acceptors;
mod config;
mod enrollment;
mod ipc;
mod session;

pub use acceptors::{run_lan_acceptor, LanLink, LAN_AUTHENTICATION_LIMIT, LAN_HANDSHAKE_PERMITS};
pub use config::RemoteLinkHostConfig;

use enrollment::run_enrollment;
use ipc::{emit_json, ipc_reader, shutdown_signal, AUDIT_HOME};
use session::run_session;

/// Runs one host pair and owns its WSS/LAN carriers, authenticated link, and
/// stream tasks. The loopback gateway has an independent shared owner, so
/// returning here never terminates another device or the Conversation Hub.
pub fn serve_remote_link(home: LatchHome, config: RemoteLinkHostConfig) -> anyhow::Result<()> {
    config.validate()?;
    let _ = AUDIT_HOME.set(home.clone());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    match config.purpose {
        LinkPurpose::Session => runtime.block_on(run_session(home, config)),
        LinkPurpose::Enrollment => runtime.block_on(run_enrollment(home, config)),
    }
}

/// Owns the sole loopback Conversation Hub gateway independently of every
/// device link. Desktop learns only that it is ready; the address and bearer
/// remain in the owner-only runtime directory and are consumed by Rust.
pub fn serve_shared_gateway(home: LatchHome, latch_bin: PathBuf) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let mut owner = SharedGatewayOwner::start(&home, &latch_bin).await?;
        emit_json(&serde_json::json!({
            "type": "gateway_ready", "version": 1,
        }))?;
        let mut ipc = ipc_reader("latch-remote-gateway-ipc")?;
        let result = tokio::select! {
            result = owner.wait() => result,
            signal = shutdown_signal() => signal,
            _ = ipc.recv() => Ok(()),
        };
        owner.close().await;
        result
    })
}
