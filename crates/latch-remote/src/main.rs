//! Dedicated process boundary for remote-facing parsers and sockets.

use std::io::BufRead;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use latch::session::paths::LatchHome;

#[derive(Parser)]
#[command(name = "latch-remote", version, about)]
struct Arguments {
    /// Run the Remote Link v1 WSS host and read its admission JSON from stdin.
    #[arg(long)]
    link_serve: bool,
    /// Own the one shared loopback Conversation Hub gateway.
    #[arg(long)]
    gateway_serve: bool,
    /// Main latch executable used only by the shared gateway owner.
    #[arg(long)]
    latch_bin: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let home = LatchHome::from_env()?;
    if arguments.gateway_serve {
        if arguments.link_serve {
            anyhow::bail!("choose exactly one latch-remote serve mode");
        }
        let latch_bin = arguments
            .latch_bin
            .context("--gateway-serve requires --latch-bin")?;
        return latch_remote::link::serve_shared_gateway(home, latch_bin)
            .context("shared Remote Link gateway failed");
    }
    if !arguments.link_serve {
        anyhow::bail!("latch-remote requires --link-serve or --gateway-serve");
    }
    let mut document = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut document)
        .context("cannot read Remote Link host configuration from stdin")?;
    let config =
        serde_json::from_str(&document).context("invalid Remote Link host configuration")?;
    latch_remote::link::serve_remote_link(home, config).context("Remote Link helper failed")
}
