use std::collections::BTreeMap;

use thiserror::Error;

use super::*;

enum Seed {
    Upserted(Ordinal),
    Removed(Vec<ConversationItemId>),
    State,
    Reset,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProjectionError {
    #[error("revision gap: expected {expected}, got {actual}")]
    RevisionGap { expected: u64, actual: u64 },
    #[error("mutation generation does not match projection")]
    GenerationMismatch,
    #[error("truncate target does not exist")]
    UnknownTruncateTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StampedMutation {
    pub generation: GenerationId,
    pub revision: Revision,
    pub mutation: ConnectorMutation,
}
/// What one stamped mutation did to the projection, resolved against Hub-owned
/// ordinals so subscribers never re-derive placement.
///
/// `state` accompanies an item change only when the derived interaction state
/// moved with it; the gateway then emits both wire messages at the same
/// revision, so a resume replays exactly the same pair.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationEffect {
    Upserted {
        item: ConversationItem,
        state: Option<ConversationState>,
    },
    Removed {
        item_ids: Vec<ConversationItemId>,
        state: Option<ConversationState>,
    },
    StateChanged(ConversationState),
    /// The generation restarted; retained history before it is meaningless.
    Reset(ConversationState),
}

/// A stamped mutation plus its resolved effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedMutation {
    pub stamped: StampedMutation,
    pub effect: MutationEffect,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConversationSnapshot {
    pub generation: GenerationId,
    pub revision: Revision,
    pub operation_epoch: OperationEpoch,
    pub items: Vec<ConversationItem>,
    pub state: ConversationState,
    pub has_more_before: bool,
}

/// Pure in-memory reducer; Hub lifecycle and persistence are deliberately absent.
#[derive(Clone, Debug)]
pub struct Projection {
    generation: GenerationId,
    revision: Revision,
    operation_epoch: OperationEpoch,
    items: BTreeMap<Ordinal, ConversationItem>,
    ids: BTreeMap<ConversationItemId, Ordinal>,
    next_ordinal: Ordinal,
    state: ConversationState,
}

impl Projection {
    pub fn new(operation_epoch: OperationEpoch, state: ConversationState) -> Self {
        Self {
            generation: GenerationId::initial(),
            revision: Revision::zero(),
            operation_epoch,
            items: BTreeMap::new(),
            ids: BTreeMap::new(),
            next_ordinal: Ordinal::first(),
            state,
        }
    }
    pub fn generation(&self) -> GenerationId {
        self.generation
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn operation_epoch(&self) -> OperationEpoch {
        self.operation_epoch.clone()
    }
    pub fn apply_connector(
        &mut self,
        mutation: ConnectorMutation,
    ) -> Result<AppliedMutation, ProjectionError> {
        self.validate(&mutation)?;
        if !matches!(&mutation, ConnectorMutation::Rebuild { .. }) {
            self.revision = self.revision.next();
        }
        let effect = self.apply(mutation.clone());
        Ok(AppliedMutation {
            stamped: StampedMutation {
                generation: self.generation,
                revision: self.revision,
                mutation,
            },
            effect,
        })
    }
    pub fn apply_stamped(&mut self, stamped: StampedMutation) -> Result<(), ProjectionError> {
        if matches!(&stamped.mutation, ConnectorMutation::Rebuild { .. })
            && stamped.generation == self.generation.next()
            && stamped.revision == Revision::zero()
        {
            self.apply(stamped.mutation);
            return Ok(());
        }
        if stamped.generation != self.generation {
            return Err(ProjectionError::GenerationMismatch);
        }
        if stamped.revision == self.revision {
            return Ok(());
        }
        let expected = self.revision.next();
        if stamped.revision != expected {
            return Err(ProjectionError::RevisionGap {
                expected: expected.get(),
                actual: stamped.revision.get(),
            });
        }
        self.validate(&stamped.mutation)?;
        self.revision = stamped.revision;
        self.apply(stamped.mutation);
        Ok(())
    }
    /// Current interaction state without paying for an item page.
    pub fn state(&self) -> ConversationState {
        self.state.clone()
    }
    fn validate(&self, mutation: &ConnectorMutation) -> Result<(), ProjectionError> {
        if let ConnectorMutation::TruncateAfter(id) = mutation {
            if !self.ids.contains_key(id) {
                return Err(ProjectionError::UnknownTruncateTarget);
            }
        }
        Ok(())
    }
    fn apply(&mut self, mutation: ConnectorMutation) -> MutationEffect {
        let before = self.state.pending_request.clone();
        let seed = match mutation {
            ConnectorMutation::Upsert(observed) => Seed::Upserted(self.upsert(observed)),
            ConnectorMutation::TruncateAfter(id) => Seed::Removed(self.truncate_after(&id)),
            ConnectorMutation::State(state) => {
                self.state = state;
                Seed::State
            }
            ConnectorMutation::Rebuild { .. } => {
                self.generation = self.generation.next();
                self.revision = Revision::zero();
                self.items.clear();
                self.ids.clear();
                self.next_ordinal = Ordinal::first();
                Seed::Reset
            }
        };
        self.derive_pending_request();
        // An item change that moves the derived pending request is also a state
        // change; reporting only the item would leave the composer stale.
        let moved = (self.state.pending_request != before).then(|| self.state.clone());
        match seed {
            Seed::Upserted(ordinal) => MutationEffect::Upserted {
                item: self.items[&ordinal].clone(),
                state: moved,
            },
            Seed::Removed(item_ids) => MutationEffect::Removed {
                item_ids,
                state: moved,
            },
            Seed::State => MutationEffect::StateChanged(self.state.clone()),
            Seed::Reset => MutationEffect::Reset(self.state.clone()),
        }
    }
    fn upsert(&mut self, observed: ObservedItem) -> Ordinal {
        let ordinal = match self.ids.get(&observed.id).copied() {
            Some(ordinal) => ordinal,
            None => {
                let ordinal = self.next_ordinal;
                self.next_ordinal = self.next_ordinal.next();
                self.ids.insert(observed.id.clone(), ordinal);
                ordinal
            }
        };
        self.items.insert(
            ordinal,
            ConversationItem {
                id: observed.id,
                ordinal,
                created_at: observed.created_at,
                kind: observed.kind,
            },
        );
        ordinal
    }
    fn truncate_after(&mut self, id: &ConversationItemId) -> Vec<ConversationItemId> {
        let Some(ordinal) = self.ids.get(id).copied() else {
            return Vec::new();
        };
        let removed: Vec<_> = self
            .items
            .range((
                std::ops::Bound::Excluded(ordinal),
                std::ops::Bound::Unbounded,
            ))
            .map(|(_, item)| item.id.clone())
            .collect();
        self.items.retain(|key, _| *key <= ordinal);
        for id in &removed {
            self.ids.remove(id);
        }
        removed
    }
    fn derive_pending_request(&mut self) {
        self.state.pending_request = self.items.values().rev().find_map(|item| match &item.kind {
            ConversationItemKind::Request {
                request_id,
                status: RequestStatus::Pending,
                ..
            } => Some(request_id.clone()),
            _ => None,
        });
    }
    pub fn snapshot(&self, limit: usize) -> ConversationSnapshot {
        self.snapshot_bounded(limit, usize::MAX)
    }
    pub(crate) fn item(&self, id: &ConversationItemId) -> Option<&ConversationItem> {
        self.ids.get(id).and_then(|ordinal| self.items.get(ordinal))
    }
    pub fn snapshot_bounded(&self, limit: usize, max_bytes: usize) -> ConversationSnapshot {
        let len = self.items.len();
        let count_skip = len.saturating_sub(limit);
        let candidates: Vec<_> = self.items.values().skip(count_skip).cloned().collect();
        let envelope_bytes = serde_json::to_vec(&self.state)
            .map(|value| value.len())
            .unwrap_or(4096)
            .saturating_add(1024);
        let item_budget = max_bytes.saturating_sub(envelope_bytes);
        let mut used = 0usize;
        let mut keep = Vec::new();
        for item in candidates.into_iter().rev() {
            let bytes = serde_json::to_vec(&item)
                .map(|value| value.len())
                .unwrap_or(max_bytes);
            if !keep.is_empty() && used.saturating_add(bytes) > item_budget {
                break;
            }
            used = used.saturating_add(bytes);
            keep.push(item);
        }
        keep.reverse();
        let skip = len.saturating_sub(keep.len());
        ConversationSnapshot {
            generation: self.generation,
            revision: self.revision,
            operation_epoch: self.operation_epoch.clone(),
            items: keep,
            state: self.state.clone(),
            has_more_before: skip > 0,
        }
    }
    /// Restores the compact, Hub-owned cache representation.  Cache loading is
    /// deliberately all-or-nothing: callers rebuild from the connector when it
    /// is malformed rather than attempting a partial migration.
    pub(crate) fn from_snapshot(snapshot: ConversationSnapshot) -> Self {
        let mut ids = BTreeMap::new();
        let mut next = Ordinal::first();
        for item in &snapshot.items {
            ids.insert(item.id.clone(), item.ordinal);
            if item.ordinal >= next {
                next = item.ordinal.next();
            }
        }
        Self {
            generation: snapshot.generation,
            revision: snapshot.revision,
            operation_epoch: snapshot.operation_epoch,
            items: snapshot
                .items
                .into_iter()
                .map(|item| (item.ordinal, item))
                .collect(),
            ids,
            next_ordinal: next,
            state: snapshot.state,
        }
    }
    pub fn page_before(&self, before: Ordinal, limit: usize) -> (Vec<ConversationItem>, bool) {
        self.page_before_bounded(before, limit, usize::MAX)
    }
    pub fn page_before_bounded(
        &self,
        before: Ordinal,
        limit: usize,
        max_bytes: usize,
    ) -> (Vec<ConversationItem>, bool) {
        let all: Vec<_> = self
            .items
            .range(..before)
            .map(|(_, item)| item.clone())
            .collect();
        let count_start = all.len().saturating_sub(limit);
        let item_budget = max_bytes.saturating_sub(1024);
        let mut used = 0usize;
        let mut keep = Vec::new();
        for item in all[count_start..].iter().rev() {
            let bytes = serde_json::to_vec(item)
                .map(|value| value.len())
                .unwrap_or(max_bytes);
            if !keep.is_empty() && used.saturating_add(bytes) > item_budget {
                break;
            }
            used = used.saturating_add(bytes);
            keep.push(item.clone());
        }
        keep.reverse();
        let skipped = all.len().saturating_sub(keep.len());
        (keep, skipped > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ConversationState {
        ConversationState::starting(None)
    }
    fn message(id: &str, created_at: &str, status: MessageStatus) -> ConnectorMutation {
        ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native(id),
            created_at: created_at.into(),
            kind: ConversationItemKind::Message {
                role: MessageRole::Assistant,
                text: id.into(),
                status,
            },
        })
    }
    fn request(id: &str, status: RequestStatus) -> ConnectorMutation {
        ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native(id),
            created_at: "2026-01-01T00:00:00Z".into(),
            kind: ConversationItemKind::Request {
                request_id: id.into(),
                request_type: RequestType::Question,
                prompt: "continue?".into(),
                choices: vec!["yes".into()],
                status,
            },
        })
    }

    #[test]
    fn observation_order_beats_source_timestamp_and_tools_match_by_id() {
        let mut p = Projection::new(OperationEpoch::new("epoch"), state());
        p.apply_connector(message(
            "later-source",
            "2026-02-01",
            MessageStatus::Complete,
        ))
        .unwrap();
        p.apply_connector(message(
            "earlier-source",
            "2026-01-01",
            MessageStatus::Complete,
        ))
        .unwrap();
        p.apply_connector(ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native("tool-a"),
            created_at: "x".into(),
            kind: ConversationItemKind::Tool {
                name: "Bash".into(),
                summary: "a".into(),
                status: ToolStatus::Running,
                parent_message_id: None,
            },
        }))
        .unwrap();
        p.apply_connector(ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native("tool-b"),
            created_at: "x".into(),
            kind: ConversationItemKind::Tool {
                name: "Bash".into(),
                summary: "b".into(),
                status: ToolStatus::Running,
                parent_message_id: None,
            },
        }))
        .unwrap();
        p.apply_connector(ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native("tool-a"),
            created_at: "x".into(),
            kind: ConversationItemKind::Tool {
                name: "Bash".into(),
                summary: "done".into(),
                status: ToolStatus::Succeeded,
                parent_message_id: None,
            },
        }))
        .unwrap();
        let items = p.snapshot(100).items;
        assert_eq!(
            items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["later-source", "earlier-source", "tool-a", "tool-b"]
        );
        assert!(matches!(
            items[2].kind,
            ConversationItemKind::Tool {
                status: ToolStatus::Succeeded,
                ..
            }
        ));
        assert!(matches!(
            items[3].kind,
            ConversationItemKind::Tool {
                status: ToolStatus::Running,
                ..
            }
        ));
    }

    #[test]
    fn requests_are_derived_and_only_explicit_status_closes_them() {
        let mut p = Projection::new(OperationEpoch::new("epoch"), state());
        p.apply_connector(request("old", RequestStatus::Pending))
            .unwrap();
        p.apply_connector(request("new", RequestStatus::Pending))
            .unwrap();
        assert_eq!(p.snapshot(10).state.pending_request.as_deref(), Some("new"));
        p.apply_connector(request("new", RequestStatus::Resolved))
            .unwrap();
        assert_eq!(p.snapshot(10).state.pending_request.as_deref(), Some("old"));
        p.apply_connector(request("old", RequestStatus::Dismissed))
            .unwrap();
        assert_eq!(p.snapshot(10).state.pending_request, None);
    }

    #[test]
    fn truncate_keeps_generation_and_rebuild_changes_it() {
        let mut p = Projection::new(OperationEpoch::new("epoch"), state());
        p.apply_connector(message("one", "1", MessageStatus::Complete))
            .unwrap();
        p.apply_connector(message("two", "2", MessageStatus::Complete))
            .unwrap();
        let generation = p.generation();
        p.apply_connector(ConnectorMutation::TruncateAfter(
            ConversationItemId::native("one"),
        ))
        .unwrap();
        assert_eq!(p.generation(), generation);
        assert_eq!(p.snapshot(10).items.len(), 1);
        let reset = p
            .apply_connector(ConnectorMutation::Rebuild {
                reason: "source replaced".into(),
            })
            .unwrap();
        assert_ne!(p.generation(), generation);
        assert_eq!(reset.stamped.revision, Revision::zero());
        assert!(p.snapshot(10).items.is_empty());
    }

    #[test]
    fn stamped_revisions_are_idempotent_and_reject_gaps() {
        let mut source = Projection::new(OperationEpoch::new("epoch"), state());
        let one = source
            .apply_connector(message("one", "1", MessageStatus::Complete))
            .unwrap()
            .stamped;
        let two = source
            .apply_connector(message("two", "2", MessageStatus::Complete))
            .unwrap()
            .stamped;
        let mut replica = Projection::new(OperationEpoch::new("epoch"), state());
        assert_eq!(
            replica.apply_stamped(two.clone()),
            Err(ProjectionError::RevisionGap {
                expected: 1,
                actual: 2
            })
        );
        replica.apply_stamped(one.clone()).unwrap();
        replica.apply_stamped(one).unwrap();
        replica.apply_stamped(two).unwrap();
        assert_eq!(replica.snapshot(10).items.len(), 2);
    }

    #[test]
    fn deterministic_fallback_ids_are_connector_and_epoch_scoped() {
        assert_eq!(
            ConversationItemId::derived("claude", "a", "line-1"),
            ConversationItemId::derived("claude", "a", "line-1")
        );
        assert_ne!(
            ConversationItemId::derived("claude", "a", "line-1"),
            ConversationItemId::derived("codex", "a", "line-1")
        );
    }
}
