# Latch Remote SDK — plan

**Status:** steps 1–3 shipped; step 4 outstanding. Builds on
[`ENGINE_PLAN.md`](./ENGINE_PLAN.md), whose Phases 0, 1, and 3 are all landed.
Where this document and [`../packages/README.md`](../packages/README.md)
disagree, this document wins on architecture; the README is the current
working reference for the packages themselves.

## The feature

The Remote SDK lets a developer build software — a web app, a mobile app, a
service — that connects to a Latch session running on someone else's machine
and gives its users three things:

1. **The terminal.** The full live session, rendered remotely, with input.
   Byte-faithful: what iTerm would show, a browser shows.
2. **The chat view.** The same session presented as a conversation — turns,
   tool activity, streaming assistant text — derived from the harness's own
   transcript via Latch's connectors, not from scraping the screen.
3. **Interaction.** Sending a user message into the running agent, answering a
   permission prompt, resolving a question — capability-gated, bound to
   request IDs, exactly as the engine plan's Phase 3 defines.

Put differently: the engine plan defines *what Latch can say and do* about a
session (`events`, `send`, `capabilities`). The SDK is *how software that is
not on the same machine reaches those verbs*, plus the client-side building
blocks that make a good UI cheap to build.

| Capability | Today | With the SDK |
| --- | --- | --- |
| Attach from another machine | SSH + `latch attach` | WebSocket from any HTTP client |
| Terminal in a web/mobile UI | No | `<LatchTerminal>` component |
| Chat view of a live agent | No | Transcript store + headless components |
| Answer a permission remotely | No | `resolve(requestId, choice)`, gated by `canSend` |

## What exists and what is missing

The engine half is done. `latch events --json [--from N]` streams normalized
`HarnessEvent` records with a `connectorEpoch` stamp, `latch send` applies
capability-gated input bound to a `requestId`, and `latch capabilities
<session>` reports per-session interaction capabilities — Phases 1 and 3 of
the engine plan, both landed. The Claude Code connector is verified against
`fixtures/harness/`.

The gateway and the client now cover the same verbs:

1. **`latch serve` covers sessions, capabilities, the terminal, events, and send.**
   A capability or screen-state refusal is HTTP 409 `{ error: "refused",
   reason }`, not a 500.
2. **`@latch/client` matches that surface** — list, inspect, gateway and
   per-session capabilities, `attachTerminal`, `subscribeEvents`, `canSend`,
   and `send()`.
3. **`@latch/chat-react` has the transcript store, `<AwaitingInputPrompt>`, and
   `<Composer>`.** Composer disables with a reason when `canSend.ok` is false;
   the prompt widget is bound to the `requestId` it was rendered for.

Step 4 is third-party hardening (versioned guarantees, example app, publishing).

The SDK is three layers, each consuming only the layer below:

```text
 web app            mobile app
   @latch/terminal-react   @latch/chat-react     UI building blocks
              @latch/client                      transport, reconnect, types
                   │  wss / https  (token auth)
              latch serve                        the gateway, on the session host
                   │  child processes
              engine API: events · send · capabilities · attach
                   │
              private tmux kernel
```

## Layer 1 — the gateway: `latch serve`

A new subcommand, not a new binary and not a resident daemon. The user (or
Overlord, or a launchd job the user opts into) starts it deliberately; it is a
long-lived child process exactly like `latch events` is — D2 survives.

```bash
latch serve [--bind 127.0.0.1:4610] [--token-file <path>] [--allow-remote]
latch serve token   # mint / rotate the bearer token
```

Surface, versioned under `/v1`:

| Endpoint | Maps to | State |
| --- | --- | --- |
| `GET /v1/capabilities` | `latch capabilities --json` plus the gateway endpoint set | built |
| `GET /v1/sessions` · `GET /v1/sessions/:id` | `list --json` / `inspect --json` | built |
| `GET /v1/sessions/:id/capabilities` | `capabilities <session> --json`, plus `events` (connector presence) | built |
| `WS /v1/sessions/:id/terminal` | a PTY running `latch attach :id` — binary frames both ways, resize as a control frame | built |
| `WS /v1/sessions/:id/events?cursor=N` | `latch events :id --json --from N`, one HarnessEvent per message, backfill from cursor | built |
| `POST /v1/sessions/:id/send` | `latch send` — `{message}` \| `{keys}` \| `{resolve: {requestId, choice}}` | built |

Decisions worth recording:

- **The terminal channel wraps `latch attach` under a PTY, per client.** It is
  the exact code path a human uses, so fidelity is free, and multi-client
  semantics are tmux's `window-size latest` — already decided. A tmux
  control-mode (`-C`) integration would be cleverer and is not needed for v1.
- **The events channel is a cursor, not a firehose.** The transcript is
  append-only, so a client that reconnects resumes from its last event index
  and misses nothing. This is what makes mobile — where the socket dies every
  time the phone locks — workable without a daemon buffering anything.
- **Auth is a bearer token; transport security is deployment.** `latch serve`
  binds loopback by default and requires the token on every connection.
  Reaching it from outside is a tunnel, Tailscale, or a reverse proxy — v1
  does not ship TLS or a hosted relay. The client takes a URL and a token, so
  a relay can be added later without touching the SDK. A non-loopback bind is
  refused unless `--allow-remote` is passed, because the token would otherwise
  cross the network in the clear.
- **Browsers present the token as a subprotocol.** A browser cannot set an
  `Authorization` header on a WebSocket handshake, so the client offers
  `latch.v1.<token>` as `Sec-WebSocket-Protocol` and the gateway accepts it
  there or in the header. A query-string token was rejected: it lands in logs
  and history. WebSocket handshakes are exempt from CORS, so a loopback bind
  additionally rejects non-loopback `Origin` values — otherwise any page the
  user visits could probe `ws://127.0.0.1:4610`.
- **Close codes carry the reason a retry will not help.** The terminal socket
  closes with 4404 for a session that does not exist; the client treats that
  and 1008 as final rather than reconnecting into an answer that will not
  change. The events socket adds 4408 (no harness connector) and 1000 (the
  stream ended because the session did). 4422 means the cursor is invalid for
  this connector epoch — the client drops the stored cursor and replays from
  0, once. 1013 means the transcript is not on disk yet and is worth retrying.
- **A chat tab is offered only when `events.ok` is true.** Per-session
  capabilities carry an additive `events` object: `{ok, harness,
  connectorEpoch}` when a connector is attached, or `{ok: false, reason}`
  when it is not. Connecting the events socket anyway closes 4408. That is
  the connector-presence signal; it is not inferred from a failed WebSocket.
- **A refusal is 409 with a reason.** `canSend` is screen-derived and racy, so
  it is UX-only: Composer disables with that reason, but `send()` always
  POSTs. The endpoint is the authority. A capability or screen-state refusal
  is `{ error: "refused", reason }` at 409, not a 500. Malformed bodies are
  400. Operation flags (`sendMessage` / `sendKeys` / `resolve`) still decide
  which widgets to offer so a harness that cannot send is not faked; `--keys`
  remains available when that is the advertised path.
- **Cursor invalidation is the client's job, with a gateway backstop.** Every
  event already carries `connectorEpoch`. The client stores `(cursor, epoch)`
  together. If the first event of a resumed socket has a different epoch, or
  the gateway closes 4422, the store resets and the socket reconnects from
  cursor 0. The gateway does not take an epoch query parameter; it uses the
  engine's `--from` and translates the engine's "beyond / restart from 0"
  failures into 4422.

## Layer 2 — the client: `@latch/client`

Pure TypeScript, no DOM, no React — usable from a browser, React Native, or
Node. It owns everything that is annoying to write twice:

- session list/inspect, typed against the CLI's stable `--json` shapes
- `GET /v1/capabilities` for the gateway endpoint set
- `attachTerminal({sessionId})` → duplex byte stream + `resize()`
- `subscribeEvents({sessionId, cursor})` → async iterator of `HarnessEvent`,
  with automatic reconnect-and-resume and a resync callback on epoch/cursor
  invalidation
- `send({sessionId, ...})` — always POSTs; 409 `LatchSendError` is the authority
- `canSend({sessionId})` — UX preflight for Composer; racy, not a substitute for POST
- one reconnect/backoff policy shared by both sockets

Types come from **`@latch/harness-schema`**: TypeScript generated from
`fixtures/harness/harness-event.v1.json` and
`interaction-capabilities.v1.json` — the engine plan's "one schema, two
languages" decision, applied. (This supersedes the old `packages/README.md`
rule against shared generators; that rule was about the deleted framing
protocol, where independent implementations kept each other honest. A JSON
schema consumed as JSON has no framing to get wrong.)

## Layer 3 — the UI building blocks

Two packages, both **headless-first**: logic and state as hooks/stores,
unstyled components on top, so embedders keep their own design systems.

**`@latch/terminal-react`** — xterm.js behind a Latch renderer API, as the
packages README already planned. `<LatchTerminal client={c} sessionId={id}>`
wires bytes, resize, focus, and reconnect. The renderer contract is duplex:
`write` paints session bytes, `onInput` returns keystrokes and pastes. A
renderer with only `write` is a viewer, and the whole point of wrapping
`latch attach` is that the far end is waiting on stdin. xterm.js stays private
so it can be swapped without breaking embedders.

**`@latch/chat-react`** — the claude-code-like chat kit:

- **Transcript store — shipped.** A reducer folding the HarnessEvent stream into a
  renderable timeline: `assistant_delta` accumulates into the open turn,
  `tool_started`/`tool_finished` pair into collapsible tool cards,
  `awaiting_input` opens a pending-request entry, `status` drives presence.
  Pure function of the event sequence — tested against the same
  fixtures the Rust connector is verified with.
- **`useTranscript(sessionId)` — shipped.** Store + subscription + cursor
  persistence. A live subscription reconnects from its in-memory cursor; a
  remount or a page reload restores the last snapshot — timeline, cursor, and
  the epoch that minted it — and resumes from there, so a phone that locked
  mid-turn does not re-derive the whole transcript. Persistence is pluggable
  (`localStorage` when the host has it, `null` for memory-only); a snapshot
  whose epoch is unknown, whose envelope version differs, or which fails to
  parse is dropped rather than migrated, because replaying from 0 is cheap and
  a migration bug is not.
- **The store retires a prompt when the stream moves past it — shipped.** There
  is no "resolved" event to pair with `awaiting_input`; the connector emits
  the request when the harness blocks. Any later event means the harness moved
  on, so the store marks that entry `resolved` and clears
  `pendingRequestId` — which is what keeps a widget from staying live against
  a prompt that has already been answered.
- **`<AwaitingInputPrompt>` — shipped.** Renders a permission or question with its
  choices and answers via `resolve(requestId, choice)`. The binding to
  `requestId` is the engine plan's own invariant: a widget cannot answer a
  different or later prompt.
- **`<Composer>` — shipped.** Free-text entry that submits through
  `send --message`, disabled with a reason whenever `canSend.ok` is false
  (mid-turn, open menu, half-typed composer — the screen-derived check).
- **Escape hatch.** Every chat surface renders next to, or can flip to, the
  terminal component. The terminal remains the universal fallback; the chat
  view never has to express everything.

Mobile: the client and the transcript store are pure TS and work in React
Native as-is. Chat-first is the mobile v1; a native terminal view is later
(RN xterm is poor — likely a webview embedding `terminal-react`).

## Where the maintenance burden lives

The mission brief is right that the transcript parser is an ongoing cost. The
architecture already puts that cost in the right place: **parsing lives in the
Rust connectors, behind the schema.** When Claude Code changes its transcript
format, the fix is a fixture and a connector patch and a `latch` release — the
SDK, and every app built on it, sees the same `HarnessEvent` v1 and does not
change. `harnessVersion` is stamped on every event, so a client can detect
skew and degrade to the terminal view instead of rendering garbage.

The SDK's own recurring costs are smaller and bounded: the transcript-store
reducer when the *schema* gains event types (versioned, additive), and the
widget set as harnesses grow new prompt kinds — both covered by the shared
fixture corpus.

## Build sequence

Ordered so every step ships something usable, and aligned with the engine
plan's phases rather than ahead of them:

1. **Remote terminal — shipped.** `latch serve` (sessions + capabilities +
   terminal WS + token auth), `@latch/harness-schema`, the `@latch/client`
   terminal path, `@latch/terminal-react`. This is the *reach for your agent
   from your phone* question answered with the smallest possible build.
2. **Read-only chat — shipped.** Events WebSocket over `latch events --from N`,
   `subscribeEvents`, `@latch/chat-react` transcript store, per-session
   `events` presence, `GET /v1/capabilities`.
3. **Interaction — shipped.** The engine prerequisite (`latch send`, per-session
   interaction capabilities) was already landed: this is the send endpoint,
   `send()`, `<AwaitingInputPrompt>`, and `<Composer>`. Gated on the capability
   schema so shipping it early for one harness does not fake it for another.
   `canSend` is UX-only — it is derived from the screen and therefore racy;
   `send()` always POSTs, and a refusal is a 409 with a reason, not a 500.
4. **Hardening for third parties — shipped.** `/v1` now has an additive
   compatibility rule and a terminal-only fallback for pre-discovery gateways;
   `examples/remote-sdk-react` composes the public package exports end to end.
   Packages build ESM and declarations into `dist/`, but remain private until
   Latch chooses a redistribution license and claims its npm scope. A hosted
   relay remains an explicit later product decision, not a side effect of
   publishing the client libraries.

Overlord is the first consumer of steps 2–3 (its embedded view and permission
relays, per Phase 2 of the engine plan), which keeps the SDK honest before it
is anyone else's dependency.

## Open decisions

1. **Does `latch serve` ship inside the `latch` binary?** Settled at step 1:
   yes. An HTTP/WS server in Rust added little to the payload and kept the
   one-payload update story. Revisit if the server grows product features of
   its own.
2. **Screen-sync transport for bad networks.** Raw PTY bytes over WS is v1.
   If mobile use demands mosh-style resilience, `latch-term` was archived, not
   deleted (`archive/latch-term-v1`) — it is most of that server. Do not build
   it speculatively.
3. **Package naming and publishing.** Settled at step 4: packages version
   together as `@latch/*` from `0.1.0`, emit ESM plus declarations under
   `dist/`, and retain the repository's `UNLICENSED` status. They stay private
   until Latch selects a redistribution license and configures the npm scope;
   source TypeScript is no longer a publishable entry point.
4. **How a client discovers what the gateway can do.** Settled at step 4:
   `GET /v1/capabilities` returns the engine discovery document plus
   `endpoints: {sessions, sessionCapabilities, terminal, events, send}`.
   `send` is true as of step 3. `/v1` is additive within protocol major 1: a
   newer client enables an optional route only when its endpoint flag is true;
   a 404 from discovery means the older, terminal-only surface. A different
   protocol major is unsupported rather than guessed at.
5. **Viewer versus controller on the terminal channel.** Settled for v1:
   every terminal client is a full controller, and tmux's `window-size latest`
   means a phone in portrait can reflow the desktop terminal. `latch resize
   --pinned` is the escape hatch that exists today. A read-only, ignore-size
   viewer mode would be a new explicit protocol request, never inferred from
   client dimensions.
6. **Scrollback on connect is out of scope for v1.** Settled: a fresh terminal socket
   shows what tmux redraws, which is the visible screen; history stays on the
   server. The chat view is the real answer to *what has it been doing*.
   `capture-pane -S` is a possible later backfill.
7. **Token rotation.** Settled for v1: `latch serve token` changes the token
   required by new handshakes without terminating authenticated terminal or
   events sockets. Reconnecting clients must use the rotated token.
