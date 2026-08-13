# CLI / Tmux Engine Serve Polish

**Date:** 2026-08-13
**Mission:** coo:715 — objective 6 (serve gateway polish)
**Scope:** Findings A3, D6, B4, and B5 from
`ai/history/2026-08-13-cli-tmux-engine-review.md`.

## What landed

1. **A3 — terminal attach no longer snaps every client to 80×24.** The
   WebSocket URL accepts `cols`/`rows` query parameters and opens the PTY at
   that size. Without them, the gateway waits for the first `resize` control
   frame before spawning `latch attach`. `@latch/client` puts the last known
   size on the URL; the React terminal passes the xterm size at connect time.

2. **D6 — inspect reports an honest attached count.** The fabricated
   `attachments` array (`client-N` / `shared` / `tmux` / `attached terminal`)
   is gone. `InspectReport.attached` is the tmux client count when the private
   server has the session. LatchDesktop and `@latch/client` consume the new
   field.

3. **B4 — serve errors are classified by type, not substring.**
   `SessionLookupError` from `resolve_session` / `resolve_existing` maps to
   generic HTTP bodies: 404 `session not found`, 409 `session name is
   ambiguous`, otherwise 500 `internal error`. CLI wording is unchanged.

4. **B5 — non-loopback binds are opt-in, and WebSocket closes carry a
   reason.** `--help` documents SSH-tunnel-to-loopback as the supported remote
   path. Binding anything other than loopback refuses unless `--allow-remote`
   is passed (and then warns). Missing-session terminal sockets close with
   code 4404 and reason `session not found`; attach failures close 1011.
   Auth failures remain HTTP 401 before upgrade.

## Tests

- Inspect JSON has `attached` and no `attachments` array.
- HTTP inspect of an unknown session is 404 with the generic error body.
- A terminal socket without `cols`/`rows` does not spawn attach until a
  resize frame; query parameters spawn immediately.
- A terminal socket for a missing session closes 4404 `session not found`.
- `latch serve --bind 0.0.0.0:0` exits non-zero without minting a token.

Verified: `cargo test --workspace` (73 passing), `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo fmt --all -- --check`.
