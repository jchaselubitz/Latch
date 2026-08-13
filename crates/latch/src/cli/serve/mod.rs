//! Loopback HTTP/WebSocket gateway for remote Latch clients.
//!
//! `latch serve` is a subcommand of the existing binary, not a second product.
//! It speaks the public CLI JSON contracts over `/v1` and wraps `latch attach`
//! under a per-client PTY for the terminal channel.

mod auth;
mod http;
mod pty;
mod terminal;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;

use crate::session::paths::LatchHome;

pub use auth::mint_token;

/// How the gateway should bind and authenticate.
pub struct ServeOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Listen address. Loopback by default.
    pub bind: SocketAddr,
    /// File holding the bearer token.
    pub token_file: PathBuf,
    /// `latch` executable used to spawn `attach` under a PTY.
    pub latch_bin: PathBuf,
}

/// Mints a token if needed, then serves until interrupted.
pub fn serve(options: ServeOptions) -> anyhow::Result<()> {
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
