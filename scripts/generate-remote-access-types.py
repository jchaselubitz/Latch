#!/usr/bin/env python3
"""Generate the Rust and TypeScript remote-access contract types.

The JSON Schema documents are the canonical wire contract. This intentionally
small generator validates their versioned identifiers before emitting the two
runtime representations; a future Swift target consumes the same schemas via
the documented OpenAPI conversion path rather than copying these types.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCHEMAS = ROOT / "schemas/remote-access/v1"
RUST = ROOT / "crates/latch/src/cli/serve/contract.rs"
TYPESCRIPT = ROOT / "packages/client/src/generated.ts"


def load(name: str) -> dict:
    document = json.loads((SCHEMAS / name).read_text())
    expected = f"https://latch.cooperativ.dev/schemas/remote-access/v1/{name}"
    if document.get("$id") != expected:
        raise ValueError(f"{name} must use canonical id {expected}")
    return document


def rust_source() -> str:
    return '''//! Generated from `schemas/remote-access/v1/*.schema.json`; do not edit by hand.

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
'''


def typescript_source() -> str:
    return '''// Generated from schemas/remote-access/v1/*.schema.json; do not edit by hand.

export type TerminalAccessMode = 'control' | 'read-only';

export type GatewayFeatures = {
  idempotencyKeys: boolean;
  readOnlyTerminal: boolean;
};

export type GatewayReadiness = {
  formatVersion: 1;
  address: string;
  url: string;
  protocolVersion: 1;
  gatewayInstanceId: string;
};

export type IdempotencyKey = string;
'''


def main() -> None:
    # Validate every schema family even where a type is deliberately only a
    # header/body contract and therefore has no standalone generated struct.
    for name in (
        "gateway-capabilities.schema.json",
        "gateway-readiness.schema.json",
        "terminal-connection.schema.json",
        "idempotency-key.schema.json",
        "send-request.schema.json",
    ):
        load(name)
    outputs = {RUST: rust_source(), TYPESCRIPT: typescript_source()}
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
