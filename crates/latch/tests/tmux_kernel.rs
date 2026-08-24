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
        self.create_session_from_source(
            CreateSession {
                shell,
                command_label: "redacted",
            },
            "test",
        )
    }

    fn create_session_from_source(&self, request: CreateSession<'_>, source_kind: &str) -> Value {
        self.create_from_manifest(json!({
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
                "source": {"kind": source_kind, "external_run_id": "run-1"}
            }
        }))
    }

    /// Launches a session whose argv[0] basename is `program`, which is the
    /// only input the persisted harness marker is derived from. `create`
    /// cannot express this: it always launches through `/bin/sh`, and no
    /// harness matches that.
    fn create_program(&self, program: &str) -> Value {
        let path = self._temp.path().join(program);
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write launch program");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod program");
        self.create_from_manifest(json!({
            "format_version": 1,
            "launch": {
                "argv": [path],
                "cwd": self._temp.path(),
                "env": {},
                "inherit_env": true,
                "size": {"cols": 100, "rows": 30},
                "term": "xterm-256color"
            },
            "display": {
                "name": program,
                "title": program,
                "command_label": program,
                "source": {"kind": "test", "external_run_id": "run-1"}
            }
        }))
    }

    fn create_from_manifest(&self, manifest: Value) -> Value {
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

    fn attach_attempts(&self) -> u64 {
        let path = self.home.join("server.fake.json");
        let state: Value = serde_json::from_slice(&fs::read(path).expect("read fake tmux state"))
            .expect("parse fake tmux state");
        state
            .get("_attach_attempts")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    fn last_attach_was_raw(&self) -> bool {
        let path = self.home.join("server.fake.json");
        let state: Value = serde_json::from_slice(&fs::read(path).expect("read fake tmux state"))
            .expect("parse fake tmux state");
        state
            .get("_last_attach_raw")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn surface_attached(&self, id: &str) -> bool {
        self.json(&["inspect", id, "--json"])["surfaceAttached"] == true
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

    /// Stands in for `latch open`, which cannot run its macOS viewer here.
    fn announce_viewer_open(&self, id: &str) {
        let path = self.home.join("sessions").join(id).join("viewer-open.json");
        fs::write(
            path,
            json!({"viewer": "iterm", "behavior": "new-window", "at": "2026-08-18T12:00:00Z"})
                .to_string(),
        )
        .expect("write viewer marker");
    }

    fn launch_timings(&self, id: &str) -> Vec<Value> {
        self.json(&["inspect", id, "--json"])["launch_timings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn patch_state(&self, id: &str, patch: impl FnOnce(&mut Value)) {
        let path = self.home.join("server.fake.json");
        let mut state: Value =
            serde_json::from_slice(&fs::read(&path).expect("read fake tmux state"))
                .expect("parse fake tmux state");
        patch(&mut state[id]);
        fs::write(path, serde_json::to_vec(&state).unwrap()).expect("write fake tmux state");
    }
}

#[test]
fn overlord_launch_waits_until_the_first_viewer_is_attached() {
    let harness = Harness::new();
    let started = harness._temp.path().join("agent-started");
    let command = format!("touch {}; sleep 30", started.display());
    let created = harness.create_session_from_source(
        CreateSession {
            shell: &command,
            command_label: "codex",
        },
        "overlord",
    );
    let id = created["session"]["id"].as_str().unwrap();

    thread::sleep(Duration::from_millis(100));
    assert!(
        !started.exists(),
        "the hosted command started before a viewer attached"
    );

    let output = harness.command().args(["attach", id]).output().unwrap();
    assert_success(&output);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(
            Instant::now() < deadline,
            "the hosted command did not start after the viewer attached"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// A viewer that is slower to appear than the unannounced grace must still be
/// waited for: `latch open` stamps the marker before it asks for a window, and
/// the launcher has to read that as "a terminal is coming" rather than starting
/// the agent into a pane nobody is watching yet.
#[test]
fn an_announced_viewer_open_extends_the_wait_past_the_unannounced_grace() {
    let harness = Harness::new();
    let started = harness._temp.path().join("agent-started");
    let command = format!("touch {}; sleep 30", started.display());
    let created = harness.create_session_from_source(
        CreateSession {
            shell: &command,
            command_label: "codex",
        },
        "overlord",
    );
    let id = created["session"]["id"].as_str().unwrap();
    harness.announce_viewer_open(id);

    // Comfortably past the unannounced grace, and still holding.
    thread::sleep(Duration::from_millis(4_500));
    assert!(
        !started.exists(),
        "the hosted command started while a viewer open was still in flight"
    );

    assert_success(&harness.command().args(["attach", id]).output().unwrap());
    let deadline = Instant::now() + Duration::from_secs(2);
    while !started.exists() {
        assert!(
            Instant::now() < deadline,
            "the hosted command did not start after the viewer attached"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let timings = harness.launch_timings(id);
    let wait = timings
        .iter()
        .find(|phase| phase["phase"] == "launch.first_viewer_wait")
        .expect("the launcher recorded its wait");
    assert_eq!(wait["outcome"], "attached");
    assert!(
        timings.iter().any(|phase| phase["phase"] == "create.total"),
        "create did not record its own phases: {timings:?}"
    );
}

/// Nothing opens a viewer for a background launch, so the gate has to give up
/// on its own — quickly, because every second of it is launch latency.
#[test]
fn a_launch_with_no_viewer_coming_starts_without_waiting_out_a_long_timeout() {
    let harness = Harness::new();
    let started = harness._temp.path().join("agent-started");
    let command = format!("touch {}; sleep 30", started.display());
    let created = harness.create_session_from_source(
        CreateSession {
            shell: &command,
            command_label: "codex",
        },
        "overlord",
    );
    let id = created["session"]["id"].as_str().unwrap();

    let deadline = Instant::now() + Duration::from_secs(8);
    while !started.exists() {
        assert!(
            Instant::now() < deadline,
            "an unannounced launch never started headlessly"
        );
        thread::sleep(Duration::from_millis(50));
    }

    let timings = harness.launch_timings(id);
    let wait = timings
        .iter()
        .find(|phase| phase["phase"] == "launch.first_viewer_wait")
        .expect("the launcher recorded its wait");
    assert_eq!(wait["outcome"], "headless");
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
    assert_eq!(inspect["surfaceAttached"], false);

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

/// A remote client routes a tap from the session list alone, so the list — not
/// the conversation socket — has to say whether a session has a connector.
/// Probing with the socket would steal the desk before anyone chose anything.
///
/// `null` and *absent* are different answers: null means this session is a
/// plain terminal, absent means the gateway predates the field. The field is
/// therefore serialized even when there is nothing to report.
#[test]
fn the_session_list_reports_whether_each_session_has_a_connector() {
    let harness = Harness::new();
    let shell = harness.create("exit 0");
    let shell_id = shell["session"]["id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let agent = harness.create_program("claude");
    let agent_id = agent["session"]["id"]
        .as_str()
        .expect("session id")
        .to_owned();

    let listed = harness.json(&["list", "--json"]);
    let sessions = listed["sessions"].as_array().expect("sessions");
    let row = |id: &str| -> &Value {
        sessions
            .iter()
            .find(|session| session["id"] == id)
            .unwrap_or_else(|| panic!("{id} is missing from the session list"))
    };

    assert!(
        row(&shell_id)
            .as_object()
            .expect("session row")
            .contains_key("connector"),
        "a shell must report an explicit null, not an omitted key"
    );
    assert_eq!(row(&shell_id)["connector"], Value::Null);
    assert_eq!(row(&agent_id)["connector"], "claude");

    // Inspecting one session must not contradict the list it came from.
    assert_eq!(
        harness.json(&["inspect", &shell_id, "--json"])["connector"],
        Value::Null
    );
    assert_eq!(
        harness.json(&["inspect", &agent_id, "--json"])["connector"],
        "claude"
    );
}

#[test]
fn local_attach_uses_the_exclusive_raw_surface_and_can_steal_back() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();

    for _ in 0..2 {
        let output = harness.command().args(["attach", id]).output().unwrap();
        assert_success(&output);
        assert!(harness.last_attach_was_raw());
        assert!(harness.surface_attached(id));
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
fn local_attach_fails_closed_when_the_complete_kernel_payload_is_not_installed() {
    let harness = Harness::new();
    let upstream = harness._temp.path().join("upstream-tmux");
    fs::write(
        &upstream,
        "#!/bin/sh\necho 'unknown option: R' >&2\nexit 2\n",
    )
    .expect("write upstream stand-in");
    fs::set_permissions(&upstream, fs::Permissions::from_mode(0o755))
        .expect("chmod upstream stand-in");

    let mut child = harness
        .command()
        .env("LATCH_TMUX_BIN", upstream)
        .args(["create", "--manifest-file", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn create with upstream stand-in");
    serde_json::to_writer(
        child.stdin.take().expect("create stdin"),
        &json!({
            "format_version": 1,
            "launch": {
                "argv": ["/bin/sh", "-c", "sleep 1"],
                "cwd": harness._temp.path(),
                "env": {},
                "inherit_env": true,
                "size": {"cols": 80, "rows": 24}
            },
            "display": {"source": {"kind": "test"}}
        }),
    )
    .expect("write manifest");
    let output = child.wait_with_output().expect("wait for rejected create");
    assert!(!output.status.success());
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostic.contains("complete Latch payload"),
        "{diagnostic}"
    );
    assert!(
        fs::read_dir(harness.home.join("sessions"))
            .expect("sessions directory")
            .next()
            .is_none(),
        "an unpatched kernel must be rejected before a session directory is created"
    );
}

#[test]
fn local_read_only_attach_is_not_a_supported_surface() {
    let harness = Harness::new();
    let output = harness
        .command()
        .args(["attach", "--read-only"])
        .output()
        .expect("run removed read-only attach option");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--read-only"));
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

    let output = harness
        .command()
        .args(["attach", "--retry", id])
        .output()
        .expect("attach retry");
    assert!(!output.status.success());
    assert_eq!(
        harness.attach_attempts(),
        1,
        "attach --retry retried a permanent failure"
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
        path: "/v2/sessions",
        token: None,
        origin: None,
        grant: None,
    });
    assert_eq!(unauth.status, 401);
    let wrong = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/sessions",
        token: Some("nope"),
        origin: None,
        grant: None,
    });
    assert_eq!(wrong.status, 401);
    let blocked = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/sessions",
        token: Some(&gateway.token),
        origin: Some("https://evil.example"),
        grant: None,
    });
    assert_eq!(blocked.status, 403);

    let listed = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/sessions",
        token: Some(&gateway.token),
        origin: None,
        grant: None,
    });
    assert_eq!(listed.status, 200);
    let body: Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(body["sessions"][0]["id"], id);

    let inspect = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}"),
        token: Some(&gateway.token),
        origin: None,
        grant: None,
    });
    assert_eq!(inspect.status, 200);
    let inspect_body: Value = serde_json::from_str(&inspect.body).unwrap();
    assert_eq!(inspect_body["id"], id);
    assert_eq!(inspect_body["surfaceAttached"], false);

    let missing = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/sessions/missing-session",
        token: Some(&gateway.token),
        origin: None,
        grant: None,
    });
    assert_eq!(missing.status, 404);
    let missing_body: Value = serde_json::from_str(&missing.body).unwrap();
    assert_eq!(missing_body["error"], "session not found");
    assert!(!missing.body.contains("no session named"));

    let gateway_caps = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/capabilities",
        token: Some(&gateway.token),
        origin: None,
        grant: None,
    });
    assert_eq!(gateway_caps.status, 200);
    let gateway_body: Value = serde_json::from_str(&gateway_caps.body).unwrap();
    assert_eq!(gateway_body["protocolVersion"], 2);
    assert_eq!(gateway_body["endpoints"]["conversation"], true);
    assert_eq!(gateway_body["operationRetentionSeconds"], 600);
    assert_eq!(gateway_body["endpoints"]["terminal"], true);

    // The whole binary, not just the router: one authenticated conversation
    // socket that speaks first and answers history without a second connection.
    let (mut conversation, response) = connect_conversation(&gateway.addr, id, &gateway.token);
    assert_eq!(
        response.status(),
        tungstenite::http::StatusCode::SWITCHING_PROTOCOLS
    );
    let first = read_conversation_message(&mut conversation);
    assert_eq!(first["type"], "snapshot");
    assert_eq!(first["reason"], "initial");
    assert_eq!(first["revision"], 0);
    conversation
        .send(tungstenite::Message::Text(
            r#"{"type":"history_request","requestId":"h","beforeOrdinal":1,"limit":10}"#.into(),
        ))
        .unwrap();
    let page = read_conversation_message(&mut conversation);
    assert_eq!(page["type"], "history_page");
    assert_eq!(page["requestId"], "h");
    conversation.close(None).ok();

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

/// The preview is the only terminal-shaped route an observing device may call,
/// and the reason is mechanical rather than a policy choice: `capture-pane` is
/// a query, so it never enters the exclusive surface. This asserts both halves
/// — that observe is accepted here where the terminal route refuses it, and
/// that the capture left the surface exactly as it found it.
#[test]
fn serve_preview_reads_the_pane_at_observe_without_taking_the_surface() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    harness.patch_state(id, |entry| {
        entry["screen"] = Value::String("plain pane".into());
        entry["styled_screen"] = Value::String("\u{1b}[32mgreen pane\u{1b}[0m".into());
    });
    let gateway = start_gateway(&harness);

    assert!(!harness.surface_attached(id));
    let preview = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/preview"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    assert_eq!(preview.status, 200);
    let body: Value = serde_json::from_str(&preview.body).unwrap();
    // Captured with `-e`, so the escapes survive and the same renderer that
    // paints the live stream can paint the still.
    assert_eq!(body["content"], "\u{1b}[32mgreen pane\u{1b}[0m");
    // The desk's own grid, which is what Phase 4 attaches at so the pane
    // never resizes.
    assert_eq!(body["cols"], 100);
    assert_eq!(body["rows"], 30);
    assert_eq!(body["alternateScreen"], false);
    assert_eq!(body["scrollbackLines"], 0);
    assert!(body["capturedAt"].as_str().unwrap().ends_with('Z'));

    // The capture's own trailing newline is not content. Left on, it scrolls
    // a still fed into a grid exactly `rows` tall up by one row and costs the
    // top line.
    harness.patch_state(id, |entry| {
        entry["styled_screen"] = Value::String("last row\n".into());
    });
    let trimmed = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/preview"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    let body: Value = serde_json::from_str(&trimmed.body).unwrap();
    assert_eq!(body["content"], "last row");

    // Nothing was stolen: the kernel still reports no raw surface. This reads
    // `#{client_flags}` through `inspect`, not `session_attached`, because
    // administrative tmux clients are clients too.
    assert!(!harness.surface_attached(id));

    // The same grant on the terminal route is refused, which is what makes the
    // preview's availability a statement about capture rather than about the
    // device.
    let refused = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/terminal"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    assert_eq!(refused.status, 403);

    let missing = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/sessions/missing-session/preview",
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    assert_eq!(missing.status, 404);

    let caps = http_get(HttpGet {
        addr: &gateway.addr,
        path: "/v2/capabilities",
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    let caps_body: Value = serde_json::from_str(&caps.body).unwrap();
    assert_eq!(caps_body["endpoints"]["preview"], true);
}

/// Scrollback is a primary-screen idea. A full-screen application overwrites
/// in place and keeps no history, so asking for lines above the viewport there
/// would only widen the capture over the same one screen.
#[test]
fn serve_preview_ignores_scrollback_while_the_alternate_screen_is_active() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    harness.patch_state(id, |entry| {
        entry["screen"] = Value::String("viewport".into());
        entry["history"] = Value::String("older-1\nolder-2".into());
    });
    let gateway = start_gateway(&harness);

    let on_primary = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/preview?scrollbackLines=2"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    assert_eq!(on_primary.status, 200);
    let body: Value = serde_json::from_str(&on_primary.body).unwrap();
    assert_eq!(body["scrollbackLines"], 2);
    assert_eq!(body["content"], "older-1\nolder-2\nviewport");

    harness.patch_state(id, |entry| {
        entry["alternate"] = Value::Bool(true);
    });
    let on_alternate = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/preview?scrollbackLines=2"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    assert_eq!(on_alternate.status, 200);
    let body: Value = serde_json::from_str(&on_alternate.body).unwrap();
    assert_eq!(body["alternateScreen"], true);
    assert_eq!(body["scrollbackLines"], 0);
    assert_eq!(body["content"], "viewport");

    // And a request past the cap is clamped rather than honored.
    let capped = http_get(HttpGet {
        addr: &gateway.addr,
        path: &format!("/v2/sessions/{id}/preview?scrollbackLines=5000"),
        token: Some(&gateway.token),
        origin: None,
        grant: Some("observe"),
    });
    let body: Value = serde_json::from_str(&capped.body).unwrap();
    assert_eq!(
        body["scrollbackLines"], 0,
        "alternate screen wins over the cap"
    );
}

#[test]
fn serve_terminal_relays_bytes_in_both_directions() {
    let harness = Harness::new();
    let created = harness.create("sleep 30");
    let id = created["session"]["id"].as_str().unwrap();
    let gateway = start_gateway_with(GatewayOptions {
        harness: &harness,
        echo_attach: true,
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
    assert_eq!(
        harness.json(&["inspect", id, "--json"])["surfaceAttached"],
        false
    );

    let (mut waiting, _) = connect_terminal(TerminalWsRequest {
        addr: &gateway.addr,
        session: id,
        token: &gateway.token,
        cols: None,
        rows: None,
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        harness.json(&["inspect", id, "--json"])["surfaceAttached"],
        false
    );
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
        min: 1,
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
}

fn start_gateway(harness: &Harness) -> Gateway {
    start_gateway_with(GatewayOptions {
        harness,
        echo_attach: false,
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

/// Opens the v2 conversation socket against a live `latch serve`.
fn connect_conversation(
    addr: &str,
    session: &str,
    token: &str,
) -> (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::http::Response<Option<Vec<u8>>>,
) {
    let http_request = tungstenite::http::Request::builder()
        .uri(format!("ws://{addr}/v2/sessions/{session}/conversation"))
        .header("Host", addr)
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
    tungstenite::connect(http_request).expect("conversation websocket")
}

/// Reads one conversation frame, ignoring control frames.
fn read_conversation_message(
    socket: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> Value {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
    }
    for _ in 0..32 {
        if let tungstenite::Message::Text(text) = socket.read().expect("conversation frame") {
            return serde_json::from_str(&text).expect("conversation json");
        }
    }
    panic!("no conversation text frame arrived");
}

fn connect_terminal(
    request: TerminalWsRequest<'_>,
) -> (
    tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
    tungstenite::http::Response<Option<Vec<u8>>>,
) {
    let mut uri = format!(
        "ws://{}/v2/sessions/{}/terminal",
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
        let attached = request.harness.json(&["inspect", request.id, "--json"])["surfaceAttached"]
            .as_bool()
            .unwrap_or(false);
        if attached {
            return 1;
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
    /// The grant the paired proxy would have stamped on this request. `None`
    /// is a bare loopback call, which the gateway treats as `control`.
    grant: Option<&'a str>,
}

struct HttpRequest<'a> {
    addr: &'a str,
    path: &'a str,
    token: Option<&'a str>,
    origin: Option<&'a str>,
    grant: Option<&'a str>,
}

fn http_get(request: HttpGet<'_>) -> HttpResponse {
    http_request(HttpRequest {
        addr: request.addr,
        path: request.path,
        token: request.token,
        origin: request.origin,
        grant: request.grant,
    })
}

fn http_request(request: HttpRequest<'_>) -> HttpResponse {
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
    if let Some(grant) = request.grant {
        message.push_str(&format!("x-latch-device-grant: {grant}\r\n"));
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

const FAKE_TMUX: &str = include_str!("../../../fixtures/testing/fake-tmux.py");
