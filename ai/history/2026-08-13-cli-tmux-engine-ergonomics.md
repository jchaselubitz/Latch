# CLI / Tmux Engine Ergonomics and Robustness

**Date:** 2026-08-13
**Mission:** coo:715 — objective 5 (CLI ergonomics and robustness fixes)
**Scope:** Findings A5, A7, A8, A9, B1, B2, B3, B6, and F2 from
`ai/history/2026-08-13-cli-tmux-engine-review.md`.

## What landed

1. **A5 — `latch stop` is detectable as failure.** When the pane is still
   running after SIGKILL, the command prints the report then exits non-zero.
   `--json` includes `stopped: false` so scripted callers do not have to
   re-inspect. A successful stop still exits 0 with `stopped: true`.

2. **A7 — `latch attach --retry` no longer retries permanent failures.**
   Inspect runs inside the retry loop. A still-running session, or a missing
   private server, is retried; a gone or exited session, or a missing tmux
   binary, fails immediately with the last error.

3. **A8 — `latch rename` refuses a display name already used by another
   session.** Renaming a session to its own name remains a no-op success.

4. **A9 — `latch shell --name` / `--title` inside an enclosing session
   refuses**, matching `run`/`create`, instead of silently attaching and
   dropping the requested identity. Bare `latch` / `latch shell` still attach
   to the enclosing session.

5. **B1 — tmux stderr markers** (`no server running`, `can't find session`,
   `no sessions`) live next to `TMUX_VERSION` and are classified by one helper
   used by list, inspect, and attach retry.

6. **B2 — `tmux()` goes through `tmux_binary()`**, so a missing bundled binary
   reports `run latch update to repair` instead of a raw ENOENT from
   `/nonexistent/latch-tmux`.

7. **B3 — bare `latch attach` picks the most recently active session** from
   `engine::list` activity, not the lexically largest id.

8. **B6 — missing `$HOME` / vanished cwd** in `shell`/`run` are propagated
   `Result`s, not `.expect()` panics.

9. **F2 — stop polls every 50 ms** (was 20 ms) while keeping the same ~5 s
   graceful and ~2 s SIGKILL windows, so tmux is spawned less often.

## Tests

- Successful stop returns `stopped: true`; a session whose pane tmux still
  reports live after SIGKILL exits non-zero with `stopped: false`.
- Rename collision is refused by id of the occupant; self-rename succeeds.
- Bare attach follows the higher `session_activity`, not creation-id order.
- `attach --retry` against a session that disappears fails in well under the
  retry budget.
- Named `latch shell` inside `LATCH_SESSION_ID` refuses; missing `HOME` is an
  error rather than a panic.

Verified: `cargo test --workspace` (65 passing), `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo fmt --all -- --check`.
