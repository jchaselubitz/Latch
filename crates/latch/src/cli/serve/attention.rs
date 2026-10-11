//! Attention producer for paired phones.
//!
//! Every running agent session on this Mac is watched on behalf of every
//! paired device that may observe, as is any session a phone explicitly
//! opened. While the gateway runs, the Hub keeps observing those sessions
//! through subscriptions this module owns — not a phone's socket, which is
//! gone the moment the phone is in a pocket — and every transition into
//! "needs input" or "finished working" becomes one attention event per
//! device in an owner-only spool. Desktop forwards spool entries to the
//! control plane, which pushes an APNs alert.
//!
//! What leaves the Mac in the clear is still only an opaque event id. When
//! the device registered a notification key, the event also carries a
//! payload sealed to that key with HPKE (see [`seal`]): the session id, its
//! name, and which transition happened. The control plane forwards those
//! bytes without being able to read them, and the phone's notification
//! extension opens them to say which session needs attention.
//!
//! Nothing here reads harness transcripts or takes a terminal surface. The
//! only signal is the Hub-normalized `ConversationState` phase and pending
//! request.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::cli::remote_access::{self, DevicePermission};
use crate::conversation::{ConversationHub, ConversationId, ConversationPhase};
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

pub mod seal;

/// Sessions one device may keep the gateway observing on its behalf.
pub const MAX_WATCHED_SESSIONS_PER_DEVICE: usize = 16;
/// Devices with retained watches.
pub const MAX_WATCHING_DEVICES: usize = 8;
/// Event ids remembered so a helper restart does not re-notify history.
pub const MAX_DEDUPE_RECORDS: usize = 256;
/// Spool entries older than this are useless: the phone has long since
/// refreshed on its own, and the relay of a stale alert is noise.
pub const SPOOL_TTL: Duration = Duration::from_secs(10 * 60);
/// How often watched sessions are examined for transitions.
pub const TICK: Duration = Duration::from_secs(2);
/// Running agent sessions watched automatically, most recently active first.
pub const MAX_AUTO_WATCHED_SESSIONS: usize = 32;
/// How often the session list behind automatic watches is re-read. Listing
/// touches every session directory, so it runs far less often than the tick.
pub const SESSION_REFRESH: Duration = Duration::from_secs(10);

/// Which Hub transition produced an event. Stays on the Mac; the cloud
/// payload carries only the opaque event id.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// The agent finished a turn or the session exited.
    Completion,
    /// The agent is waiting on a permission or question.
    Approval,
}

/// One spooled event, written as `runtime/attention/<eventId>.json`.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttentionEvent {
    /// Random-looking but deterministic: hash of device, session,
    /// generation, revision, and kind, so one transition is spooled once per
    /// device and two devices never collide in the control plane's dedupe.
    pub event_id: String,
    /// Mac-local opaque controller id from the device store.
    pub device_id: String,
    /// Which Hub transition produced the event.
    pub kind: AttentionKind,
    /// Unix seconds.
    pub created_at: u64,
    /// Session the transition happened in. Mac-local; never sent in the clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// HPKE-sealed notification payload for this device, as lowercase hex,
    /// when the device registered a notification key. Opaque to everything
    /// between the Mac and the phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<String>,
}

/// What a notification can say about a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SessionLabel {
    name: String,
    title: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    /// device id -> watched session ids, most recently watched last.
    #[serde(default)]
    watches: HashMap<String, VecDeque<String>>,
    /// Event ids already spooled, oldest first.
    #[serde(default)]
    delivered: VecDeque<String>,
}

#[derive(Clone, PartialEq, Eq)]
struct Observed {
    phase: ConversationPhase,
    pending: bool,
}

struct State {
    persisted: Persisted,
    /// Last observed state per session, to detect transitions.
    observed: HashMap<String, Observed>,
    /// Hub subscriptions this watcher owns, per session.
    subscriptions: HashMap<String, u64>,
    /// Running agent sessions watched for every device, refreshed every
    /// [`SESSION_REFRESH`].
    automatic: Vec<String>,
    /// Labels of the sessions last listed, for sealed payloads.
    labels: HashMap<String, SessionLabel>,
    /// Unix seconds of the last session listing, `None` before the first.
    listed_at: Option<u64>,
}

/// Owner of watched-session subscriptions and the attention spool.
#[derive(Clone)]
pub struct AttentionWatcher {
    home: LatchHome,
    hub: ConversationHub,
    state: Arc<Mutex<State>>,
}

impl AttentionWatcher {
    /// Loads retained watches and the dedupe record from the owner-only
    /// runtime directory. Missing or unreadable state starts empty.
    pub fn new(home: LatchHome, hub: ConversationHub) -> Self {
        let persisted = fs::read(watches_path(&home))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            home,
            hub,
            state: Arc::new(Mutex::new(State {
                persisted,
                observed: HashMap::new(),
                subscriptions: HashMap::new(),
                automatic: Vec::new(),
                labels: HashMap::new(),
                listed_at: None,
            })),
        }
    }

    /// Records that `device` opened `session`'s conversation. Bounded per
    /// device and overall; the oldest watch is forgotten first.
    pub fn watch(&self, session: &str, device: &str) {
        let mut state = self.state.lock().expect("attention poisoned");
        let watches = &mut state.persisted.watches;
        if !watches.contains_key(device) && watches.len() >= MAX_WATCHING_DEVICES {
            return;
        }
        let list = watches.entry(device.to_owned()).or_default();
        list.retain(|id| id != session);
        list.push_back(session.to_owned());
        while list.len() > MAX_WATCHED_SESSIONS_PER_DEVICE {
            list.pop_front();
        }
        let _ = persist(&self.home, &state.persisted);
    }

    /// Whether any device keeps this session watched.
    #[cfg(test)]
    pub fn is_watched(&self, session: &str) -> bool {
        let state = self.state.lock().expect("attention poisoned");
        state
            .persisted
            .watches
            .values()
            .any(|list| list.iter().any(|id| id == session))
    }

    /// Runs one observation tick: prunes watches for revoked or observe-less
    /// devices, keeps every watched session subscribed, and spools one event
    /// per new attention transition. Returns the events spooled.
    pub fn tick(&self) -> Result<Vec<AttentionEvent>> {
        self.tick_at(unix_time())
    }

    fn tick_at(&self, now: u64) -> Result<Vec<AttentionEvent>> {
        let devices = remote_access::list_devices(&self.home).unwrap_or_default();
        let allowed: Vec<String> = devices
            .iter()
            .filter(|device| {
                !device.revoked && device.permission.permits(DevicePermission::Observe)
            })
            .map(|device| device.device_id.clone())
            .collect();
        let listing = self
            .session_listing_due(now)
            .then(|| list_agent_sessions(&self.home));
        let mut spooled = Vec::new();
        let mut state = self.state.lock().expect("attention poisoned");
        if let Some((automatic, labels)) = listing {
            state.automatic = automatic;
            state.labels = labels;
            state.listed_at = Some(now);
        }
        let before = state.persisted.watches.len();
        state
            .persisted
            .watches
            .retain(|device, _| allowed.contains(device));
        if state.persisted.watches.len() != before {
            let _ = persist(&self.home, &state.persisted);
        }
        // Which devices each session is watched for: every allowed device for
        // a running agent session, plus whatever a device opened explicitly.
        let mut watchers: HashMap<String, Vec<String>> = HashMap::new();
        for device in &allowed {
            let explicit = state.persisted.watches.get(device);
            let sessions = state.automatic.iter().chain(explicit.into_iter().flatten());
            for session in sessions {
                let list = watchers.entry(session.clone()).or_default();
                if !list.contains(device) {
                    list.push(device.clone());
                }
            }
        }
        // Drop subscriptions for sessions nobody watches any more.
        let stale: Vec<String> = state
            .subscriptions
            .keys()
            .filter(|id| !watchers.contains_key(*id))
            .cloned()
            .collect();
        for session in stale {
            if let Some(subscriber) = state.subscriptions.remove(&session) {
                self.hub
                    .unsubscribe(&ConversationId::new(session.as_str()), subscriber);
            }
            state.observed.remove(&session);
        }
        let mut sessions: Vec<_> = watchers.into_iter().collect();
        sessions.sort();
        for (session, devices) in sessions {
            let id = ConversationId::new(session.as_str());
            if self.hub.ensure_watched(&id).is_err() {
                continue;
            }
            if !state.subscriptions.contains_key(&session) {
                let Some((subscriber, _)) = self.hub.subscribe(&id, DevicePermission::Observe)
                else {
                    continue;
                };
                state.subscriptions.insert(session.clone(), subscriber);
            }
            if let Some(subscriber) = state.subscriptions.get(&session) {
                // The queue is bounded; draining discards fanout this
                // watcher never renders.
                let _ = self.hub.drain(&id, *subscriber);
            }
            let Some(snapshot) = self.hub.snapshot(&id, 0) else {
                continue;
            };
            let current = Observed {
                phase: snapshot.state.phase,
                pending: snapshot.state.pending_request.is_some(),
            };
            // One observation per session per tick, shared by every device:
            // observing per device would let the first device consume the
            // transition and leave the rest seeing no change.
            let previous = state.observed.insert(session.clone(), current.clone());
            let Some(previous) = previous else {
                // The first observation is a baseline, not a transition:
                // a helper restart must not re-notify the state it finds.
                continue;
            };
            let Some(kind) = transition(previous, current) else {
                continue;
            };
            let label = state.labels.get(&session).cloned().unwrap_or_default();
            for device in devices {
                let event_id = event_id(
                    &device,
                    &session,
                    &snapshot.generation.as_wire(),
                    snapshot.revision.get(),
                    kind,
                );
                if state.persisted.delivered.iter().any(|id| *id == event_id) {
                    continue;
                }
                let sealed = remote_access::notification_key(&self.home, &device).and_then(|key| {
                    let payload =
                        seal::Payload::new(&session, &label.name, label.title.as_deref(), kind);
                    seal::seal(&key, &event_id, &payload).ok()
                });
                let event = AttentionEvent {
                    event_id: event_id.clone(),
                    device_id: device,
                    kind,
                    created_at: now,
                    session_id: Some(session.clone()),
                    sealed,
                };
                if spool(&self.home, &event).is_ok() {
                    state.persisted.delivered.push_back(event_id);
                    while state.persisted.delivered.len() > MAX_DEDUPE_RECORDS {
                        state.persisted.delivered.pop_front();
                    }
                    spooled.push(event);
                }
            }
        }
        if !spooled.is_empty() {
            persist(&self.home, &state.persisted)?;
        }
        let _ = sweep_spool(&self.home, now);
        Ok(spooled)
    }

    fn session_listing_due(&self, now: u64) -> bool {
        let state = self.state.lock().expect("attention poisoned");
        state
            .listed_at
            .is_none_or(|at| now.saturating_sub(at) >= SESSION_REFRESH.as_secs())
    }

    /// Runs `tick` forever at [`TICK`]. Errors are content-free and swallowed:
    /// notifications are best effort and never block the gateway.
    pub async fn run(self) {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let watcher = self.clone();
            let _ = tokio::task::spawn_blocking(move || watcher.tick()).await;
        }
    }
}

fn transition(previous: Observed, current: Observed) -> Option<AttentionKind> {
    let (previous, current) = (&previous, &current);
    if !previous.pending && current.pending {
        return Some(AttentionKind::Approval);
    }
    match (&previous.phase, &current.phase) {
        (ConversationPhase::Working, ConversationPhase::AwaitingInput) => {
            Some(AttentionKind::Approval)
        }
        (ConversationPhase::Working, ConversationPhase::Idle)
        | (ConversationPhase::Working, ConversationPhase::Exited) => {
            Some(AttentionKind::Completion)
        }
        _ => None,
    }
}

fn event_id(
    device: &str,
    session: &str,
    generation: &str,
    revision: u64,
    kind: AttentionKind,
) -> String {
    let mut digest = sha2::Sha256::new();
    digest.update(b"latch-attention/v2\0");
    digest.update(device.as_bytes());
    digest.update([0]);
    digest.update(session.as_bytes());
    digest.update([0]);
    digest.update(generation.as_bytes());
    digest.update([0]);
    digest.update(revision.to_be_bytes());
    digest.update([0]);
    digest.update(match kind {
        AttentionKind::Completion => b"completion".as_slice(),
        AttentionKind::Approval => b"approval".as_slice(),
    });
    let value = digest.finalize();
    value[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Running agent sessions, most recently active first and bounded, plus a
/// label for every listed session. A listing failure watches nothing new
/// rather than failing the tick.
fn list_agent_sessions(home: &LatchHome) -> (Vec<String>, HashMap<String, SessionLabel>) {
    let Ok(report) =
        crate::cli::manage::list(crate::cli::manage::ListOptions { home: home.clone() })
    else {
        return (Vec::new(), HashMap::new());
    };
    let labels = report
        .sessions
        .iter()
        .map(|session| {
            (
                session.id.clone(),
                SessionLabel {
                    name: session.name.clone(),
                    title: session.title.clone(),
                },
            )
        })
        .collect();
    let mut agents: Vec<_> = report
        .sessions
        .iter()
        .filter(|session| session.connector.is_some() && session.state == "running")
        .collect();
    // Smallest idle time first: the session most recently active.
    agents.sort_by_key(|session| session.idle_ms.unwrap_or(u64::MAX));
    let automatic = agents
        .into_iter()
        .take(MAX_AUTO_WATCHED_SESSIONS)
        .map(|session| session.id.clone())
        .collect();
    (automatic, labels)
}

fn runtime_dir(home: &LatchHome) -> PathBuf {
    home.remote_access_dir().join("runtime")
}

/// Directory Desktop reads for pending events.
pub fn spool_dir(home: &LatchHome) -> PathBuf {
    runtime_dir(home).join("attention")
}

fn watches_path(home: &LatchHome) -> PathBuf {
    runtime_dir(home).join("attention-watches.json")
}

fn ensure_private(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("spool path has no parent")?;
    ensure_private(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("event")
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn persist(home: &LatchHome, persisted: &Persisted) -> Result<()> {
    write_private(&watches_path(home), &serde_json::to_vec(persisted)?)
}

fn spool(home: &LatchHome, event: &AttentionEvent) -> Result<()> {
    let path = spool_dir(home).join(format!("{}.json", event.event_id));
    write_private(&path, &serde_json::to_vec(event)?)
}

/// Removes spool entries past [`SPOOL_TTL`]; Desktop removes the ones it
/// forwards.
fn sweep_spool(home: &LatchHome, now: u64) -> Result<()> {
    let dir = spool_dir(home);
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(event) = serde_json::from_slice::<AttentionEvent>(&bytes) else {
            let _ = fs::remove_file(&path);
            continue;
        };
        if now.saturating_sub(event.created_at) > SPOOL_TTL.as_secs() {
            let _ = fs::remove_file(&path);
        }
    }
    Ok(())
}

/// Removes one forwarded (or abandoned) spool entry. Unknown ids are not an
/// error: Desktop may acknowledge an entry the sweeper already expired.
pub fn acknowledge(home: &LatchHome, event_id: &str) -> Result<()> {
    if event_id.len() != 32 || !event_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("invalid attention event id");
    }
    let path = spool_dir(home).join(format!("{event_id}.json"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Reads every pending spool entry, oldest first.
pub fn pending_events(home: &LatchHome) -> Result<Vec<AttentionEvent>> {
    let dir = spool_dir(home);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut events = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Ok(event) = serde_json::from_slice::<AttentionEvent>(&fs::read(&path)?) {
            events.push(event);
        }
    }
    events.sort_by_key(|event| event.created_at);
    Ok(events)
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{
        ActionDescriptor, ApplyResult, CheckpointDelta, Connector, ConnectorAction,
        ConnectorIdentity, ConnectorMutation, ConversationItemId, ConversationState, Detection,
        PollBudget, PollResult,
    };
    use std::time::Duration;

    struct IdleConnector;
    impl Connector for IdleConnector {
        fn detect(&self) -> Detection {
            Detection::Supported(ConnectorIdentity {
                id: "fake".into(),
                version: "1".into(),
            })
        }
        fn poll(&mut self, _: PollBudget) -> anyhow::Result<PollResult> {
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
            vec![]
        }
        fn apply(&mut self, _: ConnectorAction, _: Duration) -> anyhow::Result<ApplyResult> {
            Ok(ApplyResult::Accepted { correlation: None })
        }
        fn reconcile(
            &self,
            _: &[ConversationItemId],
            _: &[ConversationItemId],
        ) -> Vec<ConnectorMutation> {
            vec![]
        }
        fn checkpoint_snapshot(&self) -> anyhow::Result<Vec<u8>> {
            Ok(vec![])
        }
    }

    fn state(phase: ConversationPhase, pending: Option<&str>) -> ConversationState {
        let mut state = ConversationState::starting(None);
        state.phase = phase;
        state.pending_request = pending.map(str::to_owned);
        state
    }

    fn enrolled_home() -> (tempfile::TempDir, LatchHome, String) {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path());
        remote_access::set_enabled(&home, true).unwrap();
        remote_access::authorize_enrollment(
            &home,
            &format!("enr_{}", "1".repeat(32)),
            &"ab".repeat(32),
            "Phone",
            DevicePermission::Interact,
            &format!("dev_{}", "2".repeat(32)),
        )
        .unwrap();
        let device = remote_access::list_devices(&home)
            .unwrap()
            .remove(0)
            .device_id;
        (directory, home, device)
    }

    #[test]
    fn transitions_spool_once_dedupe_across_restart_and_stop_on_revoke() {
        let (directory, home, device) = enrolled_home();
        let hub = ConversationHub::with_connector_factory(
            directory.path().join("hub"),
            Arc::new(|_| Box::new(IdleConnector) as Box<dyn Connector>),
        )
        .unwrap();
        let id = ConversationId::new("ses_attention");
        hub.ensure_watched(&id).unwrap();
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::Working,
                None,
            ))],
        )
        .unwrap();

        let watcher = AttentionWatcher::new(home.clone(), hub.clone());
        watcher.watch("ses_attention", &device);
        // Baseline: the first observation of a working session notifies nobody.
        assert!(watcher.tick_at(1_000).unwrap().is_empty());
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::Idle,
                None,
            ))],
        )
        .unwrap();
        let events = watcher.tick_at(1_001).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AttentionKind::Completion);
        assert_eq!(events[0].device_id, device);
        assert_eq!(pending_events(&home).unwrap(), events);
        // The same state again is not a new transition.
        assert!(watcher.tick_at(1_002).unwrap().is_empty());

        // Working again, then waiting on input: an approval event.
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::Working,
                None,
            ))],
        )
        .unwrap();
        assert!(watcher.tick_at(1_002).unwrap().is_empty());
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::AwaitingInput,
                None,
            ))],
        )
        .unwrap();
        let events = watcher.tick_at(1_003).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, AttentionKind::Approval);
        let spooled: Vec<String> = pending_events(&home)
            .unwrap()
            .into_iter()
            .map(|event| event.event_id)
            .collect();
        assert_eq!(spooled.len(), 2);
        // Desktop acknowledges what it forwarded; nothing else is touched.
        acknowledge(&home, &spooled[0]).unwrap();
        assert_eq!(pending_events(&home).unwrap().len(), 1);
        assert!(acknowledge(&home, "../../etc/passwd").is_err());
        acknowledge(&home, &spooled[0]).unwrap();

        // A restarted watcher restores the watch list and dedupe record, so
        // the history it finds does not become fresh notifications.
        let restarted = AttentionWatcher::new(home.clone(), hub.clone());
        assert!(restarted.is_watched("ses_attention"));
        assert!(restarted.tick_at(1_004).unwrap().is_empty());
        assert_eq!(pending_events(&home).unwrap().len(), 1);

        // Spool entries expire, and revoking the device ends its watches.
        let _ = sweep_spool(&home, 1_004 + SPOOL_TTL.as_secs() + 1);
        assert!(pending_events(&home).unwrap().is_empty());
        remote_access::revoke(&home, &device).unwrap();
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::Working,
                None,
            ))],
        )
        .unwrap();
        assert!(restarted.tick_at(1_005).unwrap().is_empty());
        assert!(!restarted.is_watched("ses_attention"));
        hub.apply_poll(
            &id,
            vec![ConnectorMutation::State(state(
                ConversationPhase::Idle,
                None,
            ))],
        )
        .unwrap();
        assert!(restarted.tick_at(1_006).unwrap().is_empty());
    }

    #[test]
    fn every_watching_device_gets_its_own_event_and_only_keyed_devices_a_sealed_one() {
        use hpke::{Deserializable, Kem as _, OpModeR, Serializable};
        type Kem = hpke::kem::X25519HkdfSha256;

        let (directory, home, first) = enrolled_home();
        remote_access::authorize_enrollment(
            &home,
            &format!("enr_{}", "3".repeat(32)),
            &"cd".repeat(32),
            "Tablet",
            DevicePermission::Observe,
            &format!("dev_{}", "4".repeat(32)),
        )
        .unwrap();
        let second = remote_access::list_devices(&home)
            .unwrap()
            .into_iter()
            .find(|device| device.device_id != first)
            .unwrap()
            .device_id;
        let (secret, public) = Kem::gen_keypair();
        let public_hex: String = public
            .to_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        remote_access::set_notification_key(&home, &first, &public_hex).unwrap();

        let hub = ConversationHub::with_connector_factory(
            directory.path().join("hub"),
            Arc::new(|_| Box::new(IdleConnector) as Box<dyn Connector>),
        )
        .unwrap();
        let id = ConversationId::new("ses_shared");
        hub.ensure_watched(&id).unwrap();
        let set = |phase| {
            hub.apply_poll(&id, vec![ConnectorMutation::State(state(phase, None))])
                .unwrap()
        };
        set(ConversationPhase::Working);
        let watcher = AttentionWatcher::new(home.clone(), hub.clone());
        watcher.watch("ses_shared", &first);
        watcher.watch("ses_shared", &second);
        assert!(watcher.tick_at(2_000).unwrap().is_empty());
        set(ConversationPhase::AwaitingInput);
        let mut events = watcher.tick_at(2_001).unwrap();
        events.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        assert_eq!(events.len(), 2, "one transition, one event per device");
        assert_ne!(events[0].event_id, events[1].event_id);
        let keyed = events
            .iter()
            .find(|event| event.device_id == first)
            .unwrap();
        let plain = events
            .iter()
            .find(|event| event.device_id == second)
            .unwrap();
        assert!(plain.sealed.is_none());
        assert_eq!(keyed.session_id.as_deref(), Some("ses_shared"));

        let wire = keyed.sealed.as_deref().expect("sealed payload");
        let bytes: Vec<u8> = (0..wire.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&wire[i..i + 2], 16).unwrap())
            .collect();
        let (encapped, ciphertext) = bytes.split_at(32);
        let opened =
            hpke::single_shot_open::<hpke::aead::ChaCha20Poly1305, hpke::kdf::HkdfSha256, Kem>(
                &OpModeR::Base,
                &secret,
                &<Kem as hpke::Kem>::EncappedKey::from_bytes(encapped).unwrap(),
                seal::INFO,
                ciphertext,
                keyed.event_id.as_bytes(),
            )
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&opened).unwrap();
        assert_eq!(payload["s"], "ses_shared");
        assert_eq!(payload["k"], "approval");
        // Nothing readable about the session sits in the clear on the wire.
        let spooled = serde_json::to_string(keyed).unwrap();
        assert!(!wire.contains("ses_shared"));
        assert!(spooled.contains("\"sealed\""));
    }

    #[test]
    fn watches_are_bounded_per_device() {
        let (directory, home, device) = enrolled_home();
        let hub = ConversationHub::new(directory.path().join("hub")).unwrap();
        let watcher = AttentionWatcher::new(home, hub);
        for index in 0..(MAX_WATCHED_SESSIONS_PER_DEVICE + 4) {
            watcher.watch(&format!("ses_{index}"), &device);
        }
        assert!(!watcher.is_watched("ses_0"));
        assert!(watcher.is_watched(&format!("ses_{}", MAX_WATCHED_SESSIONS_PER_DEVICE + 3)));
        let file = fs::read(watches_path(&watcher.home)).unwrap();
        let text = String::from_utf8(file).unwrap();
        assert!(!text.contains("prompt"));
    }
}
