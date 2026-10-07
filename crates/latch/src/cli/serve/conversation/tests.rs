/// The turn outcome and command catalog cross the wire only when the
/// connector has them, so older clients and states read the same JSON.
#[test]
fn wire_state_carries_turn_outcome_and_commands_only_when_known() {
    let mut state = crate::conversation::ConversationState::starting(None);
    let bare = serde_json::to_value(super::wire_state(&state)).unwrap();
    assert!(bare.get("turnOutcome").is_none());
    assert!(bare.get("commands").is_none());

    state.turn_outcome = Some(crate::conversation::TurnOutcome::Refusal);
    state.commands = Some(vec![crate::conversation::AdvertisedCommand {
        name: "compact".into(),
        description: "Compacts the conversation.".into(),
        source: Some("builtin".into()),
    }]);
    let full = serde_json::to_value(super::wire_state(&state)).unwrap();
    assert_eq!(full["turnOutcome"], "refusal");
    assert_eq!(
        full["commands"],
        serde_json::json!([{ "name": "compact", "description": "Compacts the conversation.", "source": "builtin" }])
    );
    let decoded: super::super::contract::ConversationState = serde_json::from_value(bare).unwrap();
    assert_eq!(decoded.turn_outcome, None);
    assert_eq!(decoded.commands, None);
}

use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;

use super::*;
use crate::conversation::{
    ActionDescriptor, ApplyResult, CheckpointDelta, Connector, ConnectorIdentity,
    ConnectorMutation, ConversationItemId, ConversationItemKind, Detection, MessageRole,
    MessageStatus, ObservedItem, PollResult,
};
use crate::session::manifest::{SourceInfo, TerminalSize};
use crate::session::meta::{self, SessionMeta};
use crate::session::paths::{LatchHome, SessionId};

const TOKEN: &str = "conversation-test-token";

/// A connector whose observations and action outcome the test controls, so
/// the socket can be exercised before any real agent adapter exists.
struct ScriptedConnector {
    pending: Arc<Mutex<VecDeque<ConnectorMutation>>>,
    applies: Arc<AtomicUsize>,
    enabled: bool,
}
impl Connector for ScriptedConnector {
    fn detect(&self) -> Detection {
        Detection::Supported(ConnectorIdentity {
            id: "scripted".into(),
            version: "1".into(),
        })
    }
    fn poll(&mut self, _budget: PollBudget) -> Result<PollResult> {
        let mutations = self
            .pending
            .lock()
            .expect("script poisoned")
            .drain(..)
            .collect();
        Ok(PollResult {
            mutations,
            checkpoint_delta: CheckpointDelta {
                source_offsets: Vec::new(),
                active_branch_delta: Vec::new(),
                connector_state: None,
            },
        })
    }
    fn actions(&self) -> Vec<ActionDescriptor> {
        [
            ACTION_SEND_MESSAGE,
            ACTION_RESOLVE_REQUEST,
            crate::conversation::ACTION_CANCEL_TURN,
        ]
        .into_iter()
        .map(|id| ActionDescriptor {
            id: id.to_owned(),
            required_grant: Grant::Interact,
            enabled: self.enabled,
            reason: (!self.enabled).then(|| "scripted refusal".to_owned()),
        })
        .collect()
    }
    fn apply(&mut self, action: ConnectorAction, _deadline: Duration) -> Result<ApplyResult> {
        self.applies.fetch_add(1, Ordering::SeqCst);
        Ok(ApplyResult::Accepted {
            correlation: (action.id != crate::conversation::ACTION_CANCEL_TURN)
                .then(|| ConversationItemId::native("submitted-1")),
        })
    }
    fn reconcile(
        &self,
        _outstanding: &[ConversationItemId],
        _observed: &[ConversationItemId],
    ) -> Vec<ConnectorMutation> {
        Vec::new()
    }
    fn checkpoint_snapshot(&self) -> Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

fn message(id: &str) -> ConnectorMutation {
    ConnectorMutation::Upsert(ObservedItem {
        id: ConversationItemId::native(id),
        created_at: "2026-08-20T00:00:00Z".into(),
        kind: ConversationItemKind::Message {
            role: MessageRole::Assistant,
            text: id.into(),
            status: MessageStatus::Complete,
        },
    })
}

struct Harness {
    _dir: tempfile::TempDir,
    address: SocketAddr,
    session: String,
    script: Arc<Mutex<VecDeque<ConnectorMutation>>>,
    applies: Arc<AtomicUsize>,
}

/// Boots the production router (token and grant middleware included) on a
/// real loopback port.
async fn harness(connector_enabled: bool) -> Harness {
    let dir = tempfile::tempdir().expect("temp home");
    let home = LatchHome::new(dir.path());
    home.ensure().expect("home");
    let id = SessionId::parse("ses_conversationtest").expect("session id");
    let paths = home.session(&id);
    paths.ensure().expect("session dir");
    meta::write_once(
        &paths,
        &SessionMeta {
            format_version: 1,
            id: id.as_str().to_owned(),
            name: "conversation".into(),
            title: None,
            cwd: dir.path().to_path_buf(),
            command_label: "claude".into(),
            harness: Some("claude-code".into()),
            created_at: "2026-08-20T00:00:00Z".into(),
            initial_size: TerminalSize::new(80, 24),
            source: SourceInfo {
                kind: "test".into(),
                external_run_id: None,
            },
        },
    )
    .expect("write meta");
    let token_file = dir.path().join("serve.token");
    std::fs::write(&token_file, TOKEN).expect("token");

    let script = Arc::new(Mutex::new(VecDeque::new()));
    let factory_script = script.clone();
    let applies = Arc::new(AtomicUsize::new(0));
    let factory_applies = applies.clone();
    let hub = crate::conversation::ConversationHub::with_connector_factory(
        dir.path().join("hub"),
        Arc::new(move |_| {
            Box::new(ScriptedConnector {
                pending: factory_script.clone(),
                applies: factory_applies.clone(),
                enabled: connector_enabled,
            })
        }),
    )
    .expect("hub");
    let app =
        super::super::http::test_router(home, token_file, hub, std::path::PathBuf::from("latch"));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Harness {
        _dir: dir,
        address,
        session: id.as_str().to_owned(),
        script,
        applies,
    }
}

/// A blocking client on its own thread, so the test drives a real
/// WebSocket rather than the handler function.
struct Client {
    socket: tungstenite::WebSocket<TcpStream>,
}
impl Client {
    fn open(harness: &Harness, query: &str, grant: &str) -> Self {
        let url = format!(
            "ws://{}/v2/sessions/{}/conversation{query}",
            harness.address, harness.session
        );
        let mut request = url.into_client_request().expect("request");
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("token header"),
        );
        request.headers_mut().insert(
            "x-latch-device-grant",
            HeaderValue::from_str(grant).expect("grant header"),
        );
        let stream = TcpStream::connect(harness.address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        let (socket, _) = tungstenite::client::client(request, stream).expect("handshake");
        Self { socket }
    }
    fn send(&mut self, value: Value) {
        self.socket
            .send(tungstenite::Message::Text(value.to_string().into()))
            .expect("send");
    }
    fn send_raw(&mut self, text: &str) {
        self.socket
            .send(tungstenite::Message::Text(text.into()))
            .expect("send");
    }
    fn next(&mut self) -> Value {
        loop {
            match self.socket.read().expect("read") {
                tungstenite::Message::Text(text) => {
                    return serde_json::from_str(&text).expect("json")
                }
                tungstenite::Message::Close(_) => return json!({"type": "closed"}),
                _ => continue,
            }
        }
    }
    /// Reads until a message of `kind` arrives, so live mutations cannot
    /// make an assertion flaky.
    fn next_of(&mut self, kind: &str) -> Value {
        for _ in 0..64 {
            let message = self.next();
            if message["type"] == kind {
                return message;
            }
        }
        panic!("never received {kind}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_socket_receives_a_snapshot_with_no_client_round_trip() {
    let harness = harness(true).await;
    let opened = tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "control");
        let first = client.next();
        (harness, first)
    })
    .await
    .expect("client");
    let (_harness, first) = opened;
    assert_eq!(first["type"], "snapshot");
    assert_eq!(first["reason"], "initial");
    assert_eq!(first["revision"], 0);
    assert!(first["items"].as_array().expect("items").is_empty());
    assert_eq!(first["state"]["connector"]["id"], "scripted");
}

#[tokio::test(flavor = "multi_thread")]
async fn observed_items_stream_and_history_pages_on_the_same_socket() {
    let harness = harness(true).await;
    for n in 0..20 {
        harness
            .script
            .lock()
            .expect("script")
            .push_back(message(&format!("m{n:02}")));
    }
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "control");
        assert_eq!(client.next()["type"], "snapshot");
        let mut seen = Vec::new();
        while seen.len() < 20 {
            let message = client.next();
            if message["type"] == "items_upserted" {
                for item in message["items"].as_array().expect("items") {
                    seen.push(item["ordinal"].as_u64().expect("ordinal"));
                }
            }
        }
        assert_eq!(seen, (1..=20).collect::<Vec<_>>());

        client.send(json!({
            "type": "history_request",
            "requestId": "h1",
            "beforeOrdinal": 10,
            "limit": 5,
        }));
        let page = client.next_of("history_page");
        assert_eq!(page["requestId"], "h1");
        let ordinals: Vec<u64> = page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["ordinal"].as_u64().expect("ordinal"))
            .collect();
        assert_eq!(ordinals, vec![5, 6, 7, 8, 9]);
        assert_eq!(page["hasMoreBefore"], true);
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn observe_only_device_reads_but_is_refused_send_and_resolve() {
    let harness = harness(true).await;
    harness
        .script
        .lock()
        .expect("script")
        .push_back(message("visible"));
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "observe");
        assert_eq!(client.next()["type"], "snapshot");
        let upserted = client.next_of("items_upserted");
        assert_eq!(upserted["items"][0]["id"], "visible");

        client.send(json!({
            "type": "send_message",
            "operationEpoch": "any",
            "operationId": "op-send",
            "text": "hello",
        }));
        let refused = client.next_of("operation_result");
        assert_eq!(refused["operationId"], "op-send");
        assert_eq!(refused["status"], "refused");
        assert!(refused["reason"]
            .as_str()
            .expect("reason")
            .contains("device grant"));

        client.send(json!({
            "type": "resolve_request",
            "operationEpoch": "any",
            "operationId": "op-resolve",
            "requestId": "r1",
            "choice": "yes",
        }));
        let refused = client.next_of("operation_result");
        assert_eq!(refused["operationId"], "op-resolve");
        assert_eq!(refused["status"], "refused");
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_answers_use_interact_and_deduplicated_receipts() {
    let harness = harness(true).await;
    harness
        .script
        .lock()
        .expect("script")
        .push_back(ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native("rich-request"),
            created_at: "2026-08-20T00:00:00Z".into(),
            kind: ConversationItemKind::Request {
                request_id: "r".into(),
                request_type: crate::conversation::RequestType::Question,
                prompt: "Question?\nWhy?".into(),
                choices: vec!["Red".into()],
                status: crate::conversation::RequestStatus::Pending,
                questions: vec![
                    crate::conversation::RequestQuestion {
                        question: "Question?".into(),
                        header: Some("Colors".into()),
                        multi_select: true,
                        options: vec![crate::conversation::QuestionOption {
                            label: "Red".into(),
                            description: "Warm".into(),
                        }],
                    },
                    crate::conversation::RequestQuestion {
                        question: "Why?".into(),
                        header: None,
                        multi_select: false,
                        options: vec![],
                    },
                ],
            },
        }));
    tokio::task::spawn_blocking(move || {
        let mut observe = Client::open(&harness, "", "observe");
        let snapshot = observe.next();
        let item = observe.next_of("items_upserted");
        let questions = &item["items"][0]["kind"]["questions"];
        assert_eq!(questions[0]["multiSelect"], true);
        assert_eq!(questions[0]["options"][0]["description"], "Warm");
        assert_eq!(questions[1]["question"], "Why?");
        observe.send(json!({"type": "resolve_request", "operationEpoch": snapshot["operationEpoch"], "operationId": "structured-observe", "requestId": "r", "answers": {"Question?": "Free text"}}));
        assert_eq!(observe.next_of("operation_result")["status"], "refused");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 0);
        let mut interact = Client::open(&harness, "", "interact");
        let snapshot = interact.next();
        let action = json!({"type": "resolve_request", "operationEpoch": snapshot["operationEpoch"], "operationId": "structured-interact", "requestId": "r", "answers": {"Question?": "Red, Blue", "Why?": "Free text"}});
        interact.send(action.clone());
        assert_eq!(interact.next_of("operation_result")["status"], "accepted");
        interact.send(action);
        assert_eq!(interact.next_of("operation_result")["status"], "accepted");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 1);
        interact.send(json!({"type": "resolve_request", "operationEpoch": snapshot["operationEpoch"], "operationId": "both", "requestId": "r", "choice": "Yes", "answers": {"Question?": "Yes"}}));
        assert_eq!(interact.next_of("error")["code"], "payload_too_large");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 1);
    }).await.expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_uses_the_interact_grant_and_operation_receipt_without_a_message_row() {
    let harness = harness(true).await;
    tokio::task::spawn_blocking(move || {
        let mut observe = Client::open(&harness, "", "observe");
        let snapshot = observe.next();
        observe.send(json!({ "type": "cancel_turn", "operationEpoch": snapshot["operationEpoch"], "operationId": "stop-observe" }));
        assert_eq!(observe.next_of("operation_result")["status"], "refused");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 0);
        let mut interact = Client::open(&harness, "", "interact");
        let snapshot = interact.next();
        let command = json!({ "type": "cancel_turn", "operationEpoch": snapshot["operationEpoch"], "operationId": "stop-interact" });
        interact.send(command.clone());
        assert_eq!(interact.next_of("operation_result")["status"], "accepted");
        interact.send(command);
        assert_eq!(interact.next_of("operation_result")["status"], "accepted");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 1);
    }).await.expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn interact_device_sends_and_receives_a_correlated_result() {
    let harness = harness(true).await;
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "interact");
        let snapshot = client.next();
        let epoch = snapshot["operationEpoch"]
            .as_str()
            .expect("epoch")
            .to_owned();

        client.send(json!({
            "type": "send_message",
            "operationEpoch": epoch,
            "operationId": "op-1",
            "text": "hello",
        }));
        let accepted = client.next_of("operation_result");
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(accepted["itemId"], "submitted-1");

        // Replaying the same operation id must not dispatch a second time.
        client.send(json!({
            "type": "send_message",
            "operationEpoch": epoch,
            "operationId": "op-1",
            "text": "hello",
        }));
        let replayed = client.next_of("operation_result");
        assert_eq!(replayed["status"], "accepted");
        assert_eq!(harness.applies.load(Ordering::SeqCst), 1);

        // A stale epoch is refused without touching the connector.
        client.send(json!({
            "type": "send_message",
            "operationEpoch": "op-stale",
            "operationId": "op-2",
            "text": "hello",
        }));
        let stale = client.next_of("operation_result");
        assert_eq!(stale["status"], "refused");
        assert!(stale["reason"]
            .as_str()
            .expect("reason")
            .contains("operation epoch"));
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unavailable_connector_refuses_an_authorized_send() {
    let harness = harness(false).await;
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "interact");
        let snapshot = client.next();
        let epoch = snapshot["operationEpoch"]
            .as_str()
            .expect("epoch")
            .to_owned();
        client.send(json!({
            "type": "send_message",
            "operationEpoch": epoch,
            "operationId": "op-1",
            "text": "hello",
        }));
        let refused = client.next_of("operation_result");
        assert_eq!(refused["status"], "refused");
        // Authorization passed; only availability refused it.
        assert_eq!(refused["reason"], "scripted refusal");
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_resumes_from_revision_and_a_stale_position_re_bases() {
    let harness = harness(true).await;
    for n in 0..3 {
        harness
            .script
            .lock()
            .expect("script")
            .push_back(message(&format!("m{n}")));
    }
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "control");
        let snapshot = client.next();
        let generation = snapshot["generation"]
            .as_str()
            .expect("generation")
            .to_owned();
        let epoch = snapshot["operationEpoch"]
            .as_str()
            .expect("epoch")
            .to_owned();
        let mut revision = 0;
        let mut seen = 0;
        while seen < 3 {
            let message = client.next();
            if message["type"] == "items_upserted" {
                revision = message["revision"].as_u64().expect("revision");
                seen += message["items"].as_array().expect("items").len();
            }
        }
        drop(client);

        // Resuming one revision back replays only what is missing.
        let mut resumed = Client::open(
            &harness,
            &format!(
                "?generation={generation}&afterRevision={}&operationEpoch={epoch}",
                revision - 1
            ),
            "control",
        );
        let first = resumed.next();
        assert_eq!(first["type"], "items_upserted");
        assert_eq!(first["revision"], revision);
        assert_eq!(first["items"][0]["id"], "m2");
        drop(resumed);

        // An exactly-current resume still receives a server-first frame;
        // it must not sit silent until the next agent mutation.
        let mut current = Client::open(
            &harness,
            &format!("?generation={generation}&afterRevision={revision}&operationEpoch={epoch}"),
            "control",
        );
        let first = current.next();
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["revision"], revision);
        drop(current);

        // A stale generation is re-based with a snapshot, not a close.
        let mut stale = Client::open(
            &harness,
            &format!("?generation=generation-99&afterRevision=1&operationEpoch={epoch}"),
            "control",
        );
        let first = stale.next();
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["reason"], "generation");
        stale.send(json!({
            "type": "history_request",
            "requestId": "h",
            "beforeOrdinal": 3,
            "limit": 2,
        }));
        assert_eq!(stale.next_of("history_page")["requestId"], "h");

        // A replaced operation epoch re-bases without changing generation.
        let mut epoch_mismatch = Client::open(
            &harness,
            &format!("?generation={generation}&afterRevision={revision}&operationEpoch=op-old"),
            "control",
        );
        let first = epoch_mismatch.next();
        assert_eq!(first["type"], "snapshot");
        assert_eq!(first["reason"], "operation_epoch");
        assert_eq!(first["generation"], generation.as_str());
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnecting_the_first_socket_does_not_stop_the_shared_observer() {
    let harness = harness(true).await;
    tokio::task::spawn_blocking(move || {
        let mut first = Client::open(&harness, "", "control");
        assert_eq!(first.next()["type"], "snapshot");
        let mut replacement = Client::open(&harness, "", "control");
        assert_eq!(replacement.next()["type"], "snapshot");

        // `first` owns the original observation task. The task belongs to
        // the session, though, and must survive while `replacement` is a
        // subscriber.
        drop(first);
        harness
            .script
            .lock()
            .expect("script")
            .push_back(message("after-reconnect"));
        let update = replacement.next_of("items_upserted");
        assert_eq!(update["items"][0]["id"], "after-reconnect");
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_and_oversized_frames_are_protocol_errors_not_closes() {
    let harness = harness(true).await;
    tokio::task::spawn_blocking(move || {
        let mut client = Client::open(&harness, "", "control");
        assert_eq!(client.next()["type"], "snapshot");

        client.send_raw("{not json");
        assert_eq!(client.next_of("error")["code"], "invalid_message");

        client.send(json!({"type": "unknown_message"}));
        assert_eq!(client.next_of("error")["code"], "invalid_message");

        client.send(json!({
            "type": "send_message",
            "operationEpoch": "e",
            "operationId": "op",
            "text": "x".repeat(MAX_TEXT + 1),
        }));
        assert_eq!(client.next_of("error")["code"], "payload_too_large");

        client.send(json!({
            "type": "history_request",
            "requestId": "h",
            "beforeOrdinal": 0,
            "limit": 5,
        }));
        assert_eq!(client.next_of("error")["code"], "invalid_message");

        // The socket is still usable after every rejection.
        client.send(json!({
            "type": "history_request",
            "requestId": "ok",
            "beforeOrdinal": 1,
            "limit": 5,
        }));
        assert_eq!(client.next_of("history_page")["requestId"], "ok");
    })
    .await
    .expect("client");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_session_closes_with_the_not_found_code() {
    let harness = harness(true).await;
    tokio::task::spawn_blocking(move || {
        let url = format!(
            "ws://{}/v2/sessions/ses_missing/conversation",
            harness.address
        );
        let mut request = url.into_client_request().expect("request");
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).expect("header"),
        );
        let stream = TcpStream::connect(harness.address).expect("connect");
        let (mut socket, _) = tungstenite::client::client(request, stream).expect("handshake");
        match socket.read().expect("read") {
            tungstenite::Message::Close(Some(frame)) => {
                assert_eq!(u16::from(frame.code), WS_CLOSE_SESSION_NOT_FOUND)
            }
            other => panic!("expected a close frame, got {other:?}"),
        }
    })
    .await
    .expect("client");
}
