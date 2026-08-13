# CLI / Tmux Engine Cleanup — Unbreak CI and Remove Dead Pre-tmux Code

**Date:** 2026-08-13
**Mission:** coo:715 — objective 2 (Unbreak CI and remove dead pre-tmux code and doc rot)
**Scope:** Findings A2, D1–D5, D7 (partial), D8, E1–E3, B7 from
`ai/history/2026-08-13-cli-tmux-engine-review.md`.

## What landed

1. **A2 — CI lint gate.** `openpty` now takes `&size` (`unnecessary_mut_passed`).
   Consecutive `str::replace` calls in `display_session_row` collapsed to
   `line.replace([SESSION_ROW_SEPARATOR, '\t'], ",")`.
2. **D1 / D3.** Deleted `ConnectionState`, `AttachOutcome` (computed then
   discarded by every caller), `RetryPolicy::NONE`, and `RetryPolicy::budget()`.
   `attach` / `attach_with_retry` now return `Result<()>`.
3. **D2.** Removed `--watch` and `--steal`. Mapping `--watch` to
   `tmux attach-session -r` was the preferred option, but a read-only tmux
   client still participates in `window-size latest` and would snap the shared
   session — the opposite of the documented "peek without resizing" contract.
   Nothing consumed either flag, so both were dropped. Callers that still pass
   them now get clap's unknown-argument error instead of a silent no-op.
4. **D4.** Removed unreachable `NestingDecision::Decline` and the always-true
   `wants_create` parameter. Call sites now match on `Allow` vs
   `AttachToEnclosing` only.
5. **D5.** Deleted unused `cli/json.rs::parse_state`.
6. **D7 (partial).** `tests/tmux_kernel.rs` `FAKE_TMUX` now joins fields with
   `\x1f` so the fixture exercises the real tmux format. The tab fallback in
   `split_session_row` / `display_session_row` was **kept**: a later objective
   in this mission covers Finder/launchd-launched parents whose `LC_CTYPE` is
   not UTF-8, where tmux sanitizes U+001F to `_`. Deleting the fallback now
   would widen that failure surface. Unit tests still parse tab-separated rows.
7. **D8.** Deleted empty untracked scaffold dirs: `crates/latch-protocol/`,
   `crates/latch-term/`, `crates/latch/src/worker/`, `crates/latch/tests/support/`.
   History remains at tag `archive/latch-term-v1`.
8. **E1–E3, B7.** Module docs no longer describe a `worker` subcommand or
   `todo!` stubs. Worker-era phrasing in `json.rs` / `create.rs` now refers to
   tmux. `harness-interaction.lock` is documented as send-only (capabilities
   queries do not take it). The "size supplied to the worker" phrase lived in
   `cli/create.rs`, not `session/manifest.rs`.

## Verification

- `cargo test --workspace` — 47/47 passed (43 unit + 4 integration).
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.
- `./scripts/check-boundaries.sh` — `boundaries ok`.

## Tradeoffs

- **`--watch` / `--steal` removed rather than `--watch` → `-r`.** Read-only
  attach would still resize under `window-size latest`. Silent no-ops were
  worse than dropping unused flags.
- **Tab fallback retained.** Coordinates with the later locale-hardening
  objective. `FAKE_TMUX` now uses the real separator, so the primary parse
  path is what integration tests cover.
