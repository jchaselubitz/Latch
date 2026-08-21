//! Host-local, bounded Conversation Hub.
//!
//! This module intentionally exposes no wire protocol.  The gateway in Phase 4
//! subscribes to this actor boundary; every state change remains serialized here.
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{
    ActionDescriptor, ApplyResult, Connector, ConnectorAction, ConversationCache, ConversationId,
    ConversationItem, ConversationItemId, ConversationSnapshot, ConversationState, Detection,
    GatewayLock, GenerationId, MutationEffect, OperationEpoch, Ordinal, PendingConnector,
    PollBudget, Projection, Revision,
};
use crate::cli::serve::routes::Grant;

pub const MAX_SUBSCRIBER_MESSAGES: usize = 128;
pub const MAX_SUBSCRIBER_BYTES: usize = 256 * 1024;
pub const MAX_OPERATION_RECORDS: usize = 512;
pub const OPERATION_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
/// Retained mutation history a reconnecting subscriber can replay instead of
/// paying for a snapshot. Exceeding either bound downgrades resume to snapshot.
pub const MAX_RETAINED_MUTATIONS: usize = 512;
pub const MAX_RETAINED_BYTES: usize = 512 * 1024;
/// Snapshot page size served on subscribe, resync, and overflow recovery.
pub const SNAPSHOT_PAGE: usize = 100;
/// Compact-cache page size. Larger than a wire page so a warm restart keeps
/// enough scrollback to answer the first history request from memory.
const CACHE_PAGE: usize = 1000;

/// Why the Hub had to send a whole snapshot instead of resuming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotCause {
    /// The subscriber supplied no usable resume position.
    Initial,
    /// The subscriber's generation is not the live one.
    Generation,
    /// Queued operations were minted against a replaced operation epoch.
    OperationEpoch,
    /// Retained history no longer covers the subscriber's revision, or its
    /// queue overflowed.
    Overflow,
}

/// A resolved, revisioned change that a subscriber can apply directly.
#[derive(Clone, Debug)]
pub struct RetainedMutation {
    pub generation: GenerationId,
    pub revision: Revision,
    pub effect: MutationEffect,
}

/// Position a client claims to already hold, from the upgrade URL or a
/// mid-connection `resume`.
#[derive(Clone, Debug, Default)]
pub struct ResumePosition {
    pub generation: Option<GenerationId>,
    pub after_revision: Option<Revision>,
    /// Checked only when the client actually holds queued operations.
    pub operation_epoch: Option<OperationEpoch>,
}

/// What the Hub decided to send first.
#[derive(Clone, Debug)]
pub enum SubscribeOutcome {
    Resumed(Vec<RetainedMutation>),
    Snapshot {
        snapshot: ConversationSnapshot,
        cause: SnapshotCause,
    },
}

/// Builds the connector for one watched session. Keeping it behind a factory is
/// what stops the gateway from ever naming an agent.
pub type ConnectorFactory = Arc<dyn Fn(&ConversationId) -> Box<dyn Connector> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum OperationOutcome {
    Started,
    Accepted {
        correlation: Option<ConversationItemId>,
    },
    Refused {
        reason: String,
    },
    Ambiguous,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: String,
    pub epoch: OperationEpoch,
    pub at_ms: u128,
    pub outcome: OperationOutcome,
}
#[derive(Clone, Debug)]
pub enum SubscriberEvent {
    Mutation(RetainedMutation),
    Snapshot(ConversationSnapshot, SnapshotCause),
    /// Tier-two overflow recovery: current state without an item page. It
    /// carries the live position so the client stays resumable.
    StateOnly {
        generation: GenerationId,
        revision: Revision,
        state: ConversationState,
    },
}

#[derive(Clone, Debug)]
struct Subscriber {
    grant: Grant,
    queue: VecDeque<SubscriberEvent>,
    bytes: usize,
    last_snapshot: Option<Instant>,
    overflowed: bool,
}
struct SessionActor {
    projection: Projection,
    connector: Arc<Mutex<Box<dyn Connector>>>,
    actions: Vec<ActionDescriptor>,
    cache: ConversationCache,
    subscribers: HashMap<u64, Subscriber>,
    operations: VecDeque<OperationRecord>,
    retained: VecDeque<RetainedMutation>,
    retained_bytes: usize,
    next_subscriber: u64,
    last_active: Instant,
    /// Set while one task owns this session's observation loop, so polling
    /// stays O(1) per session rather than per subscriber.
    observing: bool,
}

/// One process-local Hub.  `Mutex` is used only to serialize state transitions;
/// callers must invoke `poll`/`apply` from bounded worker tasks in production.
#[derive(Clone)]
pub struct ConversationHub {
    inner: Arc<Mutex<HubInner>>,
    connector_factory: ConnectorFactory,
    _lock: Arc<GatewayLock>,
}
struct HubInner {
    root: PathBuf,
    sessions: HashMap<ConversationId, SessionActor>,
}

impl ConversationHub {
    pub fn new(cache_root: impl Into<PathBuf>) -> Result<Self> {
        Self::with_connector_factory(cache_root, Arc::new(|_| Box::new(PendingConnector::new())))
    }
    /// Same Hub with an injected connector builder. Phase 2 and Phase 7 replace
    /// the default factory; nothing else in the gateway changes.
    pub fn with_connector_factory(
        cache_root: impl Into<PathBuf>,
        connector_factory: ConnectorFactory,
    ) -> Result<Self> {
        let root = cache_root.into();
        let lock = GatewayLock::acquire(&root)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(HubInner {
                root,
                sessions: HashMap::new(),
            })),
            connector_factory,
            _lock: Arc::new(lock),
        })
    }
    /// Starts (or reuses) the actor for a session using the configured factory.
    pub fn ensure_watched(&self, id: &ConversationId) -> Result<()> {
        if self
            .inner
            .lock()
            .expect("hub poisoned")
            .sessions
            .contains_key(id)
        {
            return Ok(());
        }
        let connector = (self.connector_factory)(id);
        let state = initial_state(connector.detect());
        self.watch(id.clone(), connector, state)
    }
    /// Starts one lazy actor for a session. Additional subscribers reuse it.
    pub fn watch(
        &self,
        id: ConversationId,
        connector: Box<dyn Connector>,
        state: ConversationState,
    ) -> Result<()> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        if hub.sessions.contains_key(&id) {
            return Ok(());
        }
        let cache = ConversationCache::new(&hub.root, id.as_str());
        let mut connector = connector;
        let restored: Result<Option<(Projection, VecDeque<OperationRecord>)>> = (|| {
            let Some((snapshot, operations, checkpoint, batches)) = cache.load()? else {
                return Ok(None);
            };
            connector.restore_checkpoint(&checkpoint)?;
            let mut projection = Projection::from_snapshot(snapshot);
            let mut operations = decode_operations(operations);
            for batch in batches {
                if let Some(delta) = batch.checkpoint_delta.as_ref() {
                    connector.apply_checkpoint_delta(delta)?;
                }
                for mutation in batch.mutations {
                    projection.apply_stamped(mutation)?;
                }
                operations.extend(decode_operations(batch.operation_records));
            }
            Ok(Some((projection, operations)))
        })();
        let (projection, operations) = match restored {
            Ok(Some(restored)) => restored,
            Ok(None) => (Projection::new(fresh_epoch(), state), VecDeque::new()),
            Err(_) => {
                cache.discard()?;
                (Projection::new(fresh_epoch(), state), VecDeque::new())
            }
        };
        let actions = connector.actions();
        // Establish a compact base before the first append-only batch.  Later
        // restarts replay only the bounded journal after this base.
        let persisted = operations
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        cache.compact(
            &projection.snapshot(CACHE_PAGE),
            &persisted,
            &connector.checkpoint_snapshot()?,
        )?;
        hub.sessions.insert(
            id,
            SessionActor {
                projection,
                connector: Arc::new(Mutex::new(connector)),
                actions,
                cache,
                subscribers: HashMap::new(),
                operations,
                retained: VecDeque::new(),
                retained_bytes: 0,
                next_subscriber: 1,
                last_active: Instant::now(),
                observing: false,
            },
        );
        Ok(())
    }
    pub fn subscribe(
        &self,
        id: &ConversationId,
        grant: Grant,
    ) -> Option<(u64, ConversationSnapshot)> {
        match self.subscribe_at(id, grant, ResumePosition::default())? {
            (key, SubscribeOutcome::Snapshot { snapshot, .. }) => Some((key, snapshot)),
            // A default position never resumes.
            (key, SubscribeOutcome::Resumed(_)) => {
                let hub = self.inner.lock().ok()?;
                Some((
                    key,
                    hub.sessions.get(id)?.projection.snapshot(SNAPSHOT_PAGE),
                ))
            }
        }
    }
    /// Registers a subscriber and decides, in the same critical section, what
    /// the server sends first. The client never has to ask.
    pub fn subscribe_at(
        &self,
        id: &ConversationId,
        grant: Grant,
        position: ResumePosition,
    ) -> Option<(u64, SubscribeOutcome)> {
        let mut hub = self.inner.lock().ok()?;
        let actor = hub.sessions.get_mut(id)?;
        let key = actor.next_subscriber;
        actor.next_subscriber += 1;
        actor.subscribers.insert(
            key,
            Subscriber {
                grant,
                queue: VecDeque::new(),
                bytes: 0,
                last_snapshot: None,
                overflowed: false,
            },
        );
        actor.last_active = Instant::now();
        Some((key, resolve_position(actor, &position)))
    }
    /// Mid-connection re-sync. It reuses the upgrade-time decision so a client
    /// cannot reach a state the initial handshake could not produce.
    pub fn resync(
        &self,
        id: &ConversationId,
        subscriber: u64,
        position: ResumePosition,
    ) -> Option<SubscribeOutcome> {
        let mut hub = self.inner.lock().ok()?;
        let actor = hub.sessions.get_mut(id)?;
        if !actor.subscribers.contains_key(&subscriber) {
            return None;
        }
        // A re-sync supersedes whatever the subscriber had queued.
        if let Some(sub) = actor.subscribers.get_mut(&subscriber) {
            sub.queue.clear();
            sub.bytes = 0;
            sub.overflowed = false;
        }
        actor.last_active = Instant::now();
        Some(resolve_position(actor, &position))
    }
    /// One bounded page of items older than `before`.
    pub fn history(
        &self,
        id: &ConversationId,
        before: Ordinal,
        limit: usize,
    ) -> Option<(Vec<ConversationItem>, bool)> {
        let hub = self.inner.lock().ok()?;
        Some(hub.sessions.get(id)?.projection.page_before(before, limit))
    }
    pub fn snapshot(&self, id: &ConversationId, limit: usize) -> Option<ConversationSnapshot> {
        let hub = self.inner.lock().ok()?;
        Some(hub.sessions.get(id)?.projection.snapshot(limit))
    }
    /// Grants exactly one caller the right to run this session's observation
    /// loop. Later subscribers share its output instead of polling the source.
    pub fn claim_observation(&self, id: &ConversationId) -> bool {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let Some(actor) = hub.sessions.get_mut(id) else {
            return false;
        };
        if actor.observing {
            return false;
        }
        actor.observing = true;
        true
    }
    pub fn release_observation(&self, id: &ConversationId) {
        if let Ok(mut hub) = self.inner.lock() {
            if let Some(actor) = hub.sessions.get_mut(id) {
                actor.observing = false;
            }
        }
    }
    pub fn has_subscribers(&self, id: &ConversationId) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|hub| hub.sessions.get(id).map(|a| !a.subscribers.is_empty()))
            .unwrap_or(false)
    }
    pub fn unsubscribe(&self, id: &ConversationId, subscriber: u64) {
        if let Ok(mut hub) = self.inner.lock() {
            if let Some(a) = hub.sessions.get_mut(id) {
                a.subscribers.remove(&subscriber);
                a.last_active = Instant::now();
            }
        }
    }
    /// Stops idle session actors after their warm interval. The compact cache is
    /// retained, so the next `watch` restores it before connector catch-up.
    pub fn evict_idle(&self, warm_for: Duration) -> Result<Vec<ConversationId>> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let now = Instant::now();
        let ids: Vec<_> = hub
            .sessions
            .iter()
            .filter(|(_, actor)| {
                actor.subscribers.is_empty() && now.duration_since(actor.last_active) >= warm_for
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            let actor = hub.sessions.get(id).expect("selected session exists");
            let operations = actor
                .operations
                .iter()
                .map(serde_json::to_value)
                .collect::<Result<Vec<_>, _>>()?;
            let checkpoint = actor
                .connector
                .lock()
                .map_err(|_| anyhow::anyhow!("connector worker poisoned"))?
                .checkpoint_snapshot()?;
            actor.cache.compact(
                &actor.projection.snapshot(CACHE_PAGE),
                &operations,
                &checkpoint,
            )?;
        }
        for id in &ids {
            hub.sessions.remove(id);
        }
        Ok(ids)
    }
    /// Applies immutable observation output after its worker finishes.
    pub fn apply_poll(
        &self,
        id: &ConversationId,
        mutations: Vec<super::ConnectorMutation>,
    ) -> Result<()> {
        self.apply_poll_with_delta(id, mutations, None)
    }
    fn apply_poll_with_delta(
        &self,
        id: &ConversationId,
        mutations: Vec<super::ConnectorMutation>,
        checkpoint_delta: Option<super::CheckpointDelta>,
    ) -> Result<()> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let actor = hub
            .sessions
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?;
        // One source poll is one durable transition batch. Appending the
        // checkpoint delta for every emitted item would turn a normal
        // multi-block Claude record into O(mutations²) journal work.
        let mut applied = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            applied.push(actor.projection.apply_connector(mutation)?);
        }
        if !applied.is_empty() || checkpoint_delta.is_some() {
            actor.cache.append(&super::CacheBatch {
                mutations: applied
                    .iter()
                    .map(|change| change.stamped.clone())
                    .collect(),
                checkpoint_delta,
                operation_records: vec![],
            })?;
            for change in applied {
                publish(actor, change);
            }
        }
        compact_if_needed(actor)?;
        Ok(())
    }
    /// Executes observation on a blocking worker.  The state lock is released
    /// before source I/O begins, so a hung source cannot stall fanout/actions.
    pub async fn poll_once(&self, id: ConversationId, budget: PollBudget) -> Result<()> {
        let connector = {
            let hub = self.inner.lock().expect("hub poisoned");
            hub.sessions
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?
                .connector
                .clone()
        };
        let deadline = budget.deadline;
        let task = tokio::task::spawn_blocking(move || {
            connector
                .lock()
                .map_err(|_| anyhow::anyhow!("connector worker poisoned"))?
                .poll(budget)
        });
        match tokio::time::timeout(deadline, task).await {
            Ok(Ok(Ok(result))) => {
                let checkpoint_delta =
                    (!result.checkpoint_delta.is_empty()).then_some(result.checkpoint_delta);
                self.apply_poll_with_delta(&id, result.mutations, checkpoint_delta)
            }
            Ok(Ok(Err(error))) => self.degrade(&id, format!("observation failed: {error}")),
            Ok(Err(_)) => self.degrade(&id, "observation worker stopped".into()),
            Err(_) => self.degrade(&id, "observation deadline exceeded".into()),
        }
    }
    /// Runs an authorized action on the one serialized connector worker.  A
    /// deadline after dispatch is deliberately ambiguous, never auto-retried.
    pub async fn dispatch_action(
        &self,
        id: ConversationId,
        subscriber: u64,
        epoch: OperationEpoch,
        operation_id: String,
        action: ConnectorAction,
        deadline: Duration,
    ) -> Result<OperationOutcome> {
        let begun = self.begin_action(&id, subscriber, &epoch, operation_id.clone(), &action)?;
        if begun != OperationOutcome::Started {
            return Ok(begun);
        }
        let connector = {
            let hub = self.inner.lock().expect("hub poisoned");
            hub.sessions
                .get(&id)
                .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?
                .connector
                .clone()
        };
        let task = tokio::task::spawn_blocking(move || {
            connector
                .lock()
                .map_err(|_| anyhow::anyhow!("connector worker poisoned"))?
                .apply(action)
        });
        let result = match tokio::time::timeout(deadline, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(anyhow::anyhow!("action worker stopped")),
            Err(_) => Err(anyhow::anyhow!("action deadline exceeded after dispatch")),
        };
        self.finish_action(&id, &operation_id, result)
    }
    fn degrade(&self, id: &ConversationId, reason: String) -> Result<()> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let actor = hub
            .sessions
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?;
        let mut state = actor.projection.snapshot(0).state;
        state.phase = super::ConversationPhase::Unavailable;
        state.send_message.enabled = false;
        state.send_message.reason = Some(reason);
        let applied = actor
            .projection
            .apply_connector(super::ConnectorMutation::State(state))?;
        actor.cache.append(&super::CacheBatch {
            mutations: vec![applied.stamped.clone()],
            checkpoint_delta: None,
            operation_records: vec![],
        })?;
        publish(actor, applied);
        Ok(())
    }
    /// Enforces grants and operation epochs before a connector can receive an action.
    /// The caller persists `Started` before dispatch; a restart turns it ambiguous.
    pub fn begin_action(
        &self,
        id: &ConversationId,
        subscriber: u64,
        epoch: &OperationEpoch,
        operation_id: String,
        action: &ConnectorAction,
    ) -> Result<OperationOutcome> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let actor = hub
            .sessions
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?;
        prune_operations(&mut actor.operations);
        if let Some(old) = actor.operations.iter().find(|r| r.id == operation_id) {
            return Ok(match &old.outcome {
                OperationOutcome::Started => OperationOutcome::Ambiguous,
                v => v.clone(),
            });
        }
        let grant = actor
            .subscribers
            .get(&subscriber)
            .ok_or_else(|| anyhow::anyhow!("unknown subscriber"))?
            .grant;
        let descriptor = actor
            .actions
            .iter()
            .find(|d| d.id == action.id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown action"))?;
        // Authorization is decided before availability so an observe-only
        // device is refused for the same reason whatever the connector's state.
        if !grant.permits(descriptor.required_grant) {
            return Ok(OperationOutcome::Refused {
                reason: "device grant does not permit this action".into(),
            });
        }
        if actor.projection.operation_epoch() != *epoch {
            return Ok(OperationOutcome::Refused {
                reason: "stale operation epoch; refresh before retrying".into(),
            });
        }
        if !descriptor.enabled {
            return Ok(OperationOutcome::Refused {
                reason: descriptor
                    .reason
                    .unwrap_or_else(|| "action unavailable".into()),
            });
        }
        let record = OperationRecord {
            id: operation_id,
            epoch: epoch.clone(),
            at_ms: now_ms(),
            outcome: OperationOutcome::Started,
        };
        actor.operations.push_back(record.clone());
        actor.cache.append(&super::CacheBatch {
            mutations: vec![],
            checkpoint_delta: None,
            operation_records: vec![serde_json::to_value(record)?],
        })?;
        Ok(OperationOutcome::Started)
    }
    /// Records a completed connector action. A hung/mutating worker calls this with Ambiguous.
    pub fn finish_action(
        &self,
        id: &ConversationId,
        operation_id: &str,
        result: Result<ApplyResult>,
    ) -> Result<OperationOutcome> {
        let mut hub = self.inner.lock().expect("hub poisoned");
        let actor = hub
            .sessions
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown conversation"))?;
        let outcome = match result {
            Ok(ApplyResult::Accepted { correlation }) => OperationOutcome::Accepted { correlation },
            Ok(ApplyResult::Refused { reason }) => OperationOutcome::Refused { reason },
            Err(_) => OperationOutcome::Ambiguous,
        };
        if let Some(record) = actor.operations.iter_mut().find(|r| r.id == operation_id) {
            record.outcome = outcome.clone();
            actor.cache.append(&super::CacheBatch {
                mutations: vec![],
                checkpoint_delta: None,
                operation_records: vec![serde_json::to_value(record)?],
            })?;
        }
        Ok(outcome)
    }
    pub fn drain(&self, id: &ConversationId, subscriber: u64) -> Vec<SubscriberEvent> {
        let mut hub = match self.inner.lock() {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let Some(actor) = hub.sessions.get_mut(id) else {
            return vec![];
        };
        let state = actor.projection.state();
        let generation = actor.projection.generation();
        let revision = actor.projection.revision();
        let snapshot = actor.projection.snapshot(SNAPSHOT_PAGE);
        let Some(sub) = actor.subscribers.get_mut(&subscriber) else {
            return vec![];
        };
        if sub.overflowed {
            let now = Instant::now();
            if sub
                .last_snapshot
                .map(|t| now.duration_since(t) < Duration::from_secs(5))
                .unwrap_or(false)
            {
                sub.queue.push_back(SubscriberEvent::StateOnly {
                    generation,
                    revision,
                    state,
                });
            } else {
                sub.queue
                    .push_back(SubscriberEvent::Snapshot(snapshot, SnapshotCause::Overflow));
                sub.last_snapshot = Some(now);
            }
            sub.overflowed = false;
        }
        sub.bytes = 0;
        sub.queue.drain(..).collect()
    }
}
/// Retains a resolved mutation for reconnect replay and fans it out.
///
/// A generation reset makes retained history meaningless, so it is dropped and
/// every live subscriber is re-based on a snapshot instead of a mutation.
fn publish(actor: &mut SessionActor, applied: super::AppliedMutation) {
    if let MutationEffect::Reset(_) = &applied.effect {
        actor.retained.clear();
        actor.retained_bytes = 0;
        let snapshot = actor.projection.snapshot(SNAPSHOT_PAGE);
        fanout(
            actor,
            SubscriberEvent::Snapshot(snapshot, SnapshotCause::Generation),
        );
        return;
    }
    let retained = RetainedMutation {
        generation: applied.stamped.generation,
        revision: applied.stamped.revision,
        effect: applied.effect,
    };
    let bytes = effect_bytes(&retained.effect);
    actor.retained.push_back(retained.clone());
    actor.retained_bytes += bytes;
    while actor.retained.len() > MAX_RETAINED_MUTATIONS || actor.retained_bytes > MAX_RETAINED_BYTES
    {
        let Some(dropped) = actor.retained.pop_front() else {
            break;
        };
        actor.retained_bytes = actor
            .retained_bytes
            .saturating_sub(effect_bytes(&dropped.effect));
    }
    fanout(actor, SubscriberEvent::Mutation(retained));
}

/// Decides what a subscriber at `position` must receive.
fn resolve_position(actor: &SessionActor, position: &ResumePosition) -> SubscribeOutcome {
    let snapshot = |cause| SubscribeOutcome::Snapshot {
        snapshot: actor.projection.snapshot(SNAPSHOT_PAGE),
        cause,
    };
    let (Some(generation), Some(after)) = (position.generation, position.after_revision) else {
        return snapshot(SnapshotCause::Initial);
    };
    if generation != actor.projection.generation() {
        return snapshot(SnapshotCause::Generation);
    }
    // A replaced epoch invalidates queued actions but is not a new generation,
    // so the client is re-based rather than reset.
    if position
        .operation_epoch
        .as_ref()
        .is_some_and(|epoch| *epoch != actor.projection.operation_epoch())
    {
        return snapshot(SnapshotCause::OperationEpoch);
    }
    let current = actor.projection.revision();
    if after > current {
        return snapshot(SnapshotCause::Generation);
    }
    if after == current {
        // The server must speak first even when the client is exactly current.
        // The v2 protocol has no separate resume-ack carrying operationEpoch,
        // so a bounded snapshot is the only complete acknowledgement.
        return snapshot(SnapshotCause::Initial);
    }
    let missing: Vec<_> = actor
        .retained
        .iter()
        .filter(|retained| retained.revision > after)
        .cloned()
        .collect();
    // Retention must cover the whole gap; a partial replay would silently drop
    // revisions the client can never ask for again.
    let covered = missing
        .first()
        .is_some_and(|first| first.revision == after.next());
    if !covered {
        return snapshot(SnapshotCause::Overflow);
    }
    SubscribeOutcome::Resumed(missing)
}

fn initial_state(detection: Detection) -> ConversationState {
    match detection {
        Detection::Supported(identity) => ConversationState::starting(Some(identity)),
        // A recognized connector without an authoritative source binding is
        // starting, not unavailable. The Hub keeps watching its hook sidecar
        // and transitions when SessionStart arrives.
        Detection::Pending { .. } => ConversationState::starting(None),
        Detection::Unsupported => ConversationState::unavailable(
            None,
            "this session has no supported conversation connector".into(),
        ),
    }
}

fn effect_bytes(effect: &MutationEffect) -> usize {
    match effect {
        MutationEffect::Upserted { item, .. } => {
            serde_json::to_vec(item).map(|v| v.len()).unwrap_or(1024)
        }
        MutationEffect::Removed { item_ids, .. } => 64 * item_ids.len().max(1),
        MutationEffect::StateChanged(state) | MutationEffect::Reset(state) => {
            serde_json::to_vec(state).map(|v| v.len()).unwrap_or(512)
        }
    }
}

fn fanout(actor: &mut SessionActor, event: SubscriberEvent) {
    let bytes = estimate(&event);
    for sub in actor.subscribers.values_mut() {
        // Once a subscriber has overflowed, its queued mutation stream is no
        // longer contiguous. Keep it empty until `drain` emits the bounded
        // recovery event; accepting later mutations here would put them ahead
        // of that recovery marker and expose an impossible revision sequence.
        if sub.overflowed {
            continue;
        }
        if sub.queue.len() >= MAX_SUBSCRIBER_MESSAGES || sub.bytes + bytes > MAX_SUBSCRIBER_BYTES {
            sub.queue.clear();
            sub.bytes = 0;
            sub.overflowed = true;
            continue;
        }
        sub.bytes += bytes;
        sub.queue.push_back(event.clone());
    }
}
fn estimate(event: &SubscriberEvent) -> usize {
    match event {
        SubscriberEvent::Mutation(v) => effect_bytes(&v.effect),
        SubscriberEvent::Snapshot(v, _) => serde_json::to_vec(v)
            .map(|v| v.len())
            .unwrap_or(MAX_SUBSCRIBER_BYTES),
        SubscriberEvent::StateOnly { state, .. } => {
            serde_json::to_vec(state).map(|v| v.len()).unwrap_or(1024)
        }
    }
}
fn compact_if_needed(actor: &mut SessionActor) -> Result<()> {
    if !actor.cache.journal_is_bounded()? {
        let records = actor
            .operations
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        let checkpoint = actor
            .connector
            .lock()
            .map_err(|_| anyhow::anyhow!("connector worker poisoned"))?
            .checkpoint_snapshot()?;
        actor.cache.compact(
            &actor.projection.snapshot(CACHE_PAGE),
            &records,
            &checkpoint,
        )?;
    }
    Ok(())
}
fn decode_operations(values: Vec<serde_json::Value>) -> VecDeque<OperationRecord> {
    let mut records: VecDeque<OperationRecord> = VecDeque::new();
    for record in values
        .into_iter()
        .filter_map(|value| serde_json::from_value::<OperationRecord>(value).ok())
    {
        if let Some(index) = records.iter().position(|previous| previous.id == record.id) {
            records[index] = record;
        } else {
            records.push_back(record);
        }
    }
    for record in &mut records {
        if record.outcome == OperationOutcome::Started {
            record.outcome = OperationOutcome::Ambiguous;
        }
    }
    records
}
fn prune_operations(records: &mut VecDeque<OperationRecord>) {
    let cutoff = now_ms().saturating_sub(OPERATION_RETENTION.as_millis());
    while records.front().map(|r| r.at_ms < cutoff).unwrap_or(false)
        || records.len() > MAX_OPERATION_RECORDS
    {
        records.pop_front();
    }
}
fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
fn fresh_epoch() -> OperationEpoch {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    OperationEpoch::new(format!(
        "op-{:x}-{nanos:x}-{:x}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::json;

    use crate::conversation::{
        ActionDescriptor, CheckpointDelta, ConnectorIdentity, ConnectorMutation, Detection,
        MessageRole, MessageStatus, ObservedItem, PollBudget, PollResult,
    };

    struct FakeConnector;
    impl Connector for FakeConnector {
        fn detect(&self) -> Detection {
            Detection::Supported(ConnectorIdentity {
                id: "fake".into(),
                version: "1".into(),
            })
        }
        fn poll(&mut self, _: PollBudget) -> Result<PollResult> {
            Ok(PollResult {
                mutations: vec![],
                checkpoint_delta: CheckpointDelta {
                    source_offsets: vec![],
                    active_branch_delta: vec![],
                    connector_state: None,
                },
            })
        }
        fn actions(&self) -> Vec<ActionDescriptor> {
            vec![ActionDescriptor {
                id: "send".into(),
                required_grant: Grant::Interact,
                enabled: true,
                reason: None,
            }]
        }
        fn apply(&mut self, _: ConnectorAction) -> Result<ApplyResult> {
            Ok(ApplyResult::Accepted { correlation: None })
        }
        fn reconcile(
            &self,
            _: &[ConversationItemId],
            _: &[ConversationItemId],
        ) -> Vec<ConnectorMutation> {
            vec![]
        }
        fn checkpoint_snapshot(&self) -> Result<Vec<u8>> {
            Ok(vec![])
        }
    }
    fn item(id: &str) -> ConnectorMutation {
        ConnectorMutation::Upsert(ObservedItem {
            id: ConversationItemId::native(id),
            created_at: "now".into(),
            kind: super::super::ConversationItemKind::Message {
                role: MessageRole::Assistant,
                text: id.into(),
                status: MessageStatus::Complete,
            },
        })
    }
    #[test]
    fn shared_actor_enforces_grants_epochs_and_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let hub = ConversationHub::new(temp.path()).unwrap();
        let id = ConversationId::new("ses_test");
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        let (observe, snapshot) = hub.subscribe(&id, Grant::Observe).unwrap();
        let (healthy, _) = hub.subscribe(&id, Grant::Interact).unwrap();
        let action = ConnectorAction {
            id: "send".into(),
            payload: json!({"text":"x"}),
        };
        assert!(matches!(
            hub.begin_action(
                &id,
                observe,
                &snapshot.operation_epoch,
                "op-observe".into(),
                &action
            )
            .unwrap(),
            OperationOutcome::Refused { .. }
        ));
        assert!(matches!(
            hub.begin_action(
                &id,
                healthy,
                &OperationEpoch::new("old"),
                "op-old".into(),
                &action
            )
            .unwrap(),
            OperationOutcome::Refused { .. }
        ));
        for n in 0..=MAX_SUBSCRIBER_MESSAGES {
            hub.apply_poll(&id, vec![item(&format!("m{n}"))]).unwrap();
        }
        assert!(matches!(
            hub.drain(&id, observe).as_slice(),
            [SubscriberEvent::Snapshot(_, SnapshotCause::Overflow)]
        ));
        assert!(!hub.drain(&id, healthy).is_empty());

        // Repeated pressure inside the recovery window degrades to state-only
        // instead of enqueueing another payload larger than the mutations.
        for n in 0..=MAX_SUBSCRIBER_MESSAGES {
            hub.apply_poll(&id, vec![item(&format!("again-{n}"))])
                .unwrap();
        }
        assert!(matches!(
            hub.drain(&id, observe).as_slice(),
            [SubscriberEvent::StateOnly { .. }]
        ));
        assert!(!hub.drain(&id, healthy).is_empty());
    }
    #[test]
    fn resume_replays_retained_mutations_and_re_bases_when_it_cannot() {
        let temp = tempfile::tempdir().unwrap();
        let hub = ConversationHub::new(temp.path()).unwrap();
        let id = ConversationId::new("ses_resume");
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        let (_, snapshot) = hub.subscribe(&id, Grant::Observe).unwrap();
        let generation = snapshot.generation;
        let epoch = snapshot.operation_epoch.clone();
        let total = MAX_RETAINED_MUTATIONS + 10;
        for n in 0..total {
            hub.apply_poll(&id, vec![item(&format!("m{n}"))]).unwrap();
        }
        let current = Revision::new(total as u64);

        // One revision behind, well inside retention: replay only the gap.
        let (_, outcome) = hub
            .subscribe_at(
                &id,
                Grant::Observe,
                ResumePosition {
                    generation: Some(generation),
                    after_revision: Some(Revision::new(current.get() - 2)),
                    operation_epoch: Some(epoch.clone()),
                },
            )
            .unwrap();
        let SubscribeOutcome::Resumed(missing) = outcome else {
            panic!("a fresh position inside retention must resume");
        };
        assert_eq!(missing.len(), 2);
        assert_eq!(missing[1].revision, current);

        // Older than retention: a snapshot, because a partial replay would
        // silently drop revisions the client can never request again.
        let cases = [
            (
                ResumePosition {
                    generation: Some(generation),
                    after_revision: Some(Revision::new(1)),
                    operation_epoch: Some(epoch.clone()),
                },
                SnapshotCause::Overflow,
            ),
            (
                ResumePosition {
                    generation: Some(generation.next()),
                    after_revision: Some(current),
                    operation_epoch: Some(epoch.clone()),
                },
                SnapshotCause::Generation,
            ),
            (
                ResumePosition {
                    generation: Some(generation),
                    after_revision: Some(current),
                    operation_epoch: Some(OperationEpoch::new("replaced")),
                },
                SnapshotCause::OperationEpoch,
            ),
            (
                ResumePosition {
                    generation: Some(generation),
                    after_revision: Some(current),
                    operation_epoch: Some(epoch.clone()),
                },
                SnapshotCause::Initial,
            ),
            (ResumePosition::default(), SnapshotCause::Initial),
        ];
        for (position, expected) in cases {
            let (_, outcome) = hub.subscribe_at(&id, Grant::Observe, position).unwrap();
            let SubscribeOutcome::Snapshot { snapshot, cause } = outcome else {
                panic!("expected a snapshot for {expected:?}");
            };
            assert_eq!(cause, expected);
            // An operation-epoch mismatch never changes the generation.
            assert_eq!(snapshot.generation, generation);
        }
    }

    #[test]
    fn only_one_task_ever_observes_a_session() {
        let temp = tempfile::tempdir().unwrap();
        let hub = ConversationHub::new(temp.path()).unwrap();
        let id = ConversationId::new("ses_observe");
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        hub.subscribe(&id, Grant::Observe).unwrap();
        hub.subscribe(&id, Grant::Observe).unwrap();
        assert!(hub.claim_observation(&id));
        assert!(
            !hub.claim_observation(&id),
            "a second subscriber must share"
        );
        hub.release_observation(&id);
        assert!(hub.claim_observation(&id));
    }

    #[tokio::test]
    async fn idle_poll_does_not_append_an_empty_checkpoint_batch() {
        let temp = tempfile::tempdir().unwrap();
        let hub = ConversationHub::new(temp.path()).unwrap();
        let id = ConversationId::new("ses_idle");
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        let journal = temp.path().join("conversations/ses_idle/transitions.jsonl");
        let before = std::fs::read(&journal).unwrap();
        hub.poll_once(
            id,
            PollBudget {
                max_records: 64,
                deadline: Duration::from_secs(1),
            },
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(journal).unwrap(), before);
    }

    #[test]
    fn restart_turns_started_operation_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let id = ConversationId::new("ses_restart");
        let epoch;
        {
            let hub = ConversationHub::new(temp.path()).unwrap();
            hub.watch(
                id.clone(),
                Box::new(FakeConnector),
                ConversationState::starting(None),
            )
            .unwrap();
            let (subscriber, snapshot) = hub.subscribe(&id, Grant::Interact).unwrap();
            epoch = snapshot.operation_epoch;
            let action = ConnectorAction {
                id: "send".into(),
                payload: json!({}),
            };
            assert_eq!(
                hub.begin_action(&id, subscriber, &epoch, "op".into(), &action)
                    .unwrap(),
                OperationOutcome::Started
            );
        }
        let hub = ConversationHub::new(temp.path()).unwrap();
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        let (subscriber, snapshot) = hub.subscribe(&id, Grant::Interact).unwrap();
        let action = ConnectorAction {
            id: "send".into(),
            payload: json!({}),
        };
        assert_eq!(snapshot.operation_epoch, epoch);
        assert_eq!(
            hub.begin_action(&id, subscriber, &epoch, "op".into(), &action)
                .unwrap(),
            OperationOutcome::Ambiguous
        );
    }

    #[test]
    fn corrupt_operation_journal_rotates_epoch_before_any_retry_can_dispatch() {
        let temp = tempfile::tempdir().unwrap();
        let id = ConversationId::new("ses_corrupt");
        let old_epoch;
        {
            let hub = ConversationHub::new(temp.path()).unwrap();
            hub.watch(
                id.clone(),
                Box::new(FakeConnector),
                ConversationState::starting(None),
            )
            .unwrap();
            let (subscriber, snapshot) = hub.subscribe(&id, Grant::Interact).unwrap();
            old_epoch = snapshot.operation_epoch;
            hub.begin_action(
                &id,
                subscriber,
                &old_epoch,
                "old-operation".into(),
                &ConnectorAction {
                    id: "send".into(),
                    payload: json!({}),
                },
            )
            .unwrap();
        }

        let journal = temp
            .path()
            .join("conversations/ses_corrupt/transitions.jsonl");
        let contents = std::fs::read_to_string(&journal).unwrap();
        let (header, records) = contents.split_once('\n').unwrap();
        std::fs::write(&journal, format!("{header}\n{{corrupt\n{records}")).unwrap();

        let hub = ConversationHub::new(temp.path()).unwrap();
        hub.watch(
            id.clone(),
            Box::new(FakeConnector),
            ConversationState::starting(None),
        )
        .unwrap();
        let (subscriber, snapshot) = hub.subscribe(&id, Grant::Interact).unwrap();
        assert_ne!(snapshot.operation_epoch, old_epoch);
        assert!(matches!(
            hub.begin_action(
                &id,
                subscriber,
                &old_epoch,
                "old-operation".into(),
                &ConnectorAction {
                    id: "send".into(),
                    payload: json!({}),
                },
            )
            .unwrap(),
            OperationOutcome::Refused { .. }
        ));
    }
}
