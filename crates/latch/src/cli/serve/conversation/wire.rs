//! Domain snapshots, mutations, and operation outcomes mapped to the v2 wire contract.

use super::super::contract::{ConversationServerMessage, OperationResultStatus, SnapshotReason};
use crate::conversation::{
    MutationEffect, OperationOutcome, RetainedMutation, SnapshotCause, SubscribeOutcome,
    SubscriberEvent, SNAPSHOT_PAGE,
};

pub(super) fn operation_result(
    operation_id: String,
    outcome: OperationOutcome,
) -> ConversationServerMessage {
    match outcome {
        OperationOutcome::Queued { correlation } => ConversationServerMessage::OperationResult {
            operation_id,
            status: OperationResultStatus::Queued,
            item_id: correlation.map(|id| id.as_str().to_owned()),
            reason: None,
        },
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

pub(super) fn first_messages(outcome: SubscribeOutcome) -> Vec<ConversationServerMessage> {
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

pub(super) fn event_messages(event: SubscriberEvent) -> Vec<ConversationServerMessage> {
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

pub(super) fn wire_item(
    item: &crate::conversation::ConversationItem,
) -> super::super::contract::ConversationItem {
    use super::super::contract as wire;
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
                    domain::MessageStatus::Queued => wire::MessageStatus::Queued,
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
            questions,
            status,
        } => wire::ConversationItemKind::Request {
            request_id: request_id.clone(),
            request_type: match request_type {
                domain::RequestType::Permission => wire::RequestType::Permission,
                domain::RequestType::Question => wire::RequestType::Question,
            },
            prompt: prompt.clone(),
            choices: choices.clone(),
            questions: questions
                .iter()
                .map(|q| wire::RequestQuestion {
                    question: q.question.clone(),
                    header: q.header.clone(),
                    multi_select: q.multi_select,
                    options: q
                        .options
                        .iter()
                        .map(|o| wire::QuestionOption {
                            label: o.label.clone(),
                            description: o.description.clone(),
                        })
                        .collect(),
                })
                .collect(),
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

pub(super) fn wire_state(
    state: &crate::conversation::ConversationState,
) -> super::super::contract::ConversationState {
    use super::super::contract as wire;
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
        cancel_turn: wire::OperationAvailability {
            enabled: state.cancel_turn.enabled,
            reason: state.cancel_turn.reason.clone(),
        },
        pending_request: state.pending_request.clone(),
        connector: state.connector.as_ref().map(|c| wire::ConnectorIdentity {
            id: c.id.clone(),
            version: c.version.clone(),
        }),
        turn_outcome: state.turn_outcome.map(|outcome| match outcome {
            domain::TurnOutcome::Answer => wire::TurnOutcome::Answer,
            domain::TurnOutcome::Aborted => wire::TurnOutcome::Aborted,
            domain::TurnOutcome::Refusal => wire::TurnOutcome::Refusal,
            domain::TurnOutcome::Error => wire::TurnOutcome::Error,
        }),
        commands: state.commands.as_ref().map(|commands| {
            commands
                .iter()
                .map(|command| wire::AdvertisedCommand {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    source: command.source.clone(),
                })
                .collect()
        }),
    }
}
