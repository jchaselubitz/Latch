# Remote SDK: revised objectives applied, gaps filled

**Date:** 2026-08-14
**Mission:** coo:708 — objective 7 (apply revised objectives 5–7, then close
the gaps in step 2)
**Scope:** `@latch/chat-react` cursor persistence and prompt retirement,
`@latch/client` error mapping, and the first end-to-end test of the client
against a real `latch serve`.

## What landed

Steps 1–3 were already shipped and green. This session applied the revised
objectives from the mission artifact and closed what step 2 had left open.

1. **Cursor persistence in `useTranscript`.** It subscribed from cursor 0 on
   every mount and stored nothing, so a reload re-derived the whole
   transcript. It now restores a snapshot — timeline, cursor, and the
   connector epoch that minted the cursor — through a pluggable
   `TranscriptStorage`, defaulting to `localStorage` where the host has one
   and `null` (memory only) where it does not. Snapshots are written at most
   every 250ms while events stream and once more at teardown.
2. **Snapshots are dropped, never migrated.** A snapshot with a different
   envelope version, a cursor greater than zero but no epoch, or a body that
   fails validation is discarded and the socket starts at 0. The transcript is
   a pure function of the event stream, so replaying is always cheaper than a
   migration bug. Storage that throws — private mode, quota — never reaches
   the stream.
3. **The store retires a prompt when the stream moves past it.** There is no
   "resolved" event to pair with `awaiting_input`; the connector emits the
   request when the harness blocks. Any later event now marks that entry
   `resolved` and clears `pendingRequestId`, which is what keeps
   `<AwaitingInputPrompt>` from staying live against an answered prompt.
4. **A 409 is not always a refusal.** `@latch/client` mapped every 409 to
   `LatchSendError{refused: true}`, including "session name is ambiguous" —
   a lookup failure a Composer would have shown as the harness declining.
   Only `{ error: "refused" }` is a refusal now.
5. **The end-to-end test that did not exist.** `client/src/gateway.test.ts`
   builds a temp `LATCH_HOME` on the fake tmux, starts the real `latch serve`,
   and drives it through `@latch/client` over Node's WebSocket: sessions and
   capabilities, events resumed from a cursor plus a 4422 re-sync, a 409
   refusal followed by a permitted send, and terminal bytes in both
   directions. Every other test in the package fakes the socket.
6. **One fake tmux, two suites.** The Python fake moved from a string constant
   in `tmux_kernel.rs` to `fixtures/testing/fake-tmux.py`, which the Rust test
   now `include_str!`s and the TypeScript test copies.

Revised objective 7 (step 4: hardening and publishing) was added to the
mission. Revised 5 and 6 describe work that is already complete, so they stay
in shared context — a completed objective's prompt cannot be rewritten.

## Decisions

- **Persistence is opt-out, not opt-in.** `localStorage` is the default
  because the mobile case — the socket dies when the phone locks — is the one
  the events cursor exists for. `defaultTranscriptStorage()` returns `null`
  rather than a memory store where there is no `localStorage`: a memory store
  would look like persistence and lose everything on reload.
- **The store's epoch check stays even though the client resyncs.** The client
  drops its cursor when the epoch shifts, so the reducer's own reset is
  belt-and-braces — but `foldEvents` is also used directly on fixture streams,
  where nothing else would notice a spliced epoch.
- **The prompt-retirement rule is "any later event", including `status`.** The
  connector emits `awaiting_input` as the newest observation while the harness
  blocks; anything after it means the harness moved on. Screen-derived
  capabilities remain the authority for whether a resolve will be accepted.
