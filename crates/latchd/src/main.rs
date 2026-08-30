//! `latchd` — one persistent terminal session.
//!
//! ```text
//! latchd run --id ID --socket PATH [--session-dir DIR] --cwd DIR \
//!            --cols N --rows N [--env K=V]... [--quiet-ms MS] -- PROGRAM [ARGS]...
//! ```
//!
//! `run` detaches from the parent's session, spawns the program, listens on
//! the socket, prints `ready` on stdout, and then closes stdout. A parent that
//! waits for that line knows the socket is live. Errors before readiness go
//! to stderr with a non-zero exit.
//!
//! Small verbs for inspection and scripting are also here (`stat`,
//! `snapshot`, `submit`, `key`, `kill`, `events`); `latch` links the client
//! library directly rather than shelling out to these.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use latchd::client;
use latchd::daemon::{self, Config, DEFAULT_LAUNCH_TIMEOUT_MS};
use latchd::protocol::{Request, SnapshotFormat, MAX_DIMENSION, PROTOCOL_VERSION};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("latchd: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode> {
    let Some(verb) = args.first().map(String::as_str) else {
        bail!("usage: latchd <run|stat|snapshot|submit|key|kill|events|version> ...");
    };
    match verb {
        "run" => run_daemon(&args[1..]),
        "version" | "--version" | "-V" => {
            println!(
                "latchd {} protocol {PROTOCOL_VERSION}",
                env!("CARGO_PKG_VERSION")
            );
            Ok(ExitCode::SUCCESS)
        }
        "stat" => {
            let socket = socket_arg(&args[1..])?;
            let stat = client::stat(&socket)?;
            println!("{}", serde_json::to_string_pretty(&stat)?);
            Ok(ExitCode::SUCCESS)
        }
        "snapshot" => {
            let socket = socket_arg(&args[1..])?;
            let format = match args.get(2).map(String::as_str) {
                None | Some("text") => SnapshotFormat::Text,
                Some("styled") => SnapshotFormat::Styled,
                Some("escape") => SnapshotFormat::Escape,
                Some("json") => SnapshotFormat::Json,
                Some(other) => bail!("unknown snapshot format `{other}`"),
            };
            let reply = client::call(
                &socket,
                &Request::Snapshot {
                    format,
                    scrollback_lines: 0,
                },
            )?;
            if let Some(text) = reply.text {
                print!("{text}");
            } else if let Some(bytes) = reply.bytes {
                use std::io::Write;
                std::io::stdout().write_all(&bytes)?;
            } else if let Some(screen) = reply.screen {
                println!("{}", serde_json::to_string_pretty(&screen)?);
            }
            Ok(ExitCode::SUCCESS)
        }
        "submit" => {
            let socket = socket_arg(&args[1..])?;
            let text = args.get(2).cloned().unwrap_or_else(|| {
                let mut text = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut text);
                text
            });
            client::call(&socket, &Request::Submit { text })?;
            Ok(ExitCode::SUCCESS)
        }
        "key" => {
            let socket = socket_arg(&args[1..])?;
            client::call(
                &socket,
                &Request::Key {
                    keys: args[2..].to_vec(),
                },
            )?;
            Ok(ExitCode::SUCCESS)
        }
        "kill" => {
            let socket = socket_arg(&args[1..])?;
            client::call(&socket, &Request::Kill)?;
            Ok(ExitCode::SUCCESS)
        }
        "events" => {
            let socket = socket_arg(&args[1..])?;
            let mut events = client::Client::connect(&socket)?.subscribe()?;
            while let Some(event) = events.recv()? {
                println!("{}", serde_json::to_string(&event)?);
            }
            Ok(ExitCode::SUCCESS)
        }
        "attach" => {
            let socket = socket_arg(&args[1..])?;
            let reason = client::attach_tty(&socket)?;
            eprintln!("latchd: surface released: {reason:?}");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown verb `{other}`"),
    }
}

fn socket_arg(args: &[String]) -> Result<PathBuf> {
    args.first()
        .map(PathBuf::from)
        .context("a socket path is required")
}

fn run_daemon(args: &[String]) -> Result<ExitCode> {
    let mut id = None;
    let mut socket = None;
    let mut session_dir = None;
    let mut launch_marker = None;
    let mut launch_timeout_ms = DEFAULT_LAUNCH_TIMEOUT_MS;
    let mut cwd = None;
    let mut cols = 80u16;
    let mut rows = 24u16;
    let mut env = Vec::new();
    let mut quiet_ms = 1500u64;
    let mut argv = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| -> Result<String> {
            iter.next()
                .cloned()
                .with_context(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--id" => id = Some(value("--id")?),
            "--socket" => socket = Some(PathBuf::from(value("--socket")?)),
            "--session-dir" => session_dir = Some(PathBuf::from(value("--session-dir")?)),
            "--launch-marker" => launch_marker = Some(PathBuf::from(value("--launch-marker")?)),
            "--launch-timeout-ms" => {
                launch_timeout_ms = value("--launch-timeout-ms")?
                    .parse()
                    .context("--launch-timeout-ms must be a number")?
            }
            "--cwd" => cwd = Some(PathBuf::from(value("--cwd")?)),
            "--cols" => {
                cols = value("--cols")?
                    .parse()
                    .context("--cols must be a number")?
            }
            "--rows" => {
                rows = value("--rows")?
                    .parse()
                    .context("--rows must be a number")?
            }
            "--quiet-ms" => {
                quiet_ms = value("--quiet-ms")?
                    .parse()
                    .context("--quiet-ms must be a number")?
            }
            "--env" => {
                let pair = value("--env")?;
                let (key, val) = pair.split_once('=').context("--env expects KEY=VALUE")?;
                env.push((key.to_owned(), val.to_owned()));
            }
            "--" => {
                argv = iter.cloned().collect();
                break;
            }
            other => bail!("unknown argument `{other}`"),
        }
    }
    if argv.is_empty() {
        bail!("a program to run is required after `--`");
    }
    let id = id.context("--id is required")?;
    latchd::paths::validate_session_id(&id)?;
    for (name, value) in [("--cols", cols), ("--rows", rows)] {
        if !(1..=MAX_DIMENSION).contains(&value) {
            bail!("{name} must be between 1 and {MAX_DIMENSION}");
        }
    }
    let config = Config {
        id,
        socket: socket.context("--socket is required")?,
        session_dir,
        launch_marker,
        launch_timeout_ms,
        argv,
        cwd: cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into())),
        env,
        cols,
        rows,
        quiet_ms,
    };

    // Leave the parent's session so its terminal's hangup never reaches us.
    // SAFETY: setsid has no preconditions; failure (already a leader) is fine.
    unsafe {
        libc::setsid();
    }
    daemon::run(config, || {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        let _ = writeln!(stdout, "ready");
        let _ = stdout.flush();
        // Detach stdio so a parent waiting on our pipe sees EOF and nothing
        // we print later can block on a closed pipe.
        if let Ok(null) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
        {
            use std::os::fd::AsRawFd;
            // SAFETY: redirecting our own standard descriptors.
            unsafe {
                libc::dup2(null.as_raw_fd(), 0);
                libc::dup2(null.as_raw_fd(), 1);
                libc::dup2(null.as_raw_fd(), 2);
            }
        }
    })?;
    Ok(ExitCode::SUCCESS)
}
