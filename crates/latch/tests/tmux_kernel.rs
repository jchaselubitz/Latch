use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
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
        self.create_session(CreateSession {
            shell,
            command_label: "redacted",
        })
    }

    fn create_session(&self, request: CreateSession<'_>) -> Value {
        let manifest = json!({
            "format_version": 1,
            "launch": {
                "argv": ["/bin/sh", "-c", request.shell],
                "cwd": self._temp.path(),
                "env": {"LATCH_TEST_SECRET": "must-not-reach-disk"},
                "inherit_env": true,
                "size": {"cols": 100, "rows": 30},
                "term": "host-value-must-not-win"
            },
            "display": {
                "name": "agent",
                "title": "Kernel acceptance",
                "command_label": request.command_label,
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

    fn last_attach(&self) -> Option<String> {
        let path = self.home.join("server.fake.json");
        let state: Value = serde_json::from_slice(&fs::read(path).expect("read fake tmux state"))
            .expect("parse fake tmux state");
        state
            .get("_last_attach")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn survive_stop(&self, id: &str) {
        self.patch_state(id, |entry| {
            entry["survive_stop"] = Value::Bool(true);
        });
    }

    fn fail_next_attach_and_remove(&self, id: &str) {
        self.patch_state(id, |entry| {
            entry["fail_attach_remove"] = Value::Bool(true);
        });
    }

    fn set_screen(&self, id: &str, screen: &str) {
        self.patch_state(id, |entry| {
            entry["screen"] = Value::String(screen.to_owned());
        });
    }

    fn fail_next_send_keys(&self, id: &str) {
        self.patch_state(id, |entry| {
            entry["fail_send_keys"] = Value::Bool(true);
        });
    }

    fn mark_claude_harness(&self, id: &str) {
        let path = self.home.join("sessions").join(id).join("meta.json");
        let mut metadata: Value = serde_json::from_slice(&fs::read(&path).expect("read metadata"))
            .expect("parse metadata");
        metadata["harness"] = json!("claude");
        fs::write(path, serde_json::to_vec(&metadata).unwrap()).expect("write metadata");
    }

    fn patch_state(&self, id: &str, patch: impl FnOnce(&mut Value)) {
        let path = self.home.join("server.fake.json");
        let mut state: Value =
            serde_json::from_slice(&fs::read(&path).expect("read fake tmux state"))
                .expect("parse fake tmux state");
        patch(&mut state[id]);
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
    assert_eq!(inspect["attached"], 0);
    assert!(inspect.get("attachments").is_none());

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
    let unknown = harness.json(&["capabilities", id, "--json"]);
    assert_eq!(unknown["sendMessage"], false);
    assert_eq!(unknown["sendKeys"], true);
    assert_eq!(unknown["resolve"], false);
    assert_eq!(unknown["canSend"]["ok"], false);
    assert!(unknown["canSend"]["reason"]
        .as_str()
        .unwrap()
        .contains("not a known Claude Code harness"));

    let refused = harness
        .command()
        .args(["send", id, "--message", "-", "--json"])
        .stdin(Stdio::null())
        .output()
        .expect("refuse message send");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not a known Claude Code harness"));

    let keys = harness.json(&["send", id, "--keys", "C-c", "--json"]);
    assert_eq!(keys["sent"], true);

    harness.mark_claude_harness(id);
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
    assert_eq!(state["sent_keys"], json!(["C-c", "Enter"]));

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
    assert_eq!(state["sent_keys"], json!(["C-c", "Enter", "1"]));

    let repeated = harness
        .command()
        .args(["send", id, "--resolve", "permission-1=Allow once", "--json"])
        .output()
        .unwrap();
    assert!(!repeated.status.success());
    assert!(String::from_utf8_lossy(&repeated.stderr).contains("already resolved"));
}

#[test]
fn paste_enter_failure_reports_unsubmitted_text_and_recovery() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    harness.mark_claude_harness(id);
    harness.set_screen(id, "conversation\n\n  ❯   \n  ? for shortcuts\n");
    harness.fail_next_send_keys(id);

    let mut message = harness
        .command()
        .args(["send", id, "--message", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn message send");
    std::io::Write::write_all(
        message.stdin.as_mut().expect("message stdin"),
        b"do not submit this",
    )
    .unwrap();
    let output = message.wait_with_output().expect("wait for message send");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("pasted into the composer but not submitted"));
    assert!(stderr.contains("latch send --keys C-u"));
    let state = harness.state(id);
    assert_eq!(state["pasted"], json!(["do not submit this"]));
    assert_eq!(state["sent_keys"], json!([]));
}

#[test]
fn stop_reports_failure_when_the_process_survives_sigkill() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    let stopped = harness.json(&["stop", id, "--json"]);
    assert_eq!(stopped["id"], id);
    assert_eq!(stopped["state"], "exited");
    assert_eq!(stopped["stopped"], true);

    let created = harness.create("sleep 30");
    let stubborn = created["session"]["id"].as_str().unwrap();
    harness.survive_stop(stubborn);
    let output = harness
        .command()
        .args(["stop", stubborn, "--force", "--json"])
        .output()
        .expect("stop stubborn session");
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).expect("stop JSON");
    assert_eq!(report["id"], stubborn);
    assert_eq!(report["state"], "running");
    assert_eq!(report["stopped"], false);
    assert!(String::from_utf8_lossy(&output.stderr).contains("still running after stop"));
}

#[test]
fn stop_all_requires_confirmation_and_stops_every_live_session() {
    let harness = Harness::new();
    let first = harness.create("sleep 30");
    let second = harness.create("sleep 30");

    let refused = harness
        .command()
        .args(["stop", "--all", "--json"])
        .output()
        .expect("run unconfirmed stop all");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));

    let stopped = harness.json(&["stop", "--all", "--yes", "--json"]);
    let ids = stopped["sessions"]
        .as_array()
        .expect("stop-all sessions")
        .iter()
        .map(|session| session["id"].as_str().expect("session id"))
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&first["session"]["id"].as_str().unwrap()));
    assert!(ids.contains(&second["session"]["id"].as_str().unwrap()));
    assert!(stopped["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|session| session["stopped"] == true));
}

#[test]
fn rename_refuses_a_name_already_used_by_another_session() {
    let harness = Harness::new();
    let first = harness.create("sleep 30");
    let second = harness.create("sleep 30");
    let first_id = first["session"]["id"].as_str().unwrap();
    let second_id = second["session"]["id"].as_str().unwrap();

    let renamed = harness.json(&["rename", first_id, "alpha", "--json"]);
    assert_eq!(renamed["name"], "alpha");
    let again = harness.json(&["rename", first_id, "alpha", "--json"]);
    assert_eq!(again["name"], "alpha");

    let output = harness
        .command()
        .args(["rename", second_id, "alpha", "--json"])
        .output()
        .expect("colliding rename");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("already used by"));
    assert!(stderr.contains(first_id));
}

#[test]
fn bare_attach_selects_the_most_recently_active_session() {
    let harness = Harness::new();
    let first = harness.create("sleep 30");
    let second = harness.create("sleep 30");
    let older = first["session"]["id"].as_str().unwrap();
    let newer = second["session"]["id"].as_str().unwrap();
    harness.patch_state(older, |entry| {
        entry["activity"] = json!(2_000_000_000u64);
    });
    harness.patch_state(newer, |entry| {
        entry["activity"] = json!(1_000_000_000u64);
    });

    let output = harness.command().args(["attach"]).output().unwrap();
    assert_success(&output);
    assert_eq!(harness.last_attach().as_deref(), Some(older));
}

#[test]
fn attach_retry_does_not_retry_a_session_that_is_gone() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    harness.fail_next_attach_and_remove(id);

    let started = Instant::now();
    let output = harness
        .command()
        .args(["attach", "--retry", id])
        .output()
        .expect("attach retry");
    assert!(!output.status.success());
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "attach --retry retried a permanent failure: {:?}",
        started.elapsed()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not available") || stderr.contains("could not attach"),
        "stderr: {stderr}"
    );
}

#[test]
fn named_shell_inside_a_session_refuses_instead_of_dropping_the_name() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let output = harness
        .command()
        .env("LATCH_SESSION_ID", id)
        .args(["shell", "--name", "nested"])
        .output()
        .expect("nested named shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot nest"));
    assert!(stderr.contains(id));
}

#[test]
fn session_rows_parse_without_a_utf8_locale_in_the_environment() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    // Finder/launchd/cron parents ship no locale at all; tmux would sanitize
    // the U+001F separator to '_' unless latch injects a UTF-8 LC_CTYPE.
    let cleared = harness
        .command()
        .env_remove("LANG")
        .env_remove("LC_ALL")
        .env_remove("LC_CTYPE")
        .args(["inspect", id, "--json"])
        .output()
        .expect("inspect without locale environment");
    assert_success(&cleared);
    let inspect: Value = serde_json::from_slice(&cleared.stdout).expect("inspect JSON");
    assert_eq!(inspect["id"], id);
    assert_eq!(inspect["state"], "running");

    // A non-UTF-8 LC_ALL outranks LC_CTYPE, so latch must clear it too.
    let posix = harness
        .command()
        .env_remove("LANG")
        .env_remove("LC_CTYPE")
        .env("LC_ALL", "POSIX")
        .args(["list", "--json"])
        .output()
        .expect("list under a POSIX locale");
    assert_success(&posix);
    let listed: Value = serde_json::from_slice(&posix.stdout).expect("list JSON");
    assert_eq!(listed["sessions"][0]["id"], id);
}

#[test]
fn missing_home_is_an_error_not_a_panic() {
    let latch = PathBuf::from(env!("CARGO_BIN_EXE_latch"));
    let output = Command::new(latch)
        .env_remove("HOME")
        .env_remove("LATCH_HOME")
        .args(["shell"])
        .output()
        .expect("shell without HOME");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HOME") || stderr.contains("Latch root"));
    assert!(!stderr.to_lowercase().contains("panic"));
}

#[test]
fn serve_exposes_sessions_and_a_pty_terminal_behind_a_bearer_token() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let gateway = start_gateway(&harness);

    let unauth = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/sessions",
        token: None,
        origin: None,
    });
    assert_eq!(unauth.status, 401);
    let wrong = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/sessions",
        token: Some("nope"),
        origin: None,
    });
    assert_eq!(wrong.status, 401);
    let blocked = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/sessions",
        token: Some(&gateway.token),
        origin: Some("https://evil.example"),
    });
    assert_eq!(blocked.status, 403);

    let listed = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/sessions",
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(listed.status, 200);
    let body: Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(body["sessions"][0]["id"], id);

    let inspect = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}"),
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(inspect.status, 200);
    let inspect_body: Value = serde_json::from_str(&inspect.body).unwrap();
    assert_eq!(inspect_body["id"], id);
    assert_eq!(inspect_body["attached"], 0);
    assert!(inspect_body.get("attachments").is_none());

    let missing = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/sessions/missing-session",
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(missing.status, 404);
    let missing_body: Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(missing_body["error"], "session not found");
    assert!(!missing.body.contains("no session named"));

    let capabilities = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/capabilities"),
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(capabilities.status, 200);
    let capabilities_body: Value = serde_json::from_str(&capabilities.body).unwrap();
    assert!(capabilities_body.get("canSend").is_some());
    assert_eq!(capabilities_body["events"]["ok"], false);
    assert_eq!(
        capabilities_body["events"]["reason"],
        "no harness connector"
    );

    let gateway_caps = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v1/capabilities",
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(gateway_caps.status, 200);
    let gateway_body: Value = serde_json::from_str(&gateway_caps.body).unwrap();
    assert_eq!(gateway_body["endpoints"]["events"], true);
    assert_eq!(gateway_body["endpoints"]["send"], true);
    assert_eq!(gateway_body["endpoints"]["terminal"], true);

    let (mut socket, response) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cols: Some(100),
        rows: Some(30),
    });
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
}

#[test]
fn serve_terminal_relays_bytes_in_both_directions() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let gateway = start_gateway_with(GatewayOptions {
        harness: &harness,
        echo_attach: true,
        claude_config: None,
    });

    let (mut socket, _) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cols: Some(100),
        rows: Some(30),
    });
    // Output reaches the client without the client saying anything first.
    assert!(read_terminal_until(&mut socket, "<attached>"));
    socket
        .send(tungstenite::Message::Binary(b"ping\r".to_vec().into()))
        .unwrap();
    // And input reaches the process on the far side of the PTY, which is the
    // half a viewer-only client would silently drop.
    assert!(read_terminal_until(&mut socket, "<echo>ping"));
    socket.close(None).ok();
}

#[test]
fn serve_terminal_uses_query_size_and_waits_for_resize_without_it() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let gateway = start_gateway(&harness);
    assert_eq!(harness.json(&["inspect", id, "--json"])["attached"], 0);

    let (mut waiting, _) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cols: None,
        rows: None,
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(harness.json(&["inspect", id, "--json"])["attached"], 0);
    waiting
        .send(tungstenite::Message::Text(
            r#"{"type":"resize","cols":132,"rows":43}"#.into(),
        ))
        .unwrap();
    wait_for_attached(WaitAttached {
        harness: &harness,
        id,
        min: 1,
    });
    waiting.close(None).ok();

    let before_query = harness.json(&["inspect", id, "--json"])["attached"]
        .as_u64()
        .unwrap();
    let (mut sized, _) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cols: Some(100),
        rows: Some(30),
    });
    wait_for_attached(WaitAttached {
        harness: &harness,
        id,
        min: (before_query as usize) + 1,
    });
    sized.close(None).ok();
}

#[test]
fn serve_terminal_close_reports_missing_session() {
    let harness = Harness::new();
    let gateway = start_gateway(&harness);
    let (mut socket, response) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: "missing-session",
        token: &gateway.token,
        cols: None,
        rows: None,
    });
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let message = socket.read().expect("websocket message");
        match message {
            tungstenite::Message::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), 4404);
                assert!(
                    frame.reason.as_str().contains("session not found"),
                    "close reason {}",
                    frame.reason
                );
                break;
            }
            tungstenite::Message::Close(None) => panic!("silent close without code"),
            other => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for close, last={other:?}"
                );
            }
        }
    }
}

#[test]
fn serve_refuses_non_loopback_bind_without_allow_remote() {
    let harness = Harness::new();
    let output = harness
        .command()
        .args(["serve", "--bind", "0.0.0.0:0"])
        .output()
        .expect("run latch serve");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--allow-remote"), "{stderr}");
    assert!(stderr.contains("SSH tunnel"), "{stderr}");
    assert!(!harness.home.join("serve.token").is_file());
}

#[test]
fn serve_events_close_reports_missing_session_and_missing_connector() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let gateway = start_gateway(&harness);

    let (mut missing, _) = connect_events(EventsWsRequest {
        addr: &gateway.addr,
        session: "missing-session",
        token: &gateway.token,
        cursor: None,
    });
    assert_ws_close(&mut missing, 4404, "session not found");

    let (mut no_connector, _) = connect_events(EventsWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cursor: None,
    });
    assert_ws_close(&mut no_connector, 4408, "no harness connector");
}

#[test]
fn serve_events_streams_harness_events_from_a_cursor() {
    let harness = Harness::new();
    let created = harness.create_session(CreateSession {
        shell: "sleep 30",
        command_label: "claude",
    });
    let id = created["session"]["id"].as_str().unwrap();
    let inspect = harness.json(&["inspect", id, "--json"]);
    let cwd = inspect["cwd"].as_str().unwrap();
    let claude_config = plant_claude_transcript(PlantTranscript {
        harness: &harness,
        cwd,
    });
    let gateway = start_gateway_with(GatewayOptions {
        harness: &harness,
        echo_attach: false,
        claude_config: Some(claude_config),
    });

    let capabilities = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/capabilities"),
        token: Some(&gateway.token),
        origin: None,
    });
    assert_eq!(capabilities.status, 200);
    let capabilities_body: Value = serde_json::from_str(&capabilities.body).unwrap();
    assert_eq!(capabilities_body["events"]["ok"], true);
    assert_eq!(capabilities_body["events"]["connectorEpoch"], 1);

    let (mut socket, _) = connect_events(EventsWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cursor: Some(4),
    });
    let first = read_json_event(&mut socket);
    assert_eq!(first["type"], "assistant_message");
    assert_eq!(first["text"], "The build passes.");
    let second = read_json_event(&mut socket);
    assert_eq!(second["type"], "status");
    assert_eq!(second["status"], "idle");
    socket.close(None).ok();

    let (mut stale, _) = connect_events(EventsWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cursor: Some(99),
    });
    assert_ws_close(&mut stale, 4422, "stale cursor");
}

#[test]
fn serve_send_is_capability_gated_and_refusals_are_409() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    harness.set_screen(id, "conversation\n\n  ❯   \n  ? for shortcuts\n");
    let gateway = start_gateway(&harness);

    let unauth = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: None,
        body: r#"{"message":"hello"}"#,
    });
    assert_eq!(unauth.status, 401);

    let missing = http_post(HttpPost {
        addr: &gateway.addr,
        path: "/v1/sessions/missing-session/send",
        token: Some(&gateway.token),
        body: r#"{"message":"hello"}"#,
    });
    assert_eq!(missing.status, 404);
    let missing_body: Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(missing_body["error"], "session not found");

    let invalid = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: "{}",
    });
    assert_eq!(invalid.status, 400);
    let invalid_body: Value = serde_json::from_str(&invalid.body).unwrap();
    assert!(invalid_body["error"]
        .as_str()
        .unwrap()
        .contains("exactly one"));

    let refused = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: r#"{"message":"hello"}"#,
    });
    assert_eq!(refused.status, 409);
    let refused_body: Value = serde_json::from_str(&refused.body).unwrap();
    assert_eq!(refused_body["error"], "refused");
    assert!(refused_body["reason"]
        .as_str()
        .unwrap()
        .contains("not a known Claude Code harness"));

    let keys = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: r#"{"keys":"C-c"}"#,
    });
    assert_eq!(keys.status, 200);
    let keys_body: Value = serde_json::from_str(&keys.body).unwrap();
    assert_eq!(keys_body["sent"], true);
    assert_eq!(keys_body["operation"], "keys");

    harness.mark_claude_harness(id);
    let message = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: r#"{"message":"continue the task"}"#,
    });
    assert_eq!(message.status, 200, "{}", message.body);
    let message_body: Value = serde_json::from_str(&message.body).unwrap();
    assert_eq!(message_body["sent"], true);
    assert_eq!(message_body["operation"], "message");
    let state = harness.state(id);
    assert_eq!(state["pasted"], json!(["continue the task"]));

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
    let resolved = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: r#"{"resolve":{"requestId":"permission-1","choice":"Allow once"}}"#,
    });
    assert_eq!(resolved.status, 200, "{}", resolved.body);
    let resolved_body: Value = serde_json::from_str(&resolved.body).unwrap();
    assert_eq!(resolved_body["resolved"], true);
    assert_eq!(resolved_body["requestId"], "permission-1");

    let stale = http_post(HttpPost {
        addr: &gateway.addr,
        path: &format!("/v1/sessions/{id}/send"),
        token: Some(&gateway.token),
        body: r#"{"resolve":{"requestId":"permission-1","choice":"Allow once"}}"#,
    });
    assert_eq!(stale.status, 409);
    let stale_body: Value = serde_json::from_str(&stale.body).unwrap();
    assert_eq!(stale_body["error"], "refused");
    assert!(stale_body["reason"]
        .as_str()
        .unwrap()
        .contains("already resolved"));
}

struct CreateSession<'a> {
    shell: &'a str,
    command_label: &'a str,
}

struct Gateway {
    child: Child,
    addr: String,
    token: String,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct GatewayOptions<'a> {
    harness: &'a Harness,
    /// Make the fake tmux echo PTY input back, so a relay test can prove both
    /// directions rather than only that a socket opened.
    echo_attach: bool,
    /// `CLAUDE_CONFIG_DIR` for `latch events` transcript discovery.
    claude_config: Option<PathBuf>,
}

fn start_gateway(harness: &Harness) -> Gateway {
    start_gateway_with(GatewayOptions {
        harness,
        echo_attach: false,
        claude_config: None,
    })
}

fn start_gateway_with(options: GatewayOptions<'_>) -> Gateway {
    let harness = options.harness;
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

    let mut serve = harness.command();
    serve.args(["serve", "--bind", "127.0.0.1:0"]);
    if options.echo_attach {
        serve.env("LATCH_FAKE_TMUX_ECHO", "1");
    }
    if let Some(claude_config) = &options.claude_config {
        serve.env("CLAUDE_CONFIG_DIR", claude_config);
    }
    let mut child = serve
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
    Gateway { child, addr, token }
}

struct TerminalWsRequest<'a> {
    addr: &'a str,
    session: &'a str,
    token: &'a str,
    cols: Option<u16>,
    rows: Option<u16>,
}

fn connect_terminal(
    request: TerminalWsRequest<'_>,
) -> (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::http::Response<Option<Vec<u8>>>,
) {
    let mut uri = format!(
        "ws://{}/v1/sessions/{}/terminal",
        request.addr, request.session
    );
    if let (Some(cols), Some(rows)) = (request.cols, request.rows) {
        uri.push_str(&format!("?cols={cols}&rows={rows}"));
    }
    let http_request = tungstenite::http::Request::builder()
        .uri(&uri)
        .header("Host", request.addr)
        .header("Authorization", format!("Bearer {}", request.token))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    tungstenite::connect(http_request).expect("terminal websocket")
}

struct EventsWsRequest<'a> {
    addr: &'a str,
    session: &'a str,
    token: &'a str,
    cursor: Option<usize>,
}

fn connect_events(
    request: EventsWsRequest<'_>,
) -> (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::http::Response<Option<Vec<u8>>>,
) {
    let mut uri = format!(
        "ws://{}/v1/sessions/{}/events",
        request.addr, request.session
    );
    if let Some(cursor) = request.cursor {
        uri.push_str(&format!("?cursor={cursor}"));
    }
    let http_request = tungstenite::http::Request::builder()
        .uri(&uri)
        .header("Host", request.addr)
        .header("Authorization", format!("Bearer {}", request.token))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    tungstenite::connect(http_request).expect("events websocket")
}

fn read_json_event(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Value {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let message = socket.read().expect("events websocket message");
        match message {
            tungstenite::Message::Text(text) => {
                return serde_json::from_str(text.as_str()).expect("HarnessEvent JSON");
            }
            tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => {}
            other => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for an event, last={other:?}"
                );
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for an event frame"
        );
    }
}

fn assert_ws_close(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    code: u16,
    reason: &str,
) {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("read timeout");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let message = socket.read().expect("websocket message");
        match message {
            tungstenite::Message::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), code);
                assert!(
                    frame.reason.as_str().contains(reason),
                    "close reason {}",
                    frame.reason
                );
                return;
            }
            tungstenite::Message::Close(None) => panic!("silent close without code"),
            other => {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for close {code}, last={other:?}"
                );
            }
        }
    }
}

struct PlantTranscript<'a> {
    harness: &'a Harness,
    cwd: &'a str,
}

fn plant_claude_transcript(request: PlantTranscript<'_>) -> PathBuf {
    let claude_config = request.harness.home.join("claude");
    let encoded: String = request
        .cwd
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let project = claude_config.join("projects").join(encoded);
    fs::create_dir_all(&project).expect("claude project dir");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/harness/claude-code/conversation/raw.jsonl");
    fs::copy(&fixture, project.join("run-1.jsonl")).expect("plant transcript");
    claude_config
}

/// Reads terminal frames until `needle` shows up or the deadline passes.
fn read_terminal_until(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    needle: &str,
) -> bool {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .expect("read timeout");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seen = String::new();
    while Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Binary(bytes)) => {
                seen.push_str(&String::from_utf8_lossy(&bytes));
                if seen.contains(needle) {
                    return true;
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    false
}

struct WaitAttached<'a> {
    harness: &'a Harness,
    id: &'a str,
    min: usize,
}

fn wait_for_attached(request: WaitAttached<'_>) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let attached = request.harness.json(&["inspect", request.id, "--json"])["attached"]
            .as_u64()
            .unwrap_or(0);
        if attached >= request.min as u64 {
            return attached;
        }
        assert!(
            Instant::now() < deadline,
            "session never reached {} attachments",
            request.min
        );
        thread::sleep(Duration::from_millis(20));
    }
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

struct HttpPost<'a> {
    addr: &'a str,
    path: &'a str,
    token: Option<&'a str>,
    body: &'a str,
}

struct HttpRequest<'a> {
    addr: &'a str,
    method: &'a str,
    path: &'a str,
    token: Option<&'a str>,
    origin: Option<&'a str>,
    body: Option<&'a str>,
}

fn http_get(request: HttpGet<'_>) -> HttpResponse {
    http_request(HttpRequest {
        addr: request.addr,
        method: "GET",
        path: request.path,
        token: request.token,
        origin: request.origin,
        body: None,
    })
}

fn http_post(request: HttpPost<'_>) -> HttpResponse {
    http_request(HttpRequest {
        addr: request.addr,
        method: "POST",
        path: request.path,
        token: request.token,
        origin: None,
        body: Some(request.body),
    })
}

fn http_request(request: HttpRequest<'_>) -> HttpResponse {
    let mut stream = TcpStream::connect(request.addr).expect("connect to latch serve");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut message = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n",
        method = request.method,
        path = request.path,
        addr = request.addr
    );
    if let Some(token) = request.token {
        message.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    if let Some(origin) = request.origin {
        message.push_str(&format!("Origin: {origin}\r\n"));
    }
    if let Some(body) = request.body {
        message.push_str("Content-Type: application/json\r\n");
        message.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    message.push_str("\r\n");
    if let Some(body) = request.body {
        message.push_str(body);
    }
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

const FAKE_TMUX: &str = include_str!("../../../fixtures/testing/fake-tmux.py");
