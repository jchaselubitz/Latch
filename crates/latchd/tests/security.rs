//! Security regressions for the kernel, against the real `latchd` binary.
//!
//! Each test pins one property from `docs/LATCHD_SECURITY.md`. They are
//! deliberately adversarial: a hostile child, a hostile or careless client,
//! a hostile neighbour on the same host. When one of these fails, read the
//! matching section of that document before changing the assertion.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use latchd::client::{self, Client};
use latchd::daemon::{PARSER_BACKLOG_CAP, SURFACE_QUEUE_CAP};
use latchd::paths::{EXIT_RECORD, KERNEL_RECORD};
use latchd::protocol::{
    self, Request, Response, SnapshotFormat, State, MAX_DIMENSION, MAX_FRAME, PROTOCOL_VERSION,
};
use latchd::pty::EXIT_BAD_CWD;

struct Daemon {
    _dir: tempfile::TempDir,
    session_dir: PathBuf,
    socket: PathBuf,
    child: Child,
}

/// How a daemon under test is launched.
struct Launch<'a> {
    script: &'a str,
    cwd: &'a str,
    /// Shell fragment run before `exec latchd`, for resource limits.
    prelude: &'a str,
}

impl Daemon {
    fn spawn(script: &str) -> Self {
        Self::launch(Launch {
            script,
            cwd: "/",
            prelude: "",
        })
    }

    fn launch(launch: Launch<'_>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_dir = dir.path().join("session");
        std::fs::create_dir(&session_dir).unwrap();
        let socket = dir.path().join(format!("{}.sock", suffix()));
        assert!(socket.as_os_str().len() < 100);
        let mut child = daemon_command(&socket, &session_dir, launch)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn latchd");
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        Self {
            _dir: dir,
            session_dir,
            socket,
            child,
        }
    }

    fn client(&self) -> Client {
        Client::connect(&self.socket).expect("connect")
    }

    fn stat(&self) -> latchd::protocol::Stat {
        client::stat(&self.socket).expect("stat")
    }

    fn text(&self) -> String {
        self.client()
            .snapshot(SnapshotFormat::Text, 0)
            .unwrap()
            .text
            .unwrap()
    }

    fn wait_text(&self, needle: &str) -> String {
        wait_for(&format!("screen to show {needle:?}"), || {
            let text = self.text();
            text.contains(needle).then_some(text)
        })
    }

    fn wait_state(&self, state: State) {
        wait_for(&format!("state {state:?}"), || {
            (self.stat().state == state).then_some(())
        });
    }

    fn child_alive(&self) -> bool {
        let pid = self.stat().child_pid;
        // SAFETY: signal zero only asks whether the pid exists.
        unsafe { libc::kill(pid, 0) == 0 }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = client::call(&self.socket, &Request::Kill);
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn daemon_command(socket: &Path, session_dir: &Path, launch: Launch<'_>) -> Command {
    let latchd = env!("CARGO_BIN_EXE_latchd");
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!("{} exec \"$0\" \"$@\"", launch.prelude))
        .arg(latchd)
        .args(["run", "--id", "ses_security", "--socket"])
        .arg(socket)
        .arg("--session-dir")
        .arg(session_dir)
        .args(["--cwd", launch.cwd, "--cols", "40", "--rows", "10"])
        .args(["--quiet-ms", "200", "--", "/bin/sh", "-c", launch.script]);
    command
}

fn suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn wait_for<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Runs `latchd run` with the given extra arguments and returns its exit
/// status and stderr, for command-line validation.
fn run_latchd_expect_failure(args: &[&str]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("s.sock");
    let output = Command::new(env!("CARGO_BIN_EXE_latchd"))
        .arg("run")
        .args(args)
        .arg("--socket")
        .arg(&socket)
        .args(["--", "/bin/sh", "-c", "exit 0"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "latchd accepted {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------------------
// Filesystem posture: nothing the kernel creates is readable by anyone else.

#[test]
fn socket_and_records_are_owner_only() {
    let daemon = Daemon::spawn("exit 3");
    assert_eq!(mode_of(&daemon.socket), 0o600, "socket mode");
    assert_eq!(
        mode_of(&daemon.session_dir.join(KERNEL_RECORD)),
        0o600,
        "kernel.json mode"
    );
    daemon.wait_state(State::Exited);
    let exit_record = daemon.session_dir.join(EXIT_RECORD);
    wait_for("exit record", || exit_record.exists().then_some(()));
    assert_eq!(mode_of(&exit_record), 0o600, "exit.json mode");
    let stat = daemon.stat();
    assert_eq!(stat.exit.unwrap().status, Some(3));
}

#[test]
fn a_live_socket_is_not_hijacked_by_a_second_daemon() {
    let daemon = Daemon::spawn("sleep 30");
    let first_pid = daemon.stat().daemon_pid;
    let output = Command::new(env!("CARGO_BIN_EXE_latchd"))
        .args(["run", "--id", "ses_intruder", "--socket"])
        .arg(&daemon.socket)
        .args(["--cwd", "/", "--", "/bin/sh", "-c", "sleep 30"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "second daemon should refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already listening"), "{stderr}");
    // The first daemon still owns the path and still answers.
    assert!(daemon.socket.exists());
    assert_eq!(daemon.stat().daemon_pid, first_pid);
    assert!(daemon.child_alive());
}

#[test]
fn command_line_rejects_unsafe_ids_and_dimensions() {
    let stderr = run_latchd_expect_failure(&["--id", "../../escape", "--cwd", "/"]);
    assert!(stderr.contains("session id"), "{stderr}");
    let stderr = run_latchd_expect_failure(&["--id", "ses_ok", "--cwd", "/", "--cols", "0"]);
    assert!(stderr.contains("--cols"), "{stderr}");
    let stderr = run_latchd_expect_failure(&["--id", "ses_ok", "--cwd", "/", "--rows", "65535"]);
    assert!(stderr.contains("--rows"), "{stderr}");
}

#[test]
fn a_cwd_that_cannot_be_entered_fails_closed() {
    let daemon = Daemon::launch(Launch {
        script: "pwd; sleep 30",
        cwd: "/definitely/not/a/directory",
        prelude: "",
    });
    daemon.wait_state(State::Exited);
    let stat = daemon.stat();
    assert_eq!(stat.exit.unwrap().status, Some(EXIT_BAD_CWD));
    let text = daemon.text();
    assert!(text.contains("cannot enter"), "screen:\n{text}");
    assert!(!text.contains('/'), "the program must not have run: {text}");
}

// ---------------------------------------------------------------------------
// Control plane: malformed and oversized requests are contained.

#[test]
fn oversized_and_malformed_frames_close_only_that_connection() {
    let daemon = Daemon::spawn("sleep 30");
    let before = daemon.stat().control_failures;

    // A frame header claiming more than MAX_FRAME bytes.
    let mut stream = UnixStream::connect(&daemon.socket).unwrap();
    stream
        .write_all(&((MAX_FRAME as u32) + 1).to_be_bytes())
        .unwrap();
    stream.write_all(b"{}").unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
    assert!(sink.is_empty(), "no response is sent to an oversized frame");

    // A well-framed request with an unknown op.
    let mut stream = UnixStream::connect(&daemon.socket).unwrap();
    let body = br#"{"op":"format_disk"}"#;
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(body).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut sink = Vec::new();
    let _ = stream.read_to_end(&mut sink);
    assert!(sink.is_empty());

    let stat = daemon.stat();
    assert!(stat.control_failures >= before + 2, "{stat:?}");
    assert_eq!(stat.state, State::Running);
    assert!(daemon.child_alive());
}

#[test]
fn a_protocol_mismatch_never_reaches_the_surface() {
    let daemon = Daemon::spawn("sleep 30");
    let mut stream = UnixStream::connect(&daemon.socket).unwrap();
    protocol::write_frame(
        &mut stream,
        &Request::Attach {
            cols: 40,
            rows: 10,
            protocol: PROTOCOL_VERSION + 1,
        },
    )
    .unwrap();
    let response: Response = protocol::read_frame(&mut stream).unwrap().unwrap();
    assert!(!response.ok);
    assert!(!daemon.stat().attached);
}

#[test]
fn dimensions_are_bounded_on_resize_and_attach() {
    let daemon = Daemon::spawn("sleep 30");
    let error = daemon
        .client()
        .call(&Request::Resize {
            cols: u16::MAX,
            rows: u16::MAX,
            pin: false,
        })
        .unwrap_err();
    assert!(error.to_string().contains("between 1 and"), "{error}");
    assert!(daemon
        .client()
        .call(&Request::Resize {
            cols: 0,
            rows: 10,
            pin: false,
        })
        .is_err());
    assert_eq!((daemon.stat().cols, daemon.stat().rows), (40, 10));

    // An attach that bypasses the client library's clamp is clamped by the
    // daemon.
    let mut stream = UnixStream::connect(&daemon.socket).unwrap();
    protocol::write_frame(
        &mut stream,
        &Request::Attach {
            cols: u16::MAX,
            rows: u16::MAX,
            protocol: PROTOCOL_VERSION,
        },
    )
    .unwrap();
    let response: Response = protocol::read_frame(&mut stream).unwrap().unwrap();
    assert!(response.ok, "{response:?}");
    let stat = daemon.stat();
    assert_eq!((stat.cols, stat.rows), (MAX_DIMENSION, MAX_DIMENSION));
}

#[test]
fn signal_is_refused_once_the_child_has_exited() {
    let daemon = Daemon::spawn("exit 0");
    daemon.wait_state(State::Exited);
    let error = daemon
        .client()
        .call(&Request::Signal {
            signal: libc::SIGKILL,
        })
        .unwrap_err();
    assert!(error.to_string().contains("exited"), "{error}");
    // Negative signal numbers are refused before reaching kill(2).
    let daemon = Daemon::spawn("sleep 30");
    assert!(daemon
        .client()
        .call(&Request::Signal { signal: -1 })
        .is_err());
    assert!(daemon.child_alive());
}

// ---------------------------------------------------------------------------
// Hostile child: output is bounded, garbage is survived, titles are clean.

#[test]
fn child_output_flood_is_bounded_by_backpressure() {
    // 96 MiB of NULs with no surface attached: without a bound the parser
    // queue would absorb all of it.
    let daemon = Daemon::spawn("head -c 100663296 /dev/zero; echo FLOOD_DONE; sleep 30");
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut peak = 0;
    let mut samples = 0;
    loop {
        let stat = daemon.stat();
        peak = peak.max(stat.parser_backlog_bytes);
        samples += 1;
        assert!(
            stat.parser_backlog_bytes <= PARSER_BACKLOG_CAP + 64 * 1024,
            "backlog {} exceeds the cap",
            stat.parser_backlog_bytes
        );
        if daemon.text().contains("FLOOD_DONE") {
            break;
        }
        assert!(Instant::now() < deadline, "flood did not finish");
        thread::sleep(Duration::from_millis(10));
    }
    let stat = daemon.stat();
    assert!(stat.bytes_from_child >= 100_663_296, "{stat:?}");
    assert!(
        stat.parser_backlog_peak_bytes <= PARSER_BACKLOG_CAP + 64 * 1024,
        "peak {} (observed {peak} over {samples} samples)",
        stat.parser_backlog_peak_bytes
    );
    assert!(daemon.child_alive());
}

#[test]
fn a_slow_surface_is_evicted_within_its_bound_and_the_child_continues() {
    let daemon = Daemon::spawn("head -c 33554432 /dev/zero; echo SLOW_DONE; sleep 30");
    // Attach and never read.
    let surface = client::attach(&daemon.socket, 40, 10).unwrap();
    daemon.wait_text("SLOW_DONE");
    let stat = daemon.stat();
    assert!(stat.slow_client_evictions >= 1, "{stat:?}");
    assert!(stat.surface_queue_peak_bytes <= (SURFACE_QUEUE_CAP + 64 * 1024) as u64);
    assert!(!stat.attached);
    assert_eq!(
        client::release_reason(&daemon.socket, surface.id).unwrap(),
        latchd::protocol::ReleaseReason::SlowClient
    );
    assert!(daemon.child_alive());
}

#[test]
fn random_bytes_from_the_child_do_not_take_the_kernel_down() {
    let daemon = Daemon::spawn(
        "head -c 4194304 /dev/urandom; printf '\\033c\\033[?1049l'; echo SURVIVED; sleep 30",
    );
    daemon.wait_text("SURVIVED");
    let stat = daemon.stat();
    assert_eq!(stat.state, State::Running);
    assert_eq!(
        stat.parser_resets, 0,
        "the screen model panicked on random input and was rebuilt; \
         that is a latch-term bug worth a fuzz case even though the session survived"
    );
    // Every rendering still answers.
    let mut client = daemon.client();
    for format in [
        SnapshotFormat::Text,
        SnapshotFormat::Styled,
        SnapshotFormat::Escape,
        SnapshotFormat::Json,
    ] {
        client.snapshot(format, 100).unwrap();
    }
    client.call(&Request::History { max: 1000 }).unwrap();
    assert!(daemon.child_alive());
}

#[test]
fn titles_from_the_child_are_display_text_only() {
    let daemon = Daemon::spawn("printf '\\033]2;evil\\001\\002title\\007'; echo TITLED; sleep 30");
    daemon.wait_text("TITLED");
    let title = wait_for("a title", || daemon.stat().title);
    assert_eq!(title, "eviltitle");
    assert!(title.chars().all(|c| !c.is_control()));
    let screen = daemon
        .client()
        .snapshot(SnapshotFormat::Json, 0)
        .unwrap()
        .screen
        .unwrap();
    assert_eq!(screen["title"], "eviltitle");
}

// ---------------------------------------------------------------------------
// Resource pressure: the daemon degrades, it does not die and orphan a child.

#[test]
fn descriptor_exhaustion_does_not_end_the_session() {
    let mut daemon = Daemon::launch(Launch {
        script: "sleep 60",
        cwd: "/",
        prelude: "ulimit -n 48;",
    });
    let child_pid = daemon.stat().child_pid;

    // Far more connections than the daemon can hold descriptors for. Each is
    // connected (the listener's backlog accepts them) whether or not the
    // daemon has managed to accept it yet.
    let mut held = Vec::new();
    for _ in 0..96 {
        match UnixStream::connect(&daemon.socket) {
            Ok(stream) => held.push(stream),
            Err(_) => break,
        }
    }
    assert!(held.len() > 40, "connected only {}", held.len());
    thread::sleep(Duration::from_millis(500));
    // The daemon must still be alive with its child.
    assert!(daemon.child.try_wait().unwrap().is_none(), "daemon exited");
    drop(held);

    // Once pressure is released it serves again.
    let stat = wait_for("stat after exhaustion", || {
        client::call_with_timeout(&daemon.socket, &Request::Stat, Duration::from_secs(1))
            .ok()
            .and_then(|reply| reply.stat)
    });
    assert_eq!(stat.state, State::Running);
    assert_eq!(stat.child_pid, child_pid);
    assert!(daemon.socket.exists());
}

#[test]
fn many_idle_control_connections_do_not_block_others() {
    let daemon = Daemon::spawn("sleep 30");
    let idle: Vec<UnixStream> = (0..32)
        .map(|_| UnixStream::connect(&daemon.socket).unwrap())
        .collect();
    let stat = client::call_with_timeout(&daemon.socket, &Request::Stat, Duration::from_secs(5))
        .unwrap()
        .stat
        .unwrap();
    assert_eq!(stat.state, State::Running);
    drop(idle);
}
