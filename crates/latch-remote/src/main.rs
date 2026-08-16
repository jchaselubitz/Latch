//! Dedicated process boundary for remote-facing parsers and sockets.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use latch::session::paths::LatchHome;

#[derive(Parser)]
#[command(name = "latch-remote", version, about)]
struct Arguments {
    /// Authenticated listener address; loopback is refused by the helper.
    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,
    /// Main latch executable used only to supervise the private loopback gateway.
    #[arg(long)]
    latch_bin: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let home = LatchHome::from_env()?;
    if !arguments.latch_bin.is_file() {
        anyhow::bail!(
            "latch executable does not exist: {}",
            arguments.latch_bin.display()
        );
    }
    // Referencing the stack here makes the release boundary explicit: the
    // terminal-facing `latch` binary does not depend on latch-transport, while
    // this helper is the sole owner of the internet-facing protocol code.
    let _stack = latch_transport::STACK_NAME;
    latch::cli::remote_access::serve_lan(home, arguments.bind, arguments.latch_bin)
        .context("remote-access helper failed")
}
