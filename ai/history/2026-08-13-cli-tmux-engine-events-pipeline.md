# CLI / Tmux Engine Events Pipeline Efficiency

**Date:** 2026-08-13
**Mission:** coo:715 — objective 4 (Events pipeline efficiency and transcript discovery correctness)
**Scope:** Findings C1, A6, and A4 from `ai/history/2026-08-13-cli-tmux-engine-review.md`.

## What landed

1. **C1 — `latch events` no longer re-parses the whole transcript and ledger every 100 ms.**
   `stream_ledger` stats the transcript, hooks sidecar, and ledger and skips
   reconcile when sizes and mtimes are unchanged. The ledger is read from a
   remembered byte offset, so only appended JSONL is parsed. The
   occurrence-count map that prevents duplicate appends lives in memory across
   polls instead of being rebuilt by re-serializing every existing event.
   `stream_transcript` uses the same stat-gating. The append-only
   cursor-stability contract is unchanged: a rewritten prefix still fails with
   the connector-epoch restart error.

2. **A6 — `latch events` and `latch send --resolve` share one permission-request
   derivation.** `harness/permission.rs` is the only place that computes
   request id, kind, prompt, and choices. The previously divergent fallback
   (`permission:<tool>:1970-01-01T00:00:00Z` vs `permission:<tool>:unknown`)
   is gone; both sides now use the events-side timestamp default, so a
   `--resolve` id advertised by `latch events` cannot miss the pending request
   that `send` sees.

3. **A4 — Claude project-directory encoding matches Claude Code.**
   `encode_project_path` now replaces every non-ASCII-alphanumeric character
   with `-` (`path.replace(/[^a-zA-Z0-9]/g, "-")`), so cwds containing `.` or
   `_` resolve. Confirmed against a real `~/.claude/projects` entry
   (`/Users/jake/Development/Cooperativ/Latch` →
   `-Users-jake-Development-Cooperativ-Latch`). If the encoded directory still
   misses, discovery scans project directories for `{sessionId}.jsonl`.

## Tests

- Unchanged transcript/hooks poll reports skip and leaves the ledger offset
  untouched.
- A 2,000-event generated transcript reconciles once, skips on the next poll,
  then appends a single tail event without duplicating the prefix.
- Encoding unit cases for dotted and underscored cwds, plus a scan fallback
  when the encoded directory name is wrong.
- A permission hook without `request_id`/`timestamp` produces the same id
  from `parse_transcript` and `permission::from_record`.
- Existing fixture and append-only ledger tests unchanged.

Verified: `cargo test --workspace` (57 tests: 52 unit + 5 integration),
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all -- --check`.
