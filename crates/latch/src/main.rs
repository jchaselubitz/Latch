//! `latch` — persistent terminal sessions.
//!
//! One binary, two modes. Invoked normally it is the CLI; invoked with the
//! hidden `worker` subcommand it is the per-session PTY owner. Decision D1.
//!
//! Bare `latch` creates a persistent shell and attaches to it, because the
//! intended adoption path is pointing a terminal profile's command at this
//! binary. That makes startup cost a product requirement rather than a
//! nice-to-have: it is paid on every new window.
//!
//! Behaviour behind each arm lives in [`latch::cli`]. Until a milestone fills
//! an arm in, the command exists, documents itself, and fails loudly — which is
//! more useful than a missing subcommand, because `latch --help` already shows
//! the surface the milestones are working toward.

use std::time::Duration;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use latch::cli::attach::{self, AttachOptions, RetryPolicy};
use latch::cli::create::{self, CreateOptions, ManifestOptions};
use latch::cli::json::{CreateReport, CreatedSession};
use latch::cli::manage::{
    self, ConfigRequest, DoctorOptions, InspectOptions, ListOptions, PruneOptions, RemoveRequest,
    RenameRequest, ResizeRequest, StopRequest,
};
use latch::cli::nesting::{self, NestingDecision};
use latch::cli::open::{self, OpenRequest};
use latch::worker::manifest::{DisplayMetadata, TerminalSize};
use latch::worker::paths::LatchHome;
use latch_protocol::PROTOCOL_VERSION;

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
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Attach this terminal to an existing session.
    Attach {
        /// Session id or name.
        session: Option<String>,
        /// Attach without taking input control.
        #[arg(long)]
        watch: bool,
        /// Take input control even if another attachment holds it.
        #[arg(long)]
        steal: bool,
        /// Reconnect automatically when the transport drops (M2).
        #[arg(long)]
        retry: bool,
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
    /// Terminate a session's process group.
    Stop {
        /// Session id or name.
        session: String,
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
    /// Report the protocol versions and features this build supports.
    ///
    /// Overlord's execution provider calls this before offering Latch as a
    /// launch target (`OVERLORD_INTEGRATION.md`, discovery).
    Capabilities {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run as the session worker. Not intended for direct use.
    #[command(hide = true)]
    Worker {
        /// The session directory this worker owns.
        #[arg(long, value_name = "PATH")]
        session_dir: String,
    },
}

fn dispatch(command: Option<Command>) -> Result<()> {
    match command {
        // Bare `latch` is the adoption path: a terminal profile points at this
        // binary, so the no-argument case must be the useful one.
        None
        | Some(Command::Shell {
            name: None,
            title: None,
        }) => create_and_attach(shell_options(DisplayMetadata::default())),
        Some(Command::Shell { name, title }) => create_and_attach(shell_options(DisplayMetadata {
            name,
            title,
            ..Default::default()
        })),
        Some(Command::Run { name, title, argv }) => {
            match nesting::nesting_decision(create::enclosing_session_id().as_deref(), true) {
                NestingDecision::Allow => create_and_attach(run_options(
                    argv,
                    DisplayMetadata {
                        name,
                        title,
                        ..Default::default()
                    },
                )),
                NestingDecision::AttachToEnclosing { session_id } => bail!(
                    "refusing to create a nested Latch session (already inside {session_id}); \
                 `latch run` cannot nest — exit the enclosing session or attach to it first"
                ),
                NestingDecision::Decline { reason } => bail!("{reason}"),
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
                let meta = latch::worker::meta::read(&outcome.paths)?;
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
            json,
        }) => {
            let report = open::open(OpenRequest {
                home: LatchHome::from_env()?,
                session,
                viewer: with,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("opened {} in {}", report.id, report.viewer);
            }
            Ok(())
        }
        Some(Command::Attach {
            session,
            watch,
            steal,
            retry,
        }) => {
            let options = AttachOptions {
                home: LatchHome::from_env()?,
                session,
                watch,
                steal,
            };
            // `--retry` changes only how many times the same attach is made, and
            // never what it asks for: a watcher stays a watcher and an attach
            // that was not told to steal never starts stealing.
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
            force,
            json,
        }) => {
            let report = manage::stop(StopRequest {
                home: LatchHome::from_env()?,
                session,
                force,
            })?;
            if json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!("{} {}", report.id, report.state);
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
            } else if report.findings.is_empty() {
                println!("no problems found");
            } else {
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
        Some(Command::Capabilities { json }) => {
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
        // Worker mode is not a command a person runs, so it does not wait for
        // the rest of the CLI to be finished. It reads its launch manifest from
        // stdin, detaches itself, and exits with the child's status so a caller
        // that only looks at the process sees what it would have seen running
        // the command directly.
        Some(Command::Worker { session_dir }) => {
            let exit = latch::worker::run_from_stdin(std::path::Path::new(&session_dir))?;
            std::process::exit(exit.status_code());
        }
    }
}

fn shell_options(display: DisplayMetadata) -> CreateOptions {
    CreateOptions {
        home: LatchHome::from_env().expect("cannot locate the Latch home"),
        manifest: create::shell_manifest(ManifestOptions {
            cwd: std::env::current_dir().expect("cannot determine current directory"),
            size: terminal_size(),
            display,
        }),
        attach: true,
    }
}

fn run_options(argv: Vec<String>, display: DisplayMetadata) -> CreateOptions {
    CreateOptions {
        home: LatchHome::from_env().expect("cannot locate the Latch home"),
        manifest: create::run_manifest(
            argv,
            ManifestOptions {
                cwd: std::env::current_dir().expect("cannot determine current directory"),
                size: terminal_size(),
                display,
            },
        ),
        attach: true,
    }
}

fn create_and_attach(options: CreateOptions) -> Result<()> {
    match nesting::nesting_decision(create::enclosing_session_id().as_deref(), true) {
        NestingDecision::Allow => {
            let outcome = create::create_session(options)?;
            attach_created_session(outcome.id.as_str())
        }
        NestingDecision::AttachToEnclosing { session_id } => attach_created_session(&session_id),
        NestingDecision::Decline { reason } => bail!("{reason}"),
    }
}

/// `latch create` cannot be satisfied by attaching to the enclosing session —
/// the caller supplied a different launch. Decline rather than nest.
fn refuse_nested_create() -> Result<()> {
    match nesting::nesting_decision(create::enclosing_session_id().as_deref(), true) {
        NestingDecision::Allow => Ok(()),
        NestingDecision::AttachToEnclosing { session_id } => bail!(
            "refusing to create a nested Latch session (already inside {session_id}); \
             run `latch attach` or exit the enclosing session first"
        ),
        NestingDecision::Decline { reason } => bail!("{reason}"),
    }
}

fn attach_created_session(session: &str) -> Result<()> {
    attach::attach(AttachOptions {
        home: LatchHome::from_env()?,
        session: Some(session.to_owned()),
        watch: false,
        steal: false,
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
}
