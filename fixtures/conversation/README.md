# Conversation fixtures

Captured Claude Code transcripts and the Hub projection they produce today.
Later conversation UI work should justify presentation decisions against these
files rather than against invented records.

`codex/source-corpus.jsonl` is an invented generic-reader format. Nothing in
this repository emits it, and it is **not** a model for new fixtures. Replace it
from a real Codex session in the Codex connector phase.

## Layout

```text
fixtures/conversation/
  README.md
  claude/cases/<id>/
    meta.json         # what the case covers and how it was captured
    source.jsonl      # sanitized Claude Code records (connector input)
    expected.json     # current v2 wire snapshot (connector + projection output)
  presentation/
    reconnect-optimistic.json
    operations-refused-ambiguous.json
  codex/source-corpus.jsonl   # invented; do not copy this shape
```

`expected.json` is the public Conversation Hub snapshot the phone already
decodes: `message`, `tool`, and `request` items with Hub ordinals. It is the
baseline for every later phase. When the connector starts emitting richer
fields, regenerate it with `UPDATE_CONVERSATION_FIXTURES=1 cargo test -p latch
--lib claude_cases_match_checked_in_projections` and treat the diff as the
behavioural change under review.

## Claude cases

| Case | Covers | Capture |
| --- | --- | --- |
| `markdown-prose` | Multi-paragraph assistant Markdown | Real Latch checkout session `0e920c11…`, pairing-debug turn. Intermediate tools omitted; `parentUuid` relinked so the slice is a valid main chain. |
| `fenced-code` | Fenced blocks and command lines long enough to need horizontal scrolling | Real session `4eb7442e…`. Internal hostnames and IPv6 redacted. |
| `multi-tool-turn` | Several `tool_use` blocks in one assistant record | Real session `ae5465f4…` (Read, Read, Bash, then three `tool_result` records). Today's connector `TruncateAfter`s sibling results that all parent the assistant, so `expected.json` currently keeps one tool. The source retains all three. |
| `failed-tool` | `tool_result` with `is_error` followed by a successful assistant answer | Real Latch session `0e920c11…`, `Exit code 1`. The connector cannot represent a failed tool yet (`status: succeeded`, `summary: "completed"`); the source keeps `is_error` for the later connector mission. |
| `permission-request` | Claude `PermissionRequest` hook | Live Claude Code 2.1.228 capture already retained as `fixtures/harness/claude-code/live-2.1.228` and `live-permission-2.1.228` (2026-08-13). Stored inline because the connector also accepts hook records in the transcript stream. |
| `ask-user-question` | `AskUserQuestion` with two questions, option descriptions, and `multiSelect` | Real session `08489850…`. The connector flattens this to `prompt` (newline-joined) plus choice labels. |
| `branch-truncation` | A record whose `parentUuid` is an earlier assistant, not the previous record | Real Latch session `0e920c11…`. Emits `TruncateAfter`. |
| `interruption` | Rejected tool use plus `[Request interrupted by user for tool use]` | Real Latch session `0e920c11…`, then the assistant's status summary. |
| `long-transcript` | Scroll-measurement corpus | Same Latch session, full user+assistant chain, parents linearized onto one branch. `expected.json` is the ConversationStore published window: at most 300 items / 512 KiB, with `hasMoreBefore: true`. Do not assume 1,000 rows. |

## Presentation fixtures

These are Hub/client scenarios that Claude's transcript does not record. They
reuse the `markdown-prose` snapshot as the surrounding conversation.

| File | Covers |
| --- | --- |
| `presentation/reconnect-optimistic.json` | Reconnect while a send is still `sending`: the submitted user row stays until a matching observed user item arrives. |
| `presentation/operations-refused-ambiguous.json` | `operation_result` `refused` and `ambiguous` remain distinct, keep their text, and are not retried automatically. |

## How they were captured

Slices were taken from Claude Code JSONL under `~/.claude/projects` on 2026-09-21
(permission records: the 2026-08-13 live harness capture). Records keep Claude's
`type` / `uuid` / `parentUuid` / `message.content` vocabulary. Unused fields
(usage, model diagnostics, thinking text, attachments) were dropped. Absolute
paths, home directories, emails, IPs, env assignments, and token-shaped values
were sanitized before check-in.

## Tests

- Rust: `crates/latch/src/conversation/connectors/jsonl.rs` polls each
  `source.jsonl` and compares the projection to `expected.json`. Coverage
  assertions pin Markdown, fences, parallel tools in source, `is_error`,
  permission, AskUserQuestion flattening, `TruncateAfter`, the interrupt
  marker, and the 300-item window.
- Swift: `apps/LatchMobile/Tests/LatchMobileKitTests/ConversationFixtureTests.swift`
  loads the same `expected.json` snapshots into `ConversationStore`, so decode,
  publish order, pending-request selection, reconnect optimism, refused vs
  ambiguous operations, and the published window fail a mobile test when they
  regress. `ConversationProjection` presents every `ConversationItemKind` the
  store publishes; the test switch is exhaustive on that enum.
- Swift presentation: `ConversationProjectionTests` projects every case into
  turns and activity groups (each item exactly once, tool runs collapsed,
  one pending request); `ConversationMarkdownTests` renders every captured
  assistant message and asserts no word is lost, HTML stays literal, image
  URLs are dropped, and only web and mail links survive.

```bash
cargo test --package latch --lib conversation::connectors::jsonl
swift test --package-path apps/LatchMobile --filter 'ConversationFixtureTests|ConversationProjectionTests|ConversationMarkdownTests'
```
