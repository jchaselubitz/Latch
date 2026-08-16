//! `latch` — persistent terminal sessions.
//!
//! One binary: the CLI, plus hidden `__launch` and `__harness-hook` entry
//! points for session spawn and Claude Code hooks.
//!
//! Bare `latch` creates a persistent shell and attaches to it, because the
//! intended adoption path is pointing a terminal profile's command at this
//! binary. That makes startup cost a product requirement rather than a
//! nice-to-have: it is paid on every new window.
//!
//! Behaviour behind each arm lives in [`latch::cli`].

use std::io::Read;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use latch::cli::attach::{self, AttachOptions, RetryPolicy};
use latch::cli::create::{self, CreateOptions, ManifestOptions};
use latch::cli::json::{CreateReport, CreatedSession};
use latch::cli::manage::{
    self, ConfigRequest, DoctorOptions, InspectOptions, ListOptions, PruneOptions, RemoveRequest,
    RenameRequest, ResizeRequest, StopRequest,
};
use latch::cli::nesting::{self, NestingDecision};
use latch::cli::open::{self, OpenBehavior, OpenRequest};
use latch::cli::remote_access::{self, DevicePermission};
use latch::cli::update::{self, UpdateOptions};
use latch::engine::PROTOCOL_VERSION;
use latch::session::manifest::{DisplayMetadata, TerminalSize};
use latch::session::paths::LatchHome;

fn main() -> Result<()> {
    let cli = Cli::parse();
    dispatch(cli.command)
}

#[derive(Parser)]
#[command(
    name = "latch",
    version,
    about = "Persistent terminal sessions that outlive the window showing them.",
    disable_help_subcommand = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

// The M1 command surface (`IMPLEMENTATION_PLAN.md`, M1 "CLI" items 9–13).
//
// Every read command takes `--json` so that Overlord and other callers never
// have to parse human output.
//
// Kept as a plain comment rather than a doc comment: clap would promote a doc
// comment here into the binary's `--help` summary, replacing the `about` above.
#[derive(Subcommand)]
enum Command {
    /// Create a persistent shell session and attach to it.
    Shell {
        /// Display name; derived from cwd and command when omitted.
        #[arg(long)]
        name: Option<String>,
        /// Human title (e.g. a mission title).
        #[arg(long)]
        title: Option<String>,
    },
    /// Create a session running a specific command, then attach to it.
    Run {
        /// Display name; derived from cwd and command when omitted.
        #[arg(long)]
        name: Option<String>,
        /// Human title (e.g. a mission title).
        #[arg(long)]
        title: Option<String>,
        /// The command and its arguments, after `--`.
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    /// Create a session from a launch manifest supplied on stdin.
    ///
    /// Built at M1 rather than M3 because M3's Overlord execution provider uses
    /// this same path, and because it keeps launch secrets out of argv from the
    /// first day rather than after a migration.
    Create {
        /// Path to the manifest, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        manifest_file: String,
        /// Emit machine-readable JSON and do not attach.
        ///
        /// Overlord's execution provider creates, then opens a viewer separately
        /// (`planning/OVERLORD_INTEGRATION.md`).
        #[arg(long)]
        json: bool,
    },
    /// Open an existing session in a terminal viewer.
    ///
    /// This only opens an attachment. It never starts, stops, or otherwise
    /// changes the session process.
    Open {
        /// Session id or name.
        session: String,
        /// Viewer to open (currently `iterm`).
        #[arg(long, name = "VIEWER")]
        with: String,
        /// Open as a `window` or a `tab`.
        ///
        /// Defaults to `open.behavior` in `~/.latch/config.toml`, then to
        /// `window`. Callers that hold their own preference (Overlord, Latch
        /// Desktop) pass it here instead of relying on the stored default.
        #[arg(long = "as", value_name = "SHAPE")]
        open_as: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach this terminal to an existing session.
    Attach {
        /// Session id or name.
        session: Option<String>,
        /// Reconnect automatically when the transport drops (M2).
        #[arg(long)]
        retry: bool,
        /// Observe output without sending input or changing terminal size.
        #[arg(long)]
        read_only: bool,
    },
    /// List sessions, most recently active first.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Show everything known about one session.
    Inspect {
        /// Session id or name.
        session: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Terminate one session, or every live session with --all --yes.
    Stop {
        /// Session id or name.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        session: Option<String>,
        /// Stop every currently running session. Requires --yes.
        #[arg(long, requires = "yes")]
        all: bool,
        /// Confirm a stop-all request.
        #[arg(long)]
        yes: bool,
        /// Skip the graceful signal and force the stop immediately.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove one session and its retained screen and metadata.
    Remove {
        /// Session id or name.
        session: String,
        /// Stop a live session before removing it.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a session's display name.
    Rename {
        /// Session id or current name.
        session: String,
        /// The new name.
        name: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Set a session's size explicitly, overriding controller-driven resize.
    Resize {
        /// Session id or name.
        session: String,
        /// Columns.
        #[arg(long)]
        cols: u16,
        /// Rows.
        #[arg(long)]
        rows: u16,
        /// Hold this size against controller changes (decision D4).
        #[arg(long)]
        pin: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Reclaim directories for exited and lost sessions.
    ///
    /// Recently exited sessions are kept: they are still attachable read-only,
    /// and reclaiming one is what makes its last screen unavailable.
    Prune {
        /// Show what would be reclaimed without reclaiming it.
        #[arg(long)]
        dry_run: bool,
        /// Reclaim exited sessions regardless of how recently they ended.
        #[arg(long)]
        all: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check the local installation and report anything wrong with it.
    Doctor {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Read or write non-secret preferences in `~/.latch/config.toml`.
    Config {
        /// Preference key.
        key: Option<String>,
        /// New value; omit to read.
        value: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Update the CLI and bundled tmux payload.
    ///
    /// The complete archive is verified before either binary is replaced, and
    /// package-manager-owned copies are refused.
    Update {
        /// Report what is available without installing it.
        #[arg(long)]
        check: bool,
        /// Install the published release even when it is not newer.
        #[arg(long)]
        force: bool,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Report the protocol versions and features this build supports.
    ///
    /// Overlord's execution provider calls this before offering Latch as a
    /// launch target (`OVERLORD_INTEGRATION.md`, discovery).
    Capabilities {
        /// Session id or name for interaction capabilities.
        session: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Serve a loopback HTTP/WebSocket gateway for remote clients.
    ///
    /// The supported remote path is an SSH tunnel to this loopback port. The
    /// gateway speaks plaintext HTTP, so the bearer token is only safe on
    /// loopback (or a tunnel to it). Binding a non-loopback address requires
    /// `--allow-remote`.
    Serve {
        /// Listen address. Loopback by default.
        ///
        /// Remote access should SSH-tunnel to this address. Non-loopback binds
        /// speak plaintext HTTP and need `--allow-remote`.
        #[arg(long, default_value = "127.0.0.1:4610")]
        bind: String,
        /// Allow binding a non-loopback address (plaintext HTTP; token in the clear).
        ///
        /// Prefer an SSH tunnel to the default loopback bind instead.
        #[arg(long)]
        allow_remote: bool,
        /// Bearer token file. Defaults to `$LATCH_HOME/serve.token`.
        #[arg(long, value_name = "PATH")]
        token_file: Option<String>,
        /// Write structured readiness JSON after the loopback listener is bound.
        ///
        /// Intended for a supervising desktop helper. The document never
        /// contains the gateway token.
        #[arg(long, value_name = "PATH")]
        ready_file: Option<String>,
        #[command(subcommand)]
        command: Option<ServeCommand>,
    },
    /// Manage the paired, encrypted remote-access platform.
    ///
    /// This listener is separately authenticated and encrypted, and proxies
    /// only to a supervised ephemeral loopback gateway. It never makes
    /// `latch serve` remotely reachable.
    RemoteAccess {
        #[command(subcommand)]
        command: RemoteAccessCommand,
    },
    /// Stream normalized harness events as newline-delimited JSON.
    Events {
        /// Latch session id/name or Claude Code session id.
        session: String,
        /// Emit the stable HarnessEvent JSON contract.
        #[arg(long)]
        json: bool,
        /// Resume after this many deterministic events.
        #[arg(long, default_value_t = 0)]
        from: usize,
    },
    /// Send capability-gated input to a hosted session.
    Send {
        /// Session id or name.
        session: String,
        /// Read a free-text message from stdin; the value must be `-`.
        #[arg(long, value_name = "PATH")]
        message: Option<String>,
        /// Comma- or space-separated key names such as `C-c,Enter`.
        #[arg(long, value_name = "KEYS")]
        keys: Option<String>,
        /// Resolve one pending request as `<requestId>=<choice>`.
        #[arg(long, value_name = "REQUEST=CHOICE")]
        resolve: Option<String>,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Launch a child from an ephemeral private FIFO. Internal only.
    #[command(hide = true, name = "__launch")]
    Launch {
        /// FIFO carrying the launch manifest.
        #[arg(long, value_name = "PATH")]
        manifest_fifo: String,
    },
    /// Capture one Claude hook record. Internal only.
    #[command(hide = true, name = "__harness-hook")]
    HarnessHook,
}

#[derive(Subcommand)]
enum ServeCommand {
    /// Mint or rotate the bearer token required by every connection.
    Token,
}

#[derive(Subcommand)]
enum RemoteAccessCommand {
    /// Enable the local remote-access service and create the Mac identity.
    Enable,
    /// Report the remote-access lifecycle state, Mac identity, and listener.
    Status {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Disable new remote-access connections.
    Disable,
    /// Create short-lived QR-compatible pairing material.
    Pair {
        #[command(subcommand)]
        command: PairCommand,
    },
    /// List paired devices without revealing their identity keys.
    Devices {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Change a paired device's permission.
    Grant {
        /// Opaque paired-device identifier.
        device_id: String,
        /// observe, interact, or control.
        #[arg(value_parser = parse_remote_permission)]
        permission: DevicePermission,
    },
    /// Immediately revoke a paired device.
    Revoke {
        /// Opaque paired-device identifier.
        device_id: String,
    },
    /// Rotate a phone identity without changing its grants or re-pairing it.
    RotateDeviceKey {
        /// Opaque paired-device identifier.
        device_id: String,
        /// Replacement Noise static public key, encoded as 32-byte hex.
        #[arg(long)]
        public_key: String,
    },
    /// Independently enable or disable encrypted relay fallback.
    Relay {
        /// `enable` or `disable`.
        #[arg(value_parser = ["enable", "disable"])]
        state: String,
    },
    /// Export inspectable privacy-safe diagnostics as JSON.
    Diagnostics,
    /// Show the minimal local security audit trail.
    Audit {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Supervise a private gateway and serve the authenticated LAN transport.
    LanServe {
        /// LAN listener address. This is not the plaintext gateway address.
        #[arg(long, default_value = "0.0.0.0:0")]
        bind: String,
    },
    /// Attempt an outbound-only direct UDP rendezvous probe for a paired
    /// device. This is a headless transport smoke surface, not a phone UI.
    DirectProbe {
        /// Local UDP bind address used for simultaneous NAT probing.
        #[arg(long, default_value = "0.0.0.0:0")]
        bind: String,
        /// 32-byte, one-time rendezvous identifier supplied by the
        /// authenticated control plane as 64 hexadecimal characters.
        #[arg(long)]
        rendezvous_id: String,
        /// Short-lived peer UDP candidate supplied by rendezvous. May be
        /// repeated for multiple candidate paths.
        #[arg(long, required = true)]
        candidate: Vec<String>,
        /// Give up after this many milliseconds.
        #[arg(long, default_value_t = 3_000)]
        timeout_ms: u64,
    },
}

#[derive(Subcommand)]
enum PairCommand {
    /// Create one five-minute pairing record for QR encoding.
    Create {
        /// Emit machine-readable JSON (the default and recommended form).
        #[arg(long, default_value_t = true)]
        json: bool,
    },
    /// Confirm a phone identity after local user approval.
    Confirm {
        /// Pairing ID from the scanned QR record.
        #[arg(long)]
        pairing_id: String,
        /// One-time secret from the scanned QR record.
        #[arg(long)]
        secret: String,
        /// Phone Noise static public key, encoded as 32-byte hex.
        #[arg(long)]
        device_public_key: String,
        /// User-approved phone name.
        #[arg(long)]
        name: String,
        /// Initial permission; defaults to observe plus structured interaction.
        #[arg(long, default_value = "interact", value_parser = parse_remote_permission)]
        permission: DevicePermission,
    },
}

fn parse_remote_permission(value: &str) -> Result<DevicePermission, String> {
    match value {
        "observe" => Ok(DevicePermission::Observe),
        "interact" => Ok(DevicePermission::Interact),
        "control" => Ok(DevicePermission::Control),
        _ => Err("must be observe, interact, or control".to_owned()),
    }
}

fn dispatch(command: Option<Command>) -> Result<()> {
    match command {
        // Bare `latch` is the adoption path: a terminal profile points at this
        // binary, so the no-argument case must be the useful one.
        None
        | Some(Command::Shell {
            name: None,
            title: None,
        }) => create_and_attach(shell_options(DisplayMetadata::default())?),
        Some(Command::Shell { name, title }) => {
            create_and_attach(shell_options(DisplayMetadata {
                name,
                title,
                ..Default::default()
            })?)
        }
        Some(Command::Run { name, title, argv }) => {
            match nesting::nesting_decision(create::enclosing_session_id().as_deref()) {
                NestingDecision::Allow => create_and_attach(run_options(
                    argv,
                    DisplayMetadata {
                        name,
                        title,
                        ..Default::default()
                    },
                )?),
                NestingDecision::AttachToEnclosing { session_id } => bail!(
                    "refusing to create a nested Latch session (already inside {session_id}); \
                 `latch run` cannot nest — exit the enclosing session or attach to it first"
                ),
            }
        }
        Some(Command::Create {
            manifest_file,
            json,
        }) => {
            refuse_nested_create()?;
            let manifest = create::read_manifest_file(&manifest_file)?;
            let outcome = create::create_session(CreateOptions {
                home: LatchHome::from_env()?,
                manifest,
                attach: !json,
            })?;
            if json {
                let meta = latch::session::meta::read(&outcome.paths)?;
                println!(
                    "{}",
                    serde_json::to_string(&CreateReport {
                        protocol_version: PROTOCOL_VERSION,
                        session: CreatedSession {
                            id: outcome.id.to_string(),
                            name: outcome.name,
                            state: "running".to_owned(),
                            created_at: meta.created_at,
                        },
                    })?
                );
                Ok(())
            } else {
                attach_created_session(outcome.id.as_str())
            }
        }
        Some(Command::Open {
            session,
            with,
            open_as,
            json,
        }) => {
            let behavior = open_as.as_deref().map(OpenBehavior::parse).transpose()?;
            let report = open::open(OpenRequest {
                home: LatchHome::from_env()?,
                session,
                viewer: with,
                behavior,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!(
                    "opened {} in {} as a {}",
                    report.id,
                    report.viewer,
                    report.behavior.replace('-', " ")
                );
            }
            Ok(())
        }
        Some(Command::Attach {
            session,
            retry,
            read_only,
        }) => {
            let options = AttachOptions {
                home: LatchHome::from_env()?,
                session,
                read_only,
            };
            // `--retry` changes only how many times the same attach is made.
            if retry {
                attach::attach_with_retry(options, RetryPolicy::default())?;
            } else {
                attach::attach(options)?;
            }
            Ok(())
        }
        Some(Command::List { json }) => {
            let report = manage::list(ListOptions {
                home: LatchHome::from_env()?,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                print_list_human(&report);
            }
            Ok(())
        }
        Some(Command::Inspect { session, json }) => {
            let report = manage::inspect(InspectOptions {
                home: LatchHome::from_env()?,
                session,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                print_inspect_human(&report);
            }
            Ok(())
        }
        Some(Command::Stop {
            session,
            all,
            yes: _,
            force,
            json,
        }) => {
            let home = LatchHome::from_env()?;
            if all {
                let report = manage::stop_all(home, force)?;
                if json {
                    println!("{}", serde_json::to_string(&report)?);
                } else {
                    println!("stopped {} session(s)", report.sessions.len());
                }
                if let Some(session) = report.sessions.iter().find(|session| !session.stopped) {
                    bail!("session {} is still running after stop", session.id);
                }
            } else {
                let report = manage::stop(StopRequest {
                    home,
                    session: session.expect("clap requires a session when --all is absent"),
                    force,
                })?;
                if json {
                    println!("{}", serde_json::to_string(&report)?);
                } else {
                    println!("{} {}", report.id, report.state);
                }
                if !report.stopped {
                    bail!("session {} is still running after stop", report.id);
                }
            }
            Ok(())
        }
        Some(Command::Remove {
            session,
            force,
            json,
        }) => {
            let report = manage::remove(RemoveRequest {
                home: LatchHome::from_env()?,
                session,
                force,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("removed {}", report.id);
            }
            Ok(())
        }
        Some(Command::Rename {
            session,
            name,
            json,
        }) => {
            let report = manage::rename(RenameRequest {
                home: LatchHome::from_env()?,
                session,
                name,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("{} renamed to {}", report.id, report.name);
            }
            Ok(())
        }
        Some(Command::Resize {
            session,
            cols,
            rows,
            pin,
            json,
        }) => {
            let report = manage::resize(ResizeRequest {
                home: LatchHome::from_env()?,
                session,
                cols,
                rows,
                pin,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!(
                    "{} {}x{}{}",
                    report.id,
                    report.cols,
                    report.rows,
                    if report.pinned { " (pinned)" } else { "" }
                );
            }
            Ok(())
        }
        Some(Command::Prune { dry_run, all, json }) => {
            let report = manage::prune(PruneOptions {
                dry_run,
                retain_exited: if all {
                    Duration::ZERO
                } else {
                    manage::DEFAULT_EXITED_RETENTION
                },
                ..PruneOptions::new(LatchHome::from_env()?)
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
                return Ok(());
            }
            if report.reclaimed.is_empty() {
                println!("nothing to prune");
            } else if report.dry_run {
                println!("would reclaim {}", report.reclaimed.join(", "));
            } else {
                println!("reclaimed {}", report.reclaimed.join(", "));
            }
            // Named, not counted: the reason to keep an exited session is that
            // somebody may want to attach to it, and they need its id to do so.
            if !report.retained.is_empty() {
                println!(
                    "kept {} recently exited session(s), still attachable read-only: {}",
                    report.retained.len(),
                    report
                        .retained
                        .iter()
                        .map(|session| session.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Ok(())
        }
        Some(Command::Doctor { json }) => {
            let report = manage::doctor(DoctorOptions {
                home: LatchHome::from_env()?,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                if let Some(version) = &report.tmux_version {
                    println!("session kernel: {version}");
                }
                if report.findings.is_empty() {
                    println!("no problems found");
                }
                for finding in &report.findings {
                    println!("{} {}: {}", finding.severity, finding.code, finding.message);
                }
            }
            Ok(())
        }
        Some(Command::Config { key, value, json }) => {
            let report = manage::config(ConfigRequest {
                home: LatchHome::from_env()?,
                key,
                value,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else if let Some(config) = &report.config {
                println!("{config}");
            } else if let (Some(key), Some(value)) = (&report.key, &report.value) {
                println!("{key} = {value}");
            } else if let Some(key) = &report.key {
                println!("{key} is unset");
            }
            Ok(())
        }
        Some(Command::Update { check, force, json }) => {
            let report = update::update(UpdateOptions {
                check_only: check,
                force,
                ..UpdateOptions::new(env!("CARGO_PKG_VERSION"))
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                print_update_human(&report);
            }
            Ok(())
        }
        Some(Command::Serve {
            bind,
            allow_remote,
            token_file,
            ready_file,
            command,
        }) => {
            let home = LatchHome::from_env()?;
            let token_file = token_file
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| home.serve_token());
            match command {
                Some(ServeCommand::Token) => {
                    home.ensure()?;
                    let token = latch::cli::serve::mint_token(&token_file)?;
                    println!("{token}");
                    Ok(())
                }
                None => latch::cli::serve::serve(latch::cli::serve::ServeOptions {
                    home,
                    bind: bind.parse().context("invalid --bind address")?,
                    token_file,
                    ready_file: ready_file.map(std::path::PathBuf::from),
                    latch_bin: std::env::current_exe().context("cannot locate the latch binary")?,
                    allow_remote,
                }),
            }
        }
        Some(Command::RemoteAccess { command }) => {
            let home = LatchHome::from_env()?;
            match command {
                RemoteAccessCommand::Enable => {
                    remote_access::set_enabled(&home, true)?;
                    println!("remote access enabled");
                    Ok(())
                }
                RemoteAccessCommand::Status { json } => {
                    let status = remote_access::status(&home)?;
                    if json {
                        println!("{}", serde_json::to_string(&status)?);
                    } else {
                        println!(
                            "remote access {}\nrelay {}\ndevice {}\npaired {} ({} revoked)\nlistener {}",
                            if status.enabled { "enabled" } else { "disabled" },
                            if status.relay_enabled { "enabled" } else { "disabled" },
                            status.device_id.as_deref().unwrap_or("none"),
                            status.paired_devices,
                            status.revoked_devices,
                            status.listener_address.as_deref().unwrap_or("stopped"),
                        );
                    }
                    Ok(())
                }
                RemoteAccessCommand::Disable => {
                    remote_access::set_enabled(&home, false)?;
                    println!("remote access disabled");
                    Ok(())
                }
                RemoteAccessCommand::Pair {
                    command: PairCommand::Create { json },
                } => {
                    let material = remote_access::create_pairing(&home)?;
                    if json {
                        println!("{}", serde_json::to_string(&material)?);
                    } else {
                        println!("{}", serde_json::to_string(&material)?);
                    }
                    Ok(())
                }
                RemoteAccessCommand::Pair {
                    command:
                        PairCommand::Confirm {
                            pairing_id,
                            secret,
                            device_public_key,
                            name,
                            permission,
                        },
                } => {
                    let device = remote_access::confirm_pairing(
                        &home,
                        &pairing_id,
                        &secret,
                        &device_public_key,
                        &name,
                        permission,
                    )?;
                    println!("{}", serde_json::to_string(&device)?);
                    Ok(())
                }
                RemoteAccessCommand::Devices { json } => {
                    let devices = remote_access::list_devices(&home)?;
                    if json {
                        println!("{}", serde_json::to_string(&devices)?);
                    } else {
                        for device in devices {
                            println!(
                                "{} {} {:?}{}",
                                device.device_id,
                                device.name,
                                device.permission,
                                if device.revoked { " revoked" } else { "" }
                            );
                        }
                    }
                    Ok(())
                }
                RemoteAccessCommand::Grant {
                    device_id,
                    permission,
                } => {
                    remote_access::grant(&home, &device_id, permission)?;
                    println!("permission updated");
                    Ok(())
                }
                RemoteAccessCommand::Revoke { device_id } => {
                    remote_access::revoke(&home, &device_id)?;
                    println!("device revoked");
                    Ok(())
                }
                RemoteAccessCommand::RotateDeviceKey {
                    device_id,
                    public_key,
                } => {
                    remote_access::rotate_device_key(&home, &device_id, &public_key)?;
                    println!("device key rotated");
                    Ok(())
                }
                RemoteAccessCommand::Relay { state } => {
                    remote_access::set_relay_enabled(&home, state == "enable")?;
                    println!("relay {state}d");
                    Ok(())
                }
                RemoteAccessCommand::Diagnostics => {
                    println!(
                        "{}",
                        serde_json::to_string(&remote_access::diagnostics_export(&home)?)?
                    );
                    Ok(())
                }
                RemoteAccessCommand::Audit { json } => {
                    let events = remote_access::read_audit(&home)?;
                    if json {
                        println!("{}", serde_json::to_string(&events)?);
                    } else {
                        for event in events {
                            println!("{event}");
                        }
                    }
                    Ok(())
                }
                RemoteAccessCommand::LanServe { bind } => remote_access::serve_lan(
                    home,
                    bind.parse().context("invalid --bind address")?,
                    std::env::current_exe().context("cannot locate the latch binary")?,
                ),
                RemoteAccessCommand::DirectProbe {
                    bind,
                    rendezvous_id,
                    candidate,
                    timeout_ms,
                } => {
                    let candidates = candidate
                        .into_iter()
                        .map(|value| {
                            value
                                .parse()
                                .map(remote_access::DirectCandidate::short_lived)
                                .context("invalid --candidate address")
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    let peer = runtime.block_on(remote_access::probe_direct_path(
                        bind.parse().context("invalid --bind address")?,
                        &rendezvous_id,
                        &candidates,
                        Duration::from_millis(timeout_ms),
                    ))?;
                    // Do not emit the peer address: connection diagnostics are
                    // intentionally privacy-safe. The one-bit result is enough
                    // for the headless smoke client.
                    println!("{{\"state\":\"direct\",\"peerReachable\":true}}");
                    let _ = peer;
                    Ok(())
                }
            }
        }
        Some(Command::Capabilities { session, json }) => match session {
            Some(session) => {
                let capabilities =
                    latch::harness::interaction_capabilities(latch::harness::InteractionOptions {
                        home: LatchHome::from_env()?,
                        session,
                    })?;
                if json {
                    println!("{}", serde_json::to_string(&capabilities)?);
                } else {
                    println!(
                        "sendMessage={} sendKeys={} resolve={}",
                        capabilities.send_message, capabilities.send_keys, capabilities.resolve
                    );
                    if !capabilities.can_send.ok {
                        println!(
                            "input unavailable: {}",
                            capabilities
                                .can_send
                                .reason
                                .as_deref()
                                .unwrap_or("unknown reason")
                        );
                    }
                }
                Ok(())
            }
            None => {
                let report = manage::capabilities();
                if json {
                    println!("{}", serde_json::to_string(&report)?);
                } else {
                    println!(
                        "protocol {} product {} create={} localAttach={} cloudAttach={}",
                        report.protocol_version,
                        report.product_version,
                        report.capabilities.create,
                        report.capabilities.local_attach,
                        report.capabilities.cloud_attach
                    );
                }
                Ok(())
            }
        },
        Some(Command::Events {
            session,
            json,
            from,
        }) => {
            if !json {
                bail!("the event stream is machine-readable; pass --json");
            }
            latch::harness::stream(latch::harness::EventsOptions {
                home: LatchHome::from_env()?,
                session,
                from,
                transcript: None,
                poll_interval: latch::harness::DEFAULT_POLL_INTERVAL,
            })
        }
        Some(Command::Send {
            session,
            message,
            keys,
            resolve,
            json,
        }) => {
            let supplied = usize::from(message.is_some())
                + usize::from(keys.is_some())
                + usize::from(resolve.is_some());
            if supplied != 1 {
                bail!("choose exactly one of --message, --keys, or --resolve");
            }
            let action = if let Some(path) = message {
                if path != "-" {
                    bail!("--message accepts only `-`; pipe message text over stdin");
                }
                let mut text = String::new();
                std::io::stdin()
                    .take(1024 * 1024 + 1)
                    .read_to_string(&mut text)?;
                latch::harness::SendAction::Message(text)
            } else if let Some(keys) = keys {
                latch::harness::SendAction::Keys(keys)
            } else {
                let value = resolve.expect("one send operation was supplied");
                let (request_id, choice) = value
                    .split_once('=')
                    .filter(|(request_id, choice)| !request_id.is_empty() && !choice.is_empty())
                    .context("--resolve must be `<requestId>=<choice>`")?;
                latch::harness::SendAction::Resolve {
                    request_id: request_id.to_owned(),
                    choice: choice.to_owned(),
                }
            };
            let report = latch::harness::send(latch::harness::SendOptions {
                home: LatchHome::from_env()?,
                session,
                action,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else if report.resolved {
                println!(
                    "resolved {} for {}",
                    report.request_id.as_deref().unwrap_or("request"),
                    report.session_id
                );
            } else {
                println!("sent {} to {}", report.operation, report.session_id);
            }
            Ok(())
        }
        Some(Command::Launch { manifest_fifo }) => {
            latch::engine::launch_from_fifo(std::path::Path::new(&manifest_fifo))
        }
        Some(Command::HarnessHook) => {
            latch::harness::capture_claude_hook(&LatchHome::from_env()?, std::io::stdin())
        }
    }
}

fn shell_options(display: DisplayMetadata) -> Result<CreateOptions> {
    Ok(CreateOptions {
        home: LatchHome::from_env()?,
        manifest: create::shell_manifest(ManifestOptions {
            cwd: std::env::current_dir().context("cannot determine current directory")?,
            size: terminal_size(),
            display,
        }),
        attach: true,
    })
}

fn run_options(argv: Vec<String>, display: DisplayMetadata) -> Result<CreateOptions> {
    Ok(CreateOptions {
        home: LatchHome::from_env()?,
        manifest: create::run_manifest(
            argv,
            ManifestOptions {
                cwd: std::env::current_dir().context("cannot determine current directory")?,
                size: terminal_size(),
                display,
            },
        ),
        attach: true,
    })
}

fn create_and_attach(options: CreateOptions) -> Result<()> {
    match nesting::nesting_decision(create::enclosing_session_id().as_deref()) {
        NestingDecision::Allow => {
            let outcome = create::create_session(options)?;
            attach_created_session(outcome.id.as_str())
        }
        NestingDecision::AttachToEnclosing { session_id } => {
            if options.manifest.display.name.is_some() || options.manifest.display.title.is_some() {
                bail!(
                    "refusing to create a nested Latch session (already inside {session_id}); \
                     named `latch shell` cannot nest — exit the enclosing session or attach to it first"
                );
            }
            attach_created_session(&session_id)
        }
    }
}

/// `latch create` cannot be satisfied by attaching to the enclosing session —
/// the caller supplied a different launch. Decline rather than nest.
fn refuse_nested_create() -> Result<()> {
    match nesting::nesting_decision(create::enclosing_session_id().as_deref()) {
        NestingDecision::Allow => Ok(()),
        NestingDecision::AttachToEnclosing { session_id } => bail!(
            "refusing to create a nested Latch session (already inside {session_id}); \
             run `latch attach` or exit the enclosing session first"
        ),
    }
}

fn attach_created_session(session: &str) -> Result<()> {
    attach::attach(AttachOptions {
        home: LatchHome::from_env()?,
        session: Some(session.to_owned()),
        read_only: false,
    })?;
    Ok(())
}

fn terminal_size() -> TerminalSize {
    let size = latch::cli::term::terminal_size(libc::STDOUT_FILENO);
    TerminalSize::new(size.cols, size.rows)
}

fn print_list_human(report: &latch::cli::json::ListReport) {
    if report.sessions.is_empty() {
        println!("no sessions");
        return;
    }
    for session in &report.sessions {
        let idle = session
            .idle_ms
            .map(|ms| format!("{}s", ms / 1000))
            .unwrap_or_else(|| "-".to_owned());
        println!(
            "{}\t{}\t{}\tidle {}\t{}",
            session.id, session.name, session.state, idle, session.command_label
        );
    }
}

/// The human form says what happened and, when something is available, how to
/// get it — a check that only prints a version number leaves the reader to
/// guess the command.
fn print_update_human(report: &latch::cli::json::UpdateReport) {
    let latest = report.latest_version.as_deref().unwrap_or("unknown");
    match report.status.as_str() {
        "installed" => {
            println!("updated {} to {latest}", report.current_version);
            if let Some(path) = &report.installed_path {
                println!("{}", path.display());
            }
        }
        "available" => {
            println!(
                "latch {latest} is available (this is {}); run `latch update` to install it",
                report.current_version
            );
            if let Some(url) = &report.release_url {
                println!("{url}");
            }
        }
        _ => println!("latch {} is the latest release", report.current_version),
    }
}

fn print_inspect_human(report: &latch::cli::json::InspectReport) {
    println!("id:\t\t{}", report.id);
    println!("name:\t\t{}", report.name);
    if let Some(title) = &report.title {
        println!("title:\t\t{title}");
    }
    println!("state:\t\t{}", report.state);
    println!("cwd:\t\t{}", report.cwd.display());
    println!("command:\t{}", report.command_label);
    println!("created:\t{}", report.created_at);
    println!(
        "size:\t\t{}x{}",
        report.initial_size.cols, report.initial_size.rows
    );
    if let Some(size) = &report.size {
        println!("current:\t{}x{}", size.cols, size.rows);
    }
    if let Some(exit) = &report.exit {
        println!("exited:\t\t{}", exit.exited_at);
    }
    if let Some(attached) = report.attached {
        println!("attached:\t{attached}");
    }
}
