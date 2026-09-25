//! Loopback HTTP/WebSocket gateway for remote Latch clients.
//!
//! `latch serve` is a subcommand of the existing binary, not a second product.
//! It speaks the protocol-major-2 contracts over `/v2` and wraps `latch attach`
//! under a per-client PTY for the terminal channel.
//!
//! The gateway speaks plaintext HTTP, so the bearer token is only safe on
//! loopback. A non-loopback bind is refused outright; there is no opt-in.
//! Remote devices reach the gateway through the Remote Link proxy, which
//! authenticates the device and injects the bearer on loopback. An SSH tunnel
//! to the loopback bind is the other supported remote path.

mod attachments;
pub mod attention;
mod auth;
#[allow(dead_code)] // Phase 0 generates the full v2 wire surface before the Hub consumes it.
mod contract;
mod conversation;
mod directory;
mod http;
mod pty;
pub(crate) mod routes;
mod terminal;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context};

use crate::session::paths::LatchHome;

pub use attention::{
    acknowledge as acknowledge_attention, pending_events as pending_attention_events,
};
pub(crate) use auth::load_token;
pub use auth::mint_token;
/// The agent kinds a remote caller may name at creation; the create
/// boundary in `cli::create` launches by this same vocabulary.
pub use contract::SessionAgent;

/// How the gateway should bind and authenticate.
pub struct ServeOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Listen address. Loopback by default.
    pub bind: SocketAddr,
    /// File holding the bearer token.
    pub token_file: PathBuf,
    /// Optional structured readiness document for a supervising helper.
    pub ready_file: Option<PathBuf>,
    /// `latch` executable used to spawn `attach` under a PTY.
    pub latch_bin: PathBuf,
    /// Stop when the supervising helper exits, including after a crash.
    pub exit_with_parent: bool,
}

/// Mints a token if needed, then serves until interrupted.
pub fn serve(options: ServeOptions) -> anyhow::Result<()> {
    refuse_non_loopback(&options)?;
    options.home.ensure()?;
    if options.exit_with_parent {
        install_parent_watchdog()?;
    }
    if !options.token_file.is_file() {
        let token = mint_token(&options.token_file)?;
        eprintln!(
            "minted bearer token at {}\n{token}",
            options.token_file.display()
        );
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("cannot start the serve runtime")?;
    runtime.block_on(http::run(options))
}

/// A graceful HTTP shutdown can wait indefinitely for an existing WebSocket.
/// Parent loss means the dedicated supervisor was killed, so this watchdog
/// terminates the gateway process outright and lets the kernel release the
/// Conversation Hub advisory lock for the replacement owner.
#[cfg(unix)]
fn install_parent_watchdog() -> anyhow::Result<()> {
    let parent = unsafe { libc::getppid() };
    std::thread::Builder::new()
        .name("latch-gateway-parent".into())
        .spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if unsafe { libc::getppid() } != parent {
                std::process::exit(0);
            }
        })
        .context("cannot start gateway parent watchdog")?;
    Ok(())
}

#[cfg(not(unix))]
fn install_parent_watchdog() -> anyhow::Result<()> {
    Ok(())
}

/// `latch serve` only ever binds loopback. The gateway speaks plaintext HTTP
/// and trusts loopback peers with the device grant headers the Remote Link
/// proxy injects, so a non-loopback listener would both leak the bearer and
/// let any network peer present those headers. There is deliberately no
/// opt-in flag.
fn refuse_non_loopback(options: &ServeOptions) -> anyhow::Result<()> {
    if options.bind.ip().is_loopback() {
        return Ok(());
    }
    bail!(
        "refusing to bind {}: latch serve speaks plaintext HTTP and only listens on loopback. \
         Use Remote Link (latch remote-access) or an SSH tunnel to 127.0.0.1 for remote access.",
        options.bind
    );
}
