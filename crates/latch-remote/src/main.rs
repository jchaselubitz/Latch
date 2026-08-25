//! Dedicated process boundary for remote-facing parsers and sockets.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use latch::session::paths::LatchHome;
use latch_remote::ice::IceResponder;
use latch_transport::policy::IceServer;

#[derive(Parser)]
#[command(name = "latch-remote", version, about)]
struct Arguments {
    /// Authenticated listener address; loopback is refused by the helper.
    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,
    /// Main latch executable used only to supervise the private loopback gateway.
    #[arg(long)]
    latch_bin: PathBuf,
    /// STUN URL used for server-reflexive candidate gathering. May be repeated.
    /// Omitting it gathers host candidates only, which is what a LAN or a
    /// tailnet needs; a TURN URL is refused because relay allocation is a
    /// policy decision this flag must not be able to make.
    #[arg(long = "ice-server")]
    ice_servers: Vec<String>,
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
    // The release boundary is not decorative: the terminal-facing `latch`
    // binary does not depend on latch-transport, and this helper is the sole
    // owner of the internet-facing protocol code. `serve_lan` receives the ICE
    // agent as an injected transport rather than linking it.
    let _stack = latch_transport::STACK_NAME;
    let servers = arguments
        .ice_servers
        .into_iter()
        .map(|url| {
            let server = IceServer {
                url,
                username: String::new(),
                credential: String::new(),
            };
            if server.is_turn() {
                anyhow::bail!("--ice-server accepts STUN URLs only: {}", server.url);
            }
            Ok(server)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let responder =
        IceResponder::new(home.clone(), servers).context("cannot create the ICE responder")?;
    latch::cli::remote_access::serve_lan(
        home,
        arguments.bind,
        arguments.latch_bin,
        Some(Arc::new(responder)),
    )
    .context("remote-access helper failed")
}
