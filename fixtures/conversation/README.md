# Conversation connector corpus

These raw fixtures are intentionally agent-owned source examples, not normalized
Hub events. Phase 1 keeps them beside the domain tests so future Claude and Codex
connectors must prove their assumptions at the connector boundary.

- `claude/` covers a turn, tool lifecycle, permission request, branch rewrite, and direct terminal input.
- `codex/` covers the same categories using Codex-shaped source records.

They are not consumed by the Hub or protocol and contain no ordinals, revisions, or generations.
