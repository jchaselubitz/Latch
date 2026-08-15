//! Paired, LAN-only remote-access foundation.
//!
//! This module deliberately terminates its authenticated transport before
//! opening a connection to the existing loopback-only gateway. It never
//! forwards a caller-selected destination or the gateway bearer token.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use snow::{params::NoiseParams, Builder, TransportState};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Semaphore};

use crate::cli::serve::{load_token, mint_token};
use crate::session::paths::{LatchHome, DIR_MODE, FILE_MODE};

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
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_AUDIT_EVENTS: usize = 1_024;
const MAX_AUDIT_BYTES: usize = 512 * 1024;
#[cfg(target_os = "macos")]
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

/// Access granted to a paired device.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DevicePermission {
    /// Read sessions, events, and an output-only terminal.
    Observe,
    /// Also submit structured messages and prompt resolutions.
    Interact,
    /// Also send terminal bytes and resize controls.
    Control,
}

impl DevicePermission {
    fn permits(self, required: Self) -> bool {
        matches!(
            (self, required),
            (Self::Control, _)
                | (Self::Interact, Self::Interact | Self::Observe)
                | (Self::Observe, Self::Observe)
        )
    }
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            relay_enabled: true,
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
    #[cfg(not(target_os = "macos"))]
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

/// Independently enables or disables relay fallback. Direct and LAN paths are
/// unaffected, which gives operators a narrow incident-response switch.
pub fn set_relay_enabled(home: &LatchHome, enabled: bool) -> anyhow::Result<()> {
    let paths = Paths::new(home);
    ensure_root(&paths)?;
    let mut settings: Settings = read_json_or_default(&paths.settings())?;
    settings.relay_enabled = enabled;
    write_json(&paths.settings(), &settings)?;
    audit(
        &paths,
        if enabled {
            "relay_enabled"
        } else {
            "relay_disabled"
        },
        None,
        "ok",
    )
}

/// Returns whether relay admission is currently permitted. Hosted control
/// plane adapters must check this before issuing a [`RelayTicket`].
pub fn relay_enabled(home: &LatchHome) -> anyhow::Result<bool> {
    let settings: Settings = read_json_or_default(&Paths::new(home).settings())?;
    Ok(settings.enabled && settings.relay_enabled)
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
    };
    let summary = summary(&record);
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

/// Builds an inspectable, content-free support bundle locally. Upload remains
/// an explicit caller action; this function performs no network activity.
pub fn diagnostics_export(home: &LatchHome) -> anyhow::Result<DiagnosticsExport> {
    let paths = Paths::new(home);
    let settings: Settings = read_json_or_default(&paths.settings())?;
    let store: DeviceStore = read_json_or_default(&paths.devices())?;
    let mut event_counts = HashMap::new();
    for event in read_audit(home)? {
        if let Some(name) = event.get("event").and_then(serde_json::Value::as_str) {
            *event_counts.entry(name.to_owned()).or_insert(0) += 1;
        }
    }
    Ok(DiagnosticsExport {
        format_version: 1,
        remote_access_enabled: settings.enabled,
        relay_enabled: settings.relay_enabled,
        paired_devices: store.devices.len(),
        revoked_devices: store.devices.iter().filter(|device| device.revoked).count(),
        event_counts,
    })
}

/// Starts the LAN listener and supervises a private, ephemeral gateway.
pub fn serve_lan(home: LatchHome, bind: SocketAddr, latch_bin: PathBuf) -> anyhow::Result<()> {
    let paths = Paths::new(&home);
    ensure_enabled(&paths)?;
    let identity = identity(&paths)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move { run_lan(paths, identity, bind, latch_bin).await })
}

async fn run_lan(
    paths: Paths,
    identity: Identity,
    bind: SocketAddr,
    latch_bin: PathBuf,
) -> anyhow::Result<()> {
    ensure_private_directory(&paths.runtime())?;
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
    audit(&paths, "lan_listener_started", None, "ok")?;
    let connection_limit = Arc::new(Semaphore::new(MAX_LAN_CONNECTIONS));

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let paths = paths.clone();
                let identity = identity.clone();
                let token = token.clone();
                let Ok(permit) = connection_limit.clone().try_acquire_owned() else {
                    audit(&paths, "connection_rejected", None, "capacity")?;
                    continue;
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = tokio::time::timeout(
                        HANDSHAKE_TIMEOUT,
                        proxy_connection(stream, &paths, &identity, &token, gateway_addr),
                    )
                    .await;
                    if result.is_err() {
                        let _ = audit(&paths, "connection_rejected", None, "timeout");
                    }
                });
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
            signal = tokio::signal::ctrl_c() => {
                signal?;
                let _ = gateway.kill().await;
                audit(&paths, "lan_listener_stopped", None, "ok")?;
                return Ok(());
            }
        }
    }
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

async fn start_gateway(binary: &Path, token: &Path, ready: &Path) -> anyhow::Result<Child> {
    let _ = fs::remove_file(ready);
    // Rotate the internal credential before every launch. Minting here avoids
    // the public `serve` command printing a newly-created secret to stderr.
    mint_token(token)?;
    Command::new(binary)
        .args(["serve", "--bind", "127.0.0.1:0", "--token-file"])
        .arg(token)
        .args(["--ready-file"])
        .arg(ready)
        .kill_on_drop(true)
        .spawn()
        .context("cannot start supervised loopback gateway")
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

fn advertise_bonjour(identity: &Identity, port: u16) -> anyhow::Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new().context("cannot start Bonjour service")?;
    let instance = format!("latch-{}", &identity.device_id[..12]);
    let service = ServiceInfo::new("_latch-remote._tcp.local.", &instance, "", "", port, None)
        .context("cannot build Bonjour service record")?;
    daemon
        .register(service)
        .context("cannot advertise Bonjour service")?;
    Ok(daemon)
}

async fn proxy_connection(
    stream: TcpStream,
    paths: &Paths,
    identity: &Identity,
    token_path: &Path,
    gateway_addr: SocketAddr,
) -> anyhow::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let (mut state, peer_static) = responder_handshake(&mut reader, &mut writer, identity).await?;
    let peer_key = hex_encode(&peer_static);
    let device = lookup_device(paths, &peer_key)?.ok_or_else(|| anyhow!("unpaired device"))?;
    if device.revoked {
        bail!("revoked device");
    }
    let mut initial = decrypt_record(&mut reader, &mut state).await?;
    while !initial.windows(4).any(|window| window == b"\r\n\r\n") {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request headers exceed limit");
        }
        initial.extend(decrypt_record(&mut reader, &mut state).await?);
    }
    let required_len = complete_initial_request_len(&initial)?;
    while initial.len() < required_len {
        if initial.len() >= MAX_INITIAL_REQUEST {
            bail!("initial request exceeds limit");
        }
        initial.extend(decrypt_record(&mut reader, &mut state).await?);
    }
    let token = load_token(token_path)?;
    let initial = authorize_and_inject(initial, device.permission, &token)?;
    let gateway = TcpStream::connect(gateway_addr)
        .await
        .context("cannot connect to loopback gateway")?;
    let (mut gateway_reader, mut gateway_writer) = gateway.into_split();
    gateway_writer.write_all(&initial).await?;
    audit(paths, "connection_opened", Some(&device.device_id), "ok")?;

    let state = Arc::new(Mutex::new(state));
    let outbound_state = state.clone();
    let outbound = tokio::spawn(async move {
        let mut buf = vec![0_u8; 16 * 1024];
        loop {
            let read = gateway_reader.read(&mut buf).await?;
            if read == 0 {
                return Ok::<(), anyhow::Error>(());
            }
            let mut locked = outbound_state.lock().await;
            encrypt_record(&mut writer, &mut locked, &buf[..read]).await?;
        }
    });
    let inbound_state = state.clone();
    let inbound = tokio::spawn(async move {
        loop {
            let mut locked = inbound_state.lock().await;
            let bytes = decrypt_record(&mut reader, &mut locked).await?;
            drop(locked);
            gateway_writer.write_all(&bytes).await?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    let mut revocation_check = tokio::time::interval(Duration::from_millis(250));
    tokio::select! {
        result = outbound => result.context("encrypted response task failed")??,
        result = inbound => result.context("encrypted request task failed")??,
        _ = async {
            loop {
                revocation_check.tick().await;
                match lookup_device(paths, &peer_key)? {
                    Some(current) if !current.revoked => {}
                    _ => return Ok::<(), anyhow::Error>(()),
                }
            }
        } => { audit(paths, "connection_closed", Some(&device.device_id), "revoked")?; }
    }
    Ok(())
}

fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    token: &str,
) -> anyhow::Result<Vec<u8>> {
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
        || !target.starts_with("/v1/")
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
    let required = permission_for_request(method, target, &request[end + 4..required_len])?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 32);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(&request[end..]);
    Ok(injected)
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

fn permission_for_request(
    method: &str,
    target: &str,
    body: &[u8],
) -> anyhow::Result<DevicePermission> {
    let path = target.split('?').next().unwrap_or(target);
    if method == "GET" {
        if path == "/v1/capabilities" || path == "/v1/sessions" {
            return Ok(DevicePermission::Observe);
        }
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() == 4 && parts[1] == "v1" && parts[2] == "sessions" && !parts[3].is_empty() {
            return Ok(DevicePermission::Observe);
        }
        if parts.len() == 5
            && parts[1] == "v1"
            && parts[2] == "sessions"
            && !parts[3].is_empty()
            && matches!(parts[4], "capabilities" | "events")
        {
            return Ok(DevicePermission::Observe);
        }
        if parts.len() == 5
            && parts[1] == "v1"
            && parts[2] == "sessions"
            && !parts[3].is_empty()
            && parts[4] == "terminal"
        {
            return if target.contains("mode=read-only") {
                Ok(DevicePermission::Observe)
            } else {
                Ok(DevicePermission::Control)
            };
        }
        {
            bail!("HTTP operation is not permitted through remote access");
        }
    }
    if method == "POST" && path.ends_with("/send") {
        let parts: Vec<_> = path.split('/').collect();
        if parts.len() != 5 || parts[1] != "v1" || parts[2] != "sessions" || parts[3].is_empty() {
            bail!("HTTP operation is not permitted through remote access");
        }
        let body: serde_json::Value =
            serde_json::from_slice(body).context("invalid send request")?;
        if body.get("keys").is_some() {
            return Ok(DevicePermission::Control);
        }
        if body.get("message").is_none() && body.get("resolve").is_none() {
            bail!("HTTP operation is not permitted through remote access");
        }
        return Ok(DevicePermission::Interact);
    }
    bail!("HTTP operation is not permitted through remote access")
}

async fn responder_handshake(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    identity: &Identity,
) -> anyhow::Result<(TransportState, Vec<u8>)> {
    let params: NoiseParams = NOISE_PATTERN.parse().expect("valid Noise pattern");
    let private = decode_static_key(&identity.private_key)?;
    let mut handshake = Builder::new(params)
        .local_private_key(&private)
        .build_responder()?;
    let first = read_frame(reader).await?;
    let mut scratch = vec![0_u8; MAX_RECORD];
    handshake.read_message(&first, &mut scratch)?;
    let mut response = vec![0_u8; MAX_RECORD];
    let used = handshake.write_message(&[], &mut response)?;
    write_frame(writer, &response[..used]).await?;
    let third = read_frame(reader).await?;
    handshake.read_message(&third, &mut scratch)?;
    let peer_static = handshake
        .get_remote_static()
        .ok_or_else(|| anyhow!("peer did not present a static identity"))?
        .to_vec();
    Ok((handshake.into_transport_mode()?, peer_static))
}

async fn decrypt_record(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    state: &mut TransportState,
) -> anyhow::Result<Vec<u8>> {
    let encrypted = read_frame(reader).await?;
    let mut plain = vec![0_u8; MAX_RECORD];
    let used = state.read_message(&encrypted, &mut plain)?;
    plain.truncate(used);
    Ok(plain)
}

async fn encrypt_record(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    state: &mut TransportState,
    plain: &[u8],
) -> anyhow::Result<()> {
    if plain.len() > MAX_RECORD - 32 {
        bail!("remote frame exceeds limit");
    }
    let mut encrypted = vec![0_u8; MAX_RECORD];
    let used = state.write_message(plain, &mut encrypted)?;
    write_frame(writer, &encrypted[..used]).await
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

#[cfg(target_os = "macos")]
fn store_identity_secret(_paths: &Paths, account: &str, secret: &str) -> anyhow::Result<()> {
    security_framework::passwords::set_generic_password(SECRET_SERVICE, account, secret.as_bytes())
        .context("cannot store the remote-access identity in macOS Keychain")
}

#[cfg(target_os = "macos")]
fn load_identity_secret(_paths: &Paths, account: &str) -> anyhow::Result<String> {
    let bytes = security_framework::passwords::get_generic_password(SECRET_SERVICE, account)
        .context("cannot load the remote-access identity from macOS Keychain")?;
    String::from_utf8(bytes).context("stored remote-access identity is not UTF-8")
}

#[cfg(not(target_os = "macos"))]
fn store_identity_secret(paths: &Paths, _account: &str, secret: &str) -> anyhow::Result<()> {
    // Non-macOS builds are development/headless clients. Keep the same split
    // metadata boundary in an owner-only file; production macOS uses Keychain.
    write_bytes_atomic(&paths.identity_secret(), secret.as_bytes())
}

#[cfg(not(target_os = "macos"))]
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
    fn phone_key() -> String {
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        hex_encode(&Builder::new(params).generate_keypair().unwrap().public)
    }

    fn keypair() -> (String, String) {
        let params: NoiseParams = NOISE_PATTERN.parse().unwrap();
        let pair = Builder::new(params).generate_keypair().unwrap();
        (hex_encode(&pair.private), hex_encode(&pair.public))
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
        )
        .unwrap();
        assert_eq!(device.permission, DevicePermission::Interact);
        assert!(confirm_pairing(
            &home,
            &material.pairing_id,
            &material.secret,
            &phone_key(),
            "Other",
            DEFAULT_PERMISSION
        )
        .is_err());
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
            b"GET /v1/sessions/ses_1/terminal?mode=control HTTP/1.1\r\nHost: example\r\n\r\n"
                .to_vec();
        assert!(authorize_and_inject(request, DevicePermission::Observe, "secret").is_err());
    }

    #[test]
    fn proxy_only_injects_its_own_gateway_credential() {
        let request =
            b"GET /v1/sessions HTTP/1.1\r\nHost: example\r\nAuthorization: Bearer stolen\r\n\r\n"
                .to_vec();
        assert!(authorize_and_inject(request, DevicePermission::Control, "secret").is_err());
    }

    #[test]
    fn proxy_rejects_request_smuggling_and_closes_plain_http() {
        let pipelined = b"GET /v1/sessions HTTP/1.1\r\nHost: example\r\n\r\nGET /v1/sessions/ses_1/terminal?mode=control HTTP/1.1\r\nHost: example\r\n\r\n".to_vec();
        assert!(authorize_and_inject(pipelined, DevicePermission::Observe, "secret").is_err());
        let smuggled = b"POST /v1/sessions/ses_1/send HTTP/1.1\r\nHost: example\r\nContent-Length: 2\r\nContent-Length: 80\r\n\r\n{}".to_vec();
        assert!(authorize_and_inject(smuggled, DevicePermission::Interact, "secret").is_err());
        let transfer = b"POST /v1/sessions/ses_1/send HTTP/1.1\r\nHost: example\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec();
        assert!(authorize_and_inject(transfer, DevicePermission::Interact, "secret").is_err());
        let ordinary = b"GET /v1/sessions HTTP/1.1\r\nHost: example\r\n\r\n".to_vec();
        let authorized =
            authorize_and_inject(ordinary, DevicePermission::Observe, "secret").unwrap();
        assert!(authorized
            .windows(b"Connection: close".len())
            .any(|part| part == b"Connection: close"));
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
            if let Ok(authorized) = authorize_and_inject(bytes.clone(), DevicePermission::Observe, "bounded-token") {
                prop_assert!(authorized.len() <= bytes.len() + 64);
                prop_assert!(authorized.windows(b"Authorization: Bearer bounded-token".len()).any(|part| part == b"Authorization: Bearer bounded-token"));
            }
        }
    }
}
