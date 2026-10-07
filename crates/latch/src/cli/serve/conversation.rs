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

mod wire;
#[cfg(test)]
use wire::wire_state;
use wire::{event_messages, first_messages, operation_result, wire_item};

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use serde::Deserialize;
use tokio::sync::mpsc;

use super::contract::{
    ConversationClientMessage, ConversationServerMessage, OperationResultStatus,
};
use super::routes::Grant;
use crate::conversation::{
    ConnectorAction, ConversationHub, ConversationId, OperationEpoch, Ordinal, PollBudget,
    ResumePosition, Revision, ACTION_RESOLVE_REQUEST, ACTION_SEND_MESSAGE, MAX_MESSAGE_TEXT_BYTES,
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
/// Safety resync for event-driven kernels. This catches authoritative JSONL or
/// hook writes that produced no terminal event; it does not capture a screen.
const EVENT_IDLE_RESYNC: Duration = Duration::from_secs(5);
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
    /// Opaque device the loopback proxy proved, when remote. Operation ids
    /// and receipts are scoped to it, and it marks the session as watched
    /// for attention notifications.
    pub device: Option<String>,
    /// Gateway-owned attention producer, when running inside `latch serve`.
    pub attention: Option<super::attention::AttentionWatcher>,
    pub query: ConversationQuery,
}

/// Serves one subscriber until the socket closes.
pub async fn run(mut socket: WebSocket, connect: ConversationConnect) {
    let ConversationConnect {
        home,
        hub,
        session,
        grant,
        device,
        attention,
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
    let Some((subscriber, outcome)) =
        hub.subscribe_device(&id, grant, device.clone(), query.position())
    else {
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

    // Opening a conversation from a paired device is what makes the session
    // watched: from here the gateway keeps observing it for attention
    // transitions even after this socket is gone.
    if let (Some(device), Some(attention)) = (device.as_deref(), attention.as_ref()) {
        attention.watch(id.as_str(), device);
    }

    // The server speaks first. Nothing is expected from the client to get here.
    for message in first_messages(outcome) {
        if send(&mut socket, message).await.is_err() {
            hub.unsubscribe(&id, subscriber);
            return;
        }
    }

    // The observation task belongs to the session actor, not this socket.
    // Dropping one subscriber must not stop updates for another subscriber
    // that reconnected or was already attached.
    let _observation = spawn_observation(hub.clone(), id.clone());
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
        loop {
            if !hub.has_subscribers(&id) {
                break;
            }
            if hub.poll_once(id.clone(), POLL_BUDGET).await.is_err() {
                break;
            }
            if hub
                .wait_for_activity_once(id.clone(), POLL_INTERVAL, EVENT_IDLE_RESYNC)
                .await
                .is_err()
            {
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
        ConversationClientMessage::CancelTurn {
            operation_epoch,
            operation_id,
        } => {
            if operation_id.is_empty() || operation_id.len() > MAX_ID {
                return send(
                    socket,
                    protocol_error("invalid_message", "cancel_turn is out of bounds"),
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
                    id: crate::conversation::ACTION_CANCEL_TURN.to_owned(),
                    payload: serde_json::json!({}),
                },
                results,
            );
            true
        }
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
        ConversationClientMessage::OperationStatus { operation_id } => {
            if operation_id.is_empty() || operation_id.len() > MAX_ID {
                return send(
                    socket,
                    protocol_error("invalid_message", "operation_status is out of bounds"),
                )
                .await
                .is_ok();
            }
            // A lookup never dispatches. An id this device did not submit,
            // or one no longer retained, is `unknown`: the client reviews it
            // rather than treating the gap as permission to send again.
            let message = match hub.operation_status(id, subscriber, &operation_id) {
                Some(outcome) => operation_result(operation_id, outcome),
                None => ConversationServerMessage::OperationResult {
                    operation_id,
                    status: OperationResultStatus::Unknown,
                    item_id: None,
                    reason: Some("no retained receipt for this operation".into()),
                },
            };
            send(socket, message).await.is_ok()
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
            answers,
        } => {
            if choice.is_some() == answers.is_some()
                || choice
                    .as_ref()
                    .is_some_and(|v| v.is_empty() || v.len() > MAX_CHOICE)
                || answers.as_ref().is_some_and(|a| {
                    a.is_empty()
                        || a.len() > 16
                        || a.iter().any(|(k, v)| {
                            k.is_empty()
                                || k.len() > MAX_CHOICE
                                || v.trim().is_empty()
                                || v.len() > MAX_CHOICE
                        })
                })
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
                    payload: {
                        let mut payload = serde_json::json!({ "requestId": request_id });
                        if let Some(choice) = choice {
                            payload["choice"] = choice.into();
                        }
                        if let Some(answers) = answers {
                            payload["answers"] = serde_json::to_value(answers).expect("answer map");
                        }
                        payload
                    },
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
mod tests;
