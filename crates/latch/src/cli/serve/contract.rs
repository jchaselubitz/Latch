//! Generated from `schemas/remote-access/v1/*.schema.json`; do not edit by hand.

use serde::{Deserialize, Serialize};

/// Canonical schema-major version for the remote-access contract bundle.
pub const REMOTE_ACCESS_SCHEMA_VERSION: u8 = 1;

/// Access mode for one terminal WebSocket.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalAccessMode {
    /// Existing terminal behavior: binary input and resize controls are applied.
    #[default]
    Control,
    /// Output-only observer. Input and resize controls are ignored.
    ReadOnly,
}

/// Optional v1 gateway features that require discovery before use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFeatures {
    /// POST message and resolve operations accept Idempotency-Key.
    pub idempotency_keys: bool,
    /// The terminal endpoint accepts mode=read-only.
    pub read_only_terminal: bool,
}

/// Private structured startup handoff for a gateway supervisor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayReadiness {
    /// Readiness document version.
    pub format_version: u8,
    /// Bound loopback address.
    pub address: String,
    /// HTTP base URL for the bound loopback address.
    pub url: String,
    /// Latch protocol major served by this process.
    pub protocol_version: u32,
    /// Opaque per-process identifier used to detect a supervisor restart.
    pub gateway_instance_id: String,
}
