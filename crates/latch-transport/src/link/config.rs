//! Fixed link bounds, endpoint roles and purposes, and the wire shapes
//! exchanged before and on every logical stream.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::LinkError;

/// Maximum encoded Noise record carried by one WebSocket message or LAN frame.
pub const MAX_RECORD_BYTES: usize = 65_535;
/// Plaintext is deliberately split so one writer cannot monopolize the link.
pub const WRITE_QUANTUM_BYTES: usize = 16 * 1024;
/// Maximum concurrent application streams per authenticated link.
pub const MAX_STREAMS: usize = 32;
/// Total Yamux receive credit across all streams.
pub const MAX_RECEIVE_WINDOW_BYTES: usize = 8 * 1024 * 1024;
/// Deadline for Noise authentication and LinkHello once both peers are present.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// An idle link sends one empty encrypted record at this interval so silent
/// transport loss is detected on both carriers without any relay help.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
/// A link that receives nothing for this long is dead, whatever the socket says.
pub const DEAD_PEER_TIMEOUT: Duration = Duration::from_secs(45);
/// Silence bound on a relay carrier, applied from the moment it is connected
/// rather than from the moment it is authenticated. The relay pings every 15
/// seconds, so a carrier that delivers nothing at all for three ping periods
/// is on a path that is silently discarding traffic: ESTABLISHED, but dead.
/// Before this bound existed an endpoint waiting for its peer could sit on
/// such a socket indefinitely, because no close frame could ever reach it.
pub const RELAY_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(45);
/// Bound on the relay connect: TCP, TLS, and the WebSocket upgrade together.
pub const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on handing one record or control frame to the relay carrier. A write
/// that cannot drain within this bound is on the same kind of stranded path.
/// Expiry drops a partly written frame, so it is always fatal to the carrier:
/// every caller ends the link on a send error rather than writing again.
pub const RELAY_WRITE_TIMEOUT: Duration = Duration::from_secs(20);

/// Most relay records retained while waiting for the opposite role. Together
/// with [`MAX_RECEIVE_WINDOW_BYTES`] this bounds what an unauthenticated relay
/// peer can make an endpoint hold before any Noise message is processed.
pub const MAX_PENDING_RECORDS: usize = 128;

/// Bound on delivering the final frames of a link that is closing normally:
/// bytes already written (a response, an enrollment receipt) must reach the
/// carrier before it closes, and frames already received must reach their
/// streams before the connection is dropped. Cancellation past this bound is
/// immediate.
pub(super) const CLOSE_DRAIN_LIMIT: Duration = Duration::from_secs(3);

/// Liveness timing for one link. Production callers use the defaults; tests
/// shorten them to exercise the same code paths in milliseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkTimings {
    /// Idle interval between keepalive records.
    pub keepalive_interval: Duration,
    /// Silence after which the link is declared dead and torn down.
    pub dead_peer_timeout: Duration,
}

impl Default for LinkTimings {
    fn default() -> Self {
        Self {
            keepalive_interval: KEEPALIVE_INTERVAL,
            dead_peer_timeout: DEAD_PEER_TIMEOUT,
        }
    }
}

/// Content-free stage durations for one link establishment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LinkStageTimings {
    /// Transport connect (TCP/TLS/WebSocket upgrade, or LAN TCP connect).
    pub connect_ms: u64,
    /// Time spent waiting for the relay to report the opposite role present.
    pub peer_wait_ms: u64,
    /// Noise XX plus LinkHello.
    pub authenticate_ms: u64,
}

/// Link purpose is cryptographically bound so enrollment cannot open a gateway.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPurpose {
    /// Provisional, QR-bound enrollment only.
    Enrollment,
    /// A normal link between two locally pinned devices.
    Session,
}

/// Fixed endpoint roles. Controllers initiate Noise and Yamux.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkRole {
    /// Mac helper and gateway authority.
    Host,
    /// Phone or another approved controller.
    Controller,
}

/// Only these services may be opened over a logical stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    /// Fixed local `/v2` gateway.
    Gateway,
    /// Provisional enrollment exchange.
    Enrollment,
    /// Reserved link-control stream.
    Control,
}

/// Negotiated resource bounds. V1 accepts exactly these values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkLimits {
    /// Maximum application streams.
    pub max_streams: u16,
    /// Initial receive credit per stream.
    pub stream_window_bytes: u32,
    /// Maximum aggregate receive credit.
    pub total_buffer_bytes: u32,
    /// Maximum Noise record size.
    pub record_bytes: u32,
}

impl Default for LinkLimits {
    fn default() -> Self {
        Self {
            max_streams: MAX_STREAMS as u16,
            stream_window_bytes: 256 * 1024,
            total_buffer_bytes: MAX_RECEIVE_WINDOW_BYTES as u32,
            record_bytes: MAX_RECORD_BYTES as u32,
        }
    }
}

/// Encrypted hello exchanged before the multiplexer is exposed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkHello {
    /// Must be `link_hello`.
    pub r#type: String,
    /// Remote Link version.
    pub version: u16,
    /// Fresh endpoint nonce, hex encoded.
    pub nonce: String,
    /// Host-owned grant revision known to this endpoint.
    pub grant_revision: u64,
    /// Fixed V1 limits.
    pub limits: LinkLimits,
}

/// Header sent first on every Yamux stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenService {
    /// Must be `open_service`.
    pub r#type: String,
    /// Remote Link version.
    pub version: u16,
    /// Requested fixed service.
    pub service: Service,
    /// Grant revision held by the controller.
    pub grant_revision: u64,
}

/// Inputs required to authenticate one endpoint.
pub struct LinkConfig {
    /// Purpose bound into the Noise prologue.
    pub purpose: LinkPurpose,
    /// Local fixed role.
    pub role: LinkRole,
    /// Local 32-byte X25519 private key.
    pub local_private_key: Zeroizing<Vec<u8>>,
    /// Local 32-byte X25519 public key used in the canonical session prologue.
    pub local_public_key: Vec<u8>,
    /// Exact peer static key, when already paired.
    pub expected_remote_public_key: Option<Vec<u8>>,
    /// Enrollment identifier for provisional links.
    pub enrollment_id: Option<String>,
    /// QR-only 256-bit enrollment secret, never sent to a service.
    pub enrollment_secret: Option<Zeroizing<Vec<u8>>>,
    /// Current host grant revision.
    pub grant_revision: u64,
    /// Keepalive and dead-peer timing.
    pub timings: LinkTimings,
}

impl LinkConfig {
    pub(super) fn validate(&self) -> Result<(), LinkError> {
        if self.local_private_key.len() != 32 {
            return Err(LinkError::Configuration(
                "local private key must be 32 bytes",
            ));
        }
        if self.local_public_key.len() != 32 {
            return Err(LinkError::Configuration(
                "local public key must be 32 bytes",
            ));
        }
        if self
            .expected_remote_public_key
            .as_ref()
            .is_some_and(|key| key.len() != 32)
        {
            return Err(LinkError::Configuration(
                "remote public key must be 32 bytes",
            ));
        }
        match self.purpose {
            LinkPurpose::Session if self.expected_remote_public_key.is_none() => Err(
                LinkError::Configuration("session links require an exact peer pin"),
            ),
            LinkPurpose::Enrollment
                if self.enrollment_id.as_deref().unwrap_or_default().is_empty()
                    || self
                        .enrollment_secret
                        .as_ref()
                        .is_none_or(|secret| secret.len() != 32) =>
            {
                Err(LinkError::Configuration(
                    "enrollment links require an id and 32-byte QR secret",
                ))
            }
            _ => Ok(()),
        }
    }
}
