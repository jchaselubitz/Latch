# Remote SDK step 2: read-only chat

**Date:** 2026-08-14
**Mission:** coo:708 — objective 5 (read-only chat)
**Scope:** Events WebSocket, `@latch/client` `subscribeEvents`, `@latch/chat-react` transcript store.

## What landed

Engine Phase 1 was already done, so this is gateway and client work.

1. **`WS /v1/sessions/:id/events?cursor=N`** spawns `latch events :id --json --from N` and forwards each NDJSON line as one text frame. Missing sessions close 4404; sessions without a harness connector close 4408 without spawning; a cursor the engine rejects closes 4422; a missing transcript closes 1013 so the client can retry.
2. **Connector presence** is an additive `events` field on `GET /v1/sessions/:id/capabilities` (`ok` / `reason` / `harness` / `connectorEpoch`). A chat tab can be withheld before opening a socket.
3. **`GET /v1/capabilities`** returns the engine discovery document plus the gateway endpoint set (`events: true`, `send: false`).
4. **`@latch/client.subscribeEvents`** is an async iterator with the terminal socket's backoff, fatal-close handling, and a resync-from-zero path on epoch mismatch or 4422.
5. **`@latch/chat-react`** folds `HarnessEvent` into a timeline (deltas accumulate, tools pair, `awaiting_input` is pending, `status` is presence). `useTranscript` wires the store to the subscription. Tested against the same `fixtures/harness/` NDJSON the Rust connector uses.

Send / interaction widgets were not built.

## Decisions

- Cursor invalidation is client-side comparison of `connectorEpoch`, with 4422 as the gateway backstop. The events URL does not take an epoch query parameter.
- A session with no connector is a capabilities fact, not a failed WebSocket.
- A remount of `useTranscript` rebuilds from cursor 0; reconnect of a live subscription resumes from the in-memory cursor.
