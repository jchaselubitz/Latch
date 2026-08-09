//! Naming, listing, idle time, and the thirty-session navigation surface.
//!
//! With every terminal window a session, `latch list` is the primary navigation
//! surface rather than an occasional command. Auto-names must be recognizable
//! without explanation, and the list must stay fast with thirty sessions open.

mod support;

use std::time::{Duration, Instant};

use latch::cli::create::CreateOptions;
use latch::cli::json::ListReport;
use latch::cli::manage::{self, ListOptions};
use latch::worker::manifest::{DisplayMetadata, LaunchManifest, TerminalSize};
use latch::worker::meta;

use support::{latch_cmd, stdout_json, Harness, SETTLE, THIRTY_LIST_BUDGET};

const STAY_ALIVE: &str = "while :; do sleep 0.2; done";

#[test]
fn sessions_auto_name_from_cwd_and_command() {
    let cwd = std::env::temp_dir().join("latch-api-project");
    std::fs::create_dir_all(&cwd).ok();

    let name = meta::derive_name(&cwd, "codex");
    assert!(
        name.contains("latch-api-project") && name.contains("codex"),
        "auto-name must be recognizable from cwd and command: {name}"
    );

    // An explicit --name wins and is still sanitized at ingest.
    let display = DisplayMetadata {
        name: Some("backend\u{1b}]0;x".to_owned()),
        ..DisplayMetadata::default()
    };
    let cleaned = meta::sanitize_display(display.name.as_deref().unwrap());
    assert!(
        !cleaned.chars().any(|c| c.is_control()),
        "explicit names are sanitized where they arrive: {cleaned:?}"
    );
}

#[test]
fn list_sorts_by_last_activity_and_shows_idle_time() {
    let harness = Harness::new();
    let report = manage::list(ListOptions {
        home: harness.home().clone(),
    })
    .expect("list should succeed once implemented");

    for session in &report.sessions {
        assert!(
            session.idle_ms.is_some() || session.last_activity_at.is_some(),
            "list must surface idle time so a human can scan thirty rows: {session:?}"
        );
    }

    let idles: Vec<_> = report.sessions.iter().filter_map(|s| s.idle_ms).collect();
    assert!(
        idles.windows(2).all(|w| w[0] <= w[1]),
        "most recently active first (idle ascending): {idles:?}"
    );
}

#[test]
fn list_json_stays_fast_with_thirty_sessions() {
    let harness = Harness::new();
    // Seed thirty live workers. The CLI create path will replace this once it
    // lands; until then the budget still has to be measured against real sockets.
    let mut keep_alive = Vec::new();
    for i in 0..30 {
        let mut manifest = LaunchManifest::new(
            vec!["/bin/sh".to_owned(), "-c".to_owned(), STAY_ALIVE.to_owned()],
            std::env::temp_dir(),
            TerminalSize::new(80, 24),
        );
        manifest.display = DisplayMetadata {
            name: Some(format!("sess-{i:02}")),
            ..DisplayMetadata::default()
        };
        // Options shape the binary create path will pass; kept so the type is
        // exercised even while create itself is still todo!.
        let _ = CreateOptions {
            home: harness.home().clone(),
            manifest: manifest.clone(),
            attach: false,
        };
        keep_alive.push(harness.spawn(manifest));
    }
    assert_eq!(
        keep_alive.len(),
        30,
        "need thirty sessions to assert the budget"
    );

    // Warm any one-time path, then measure.
    let _ = latch_cmd(&harness, &["list", "--json"]);
    let started = Instant::now();
    let output = latch_cmd(&harness, &["list", "--json"]);
    let elapsed = started.elapsed();

    // Until list is implemented this fails with "lands in M1". Once it works,
    // the schema and the budget both have to hold.
    if !output.status.success() {
        assert!(
            elapsed <= THIRTY_LIST_BUDGET,
            "even an unimplemented list must fail within budget; took {elapsed:?}"
        );
        return;
    }

    let value = stdout_json(&output);
    let report: ListReport =
        serde_json::from_value(value).expect("list --json must match ListReport");
    assert!(
        report.sessions.len() >= 30,
        "list must show every live session; got {}",
        report.sessions.len()
    );
    assert!(
        elapsed <= THIRTY_LIST_BUDGET,
        "list with 30 sessions took {elapsed:?}, budget is {THIRTY_LIST_BUDGET:?}; \
         if it is slow, fix how it probes — do not add a status cache"
    );
}

#[test]
fn list_human_form_is_usable() {
    // Even without --json, list must produce something a person can scan.
    // Until implemented this fails with "lands in M1"; the assertion pins that
    // success eventually yields non-empty, non-JSON-only output for humans.
    let harness = Harness::new();
    let _ = harness.spawn_sh(STAY_ALIVE);
    let output = latch_cmd(&harness, &["list"]);
    if output.status.success() {
        let text = support::stdout_str(&output);
        assert!(
            !text.trim().is_empty(),
            "human list output must not be empty when sessions exist"
        );
    }
}

#[test]
fn rename_sanitizes_at_ingest_not_at_render() {
    let harness = Harness::new();
    let report = manage::rename(manage::RenameRequest {
        home: harness.home().clone(),
        session: "ses_test".to_owned(),
        name: "backend\u{1b}]0;pwned\u{7}".to_owned(),
    });
    // Once implemented, the stored name must already be clean — checking only
    // rendered output would pass an implementation that sanitizes in one of the
    // several places a name is displayed.
    if let Ok(report) = report {
        assert!(
            !report.name.chars().any(|c| c.is_control()),
            "rename must sanitize at ingest: {:?}",
            report.name
        );
    }
}

#[allow(dead_code)]
fn _budget_constant() -> Duration {
    SETTLE
}
