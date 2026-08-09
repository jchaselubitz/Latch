//! Filesystem: modes, atomic metadata, and what never reaches disk.
//!
//! Modes are asserted rather than inherited. The suite clears its umask first,
//! so a `0700` that only happens because the developer's shell was set that way
//! fails here instead of on somebody else's machine. The journal holds terminal
//! output and the socket carries live input to a process running as the owner;
//! both are privacy boundaries, not tidiness.
//!
//! `meta.json` is written once, by temp file plus rename. `latch list` with
//! thirty sessions open reads thirty of these while workers are still starting,
//! so "a reader can observe a half-written file" is not a theoretical race — it
//! is the ordinary path under the load this product is designed for.

mod support;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use latch::worker::manifest::{DisplayMetadata, LaunchManifest, SourceInfo, TerminalSize};
use latch::worker::meta::{self, MetaError, MAX_DISPLAY_CHARS};
use latch::worker::paths::{DIR_MODE, FILE_MODE, SOCKET_MODE};
use support::{dir_entries, mode_of, sh_manifest, Harness, SETTLE};

const ANNOUNCE_AND_WAIT: &str = "echo PID:$$; while :; do sleep 0.2; done";

#[test]
fn the_latch_root_and_sessions_directory_are_owner_only() {
    let harness = Harness::new();
    harness.home().ensure().expect("the root should be created");

    assert_eq!(
        mode_of(harness.home().root()),
        DIR_MODE,
        "~/.latch is 0700, and asserting it is the point — a permissive umask must not decide"
    );
    assert_eq!(
        mode_of(&harness.home().sessions_dir()),
        DIR_MODE,
        "the sessions directory is 0700"
    );
}

#[test]
fn an_existing_root_with_loose_modes_is_tightened() {
    let harness = Harness::new();
    std::fs::create_dir_all(harness.home().sessions_dir()).expect("cannot pre-create the root");
    for path in [
        harness.home().root().to_path_buf(),
        harness.home().sessions_dir(),
    ] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))
            .expect("cannot loosen the mode");
    }

    harness.home().ensure().expect("the root should be usable");

    assert_eq!(
        mode_of(harness.home().root()),
        DIR_MODE,
        "a root that already exists with the wrong mode is the case this rule exists for: \
         a restored backup, or a directory created before the rule did"
    );
    assert_eq!(mode_of(&harness.home().sessions_dir()), DIR_MODE);
}

#[test]
fn a_live_session_directory_socket_and_files_are_owner_only() {
    let harness = Harness::new();
    let session = harness.spawn_sh(ANNOUNCE_AND_WAIT);

    assert_eq!(
        mode_of(session.paths().dir()),
        DIR_MODE,
        "each session directory is 0700"
    );
    assert_eq!(
        mode_of(&session.paths().socket()),
        SOCKET_MODE,
        "the control socket is 0600: it carries live input to a process running as its owner"
    );
    assert_eq!(
        mode_of(&session.paths().meta()),
        FILE_MODE,
        "meta.json is 0600"
    );
    assert_eq!(
        mode_of(&session.paths().journal()),
        FILE_MODE,
        "the journal is 0600: it holds terminal output"
    );
}

#[test]
fn the_exit_record_is_owner_only() {
    let harness = Harness::new();
    let session = harness.spawn_sh("exit 0");
    session.wait_until_gone(SETTLE);

    assert_eq!(
        mode_of(&session.paths().exit()),
        FILE_MODE,
        "exit.json is 0600"
    );
}

#[test]
fn meta_json_is_written_exactly_once() {
    let harness = Harness::new();
    let paths = harness.home().session(&harness.next_session_id());
    paths
        .ensure()
        .expect("the session directory should be created");

    let manifest = sh_manifest("true", TerminalSize::new(200, 50));
    let first = meta::derive(
        "ses_test00000001",
        &manifest.launch,
        &manifest.display,
        "2026-08-06T12:00:00Z",
    );
    meta::write_once(&paths, &first).expect("the first write should succeed");

    let second = meta::derive(
        "ses_test00000001",
        &manifest.launch,
        &manifest.display,
        "2026-08-06T12:00:01Z",
    );
    let err = meta::write_once(&paths, &second)
        .expect_err("a second write must be refused, not silently applied");
    assert!(
        matches!(err, MetaError::AlreadyWritten { .. }),
        "two writers mean two workers believe they own one session; that is a condition to \
         report, not to resolve by overwriting: {err}"
    );

    let read_back = meta::read(&paths).expect("meta.json should be readable");
    assert_eq!(
        read_back.created_at, "2026-08-06T12:00:00Z",
        "the first write is the one that stands"
    );
}

#[test]
fn a_reader_never_observes_a_partial_meta_json() {
    let harness = Harness::new();
    let paths = harness.home().session(&harness.next_session_id());
    paths
        .ensure()
        .expect("the session directory should be created");

    // A title at the size bound makes the document large enough that a
    // write-in-place implementation would be caught mid-write by a reader
    // hammering the path.
    let mut manifest = sh_manifest("true", TerminalSize::new(200, 50));
    manifest.display = DisplayMetadata {
        name: Some("session".to_owned()),
        title: Some("t".repeat(MAX_DISPLAY_CHARS)),
        command_label: None,
        source: SourceInfo::default(),
    };
    let record = meta::derive(
        "ses_test00000001",
        &manifest.launch,
        &manifest.display,
        "2026-08-06T12:00:00Z",
    );

    let stop = Arc::new(AtomicBool::new(false));
    let reader_paths = paths.clone();
    let reader_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || {
        let mut seen_complete = false;
        while !reader_stop.load(Ordering::Relaxed) {
            match meta::read(&reader_paths) {
                Ok(_) => seen_complete = true,
                Err(MetaError::Io { .. }) => {}
                Err(other) => panic!(
                    "a reader observed a meta.json that was not a complete document: {other}"
                ),
            }
        }
        seen_complete
    });

    std::thread::sleep(Duration::from_millis(20));
    meta::write_once(&paths, &record).expect("the write should succeed");
    std::thread::sleep(Duration::from_millis(50));
    stop.store(true, Ordering::Relaxed);

    assert!(
        reader.join().expect("the reader thread should finish"),
        "the reader should have seen the finished document at least once"
    );

    assert_eq!(
        dir_entries(paths.dir()),
        vec!["meta.json".to_owned()],
        "the temp file used for the atomic write must not survive it"
    );
}

#[test]
fn meta_json_records_display_metadata_and_nothing_else() {
    let harness = Harness::new();

    let mut manifest = LaunchManifest::new(
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "echo starting; --token=hunter2-should-never-be-recorded; sleep 30".to_owned(),
        ],
        std::env::temp_dir(),
        TerminalSize::new(200, 50),
    );
    manifest.launch.env = BTreeMap::from([(
        "LATCH_TEST_SECRET".to_owned(),
        "swordfish-should-never-be-recorded".to_owned(),
    )]);
    manifest.display.title = Some("Authentication refactor".to_owned());

    let session = harness.spawn(manifest);
    let text = std::fs::read_to_string(session.paths().meta()).expect("meta.json is readable");

    for secret in [
        "hunter2-should-never-be-recorded",
        "swordfish-should-never-be-recorded",
        "LATCH_TEST_SECRET",
    ] {
        assert!(
            !text.contains(secret),
            "launch material must live only in worker memory; found {secret:?} in {text}"
        );
    }

    let record = meta::read(session.paths()).expect("meta.json should parse");
    assert_eq!(
        record.command_label, "sh",
        "the command is recorded as a redacted label, never as argv"
    );
    assert_eq!(record.title.as_deref(), Some("Authentication refactor"));
    assert_eq!(
        record.initial_size,
        TerminalSize::new(200, 50),
        "the size at spawn is display metadata and is bounded"
    );
    assert!(
        record.source.external_run_id.is_none(),
        "an external run id is opaque and absent unless a caller supplied one"
    );
}

#[test]
fn command_redaction_keeps_the_program_and_drops_every_argument() {
    assert_eq!(meta::redact_command(&["/bin/zsh".to_owned()]), "zsh");
    assert_eq!(
        meta::redact_command(&[
            "/usr/local/bin/codex".to_owned(),
            "--api-key".to_owned(),
            "sk-live-secret".to_owned(),
        ]),
        "codex",
        "arguments are where the secrets are, so none is preserved — not even a truncated one"
    );
    assert_eq!(
        meta::redact_command(&[]),
        "",
        "an empty argv has no label rather than a panic"
    );
}

#[test]
fn display_metadata_is_sanitized_where_it_arrives() {
    // A title flows from a caller into terminal title sequences, `latch list`,
    // and later into web and mobile clients. Sanitizing at render means every
    // future call site is a new chance to forget.
    let hostile = "Refactor\u{1b}]0;pwned\u{7}\n\r\u{0}auth";
    let clean = meta::sanitize_display(hostile);
    assert!(
        !clean.chars().any(|c| c.is_control()),
        "control characters must not survive ingest: {clean:?}"
    );
    assert!(
        clean.contains("Refactor") && clean.contains("auth"),
        "sanitizing must not destroy the readable content: {clean:?}"
    );

    let long = "x".repeat(MAX_DISPLAY_CHARS * 4);
    assert!(
        meta::sanitize_display(&long).chars().count() <= MAX_DISPLAY_CHARS,
        "an unbounded title is a denial of service against the user's own terminal"
    );
}

#[test]
fn a_hostile_title_never_reaches_the_session_directory_intact() {
    let harness = Harness::new();
    let mut manifest = sh_manifest(ANNOUNCE_AND_WAIT, TerminalSize::new(200, 50));
    manifest.display.title = Some("safe\u{1b}]0;pwned\u{7}".to_owned());

    let session = harness.spawn(manifest);
    let bytes = std::fs::read(session.paths().meta()).expect("meta.json is readable");

    assert!(
        !bytes.contains(&0x1b),
        "an escape character in a title is an injection into whatever renders it"
    );
}

#[test]
fn a_session_id_can_never_escape_the_sessions_directory() {
    use latch::worker::paths::SessionId;

    for hostile in ["../../etc", "ses_../..", "ses_a/b", "", "sess_1", "ses_"] {
        assert!(
            SessionId::parse(hostile).is_err(),
            "{hostile:?} must not be usable as a session id: an id is also a directory name"
        );
    }
    assert!(SessionId::parse("ses_01JABCDEF").is_ok());
}
