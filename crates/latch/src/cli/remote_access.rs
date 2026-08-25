//! Paired, LAN-only remote-access foundation.
//!
//! This module deliberately terminates its authenticated transport before
//! opening a connection to the existing loopback-only gateway. It never
//! forwards a caller-selected destination or the gateway bearer token.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snow::{params::NoiseParams, Builder, TransportState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Semaphore};

use crate::cli::serve::routes::{route_for, DEVICE_GRANT_HEADER};
use crate::cli::serve::{load_token, mint_token};
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

pub use crate::cli::serve::routes::Grant as DevicePermission;

const PAIRING_LIFETIME: Duration = Duration::from_secs(5 * 60);
const MAX_RECORD: usize = 65_535;
const MAX_INITIAL_REQUEST: usize = 32 * 1024;
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const PRESENCE_LIFETIME: Duration = Duration::from_secs(90);
const DIRECT_PROBE_MAGIC: &[u8; 8] = b"LCHDRCT1";
const DIRECT_PROBE_SIZE: usize = DIRECT_PROBE_MAGIC.len() + 32;
const RELAY_TICKET_LIFETIME: Duration = Duration::from_secs(60);
const RELAY_RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_PAIRED_DEVICES: usize = 32;
const MAX_PENDING_PAIRINGS: usize = 8;
const MAX_LAN_CONNECTIONS: usize = 32;
const MAX_OFFER_CANDIDATES: usize = 16;
/// How many pending offer documents one drain pass will act on. A desktop app
/// publishes at most a handful per presence refresh; anything beyond this is a
/// stale directory, not a workload.
const MAX_PENDING_OFFERS: usize = 32;
/// How often the helper looks for offers the desktop app recorded and for a
/// re-gathered agent description. Short enough that a phone waiting on a
/// rendezvous does not notice it, long enough to be free when nothing happens.
const OFFER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The deadline every peer transport applies to a single record read.
///
/// This is deliberately an inactivity deadline, not a lifetime for the
/// connection: a WebSocket may be useful for much longer than ten seconds. It
/// is public because the helper's ICE transport implements the same contract
/// and must not invent a second number for it.
pub const PROXY_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Audit event naming the route an authorized connection took.
const PATH_SELECTED_EVENT: &str = "path_selected";
/// Audit event naming the outcome of one ICE answer by the helper.
const ICE_ANSWER_EVENT: &str = "ice_answer";
const MAX_AUDIT_EVENTS: usize = 1_024;
const MAX_AUDIT_BYTES: usize = 512 * 1024;
#[cfg(all(target_os = "macos", not(test)))]
const SECRET_SERVICE: &str = "co.cooperativ.latch.remote-access";

/// Connection state exposed to a future mobile client without revealing
/// endpoint addresses or session data.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionState {
    Offline,
    Connecting,
    Direct,
    Relay,
    AuthorizationFailure,
    QuotaLimited,
}

/// Privacy-safe transport diagnostics. Endpoint addresses, session names,
/// gateway tokens, and application bytes never appear here.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDiagnostics {
    pub state: ConnectionState,
    pub reconnect_count: u32,
    pub path_migrations: u32,
    pub last_failure: Option<ConnectionFailure>,
}

impl Default for ConnectionDiagnostics {
    fn default() -> Self {
        Self {
            state: ConnectionState::Offline,
            reconnect_count: 0,
            path_migrations: 0,
            last_failure: None,
        }
    }
}

/// Coarse failure classification intended for UI and support diagnostics.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionFailure {
    PresenceExpired,
    RendezvousRejected,
    DirectTimeout,
    AuthorizationRejected,
    RelayRejected,
    RelayQuotaExceeded,
    RelayUnavailable,
}

/// A short-lived, opaque endpoint candidate. Candidates are control-plane
/// metadata, never a gateway address or a local service destination.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectCandidate {
    pub address: SocketAddr,
    pub expires_at: u64,
}

#[allow(missing_docs)] // The type-level comment documents the validation contract.
impl DirectCandidate {
    /// Creates a candidate with the same bounded lifetime as directory
    /// presence. Callers still validate it before publishing or probing.
    pub fn short_lived(address: SocketAddr) -> Self {
        Self {
            address,
            expires_at: unix_time() + PRESENCE_LIFETIME.as_secs(),
        }
    }

    pub fn validate(&self, now: u64) -> anyhow::Result<()> {
        if self.expires_at <= now || self.expires_at > now + PRESENCE_LIFETIME.as_secs() {
            bail!("direct candidate has an invalid lifetime");
        }
        if self.address.port() == 0 || !routable_candidate_ip(self.address.ip()) {
            bail!("direct candidate has an invalid address");
        }
        Ok(())
    }
}

/// Content-free presence record held by the device directory.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRecord {
    pub device_id: String,
    pub identity_key: String,
    pub candidates: Vec<DirectCandidate>,
    pub expires_at: u64,
}

/// A request passed through the control plane after its outer authenticated
/// control channel has established `requester_device_id`. The payload has no
/// terminal data, session metadata, or gateway credential.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RendezvousRequest {
    pub requester_device_id: String,
    pub target_device_id: String,
    pub request_id: String,
    pub candidates: Vec<DirectCandidate>,
    pub expires_at: u64,
}

/// Candidate response returned only to the target paired device.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RendezvousResponse {
    pub request_id: String,
    pub peer_device_id: String,
    pub peer_identity_key: String,
    pub candidates: Vec<DirectCandidate>,
    pub expires_at: u64,
}

/// Minimal directory state. It is deliberately transport-agnostic: a hosted
/// HTTPS/WebSocket control channel authenticates a caller before invoking this
/// API, while tests can exercise the same rules in memory.
#[derive(Debug, Default)]
pub struct DeviceDirectory {
    presence: HashMap<String, PresenceRecord>,
}

#[allow(missing_docs)] // The type-level comment documents the directory boundary.
impl DeviceDirectory {
    /// Registers (or refreshes) a signed device presence after validating its
    /// short lifetime and public candidate metadata.
    pub fn publish(&mut self, record: PresenceRecord, now: u64) -> anyhow::Result<()> {
        validate_presence(&record, now)?;
        self.presence.insert(record.device_id.clone(), record);
        Ok(())
    }

    /// Delivers a caller's candidates to a currently-present paired target.
    /// Pair authorization is injected by the authenticated desktop control
    /// connection; this keeps the directory from becoming an authority for
    /// Latch's local authorization policy.
    pub fn rendezvous(
        &mut self,
        request: RendezvousRequest,
        requester_is_paired: bool,
        now: u64,
    ) -> anyhow::Result<RendezvousResponse> {
        self.presence.retain(|_, record| record.expires_at > now);
        if !requester_is_paired {
            bail!("rendezvous requester is not paired");
        }
        if request.requester_device_id == request.target_device_id {
            bail!("rendezvous cannot target the requesting device");
        }
        validate_request(&request, now)?;
        let target = self
            .presence
            .get(&request.target_device_id)
            .ok_or_else(|| anyhow!("target device is offline"))?;
        Ok(RendezvousResponse {
            request_id: request.request_id,
            peer_device_id: target.device_id.clone(),
            peer_identity_key: target.identity_key.clone(),
            candidates: target.candidates.clone(),
            expires_at: target.expires_at.min(request.expires_at),
        })
    }
}

/// A state machine for reconnect and path migration. Re-running capability
/// discovery is deliberately required after every interruption before a
/// caller may send an application request over the new path.
#[derive(Debug, Default)]
pub struct DirectConnection {
    diagnostics: ConnectionDiagnostics,
    requires_capability_refresh: bool,
}

#[allow(missing_docs)] // The type-level comment documents state-machine transitions.
impl DirectConnection {
    pub fn diagnostics(&self) -> &ConnectionDiagnostics {
        &self.diagnostics
    }

    pub fn begin(&mut self) {
        self.diagnostics.state = ConnectionState::Connecting;
        self.diagnostics.last_failure = None;
    }

    pub fn connected(&mut self) {
        self.diagnostics.state = ConnectionState::Direct;
        self.diagnostics.last_failure = None;
    }

    /// Marks an opaque relay stream as established after direct establishment
    /// failed. The caller must refresh gateway capabilities before application
    /// bytes can use the new path, just as it must after a direct migration.
    pub fn relay_connected(&mut self) {
        self.diagnostics.state = ConnectionState::Relay;
        self.diagnostics.last_failure = None;
        self.requires_capability_refresh = true;
    }

    /// Selects relay fallback only for a failed direct establishment. This
    /// keeps authorization and control-plane failures from silently becoming
    /// relay attempts.
    pub fn fallback_to_relay(&mut self) -> anyhow::Result<()> {
        if !matches!(
            self.diagnostics.last_failure,
            Some(ConnectionFailure::DirectTimeout | ConnectionFailure::RendezvousRejected)
        ) {
            bail!("relay fallback requires a direct-path failure");
        }
        self.relay_connected();
        Ok(())
    }

    /// Records a successful relay-to-direct migration. The new path is held
    /// until capability discovery completes so HTTP requests cannot straddle
    /// transport identities.
    pub fn direct_recovered_from_relay(&mut self) -> anyhow::Result<()> {
        if self.diagnostics.state != ConnectionState::Relay {
            bail!("direct recovery requires an active relay path");
        }
        self.diagnostics.path_migrations = self.diagnostics.path_migrations.saturating_add(1);
        self.diagnostics.state = ConnectionState::Direct;
        self.requires_capability_refresh = true;
        Ok(())
    }

    pub fn interrupted(&mut self) {
        self.diagnostics.reconnect_count = self.diagnostics.reconnect_count.saturating_add(1);
        self.diagnostics.state = ConnectionState::Connecting;
        self.requires_capability_refresh = true;
    }

    pub fn migrated_path(&mut self) -> anyhow::Result<()> {
        if self.diagnostics.state != ConnectionState::Direct {
            bail!("cannot migrate a connection that is not direct");
        }
        self.diagnostics.path_migrations = self.diagnostics.path_migrations.saturating_add(1);
        self.requires_capability_refresh = true;
        Ok(())
    }

    pub fn capability_refreshed(&mut self) {
        self.requires_capability_refresh = false;
    }

    pub fn may_forward_application_data(&self) -> bool {
        matches!(
            self.diagnostics.state,
            ConnectionState::Direct | ConnectionState::Relay
        ) && !self.requires_capability_refresh
    }

    pub fn failed(&mut self, failure: ConnectionFailure) {
        self.diagnostics.last_failure = Some(failure);
        self.diagnostics.state = if failure == ConnectionFailure::AuthorizationRejected {
            ConnectionState::AuthorizationFailure
        } else if failure == ConnectionFailure::RelayQuotaExceeded {
            ConnectionState::QuotaLimited
        } else {
            ConnectionState::Offline
        };
    }
}

/// Short-lived, one-time relay admission material. It authorizes only a
/// device pair to occupy an opaque relay slot; it is not a gateway credential
/// and it cannot decrypt application traffic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)] // The type-level trust-boundary comment documents these wire fields.
pub struct RelayTicket {
    pub relay_id: String,
    pub authentication_secret: String,
    pub mac_device_id: String,
    pub phone_device_id: String,
    pub expires_at: u64,
}

/// Fixed resource limits for the relay. These limits are intentionally small
/// and are enforced before opaque frames are queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)] // The type-level resource-limit comment documents these fields.
pub struct RelayLimits {
    pub max_active_tickets: usize,
    pub max_active_connections: usize,
    pub max_frame_bytes: usize,
    pub max_frames_per_window: u32,
    pub max_bytes_per_window: usize,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            max_active_tickets: 256,
            max_active_connections: 2,
            max_frame_bytes: MAX_RECORD,
            max_frames_per_window: 256,
            max_bytes_per_window: 1024 * 1024,
        }
    }
}

/// Content-free aggregate relay measurements suitable for an operations
/// dashboard. Payload bytes, gateway tokens, and endpoint keys are never
/// retained or reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(missing_docs)] // The type-level privacy boundary documents these counters.
pub struct RelayDiagnostics {
    pub active_connections: usize,
    pub authenticated_connections: u64,
    pub forwarded_frames: u64,
    pub forwarded_bytes: u64,
    pub rejected_authentications: u64,
    pub rejected_quota: u64,
}

#[derive(Debug)]
struct RelaySlot {
    ticket: RelayTicket,
    connected: HashSet<String>,
    queues: HashMap<String, VecDeque<Vec<u8>>>,
    window_started_at: u64,
    window_frames: u32,
    window_bytes: usize,
}

/// An authenticated connection to an opaque relay slot. Its fields are
/// private so callers cannot bypass admission by constructing one directly.
#[derive(Debug, Clone)]
pub struct RelayConnection {
    relay_id: String,
    device_id: String,
}

/// In-memory protocol model for a regional relay. A production relay can use
/// the same admission and frame rules behind TLS/WebSocket transport; this
/// type deliberately only sees encrypted framed bytes.
#[derive(Debug)]
pub struct OpaqueRelay {
    limits: RelayLimits,
    slots: HashMap<String, RelaySlot>,
    diagnostics: RelayDiagnostics,
}

#[allow(missing_docs)] // The type-level comment documents this headless relay boundary.
impl OpaqueRelay {
    pub fn new(limits: RelayLimits) -> Self {
        Self {
            limits,
            slots: HashMap::new(),
            diagnostics: RelayDiagnostics::default(),
        }
    }

    /// Issues an admission record only after the authenticated control plane
    /// has verified that the two device identities are paired. The boolean is
    /// deliberately injected rather than trusting a client-supplied id.
    pub fn issue_ticket(
        &mut self,
        mac_device_id: &str,
        phone_device_id: &str,
        paired_by_control_plane: bool,
        now: u64,
    ) -> anyhow::Result<RelayTicket> {
        self.prune_expired(now);
        if !paired_by_control_plane
            || !valid_opaque_id(mac_device_id)
            || !valid_opaque_id(phone_device_id)
            || mac_device_id == phone_device_id
        {
            self.diagnostics.rejected_authentications += 1;
            bail!("relay ticket request is not authorized");
        }
        if self.slots.len() >= self.limits.max_active_tickets {
            self.diagnostics.rejected_quota += 1;
            bail!("relay ticket quota exceeded");
        }
        let ticket = RelayTicket {
            relay_id: random_hex(16)?,
            authentication_secret: random_hex(32)?,
            mac_device_id: mac_device_id.to_owned(),
            phone_device_id: phone_device_id.to_owned(),
            expires_at: now + RELAY_TICKET_LIFETIME.as_secs(),
        };
        self.slots.insert(
            ticket.relay_id.clone(),
            RelaySlot {
                ticket: ticket.clone(),
                connected: HashSet::new(),
                queues: HashMap::new(),
                window_started_at: now,
                window_frames: 0,
                window_bytes: 0,
            },
        );
        Ok(ticket)
    }

    /// Authenticates one endpoint using its short-lived ticket. The relay only
    /// learns opaque device ids and ticket material; endpoint Noise identities
    /// and application plaintext stay in the end-to-end handshake.
    pub fn connect(
        &mut self,
        ticket: &RelayTicket,
        device_id: &str,
        now: u64,
    ) -> anyhow::Result<RelayConnection> {
        let Some(slot) = self.slots.get_mut(&ticket.relay_id) else {
            self.diagnostics.rejected_authentications += 1;
            bail!("relay ticket is unknown");
        };
        let allowed =
            device_id == slot.ticket.mac_device_id || device_id == slot.ticket.phone_device_id;
        if ticket.expires_at <= now
            || slot.ticket.expires_at <= now
            || !allowed
            || slot.connected.contains(device_id)
            || !constant_time_eq(
                ticket.authentication_secret.as_bytes(),
                slot.ticket.authentication_secret.as_bytes(),
            )
        {
            self.diagnostics.rejected_authentications += 1;
            bail!("relay authentication was rejected");
        }
        if !slot.connected.contains(device_id)
            && slot.connected.len() >= self.limits.max_active_connections
        {
            self.diagnostics.rejected_quota += 1;
            bail!("relay connection quota exceeded");
        }
        if slot.connected.insert(device_id.to_owned()) {
            self.diagnostics.active_connections += 1;
            self.diagnostics.authenticated_connections += 1;
        }
        Ok(RelayConnection {
            relay_id: ticket.relay_id.clone(),
            device_id: device_id.to_owned(),
        })
    }

    /// Forwards an already-encrypted application frame without parsing it.
    pub fn forward(
        &mut self,
        sender: &RelayConnection,
        ciphertext: Vec<u8>,
        now: u64,
    ) -> anyhow::Result<()> {
        let Some(slot) = self.slots.get_mut(&sender.relay_id) else {
            self.diagnostics.rejected_authentications += 1;
            bail!("relay connection is no longer available");
        };
        if slot.ticket.expires_at <= now
            || !slot.connected.contains(&sender.device_id)
            || ciphertext.is_empty()
            || ciphertext.len() > self.limits.max_frame_bytes
        {
            self.diagnostics.rejected_authentications += 1;
            bail!("invalid opaque relay frame");
        }
        if now.saturating_sub(slot.window_started_at) >= RELAY_RATE_WINDOW.as_secs() {
            slot.window_started_at = now;
            slot.window_frames = 0;
            slot.window_bytes = 0;
        }
        if slot.window_frames >= self.limits.max_frames_per_window
            || slot.window_bytes.saturating_add(ciphertext.len()) > self.limits.max_bytes_per_window
        {
            self.diagnostics.rejected_quota += 1;
            bail!("relay frame quota exceeded");
        }
        let recipient = if sender.device_id == slot.ticket.mac_device_id {
            &slot.ticket.phone_device_id
        } else {
            &slot.ticket.mac_device_id
        };
        if !slot.connected.contains(recipient) {
            bail!("relay peer is not connected");
        }
        let queued_bytes: usize = slot
            .queues
            .get(recipient)
            .into_iter()
            .flatten()
            .map(Vec::len)
            .sum();
        if queued_bytes.saturating_add(ciphertext.len()) > self.limits.max_bytes_per_window {
            self.diagnostics.rejected_quota += 1;
            bail!("relay peer queue quota exceeded");
        }
        slot.window_frames += 1;
        slot.window_bytes += ciphertext.len();
        slot.queues
            .entry(recipient.clone())
            .or_default()
            .push_back(ciphertext.clone());
        self.diagnostics.forwarded_frames += 1;
        self.diagnostics.forwarded_bytes += ciphertext.len() as u64;
        Ok(())
    }

    /// Removes and returns the next opaque frame for an authenticated endpoint.
    pub fn receive(&mut self, receiver: &RelayConnection) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(slot) = self.slots.get_mut(&receiver.relay_id) else {
            bail!("relay connection is no longer available");
        };
        if !slot.connected.contains(&receiver.device_id) {
            bail!("relay receiver is not authenticated");
        }
        Ok(slot
            .queues
            .entry(receiver.device_id.clone())
            .or_default()
            .pop_front())
    }

    /// Releases relay resources immediately when an endpoint disconnects.
    pub fn disconnect(&mut self, connection: &RelayConnection) {
        let mut remove_slot = false;
        if let Some(slot) = self.slots.get_mut(&connection.relay_id) {
            if slot.connected.remove(&connection.device_id) {
                self.diagnostics.active_connections =
                    self.diagnostics.active_connections.saturating_sub(1);
            }
            slot.queues.remove(&connection.device_id);
            remove_slot = slot.connected.is_empty();
        }
        if remove_slot {
            self.slots.remove(&connection.relay_id);
        }
    }

    pub fn diagnostics(&self) -> &RelayDiagnostics {
        &self.diagnostics
    }

    fn prune_expired(&mut self, now: u64) {
        let expired_connections: usize = self
            .slots
            .values()
            .filter(|slot| slot.ticket.expires_at <= now)
            .map(|slot| slot.connected.len())
            .sum();
        self.slots.retain(|_, slot| slot.ticket.expires_at > now);
        self.diagnostics.active_connections = self
            .diagnostics
            .active_connections
            .saturating_sub(expired_connections);
    }
}

/// Stateful application encryption for a relay endpoint. This wraps Snow's
/// Noise transport mode, so the relay can forward the resulting bytes without
/// receiving terminal plaintext or endpoint decryption keys.
pub struct RelayCipher {
    state: TransportState,
}

#[allow(missing_docs)] // The type-level comment documents the encrypted-frame interface.
impl RelayCipher {
    pub fn seal(&mut self, plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
        if plaintext.is_empty() || plaintext.len() > MAX_RECORD - 32 {
            bail!("relay plaintext frame exceeds limit");
        }
        let mut ciphertext = vec![0_u8; MAX_RECORD];
        let used = self.state.write_message(plaintext, &mut ciphertext)?;
        ciphertext.truncate(used);
        Ok(ciphertext)
    }

    pub fn open(&mut self, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        if ciphertext.is_empty() || ciphertext.len() > MAX_RECORD {
            bail!("relay ciphertext frame exceeds limit");
        }
        let mut plaintext = vec![0_u8; MAX_RECORD];
        let used = self.state.read_message(ciphertext, &mut plaintext)?;
        plaintext.truncate(used);
        Ok(plaintext)
    }
}

/// Establishes an authenticated Noise XX transport pair bound to the opaque
/// relay id. In production the three handshake messages are themselves relay
/// frames; this helper also gives headless clients a direct way to validate the
/// cryptographic boundary without a phone UI.
pub fn establish_relay_ciphers(
    initiator_private_key: &str,
    initiator_expected_peer_key: &str,
    responder_private_key: &str,
    responder_expected_peer_key: &str,
    relay_id: &str,
) -> anyhow::Result<(RelayCipher, RelayCipher)> {
    if !valid_opaque_id(relay_id) {
        bail!("relay id must be a 16-byte hex value");
    }
    let params: NoiseParams = NOISE_PATTERN.parse().expect("valid Noise pattern");
    let initiator_private = decode_static_key(initiator_private_key)?;
    let initiator_expected_peer = decode_static_key(initiator_expected_peer_key)?;
    let responder_private = decode_static_key(responder_private_key)?;
    let responder_expected_peer = decode_static_key(responder_expected_peer_key)?;
    let prologue = format!("latch-relay-v1:{relay_id}");
    let mut initiator = Builder::new(params.clone())
        .prologue(prologue.as_bytes())
        .local_private_key(&initiator_private)
        .build_initiator()?;
    let mut responder = Builder::new(params)
        .prologue(prologue.as_bytes())
        .local_private_key(&responder_private)
        .build_responder()?;
    let mut message = vec![0_u8; MAX_RECORD];
    let first = initiator.write_message(&[], &mut message)?;
    responder.read_message(&message[..first], &mut vec![0_u8; MAX_RECORD])?;
    let second = responder.write_message(&[], &mut message)?;
    initiator.read_message(&message[..second], &mut vec![0_u8; MAX_RECORD])?;
    let third = initiator.write_message(&[], &mut message)?;
    responder.read_message(&message[..third], &mut vec![0_u8; MAX_RECORD])?;
    let initiator_peer = initiator
        .get_remote_static()
        .ok_or_else(|| anyhow!("relay peer did not present an identity"))?;
    let responder_peer = responder
        .get_remote_static()
        .ok_or_else(|| anyhow!("relay peer did not present an identity"))?;
    if !constant_time_eq(initiator_peer, &initiator_expected_peer)
        || !constant_time_eq(responder_peer, &responder_expected_peer)
    {
        bail!("relay peer identity did not match the paired device");
    }
    Ok((
        RelayCipher {
            state: initiator.into_transport_mode()?,
        },
        RelayCipher {
            state: responder.into_transport_mode()?,
        },
    ))
}

/// A safe default for newly paired phones.
pub const DEFAULT_PERMISSION: DevicePermission = DevicePermission::Interact;

/// QR-compatible, one-time pairing material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PairingMaterial {
    /// Version for mobile parsers.
    pub format_version: u8,
    /// Opaque, one-time pairing identifier.
    pub pairing_id: String,
    /// High-entropy one-time secret. This is only emitted at creation time.
    pub secret: String,
    /// Pinned public identity of the Mac, hex encoded.
    pub mac_public_key: String,
    /// Unix timestamp after which confirmation is refused.
    pub expires_at: u64,
}

/// Public data for an enrolled phone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    /// Opaque device identifier.
    pub device_id: String,
    /// User-approved display label.
    pub name: String,
    /// Effective permission.
    pub permission: DevicePermission,
    /// Whether this device can no longer establish a connection.
    pub revoked: bool,
    /// The device's row in the control-plane directory, when one was recorded
    /// at pairing. Present so the app can mirror a grant change; absent for
    /// devices paired before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_device_id: Option<String>,
    /// Short authentication phrase for the pairing that enrolled this device.
    /// Present only on the confirmation answer: it is what the Mac shows so
    /// the person can check it against the phone's screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_phrase: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Identity {
    device_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    private_key: String,
    public_key: String,
    #[serde(default = "initial_key_generation")]
    key_generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    enabled: bool,
    #[serde(default = "enabled_by_default")]
    relay_enabled: bool,
    /// The strict form of the relay switch. It refuses relay admission the same
    /// way `relay_enabled: false` does, and additionally tells the desktop app
    /// to publish host candidates only, so nothing this Mac advertises can be
    /// paired with a relay candidate in the first place.
    #[serde(default)]
    never_relay: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_enabled: true,
            never_relay: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PairingRecord {
    pairing_id: String,
    #[serde(default)]
    secret_digest: String,
    expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    name: String,
    public_key: String,
    permission: DevicePermission,
    revoked: bool,
    /// The row this phone occupies in the control-plane directory, recorded
    /// when it enrolled. It exists so a local grant change can be mirrored to
    /// the directory the phone reads its own permission from. It is a
    /// convenience, never an authority: a device paired before this field
    /// existed simply has no mirror, and this Mac still enforces the grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    control_plane_device_id: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DeviceStore {
    devices: Vec<DeviceRecord>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditEvent<'a> {
    timestamp: u64,
    event: &'a str,
    device_id: Option<&'a str>,
    result: &'a str,
}

/// Content-free local support bundle. It deliberately excludes endpoint
/// addresses, names, public keys, pairing material, and application data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsExport {
    /// Version of this privacy contract.
    pub format_version: u8,
    /// Whether new remote connections are allowed.
    pub remote_access_enabled: bool,
    /// Whether the hosted relay path may be selected.
    pub relay_enabled: bool,
    /// Count only; device identities and labels are excluded.
    pub paired_devices: usize,
    /// Count only; device identities and labels are excluded.
    pub revoked_devices: usize,
    /// Coarse event counters from the bounded local audit trail.
    pub event_counts: HashMap<String, u64>,
    /// How the connections this Mac actually served were routed.
    pub path_selection: PathSelectionMetrics,
}

/// Non-content path-selection counters.
///
/// These answer one question — of the connections this Mac served, how many
/// went direct and how many were relayed — without naming a device, a network,
/// an address, or a moment. They are derived from the bounded audit trail, so
/// they cover the retained window rather than all time, and a busy Mac
/// eventually ages the oldest ones out. That is the right trade for a counter
/// that must never become a connection log.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PathSelectionMetrics {
    /// Authorized connections by route slug, including routes with no hits, so
    /// a reader can tell "never relayed" from "not instrumented".
    pub routes: BTreeMap<String, u64>,
    /// Authorized connections counted here in total.
    pub connections: u64,
    /// Of those, the ones that were not relayed.
    pub direct: u64,
    /// Of those, the ones that were relayed.
    pub relay: u64,
    /// ICE answers the helper attempted, whether or not a pair was nominated.
    pub ice_answers: u64,
    /// ICE answers that produced a connected data channel.
    pub ice_answers_connected: u64,
}

impl PathSelectionMetrics {
    /// Share of served connections that were relayed, `None` before any.
    ///
    /// Expressed as a fraction rather than a percentage so the caller decides
    /// how to round; a release gate reading "0.02" should not owe its verdict
    /// to this function's choice of decimal places.
    pub fn relay_share(&self) -> Option<f64> {
        (self.connections > 0).then(|| self.relay as f64 / self.connections as f64)
    }
}

/// Owner-facing lifecycle snapshot for the desktop app.
///
/// This is the only surface that reveals the Mac's own public identity. It
/// deliberately excludes the supervised gateway address, the gateway bearer
/// token, paired-device public keys, and any session content, so a desktop
/// status poll can never become a path to the plaintext gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatus {
    /// Version of this status contract.
    pub format_version: u8,
    /// Whether new remote connections are allowed.
    pub enabled: bool,
    /// Whether the hosted relay path may be selected.
    pub relay_enabled: bool,
    /// Whether this Mac refuses the relay outright. Stricter than
    /// `relay_enabled: false`: the desktop app also narrows presence to host
    /// candidates, so a direct or tailnet address is the only thing a phone
    /// ever has to work with.
    #[serde(default)]
    pub never_relay: bool,
    /// Opaque Mac device identifier, present once an identity exists.
    pub device_id: Option<String>,
    /// The Mac's pinned Noise static public key, hex encoded. Phones verify
    /// this value during pairing, so it is public by design.
    pub public_key: Option<String>,
    /// Identity key generation, for future rotation surfaces.
    pub key_generation: Option<u64>,
    /// Count of enrolled devices, including revoked ones.
    pub paired_devices: usize,
    /// Count of enrolled devices that can no longer connect.
    pub revoked_devices: usize,
    /// The LAN listener address a supervised helper is currently advertising,
    /// or `None` when no helper is running. This is the authenticated
    /// transport listener, never the plaintext gateway.
    pub listener_address: Option<String>,
    /// The running helper's ICE agent description. The desktop app publishes
    /// these credentials and candidates as presence; it does not invent its
    /// own, because the helper is the process that answers the checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ice: Option<IceReadiness>,
    /// How many phones currently hold an authenticated stream to this Mac.
    ///
    /// Counted after the Noise handshake and the route authorization, never at
    /// accept: this number decides whether the desktop app keeps the Mac awake,
    /// and anything that can open a socket must not be able to move it.
    #[serde(default)]
    pub active_connections: usize,
}

/// One direction of an authenticated peer transport, framed as discrete byte
/// records.
///
/// The LAN listener supplies TCP halves with a two-byte length prefix; the
/// helper's ICE data channel supplies SCTP messages that already carry their
/// own boundaries. Everything above this seam — the Noise handshake, the single
/// route authorization, and the 250ms device-state check — is identical for
/// both, which is the point: a phone off the LAN gets the same enforcement as a
/// phone on it, not a second implementation of it.
#[async_trait]
pub trait PeerReader: Send {
    /// Reads one record, giving up after the proxy inactivity deadline.
    async fn read_record(&mut self) -> anyhow::Result<Vec<u8>>;
}

/// The write half of [`PeerReader`]'s transport.
#[async_trait]
pub trait PeerWriter: Send {
    /// Writes one record.
    async fn write_record(&mut self, record: &[u8]) -> anyhow::Result<()>;
}

/// How an accepted peer stream reaches the phone.
///
/// This exists so the direct-versus-relay rate is measurable after the fact
/// rather than guessed at. It is derived from candidate types and the listener
/// that accepted the stream: no address, port, interface name, or peer
/// identity is carried in it, which is what lets it be written to the audit
/// trail and read back out of the content-free diagnostics bundle.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerRoute {
    /// The authenticated TCP listener on this network accepted it.
    Lan,
    /// ICE nominated a host pair: the same network, or a tunnel interface
    /// such as a tailnet that presents as one.
    DirectHost,
    /// ICE nominated a reflexive pair: a hole was punched through a NAT.
    DirectReflexive,
    /// ICE nominated a relayed pair: the bytes take the TURN detour.
    Relay,
    /// The transport produced a stream without saying how. Counted rather than
    /// dropped, so a gap in instrumentation cannot silently flatter the
    /// direct rate.
    #[default]
    Unknown,
}

impl PeerRoute {
    /// Stable slug for counters and the audit trail.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Lan => "lan",
            Self::DirectHost => "direct_host",
            Self::DirectReflexive => "direct_reflexive",
            Self::Relay => "relay",
            Self::Unknown => "unknown",
        }
    }

    /// Every slug, so a counter map can name a zero as well as a hit.
    pub fn all() -> [Self; 5] {
        [
            Self::Lan,
            Self::DirectHost,
            Self::DirectReflexive,
            Self::Relay,
            Self::Unknown,
        ]
    }
}

/// An accepted peer transport, split so each direction can be driven by its
/// own task.
pub struct PeerStream {
    /// Records arriving from the peer.
    pub reader: Box<dyn PeerReader>,
    /// Records sent to the peer.
    pub writer: Box<dyn PeerWriter>,
    /// How this stream reached the phone. Metrics only: nothing in the proxy
    /// grants, refuses, or shortcuts anything on the strength of it, because
    /// the Noise handshake is what decides who the peer is on every path.
    pub route: PeerRoute,
}

impl PeerStream {
    /// Frames an accepted LAN socket.
    pub fn from_tcp(stream: TcpStream) -> Self {
        let (reader, writer) = stream.into_split();
        Self {
            reader: Box::new(TcpPeerReader(reader)),
            writer: Box::new(TcpPeerWriter(writer)),
            route: PeerRoute::Lan,
        }
    }
}

struct TcpPeerReader(tokio::net::tcp::OwnedReadHalf);
struct TcpPeerWriter(tokio::net::tcp::OwnedWriteHalf);

#[async_trait]
impl PeerReader for TcpPeerReader {
    async fn read_record(&mut self) -> anyhow::Result<Vec<u8>> {
        read_frame_with_idle_timeout(&mut self.0).await
    }
}

#[async_trait]
impl PeerWriter for TcpPeerWriter {
    async fn write_record(&mut self, record: &[u8]) -> anyhow::Result<()> {
        write_frame(&mut self.0, record).await
    }
}

/// An additional source of authenticated peer streams, injected by the
/// dedicated helper.
///
/// The ICE/DTLS/SCTP stack deliberately lives outside this crate: the
/// terminal-facing `latch` binary must not link the internet-facing protocol
/// code, and `latch-remote` is its sole owner. `serve_lan` therefore takes the
/// transport as a boundary and, once a stream is accepted, cannot tell it apart
/// from a LAN socket.
#[async_trait]
pub trait PeerTransport: Send + Sync + 'static {
    /// Gathers candidates and returns the description presence must publish.
    async fn start(&self) -> anyhow::Result<IceReadiness>;
    /// Applies one approved rendezvous offer collected by the desktop app.
    async fn offer(&self, offer: RemoteOffer) -> anyhow::Result<()>;
    /// The description presence should currently publish. Re-gathering after a
    /// connection changes the agent's ports, so the readiness document is
    /// refreshed from this rather than written once at startup.
    async fn local_description(&self) -> Option<IceReadiness>;
    /// Yields the next accepted peer stream, or `None` when the transport ends.
    ///
    /// This is driven inside a `select!`, so an implementation must be
    /// cancel-safe: dropping the returned future may not consume a stream that
    /// has already been accepted.
    async fn accept(&self) -> Option<PeerStream>;
}

/// One ICE candidate in the control plane's published shape.
///
/// This is transport metadata only: an interface address and its ordering
/// parameters. It names no session, no route, and no credential.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IceCandidateRecord {
    #[serde(rename = "type")]
    pub candidate_type: String,
    pub priority: u32,
    pub foundation: String,
    pub component: u16,
    pub protocol: String,
    /// `ip:port` literal.
    pub address: String,
    // Absent rather than null is the ordinary form here: a Swift encoder omits
    // nil, and these three are meaningless for most candidates.
    #[serde(default)]
    pub related_address: Option<String>,
    #[serde(default)]
    pub related_port: Option<u16>,
    #[serde(default)]
    pub tcp_type: Option<String>,
    pub expires_at: u64,
}

impl IceCandidateRecord {
    /// Applies the control plane's publication rules locally so a malformed
    /// candidate is a helper-side error rather than a 400 the desktop app's
    /// presence loop retries into.
    pub fn validate(&self, now: u64) -> anyhow::Result<()> {
        if !matches!(
            self.candidate_type.as_str(),
            "host" | "srflx" | "prflx" | "relay"
        ) {
            bail!("{} is not an ICE candidate type", self.candidate_type);
        }
        if !matches!(self.protocol.as_str(), "udp" | "tcp") {
            bail!("{} is not an ICE transport", self.protocol);
        }
        if self.component != 1 && self.component != 2 {
            bail!("an ICE component is 1 or 2");
        }
        if self.foundation.is_empty() || self.foundation.len() > 32 {
            bail!("an ICE foundation is 1 to 32 characters");
        }
        let address: SocketAddr = self
            .address
            .parse()
            .context("an ICE candidate address is an ip:port literal")?;
        if address.port() == 0 || !routable_candidate_ip(address.ip()) {
            bail!("an ICE candidate address must be routable and non-loopback");
        }
        if self.related_address.is_some() != self.related_port.is_some() {
            bail!("relatedAddress and relatedPort are published together or not at all");
        }
        if let Some(related) = &self.related_address {
            let ip: IpAddr = related
                .parse()
                .context("a related address is an IP literal")?;
            if ip.is_loopback() {
                bail!("a related address must not be loopback");
            }
        }
        if let Some(tcp_type) = &self.tcp_type {
            if !matches!(tcp_type.as_str(), "active" | "passive" | "so") {
                bail!("{tcp_type} is not a TCP candidate type");
            }
            if self.protocol != "tcp" {
                bail!("tcpType applies only to TCP candidates");
            }
        }
        if self.expires_at <= now || self.expires_at > now + PRESENCE_LIFETIME.as_secs() {
            bail!("an ICE candidate has an invalid lifetime");
        }
        Ok(())
    }
}

/// The helper's gathered ICE agent description.
///
/// The password is a STUN short-term credential, not a capability: it
/// authenticates connectivity checks and grants nothing on its own. It is
/// published to paired peers through the control plane by design, which is why
/// it travels the same owner-facing readiness channel as the listener address
/// and not the private gateway credential path.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IceReadiness {
    pub ufrag: String,
    pub password: String,
    pub candidates: Vec<IceCandidateRecord>,
}

impl IceReadiness {
    /// Mints one agent's short-term ICE credentials.
    ///
    /// RFC 8445 requires at least 24 bits of ufrag and 128 bits of password.
    /// These are 48 and 144, inside the control plane's length bounds and its
    /// `[A-Za-z0-9+/_=-]` alphabet.
    pub fn generate_credentials() -> anyhow::Result<(String, String)> {
        Ok((random_hex(6)?, random_hex(18)?))
    }
}

/// The bounded lifetime a freshly gathered candidate is published with. It
/// matches directory presence, so a candidate can never outlive the record
/// carrying it.
pub fn candidate_lifetime_from_now() -> u64 {
    unix_time() + PRESENCE_LIFETIME.as_secs()
}

/// One approved rendezvous offer handed to the helper's ICE agent.
///
/// It carries transport parameters only. Reaching the agent authorizes nothing:
/// the Noise handshake still pins the paired identity and the local device
/// store still decides what that identity may do.
#[allow(missing_docs)] // The enclosing type and serialized field names are the contract docs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOffer {
    pub request_id: String,
    pub peer_device_id: String,
    pub ice_ufrag: String,
    pub ice_pwd: String,
    pub candidates: Vec<IceCandidateRecord>,
    pub expires_at: u64,
}

impl RemoteOffer {
    /// Bounds everything the helper will act on before it reaches an ICE agent.
    pub fn validate(&self, now: u64) -> anyhow::Result<()> {
        if !valid_opaque_id(&self.request_id) || !valid_opaque_id(&self.peer_device_id) {
            bail!("rendezvous offer contains an invalid opaque identifier");
        }
        if !valid_ice_credential(&self.ice_ufrag) || !valid_ice_credential(&self.ice_pwd) {
            bail!("rendezvous offer contains invalid ICE credentials");
        }
        if self.candidates.is_empty() || self.candidates.len() > MAX_OFFER_CANDIDATES {
            bail!("rendezvous offer must contain between 1 and {MAX_OFFER_CANDIDATES} candidates");
        }
        if self.expires_at <= now || self.expires_at > now + PRESENCE_LIFETIME.as_secs() {
            bail!("rendezvous offer has an invalid lifetime");
        }
        for candidate in &self.candidates {
            candidate.validate(now)?;
        }
        Ok(())
    }
}

/// RFC 8445 bounds a ufrag and password at 4 and 22 characters minimum; the
/// control plane's alphabet is base64url plus `+/=`.
fn valid_ice_credential(value: &str) -> bool {
    (4..=256).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/_=-".contains(&byte))
}

/// Readiness document written by `remote-access lan-serve` for a supervising
/// desktop app.
///
/// It names the authenticated LAN listener only. The supervised gateway
/// address and its per-launch bearer token are never written here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanReadiness {
    /// Version of this readiness contract.
    pub format_version: u8,
    /// Bound address of the authenticated LAN listener.
    pub address: String,
    /// Opaque Mac device identifier serving this listener.
    pub device_id: String,
    /// Process id of the helper serving this listener. A reader uses it to
    /// distinguish a live listener from a document left behind by a crash.
    pub pid: u32,
    /// The helper's ICE agent description, absent when the helper runs without
    /// one. Presence publishes these candidates rather than the bare listener
    /// address, so a phone off the LAN has something to run checks against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ice: Option<IceReadiness>,
    /// Authenticated peer streams this helper is currently carrying. Rewritten
    /// whenever the count changes, because a supervising desktop app uses it to
    /// decide whether this Mac may go to sleep.
    #[serde(default)]
    pub active_connections: usize,
}

#[derive(Clone)]
struct Paths {
    root: PathBuf,
}

impl Paths {
    fn new(home: &LatchHome) -> Self {
        Self {
            root: home.remote_access_dir(),
        }
    }

    fn identity(&self) -> PathBuf {
        self.root.join("identity.json")
    }
    #[cfg(any(not(target_os = "macos"), test))]
    fn identity_secret(&self) -> PathBuf {
        self.root.join("identity.key")
    }
    fn settings(&self) -> PathBuf {
        self.root.join("settings.json")
    }
    fn pairings(&self) -> PathBuf {
        self.root.join("pairings.json")
    }
    fn devices(&self) -> PathBuf {
        self.root.join("devices.json")
    }
    fn audit(&self) -> PathBuf {
        self.root.join("audit.jsonl")
    }
    fn runtime(&self) -> PathBuf {
        self.root.join("runtime")
    }
    fn lan_readiness(&self) -> PathBuf {
        self.runtime().join("lan-ready.json")
    }
    fn offers(&self) -> PathBuf {
        self.runtime().join("offers")
    }
}

/// Enables or disables the local remote-access service.
pub fn set_enabled(home: &LatchHome, enabled: bool) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    ensure_root(&paths)?;
    if enabled {
        let _ = identity(&paths)?;
    }
    let mut settings: Settings = read_json_or_default(&paths.settings())?;
    settings.enabled = enabled;
    write_json(&paths.settings(), &settings)?;
    if !enabled {
        // Pending QR material is authority to add a device. Disabling remote
        // access invalidates it immediately and removes stale gateway secrets.
        write_json::<Vec<PairingRecord>>(&paths.pairings(), &Vec::new())?;
        let _ = fs::remove_file(paths.runtime().join("gateway.token"));
        let _ = fs::remove_file(paths.runtime().join("gateway-ready.json"));
        let _ = fs::remove_file(paths.lan_readiness());
    }
    audit(
        &paths,
        if enabled {
            "remote_access_enabled"
        } else {
            "remote_access_disabled"
        },
        None,
        "ok",
    )
}

/// How this Mac treats the hosted relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayMode {
    /// Relay admission is permitted when a direct path cannot be found.
    Enabled,
    /// Relay admission is refused. Direct and LAN paths are unaffected, which
    /// gives operators a narrow incident-response switch.
    Disabled,
    /// Relay admission is refused *and* presence is narrowed to host
    /// candidates, so the only addresses a phone ever receives are ones it can
    /// reach directly — a LAN address or a tailnet one.
    Never,
}

/// Sets the relay policy.
pub fn set_relay_mode(home: &LatchHome, mode: RelayMode) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    ensure_root(&paths)?;
    let mut settings: Settings = read_json_or_default(&paths.settings())?;
    settings.relay_enabled = mode == RelayMode::Enabled;
    settings.never_relay = mode == RelayMode::Never;
    write_json(&paths.settings(), &settings)?;
    audit(
        &paths,
        match mode {
            RelayMode::Enabled => "relay_enabled",
            RelayMode::Disabled => "relay_disabled",
            RelayMode::Never => "relay_never",
        },
        None,
        "ok",
    )
}

/// Independently enables or disables relay fallback.
///
/// Turning the relay back on also clears the stricter never-relay state: a
/// person who allows the relay is not left with presence still narrowed to
/// addresses that cannot use it.
pub fn set_relay_enabled(home: &LatchHome, enabled: bool) -> anyhow::Result<()> {
    set_relay_mode(
        home,
        if enabled {
            RelayMode::Enabled
        } else {
            RelayMode::Disabled
        },
    )
}

/// Returns whether relay admission is currently permitted. Hosted control
/// plane adapters must check this before issuing a [`RelayTicket`].
pub fn relay_enabled(home: &LatchHome) -> anyhow::Result<bool> {
    let settings: Settings = read_json_or_default(&Paths::new(home).settings())?;
    Ok(settings.enabled && settings.relay_enabled && !settings.never_relay)
}

/// Creates a five-minute QR-compatible pairing record.
pub fn create_pairing(home: &LatchHome) -> anyhow::Result<PairingMaterial> {
    let paths = Paths::new(home);
    ensure_enabled(&paths)?;
    let identity = identity(&paths)?;
    let now = unix_time();
    let record = PairingRecord {
        pairing_id: random_hex(16)?,
        secret_digest: String::new(),
        expires_at: now + PAIRING_LIFETIME.as_secs(),
    };
    let secret = random_hex(32)?;
    let record = PairingRecord {
        secret_digest: pairing_secret_digest(&record.pairing_id, &secret),
        ..record
    };
    let mut records: Vec<PairingRecord> = read_json_or_default(&paths.pairings())?;
    records.retain(|existing| existing.expires_at > now);
    if records.len() >= MAX_PENDING_PAIRINGS {
        bail!("too many pending pairing requests");
    }
    records.push(record.clone());
    write_json(&paths.pairings(), &records)?;
    audit(&paths, "pairing_created", None, "ok")?;
    Ok(PairingMaterial {
        format_version: 1,
        pairing_id: record.pairing_id,
        secret,
        mac_public_key: identity.public_key,
        expires_at: record.expires_at,
    })
}

/// Confirms a phone identity after it has presented the QR secret out of band.
pub fn confirm_pairing(
    home: &LatchHome,
    pairing_id: &str,
    secret: &str,
    device_public_key: &str,
    name: &str,
    permission: DevicePermission,
    control_plane_device_id: Option<&str>,
) -> anyhow::Result<DeviceSummary> {
    let paths = Paths::new(home);
    ensure_enabled(&paths)?;
    let key = decode_static_key(device_public_key)?;
    let _ = key;
    let now = unix_time();
    let mut pairings: Vec<PairingRecord> = read_json_or_default(&paths.pairings())?;
    let index = pairings
        .iter()
        .position(|record| record.pairing_id == pairing_id)
        .ok_or_else(|| anyhow!("pairing record not found"))?;
    let record = pairings.remove(index);
    write_json(&paths.pairings(), &pairings)?;
    let supplied_digest = pairing_secret_digest(pairing_id, secret);
    if record.expires_at <= now
        || !constant_time_eq(record.secret_digest.as_bytes(), supplied_digest.as_bytes())
    {
        audit(&paths, "pairing_confirmed", None, "rejected")?;
        bail!("pairing material is expired or invalid");
    }
    if name.trim().is_empty() || name.len() > 80 {
        bail!("device name must be between 1 and 80 characters");
    }
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    if store.devices.len() >= MAX_PAIRED_DEVICES {
        bail!("paired device limit reached");
    }
    if store
        .devices
        .iter()
        .any(|device| device.public_key == device_public_key)
    {
        bail!("device identity is already paired");
    }
    let record = DeviceRecord {
        device_id: random_hex(16)?,
        name: name.trim().to_owned(),
        public_key: device_public_key.to_owned(),
        permission,
        revoked: false,
        control_plane_device_id: control_plane_device_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    };
    let mut summary = summary(&record);
    // The phone derives the identical words from the identical transcript, so
    // the person comparing the two screens is the check that catches a
    // substituted Mac or device key. See `PairingPhrase` in LatchMobileKit.
    summary.pairing_phrase = Some(pairing_phrase(
        pairing_id,
        &identity(&paths)?.public_key,
        device_public_key,
    ));
    store.devices.push(record);
    write_json(&paths.devices(), &store)?;
    audit(&paths, "pairing_confirmed", Some(&summary.device_id), "ok")?;
    Ok(summary)
}

/// Lists paired devices without exposing public keys or credentials.
pub fn list_devices(home: &LatchHome) -> anyhow::Result<Vec<DeviceSummary>> {
    let paths = Paths::new(home);
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    Ok(store.devices.iter().map(summary).collect())
}

/// Changes a paired device's permission.
pub fn grant(
    home: &LatchHome,
    device_id: &str,
    permission: DevicePermission,
) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    let device = store
        .devices
        .iter_mut()
        .find(|device| device.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    if device.revoked {
        bail!("device is revoked");
    }
    device.permission = permission;
    write_json(&paths.devices(), &store)?;
    audit(&paths, "permission_changed", Some(device_id), "ok")
}

/// Revokes a device. Active LAN connections poll this record and close promptly.
pub fn revoke(home: &LatchHome, device_id: &str) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    let device = store
        .devices
        .iter_mut()
        .find(|device| device.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    device.revoked = true;
    write_json(&paths.devices(), &store)?;
    audit(&paths, "device_revoked", Some(device_id), "ok")
}

/// Replaces a paired device's Noise identity through the owner-authorized
/// local control surface. The old key stops authenticating immediately, while
/// the device record and grants remain intact so recovery needs no re-pairing.
pub fn rotate_device_key(
    home: &LatchHome,
    device_id: &str,
    new_public_key: &str,
) -> anyhow::Result<()> {
    decode_static_key(new_public_key)?;
    let paths = Paths::new(home);
    let mut store: DeviceStore = read_json_or_default(&paths.devices())?;
    if store
        .devices
        .iter()
        .any(|device| device.device_id != device_id && device.public_key == new_public_key)
    {
        bail!("device identity is already paired");
    }
    let device = store
        .devices
        .iter_mut()
        .find(|device| device.device_id == device_id)
        .ok_or_else(|| anyhow!("device not found"))?;
    if device.revoked {
        bail!("device is revoked");
    }
    device.public_key = new_public_key.to_owned();
    write_json(&paths.devices(), &store)?;
    audit(&paths, "device_key_rotated", Some(device_id), "ok")
}

/// Reads the privacy-minimized local audit trail.
pub fn read_audit(home: &LatchHome) -> anyhow::Result<Vec<serde_json::Value>> {
    let paths = Paths::new(home);
    if !paths.audit().exists() {
        return Ok(Vec::new());
    }
    String::from_utf8(read_private_bytes(&paths.audit())?)
        .context("audit log is not UTF-8")?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .context("invalid audit log")
}

/// Reports the owner-facing remote-access lifecycle state.
///
/// Reading status never creates an identity and never starts a listener, so
/// the desktop app can poll it while remote access is off.
pub fn status(home: &LatchHome) -> anyhow::Result<RemoteAccessStatus> {
    let paths = Paths::new(home);
    let settings: Settings = read_json_or_default(&paths.settings())?;
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    let identity: Option<Identity> = if paths.identity().is_file() {
        Some(read_json(&paths.identity())?)
    } else {
        None
    };
    let readiness = read_lan_readiness(&paths);
    let ice = readiness
        .as_ref()
        .and_then(|readiness| readiness.ice.clone());
    // A helper that is gone carries no connections, so both fields fall away
    // together rather than leaving a stale count behind a stopped listener.
    let active_connections = readiness
        .as_ref()
        .map_or(0, |readiness| readiness.active_connections);
    let listener_address = readiness.map(|readiness| readiness.address);
    Ok(RemoteAccessStatus {
        format_version: 1,
        enabled: settings.enabled,
        relay_enabled: settings.relay_enabled,
        never_relay: settings.never_relay,
        device_id: identity.as_ref().map(|value| value.device_id.clone()),
        public_key: identity.as_ref().map(|value| value.public_key.clone()),
        key_generation: identity.as_ref().map(|value| value.key_generation),
        paired_devices: store.devices.len(),
        revoked_devices: store.devices.iter().filter(|device| device.revoked).count(),
        listener_address,
        ice,
        active_connections,
    })
}

/// Reads the helper's readiness document, ignoring a missing or malformed one
/// so a stale supervisor cannot make status reporting fail.
///
/// A document whose helper is gone is discarded rather than reported. Without
/// this, a helper killed hard enough to skip its own cleanup would leave the
/// desktop app claiming a listener that nothing is serving.
fn read_lan_readiness(paths: &Paths) -> Option<LanReadiness> {
    let readiness = read_json::<LanReadiness>(&paths.lan_readiness()).ok()?;
    if helper_is_running(readiness.pid) {
        return Some(readiness);
    }
    let _ = fs::remove_file(paths.lan_readiness());
    None
}

/// Signal-free liveness probe for the helper process.
fn helper_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: signal 0 performs the permission and existence checks without
    // delivering anything to the target.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Records one ICE answer outcome from the helper.
///
/// The ICE stack lives in `latch-remote`, but the audit trail lives here and
/// there must be exactly one of it: a helper that kept its own counters would
/// give the diagnostics bundle a second, differently-bounded source of truth
/// about the same connections. The helper therefore writes through this
/// function, which takes no identity and no address — only whether an answer
/// reached a connected data channel.
pub fn record_ice_answer(home: &LatchHome, connected: bool) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    audit(
        &paths,
        ICE_ANSWER_EVENT,
        None,
        if connected { "connected" } else { "failed" },
    )
}

/// Builds an inspectable, content-free support bundle locally. Upload remains
/// an explicit caller action; this function performs no network activity.
pub fn diagnostics_export(home: &LatchHome) -> anyhow::Result<DiagnosticsExport> {
    let paths = Paths::new(home);
    let settings: Settings = read_json_or_default(&paths.settings())?;
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    let mut event_counts = HashMap::new();
    let mut path_selection = PathSelectionMetrics {
        routes: PeerRoute::all()
            .into_iter()
            .map(|route| (route.slug().to_owned(), 0))
            .collect(),
        ..PathSelectionMetrics::default()
    };
    for event in read_audit(home)? {
        let Some(name) = event.get("event").and_then(serde_json::Value::as_str) else {
            continue;
        };
        *event_counts.entry(name.to_owned()).or_insert(0) += 1;
        let result = event
            .get("result")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match name {
            PATH_SELECTED_EVENT => {
                let Some(route) = PeerRoute::all()
                    .into_iter()
                    .find(|route| route.slug() == result)
                else {
                    continue;
                };
                *path_selection
                    .routes
                    .entry(route.slug().to_owned())
                    .or_insert(0) += 1;
                path_selection.connections += 1;
                if route == PeerRoute::Relay {
                    path_selection.relay += 1;
                } else {
                    path_selection.direct += 1;
                }
            }
            ICE_ANSWER_EVENT => {
                path_selection.ice_answers += 1;
                if result == "connected" {
                    path_selection.ice_answers_connected += 1;
                }
            }
            _ => {}
        }
    }
    Ok(DiagnosticsExport {
        format_version: 1,
        remote_access_enabled: settings.enabled,
        relay_enabled: settings.relay_enabled,
        paired_devices: store.devices.len(),
        revoked_devices: store.devices.iter().filter(|device| device.revoked).count(),
        event_counts,
        path_selection,
    })
}

/// Starts the LAN listener and supervises a private, ephemeral gateway.
///
/// `transport` is the helper's ICE agent when one is available. `latch serve`
/// itself passes `None`: the terminal-facing binary neither owns nor links the
/// internet-facing protocol stack.
pub fn serve_lan(
    home: LatchHome,
    bind: SocketAddr,
    latch_bin: PathBuf,
    transport: Option<Arc<dyn PeerTransport>>,
) -> anyhow::Result<()> {
    let paths = Paths::new(&home);
    ensure_enabled(&paths)?;
    let identity = identity(&paths)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move { run_lan(paths, identity, bind, latch_bin, transport).await })
}

/// Refuses a LAN bind that would be meaningless or unsafe for the
/// authenticated transport listener.
///
/// A loopback bind is rejected because the authenticated listener exists to be
/// reachable by a paired phone; binding it to loopback would leave callers
/// assuming remote access works while nothing can ever reach it.
fn validate_lan_bind(bind: SocketAddr) -> anyhow::Result<()> {
    if bind.ip().is_loopback() {
        bail!("the authenticated LAN listener cannot be loopback-bound");
    }
    Ok(())
}

async fn run_lan(
    paths: Paths,
    identity: Identity,
    bind: SocketAddr,
    latch_bin: PathBuf,
    transport: Option<Arc<dyn PeerTransport>>,
) -> anyhow::Result<()> {
    validate_lan_bind(bind)?;
    ensure_private_directory(&paths.runtime())?;
    // Clear anything a previous run left behind before the agent exists: an
    // offer minted for a dead agent's credentials can never complete.
    let _ = fs::remove_dir_all(paths.offers());
    ensure_private_directory(&paths.offers())?;
    let ready = paths.runtime().join("gateway-ready.json");
    let token = paths.runtime().join("gateway.token");
    let mut gateway = start_gateway(&latch_bin, &token, &ready).await?;
    let readiness = wait_readiness(&ready).await?;
    let mut gateway_addr: SocketAddr = readiness
        .address
        .parse()
        .context("invalid supervised gateway address")?;
    if !gateway_addr.ip().is_loopback() {
        bail!("supervised gateway was not loopback-bound");
    }
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot bind LAN listener {bind}"))?;
    let local_addr = listener.local_addr()?;
    let _bonjour = advertise_bonjour(&identity, local_addr.port());
    // Gathering happens before the listener is advertised so the desktop app
    // never sees a readiness document that promises an agentless presence.
    let mut published_ice = match &transport {
        Some(transport) => Some(
            transport
                .start()
                .await
                .context("cannot start the ICE agent")?,
        ),
        None => None,
    };
    // The gauge the desktop app reads to decide whether this Mac may sleep. It
    // counts authenticated streams, so it is raised inside `proxy_connection`
    // after the handshake and the route authorization rather than here.
    let connections = Arc::new(AtomicUsize::new(0));
    let mut published_connections = 0_usize;
    write_lan_readiness(
        &paths,
        &identity,
        local_addr,
        published_ice.clone(),
        published_connections,
    )?;
    let _readiness_guard = LanReadinessGuard {
        path: paths.lan_readiness(),
    };
    audit(&paths, "lan_listener_started", None, "ok")?;
    let connection_limit = Arc::new(Semaphore::new(MAX_LAN_CONNECTIONS));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("cannot install the terminate handler")?;
    let mut offer_poll = tokio::time::interval(OFFER_POLL_INTERVAL);
    let mut connection_poll = tokio::time::interval(OFFER_POLL_INTERVAL);

    loop {
        let ctrl_c = tokio::signal::ctrl_c();
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                spawn_peer(
                    PeerStream::from_tcp(stream),
                    &paths,
                    &identity,
                    &token,
                    gateway_addr,
                    &connection_limit,
                    &connections,
                )?;
            }
            // An ICE data channel is a peer stream like any other once it is
            // up: it is handed the same handshake, the same authorization, and
            // the same connection budget as a socket off the LAN.
            peer = async {
                match &transport {
                    Some(transport) => transport.accept().await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(peer) = peer else {
                    bail!("the ICE transport stopped accepting");
                };
                spawn_peer(
                    peer,
                    &paths,
                    &identity,
                    &token,
                    gateway_addr,
                    &connection_limit,
                    &connections,
                )?;
            }
            _ = offer_poll.tick(), if transport.is_some() => {
                let transport = transport.as_ref().expect("guarded by the branch condition");
                for offer in drain_offers(&paths) {
                    let result = match transport.offer(offer).await {
                        Ok(()) => "ok",
                        Err(_) => "rejected",
                    };
                    audit(&paths, "rendezvous_offer", None, result)?;
                }
                // Answering an offer consumes the gathered agent, so the
                // replacement's ports have to reach presence. Rewriting only on
                // a real change keeps a desktop status poll stable in between.
                if let Some(description) = transport.local_description().await {
                    if published_ice.as_ref() != Some(&description) {
                        published_ice = Some(description);
                        write_lan_readiness(
                            &paths,
                            &identity,
                            local_addr,
                            published_ice.clone(),
                            published_connections,
                        )?;
                    }
                }
            }
            // The sleep assertion on the Mac hangs off this number, so it is
            // republished on its own timer rather than riding the offer poll,
            // which only runs when an ICE transport exists. Rewriting only on a
            // change keeps a status poll stable between connections.
            _ = connection_poll.tick() => {
                let current = connections.load(Ordering::Relaxed);
                if current != published_connections {
                    published_connections = current;
                    write_lan_readiness(
                        &paths,
                        &identity,
                        local_addr,
                        published_ice.clone(),
                        published_connections,
                    )?;
                }
            }
            status = gateway.wait() => {
                status.context("cannot wait for supervised gateway")?;
                audit(&paths, "gateway_restarted", None, "ok")?;
                gateway = start_gateway(&latch_bin, &token, &ready).await?;
                let readiness = wait_readiness(&ready).await?;
                gateway_addr = readiness.address.parse().context("invalid supervised gateway address")?;
                if !gateway_addr.ip().is_loopback() {
                    bail!("supervised gateway was not loopback-bound");
                }
            }
            signal = ctrl_c => {
                signal?;
                let _ = gateway.kill().await;
                audit(&paths, "lan_listener_stopped", None, "ok")?;
                return Ok(());
            }
            // A supervising desktop app stops the helper with SIGTERM, so
            // terminate must run the same cleanup as an interactive Ctrl-C.
            _ = terminate.recv() => {
                let _ = gateway.kill().await;
                audit(&paths, "lan_listener_stopped", None, "ok")?;
                return Ok(());
            }
        }
    }
}

/// Admits one accepted peer stream against the connection budget.
#[allow(clippy::too_many_arguments)]
fn spawn_peer(
    peer: PeerStream,
    paths: &Paths,
    identity: &Identity,
    token: &Path,
    gateway_addr: SocketAddr,
    connection_limit: &Arc<Semaphore>,
    connections: &Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let Ok(permit) = connection_limit.clone().try_acquire_owned() else {
        audit(paths, "connection_rejected", None, "capacity")?;
        return Ok(());
    };
    let paths = paths.clone();
    let identity = identity.clone();
    let token = token.to_path_buf();
    let connections = connections.clone();
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) =
            proxy_connection(peer, &paths, &identity, &token, gateway_addr, &connections).await
        {
            let result = if error.to_string().contains("handshake timed out") {
                "timeout"
            } else {
                "rejected"
            };
            let _ = audit(&paths, "connection_rejected", None, result);
        }
    });
    Ok(())
}

/// Performs the direct-path portion of rendezvous using simultaneous UDP
/// probes. Both endpoints bind locally and send first, so the desktop never
/// needs an inbound control-plane connection or a configured router rule.
///
/// This establishes a candidate path only. Application bytes remain on the
/// authenticated stream boundary; a caller must complete its mutually
/// authenticated transport handshake before forwarding gateway traffic.
pub async fn probe_direct_path(
    bind: SocketAddr,
    rendezvous_id: &str,
    candidates: &[DirectCandidate],
    timeout: Duration,
) -> anyhow::Result<SocketAddr> {
    let now = unix_time();
    if rendezvous_id.len() != 64 || !rendezvous_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("rendezvous id must be a 32-byte hex value");
    }
    let token = decode_hex(rendezvous_id)?;
    if token.len() != 32 {
        bail!("rendezvous id must be a 32-byte hex value");
    }
    if candidates.is_empty() || candidates.len() > 16 {
        bail!("rendezvous must include between 1 and 16 candidates");
    }
    for candidate in candidates {
        candidate.validate(now)?;
    }
    let socket = UdpSocket::bind(bind)
        .await
        .with_context(|| format!("cannot bind direct probe socket {bind}"))?;
    let mut probe = [0_u8; DIRECT_PROBE_SIZE];
    probe[..DIRECT_PROBE_MAGIC.len()].copy_from_slice(DIRECT_PROBE_MAGIC);
    probe[DIRECT_PROBE_MAGIC.len()..].copy_from_slice(&token);
    for candidate in candidates {
        socket.send_to(&probe, candidate.address).await?;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut received = [0_u8; DIRECT_PROBE_SIZE];
    loop {
        let result = tokio::time::timeout_at(deadline, socket.recv_from(&mut received)).await;
        let (read, peer) = result.map_err(|_| anyhow!("direct path probe timed out"))??;
        if read != DIRECT_PROBE_SIZE
            || !constant_time_eq(&received[..DIRECT_PROBE_MAGIC.len()], DIRECT_PROBE_MAGIC)
            || !constant_time_eq(&received[DIRECT_PROBE_MAGIC.len()..], &token)
        {
            continue;
        }
        // Replying makes simultaneous NAT mappings converge even when one
        // endpoint observed the first packet before it sent its own probe.
        socket.send_to(&probe, peer).await?;
        return Ok(peer);
    }
}

fn validate_presence(record: &PresenceRecord, now: u64) -> anyhow::Result<()> {
    if !valid_opaque_id(&record.device_id) || decode_static_key(&record.identity_key).is_err() {
        bail!("presence contains an invalid device identity");
    }
    if record.candidates.is_empty() || record.candidates.len() > 16 {
        bail!("presence must contain between 1 and 16 candidates");
    }
    if record.expires_at <= now || record.expires_at > now + PRESENCE_LIFETIME.as_secs() {
        bail!("presence has an invalid lifetime");
    }
    for candidate in &record.candidates {
        candidate.validate(now)?;
        if candidate.expires_at > record.expires_at {
            bail!("candidate outlives its presence record");
        }
    }
    Ok(())
}

fn validate_request(request: &RendezvousRequest, now: u64) -> anyhow::Result<()> {
    if !valid_opaque_id(&request.requester_device_id)
        || !valid_opaque_id(&request.target_device_id)
        || !valid_opaque_id(&request.request_id)
    {
        bail!("rendezvous contains an invalid opaque identifier");
    }
    if request.candidates.is_empty() || request.candidates.len() > 16 {
        bail!("rendezvous must contain between 1 and 16 candidates");
    }
    if request.expires_at <= now || request.expires_at > now + PRESENCE_LIFETIME.as_secs() {
        bail!("rendezvous has an invalid lifetime");
    }
    for candidate in &request.candidates {
        candidate.validate(now)?;
        if candidate.expires_at > request.expires_at {
            bail!("candidate outlives its rendezvous request");
        }
    }
    Ok(())
}

fn valid_opaque_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn routable_candidate_ip(ip: IpAddr) -> bool {
    !ip.is_unspecified() && !ip.is_loopback() && !ip.is_multicast()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Readiness {
    address: String,
}

/// The bind the supervised gateway is always launched with.
///
/// An ephemeral loopback port is the whole isolation guarantee: the gateway
/// speaks plaintext HTTP with a bearer token, so it must never be reachable
/// off-host. This is a constant rather than a parameter so no caller, helper,
/// or desktop app can widen it.
const GATEWAY_BIND: &str = "127.0.0.1:0";

/// Builds the supervised gateway argument vector.
///
/// Exposed separately so tests can assert the launch never requests a
/// non-loopback bind or `--allow-remote`.
fn gateway_args(token: &Path, ready: &Path) -> Vec<std::ffi::OsString> {
    vec![
        "serve".into(),
        "--bind".into(),
        GATEWAY_BIND.into(),
        "--token-file".into(),
        token.as_os_str().to_owned(),
        "--ready-file".into(),
        ready.as_os_str().to_owned(),
    ]
}

async fn start_gateway(binary: &Path, token: &Path, ready: &Path) -> anyhow::Result<Child> {
    let _ = fs::remove_file(ready);
    // Rotate the internal credential before every launch. Minting here avoids
    // the public `serve` command printing a newly-created secret to stderr.
    mint_token(token)?;
    Command::new(binary)
        .args(gateway_args(token, ready))
        .kill_on_drop(true)
        .spawn()
        .context("cannot start supervised loopback gateway")
}

/// Publishes the authenticated listener address for a supervising desktop app.
fn write_lan_readiness(
    paths: &Paths,
    identity: &Identity,
    address: SocketAddr,
    ice: Option<IceReadiness>,
    active_connections: usize,
) -> anyhow::Result<()> {
    if address.ip().is_loopback() {
        bail!("refusing to advertise a loopback LAN listener");
    }
    write_json(
        &paths.lan_readiness(),
        &LanReadiness {
            format_version: 1,
            address: address.to_string(),
            device_id: identity.device_id.clone(),
            pid: std::process::id(),
            ice,
            active_connections,
        },
    )
}

/// Holds the authenticated-connection count up for the life of one stream.
///
/// A guard rather than a pair of calls because every way out of
/// `proxy_connection` — a closed socket, a revoked device, a downgraded grant,
/// a task that panicked — has to put the count back. A leaked increment would
/// keep the Mac awake indefinitely with nothing connected to it.
struct ConnectionGauge {
    count: Arc<AtomicUsize>,
}

impl ConnectionGauge {
    fn raise(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self {
            count: count.clone(),
        }
    }
}

impl Drop for ConnectionGauge {
    fn drop(&mut self) {
        // Saturating rather than wrapping: an underflow here would publish
        // `usize::MAX` connections and pin the display awake forever.
        let _ = self
            .count
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_sub(1))
            });
    }
}

/// Records one approved rendezvous offer for a running helper to act on.
///
/// The desktop app reaches the helper the same way it reaches everything else
/// in this module: through the CLI, into the private runtime directory. One
/// file per request id rather than a shared queue, so a writer and the helper's
/// drain can never lose an offer to a read-modify-write race.
pub fn record_offer(home: &LatchHome, offer: &RemoteOffer) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    ensure_enabled(&paths)?;
    offer.validate(unix_time())?;
    ensure_private_directory(&paths.runtime())?;
    ensure_private_directory(&paths.offers())?;
    // `request_id` is checked as an opaque identifier above, so it cannot
    // traverse out of this directory or name anything but a hex file.
    write_json(
        &paths.offers().join(format!("{}.json", offer.request_id)),
        offer,
    )
}

/// Takes every pending offer document, discarding malformed and expired ones.
///
/// Draining is destructive on purpose: an offer is one-shot, and a document
/// left behind after a failed ICE attempt would have the helper retry a peer
/// that has already moved on to a fresh request.
fn drain_offers(paths: &Paths) -> Vec<RemoteOffer> {
    let Ok(entries) = fs::read_dir(paths.offers()) else {
        return Vec::new();
    };
    let now = unix_time();
    let mut offers = Vec::new();
    for entry in entries.flatten().take(MAX_PENDING_OFFERS) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let parsed = read_json::<RemoteOffer>(&path)
            .ok()
            .filter(|offer| offer.validate(now).is_ok());
        let _ = fs::remove_file(&path);
        if let Some(offer) = parsed {
            offers.push(offer);
        }
    }
    offers
}

/// Removes the readiness document when supervision ends, including on an
/// error path, so a desktop status poll never reports a listener that no
/// longer exists.
struct LanReadinessGuard {
    path: PathBuf,
}

impl Drop for LanReadinessGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

async fn wait_readiness(path: &Path) -> anyhow::Result<Readiness> {
    for _ in 0..100 {
        if let Ok(contents) = tokio::fs::read_to_string(path).await {
            return serde_json::from_str(&contents).context("invalid gateway readiness document");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("timed out waiting for supervised gateway readiness")
}

fn bonjour_service(identity: &Identity, port: u16) -> anyhow::Result<ServiceInfo> {
    let host_name = format!("latch-{}.local.", &identity.device_id[..12]);
    let properties = HashMap::from([("identityKey".to_owned(), identity.public_key.clone())]);
    // Address auto-discovery makes the SRV target resolvable on every active
    // LAN interface. The public key is only a discovery hint; Noise still
    // pins and verifies the paired identity during the handshake.
    ServiceInfo::new(
        "_latch-remote._tcp.local.",
        &format!("latch-{}", &identity.device_id[..12]),
        &host_name,
        (),
        port,
        properties,
    )
    .context("cannot build Bonjour service record")
    .map(ServiceInfo::enable_addr_auto)
}

fn advertise_bonjour(identity: &Identity, port: u16) -> anyhow::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("cannot start Bonjour service")?;
    let service = bonjour_service(identity, port)?;
    daemon
        .register(service)
        .context("cannot advertise Bonjour service")?;
    Ok(daemon)
}

async fn proxy_connection(
    peer: PeerStream,
    paths: &Paths,
    identity: &Identity,
    token_path: &Path,
    gateway_addr: SocketAddr,
    connections: &Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let PeerStream {
        mut reader,
        mut writer,
        route,
    } = peer;
    let (mut state, peer_static) = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        responder_handshake(reader.as_mut(), writer.as_mut(), identity),
    )
    .await
    .map_err(|_| anyhow!("handshake timed out"))??;
    let peer_key = hex_encode(&peer_static);
    let device = lookup_device(paths, &peer_key)?.ok_or_else(|| anyhow!("unpaired device"))?;
    if device.revoked {
        bail!("revoked device");
    }
    let mut initial = decrypt_peer_record(reader.as_mut(), &mut state).await?;
    while !initial.windows(4).any(|window| window == b"\r\n\r\n") {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request headers exceed limit");
        }
        initial.extend(decrypt_peer_record(reader.as_mut(), &mut state).await?);
    }
    let required_len = complete_initial_request_len(&initial)?;
    while initial.len() < required_len {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request exceeds limit");
        }
        initial.extend(decrypt_peer_record(reader.as_mut(), &mut state).await?);
    }
    let token = load_token(token_path)?;
    let (initial, required) = authorize_and_inject(initial, device.permission, &token)?;
    let gateway = TcpStream::connect(gateway_addr)
        .await
        .context("cannot connect to loopback gateway")?;
    let (mut gateway_reader, mut gateway_writer) = gateway.into_split();
    gateway_writer.write_all(&initial).await?;
    // Raised here, at the same point the connection is recorded, and dropped
    // however this function leaves. A stream that never proved a paired
    // identity has not kept this Mac awake for a moment.
    let _awake = ConnectionGauge::raise(connections);
    audit(paths, "connection_opened", Some(&device.device_id), "ok")?;
    // Recorded after authorization rather than at accept: a stream that never
    // proved a paired identity is not a path this Mac served, and counting it
    // would let anything that can open a socket move the rate.
    audit(
        paths,
        PATH_SELECTED_EVENT,
        Some(&device.device_id),
        route.slug(),
    )?;

    let state = Arc::new(Mutex::new(state));
    let outbound_state = state.clone();
    let mut outbound = tokio::spawn(async move {
        let mut buf = vec![0_u8; 16 * 1024];
        loop {
            let read = tokio::time::timeout(PROXY_IDLE_TIMEOUT, gateway_reader.read(&mut buf))
                .await
                .map_err(|_| anyhow!("gateway response idle timeout"))??;
            if read == 0 {
                return Ok::<(), anyhow::Error>(());
            }
            let encrypted = {
                let mut locked = outbound_state.lock().await;
                encrypt_transport_record(&mut locked, &buf[..read])?
            };
            writer.write_record(&encrypted).await?;
        }
    });
    let inbound_state = state.clone();
    let mut inbound = tokio::spawn(async move {
        loop {
            let encrypted = reader.read_record().await?;
            // Each direction has one task, so record ordering is preserved;
            // the shared state lock protects Noise counters only and never
            // spans socket I/O, which would deadlock request and response.
            let bytes = {
                let mut locked = inbound_state.lock().await;
                decrypt_transport_record(&mut locked, &encrypted)?
            };
            gateway_writer.write_all(&bytes).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    // The stream this connection carries — a terminal, most of all — outlives
    // the single authorization performed above, so the record is re-read on a
    // timer. Losing the grant matters as much as losing the pairing: a device
    // downgraded from control while it holds a terminal has to lose the
    // terminal, not keep writing to it until it disconnects on its own.
    let mut state_check = tokio::time::interval(Duration::from_millis(250));
    let result = tokio::select! {
        result = &mut outbound => result.context("encrypted response task failed")?,
        result = &mut inbound => result.context("encrypted request task failed")?,
        reason = async {
            loop {
                state_check.tick().await;
                match lookup_device(paths, &peer_key)? {
                    Some(current) if current.revoked => {
                        return Ok::<&'static str, anyhow::Error>("revoked");
                    }
                    Some(current) if !current.permission.permits(required) => {
                        return Ok("permission_downgraded");
                    }
                    Some(_) => {}
                    None => return Ok("revoked"),
                }
            }
        } => {
            audit(paths, "connection_closed", Some(&device.device_id), reason?)?;
            Ok(())
        }
    };
    // Dropping JoinHandles detaches their tasks. Abort both halves explicitly
    // so a device that loses its pairing or its grant loses the TCP stream as
    // soon as the state check wins.
    outbound.abort();
    inbound.abort();
    result
}

/// Checks one initial request against the device's grant and rewrites it for
/// the loopback gateway.
///
/// Returns the rewritten request together with the grant the matched route
/// requires, so a long-lived stream can keep re-checking the grant it was
/// admitted under rather than only the one it happened to hold at handshake.
fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers = std::str::from_utf8(&request[..end]).context("request headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words.next().ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = words.next().ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = words
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if words.next().is_some()
        || version != "HTTP/1.1"
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            bail!("malformed HTTP header");
        }
        let (name, value) = line.split_once(':').expect("header delimiter checked");
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
        websocket_upgrade |=
            name.eq_ignore_ascii_case("upgrade") && value.trim().eq_ignore_ascii_case("websocket");
    }
    let required_len = complete_initial_request_len(&request)?;
    if required_len != request.len() {
        bail!("HTTP pipelining is not permitted through remote access");
    }
    let (_, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through remote access"))?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    // `end` indexes the blank line, so `request[end..]` still carries the
    // header terminator. Emitting another CRLF here would leave a stray one in
    // the stream, which an upgraded gateway reads as a corrupt first frame.
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_GRANT_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(permission.as_header_value().as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(&request[end..]);
    Ok((injected, required))
}

fn complete_initial_request_len(request: &[u8]) -> anyhow::Result<usize> {
    let header_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers =
        std::str::from_utf8(&request[..header_end]).context("request headers are not UTF-8")?;
    let lengths = headers
        .split("\r\n")
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .map(str::parse::<usize>)
        .collect::<Result<Vec<_>, _>>()
        .context("invalid Content-Length")?;
    if lengths.len() > 1 {
        bail!("multiple Content-Length headers are not permitted");
    }
    let content_length = lengths.first().copied().unwrap_or(0);
    let total = header_end + 4 + content_length;
    if total > MAX_INITIAL_REQUEST {
        bail!("initial request exceeds limit");
    }
    Ok(total)
}

async fn responder_handshake(
    reader: &mut dyn PeerReader,
    writer: &mut dyn PeerWriter,
    identity: &Identity,
) -> anyhow::Result<(TransportState, Vec<u8>)> {
    let params: NoiseParams = NOISE_PATTERN.parse().expect("valid Noise pattern");
    let private = decode_static_key(&identity.private_key)?;
    let mut handshake = Builder::new(params)
        .local_private_key(&private)
        .build_responder()?;
    let first = reader.read_record().await?;
    let mut scratch = vec![0_u8; MAX_RECORD];
    handshake.read_message(&first, &mut scratch)?;
    let mut response = vec![0_u8; MAX_RECORD];
    let used = handshake.write_message(&[], &mut response)?;
    writer.write_record(&response[..used]).await?;
    let third = reader.read_record().await?;
    handshake.read_message(&third, &mut scratch)?;
    let peer_static = handshake
        .get_remote_static()
        .ok_or_else(|| anyhow!("peer did not present a static identity"))?
        .to_vec();
    Ok((handshake.into_transport_mode()?, peer_static))
}

async fn decrypt_peer_record(
    reader: &mut dyn PeerReader,
    state: &mut TransportState,
) -> anyhow::Result<Vec<u8>> {
    let encrypted = reader.read_record().await?;
    decrypt_transport_record(state, &encrypted)
}

#[cfg(test)]
async fn decrypt_record(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    state: &mut TransportState,
) -> anyhow::Result<Vec<u8>> {
    let encrypted = read_frame_with_idle_timeout(reader).await?;
    decrypt_transport_record(state, &encrypted)
}

fn decrypt_transport_record(
    state: &mut TransportState,
    encrypted: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let mut plain = vec![0_u8; MAX_RECORD];
    let used = state.read_message(encrypted, &mut plain)?;
    plain.truncate(used);
    Ok(plain)
}

#[cfg(test)]
async fn encrypt_record(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    state: &mut TransportState,
    plain: &[u8],
) -> anyhow::Result<()> {
    let encrypted = encrypt_transport_record(state, plain)?;
    write_frame(writer, &encrypted).await
}

fn encrypt_transport_record(state: &mut TransportState, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
    if plain.len() > MAX_RECORD - 32 {
        bail!("remote frame exceeds limit");
    }
    let mut encrypted = vec![0_u8; MAX_RECORD];
    let used = state.write_message(plain, &mut encrypted)?;
    encrypted.truncate(used);
    Ok(encrypted)
}

async fn read_frame_with_idle_timeout(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(PROXY_IDLE_TIMEOUT, read_frame(reader))
        .await
        .map_err(|_| anyhow!("remote connection idle timeout"))?
}

async fn read_frame(reader: &mut tokio::net::tcp::OwnedReadHalf) -> anyhow::Result<Vec<u8>> {
    let len = reader.read_u16().await? as usize;
    if len == 0 || len > MAX_RECORD {
        bail!("invalid encrypted frame length");
    }
    let mut frame = vec![0_u8; len];
    reader.read_exact(&mut frame).await?;
    Ok(frame)
}

async fn write_frame(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    frame: &[u8],
) -> anyhow::Result<()> {
    if frame.is_empty() || frame.len() > u16::MAX as usize {
        bail!("invalid encrypted frame");
    }
    writer.write_u16(frame.len() as u16).await?;
    writer.write_all(frame).await?;
    writer.flush().await?;
    Ok(())
}

fn lookup_device(paths: &Paths, public_key: &str) -> anyhow::Result<Option<DeviceRecord>> {
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    Ok(store
        .devices
        .into_iter()
        .find(|device| device.public_key == public_key))
}

fn identity(paths: &Paths) -> anyhow::Result<Identity> {
    ensure_root(paths)?;
    if paths.identity().exists() {
        let mut identity: Identity = read_json(&paths.identity())?;
        if identity.private_key.is_empty() {
            identity.private_key = load_identity_secret(paths, &identity.device_id)?;
        } else {
            // One-time migration from the Phase 1 plaintext metadata file.
            store_identity_secret(paths, &identity.device_id, &identity.private_key)?;
            let private = std::mem::take(&mut identity.private_key);
            write_json(&paths.identity(), &identity)?;
            identity.private_key = private;
        }
        decode_static_key(&identity.private_key).context("invalid stored Mac identity")?;
        return Ok(identity);
    }
    let params: NoiseParams = NOISE_PATTERN.parse().expect("valid Noise pattern");
    let keypair = Builder::new(params).generate_keypair()?;
    let identity = Identity {
        device_id: random_hex(16)?,
        private_key: hex_encode(&keypair.private),
        public_key: hex_encode(&keypair.public),
        key_generation: initial_key_generation(),
    };
    store_identity_secret(paths, &identity.device_id, &identity.private_key)?;
    let mut public_identity = identity.clone();
    public_identity.private_key.clear();
    write_json(&paths.identity(), &public_identity)?;
    Ok(identity)
}

fn ensure_enabled(paths: &Paths) -> anyhow::Result<()> {
    ensure_root(paths)?;
    let settings: Settings = read_json_or_default(&paths.settings())?;
    if !settings.enabled {
        bail!("remote access is disabled; run `latch remote-access enable` first");
    }
    Ok(())
}

fn ensure_root(paths: &Paths) -> anyhow::Result<()> {
    ensure_private_directory(&paths.root)
}

fn ensure_private_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("cannot create {}", path.display()))?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        bail!("refusing symlinked private directory {}", path.display());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(DIR_MODE))?;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!("refusing non-private directory {}", path.display());
    }
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<T> {
    serde_json::from_slice(&read_private_bytes(path)?)
        .with_context(|| format!("invalid {}", path.display()))
}

fn read_private_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("cannot inspect {}", path.display()))?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        bail!("refusing insecure private file {}", path.display());
    }
    fs::read(path).with_context(|| format!("cannot read {}", path.display()))
}

fn read_json_or_default<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> anyhow::Result<T> {
    if path.exists() {
        read_json(path)
    } else {
        Ok(T::default())
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    ensure_private_directory(parent)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        random_hex(8)?
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)
        .with_context(|| format!("cannot write {}", temporary.display()))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(FILE_MODE))?;
    fs::rename(&temporary, path).with_context(|| format!("cannot replace {}", path.display()))?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

fn audit(paths: &Paths, event: &str, device_id: Option<&str>, result: &str) -> anyhow::Result<()> {
    ensure_root(paths)?;
    let bytes = serde_json::to_vec(&AuditEvent {
        timestamp: unix_time(),
        event,
        device_id,
        result,
    })?;
    let existing = if paths.audit().exists() {
        read_private_bytes(&paths.audit())?
    } else {
        Vec::new()
    };
    let mut lines: VecDeque<&[u8]> = existing
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    while lines.len() >= MAX_AUDIT_EVENTS {
        lines.pop_front();
    }
    let mut retained_bytes = lines.iter().map(|line| line.len() + 1).sum::<usize>();
    while retained_bytes.saturating_add(bytes.len() + 1) > MAX_AUDIT_BYTES {
        let Some(removed) = lines.pop_front() else {
            break;
        };
        retained_bytes = retained_bytes.saturating_sub(removed.len() + 1);
    }
    let mut bounded = Vec::with_capacity(retained_bytes + bytes.len() + 1);
    for line in lines {
        bounded.extend_from_slice(line);
        bounded.push(b'\n');
    }
    bounded.extend_from_slice(&bytes);
    bounded.push(b'\n');
    write_bytes_atomic(&paths.audit(), &bounded)
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    ensure_private_directory(parent)?;
    let temporary = parent.join(format!(".audit.{}.tmp", random_hex(8)?));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(FILE_MODE)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    OpenOptions::new().read(true).open(parent)?.sync_all()?;
    Ok(())
}

fn summary(record: &DeviceRecord) -> DeviceSummary {
    DeviceSummary {
        device_id: record.device_id.clone(),
        name: record.name.clone(),
        permission: record.permission,
        revoked: record.revoked,
        control_plane_device_id: record.control_plane_device_id.clone(),
        pairing_phrase: None,
    }
}

fn enabled_by_default() -> bool {
    true
}

fn initial_key_generation() -> u64 {
    1
}

fn pairing_secret_digest(pairing_id: &str, secret: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"latch-remote-pairing-v1\0");
    digest.update(pairing_id.as_bytes());
    digest.update(b"\0");
    digest.update(secret.as_bytes());
    hex_encode(&digest.finalize())
}

/// The 64 words the pairing phrase is drawn from. Order is part of the
/// contract: an insertion would silently change every phrase.
const PHRASE_WORDS: [&str; 64] = [
    "anchor", "apple", "arrow", "atlas", "badge", "baker", "beacon", "birch", "cable", "cactus",
    "candle", "cedar", "chalk", "cliff", "clover", "comet", "coral", "crane", "delta", "dune",
    "ember", "falcon", "fern", "flint", "forge", "garnet", "glacier", "granite", "harbor", "hazel",
    "ivory", "jade", "kettle", "lantern", "ledger", "lily", "lumen", "maple", "marble", "meadow",
    "mesa", "nickel", "nomad", "oak", "onyx", "orbit", "otter", "pebble", "pilot", "prism",
    "quartz", "quill", "raven", "ribbon", "saffron", "sable", "spruce", "summit", "thistle",
    "timber", "tundra", "velvet", "walnut", "willow",
];

/// Six words of six bits, taken big-endian from the front of the digest.
const PHRASE_WORD_COUNT: usize = 6;
const PHRASE_BITS_PER_WORD: u32 = 6;

/// Derives the short authentication phrase for one pairing.
///
/// This is a cross-client contract, not an implementation detail: the phone
/// computes the same words from the same transcript and the user compares the
/// two screens. Nothing else in pairing catches a control plane or relay that
/// swapped a public key for its own — both machines would still agree — so the
/// domain separator, the field order, and the word list are all fixed.
/// `apps/LatchMobile/Sources/LatchMobileKit/PairingPhrase.swift` is the other
/// half; changing one without the other breaks pairing for everyone.
pub fn pairing_phrase(pairing_id: &str, mac_public_key: &str, device_public_key: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"latch-remote-pairing-phrase-v1");
    for field in [pairing_id, mac_public_key, device_public_key] {
        digest.update(b"\0");
        digest.update(field.to_ascii_lowercase().as_bytes());
    }
    let bytes = digest.finalize();
    let mut words = Vec::with_capacity(PHRASE_WORD_COUNT);
    let mut accumulator: u32 = 0;
    let mut bits: u32 = 0;
    for byte in bytes {
        accumulator = accumulator << 8 | u32::from(byte);
        bits += 8;
        while bits >= PHRASE_BITS_PER_WORD && words.len() < PHRASE_WORD_COUNT {
            bits -= PHRASE_BITS_PER_WORD;
            let index = (accumulator >> bits) & ((1 << PHRASE_BITS_PER_WORD) - 1);
            words.push(PHRASE_WORDS[index as usize]);
        }
        if words.len() == PHRASE_WORD_COUNT {
            break;
        }
    }
    words.join("-")
}

#[cfg(all(target_os = "macos", not(test)))]
fn store_identity_secret(_paths: &Paths, account: &str, secret: &str) -> anyhow::Result<()> {
    security_framework::passwords::set_generic_password(SECRET_SERVICE, account, secret.as_bytes())
        .context("cannot store the remote-access identity in macOS Keychain")
}

#[cfg(all(target_os = "macos", not(test)))]
fn load_identity_secret(_paths: &Paths, account: &str) -> anyhow::Result<String> {
    let bytes = security_framework::passwords::get_generic_password(SECRET_SERVICE, account)
        .context("cannot load the remote-access identity from macOS Keychain")?;
    String::from_utf8(bytes).context("stored remote-access identity is not UTF-8")
}

#[cfg(any(not(target_os = "macos"), test))]
fn store_identity_secret(paths: &Paths, _account: &str, secret: &str) -> anyhow::Result<()> {
    // Non-macOS builds are development/headless clients. Keep the same split
    // metadata boundary in an owner-only file; production macOS uses Keychain.
    write_bytes_atomic(&paths.identity_secret(), secret.as_bytes())
}

#[cfg(any(not(target_os = "macos"), test))]
fn load_identity_secret(paths: &Paths, _account: &str) -> anyhow::Result<String> {
    let path = paths.identity_secret();
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("cannot inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("refusing insecure identity secret {}", path.display());
    }
    fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))
}

fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut data = vec![0_u8; bytes];
    OpenOptions::new()
        .read(true)
        .open("/dev/urandom")?
        .read_exact(&mut data)?;
    Ok(hex_encode(&data))
}

fn decode_static_key(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64 || value.len() % 2 != 0 {
        bail!("device public key must be a 32-byte hex key");
    }
    decode_hex(value).context("invalid device key")
}

fn decode_hex(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() % 2 != 0 {
        bail!("hex value must have an even length");
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0]).ok_or_else(|| anyhow!("invalid hex value"))?;
        let low = hex_nibble(pair[1]).ok_or_else(|| anyhow!("invalid hex value"))?;
        bytes.push(high << 4 | low);
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::TempDir;

    fn home() -> (TempDir, LatchHome) {
        let dir = TempDir::new().unwrap();
        let home = LatchHome::new(dir.path());
        (dir, home)
    }
    #[test]
    fn supervised_gateway_is_always_launched_on_an_ephemeral_loopback_port() {
        let args = gateway_args(Path::new("/tmp/token"), Path::new("/tmp/ready"));
        let rendered: Vec<String> = args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect();
        assert_eq!(rendered[0], "serve");
        let bind = rendered
            .iter()
            .position(|value| value == "--bind")
            .map(|index| rendered[index + 1].clone())
            .expect("gateway launch must pin a bind");
        let parsed: SocketAddr = bind.parse().unwrap();
        assert!(parsed.ip().is_loopback(), "gateway bind must be loopback");
        assert_eq!(parsed.port(), 0, "gateway port must be ephemeral");
        assert!(
            !rendered.iter().any(|value| value == "--allow-remote"),
            "the helper must never opt the gateway into a public bind"
        );
    }

    fn offer_candidate(address: &str) -> IceCandidateRecord {
        IceCandidateRecord {
            candidate_type: "host".into(),
            priority: 2_130_706_431,
            foundation: "abc123".into(),
            component: 1,
            protocol: "udp".into(),
            address: address.into(),
            related_address: None,
            related_port: None,
            tcp_type: None,
            expires_at: unix_time() + 60,
        }
    }

    fn offer(request_id: &str) -> RemoteOffer {
        RemoteOffer {
            request_id: request_id.into(),
            peer_device_id: "b".repeat(32),
            ice_ufrag: "phoneufr".into(),
            ice_pwd: "phone-password-with-more-than-128-bits".into(),
            candidates: vec![offer_candidate("192.168.1.30:51234")],
            expires_at: unix_time() + 60,
        }
    }

    #[test]
    fn a_rendezvous_offer_is_bounded_before_it_can_reach_an_ice_agent() {
        let now = unix_time();
        assert!(offer(&"a".repeat(32)).validate(now).is_ok());

        // A request id names a file in the private runtime directory, so a
        // non-opaque one is refused rather than sanitized.
        assert!(offer("../../etc/passwd").validate(now).is_err());
        assert!(offer(&"a".repeat(31)).validate(now).is_err());

        let mut loopback = offer(&"a".repeat(32));
        loopback.candidates = vec![offer_candidate("127.0.0.1:51234")];
        assert!(loopback.validate(now).is_err());

        let mut unroutable = offer(&"a".repeat(32));
        unroutable.candidates = vec![offer_candidate("0.0.0.0:51234")];
        assert!(unroutable.validate(now).is_err());

        let mut portless = offer(&"a".repeat(32));
        portless.candidates = vec![offer_candidate("192.168.1.30:0")];
        assert!(portless.validate(now).is_err());

        let mut flooded = offer(&"a".repeat(32));
        flooded.candidates = (0..MAX_OFFER_CANDIDATES + 1)
            .map(|index| offer_candidate(&format!("192.168.1.30:{}", 40_000 + index)))
            .collect();
        assert!(flooded.validate(now).is_err());

        let mut empty = offer(&"a".repeat(32));
        empty.candidates.clear();
        assert!(empty.validate(now).is_err());

        let mut immortal = offer(&"a".repeat(32));
        immortal.expires_at = now + PRESENCE_LIFETIME.as_secs() + 1;
        assert!(immortal.validate(now).is_err());

        let mut expired = offer(&"a".repeat(32));
        expired.expires_at = now;
        assert!(expired.validate(now).is_err());

        let mut forged = offer(&"a".repeat(32));
        forged.ice_pwd = "not a credential".into();
        assert!(forged.validate(now).is_err());

        let mut tcp_only = offer(&"a".repeat(32));
        tcp_only.candidates = vec![IceCandidateRecord {
            tcp_type: Some("passive".into()),
            ..offer_candidate("192.168.1.30:51234")
        }];
        assert!(tcp_only.validate(now).is_err());
    }

    #[test]
    fn status_republishes_the_helpers_ice_agent_under_the_names_the_desktop_decodes() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        let identity = identity(&paths).unwrap();
        ensure_private_directory(&paths.runtime()).unwrap();

        // Without an agent the readiness document simply omits the block; a
        // desktop app then publishes presence with no ICE rather than
        // credentials nothing would answer.
        write_lan_readiness(
            &paths,
            &identity,
            "192.168.1.20:49221".parse().unwrap(),
            None,
            0,
        )
        .unwrap();
        assert!(status(&home).unwrap().ice.is_none());
        assert!(!fs::read_to_string(paths.lan_readiness())
            .unwrap()
            .contains("\"ice\""));

        let gathered = IceReadiness {
            ufrag: "abc123".into(),
            password: "a-short-term-stun-credential".into(),
            candidates: vec![
                offer_candidate("100.64.0.7:52000"),
                IceCandidateRecord {
                    candidate_type: "srflx".into(),
                    related_address: Some("192.168.1.20".into()),
                    related_port: Some(52_000),
                    ..offer_candidate("203.0.113.9:52000")
                },
            ],
        };
        write_lan_readiness(
            &paths,
            &identity,
            "192.168.1.20:49221".parse().unwrap(),
            Some(gathered.clone()),
            0,
        )
        .unwrap();

        let reported = status(&home).unwrap();
        assert_eq!(reported.ice.as_ref(), Some(&gathered));
        // The desktop app decodes this by name, so the wire form is the
        // contract, not the Rust field spelling.
        let document = serde_json::to_value(&reported).unwrap();
        let candidate = &document["ice"]["candidates"][1];
        assert_eq!(candidate["type"], "srflx");
        assert_eq!(candidate["relatedAddress"], "192.168.1.20");
        assert_eq!(candidate["relatedPort"], 52_000);
        assert_eq!(candidate["expiresAt"], gathered.candidates[1].expires_at);
        assert!(candidate["tcpType"].is_null());
        // Still never the supervised gateway or its credential.
        assert!(!document.to_string().contains("127.0.0.1"));
    }

    #[test]
    fn recorded_offers_are_drained_once_and_never_outlive_their_window() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);

        record_offer(&home, &offer(&"a".repeat(32))).unwrap();
        record_offer(&home, &offer(&"c".repeat(32))).unwrap();
        // A malformed offer never reaches the private directory at all.
        let mut rejected = offer(&"d".repeat(32));
        rejected.candidates = vec![offer_candidate("127.0.0.1:51234")];
        assert!(record_offer(&home, &rejected).is_err());

        // An expired document is dropped on the way out rather than handed to
        // an agent that could only fail the checks it implies.
        let mut stale = offer(&"e".repeat(32));
        stale.expires_at = unix_time() + 60;
        write_json(&paths.offers().join("stale.json"), &stale).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(paths.offers().join("stale.json")).unwrap()).unwrap();
        document["expiresAt"] = serde_json::json!(1);
        write_json(&paths.offers().join("stale.json"), &document).unwrap();

        let mut drained: Vec<String> = drain_offers(&paths)
            .into_iter()
            .map(|offer| offer.request_id)
            .collect();
        drained.sort();
        assert_eq!(drained, vec!["a".repeat(32), "c".repeat(32)]);
        // Offers are one-shot: a second drain finds nothing to replay.
        assert!(drain_offers(&paths).is_empty());
        assert!(fs::read_dir(paths.offers()).unwrap().next().is_none());
    }

    #[test]
    fn the_helper_never_advertises_the_gateway_and_status_tracks_the_listener() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        let identity = identity(&paths).unwrap();
        ensure_private_directory(&paths.runtime()).unwrap();

        assert!(write_lan_readiness(
            &paths,
            &identity,
            "127.0.0.1:9100".parse().unwrap(),
            None,
            0
        )
        .is_err());
        assert!(validate_lan_bind("127.0.0.1:0".parse().unwrap()).is_err());
        assert!(validate_lan_bind("0.0.0.0:0".parse().unwrap()).is_ok());

        write_lan_readiness(
            &paths,
            &identity,
            "192.168.1.20:49221".parse().unwrap(),
            None,
            0,
        )
        .unwrap();
        let document = fs::read_to_string(paths.lan_readiness()).unwrap();
        assert!(!document.contains("127.0.0.1"));
        assert!(!document.contains("token"));
        assert!(!document.contains(&identity.public_key));

        // A document left behind by a helper that is gone must not be reported
        // as a live listener.
        let mut orphan: LanReadiness = read_json(&paths.lan_readiness()).unwrap();
        orphan.pid = 0;
        write_json(&paths.lan_readiness(), &orphan).unwrap();
        assert_eq!(status(&home).unwrap().listener_address, None);
        assert!(!paths.lan_readiness().exists());
        write_lan_readiness(
            &paths,
            &identity,
            "192.168.1.20:49221".parse().unwrap(),
            None,
            0,
        )
        .unwrap();

        let running = status(&home).unwrap();
        assert!(running.enabled);
        assert_eq!(
            running.listener_address.as_deref(),
            Some("192.168.1.20:49221")
        );
        assert_eq!(
            running.device_id.as_deref(),
            Some(identity.device_id.as_str())
        );
        assert_eq!(
            running.public_key.as_deref(),
            Some(identity.public_key.as_str())
        );

        // Disabling remote access must retract the advertised listener along
        // with the gateway credential.
        set_enabled(&home, false).unwrap();
        let stopped = status(&home).unwrap();
        assert!(!stopped.enabled);
        assert_eq!(stopped.listener_address, None);
    }

    #[test]
    fn status_never_creates_an_identity_or_reveals_a_private_key() {
        let (_dir, home) = home();
        let initial = status(&home).unwrap();
        assert!(!initial.enabled);
        assert_eq!(initial.device_id, None);
        assert_eq!(initial.paired_devices, 0);
        assert!(!Paths::new(&home).identity().exists());

        set_enabled(&home, true).unwrap();
        let enabled = status(&home).unwrap();
        let serialized = serde_json::to_string(&enabled).unwrap();
        assert!(!serialized.contains("privateKey"));
        assert!(!serialized.contains("private_key"));
    }

    /// The phrase for the fixed transcript below. `PairingPhraseTests` in the
    /// phone app asserts the same string against the same inputs, which is how
    /// the two implementations are kept from drifting apart.
    const PAIRING_PHRASE_VECTOR: &str = "sable-apple-maple-garnet-maple-flint";

    fn phone_key() -> String {
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        hex_encode(&Builder::new(params).generate_keypair().unwrap().public)
    }

    fn keypair() -> (String, String) {
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        let pair = Builder::new(params).generate_keypair().unwrap();
        (hex_encode(&pair.private), hex_encode(&pair.public))
    }

    async fn initiator_handshake(
        reader: &mut tokio::net::tcp::OwnedReadHalf,
        writer: &mut tokio::net::tcp::OwnedWriteHalf,
        private_key: &str,
    ) -> TransportState {
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        let private = decode_static_key(private_key).unwrap();
        let mut handshake = Builder::new(params)
            .local_private_key(&private)
            .build_initiator()
            .unwrap();
        let mut first = vec![0_u8; MAX_RECORD];
        let first_len = handshake.write_message(&[], &mut first).unwrap();
        write_frame(writer, &first[..first_len]).await.unwrap();
        let second = read_frame(reader).await.unwrap();
        let mut scratch = vec![0_u8; MAX_RECORD];
        handshake.read_message(&second, &mut scratch).unwrap();
        let mut third = vec![0_u8; MAX_RECORD];
        let third_len = handshake.write_message(&[], &mut third).unwrap();
        write_frame(writer, &third[..third_len]).await.unwrap();
        handshake.into_transport_mode().unwrap()
    }

    async fn read_request_headers(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "gateway peer closed before sending headers");
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                return request;
            }
        }
    }

    #[test]
    fn bonjour_advertisement_is_resolvable_and_hints_the_pinned_identity() {
        let identity = Identity {
            device_id: "a".repeat(32),
            private_key: String::new(),
            public_key: "b".repeat(64),
            key_generation: 1,
        };
        let service = bonjour_service(&identity, 49_221).unwrap();
        assert_eq!(service.get_hostname(), "latch-aaaaaaaaaaaa.local.");
        assert!(service.is_addr_auto());
        assert_eq!(service.get_port(), 49_221);
        assert_eq!(
            service.get_property_val_str("identityKey"),
            Some(identity.public_key.as_str())
        );
    }

    /// Losing the grant closes a live terminal exactly as losing the pairing
    /// does. A device downgraded from control while it holds the surface must
    /// not keep writing to it until it happens to disconnect.
    #[tokio::test]
    async fn noise_proxy_closes_a_live_terminal_when_the_grant_is_downgraded() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        let mac_identity = identity(&paths).unwrap();
        let (phone_private, phone_public) = keypair();
        let pairing = create_pairing(&home).unwrap();
        let device = confirm_pairing(
            &home,
            &pairing.pairing_id,
            &pairing.secret,
            &phone_public,
            "downgrade test phone",
            DevicePermission::Control,
            None,
        )
        .unwrap();

        ensure_private_directory(&paths.runtime()).unwrap();
        let token_path = paths.runtime().join("gateway.token");
        write_bytes_atomic(&token_path, b"test-gateway-token").unwrap();
        let gateway_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let gateway = tokio::spawn(async move {
            let (mut stream, _) = gateway_listener.accept().await.unwrap();
            let _ = read_request_headers(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                )
                .await
                .unwrap();
            let mut trailing = [0_u8; 1];
            let _ = stream.read(&mut trailing).await;
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();
        let proxy_paths = paths.clone();
        let proxy_identity = mac_identity.clone();
        let proxy_token = token_path.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let proxy_connections = connections.clone();
        let proxy = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            proxy_connection(
                PeerStream::from_tcp(stream),
                &proxy_paths,
                &proxy_identity,
                &proxy_token,
                gateway_addr,
                &proxy_connections,
            )
            .await
        });

        let stream = TcpStream::connect(listener_addr).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();
        let mut transport = initiator_handshake(&mut reader, &mut writer, &phone_private).await;
        encrypt_record(
            &mut writer,
            &mut transport,
            b"GET /v2/sessions/ses_1/terminal?cols=80&rows=24 HTTP/1.1\r\nHost: latch\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        )
        .await
        .unwrap();
        let response = decrypt_record(&mut reader, &mut transport).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 101"));
        // The gauge the Mac's sleep assertion hangs off: a phone holding a
        // terminal is exactly the case where this Mac must stay awake.
        assert_eq!(connections.load(Ordering::Relaxed), 1);

        // Still paired, still able to read and send messages — but no longer
        // allowed to hold a terminal.
        grant(&home, &device.device_id, DevicePermission::Interact).unwrap();
        let closed =
            tokio::time::timeout(Duration::from_millis(500), read_frame(&mut reader)).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "a permission downgrade must close the peer stream"
        );
        assert!(!list_devices(&home).unwrap()[0].revoked);
        let trail = read_audit(&home).unwrap();
        assert!(trail.iter().any(|event| {
            event["event"] == "connection_closed" && event["result"] == "permission_downgraded"
        }));
        drop(writer);
        assert!(proxy.await.unwrap().is_ok());
        // And released with the stream, so a Mac with nothing connected is
        // free to sleep again.
        assert_eq!(connections.load(Ordering::Relaxed), 0);
        gateway.await.unwrap();
    }

    #[tokio::test]
    async fn noise_proxy_carries_the_v2_terminal_for_control_and_closes_on_revocation() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        let mac_identity = identity(&paths).unwrap();
        let (phone_private, phone_public) = keypair();
        let pairing = create_pairing(&home).unwrap();
        let device = confirm_pairing(
            &home,
            &pairing.pairing_id,
            &pairing.secret,
            &phone_public,
            "LAN test phone",
            // A terminal is the session's one exclusive surface, so only a
            // control device may open it. There is no observing terminal.
            DevicePermission::Control,
            None,
        )
        .unwrap();

        ensure_private_directory(&paths.runtime()).unwrap();
        let token_path = paths.runtime().join("gateway.token");
        write_bytes_atomic(&token_path, b"test-gateway-token").unwrap();
        let gateway_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        let gateway = tokio::spawn(async move {
            let (mut stream, _) = gateway_listener.accept().await.unwrap();
            let request = read_request_headers(&mut stream).await;
            assert!(request
                .windows(b"Authorization: Bearer test-gateway-token".len())
                .any(|part| part == b"Authorization: Bearer test-gateway-token"));
            assert!(request
                .windows(b"x-latch-device-grant: control".len())
                .any(|part| part == b"x-latch-device-grant: control"));
            stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n",
                )
                .await
                .unwrap();
            let mut ping = [0_u8; 4];
            stream.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            stream.write_all(b"pong").await.unwrap();
            let mut trailing = [0_u8; 1];
            let _ = stream.read(&mut trailing).await;
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();
        let proxy_paths = paths.clone();
        let proxy_identity = mac_identity.clone();
        let proxy_token = token_path.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let proxy_connections = connections.clone();
        let proxy = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            proxy_connection(
                PeerStream::from_tcp(stream),
                &proxy_paths,
                &proxy_identity,
                &proxy_token,
                gateway_addr,
                &proxy_connections,
            )
            .await
        });

        let stream = TcpStream::connect(listener_addr).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();
        let mut transport = initiator_handshake(&mut reader, &mut writer, &phone_private).await;
        encrypt_record(
            &mut writer,
            &mut transport,
            b"GET /v2/sessions/ses_1/terminal?cols=80&rows=24 HTTP/1.1\r\nHost: latch\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
        )
        .await
        .unwrap();
        let response = decrypt_record(&mut reader, &mut transport).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 101"));

        // This used to be an absolute timeout around the whole proxy. Keeping
        // the authenticated stream alive past it proves only the handshake is
        // deadline-bound.
        tokio::time::sleep(HANDSHAKE_TIMEOUT + Duration::from_millis(100)).await;
        encrypt_record(&mut writer, &mut transport, b"ping")
            .await
            .unwrap();
        assert_eq!(
            decrypt_record(&mut reader, &mut transport).await.unwrap(),
            b"pong"
        );

        revoke(&home, &device.device_id).unwrap();
        let closed =
            tokio::time::timeout(Duration::from_millis(500), read_frame(&mut reader)).await;
        assert!(
            matches!(closed, Ok(Err(_))),
            "revocation must close the peer stream"
        );
        drop(writer);
        assert!(proxy.await.unwrap().is_ok());
        gateway.await.unwrap();
    }

    /// The whole paired path, end to end: a Noise-authenticated observe-only
    /// device opens the real v2 conversation WebSocket through the proxy,
    /// reads the conversation, and is refused `send_message` by the Hub.
    ///
    /// The proxy authorizes only the upgrade, so this is the test that proves
    /// the per-message check actually happens behind it.
    #[tokio::test]
    async fn noise_tunnel_carries_the_v2_conversation_and_the_hub_refuses_an_observe_only_send() {
        let (dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        let mac_identity = identity(&paths).unwrap();
        let (phone_private, phone_public) = keypair();
        let pairing = create_pairing(&home).unwrap();
        confirm_pairing(
            &home,
            &pairing.pairing_id,
            &pairing.secret,
            &phone_public,
            "observe-only phone",
            DevicePermission::Observe,
            None,
        )
        .unwrap();

        let session = conversation_session(&home);
        ensure_private_directory(&paths.runtime()).unwrap();
        let token_path = paths.runtime().join("gateway.token");
        write_bytes_atomic(&token_path, b"tunnel-gateway-token").unwrap();
        let hub = crate::conversation::ConversationHub::new(dir.path().join("hub")).unwrap();
        let app = crate::cli::serve::test_router(
            home.clone(),
            token_path.clone(),
            hub,
            std::path::PathBuf::from("latch"),
        );
        let gateway_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gateway_addr = gateway_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(
                gateway_listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listener_addr = listener.local_addr().unwrap();
        let proxy_paths = paths.clone();
        let proxy_identity = mac_identity.clone();
        let proxy_token = token_path.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let proxy_connections = connections.clone();
        let proxy = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            proxy_connection(
                PeerStream::from_tcp(stream),
                &proxy_paths,
                &proxy_identity,
                &proxy_token,
                gateway_addr,
                &proxy_connections,
            )
            .await
        });

        let stream = TcpStream::connect(listener_addr).await.unwrap();
        let (mut reader, mut writer) = stream.into_split();
        let mut transport = initiator_handshake(&mut reader, &mut writer, &phone_private).await;
        let upgrade = format!(
            "GET /v2/sessions/{session}/conversation HTTP/1.1\r\nHost: latch\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        encrypt_record(&mut writer, &mut transport, upgrade.as_bytes())
            .await
            .unwrap();

        let mut buffered = Vec::new();
        let response = loop {
            buffered.extend(decrypt_record(&mut reader, &mut transport).await.unwrap());
            if let Some(end) = buffered.windows(4).position(|window| window == b"\r\n\r\n") {
                break buffered.drain(..end + 4).collect::<Vec<_>>();
            }
        };
        assert!(
            response.starts_with(b"HTTP/1.1 101"),
            "expected a websocket upgrade, got {}",
            String::from_utf8_lossy(&response)
        );

        // The server speaks first: no client frame has been sent yet.
        let snapshot = next_ws_message(&mut reader, &mut transport, &mut buffered).await;
        assert_eq!(snapshot["type"], "snapshot");
        assert_eq!(snapshot["reason"], "initial");

        encrypt_record(
            &mut writer,
            &mut transport,
            &ws_text_frame(
                &serde_json::json!({
                    "type": "send_message",
                    "operationEpoch": snapshot["operationEpoch"],
                    "operationId": "tunnel-op",
                    "text": "hello",
                })
                .to_string(),
            ),
        )
        .await
        .unwrap();

        let mut result = next_ws_message(&mut reader, &mut transport, &mut buffered).await;
        for _ in 0..16 {
            if result["type"] == "operation_result" {
                break;
            }
            result = next_ws_message(&mut reader, &mut transport, &mut buffered).await;
        }
        assert_eq!(result["type"], "operation_result");
        assert_eq!(result["operationId"], "tunnel-op");
        assert_eq!(result["status"], "refused");
        assert!(result["reason"].as_str().unwrap().contains("device grant"));

        drop(writer);
        let _ = proxy.await.unwrap();
    }

    /// Creates the metadata one session needs to be routable.
    fn conversation_session(home: &LatchHome) -> String {
        use crate::session::manifest::{SourceInfo, TerminalSize};
        use crate::session::meta::{self, SessionMeta};
        use crate::session::paths::SessionId;

        home.ensure().unwrap();
        let id = SessionId::parse("ses_tunnelconversation").unwrap();
        let paths = home.session(&id);
        paths.ensure().unwrap();
        meta::write_once(
            &paths,
            &SessionMeta {
                format_version: 1,
                id: id.as_str().to_owned(),
                name: "tunnel".into(),
                title: None,
                cwd: std::path::PathBuf::from("/tmp"),
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
        .unwrap();
        id.as_str().to_owned()
    }

    /// Minimal client-to-server text frame. A zero mask key is a legal mask.
    fn ws_text_frame(text: &str) -> Vec<u8> {
        let payload = text.as_bytes();
        let mut frame = vec![0x81];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&[0, 0, 0, 0]);
        frame.extend_from_slice(payload);
        frame
    }

    /// Reads one server text frame out of the tunnel, refilling from Noise
    /// records because record and frame boundaries do not align.
    async fn next_ws_message(
        reader: &mut tokio::net::tcp::OwnedReadHalf,
        transport: &mut TransportState,
        buffered: &mut Vec<u8>,
    ) -> serde_json::Value {
        loop {
            if let Some((opcode, payload, consumed)) = parse_ws_frame(buffered) {
                buffered.drain(..consumed);
                match opcode {
                    0x1 => {
                        return serde_json::from_slice(&payload).expect("server frame is not JSON")
                    }
                    0x8 => panic!("tunnel closed before a text frame"),
                    _ => continue,
                }
            }
            let record =
                tokio::time::timeout(Duration::from_secs(10), decrypt_record(reader, transport))
                    .await
                    .expect("timed out waiting for a conversation frame")
                    .expect("tunnel record");
            buffered.extend(record);
        }
    }

    /// Returns (opcode, payload, bytes consumed) for one complete server frame.
    fn parse_ws_frame(buffer: &[u8]) -> Option<(u8, Vec<u8>, usize)> {
        if buffer.len() < 2 {
            return None;
        }
        let opcode = buffer[0] & 0x0f;
        let masked = buffer[1] & 0x80 != 0;
        let short = (buffer[1] & 0x7f) as usize;
        let (length, mut offset) = match short {
            126 => {
                if buffer.len() < 4 {
                    return None;
                }
                (u16::from_be_bytes([buffer[2], buffer[3]]) as usize, 4)
            }
            127 => {
                if buffer.len() < 10 {
                    return None;
                }
                let mut bytes = [0_u8; 8];
                bytes.copy_from_slice(&buffer[2..10]);
                (u64::from_be_bytes(bytes) as usize, 10)
            }
            other => (other, 2),
        };
        if masked {
            offset += 4;
        }
        if buffer.len() < offset + length {
            return None;
        }
        Some((
            opcode,
            buffer[offset..offset + length].to_vec(),
            offset + length,
        ))
    }

    #[test]
    fn pairing_is_single_use_and_defaults_to_interact() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let material = create_pairing(&home).unwrap();
        let device = confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &phone_key(),
            "Test phone",
            DEFAULT_PERMISSION,
            None,
        )
        .unwrap();
        assert_eq!(device.permission, DevicePermission::Interact);
        assert!(confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &phone_key(),
            "Other",
            DEFAULT_PERMISSION,
            None
        )
        .is_err());
    }

    #[test]
    fn pairing_phrase_is_transcript_bound_and_stable() {
        // A fixed vector, asserted identically by the phone's
        // `PairingPhraseTests`. If these two ever disagree, the words on the
        // Mac and on the phone disagree, and the user's only real check on a
        // substituted identity is gone.
        let phrase = pairing_phrase(
            "0123456789abcdef0123456789abcdef",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222222222222222222222222222",
        );
        assert_eq!(phrase.split('-').count(), PHRASE_WORD_COUNT);
        assert_eq!(phrase, PAIRING_PHRASE_VECTOR);

        // Hex case is not meaningful, so it must not change the words.
        assert_eq!(
            pairing_phrase(
                "0123456789ABCDEF0123456789ABCDEF",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222222222222222222222222222",
            ),
            phrase
        );

        // Every field is bound: changing any one of them changes the phrase.
        assert_ne!(
            pairing_phrase(
                "0123456789abcdef0123456789abcde0",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222222222222222222222222222",
            ),
            phrase
        );
        assert_ne!(
            pairing_phrase(
                "0123456789abcdef0123456789abcdef",
                "1111111111111111111111111111111111111111111111111111111111111112",
                "2222222222222222222222222222222222222222222222222222222222222222",
            ),
            phrase
        );
        assert_ne!(
            pairing_phrase(
                "0123456789abcdef0123456789abcdef",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "2222222222222222222222222222222222222222222222222222222222222223",
            ),
            phrase
        );

        // The phrase digest must not collide with the secret digest, which is
        // taken over a nearly identical transcript.
        assert_ne!(
            phrase,
            pairing_secret_digest("0123456789abcdef0123456789abcdef", "secret")
        );
    }

    #[test]
    fn confirmation_reports_the_phrase_and_listing_does_not() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let material = create_pairing(&home).unwrap();
        let (public, _private) = keypair();
        let device = confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &public,
            "Phone",
            DEFAULT_PERMISSION,
            None,
        )
        .unwrap();
        assert_eq!(
            device.pairing_phrase.as_deref(),
            Some(pairing_phrase(&material.pairing_id, &material.mac_public_key, &public).as_str())
        );
        // The phrase belongs to one pairing transcript, not to the device, so
        // it is absent from the ordinary device listing.
        assert!(list_devices(&home).unwrap()[0].pairing_phrase.is_none());
    }

    #[test]
    fn revocation_is_persistent() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let material = create_pairing(&home).unwrap();
        let device = confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &phone_key(),
            "Test phone",
            DEFAULT_PERMISSION,
            None,
        )
        .unwrap();
        revoke(&home, &device.device_id).unwrap();
        assert!(list_devices(&home).unwrap()[0].revoked);
    }

    #[test]
    fn pairing_secret_is_hashed_at_rest_and_disable_cancels_it() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let material = create_pairing(&home).unwrap();
        let stored = fs::read_to_string(home.remote_access_dir().join("pairings.json")).unwrap();
        assert!(!stored.contains(&material.secret));
        assert!(stored.contains("secret_digest"));

        set_enabled(&home, false).unwrap();
        assert!(confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &phone_key(),
            "late phone",
            DEFAULT_PERMISSION,
            None,
        )
        .is_err());
    }

    #[test]
    fn device_key_rotation_preserves_grants_and_rejects_the_old_key() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let material = create_pairing(&home).unwrap();
        let old_key = phone_key();
        let device = confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &old_key,
            "rotating phone",
            DevicePermission::Control,
            None,
        )
        .unwrap();
        let new_key = phone_key();
        rotate_device_key(&home, &device.device_id, &new_key).unwrap();
        let paths = Paths::new(&home);
        assert!(lookup_device(&paths, &old_key).unwrap().is_none());
        let rotated = lookup_device(&paths, &new_key).unwrap().unwrap();
        assert_eq!(rotated.permission, DevicePermission::Control);
    }

    #[test]
    fn diagnostics_are_content_free_and_relay_has_an_independent_kill_switch() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        set_relay_enabled(&home, false).unwrap();
        assert!(!relay_enabled(&home).unwrap());
        let export = diagnostics_export(&home).unwrap();
        let json = serde_json::to_string(&export).unwrap();
        assert!(!json.contains("publicKey"));
        assert!(!json.contains("deviceId"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("token"));
        assert!(!export.relay_enabled);
    }

    /// `never` is the strict form of the same switch: it refuses relay
    /// admission like `disable` does, and additionally tells the desktop app to
    /// stop publishing anything but host candidates.
    #[test]
    fn never_relay_refuses_admission_and_is_cleared_by_allowing_the_relay_again() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();

        set_relay_mode(&home, RelayMode::Never).unwrap();
        assert!(!relay_enabled(&home).unwrap());
        let strict = status(&home).unwrap();
        assert!(strict.never_relay);
        assert!(!strict.relay_enabled);

        // Turning the relay off and turning it never-off are different
        // answers, and the plain disable must not claim the stricter one.
        set_relay_enabled(&home, false).unwrap();
        assert!(!status(&home).unwrap().never_relay);

        set_relay_mode(&home, RelayMode::Never).unwrap();
        // Allowing the relay again clears it, rather than leaving presence
        // narrowed to addresses that cannot use the path just permitted.
        set_relay_enabled(&home, true).unwrap();
        let permissive = status(&home).unwrap();
        assert!(!permissive.never_relay);
        assert!(permissive.relay_enabled);
        assert!(relay_enabled(&home).unwrap());

        assert!(read_audit(&home)
            .unwrap()
            .iter()
            .any(|event| event["event"] == "relay_never"));
    }

    #[test]
    fn path_metrics_separate_direct_from_relay_and_stay_content_free() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let paths = Paths::new(&home);
        for route in [
            PeerRoute::Lan,
            PeerRoute::DirectHost,
            PeerRoute::DirectReflexive,
            PeerRoute::Relay,
            PeerRoute::Relay,
        ] {
            audit(&paths, PATH_SELECTED_EVENT, Some("dev_1"), route.slug()).unwrap();
        }
        record_ice_answer(&home, true).unwrap();
        record_ice_answer(&home, false).unwrap();

        let export = diagnostics_export(&home).unwrap();
        let metrics = &export.path_selection;
        assert_eq!(metrics.connections, 5);
        assert_eq!(metrics.direct, 3);
        assert_eq!(metrics.relay, 2);
        assert_eq!(metrics.routes["lan"], 1);
        assert_eq!(metrics.routes["direct_reflexive"], 1);
        assert_eq!(metrics.routes["relay"], 2);
        // A route with no hits is still named, so "never relayed" is
        // distinguishable from "this build did not measure it".
        assert_eq!(metrics.routes["unknown"], 0);
        assert_eq!(metrics.ice_answers, 2);
        assert_eq!(metrics.ice_answers_connected, 1);
        assert_eq!(metrics.relay_share(), Some(0.4));

        // The counters travel in the content-free bundle, so they must not
        // reintroduce the identity the rest of it excludes.
        let json = serde_json::to_string(&export.path_selection).unwrap();
        assert!(!json.contains("dev_1"));
    }

    #[test]
    fn path_metrics_report_no_share_before_any_connection() {
        let (_dir, home) = home();
        set_enabled(&home, true).unwrap();
        let export = diagnostics_export(&home).unwrap();
        // Zero of zero is not zero percent relayed; it is nothing measured.
        assert_eq!(export.path_selection.relay_share(), None);
        assert_eq!(export.path_selection.connections, 0);
    }

    #[test]
    fn private_state_rejects_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let (dir, home) = home();
        set_enabled(&home, true).unwrap();
        let outside = dir.path().join("outside.json");
        fs::write(&outside, b"{}\n").unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(FILE_MODE)).unwrap();
        let devices = home.remote_access_dir().join("devices.json");
        symlink(&outside, &devices).unwrap();
        assert!(list_devices(&home).is_err());
    }

    #[test]
    fn proxy_authorization_never_accepts_control_for_observers() {
        let request =
            b"GET /v2/sessions/ses_1/terminal?mode=control HTTP/1.1\r\nHost: example\r\n\r\n"
                .to_vec();
        assert!(authorize_and_inject(request, DevicePermission::Observe, "secret").is_err());
    }

    /// The grant a live stream has to keep holding is the route's, not the
    /// one the device happened to have at handshake. A terminal admitted
    /// under `control` reports `control`, which is what the periodic device
    /// check compares against after a downgrade.
    #[test]
    fn proxy_authorization_reports_the_grant_the_stream_must_keep() {
        let terminal =
            b"GET /v2/sessions/ses_1/terminal?mode=control HTTP/1.1\r\nHost: example\r\n\r\n"
                .to_vec();
        let (_, required) =
            authorize_and_inject(terminal, DevicePermission::Control, "secret").unwrap();
        assert_eq!(required, DevicePermission::Control);
        assert!(!DevicePermission::Interact.permits(required));
    }

    #[test]
    fn proxy_only_injects_its_own_gateway_credential() {
        let request =
            b"GET /v2/sessions HTTP/1.1\r\nHost: example\r\nAuthorization: Bearer stolen\r\n\r\n"
                .to_vec();
        assert!(authorize_and_inject(request, DevicePermission::Control, "secret").is_err());
        let forged_grant =
            b"GET /v2/sessions HTTP/1.1\r\nHost: example\r\nX-Latch-Device-Grant: control\r\n\r\n"
                .to_vec();
        assert!(authorize_and_inject(forged_grant, DevicePermission::Observe, "secret").is_err());
    }

    #[test]
    fn proxy_rejects_request_smuggling_and_closes_plain_http() {
        let pipelined = b"GET /v2/sessions HTTP/1.1\r\nHost: example\r\n\r\nGET /v2/sessions/ses_1/terminal?mode=control HTTP/1.1\r\nHost: example\r\n\r\n".to_vec();
        assert!(authorize_and_inject(pipelined, DevicePermission::Observe, "secret").is_err());
        let smuggled = b"GET /v2/sessions/ses_1/conversation HTTP/1.1\r\nHost: example\r\nContent-Length: 2\r\nContent-Length: 80\r\n\r\n{}".to_vec();
        assert!(authorize_and_inject(smuggled, DevicePermission::Interact, "secret").is_err());
        let transfer = b"GET /v2/sessions/ses_1/conversation HTTP/1.1\r\nHost: example\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec();
        assert!(authorize_and_inject(transfer, DevicePermission::Interact, "secret").is_err());
        let ordinary = b"GET /v2/sessions HTTP/1.1\r\nHost: example\r\n\r\n".to_vec();
        let (authorized, required) =
            authorize_and_inject(ordinary, DevicePermission::Observe, "secret").unwrap();
        assert_eq!(required, DevicePermission::Observe);
        assert!(authorized
            .windows(b"Connection: close".len())
            .any(|part| part == b"Connection: close"));
        assert!(authorized
            .windows(b"x-latch-device-grant: observe".len())
            .any(|part| part == b"x-latch-device-grant: observe"));
    }

    fn candidate(port: u16) -> DirectCandidate {
        DirectCandidate {
            address: SocketAddr::new("192.168.1.20".parse::<IpAddr>().unwrap(), port),
            // Leave margin below the enclosing record lifetime so a wall-clock
            // second tick cannot make this helper intermittently outlive it.
            expires_at: unix_time() + 20,
        }
    }

    #[test]
    fn directory_only_releases_presence_to_paired_requesters() {
        let now = unix_time();
        let mut directory = DeviceDirectory::default();
        let target = PresenceRecord {
            device_id: "b".repeat(32),
            identity_key: phone_key(),
            candidates: vec![candidate(42000)],
            expires_at: now + 30,
        };
        directory.publish(target, now).unwrap();
        let request = RendezvousRequest {
            requester_device_id: "a".repeat(32),
            target_device_id: "b".repeat(32),
            request_id: "c".repeat(32),
            candidates: vec![candidate(42001)],
            expires_at: now + 30,
        };
        assert!(directory.rendezvous(request.clone(), false, now).is_err());
        let response = directory.rendezvous(request, true, now).unwrap();
        assert_eq!(response.peer_device_id, "b".repeat(32));
        assert_eq!(response.candidates.len(), 1);
    }

    #[test]
    fn direct_reconnect_requires_capability_refresh_before_forwarding() {
        let mut connection = DirectConnection::default();
        connection.begin();
        connection.connected();
        assert!(connection.may_forward_application_data());
        connection.migrated_path().unwrap();
        assert!(!connection.may_forward_application_data());
        connection.capability_refreshed();
        assert!(connection.may_forward_application_data());
        connection.interrupted();
        assert_eq!(connection.diagnostics().reconnect_count, 1);
        assert!(!connection.may_forward_application_data());
    }

    #[test]
    fn forced_relay_forwards_only_e2e_ciphertext() {
        let now = unix_time();
        let mac_id = "a".repeat(32);
        let phone_id = "b".repeat(32);
        let (mac_private, mac_public) = keypair();
        let (phone_private, phone_public) = keypair();
        let mut relay = OpaqueRelay::new(RelayLimits::default());
        assert!(relay.issue_ticket(&mac_id, &phone_id, false, now).is_err());
        let ticket = relay.issue_ticket(&mac_id, &phone_id, true, now).unwrap();
        let mac = relay.connect(&ticket, &mac_id, now).unwrap();
        let phone = relay.connect(&ticket, &phone_id, now).unwrap();
        let (mut mac_cipher, mut phone_cipher) = establish_relay_ciphers(
            &mac_private,
            &phone_public,
            &phone_private,
            &mac_public,
            &ticket.relay_id,
        )
        .unwrap();
        let plaintext = b"terminal plaintext and internal gateway token";
        let ciphertext = mac_cipher.seal(plaintext).unwrap();
        assert!(!ciphertext
            .windows(plaintext.len())
            .any(|window| window == plaintext));
        relay.forward(&mac, ciphertext.clone(), now).unwrap();
        let delivered = relay.receive(&phone).unwrap().unwrap();
        assert_eq!(delivered, ciphertext);
        assert_eq!(phone_cipher.open(&delivered).unwrap(), plaintext);
        assert_eq!(relay.diagnostics().forwarded_frames, 1);
    }

    #[test]
    fn relay_quota_and_path_selection_block_unbounded_fallback() {
        let now = unix_time();
        let mac_id = "a".repeat(32);
        let phone_id = "b".repeat(32);
        let mut relay = OpaqueRelay::new(RelayLimits {
            max_active_tickets: 1,
            max_active_connections: 2,
            max_frame_bytes: 64,
            max_frames_per_window: 1,
            max_bytes_per_window: 32,
        });
        let ticket = relay.issue_ticket(&mac_id, &phone_id, true, now).unwrap();
        let mac = relay.connect(&ticket, &mac_id, now).unwrap();
        let _phone = relay.connect(&ticket, &phone_id, now).unwrap();
        assert!(relay.forward(&mac, vec![7; 33], now).is_err());
        assert_eq!(relay.diagnostics().rejected_quota, 1);

        let mut connection = DirectConnection::default();
        connection.begin();
        connection.failed(ConnectionFailure::DirectTimeout);
        connection.fallback_to_relay().unwrap();
        assert_eq!(connection.diagnostics().state, ConnectionState::Relay);
        assert!(!connection.may_forward_application_data());
        connection.capability_refreshed();
        assert!(connection.may_forward_application_data());
        connection.direct_recovered_from_relay().unwrap();
        assert!(!connection.may_forward_application_data());
    }

    #[test]
    fn relay_rejects_duplicate_and_expired_connections_and_releases_capacity() {
        let now = unix_time();
        let mac_id = "a".repeat(32);
        let phone_id = "b".repeat(32);
        let mut relay = OpaqueRelay::new(RelayLimits::default());
        let ticket = relay.issue_ticket(&mac_id, &phone_id, true, now).unwrap();
        let mac = relay.connect(&ticket, &mac_id, now).unwrap();
        assert!(relay.connect(&ticket, &mac_id, now).is_err());
        let phone = relay.connect(&ticket, &phone_id, now).unwrap();
        assert!(relay.forward(&mac, vec![1], ticket.expires_at).is_err());
        relay.disconnect(&phone);
        relay.disconnect(&mac);
        assert_eq!(relay.diagnostics().active_connections, 0);
    }

    #[test]
    fn presence_rejects_loopback_candidates_from_control_plane() {
        let now = unix_time();
        let mut directory = DeviceDirectory::default();
        let record = PresenceRecord {
            device_id: "a".repeat(32),
            identity_key: phone_key(),
            candidates: vec![DirectCandidate {
                address: "127.0.0.1:42000".parse().unwrap(),
                expires_at: now + 30,
            }],
            expires_at: now + 30,
        };
        assert!(directory.publish(record, now).is_err());
    }

    proptest! {
        #[test]
        fn external_http_parser_never_panics_or_expands_arbitrary_input(bytes in prop::collection::vec(any::<u8>(), 0..MAX_INITIAL_REQUEST)) {
            if let Ok((authorized, _)) = authorize_and_inject(bytes.clone(), DevicePermission::Observe, "bounded-token") {
                prop_assert!(authorized.len() <= bytes.len() + 64);
                prop_assert!(authorized.windows(b"Authorization: Bearer bounded-token".len()).any(|part| part == b"Authorization: Bearer bounded-token"));
            }
        }
    }
}
