#!/usr/bin/env python3
"""Generate Rust and TypeScript harness contracts from their JSON schemas."""

from __future__ import annotations

import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EVENT_SCHEMA = ROOT / 'fixtures/harness/harness-event.v1.json'
CAPABILITIES_SCHEMA = ROOT / 'fixtures/harness/interaction-capabilities.v1.json'
RUST_OUTPUT = ROOT / 'crates/latch/src/harness/generated.rs'
TYPESCRIPT_OUTPUT = ROOT / 'packages/harness-schema/src/generated.ts'


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def event_variants(schema: dict) -> list[str]:
    return list(schema['properties']['type']['enum'])


def rust_source(variants: list[str]) -> str:
    expected = [
        'user_message',
        'assistant_delta',
        'assistant_message',
        'tool_started',
        'tool_finished',
        'awaiting_input',
        'status'
    ]
    if variants != expected:
        raise ValueError(f'unsupported harness event variants: {variants}')
    return '''//! Generated from `fixtures/harness/*.v1.json`; do not edit by hand.

use serde::{Deserialize, Serialize};

/// One normalized harness observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessEvent {
    /// Harness session identifier.
    pub session_id: String,
    /// RFC 3339 observation timestamp.
    pub at: String,
    /// Harness version that produced the raw record.
    pub harness_version: String,
    /// Connector derivation epoch used to validate cursors.
    pub connector_epoch: u32,
    /// Variant-specific event data.
    #[serde(flatten)]
    pub payload: HarnessEventPayload,
}

/// Variant-specific event data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarnessEventPayload {
    /// User-authored text.
    UserMessage {
        /// Message text.
        text: String,
    },
    /// Streaming assistant text.
    AssistantDelta {
        /// Delta text.
        text: String,
    },
    /// Complete assistant text.
    AssistantMessage {
        /// Message text.
        text: String,
    },
    /// Tool invocation began.
    ToolStarted {
        /// Harness-native tool name.
        tool: String,
        /// Reduced, safe input details.
        input: serde_json::Value,
    },
    /// Tool invocation completed.
    ToolFinished {
        /// Harness-native tool name.
        tool: String,
        /// Reduced, safe output details.
        output: serde_json::Value,
    },
    /// The harness is waiting for a person.
    AwaitingInput {
        /// Stable native request identifier.
        #[serde(rename = "requestId")]
        request_id: String,
        /// Permission or structured question.
        kind: AwaitingInputKind,
        /// Human-readable request.
        prompt: String,
        /// Choices when the harness supplied a closed set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choices: Option<Vec<String>>,
    },
    /// Harness lifecycle state changed.
    Status {
        /// Harness-native normalized status.
        status: String,
    },
}

/// Kinds of input that can block a harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingInputKind {
    /// A tool needs authorization.
    Permission,
    /// The harness asked a structured question.
    Question,
}

/// Operations a client may safely offer for one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InteractionCapabilities {
    /// Free-text message injection.
    pub send_message: bool,
    /// Direct keypress injection.
    pub send_keys: bool,
    /// Resolution of a specific pending request.
    pub resolve: bool,
    /// Current input-safety decision.
    pub can_send: CanSend,
}

/// Current input-safety decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanSend {
    /// Whether input is currently safe.
    pub ok: bool,
    /// Why input is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
'''


def typescript_source(variants: list[str]) -> str:
    union = ' | '.join(f"'{variant}'" for variant in variants)
    return f'''// Generated from fixtures/harness/*.v1.json; do not edit by hand.

export type HarnessEventType = {union};

type HarnessEventBase = {{
  sessionId: string;
  at: string;
  harnessVersion: string;
  connectorEpoch: number;
}};

export type HarnessEvent =
  | (HarnessEventBase & {{ type: 'user_message'; text: string }})
  | (HarnessEventBase & {{ type: 'assistant_delta'; text: string }})
  | (HarnessEventBase & {{ type: 'assistant_message'; text: string }})
  | (HarnessEventBase & {{ type: 'tool_started'; tool: string; input: unknown }})
  | (HarnessEventBase & {{ type: 'tool_finished'; tool: string; output: unknown }})
  | (HarnessEventBase & {{
      type: 'awaiting_input';
      requestId: string;
      kind: 'permission' | 'question';
      prompt: string;
      choices?: string[];
    }})
  | (HarnessEventBase & {{ type: 'status'; status: string }});

export type InteractionCapabilities = {{
  sendMessage: boolean;
  sendKeys: boolean;
  resolve: boolean;
  canSend: {{
    ok: boolean;
    reason?: string;
  }};
}};
'''


def main() -> None:
    events = load(EVENT_SCHEMA)
    load(CAPABILITIES_SCHEMA)
    variants = event_variants(events)
    outputs = {
        RUST_OUTPUT: rust_source(variants),
        TYPESCRIPT_OUTPUT: typescript_source(variants)
    }
    if sys.argv[1:] == ['--check']:
        stale = [path for path, source in outputs.items() if path.read_text() != source]
        if stale:
            names = ', '.join(str(path.relative_to(ROOT)) for path in stale)
            raise SystemExit(f'generated harness types are stale: {names}')
        return
    if sys.argv[1:]:
        raise SystemExit('usage: generate-harness-types.py [--check]')
    for path, source in outputs.items():
        path.write_text(source)


if __name__ == '__main__':
    main()
