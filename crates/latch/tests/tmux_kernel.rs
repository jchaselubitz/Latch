use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Harness {
    _temp: tempfile::TempDir,
    home: PathBuf,
    tmux: PathBuf,
    latch: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        let tmux = temp.path().join("tmux-3.7b");
        fs::write(&tmux, FAKE_TMUX).expect("write fake tmux");
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).expect("chmod fake tmux");
        Self {
            _temp: temp,
            home,
            tmux,
            latch: PathBuf::from(env!("CARGO_BIN_EXE_latch")),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.latch);
        command
            .env("LATCH_HOME", &self.home)
            .env("LATCH_TMUX_BIN", &self.tmux)
            .env("LATCH_INHERITED", "fresh-client-environment")
            .env_remove("LATCH_SESSION_ID");
        command
    }

    fn create(&self, shell: &str) -> Value {
        let manifest = json!({
            "format_version": 1,
            "launch": {
                "argv": ["/bin/sh", "-c", shell],
                "cwd": self._temp.path(),
                "env": {"LATCH_TEST_SECRET": "must-not-reach-disk"},
                "inherit_env": true,
                "size": {"cols": 100, "rows": 30},
                "term": "host-value-must-not-win"
            },
            "display": {
                "name": "agent",
                "title": "Kernel acceptance",
                "command_label": "redacted",
                "source": {"kind": "test", "external_run_id": "run-1"}
            }
        });
        let mut child = self
            .command()
            .args(["create", "--manifest-file", "-", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn latch create");
        serde_json::to_writer(child.stdin.take().unwrap(), &manifest).expect("write manifest");
        let output = child.wait_with_output().expect("wait for create");
        assert_success(&output);
        serde_json::from_slice(&output.stdout).expect("create JSON")
    }

    fn json(&self, arguments: &[&str]) -> Value {
        let output = self.command().args(arguments).output().expect("run latch");
        assert_success(&output);
        serde_json::from_slice(&output.stdout).expect("command JSON")
    }

    fn set_screen(&self, id: &str, screen: &str) {
        let path = self.home.join("server.fake.json");
        let mut state: Value =
            serde_json::from_slice(&fs::read(&path).expect("read fake tmux state"))
                .expect("parse fake tmux state");
        state[id]["screen"] = Value::String(screen.to_owned());
        fs::write(path, serde_json::to_vec(&state).unwrap()).expect("write fake tmux state");
    }

    fn state(&self, id: &str) -> Value {
        let path = self.home.join("server.fake.json");
        let state: Value = serde_json::from_slice(&fs::read(path).expect("read fake tmux state"))
            .expect("parse fake tmux state");
        state[id].clone()
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct WaitRequest<'a> {
    harness: &'a Harness,
    id: &'a str,
    expected: &'a str,
}

fn wait_for_state(request: WaitRequest<'_>) -> Value {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let report = request.harness.json(&["inspect", request.id, "--json"]);
        if report["state"] == request.expected {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "session never became {}",
            request.expected
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn private_tmux_preserves_the_cli_contract_and_never_persists_launch_material() {
    let harness = Harness::new();
    let created = harness.create(
        "test \"$TERM\" = xterm-256color; test -z \"$TMUX\"; \
         test \"$LATCH_INHERITED\" = fresh-client-environment; exit 7",
    );
    let id = created["session"]["id"].as_str().expect("session id");

    let metadata_path = harness.home.join("sessions").join(id).join("meta.json");
    let metadata = fs::read_to_string(metadata_path).expect("read metadata");
    assert!(!metadata.contains("must-not-reach-disk"));
    assert!(!metadata.contains("host-value-must-not-win"));
    assert!(!metadata.contains("fresh-client-environment"));
    assert!(!metadata.contains("/bin/sh"));
    assert!(metadata.contains("\"command_label\":\"redacted\""));

    let inspect = wait_for_state(WaitRequest {
        harness: &harness,
        id,
        expected: "exited",
    });
    assert_eq!(inspect["exit"]["code"], 7);
    assert_eq!(inspect["size"], json!({"cols": 100, "rows": 30}));

    let listed = harness.json(&["list", "--json"]);
    assert_eq!(listed["sessions"][0]["id"], id);
    assert_eq!(listed["sessions"][0]["state"], "exited");

    let config = fs::read_to_string(harness.home.join("tmux.conf")).expect("read tmux config");
    assert!(config.contains("status off"));
    assert!(config.contains("prefix None"));
    assert!(config.contains("remain-on-exit on"));
    assert!(config.contains("window-size latest"));
    assert!(config.contains("default-terminal \"xterm-256color\""));
    assert!(config.contains("-T copy-mode"));
}

#[test]
fn multiple_and_nested_attaches_are_always_accepted_and_resize_can_pin() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    for _ in 0..2 {
        let output = harness.command().args(["attach", id]).output().unwrap();
        assert_success(&output);
    }
    let output = harness
        .command()
        .env("LATCH_SESSION_ID", id)
        .output()
        .unwrap();
    assert_success(&output);

    let resized = harness.json(&[
        "resize", id, "--cols", "132", "--rows", "43", "--pin", "--json",
    ]);
    assert_eq!(resized["cols"], 132);
    assert_eq!(resized["rows"], 43);
    let inspect = harness.json(&["inspect", id, "--json"]);
    assert_eq!(inspect["size"], json!({"cols": 132, "rows": 43}));

    let removed = harness.json(&["remove", id, "--force", "--json"]);
    assert_eq!(removed["removed"], true);
}

#[test]
fn interaction_is_screen_gated_and_resolution_is_request_bound() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    harness.set_screen(id, "conversation\n\n  ❯   \n  ? for shortcuts\n");
    let idle = harness.json(&["capabilities", id, "--json"]);
    assert_eq!(
        idle,
        json!({
            "sendMessage": true,
            "sendKeys": true,
            "resolve": false,
            "canSend": {"ok": true}
        })
    );

    let mut message = harness
        .command()
        .args(["send", id, "--message", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn message send");
    std::io::Write::write_all(
        message.stdin.as_mut().expect("message stdin"),
        b"continue the task",
    )
    .unwrap();
    let output = message.wait_with_output().expect("wait for message send");
    assert_success(&output);
    let state = harness.state(id);
    assert_eq!(state["pasted"], json!(["continue the task"]));
    assert_eq!(state["sent_keys"], json!(["Enter"]));

    let session_dir = harness.home.join("sessions").join(id);
    fs::write(
        session_dir.join("harness-hooks.jsonl"),
        concat!(
            "{\"session_id\":\"claude-session-3\",\"hook_event_name\":\"PermissionRequest\",",
            "\"request_id\":\"permission-1\",\"tool_name\":\"Bash\",",
            "\"tool_input\":{\"description\":\"Install the test runner\"},",
            "\"permission_suggestions\":[{\"description\":\"Allow once\"},{\"description\":\"Deny\"}]}\n"
        ),
    )
    .unwrap();
    harness.set_screen(
        id,
        "Install the test runner\n  1. Allow once\n  2. Deny\nEnter to confirm\n",
    );
    let pending = harness.json(&["capabilities", id, "--json"]);
    assert_eq!(pending["sendMessage"], false);
    assert_eq!(pending["sendKeys"], true);
    assert_eq!(pending["resolve"], true);
    assert_eq!(pending["canSend"]["ok"], true);

    let resolved = harness.json(&["send", id, "--resolve", "permission-1=Allow once", "--json"]);
    assert_eq!(resolved["resolved"], true);
    let state = harness.state(id);
    assert_eq!(state["sent_keys"], json!(["Enter", "1"]));

    let repeated = harness
        .command()
        .args(["send", id, "--resolve", "permission-1=Allow once", "--json"])
        .output()
        .unwrap();
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already resolved"));
}

#[test]
fn serve_exposes_sessions_and_a_pty_terminal_behind_a_bearer_token() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    let token_output = harness
        .command()
        .args(["serve", "token"])
        .output()
        .expect("mint token");
    assert_success(&token_output);
    let token = String::from_utf8(token_output.stdout)
        .unwrap()
        .trim()
        .to_owned();
    assert_eq!(token.len(), 64);
    let token_path = harness.home.join("serve.token");
    let mode = fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let mut child = harness
        .command()
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn latch serve");
    let stderr = child.stderr.take().expect("serve stderr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if let Some(addr) = line.strip_prefix("latch serve listening on ") {
                let _ = tx.send(addr.to_owned());
                break;
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("serve bound an address");

    let unauth = http_get(HttpGet {
        addr: &addr,
        path: "/v1/sessions",
        token: None,
        origin: None,
    });
    assert_eq!(unauth.status, 401);
    let wrong = http_get(HttpGet {
        addr: &addr,
        path: "/v1/sessions",
        token: Some("nope"),
        origin: None,
    });
    assert_eq!(wrong.status, 401);
    let blocked = http_get(HttpGet {
        addr: &addr,
        path: "/v1/sessions",
        token: Some(&token),
        origin: Some("https://evil.example"),
    });
    assert_eq!(blocked.status, 403);

    let listed = http_get(HttpGet {
        addr: &addr,
        path: "/v1/sessions",
        token: Some(&token),
        origin: None,
    });
    assert_eq!(listed.status, 200);
    let body: Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(body["sessions"][0]["id"], id);

    let inspect = http_get(HttpGet {
        addr: &addr,
        path: &format!("/v1/sessions/{id}"),
        token: Some(&token),
        origin: None,
    });
    assert_eq!(inspect.status, 200);
    let inspect_body: Value = serde_json::from_str(&inspect.body).unwrap();
    assert_eq!(inspect_body["id"], id);

    let capabilities = http_get(HttpGet {
        addr: &addr,
        path: &format!("/v1/sessions/{id}/capabilities"),
        token: Some(&token),
        origin: None,
    });
    assert_eq!(capabilities.status, 200);
    let capabilities_body: Value = serde_json::from_str(&capabilities.body).unwrap();
    assert!(capabilities_body.get("canSend").is_some());

    let request = tungstenite::http::Request::builder()
        .uri(format!("ws://{addr}/v1/sessions/{id}/terminal"))
        .header("Host", &addr)
        .header("Authorization", format!("Bearer {token}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let (mut socket, response) = tungstenite::connect(request).expect("terminal websocket");
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
    );
    socket
        .send(tungstenite::Message::Text(
            r#"{"type":"resize","cols":100,"rows":30}"#.into(),
        ))
        .unwrap();
    socket.close(None).ok();
    let _ = child.kill();
    let _ = child.wait();
}

struct HttpResponse {
    status: u16,
    body: String,
}

struct HttpGet<'a> {
    addr: &'a str,
    path: &'a str,
    token: Option<&'a str>,
    origin: Option<&'a str>,
}

fn http_get(request: HttpGet<'_>) -> HttpResponse {
    let mut stream = TcpStream::connect(request.addr).expect("connect to latch serve");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut message = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n",
        path = request.path,
        addr = request.addr
    );
    if let Some(token) = request.token {
        message.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(origin) = request.origin {
        message.push_str(&format!("Origin: {origin}\r\n"));
    }
    message.push_str("\r\n");
    stream.write_all(message.as_bytes()).unwrap();
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    HttpResponse {
        status,
        body: body.to_owned(),
    }
}

const FAKE_TMUX: &str = r#"#!/usr/bin/env python3
import json
import os
import signal
import subprocess
import sys
import time

args = sys.argv[1:]
if args == ["-V"]:
    print("tmux 3.7b")
    raise SystemExit(0)

socket = args[args.index("-S") + 1]
args = args[args.index("-f") + 2:]
state_path = socket + ".fake.json"

def load():
    try:
        with open(state_path) as source:
            return json.load(source)
    except FileNotFoundError:
        return {}

def save(state):
    os.makedirs(os.path.dirname(state_path), exist_ok=True)
    temporary = state_path + ".tmp"
    with open(temporary, "w") as destination:
        json.dump(state, destination)
    os.replace(temporary, state_path)

def refresh(entry):
    exit_path = entry["exit_path"]
    if os.path.exists(exit_path):
        with open(exit_path) as source:
            entry["status"] = int(source.read() or "0")
        entry["dead"] = True
        entry["dead_at"] = entry["dead_at"] or int(time.time())
    elif not entry["dead"]:
        try:
            os.kill(entry["pid"], 0)
        except ProcessLookupError:
            entry["status"] = 137
            entry["dead"] = True
            entry["dead_at"] = int(time.time())
            entry["signal"] = 9
    return entry

state = load()
command = args.pop(0)

if command == "new-session":
    session = None
    cols = 80
    rows = 24
    cwd = os.getcwd()
    environment = os.environ.copy()
    index = 0
    while index < len(args):
        value = args[index]
        if value == "--":
            child = args[index + 1:]
            break
        if value == "-d":
            index += 1
            continue
        if value in ("-s", "-x", "-y", "-c", "-e"):
            argument = args[index + 1]
            if value == "-s":
                session = argument
            elif value == "-x":
                cols = int(argument)
            elif value == "-y":
                rows = int(argument)
            elif value == "-c":
                cwd = argument
            else:
                key, setting = argument.split("=", 1)
                environment[key] = setting
            index += 2
            continue
        index += 1
    exit_path = socket + "." + session + ".exit"
    wrapper = [
        "/bin/sh", "-c",
        '"$@"; code=$?; printf "%s" "$code" > "$0"; exit "$code"',
        exit_path, *child
    ]
    process = subprocess.Popen(
        wrapper,
        cwd=cwd,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True
    )
    state[session] = {
        "pid": process.pid,
        "cols": cols,
        "rows": rows,
        "activity": int(time.time()),
        "attached": 0,
        "dead": False,
        "status": None,
        "dead_at": None,
        "signal": None,
        "exit_path": exit_path,
        "screen": "",
        "sent_keys": [],
        "pasted": [],
        "buffers": {}
    }
    save(state)
elif command == "list-sessions":
    for name, entry in state.items():
        refresh(entry)
        status = "" if entry["status"] is None else str(entry["status"])
        print("\t".join([
            name,
            "1" if entry["dead"] else "0",
            status,
            str(entry["cols"]),
            str(entry["rows"]),
            str(entry["activity"]),
            str(entry["attached"]),
            str(entry["pid"]),
            "" if entry["dead_at"] is None else str(entry["dead_at"]),
            "" if entry["signal"] is None else str(entry["signal"])
        ]))
    save(state)
elif command == "display-message":
    session = args[args.index("-t") + 1]
    if session not in state:
        raise SystemExit(1)
    entry = refresh(state[session])
    status = "" if entry["status"] is None else str(entry["status"])
    print("\t".join([
        session,
        "1" if entry["dead"] else "0",
        status,
        str(entry["cols"]),
        str(entry["rows"]),
        str(entry["activity"]),
        str(entry["attached"]),
        str(entry["pid"]),
        "" if entry["dead_at"] is None else str(entry["dead_at"]),
        "" if entry["signal"] is None else str(entry["signal"])
    ]))
    save(state)
elif command == "capture-pane":
    session = args[args.index("-t") + 1]
    print(state[session].get("screen", ""), end="")
elif command == "send-keys":
    session = args[args.index("-t") + 1]
    keys = args[args.index("--") + 1:]
    state[session].setdefault("sent_keys", []).extend(keys)
    save(state)
elif command == "load-buffer":
    name = args[args.index("-b") + 1]
    buffers = state.setdefault("_buffers", {})
    buffers[name] = sys.stdin.read()
    save(state)
elif command == "paste-buffer":
    session = args[args.index("-t") + 1]
    name = args[args.index("-b") + 1]
    buffers = state.setdefault("_buffers", {})
    state[session].setdefault("pasted", []).append(buffers[name])
    if "-d" in args:
        del buffers[name]
    save(state)
elif command == "delete-buffer":
    name = args[args.index("-b") + 1]
    state.setdefault("_buffers", {}).pop(name, None)
    save(state)
elif command == "has-session":
    session = args[args.index("-t") + 1]
    raise SystemExit(0 if session in state else 1)
elif command == "attach-session":
    session = args[args.index("-t") + 1]
    raise SystemExit(0 if session in state else 1)
elif command == "resize-window":
    session = args[args.index("-t") + 1]
    state[session]["cols"] = int(args[args.index("-x") + 1])
    state[session]["rows"] = int(args[args.index("-y") + 1])
    state[session]["activity"] = int(time.time())
    save(state)
elif command == "set-option":
    pass
elif command == "kill-session":
    session = args[args.index("-t") + 1]
    entry = state.pop(session, None)
    if entry and not refresh(entry)["dead"]:
        try:
            os.killpg(entry["pid"], signal.SIGKILL)
        except ProcessLookupError:
            pass
    save(state)
else:
    print("unsupported fake tmux command: " + command, file=sys.stderr)
    raise SystemExit(2)
"#;
