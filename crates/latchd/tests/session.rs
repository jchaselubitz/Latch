//! The kernel contract, against the real `latchd` binary.
//!
//! These are the primitives `latch` builds on: create, exclusive attach with
//! one snapshot then raw bytes, steal with a reasoned release, driving the
//! child through control verbs, the last frame surviving exit, slow-client
//! eviction, and events for the chat system.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use latchd::client::{self, Client};
use latchd::paths::{KernelRecord, EXIT_RECORD};
use latchd::protocol::{Event, ReleaseReason, Request, SnapshotFormat, State};

struct Daemon {
    _dir: tempfile::TempDir,
    session_dir: PathBuf,
    socket: PathBuf,
    child: Child,
}

impl Daemon {
    fn spawn(script: &str) -> Self {
        Self::spawn_sized(script, 40, 10)
    }

    fn spawn_sized(script: &str, cols: u16, rows: u16) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        Self::spawn_in(dir, script, cols, rows)
    }

    fn spawn_with_payload(payload: &[u8]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload_path = dir.path().join("payload.bin");
        std::fs::write(&payload_path, payload).expect("write performance payload");
        let quoted = format!(
            "'{}'",
            payload_path.display().to_string().replace('\'', "'\\''")
        );
        let script =
            format!("stty -echo; printf 'READY'; IFS= read -r start; cat {quoted}; sleep 30");
        Self::spawn_in(dir, &script, 80, 24)
    }

    fn spawn_in(dir: tempfile::TempDir, script: &str, cols: u16, rows: u16) -> Self {
        let session_dir = dir.path().join("session");
        std::fs::create_dir(&session_dir).unwrap();
        // Keep the socket inside the harness tempdir. Besides automatic cleanup,
        // this exercises the same private-directory posture as production and
        // works in sandboxes that intentionally forbid binding directly in /tmp.
        let socket = dir.path().join(format!("{}.sock", rand_suffix()));
        assert!(
            socket.as_os_str().len() < 100,
            "test socket path is too long: {}",
            socket.display()
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_latchd"))
            .args(["run", "--id", "ses_test", "--socket"])
            .arg(&socket)
            .arg("--session-dir")
            .arg(&session_dir)
            .args([
                "--cwd",
                "/",
                "--cols",
                &cols.to_string(),
                "--rows",
                &rows.to_string(),
            ])
            .args(["--quiet-ms", "200", "--", "/bin/sh", "-c", script])
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

    fn wait_text(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = self
                .client()
                .snapshot(SnapshotFormat::Text, 0)
                .unwrap()
                .text
                .unwrap();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {needle:?}; screen:\n{text}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_state(&self, state: State) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stat = client::stat(&self.socket).unwrap();
            if stat.state == state {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {state:?}");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = client::call(&self.socket, &Request::Kill);
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

fn rand_suffix() -> String {
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

/// Reads from a surface until `needle` appears or the stream ends.
fn read_until(stream: &mut impl Read, needle: &[u8]) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return seen,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
        }
        if seen.windows(needle.len()).any(|w| w == needle) || Instant::now() > deadline {
            return seen;
        }
    }
}

fn read_to_end_with_timeout(stream: &mut std::os::unix::net::UnixStream) -> Vec<u8> {
    // A peer the daemon already shut down may refuse socket options.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut out = Vec::new();
    let _ = stream.read_to_end(&mut out);
    out
}

#[test]
fn stat_reports_a_running_child_and_the_kernel_record() {
    let daemon = Daemon::spawn("sleep 30");
    let stat = client::stat(&daemon.socket).unwrap();
    assert_eq!(stat.id, "ses_test");
    assert_eq!(stat.state, State::Running);
    assert_eq!((stat.cols, stat.rows), (40, 10));
    assert!(!stat.attached);
    assert!(stat.child_pid > 0);
    let record = KernelRecord::read(&daemon.session_dir).unwrap().unwrap();
    assert_eq!(record.socket, daemon.socket);
    assert_eq!(record.pid, daemon.child.id() as i32);

    let socket_meta = std::fs::metadata(&daemon.socket).unwrap();
    assert_eq!(socket_meta.permissions().mode() & 0o777, 0o600);
    // SAFETY: getuid has no preconditions.
    assert_eq!(socket_meta.uid(), unsafe { libc::getuid() });
    let record_meta = std::fs::metadata(daemon.session_dir.join("kernel.json")).unwrap();
    assert_eq!(record_meta.permissions().mode() & 0o777, 0o600);
}

#[test]
fn attach_paints_one_snapshot_then_raw_bytes() {
    let daemon = Daemon::spawn("printf 'before\\n'; sleep 0.3; printf 'after\\n'; sleep 30");
    daemon.wait_text("before");
    let mut surface = client::attach(&daemon.socket, 40, 10).unwrap();
    let painted = String::from_utf8_lossy(&surface.snapshot).into_owned();
    assert!(painted.contains("before"), "snapshot: {painted:?}");
    assert!(!painted.contains("after"), "snapshot: {painted:?}");
    let live = read_until(&mut surface.stream, b"after");
    let live = String::from_utf8_lossy(&live);
    assert!(live.contains("after"), "live: {live:?}");
    assert!(
        !live.contains("before"),
        "live must not repeat the snapshot: {live:?}"
    );
    assert!(client::stat(&daemon.socket).unwrap().attached);
}

#[test]
fn input_reaches_the_child_and_attach_resizes_the_session() {
    let daemon = Daemon::spawn(
        "stty -echo; read line; printf 'got:%s cols:%s\\n' \"$line\" \"$(tput cols)\"; sleep 30",
    );
    thread::sleep(Duration::from_millis(200));
    let mut surface = client::attach(&daemon.socket, 61, 12).unwrap();
    surface.stream.write_all(b"hello\r").unwrap();
    let out = read_until(&mut surface.stream, b"cols:");
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("got:hello"), "{out:?}");
    let stat = client::stat(&daemon.socket).unwrap();
    assert_eq!((stat.cols, stat.rows), (61, 12));
}

#[test]
fn a_second_attach_steals_and_the_first_learns_why() {
    let daemon = Daemon::spawn("sleep 30");
    let mut first = client::attach(&daemon.socket, 40, 10).unwrap();
    let second = client::attach(&daemon.socket, 40, 10).unwrap();
    let rest = read_to_end_with_timeout(&mut first.stream);
    assert!(rest.is_empty(), "stolen surface received bytes: {rest:?}");
    assert_eq!(
        client::release_reason(&daemon.socket, first.id).unwrap(),
        ReleaseReason::Stolen
    );
    let stat = client::stat(&daemon.socket).unwrap();
    assert!(stat.attached);
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(2);
    while client::stat(&daemon.socket).unwrap().attached {
        assert!(Instant::now() < deadline, "surface never released");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn control_verbs_drive_the_child_without_a_surface() {
    let daemon =
        Daemon::spawn("stty -echo; while read line; do printf 'line=[%s]\\n' \"$line\"; done");
    thread::sleep(Duration::from_millis(200));
    let mut control = daemon.client();
    control
        .call(&Request::Submit {
            text: "one two".into(),
        })
        .unwrap();
    daemon.wait_text("line=[one two]");
    control
        .call(&Request::Key {
            keys: vec!["x".into(), "Enter".into()],
        })
        .unwrap();
    daemon.wait_text("line=[x]");
    control
        .call(&Request::Write {
            bytes: b"raw\r".to_vec(),
        })
        .unwrap();
    daemon.wait_text("line=[raw]");
    let json = control
        .snapshot(SnapshotFormat::Json, 0)
        .unwrap()
        .screen
        .unwrap();
    assert_eq!(json["cols"], 40);
    assert!(json["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line == "line=[raw]"));
}

#[test]
fn paste_is_bracketed_only_when_the_child_asked() {
    let daemon = Daemon::spawn(
        "stty -echo; read a; printf '%s\\n' \"$a\" | cat -v; printf '\\033[?2004h'; \
         read b; printf '%s\\n' \"$b\" | cat -v; sleep 30",
    );
    thread::sleep(Duration::from_millis(300));
    daemon
        .client()
        .call(&Request::Submit { text: "p".into() })
        .unwrap();
    let plain = daemon.wait_text("p");
    assert!(!plain.contains("200~"), "unrequested bracketing: {plain}");
    thread::sleep(Duration::from_millis(300));
    daemon
        .client()
        .call(&Request::Submit { text: "q".into() })
        .unwrap();
    daemon.wait_text("^[[200~q^[[201~");
}

#[test]
fn history_returns_primary_screen_scrollback() {
    let daemon =
        Daemon::spawn("i=0; while [ $i -lt 30 ]; do echo line$i; i=$((i+1)); done; sleep 30");
    daemon.wait_text("line29");
    let reply = daemon.client().call(&Request::History { max: 5 }).unwrap();
    let lines = reply.lines.unwrap();
    assert_eq!(lines.len(), 5);
    assert!(
        lines.iter().all(|line| line.starts_with("line")),
        "{lines:?}"
    );
    let with_history = daemon
        .client()
        .snapshot(SnapshotFormat::Text, 100)
        .unwrap()
        .text
        .unwrap();
    assert!(with_history.contains("line0\n"), "{with_history}");
}

#[test]
fn exit_is_recorded_and_the_last_frame_survives() {
    let daemon = Daemon::spawn("printf 'final frame\\n'; exit 3");
    daemon.wait_state(State::Exited);
    let stat = client::stat(&daemon.socket).unwrap();
    let exit = stat.exit.unwrap();
    assert_eq!(exit.status, Some(3));
    assert_eq!(exit.signal, None);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(daemon.session_dir.join(EXIT_RECORD)).unwrap())
            .unwrap();
    assert_eq!(record["status"], 3);
    assert!(daemon
        .client()
        .snapshot(SnapshotFormat::Text, 0)
        .unwrap()
        .text
        .unwrap()
        .contains("final frame"));
    // Attaching to an exited session paints the frame and releases at once.
    let mut surface = client::attach(&daemon.socket, 40, 10).unwrap();
    assert!(String::from_utf8_lossy(&surface.snapshot).contains("final frame"));
    let rest = read_to_end_with_timeout(&mut surface.stream);
    assert!(rest.is_empty());
    assert_eq!(
        client::release_reason(&daemon.socket, surface.id).unwrap(),
        ReleaseReason::SessionExited
    );
    let err = daemon
        .client()
        .call(&Request::Write { bytes: vec![b'x'] })
        .unwrap_err();
    assert!(err.to_string().contains("exited"), "{err}");
}

#[test]
fn a_surface_holder_is_released_when_the_child_exits() {
    let daemon = Daemon::spawn("sleep 0.3; printf 'bye\\n'; exit 0");
    let mut surface = client::attach(&daemon.socket, 40, 10).unwrap();
    let rest = read_to_end_with_timeout(&mut surface.stream);
    assert!(String::from_utf8_lossy(&rest).contains("bye"), "{rest:?}");
    assert_eq!(
        client::release_reason(&daemon.socket, surface.id).unwrap(),
        ReleaseReason::SessionExited
    );
}

#[test]
fn signal_ends_the_child_with_a_signal_record() {
    let daemon = Daemon::spawn("trap '' INT; sleep 30");
    thread::sleep(Duration::from_millis(200));
    daemon
        .client()
        .call(&Request::Signal {
            signal: libc::SIGTERM,
        })
        .unwrap();
    daemon.wait_state(State::Exited);
    let exit = client::stat(&daemon.socket).unwrap().exit.unwrap();
    assert_eq!(exit.signal, Some(libc::SIGTERM));
}

#[test]
fn a_slow_surface_is_evicted_and_the_child_keeps_running() {
    // ~16 MiB of output against a surface that never reads.
    let daemon =
        Daemon::spawn("head -c 16777216 /dev/zero | tr '\\0' 'x'; printf 'DONE\\n'; sleep 30");
    let surface = client::attach(&daemon.socket, 40, 10).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match client::release_reason(&daemon.socket, surface.id) {
            Ok(reason) => {
                assert_eq!(reason, ReleaseReason::SlowClient);
                break;
            }
            Err(_) => {
                assert!(Instant::now() < deadline, "surface was never evicted");
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    daemon.wait_text("DONE");
    assert_eq!(client::stat(&daemon.socket).unwrap().state, State::Running);
    drop(surface);
}

#[test]
fn live_surface_is_byte_exact_at_the_phase_a_throughput_gate() {
    const FRAME: &[u8] = b"\x1b[31mX\x1b[0m";
    const FRAMES: usize = 200_000;
    const MIN_FRAMES_PER_SECOND: f64 = 70_000.0;

    let payload = FRAME.repeat(FRAMES);
    let daemon = Daemon::spawn_with_payload(&payload);
    daemon.wait_text("READY");
    let mut surface = client::attach(&daemon.socket, 80, 24).unwrap();
    surface
        .stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();

    let started = Instant::now();
    surface.stream.write_all(b"go\r").unwrap();
    let mut received = vec![0; payload.len()];
    surface.stream.read_exact(&mut received).unwrap();
    let elapsed = started.elapsed();

    assert_eq!(
        received, payload,
        "the raw surface changed or repeated bytes"
    );
    let amplification = received.len() as f64 / payload.len() as f64;
    assert_eq!(amplification, 1.0);
    let frames_per_second = FRAMES as f64 / elapsed.as_secs_f64();
    eprintln!(
        "measure: frames_per_second={frames_per_second:.0} post_boundary_amplification={amplification:.4} bytes={} elapsed_ms={}",
        received.len(),
        elapsed.as_millis()
    );
    assert!(
        frames_per_second >= MIN_FRAMES_PER_SECOND,
        "raw surface delivered only {frames_per_second:.0} frames/s; Phase A requires at least {MIN_FRAMES_PER_SECOND:.0}"
    );
    assert_eq!(client::stat(&daemon.socket).unwrap().state, State::Running);
}

#[test]
fn events_announce_surfaces_quiet_and_exit() {
    let daemon =
        Daemon::spawn("printf 'hi\\n'; sleep 0.6; printf '\\033]0;titled\\007'; sleep 0.5; exit 0");
    let events = daemon.client().subscribe().unwrap();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut events = events;
        while let Ok(Some(event)) = events.recv() {
            if sender.send(event).is_err() {
                break;
            }
        }
    });
    let surface = client::attach(&daemon.socket, 40, 10).unwrap();
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        if let Ok(event) = receiver.recv_timeout(Duration::from_millis(100)) {
            let done = matches!(event, Event::ChildExited { .. });
            seen.push(event);
            if done {
                break;
            }
        }
    }
    drop(surface);
    let has = |pred: &dyn Fn(&Event) -> bool| seen.iter().any(pred);
    assert!(
        has(&|e| matches!(e, Event::SurfaceAttached { .. })),
        "{seen:?}"
    );
    assert!(has(&|e| matches!(e, Event::OutputQuiet { .. })), "{seen:?}");
    assert!(
        has(&|e| matches!(e, Event::TitleChanged { title: Some(t) } if t == "titled")),
        "{seen:?}"
    );
    assert!(
        has(&|e| matches!(e, Event::ChildExited { exit } if exit.status == Some(0))),
        "{seen:?}"
    );
    assert!(
        has(&|e| matches!(
            e,
            Event::SurfaceDetached {
                reason: ReleaseReason::SessionExited,
                ..
            }
        )),
        "{seen:?}"
    );
}

#[test]
fn await_surface_returns_when_a_viewer_arrives() {
    let daemon = Daemon::spawn("sleep 30");
    let socket = daemon.socket.clone();
    let waiter = thread::spawn(move || {
        client::call(&socket, &Request::AwaitSurface { timeout_ms: 5000 })
            .unwrap()
            .attached
            .unwrap()
    });
    thread::sleep(Duration::from_millis(200));
    assert!(!waiter.is_finished());
    let _surface = client::attach(&daemon.socket, 40, 10).unwrap();
    assert!(waiter.join().unwrap());
    // Already-attached history answers immediately.
    assert!(
        client::call(&daemon.socket, &Request::AwaitSurface { timeout_ms: 10 })
            .unwrap()
            .attached
            .unwrap()
    );
    let empty = Daemon::spawn("sleep 30");
    assert!(
        !client::call(&empty.socket, &Request::AwaitSurface { timeout_ms: 50 })
            .unwrap()
            .attached
            .unwrap()
    );
}

#[test]
fn kill_ends_the_daemon_and_removes_the_socket() {
    let mut daemon = Daemon::spawn("sleep 30");
    client::call(&daemon.socket, &Request::Kill).unwrap();
    let status = daemon.child.wait().unwrap();
    assert!(status.success());
    assert!(!daemon.socket.exists());
    assert!(client::stat(&daemon.socket).is_err());
}
