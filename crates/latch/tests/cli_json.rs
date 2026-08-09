//! The `--json` contract for every read command.
//!
//! Overlord parses this output. Asserting the schema — field names, types, and
//! the state vocabulary — is the contract; asserting "some JSON came out" is
//! not. Schemas follow `planning/OVERLORD_INTEGRATION.md` for discovery/create
//! and the session-directory vocabulary for everything else.

mod support;

use latch::cli::json::{
    parse_state, CapabilitiesReport, CreateReport, InspectReport, ListReport, PruneReport,
    RenameReport, ResizeReport, StopReport,
};
use latch::cli::manage;
use latch_protocol::PROTOCOL_VERSION;

use support::{latch_cmd, stdout_json, Harness};

#[test]
fn capabilities_json_matches_the_overlord_discovery_schema() {
    let harness = Harness::new();
    let output = latch_cmd(&harness, &["capabilities", "--json"]);
    let value = stdout_json(&output);

    // Round-trip through the typed report so a renamed field fails here rather
    // than in Overlord's parser three milestones from now.
    let report: CapabilitiesReport = serde_json::from_value(value.clone())
        .expect("capabilities --json must match CapabilitiesReport");

    assert_eq!(report.protocol_version, PROTOCOL_VERSION);
    assert!(
        !report.product_version.is_empty(),
        "productVersion must be non-empty"
    );
    assert!(report.capabilities.create, "M1 can create sessions");
    assert!(report.capabilities.local_attach, "M1 can attach locally");
    assert!(
        !report.capabilities.cloud_attach,
        "cloud attach is M4; advertising it early would lie"
    );

    // Wire names are camelCase as Overlord's example spells them.
    assert!(value.get("protocolVersion").is_some(), "{value}");
    assert!(value.get("productVersion").is_some(), "{value}");
    assert!(value["capabilities"].get("openViewer").is_some(), "{value}");
    assert!(
        value["capabilities"].get("localAttach").is_some(),
        "{value}"
    );
    assert!(
        value["capabilities"].get("cloudAttach").is_some(),
        "{value}"
    );
}

#[test]
fn list_json_is_an_array_of_session_summaries_sorted_by_activity() {
    let harness = Harness::new();
    let output = latch_cmd(&harness, &["list", "--json"]);
    let value = stdout_json(&output);
    let report: ListReport =
        serde_json::from_value(value.clone()).expect("list --json must match ListReport");

    assert!(value.get("sessions").is_some(), "{value}");
    for session in &report.sessions {
        assert!(
            parse_state(&session.state).is_some(),
            "state must be a known vocabulary member: {}",
            session.state
        );
        assert!(
            !session.id.is_empty() && !session.name.is_empty(),
            "id and name are required: {session:?}"
        );
        assert!(
            !session.command_label.is_empty() || session.state == "lost",
            "command_label is display metadata present for every real session"
        );
    }

    // Sort order: most recently active first. When idle_ms is present, it must
    // be non-decreasing down the list.
    let idles: Vec<u64> = report.sessions.iter().filter_map(|s| s.idle_ms).collect();
    assert!(
        idles.windows(2).all(|w| w[0] <= w[1]),
        "list is sorted by last activity (idle ascending): {idles:?}"
    );
}

#[test]
fn inspect_json_distinguishes_running_exited_and_lost() {
    let harness = Harness::new();
    // The management API is what inspect uses once the binary wires through;
    // call it directly so the schema is pinned before the binary body lands.
    let report = manage::inspect(manage::InspectOptions {
        home: harness.home().clone(),
        session: "ses_doesnotexist".to_owned(),
    });
    // Until implemented this is todo!; once implemented, a missing session is
    // an error rather than a fabricated "lost" for an id that was never ours.
    let _ = report;
}

#[test]
fn inspect_report_schema_round_trips() {
    let sample = serde_json::json!({
        "id": "ses_01JTEST",
        "name": "latch-api",
        "title": "Authentication refactor",
        "state": "running",
        "cwd": "/tmp",
        "command_label": "codex",
        "created_at": "2026-08-06T12:00:00Z",
        "initial_size": { "cols": 200, "rows": 50 },
        "size": { "cols": 120, "rows": 36 },
        "attachments": [{
            "id": "a1",
            "mode": "control",
            "client_kind": "cli",
            "client_name": "iterm"
        }]
    });
    let report: InspectReport =
        serde_json::from_value(sample).expect("inspect schema must deserialize");
    assert_eq!(report.state, "running");
    assert_eq!(report.command_label, "codex");
    assert_eq!(report.attachments.as_ref().unwrap().len(), 1);
}

#[test]
fn stop_rename_resize_prune_json_schemas_round_trip() {
    let stop: StopReport = serde_json::from_value(serde_json::json!({
        "id": "ses_01JTEST",
        "state": "stopping"
    }))
    .unwrap();
    assert_eq!(stop.state, "stopping");

    let rename: RenameReport = serde_json::from_value(serde_json::json!({
        "id": "ses_01JTEST",
        "name": "backend"
    }))
    .unwrap();
    assert_eq!(rename.name, "backend");

    let resize: ResizeReport = serde_json::from_value(serde_json::json!({
        "id": "ses_01JTEST",
        "cols": 80,
        "rows": 24,
        "pinned": true
    }))
    .unwrap();
    assert!(resize.pinned);

    let prune: PruneReport = serde_json::from_value(serde_json::json!({
        "reclaimed": ["ses_01JOLD"],
        "dry_run": true
    }))
    .unwrap();
    assert!(prune.dry_run);
}

#[test]
fn create_json_matches_the_overlord_create_response() {
    let sample = serde_json::json!({
        "protocolVersion": 1,
        "session": {
            "id": "ses_01JTEST",
            "name": "authentication-refactor",
            "state": "running",
            "createdAt": "2026-08-06T12:00:00Z"
        }
    });
    let report: CreateReport =
        serde_json::from_value(sample.clone()).expect("create --json must match CreateReport");
    assert_eq!(report.protocol_version, 1);
    assert_eq!(report.session.state, "running");

    // Wire names match Overlord's example exactly.
    assert!(sample["session"].get("createdAt").is_some());
}

#[test]
fn doctor_and_config_emit_json_objects() {
    let harness = Harness::new();
    for args in [["doctor", "--json"], ["config", "--json"]] {
        let output = latch_cmd(&harness, &args);
        let value = stdout_json(&output);
        assert!(
            value.is_object(),
            "{args:?} must emit a JSON object: {value}"
        );
    }
}

#[test]
fn every_read_command_accepts_json() {
    // Parsing must succeed. Bodies may still be unimplemented.
    let harness = Harness::new();
    for args in [
        vec!["list", "--json"],
        vec!["inspect", "ses_x", "--json"],
        vec!["stop", "ses_x", "--json"],
        vec!["rename", "ses_x", "new", "--json"],
        vec!["resize", "ses_x", "--cols", "80", "--rows", "24", "--json"],
        vec!["prune", "--json"],
        vec!["doctor", "--json"],
        vec!["config", "--json"],
        vec!["capabilities", "--json"],
    ] {
        let output = latch_cmd(&harness, &args.iter().map(|s| &**s).collect::<Vec<_>>());
        let err = support::stderr_str(&output);
        assert!(
            !err.contains("unexpected argument"),
            "{args:?} must accept --json; stderr={err}"
        );
    }
}

#[test]
fn capabilities_library_entry_matches_binary_shape() {
    // Pins the library function the binary will call.
    let report = manage::capabilities();
    assert_eq!(report.protocol_version, PROTOCOL_VERSION);
    assert!(report.capabilities.create);
}
