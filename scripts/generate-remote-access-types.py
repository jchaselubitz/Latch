#!/usr/bin/env python3
"""Generate Rust types and terminal TypeScript types from canonical v2 schemas.

The deliberately small generator keeps the generated representations reviewable
while making the JSON Schema documents the source of truth for protocol major 2.
"""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCHEMAS = ROOT / "schemas/remote-access/v2"
RUST = ROOT / "crates/latch/src/cli/serve/contract.rs"
TYPESCRIPT = ROOT / "packages/client/src/generated.ts"
SCHEMA_NAMES = (
    "create-session-request.schema.json",
    "create-session-response.schema.json",
    "conversation-item.schema.json",
    "conversation-state.schema.json",
    "conversation-protocol.schema.json",
    "directory-page.schema.json",
    "gateway-capabilities.schema.json",
    "gateway-readiness.schema.json",
    "terminal-connection.schema.json",
)


def load(name: str) -> dict:
    document = json.loads((SCHEMAS / name).read_text())
    expected = f"https://latch.cooperativ.dev/schemas/remote-access/v2/{name}"
    if document.get("$id") != expected:
        raise ValueError(f"{name} must use canonical id {expected}")
    return document


def contract_digest() -> str:
    payload = "".join((SCHEMAS / name).read_text() for name in SCHEMA_NAMES)
    return hashlib.sha256(payload.encode()).hexdigest()


def rust_source(schema_digest: str) -> str:
    source = f'''//! Generated from `schemas/remote-access/v2/*.schema.json`; do not edit by hand.
//! Canonical schema set SHA-256: {schema_digest}
''' + r'''

use serde::{Deserialize, Serialize};

pub const REMOTE_ACCESS_SCHEMA_VERSION: u8 = 2;
pub const OPERATION_RETENTION_SECONDS: u64 = 10 * 60;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryPage {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    pub request_id: String,
    pub cwd: String,
}

/// Reason carried in a terminal WebSocket close frame. `Detached` is a clean
/// end; every other value says why the single exclusive surface was taken away.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCloseReason {
    Detached,
    Stolen,
    SlowClient,
    SessionExited,
    KernelError,
    ResumeRefused,
}

impl TerminalCloseReason {
    /// Application close code paired with this reason. `Detached` uses the
    /// ordinary 1000.
    pub const fn close_code(self) -> u16 {
        match self {
            Self::Detached => 1000,
            Self::SlowClient => 4408,
            Self::Stolen => 4409,
            Self::SessionExited => 4410,
            Self::KernelError => 4500,
            Self::ResumeRefused => 4411,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFeatures {
    /// Always true: a terminal connection is the session's one exclusive
    /// surface. The field remains so a client can detect a gateway that
    /// predates the exclusive cutover and refuse it.
    pub exclusive_terminal: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayReadiness {
    pub format_version: u8,
    pub address: String,
    pub url: String,
    pub protocol_version: u32,
    pub gateway_instance_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole { User, Assistant }

/// `Partial` is reserved in v2. Clients render unknown future statuses as complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus { Submitted, Observed, Partial, Complete, Failed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus { Running, Succeeded, Failed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestType { Permission, Question }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus { Pending, Resolved, Dismissed }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationItemKind {
    Message { role: MessageRole, text: String, status: MessageStatus },
    Tool {
        name: String,
        summary: String,
        status: ToolStatus,
        #[serde(rename = "parentMessageId", skip_serializing_if = "Option::is_none")]
        parent_message_id: Option<String>,
    },
    Request {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "requestType")]
        request_type: RequestType,
        prompt: String,
        choices: Vec<String>,
        status: RequestStatus,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationItem {
    pub id: String,
    /// Hub-assigned observation order. Clients never sort by `created_at`.
    pub ordinal: u64,
    /// Display metadata only.
    pub created_at: String,
    pub kind: ConversationItemKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPhase { Starting, Idle, Working, AwaitingInput, Exited, Unavailable }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OperationAvailability {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorIdentity { pub id: String, pub version: String }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    pub phase: ConversationPhase,
    pub send_message: OperationAvailability,
    pub resolve_request: OperationAvailability,
    /// Derived from the newest request item whose status is pending.
    pub pending_request: Option<String>,
    pub connector: Option<ConnectorIdentity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotReason { Initial, Generation, OperationEpoch, Overflow }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationResultStatus { Accepted, Refused, Ambiguous, Unknown }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationServerMessage {
    Snapshot {
        generation: String,
        revision: u64,
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        items: Vec<ConversationItem>,
        state: ConversationState,
        #[serde(rename = "hasMoreBefore")]
        has_more_before: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<SnapshotReason>,
    },
    ItemsUpserted { generation: String, revision: u64, items: Vec<ConversationItem> },
    ItemsRemoved {
        generation: String,
        revision: u64,
        #[serde(rename = "itemIds")]
        item_ids: Vec<String>,
    },
    StateChanged { generation: String, revision: u64, state: ConversationState },
    OperationResult {
        #[serde(rename = "operationId")]
        operation_id: String,
        status: OperationResultStatus,
        #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    HistoryPage {
        #[serde(rename = "requestId")]
        request_id: String,
        items: Vec<ConversationItem>,
        #[serde(rename = "hasMoreBefore")]
        has_more_before: bool,
    },
    Error { code: String, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationClientMessage {
    Resume {
        #[serde(skip_serializing_if = "Option::is_none")]
        generation: Option<String>,
        #[serde(rename = "afterRevision", skip_serializing_if = "Option::is_none")]
        after_revision: Option<u64>,
    },
    SendMessage {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        text: String,
    },
    ResolveRequest {
        #[serde(rename = "operationEpoch")]
        operation_epoch: String,
        #[serde(rename = "operationId")]
        operation_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        choice: String,
    },
    HistoryRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "beforeOrdinal")]
        before_ordinal: u64,
        limit: u16,
    },
    OperationStatus {
        #[serde(rename = "operationId")]
        operation_id: String,
    },
}

/// Text frame sent once the terminal surface is held by this socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAttachedFrame {
    pub r#type: String,
    pub resume_capability: String,
    pub resume_window_seconds: u64,
}
'''
    return subprocess.run(
        ["rustfmt", "--emit", "stdout"],
        input=source,
        text=True,
        check=True,
        capture_output=True,
    ).stdout


def typescript_endpoints(capabilities: dict) -> str:
    """Emit `GatewayEndpoints` from the discovery schema rather than by hand.

    A key the schema lists as required is always present; every other route is
    optional because a gateway that predates it omits the key, and a client must
    read that omission as unavailable rather than as false-by-default.
    """
    endpoints = capabilities["properties"]["endpoints"]
    required = set(endpoints["required"])
    names = list(endpoints["properties"])
    fields = "".join(
        f"  {name}{'' if name in required else '?'}: boolean;\n" for name in names
    )
    union = " | ".join(f"'{name}'" for name in names)
    return (
        "/** Routes the gateway serves. An absent key means the gateway predates\n"
        " * that route: treat it as unavailable, never as false-by-default. */\n"
        f"export type GatewayEndpoints = {{\n{fields}}};\n"
        f"export type GatewayEndpointName = {union};\n"
    )


def typescript_source(schema_digest: str, capabilities: dict) -> str:
    return f'''// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: {schema_digest}
''' + '''

export type TerminalCloseReason =
  | 'detached'
  | 'stolen'
  | 'slow_client'
  | 'session_exited'
  | 'kernel_error'
  | 'resume_refused';
export const TERMINAL_CLOSE_CODES = {
  detached: 1000,
  slow_client: 4408,
  stolen: 4409,
  session_exited: 4410,
  kernel_error: 4500,
  resume_refused: 4411
} as const satisfies Record<TerminalCloseReason, number>;
export type GatewayFeatures = { exclusiveTerminal: boolean };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
''' + typescript_endpoints(capabilities)


def main() -> None:
    documents = {name: load(name) for name in SCHEMA_NAMES}
    schema_digest = contract_digest()
    outputs = {
        RUST: rust_source(schema_digest),
        TYPESCRIPT: typescript_source(
            schema_digest, documents["gateway-capabilities.schema.json"]
        ),
    }
    if sys.argv[1:] == ["--check"]:
        stale = [path for path, source in outputs.items() if not path.is_file() or path.read_text() != source]
        if stale:
            names = ", ".join(str(path.relative_to(ROOT)) for path in stale)
            raise SystemExit(f"generated remote-access types are stale: {names}")
        return
    if sys.argv[1:]:
        raise SystemExit("usage: generate-remote-access-types.py [--check]")
    for path, source in outputs.items():
        path.write_text(source)


if __name__ == "__main__":
    main()
