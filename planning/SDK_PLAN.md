# Latch Remote SDK — plan

**Status:** proposed. Builds on [`ENGINE_PLAN.md`](./ENGINE_PLAN.md); assumes
Phase 0 (the tmux kernel) is landed, which it is. Where this document and
[`../packages/README.md`](../packages/README.md) disagree, this document wins —
that README predates the engine plan and still describes the deleted wire
protocol.

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

Three gaps stand between the current tree and the feature:

1. **The engine API is planned, not built.** `latch events` (NDJSON
   HarnessEvent stream) and `latch send` are Phase 1 and Phase 3 of the engine
   plan. `latch capabilities` exists but does not yet report connectors or
   interaction capabilities per session.
2. **There is no network surface.** Every contract is a local CLI invocation.
   The engine plan anticipated this: *"a local socket or HTTP surface can come
   later without changing the event model."* Later is now.
3. **There is no client code.** `packages/` is deliberately empty.

The SDK is therefore three layers, each consuming only the layer below:

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
latch serve [--bind 127.0.0.1:4610] [--token-file <path>]
latch serve token   # mint / rotate the bearer token
```

Surface, versioned under `/v1`:

| Endpoint | Maps to |
| --- | --- |
| `GET /v1/sessions` · `GET /v1/sessions/:id` | `list --json` / `inspect --json` |
| `GET /v1/sessions/:id/capabilities` | `capabilities --json`, per session |
| `WS /v1/sessions/:id/terminal` | a PTY running `latch attach :id` — binary frames both ways, resize as a control frame |
| `WS /v1/sessions/:id/events?cursor=N` | `latch events :id --json`, one HarnessEvent per message, backfill from cursor |
| `POST /v1/sessions/:id/send` | `latch send` — `{message}` \| `{keys}` \| `{resolve: {requestId, choice}}` |

Three decisions worth recording:

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
  a relay can be added later without touching the SDK.

## Layer 2 — the client: `@latch/client`

Pure TypeScript, no DOM, no React — usable from a browser, React Native, or
Node. It owns everything that is annoying to write twice:

- session list/inspect, typed against the CLI's stable `--json` shapes
- `attachTerminal({sessionId})` → duplex byte stream + `resize()`
- `subscribeEvents({sessionId, cursor})` → async iterator of `HarnessEvent`,
  with automatic reconnect-and-resume
- `send({sessionId, ...})` with `canSend` preflight
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
wires bytes, resize, focus, and reconnect. xterm.js stays private so it can be
swapped without breaking embedders.

**`@latch/chat-react`** — the claude-code-like chat kit:

- **Transcript store.** A reducer folding the HarnessEvent stream into a
  renderable timeline: `assistant_delta` accumulates into the open turn,
  `tool_started`/`tool_finished` pair into collapsible tool cards,
  `awaiting_input` opens a pending-request entry, `status` drives presence.
  Pure function of the event sequence — trivially testable against the same
  fixtures the Rust connector is verified with.
- **`useTranscript(sessionId)`** — store + subscription + cursor persistence.
- **`<AwaitingInputPrompt>`** — renders a permission or question with its
  choices and answers via `resolve(requestId, choice)`. The binding to
  `requestId` is the engine plan's own invariant: a widget cannot answer a
  different or later prompt.
- **`<Composer>`** — free-text entry that submits through
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

1. **Remote terminal.** `latch serve` (sessions + terminal WS + token auth),
   `@latch/harness-schema` scaffold, `@latch/client` terminal path,
   `@latch/terminal-react`. Depends only on Phase 0, which is done. This is
   the *reach for your agent from your phone* question answered with the
   smallest possible build.
2. **Read-only chat.** Engine Phase 1 (`latch events`, the Claude Code
   connector, the schema in `fixtures/harness/`), then the events endpoint,
   the client subscription, and the transcript store. Runs in parallel with
   step 1 up to the endpoint, exactly as Phases 0 and 1 were parallel.
3. **Interaction.** Engine Phase 3 (`latch send`, per-session interaction
   capabilities), then the send endpoint, `<AwaitingInputPrompt>`, and
   `<Composer>`. Gated on the capability schema so shipping it early for one
   harness does not fake it for another.
4. **Hardening for third parties.** Versioned API guarantees, example app,
   published packages, and — if demand shows up — the hosted relay for
   NAT-traversal without a tunnel.

Overlord is the first consumer of steps 2–3 (its embedded view and permission
relays, per Phase 2 of the engine plan), which keeps the SDK honest before it
is anyone else's dependency.

## Open decisions

1. **Does `latch serve` ship inside the `latch` binary?** Proposed: yes — an
   HTTP/WS server in Rust adds little to the payload and keeps the one-payload
   update story. Revisit if the server grows product features of its own.
2. **Screen-sync transport for bad networks.** Raw PTY bytes over WS is v1.
   If mobile use demands mosh-style resilience, `latch-term` was archived, not
   deleted (`archive/latch-term-v1`) — it is most of that server. Do not build
   it speculatively.
3. **Package naming and publishing.** `@latch/*` scope assumed; registry,
   versioning policy, and license decided at step 4, not before.
