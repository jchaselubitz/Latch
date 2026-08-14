# Remote SDK step 3: interaction

**Date:** 2026-08-14
**Mission:** coo:708 — objective 6 (interaction)
**Scope:** Send endpoint, `@latch/client` `send`/`canSend`, `@latch/chat-react` widgets.

## What landed

Engine Phase 3 was already done (`latch send`, per-session interaction
capabilities), so this is gateway, client, and widgets.

1. **`POST /v1/sessions/:id/send`** applies `{message}` | `{keys}` |
   `{resolve: {requestId, choice}}` through the same `latch send` library path
   the CLI uses. `GET /v1/capabilities` now reports `endpoints.send: true`.
2. **Refusals are structured.** A capability or screen-state refusal is HTTP
   409 `{ error: "refused", reason }`. Malformed bodies are 400. Missing
   sessions stay 404. The engine now raises `SendRefused` / `SendInvalid` so
   the gateway does not have to guess from stderr.
3. **`@latch/client.send`** always POSTs. A 409 becomes `LatchSendError` with
   the server reason — the endpoint is the authority. `canSend` is the
   session capabilities document Composer polls; it is UX-only and racy.
4. **`@latch/chat-react`** ships `<Composer>` (disabled with a reason when
   `canSend.ok` is false or `sendMessage` is not advertised; a 409 from send
   is still shown) and `<AwaitingInputPrompt>` (answers via
   `resolve(requestId, choice)`, bound to the requestId it was rendered for
   so it cannot answer a later prompt).

TLS, a hosted relay, and third-party publishing remain step 4.

## Decisions

- `canSend` is UX-only (Composer disable). `send()` always POSTs; a racy
  preflight must not swallow a request the server would accept. A 409 is
  surfaced, not dropped.
- A widget always submits the `requestId` it was given; `activeRequestId`
  only disables stale UI. The server 409s a stale resolve.
