//! Real PTY measurements and connector actions. Build `cargo build -p latchd`
//! first (or set LATCH_E2E_LATCHD_BIN), then run:
//! cargo test -p latch --lib geometry_tests -- --nocapture
//! These deterministic paints test the rendered-screen contract, not a live
//! Claude/Codex release or their application-specific line wrapping.
use super::*;
use latchd::client;
use latchd::protocol::{Request, SnapshotFormat};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const LONG_CHOICE: &str = "Allow this command to read the project configuration files";

struct Pty {
    _dir: tempfile::TempDir,
    home: LatchHome,
    socket: PathBuf,
    child: Child,
}

impl Pty {
    fn spawn(paint: &str, read_choice: bool, launch_size: Option<(u16, u16)>) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("geometry-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = LatchHome::new(dir.path());
        let session = home.session(&SessionId::parse("ses_fixture").unwrap());
        fs::create_dir_all(session.dir()).unwrap();
        let socket = dir.path().join("kernel.sock");
        let binary = std::env::var_os("LATCH_E2E_LATCHD_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("latchd")
            });
        let receive = if read_choice {
            "stty -icanon min 1 time 0; printf 'PAINTED\\r\\n'; answer=$(dd bs=1 count=1 2>/dev/null)"
        } else {
            "printf 'PAINTED\\r\\n'; IFS= read -r answer"
        };
        // Only test-owned literals enter this shell script. Waiting for start
        // makes the child paint at the attached size, not the launch size. The
        // WINCH trap is armed after that, so it reports only resizes caused by
        // what the test does next; the second read shows what input followed
        // the first.
        let script = format!("stty -echo; printf 'READY\\n'; read start; trap 'printf WINCH' WINCH; printf '\\033[2J\\033[H'; printf 'SIZE:'; stty size; printf '%s\\r\\n' '{paint}'; {receive}; printf '\\r\\nRECEIVED:%s\\r\\n' \"$answer\"; IFS= read -r second; printf 'SECOND:%s\\r\\n' \"$second\"; sleep 30");
        let mut command = Command::new(binary);
        command
            .args(["run", "--id", "ses_fixture", "--socket"])
            .arg(&socket)
            .arg("--session-dir")
            .arg(session.dir())
            .args(["--cwd", "/"]);
        if let Some((cols, rows)) = launch_size {
            command.args(["--cols", &cols.to_string(), "--rows", &rows.to_string()]);
        }
        let mut child = command
            .args(["--", "/bin/sh", "-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("build the real daemon with cargo build -p latchd");
        let mut ready = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        let pty = Self {
            _dir: dir,
            home,
            socket,
            child,
        };
        pty.wait_text("READY");
        pty
    }

    fn wait_text(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let screen = client::Client::connect(&self.socket)
                .unwrap()
                .snapshot(SnapshotFormat::Text, 0)
                .unwrap()
                .text
                .unwrap();
            if screen.contains(needle) {
                return screen;
            }
            assert!(Instant::now() < deadline, "missing {needle:?}: {screen:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn start(&self) {
        client::call(
            &self.socket,
            &Request::Submit {
                text: "start".into(),
            },
        )
        .unwrap();
        self.wait_text("PAINTED");
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = client::call(&self.socket, &Request::Kill);
        let _ = self.child.wait();
    }
}

#[test]
fn send_and_resolve_at_desktop_phone_and_never_attached_geometry() {
    for (name, geometry) in [
        ("desktop", Some((160, 48))),
        ("phone", Some((32, 24))),
        ("never attached", None),
    ] {
        for (connector_id, paint, choice, refusal) in [
            ("claude", "❯", None, None),
            ("codex", "›", None, None),
            ("claude", "❯ draft", None, Some("the claude composer is no longer empty")),
            ("codex", "› draft", None, Some("the codex composer is no longer empty")),
            ("claude", "Permission required\n1. Yes\n2. No", Some("Yes"), None),
            ("claude", "Permission required\n1. Allow this command to read the project configuration files\n2. No", Some(LONG_CHOICE),
                if name == "phone" { Some("the requested choice is not identifiable on the current screen") } else { None }),
            ("claude", "Unrelated screen", Some("Yes"), Some("the requested Claude prompt is no longer visible")),
        ] {
            let pty = Pty::spawn(paint, choice.is_some(), None);
            let surface = geometry.map(|(cols, rows)| client::attach(&pty.socket, cols, rows).unwrap());
            pty.start();
            let (cols, rows) = geometry.unwrap_or((80, 24));
            // stty asks the actual child PTY; stat independently checks the kernel.
            pty.wait_text(&format!("SIZE:{rows} {cols}"));
            let before = client::stat(&pty.socket).unwrap();
            assert_eq!((before.cols, before.rows, before.attached, before.pinned), (cols, rows, surface.is_some(), false));
            let mut connector = JsonlConnector::fixture(connector_id, PathBuf::from("unused-source.jsonl"));
            connector.home = pty.home.clone();
            let action = if let Some(choice) = choice {
                connector.pending_request = Some(PendingRequest {
                    id: "request".into(), request_type: RequestType::Permission,
                    prompt: "Permission required".into(), choices: vec![choice.into()],
                });
                ConnectorAction { id: ACTION_RESOLVE_REQUEST.into(), payload: serde_json::json!({"requestId":"request", "choice":choice}) }
            } else {
                ConnectorAction { id: ACTION_SEND_MESSAGE.into(), payload: serde_json::json!({"text":"hello"}) }
            };
            let result = connector.apply(action, Duration::from_secs(5)).unwrap();
            eprintln!("{name} {cols}x{rows} {connector_id} choice={choice:?} paint={paint:?}: {result:?}");
            if let Some(reason) = refusal {
                assert_eq!(result, ApplyResult::Refused { reason: reason.into() });
                // A subsequent control input must be the first input the child
                // receives: refusal must not have typed a partial answer.
                client::call(&pty.socket, &Request::Submit { text: "sentinel".into() }).unwrap();
                pty.wait_text(if choice.is_some() { "RECEIVED:s" } else { "RECEIVED:sentinel" });
            } else {
                assert!(matches!(result, ApplyResult::Accepted { .. }));
                pty.wait_text(if choice.is_some() { "RECEIVED:1" } else { "RECEIVED:hello" });
            }
            let after = client::stat(&pty.socket).unwrap();
            assert_eq!((after.cols, after.rows, after.attached), (cols, rows, surface.is_some()));
        }
    }
}

#[test]
fn geometry_retains_launch_size_and_last_attach_after_detach() {
    // No surface ever attached: both the default and an explicit manifest-like
    // launch size reach the child, with no hidden observer or reset.
    for size in [None, Some((101, 37))] {
        let pty = Pty::spawn("❯", false, size);
        pty.start();
        let (cols, rows) = size.unwrap_or((80, 24));
        pty.wait_text(&format!("SIZE:{rows} {cols}"));
        assert!(!client::stat(&pty.socket).unwrap().attached);
    }
    let pty = Pty::spawn("❯", false, None);
    let surface = client::attach(&pty.socket, 160, 48).unwrap();
    drop(surface);
    let deadline = Instant::now() + Duration::from_secs(5);
    while client::stat(&pty.socket).unwrap().attached {
        assert!(Instant::now() < deadline, "surface did not detach");
        std::thread::sleep(Duration::from_millis(10));
    }
    pty.start();
    pty.wait_text("SIZE:48 160");
    let stat = client::stat(&pty.socket).unwrap();
    assert_eq!((stat.cols, stat.rows, stat.attached), (160, 48, false));
    eprintln!("after wide detach: child PTY and kernel retain 160x48, attached=false");
}

/// What mobile Chat does now that it holds no terminal (coo:1035.kyw6): read
/// the screen and send through `ConversationControl` while a desktop terminal
/// owns the session. The desktop keeps its surface and its grid, the child
/// sees no SIGWINCH, and exactly one prompt arrives.
#[test]
fn chat_observes_and_sends_beside_an_attached_desktop_terminal() {
    let pty = Pty::spawn("❯", false, None);
    let desktop = client::attach(&pty.socket, 160, 48).unwrap();
    pty.start();
    pty.wait_text("SIZE:48 160");

    let mut connector = JsonlConnector::fixture("claude", PathBuf::from("unused-source.jsonl"));
    connector.home = pty.home.clone();
    // Opening, backgrounding, and foregrounding Chat: each round is a fresh
    // control connection reading the screen, the way a resubscribed Hub does.
    for _ in 0..5 {
        connector.control = None;
        let screen = connector.current_screen(Duration::from_secs(5)).unwrap();
        assert!(screen.contains("❯"));
        let stat = client::stat(&pty.socket).unwrap();
        assert_eq!(
            (stat.cols, stat.rows, stat.attached, stat.pinned),
            (160, 48, true, false)
        );
    }

    let result = connector
        .apply(
            ConnectorAction {
                id: ACTION_SEND_MESSAGE.into(),
                payload: serde_json::json!({"text":"hello"}),
            },
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(matches!(result, ApplyResult::Accepted { .. }));
    pty.wait_text("RECEIVED:hello");
    // The next line the child reads is ours, so the send was one prompt.
    client::call(
        &pty.socket,
        &Request::Submit {
            text: "sentinel".into(),
        },
    )
    .unwrap();
    let screen = pty.wait_text("SECOND:sentinel");
    assert!(
        !screen.contains("WINCH"),
        "observation or send resized the PTY: {screen:?}"
    );

    let stat = client::stat(&pty.socket).unwrap();
    assert_eq!(
        (stat.cols, stat.rows, stat.attached, stat.pinned),
        (160, 48, true, false)
    );
    drop(desktop);
}

/// The control for the test above: a human attach at a new size does reach
/// the child as SIGWINCH, so the trap's silence there means something.
#[test]
fn a_terminal_attach_is_what_the_winch_trap_reports() {
    let pty = Pty::spawn("❯", false, None);
    pty.start();
    let _terminal = client::attach(&pty.socket, 100, 30).unwrap();
    pty.wait_text("WINCH");
    let stat = client::stat(&pty.socket).unwrap();
    assert_eq!((stat.cols, stat.rows, stat.attached), (100, 30, true));
}
