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
    /// Main latch executable used only to supervise the private loopback gateway.
    #[arg(long)]
    latch_bin: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    let home = LatchHome::from_env()?;
    if !arguments.link_serve {
        anyhow::bail!("latch-remote only supports --link-serve Remote Link v1");
    }
    let mut document = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut document)
        .context("cannot read Remote Link host configuration from stdin")?;
    let config =
        serde_json::from_str(&document).context("invalid Remote Link host configuration")?;
    latch_remote::link::serve_remote_link(home, arguments.latch_bin, config)
        .context("Remote Link helper failed")
}
