//! The protocol-major-2 conversation WebSocket.
//!
//! One authenticated socket carries the first snapshot or resume batch, live
//! mutations, bounded history pages, and correlated operation results. The
//! server speaks first from the upgrade URL, so a cold open and a foreground
//! resume both cost zero client round trips before data arrives.
//!
//! This module is deliberately agent-neutral: it never names a connector, and
//! every authorization decision it forwards is re-checked inside the Hub,
//! because the paired proxy authorizes one upgrade and cannot see later frames.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::contract::{
    ConversationClientMessage, ConversationServerMessage, OperationResultStatus, SnapshotReason,
};
use super::routes::Grant;
use crate::conversation::{
    ConnectorAction, ConversationHub, ConversationId, MutationEffect, OperationEpoch,
    OperationOutcome, Ordinal, PollBudget, ResumePosition, RetainedMutation, Revision,
    SnapshotCause, SubscribeOutcome, SubscriberEvent, ACTION_RESOLVE_REQUEST, ACTION_SEND_MESSAGE,
    MAX_MESSAGE_TEXT_BYTES, SNAPSHOT_PAGE,
};
use crate::session::paths::LatchHome;

/// Application close code: the session id or name does not exist.
const WS_CLOSE_SESSION_NOT_FOUND: u16 = 4404;
/// Largest client frame accepted before the socket is closed. It leaves room
/// for the schema's one-mebibyte message text plus its JSON envelope.
const MAX_CLIENT_FRAME: usize = 24 * 1024;
/// Schema bounds, restated here because a frame is rejected before the Hub or
/// any connector sees it.
const MAX_TEXT: usize = MAX_MESSAGE_TEXT_BYTES;
const MAX_CHOICE: usize = 4_096;
const MAX_ID: usize = 256;
const MAX_HISTORY_LIMIT: u16 = 100;
/// How often the socket flushes queued Hub events.
const FLUSH_INTERVAL: Duration = Duration::from_millis(50);
/// How often the session's single observation loop asks its connector for work.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_BUDGET: PollBudget = PollBudget {
    max_records: 512,
    deadline: Duration::from_secs(5),
};
/// A connector action that has not answered by now is reported as ambiguous
/// rather than retried, because it may already have reached the agent.
const ACTION_DEADLINE: Duration = Duration::from_secs(10);

/// `generation`, `afterRevision`, and `operationEpoch` on the upgrade URL.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationQuery {
    pub generation: Option<String>,
    pub after_revision: Option<u64>,
    pub operation_epoch: Option<String>,
}

impl ConversationQuery {
    fn position(self) -> ResumePosition {
        ResumePosition {
            generation: self
                .generation
                .as_deref()
                .and_then(crate::conversation::GenerationId::from_wire),
            after_revision: self.after_revision.map(Revision::new),
            operation_epoch: self.operation_epoch.map(OperationEpoch::new),
        }
    }
}

/// Connection inputs for one conversation socket.
pub struct ConversationConnect {
    pub home: LatchHome,
    pub hub: ConversationHub,
    /// Session id or name from the URL.
    pub session: String,
    /// Grant the gateway proved for this upgrade. The Hub re-checks it per message.
    pub grant: Grant,
    pub query: ConversationQuery,
}

/// Serves one subscriber until the socket closes.
pub async fn run(mut socket: WebSocket, connect: ConversationConnect) {
    let ConversationConnect {
        home,
        hub,
        session,
        grant,
        query,
    } = connect;
    let Ok(resolved) = crate::cli::manage::resolve_existing(&home, &session) else {
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: WS_CLOSE_SESSION_NOT_FOUND,
                reason: "session not found".into(),
            })))
            .await;
        return;
    };
    let id = ConversationId::new(resolved.as_str());
    if let Err(error) = hub.ensure_watched(&id) {
        let _ = send(
            &mut socket,
            ConversationServerMessage::Error {
                code: "unavailable".into(),
                message: format!("conversation is unavailable: {error}"),
            },
        )
        .await;
        return;
    }
    let Some((subscriber, outcome)) = hub.subscribe_at(&id, grant, query.position()) else {
        let _ = send(
            &mut socket,
            ConversationServerMessage::Error {
                code: "unavailable".into(),
                message: "conversation is unavailable".into(),
            },
        )
        .await;
        return;
    };

    // The server speaks first. Nothing is expected from the client to get here.
    for message in first_messages(outcome) {
        if send(&mut socket, message).await.is_err() {
            hub.unsubscribe(&id, subscriber);
            return;
        }
    }

    let observation = spawn_observation(hub.clone(), id.clone());
    let (results, mut result_rx) = mpsc::channel::<ConversationServerMessage>(64);
    let mut flush = tokio::time::interval(FLUSH_INTERVAL);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = flush.tick() => {
                let mut failed = false;
                for event in hub.drain(&id, subscriber) {
                    for message in event_messages(event) {
                        if send(&mut socket, message).await.is_err() {
                            failed = true;
                            break;
                        }
                    }
                    if failed {
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
            Some(message) = result_rx.recv() => {
                if send(&mut socket, message).await.is_err() {
                    break;
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(frame)) = incoming else { break };
                let text = match frame {
                    Message::Text(text) => text,
                    Message::Binary(_) => {
                        let _ = send(&mut socket, protocol_error(
                            "invalid_message",
                            "conversation frames must be JSON text",
                        )).await;
                        continue;
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                if text.len() > MAX_CLIENT_FRAME {
                    let _ = send(&mut socket, protocol_error(
                        "payload_too_large",
                        "conversation frame exceeds the protocol bound",
                    )).await;
                    continue;
                }
                let Ok(parsed) = serde_json::from_str::<ConversationClientMessage>(&text) else {
                    let _ = send(&mut socket, protocol_error(
                        "invalid_message",
                        "frame is not a v2 conversation client message",
                    )).await;
                    continue;
                };
                if !handle(
                    &hub,
                    &id,
                    subscriber,
                    parsed,
                    &mut socket,
                    &results,
                )
                .await
                {
                    break;
                }
            }
        }
    }

    hub.unsubscribe(&id, subscriber);
    observation.abort();
}

/// Runs the session's single observation loop, shared by every subscriber.
///
/// Only one task per session ever claims it, so steady-state connector work is
/// independent of how many clients are attached.
fn spawn_observation(hub: ConversationHub, id: ConversationId) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !hub.claim_observation(&id) {
            return;
        }
        let mut ticker = tokio::time::interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if !hub.has_subscribers(&id) {
                break;
            }
            if hub.poll_once(id.clone(), POLL_BUDGET).await.is_err() {
                break;
            }
        }
        hub.release_observation(&id);
    })
}

/// Returns false when the socket should close.
async fn handle(
    hub: &ConversationHub,
    id: &ConversationId,
    subscriber: u64,
    message: ConversationClientMessage,
    socket: &mut WebSocket,
    results: &mpsc::Sender<ConversationServerMessage>,
) -> bool {
    match message {
        ConversationClientMessage::Resume {
            generation,
            after_revision,
        } => {
            let position = ResumePosition {
                generation: generation
                    .as_deref()
                    .and_then(crate::conversation::GenerationId::from_wire),
                after_revision: after_revision.map(Revision::new),
                operation_epoch: None,
            };
            let Some(outcome) = hub.resync(id, subscriber, position) else {
                return false;
            };
            for message in first_messages(outcome) {
                if send(socket, message).await.is_err() {
                    return false;
                }
            }
            true
        }
        ConversationClientMessage::HistoryRequest {
            request_id,
            before_ordinal,
            limit,
        } => {
            if request_id.len() > MAX_ID || before_ordinal == 0 || limit == 0 {
                return send(
                    socket,
                    protocol_error("invalid_message", "history request is out of bounds"),
                )
                .await
                .is_ok();
            }
            let limit = limit.min(MAX_HISTORY_LIMIT) as usize;
            let Some((items, has_more_before)) =
                hub.history(id, Ordinal::boundary(before_ordinal), limit)
            else {
                return false;
            };
            send(
                socket,
                ConversationServerMessage::HistoryPage {
                    request_id,
                    items: items.iter().map(wire_item).collect(),
                    has_more_before,
                },
            )
            .await
            .is_ok()
        }
        ConversationClientMessage::SendMessage {
            operation_epoch,
            operation_id,
            text,
        } => {
            if text.is_empty() || text.len() > MAX_TEXT || operation_id.len() > MAX_ID {
                return send(
                    socket,
                    protocol_error("payload_too_large", "send_message is out of bounds"),
                )
                .await
                .is_ok();
            }
            dispatch(
                hub,
                id,
                subscriber,
                operation_epoch,
                operation_id,
                ConnectorAction {
                    id: ACTION_SEND_MESSAGE.to_owned(),
                    payload: serde_json::json!({ "text": text }),
                },
                results,
            );
            true
        }
        ConversationClientMessage::ResolveRequest {
            operation_epoch,
            operation_id,
            request_id,
            choice,
        } => {
            if choice.is_empty()
                || choice.len() > MAX_CHOICE
                || request_id.len() > MAX_ID
                || operation_id.len() > MAX_ID
            {
                return send(
                    socket,
                    protocol_error("payload_too_large", "resolve_request is out of bounds"),
                )
                .await
                .is_ok();
            }
            dispatch(
                hub,
                id,
                subscriber,
                operation_epoch,
                operation_id,
                ConnectorAction {
                    id: ACTION_RESOLVE_REQUEST.to_owned(),
                    payload: serde_json::json!({
                        "requestId": request_id,
                        "choice": choice,
                    }),
                },
                results,
            );
            true
        }
    }
}

/// Runs one operation off the socket task so a blocked connector action cannot
/// delay this subscriber's fanout or its other frames.
#[allow(clippy::too_many_arguments)]
fn dispatch(
    hub: &ConversationHub,
    id: &ConversationId,
    subscriber: u64,
    epoch: String,
    operation_id: String,
    action: ConnectorAction,
    results: &mpsc::Sender<ConversationServerMessage>,
) {
    let hub = hub.clone();
    let id = id.clone();
    let results = results.clone();
    tokio::spawn(async move {
        let outcome = hub
            .dispatch_action(
                id,
                subscriber,
                OperationEpoch::new(epoch),
                operation_id.clone(),
                action,
                ACTION_DEADLINE,
            )
            .await;
        let message = match outcome {
            Ok(outcome) => operation_result(operation_id, outcome),
            // A failure to even record the attempt is ambiguous, never a retry
            // invitation: the connector may already have accepted it.
            Err(error) => ConversationServerMessage::OperationResult {
                operation_id,
                status: OperationResultStatus::Ambiguous,
                item_id: None,
                reason: Some(error.to_string()),
            },
        };
        let _ = results.send(message).await;
    });
}

fn operation_result(operation_id: String, outcome: OperationOutcome) -> ConversationServerMessage {
    match outcome {
        OperationOutcome::Accepted { correlation } => ConversationServerMessage::OperationResult {
            operation_id,
            status: OperationResultStatus::Accepted,
            item_id: correlation.map(|id| id.as_str().to_owned()),
            reason: None,
        },
        OperationOutcome::Refused { reason } => ConversationServerMessage::OperationResult {
            operation_id,
            status: OperationResultStatus::Refused,
            item_id: None,
            reason: Some(reason),
        },
        // `Started` reaching a client means the outcome was never observed.
        OperationOutcome::Started | OperationOutcome::Ambiguous => {
            ConversationServerMessage::OperationResult {
                operation_id,
                status: OperationResultStatus::Ambiguous,
                item_id: None,
                reason: Some("the operation may or may not have reached the agent".into()),
            }
        }
    }
}

fn first_messages(outcome: SubscribeOutcome) -> Vec<ConversationServerMessage> {
    match outcome {
        SubscribeOutcome::Snapshot { snapshot, cause } => {
            vec![wire_snapshot(snapshot, Some(wire_cause(cause)))]
        }
        SubscribeOutcome::Resumed(mutations) => mutations
            .into_iter()
            .flat_map(mutation_messages)
            .collect::<Vec<_>>(),
    }
}

fn event_messages(event: SubscriberEvent) -> Vec<ConversationServerMessage> {
    match event {
        SubscriberEvent::Mutation(mutation) => mutation_messages(mutation),
        SubscriberEvent::Snapshot(snapshot, cause) => {
            vec![wire_snapshot(snapshot, Some(wire_cause(cause)))]
        }
        // Tier-two overflow recovery: state without an item page, so a stalled
        // subscriber still gets a usable composer at a resumable position.
        SubscriberEvent::StateOnly {
            generation,
            revision,
            state,
        } => vec![ConversationServerMessage::StateChanged {
            generation: generation.as_wire(),
            revision: revision.get(),
            state: wire_state(&state),
        }],
    }
}

/// One revision can produce an item message and the state message it moved, in
/// that order. Both carry the same revision so a later resume replays the pair.
fn mutation_messages(mutation: RetainedMutation) -> Vec<ConversationServerMessage> {
    let generation = mutation.generation.as_wire();
    let revision = mutation.revision.get();
    let mut messages = Vec::new();
    let moved = match mutation.effect {
        MutationEffect::Upserted { item, state } => {
            messages.push(ConversationServerMessage::ItemsUpserted {
                generation: generation.clone(),
                revision,
                items: vec![wire_item(&item)],
            });
            state
        }
        MutationEffect::Removed { item_ids, state } => {
            messages.push(ConversationServerMessage::ItemsRemoved {
                generation: generation.clone(),
                revision,
                item_ids: item_ids.iter().map(|id| id.as_str().to_owned()).collect(),
            });
            state
        }
        MutationEffect::StateChanged(state) => Some(state),
        // A reset is delivered as a snapshot, never as a mutation.
        MutationEffect::Reset(_) => None,
    };
    if let Some(state) = moved {
        messages.push(ConversationServerMessage::StateChanged {
            generation,
            revision,
            state: wire_state(&state),
        });
    }
    messages
}

fn wire_snapshot(
    snapshot: crate::conversation::ConversationSnapshot,
    reason: Option<SnapshotReason>,
) -> ConversationServerMessage {
    ConversationServerMessage::Snapshot {
        generation: snapshot.generation.as_wire(),
        revision: snapshot.revision.get(),
        operation_epoch: snapshot.operation_epoch.as_str().to_owned(),
        items: snapshot
            .items
            .iter()
            .take(SNAPSHOT_PAGE)
            .map(wire_item)
            .collect(),
        state: wire_state(&snapshot.state),
        has_more_before: snapshot.has_more_before,
        reason,
    }
}

fn wire_cause(cause: SnapshotCause) -> SnapshotReason {
    match cause {
        SnapshotCause::Initial => SnapshotReason::Initial,
        SnapshotCause::Generation => SnapshotReason::Generation,
        SnapshotCause::OperationEpoch => SnapshotReason::OperationEpoch,
        SnapshotCause::Overflow => SnapshotReason::Overflow,
    }
}

fn wire_item(item: &crate::conversation::ConversationItem) -> super::contract::ConversationItem {
    use super::contract as wire;
    use crate::conversation as domain;
    let kind = match &item.kind {
        domain::ConversationItemKind::Message { role, text, status } => {
            wire::ConversationItemKind::Message {
                role: match role {
                    domain::MessageRole::User => wire::MessageRole::User,
                    domain::MessageRole::Assistant => wire::MessageRole::Assistant,
                },
                text: text.clone(),
                status: match status {
                    domain::MessageStatus::Submitted => wire::MessageStatus::Submitted,
                    domain::MessageStatus::Observed => wire::MessageStatus::Observed,
                    domain::MessageStatus::Partial => wire::MessageStatus::Partial,
                    domain::MessageStatus::Complete => wire::MessageStatus::Complete,
                    domain::MessageStatus::Failed => wire::MessageStatus::Failed,
                },
            }
        }
        domain::ConversationItemKind::Tool {
            name,
            summary,
            status,
            parent_message_id,
        } => wire::ConversationItemKind::Tool {
            name: name.clone(),
            summary: summary.clone(),
            status: match status {
                domain::ToolStatus::Running => wire::ToolStatus::Running,
                domain::ToolStatus::Succeeded => wire::ToolStatus::Succeeded,
                domain::ToolStatus::Failed => wire::ToolStatus::Failed,
            },
            parent_message_id: parent_message_id.as_ref().map(|id| id.as_str().to_owned()),
        },
        domain::ConversationItemKind::Request {
            request_id,
            request_type,
            prompt,
            choices,
            status,
        } => wire::ConversationItemKind::Request {
            request_id: request_id.clone(),
            request_type: match request_type {
                domain::RequestType::Permission => wire::RequestType::Permission,
                domain::RequestType::Question => wire::RequestType::Question,
            },
            prompt: prompt.clone(),
            choices: choices.clone(),
            status: match status {
                domain::RequestStatus::Pending => wire::RequestStatus::Pending,
                domain::RequestStatus::Resolved => wire::RequestStatus::Resolved,
                domain::RequestStatus::Dismissed => wire::RequestStatus::Dismissed,
            },
        },
    };
    wire::ConversationItem {
        id: item.id.as_str().to_owned(),
        ordinal: item.ordinal.get(),
        created_at: item.created_at.clone(),
        kind,
    }
}

fn wire_state(
    state: &crate::conversation::ConversationState,
) -> super::contract::ConversationState {
    use super::contract as wire;
    use crate::conversation as domain;
    wire::ConversationState {
        phase: match state.phase {
            domain::ConversationPhase::Starting => wire::ConversationPhase::Starting,
            domain::ConversationPhase::Idle => wire::ConversationPhase::Idle,
            domain::ConversationPhase::Working => wire::ConversationPhase::Working,
            domain::ConversationPhase::AwaitingInput => wire::ConversationPhase::AwaitingInput,
            domain::ConversationPhase::Exited => wire::ConversationPhase::Exited,
            domain::ConversationPhase::Unavailable => wire::ConversationPhase::Unavailable,
        },
        send_message: wire::OperationAvailability {
            enabled: state.send_message.enabled,
            reason: state.send_message.reason.clone(),
        },
        resolve_request: wire::OperationAvailability {
            enabled: state.resolve_request.enabled,
            reason: state.resolve_request.reason.clone(),
        },
        pending_request: state.pending_request.clone(),
        connector: state.connector.as_ref().map(|c| wire::ConnectorIdentity {
            id: c.id.clone(),
            version: c.version.clone(),
        }),
    }
}

fn protocol_error(code: &str, message: &str) -> ConversationServerMessage {
    ConversationServerMessage::Error {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

async fn send(socket: &mut WebSocket, message: ConversationServerMessage) -> Result<(), ()> {
    let payload = serde_json::to_string(&message).map_err(|_| ())?;
    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::{SocketAddr, TcpStream};
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
            [ACTION_SEND_MESSAGE, ACTION_RESOLVE_REQUEST]
                .into_iter()
                .map(|id| ActionDescriptor {
                    id: id.to_owned(),
                    required_grant: Grant::Interact,
                    enabled: self.enabled,
                    reason: (!self.enabled).then(|| "scripted refusal".to_owned()),
                })
                .collect()
        }
        fn apply(&mut self, _action: ConnectorAction, _deadline: Duration) -> Result<ApplyResult> {
            Ok(ApplyResult::Accepted {
                correlation: Some(ConversationItemId::native("submitted-1")),
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
        let hub = crate::conversation::ConversationHub::with_connector_factory(
            dir.path().join("hub"),
            Arc::new(move |_| {
                Box::new(ScriptedConnector {
                    pending: factory_script.clone(),
                    enabled: connector_enabled,
                })
            }),
        )
        .expect("hub");
        let app = super::super::http::test_router(
            home,
            token_file,
            hub,
            std::path::PathBuf::from("latch"),
        );
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
                &format!(
                    "?generation={generation}&afterRevision={revision}&operationEpoch={epoch}"
                ),
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
}
