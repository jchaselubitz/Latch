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
    "conversation-item.schema.json",
    "conversation-state.schema.json",
    "conversation-protocol.schema.json",
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

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalAccessMode {
    #[default]
    Control,
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFeatures {
    pub read_only_terminal: bool,
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
pub enum OperationResultStatus { Accepted, Refused, Ambiguous }

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
}
'''
    return subprocess.run(
        ["rustfmt", "--emit", "stdout"],
        input=source,
        text=True,
        check=True,
        capture_output=True,
    ).stdout


def typescript_source(schema_digest: str) -> str:
    return f'''// Generated from schemas/remote-access/v2/*.schema.json; do not edit by hand.
// Canonical schema set SHA-256: {schema_digest}
''' + '''

export type TerminalAccessMode = 'control' | 'read-only';
export type GatewayFeatures = { readOnlyTerminal: boolean };
export type GatewayReadiness = {
  formatVersion: 2;
  address: string;
  url: string;
  protocolVersion: 2;
  gatewayInstanceId: string;
};
'''


def main() -> None:
    for name in SCHEMA_NAMES:
        load(name)
    schema_digest = contract_digest()
    outputs = {
        RUST: rust_source(schema_digest),
        TYPESCRIPT: typescript_source(schema_digest),
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
