# CLI / Tmux Engine Input-Safety Hardening

**Date:** 2026-08-13
**Mission:** coo:715 — objective 3 (Input-safety hardening for latch send)
**Scope:** Findings A1 and A10 from `ai/history/2026-08-13-cli-tmux-engine-review.md`.

## What landed

1. **A1 — `latch send --message` no longer executes text in a plain shell.**
   `classify_screen` still treats a `❯` plus whitespace line as an empty Claude
   composer, but `sendMessage` and `resolve` now also require harness evidence:
   a `harness` marker in `meta.json` (written at create when argv basename is
   `claude`), `command_label == "claude"` for sessions created before the
   marker, or the `harness-hooks.jsonl` sidecar. Unknown sessions with a
   composer-glyph prompt report `sendMessage=false`, `resolve=false`,
   `sendKeys=true`, and `canSend.ok=false` with a reason that points at
   `--keys`. `--keys` is gated only by `sendKeys`, so it remains the explicit
   caller-owns-the-risk path.

2. **Harness marker at launch.** `meta::derive` persists `harness: "claude"`
   when launch argv is Claude Code. `prepare_claude_launch` uses the same
   `harness_kind` helper. The field is optional and omitted for ordinary
   shells, so existing `meta.json` documents keep deserializing.

3. **A10 — paste-then-Enter failure is recoverable.** If `paste-buffer`
   succeeds and the follow-up Enter `send-keys` fails, the error says the text
   was pasted but not submitted and tells the caller to recover with
   `latch send --keys C-u`.

4. **Capabilities wording.** README and the human-readable `latch capabilities`
   output now state that `--message`/`--resolve` require a known harness, and
   still print the per-operation flags when `canSend.ok` is false so `--keys`
   remains visible.

## Tests

- Shell session whose screen is an empty `❯` prompt: `--message` refused;
  `--keys` still accepted.
- Same session after writing `harness: "claude"` into `meta.json`: `--message`
  accepted; permission `--resolve` still works.
- Claude-marked session whose Enter send is forced to fail: error mentions
  pasted-but-unsubmitted and `C-u`; the paste buffer is present and no Enter
  key was recorded.
- Unit coverage for `harness_kind`, marker/hooks/`command_label` evidence.

Verified: `cargo test --workspace` (51 tests: 46 unit + 5 integration),
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all -- --check`.
