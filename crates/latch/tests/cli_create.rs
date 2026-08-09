//! Session creation through the CLI.
//!
//! Bare `latch`, `latch shell`, and `latch run -- <cmd>` create a session and
//! attach. `latch create --manifest-file -` reads a launch manifest from stdin
//! so secrets never reach argv, `meta.json`, or the process listing — only a
//! redacted command label is stored. Built at M1 rather than M3 because M3's
//! Overlord provider uses this same path, and because retrofitting secret
//! handling later means a window where it was wrong.

mod support;

use latch::cli::create::{self, CreateOptions};
use latch::worker::manifest::{DisplayMetadata, LaunchManifest, TerminalSize};
use latch::worker::meta;
use latch::worker::paths::{SessionId, DIR_MODE};

use support::{
    create_manifest_json, latch_cmd, latch_cmd_with_stdin, mode_of, read_meta, stdout_json,
    worker_binary, Harness, SETTLE,
};

const STAY_ALIVE: &str = "echo ready; while :; do sleep 0.2; done";

#[test]
fn bare_latch_creates_a_session_directory_at_0700() {
    let harness = Harness::new();
    // Bare `latch` attaches; drive it as a short-lived create via the library
    // so the test does not need a controlling terminal. The binary path is
    // covered by the attach suite.
    let outcome = create::create_session(CreateOptions {
        home: harness.home().clone(),
        manifest: shellish(STAY_ALIVE),
        attach: false,
    })
    .expect("create should succeed once implemented");

    assert!(
        outcome.id.as_str().starts_with("ses_"),
        "session ids are time-ordered and prefixed: {}",
        outcome.id
    );
    assert_eq!(
        mode_of(outcome.paths.dir()),
        DIR_MODE,
        "each session directory is 0700"
    );
    assert!(
        outcome.paths.meta().exists(),
        "meta.json is written at create"
    );
}

#[test]
fn latch_shell_and_run_create_and_leave_a_live_socket() {
    let harness = Harness::new();

    for (name, argv) in [
        (
            "shell-session",
            vec!["/bin/sh".to_owned(), "-c".to_owned(), STAY_ALIVE.to_owned()],
        ),
        (
            "run-session",
            vec!["/bin/sh".to_owned(), "-c".to_owned(), STAY_ALIVE.to_owned()],
        ),
    ] {
        let mut manifest =
            LaunchManifest::new(argv, std::env::temp_dir(), TerminalSize::new(80, 24));
        manifest.display = DisplayMetadata {
            name: Some(name.to_owned()),
            ..DisplayMetadata::default()
        };

        let outcome = create::create_session(CreateOptions {
            home: harness.home().clone(),
            manifest,
            attach: false,
        })
        .expect("create should succeed once implemented");

        support::wait_for("the control socket to answer", SETTLE, || {
            std::os::unix::net::UnixStream::connect(outcome.paths.socket()).is_ok()
        });
    }
}

#[test]
fn meta_json_is_written_via_temp_file_plus_rename() {
    // The atomicity property is already pinned in worker_filesystem.rs. Here we
    // assert the CLI create path produces a complete document a reader can open
    // the moment create returns — which is what temp+rename buys.
    let harness = Harness::new();
    let outcome = create::create_session(CreateOptions {
        home: harness.home().clone(),
        manifest: shellish(STAY_ALIVE),
        attach: false,
    })
    .expect("create should succeed once implemented");

    let record = meta::read(&outcome.paths).expect("meta.json must be a complete document");
    assert_eq!(record.id, outcome.id.as_str());
    assert!(
        !record.command_label.is_empty(),
        "a redacted command label is always stored"
    );
}

#[test]
fn create_from_manifest_file_stdin_never_puts_secrets_on_argv_or_disk() {
    let harness = Harness::new();
    let secret = "sk-live-should-never-leak";
    let manifest = create_manifest_json(
        &[
            "/bin/sh",
            "-c",
            &format!("echo TOKEN={secret}; {STAY_ALIVE}"),
        ],
        &std::env::temp_dir(),
        120,
        36,
        Some("Authentication refactor"),
        Some("codex"),
    );

    let output = latch_cmd_with_stdin(
        &harness,
        &["create", "--manifest-file", "-", "--json"],
        &manifest,
    );
    let report = stdout_json(&output);

    let session = report
        .get("session")
        .unwrap_or_else(|| panic!("create --json must include session: {report}"));
    let id = session["id"].as_str().expect("session.id is a string");
    assert!(
        SessionId::parse(id).is_ok(),
        "create returns a well-formed session id: {id}"
    );
    assert_eq!(
        session["state"].as_str(),
        Some("running"),
        "a just-created session is running"
    );

    let meta = read_meta(&harness, id);
    let meta_text = meta.to_string();
    assert!(
        !meta_text.contains(secret),
        "launch secrets must not reach meta.json: {meta_text}"
    );
    assert_eq!(
        meta["command_label"], "codex",
        "only the redacted command label is stored"
    );
    assert_eq!(meta["title"], "Authentication refactor");

    let worker_cmdline = worker_argv_containing(id);
    assert!(
        !worker_cmdline.iter().any(|arg| arg.contains(secret)),
        "launch material must never appear in argv; saw {worker_cmdline:?}"
    );
}

#[test]
fn create_sanitizes_manifest_display_fields_at_ingest() {
    let harness = Harness::new();
    let hostile_title = "Refactor\u{1b}]0;pwned\u{7}\n\r\u{0}auth";
    let hostile_name = "backend\u{1b}X";
    let mut body = serde_json::from_slice::<serde_json::Value>(&create_manifest_json(
        &["/bin/sh", "-c", STAY_ALIVE],
        &std::env::temp_dir(),
        80,
        24,
        Some(hostile_title),
        None,
    ))
    .unwrap();
    body["display"]["name"] = serde_json::json!(hostile_name);
    let manifest = serde_json::to_vec(&body).unwrap();

    let output = latch_cmd_with_stdin(
        &harness,
        &["create", "--manifest-file", "-", "--json"],
        &manifest,
    );
    let report = stdout_json(&output);
    let id = report["session"]["id"].as_str().expect("session.id");

    let meta = read_meta(&harness, id);
    let title = meta["title"].as_str().expect("title stored");
    let name = meta["name"].as_str().expect("name stored");
    assert!(
        !title.chars().any(|c| c.is_control()),
        "title must be clean in storage, not only at render: {title:?}"
    );
    assert!(
        !name.chars().any(|c| c.is_control()),
        "name must be clean in storage: {name:?}"
    );
    assert!(
        title.contains("Refactor") && title.contains("auth"),
        "sanitizing must not destroy readable content: {title:?}"
    );
}

#[test]
fn create_with_json_does_not_attach() {
    // Overlord creates, then opens a viewer separately. `create --json` must
    // return the session id and leave the invoking process free.
    let harness = Harness::new();
    let manifest = create_manifest_json(
        &["/bin/sh", "-c", STAY_ALIVE],
        &std::env::temp_dir(),
        80,
        24,
        None,
        None,
    );
    let started = std::time::Instant::now();
    let output = latch_cmd_with_stdin(
        &harness,
        &["create", "--manifest-file", "-", "--json"],
        &manifest,
    );
    let elapsed = started.elapsed();
    assert!(
        output.status.success(),
        "create --json should exit 0; stderr={}",
        support::stderr_str(&output)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "create --json must not block in an attach loop; took {elapsed:?}"
    );
    let _ = stdout_json(&output);
}

#[test]
fn session_ids_are_unique_across_rapid_creates() {
    let harness = Harness::new();
    let mut ids = Vec::new();
    for _ in 0..8 {
        let outcome = create::create_session(CreateOptions {
            home: harness.home().clone(),
            manifest: shellish("true"),
            attach: false,
        })
        .expect("create should succeed once implemented");
        ids.push(outcome.id.to_string());
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        ids.len(),
        "rapid creates must not collide: {ids:?}"
    );
}

#[test]
fn clap_surface_accepts_name_and_title_and_create_json() {
    // Parsing must succeed even while the bodies are unimplemented — otherwise
    // the adoption commands the plan names cannot be typed.
    for args in [
        vec![
            "shell", "--name", "backend", "--title", "API work", "--help",
        ],
        vec!["run", "--name", "auth", "--help"],
        vec!["create", "--manifest-file", "-", "--json", "--help"],
    ] {
        let output = latch_cmd(
            &Harness::new(),
            &args.iter().map(|s| &**s).collect::<Vec<_>>(),
        );
        let err = support::stderr_str(&output);
        assert!(
            output.status.success()
                && !err.contains("unexpected argument")
                && !err.contains("unrecognized subcommand"),
            "clap must accept {:?}; stderr={err}",
            args
        );
    }
}

#[test]
fn help_mentions_create_and_shell() {
    let output = std::process::Command::new(worker_binary())
        .args(["--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("create"), "{help}");
    assert!(help.contains("shell"), "{help}");
}

fn shellish(script: &str) -> LaunchManifest {
    LaunchManifest::new(
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        std::env::temp_dir(),
        TerminalSize::new(80, 24),
    )
}

fn worker_argv_containing(session_id: &str) -> Vec<String> {
    // Best-effort: scan /proc for a latch worker whose cmdline mentions the
    // session. On macOS this may be empty; the meta.json assertion is the
    // hard guarantee. When /proc is available, argv leakage is caught here.
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let pid = entry.file_name();
        let Some(pid) = pid.to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let cmdline_path = format!("/proc/{pid}/cmdline");
        let Ok(bytes) = std::fs::read(&cmdline_path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        if text.contains("latch") && text.contains(session_id) {
            return text
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_owned())
                .collect();
        }
    }
    Vec::new()
}
