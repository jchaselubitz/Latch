//! Loopback HTTP/WebSocket gateway for remote Latch clients.
//!
//! `latch serve` is a subcommand of the existing binary, not a second product.
//! It speaks the public CLI JSON contracts over `/v1` and wraps `latch attach`
//! under a per-client PTY for the terminal channel.
//!
//! The supported remote path is an SSH tunnel to the loopback bind. The gateway
//! speaks plaintext HTTP, so the bearer token is only safe on loopback (or a
//! tunnel to it). Binding a non-loopback address requires `--allow-remote`.

mod auth;
mod contract;
mod events;
mod http;
mod pty;
mod terminal;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{bail, Context};

use crate::session::paths::LatchHome;

pub(crate) use auth::load_token;
pub use auth::mint_token;

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
    /// Permit a non-loopback bind (plaintext HTTP; token in the clear).
    pub allow_remote: bool,
}

/// Mints a token if needed, then serves until interrupted.
pub fn serve(options: ServeOptions) -> anyhow::Result<()> {
    refuse_non_loopback(&options)?;
    options.home.ensure()?;
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

fn refuse_non_loopback(options: &ServeOptions) -> anyhow::Result<()> {
    if options.bind.ip().is_loopback() {
        return Ok(());
    }
    if !options.allow_remote {
        bail!(
            "refusing to bind {}: latch serve speaks plaintext HTTP and the bearer token travels in the clear. \
             The supported remote path is an SSH tunnel to 127.0.0.1. Pass --allow-remote to opt in.",
            options.bind
        );
    }
    eprintln!(
        "warning: {} is not loopback; latch serve speaks plaintext HTTP. Prefer an SSH tunnel to 127.0.0.1.",
        options.bind
    );
    Ok(())
}
