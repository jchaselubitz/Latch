# Latch TypeScript integration

This is the repository-level TypeScript gateway guide. For the complete
provider boundary, launch manifest, and Overlord example, start with
[Integrations](INTEGRATIONS.md).

Latch protocol major 2 deliberately has a small TypeScript surface:

- `@latch/client` discovers sessions, opens a terminal WebSocket, and reads explicitly shared files over a paired connection.
- `@latch/terminal-react` renders a terminal handle through an embedder-supplied
  xterm renderer.

Both packages are private workspaces. The only currently supported conversation client is the
native mobile client, which speaks the canonical v2 conversation socket
directly. A headless TypeScript conversation client and optional React renderer
are proposed in the
[rich conversation implementation guide](../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md#phase-7--minimal-embeddable-web-sdk);
this document does not describe them as available before that phase ships.

## Protocol boundary

Use `GET /v2/capabilities` before opening a gateway connection. It reports
protocol major 2, the gateway instance ID, operation retention, and the
available `sessions`, `terminal`, `preview`, `conversation`,
`browseDirectories`, and `createSession` endpoints. A consumer must
require `protocolVersion: 2`; v1 has been removed and is not negotiated,
probed, or adapted.

`@latch/client` supports session listing, inspection, discovery, stopping, and
terminal attachment. Its `GatewayEndpoints` type is generated from the
canonical discovery schema, so the optional keys are exactly the routes a
gateway may predate. The terminal socket is
`WS /v2/sessions/{id}/terminal`; the token is carried by the `latch.v2.*`
subprotocol.

A terminal connection is the session's **single exclusive surface**, so it
always requires the `control` grant. There is no read-only or observing
terminal mode: opening this socket takes the session's terminal from whatever
was showing it — an iTerm window, or another device — and a later `latch
attach` takes it back. `cols` and `rows` are required before the steal
commits, either as query parameters or as a `resize` control frame; a socket
that never declares a size is closed without disturbing the current surface.

The first frame after the steal is a paint of the pane's current screen and
terminal modes. Everything after it is the agent's own byte stream, unchanged:
no scrollback, no PTY replay, and no re-encoding.

`GET /v2/sessions/{id}/preview` is the one terminal-shaped thing a client may
do without taking the surface, and it is available at the `observe` grant
because it takes nothing. It is a `capture-pane` query, not a second live
surface: one read of the pane's cells at one instant, returning

| Field | Meaning |
| --- | --- |
| `content` | the cells as escape-encoded text, drawable by the same renderer as the live stream, rows joined by newlines with none at the end |
| `cols`, `rows` | the pane's current grid |
| `alternateScreen` | true while a full-screen application owns the pane |
| `capturedAt` | when the read happened; a still is stale the moment it is taken |
| `scrollbackLines` | how many lines of history were included, after the cap |

`scrollbackLines` may be requested as a query parameter and is capped at 200.
It is forced to zero while `alternateScreen` is true, because the alternate
screen has no history to read. The route has its own short capture deadline, so
a client waits briefly or is told the read failed.

It does not update, follow the session, or accept input, and there is no
streaming variant of it. A client that wants the next frame opens the terminal
socket and takes the surface like everyone else. Discovery reports it as
`endpoints.preview`; a gateway that predates the route omits the key, which
decodes as absent rather than failing the discovery document.

Every reasoned close names why the surface ended, as both a close code and a
close reason:

| Code | Reason | Meaning |
| --- | --- | --- |
| 1000 | `detached` | The attach ended cleanly. |
| 4408 | `slow_client` | This peer stopped draining output and was evicted. The session kept running. |
| 4409 | `stolen` | Another terminal took the surface. |
| 4410 | `session_exited` | The session's program exited. |
| 4500 | `kernel_error` | The session kernel could not hand over a surface. |

None of these is retried automatically. Reconnecting after `stolen` would take
the surface back from whoever just claimed it, and two clients set to
reconnect would trade the session forever; reattaching is a decision for the
person at the keyboard. `@latch/client` reconnects only from transport-level
drops, which carry no reasoned code.

Conversation transport is
`WS /v2/sessions/{id}/conversation`. It is server-first: the client provides
generation, revision, and operation epoch as upgrade parameters, then receives
a snapshot or retained mutations. It supports history pages and correlated
`send_message` and `resolve_request` operations, with the Hub enforcing the
device grant for every action. Its canonical schema is
[`schemas/remote-access/v2/`](../schemas/remote-access/v2/), not a published
TypeScript package API.

`GET /v2/directories` and `POST /v2/sessions` are the folder browser and the
remote-creation route. Both require the `control` grant, and both are
advertised separately — `endpoints.browseDirectories` and
`endpoints.createSession` — so a gateway can describe exactly what it serves
and a client never probes an undiscovered route. A gateway that predates them
omits both keys, which decode as absent.

The directory route answers with a page of **folders only**: the canonical
`path` it resolved to, a nullable `parent`, `entries` of `{ name, path }`, and
a nullable `nextCursor`. Omitting `path` asks for the Mac user's home
directory. Symlinks are resolved before anything is returned, entries are
ordered case-insensitively, and a page holds at most 200 of them. The route
returns no file names, no file contents, no metadata, and no search. Every
path failure — missing, unreadable, not a directory, outside what the gateway
will serve — is one stable error, so the route cannot be used to probe the
filesystem.

## Shared Files

Gate the Files client on `endpoints.sharedFiles`. Every request requires the
authenticated paired device's current Control grant and an explicit host-owner
share. The [host CLI](CLI.md) creates or revokes shares. Latch stores shares on
the host, bound to the pairing's grant revision and to the selected directory
identity. The root ID is opaque and remains usable across gateway restarts
while those bindings hold. Clients associate the root ID with their own
project authorization and authenticated host identity.

| Route | Response |
| --- | --- |
| `GET /v2/files/roots` | Up to 128 available shares for the current pairing, each with a `rootId` and directory display name. |
| `GET /v2/files/directory?rootId=&path=&cursor=` | One page of up to 64 direct child names and their kinds. An omitted or empty path opens the root. |
| `GET /v2/files/preview?rootId=&path=` | `text` or `truncated` with UTF-8 text, or `binary` with no text. |

Paths are UTF-8, slash-separated and root-relative. Use returned names with
their original spelling. Absolute paths, traversal components, symlinks,
hard-linked files, special files, and entries on a different filesystem are
outside the Files scope. Directory pages omit hidden entries, Git metadata,
common credential and secret names, and private key or credential-file
extensions. Direct access applies the same checks to every relative component.
The owner cannot share Latch state or protected system directories, or select
a root that contains either. A share
should contain only material intended for the selected device.

Directory cursors are opaque and gateway-local. They expire after five minutes
and may be evicted sooner. Reload the first page on `409 stale_cursor`. A
directory with more than 8,192 entries returns `shared_directory_too_large`.
Previews read at most 64 KiB plus one byte. Truncation may end the returned
text early at a UTF-8 scalar boundary. Binary classification examines only
that prefix and treats unsupported encodings and control bytes other than tab,
newline, and carriage return as binary. The gateway verifies that the opened
objects remain at their authorized paths and rechecks the pairing and share
before returning data. Scope or filesystem changes can yield
`shared_root_unavailable`, `shared_entry_unavailable`, or `shared_entry_changed`.
The Linux gateway needs kernel support for `openat2` and a mounted `/proc`
to advertise Files.

Swift consumers start a `SharedFilesReadInteraction` via
`AppModel.beginSharedFilesRead()`. The interaction verifies the current
paired host, uses one bounded device-owner unlock, and discards responses
from an invalidated link or interaction. Keep visible content in memory,
clear it on root or project access loss, and call `invalidate()` when leaving
the scope. Refresh root availability when rechecking visibility since share
revocation is enforced on reads.

## Remote session creation

Creation takes two fields, and optionally an agent, its model and an orientation:

```json
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src" }
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src", "agent": "claude" }
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src", "agent": "codex" }
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src", "agent": "claude", "model": "claude-opus-5-5" }
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src", "agent": "codex", "instructions": "This chat was started from Overlord Mobile…" }
```

`requestId` is a canonical hyphenated UUID and is the idempotency key. The Mac
holds a creation lock across the lookup and the durable metadata write, so
repeating one request id returns the session it already created rather than a
second one; the same id against a different `cwd` or a different `agent` is
refused as `request_id_conflict`. Without `agent`, the gateway builds the same
interactive login shell `latch shell` does, at the standard initial geometry,
with no command, name, environment, or shell path supplied by the caller.

`agent` names a hosted agent kind, and only a kind the gateway lists in
`features.sessionAgents` — a client must read an absent list as shells only
and never send the field to such a gateway, whose contract closes the object.
The gateway resolves the agent's executable on the Mac (through the owner's
login shell, then its own PATH and the installer's known locations) and
declares it as `argv[0]` with structured agent identity, then starts it through
the owner's login shell so the session carries its harness marker and
conversation connector. A kind the gateway
does not list is `invalid_request`; a listed kind it cannot find on the Mac is
refused as `agent_unavailable` (422) before anything is accepted, with a
reason fit to show. Either way creation does not attach — the new session
appears in `GET /v2/sessions` with no surface taken from anyone. The response
is the existing `CreateReport`.

`model` is optional and only valid with `agent`. It must be an `id` the
gateway lists at `GET /v2/agents/{agent}/models` (control grant, advertised as
`endpoints.agentModels`), which answers an `AgentModelCatalog`:
`{ agent, models: [{ id, name, description? }], defaultModel }`. The list is
read from the agent's own model cache on the Mac on every request, falling
back to a list bundled with Latch. The agent is started with `--model <id>`;
omitting `model` leaves it on the owner's configured default (`defaultModel`,
which may be an alias outside the list). A well-formed id the agent does not
list is `model_unavailable` (422); a `model` without `agent`, or one shaped
like anything but a model id, is `invalid_request`. The request id is bound to
the model as it is to the folder and agent.

`instructions` is optional orientation text the agent receives through its
own system-level mechanism, never as a message in the conversation: Claude
Code gets it as `--append-system-prompt=<text>` (appended to its default
system prompt) and Codex as `-c developer_instructions=<TOML string>` (a
developer-role item, which replaces any `developer_instructions` the owner
configured for that one session). Each value travels as a single argv element,
so text that starts with `-` cannot become another option, and the login shell
receives it as a positional parameter rather than script text. It is
advertised as `features.sessionInstructions: { agents, maxBytes }`, present
exactly when the create route accepts the field; a client sends it only with
an `agent` listed there and must create without it anywhere else (an older
gateway's closed contract refuses the key). The text must be non-empty after
trimming, at most `maxBytes` UTF-8 bytes (8192), and free of control
characters other than tab and newline; anything else, or instructions without
an eligible agent, is `invalid_request` before anything is accepted. The
request id is bound to the text by its SHA-256 digest (the receipt never stores
the text), so a retry with other text, or none, is `request_id_conflict`. The
session's display metadata and command label never contain it.

`features.overlord: { signedIn, backendOrigin?, connectorAgents }` reports
this Mac's local Overlord readiness for clients that start chats meant to act
in Overlord. `signedIn` is true when the Overlord CLI's stored sign-in
(`$OVLD_HOME/auth.json`, default `~/.ovld/auth.json`) has a token, a backend
and no recorded expiry in the past; `backendOrigin` is that backend's scheme,
host and port only. `connectorAgents` lists the agents of `sessionAgents` whose
Overlord connector install record (`~/.ovld/connectors/<agent>.json`, written
by `ovld agent-setup <agent>`) names a directory that still exists. The
gateway reads these files on each discovery and never contacts Overlord, so a
revoked token still reads as signed in and the account itself is not known:
it is guidance, not authorization. An absent key means an older gateway, which
is unknown rather than unavailable.

Codex currently has a connector identity but no automatic transcript-source
binding for a newly launched session. Its conversation remains in `starting`
with send unavailable until an authoritative Codex binding is supplied.

`POST /v2/sessions/{id}/stop` ends one session's hosted process. It requires
the `control` grant — a device that may not type into the pane may not end
what is running in it either — and is advertised as `endpoints.stopSession`,
absent on a gateway that predates it.

The request has no body: there is no signal, force flag, or timeout to choose.
The Mac runs the same graceful stop `latch stop` does, sending SIGTERM,
waiting out its grace period, and escalating to SIGKILL on its own, so the
route can take several seconds to answer. The response is the existing
`StopReport` — `{ id, state, stopped }` — and a pane that survived both signals
is refused as `session_still_running` rather than reported as a stop that
happened.

Stopping is not removing. The session's record and its dead pane stay, so the
session remains in `GET /v2/sessions` as `exited` and whatever it left on
screen is still readable. There is no remote removal route; erasing a session
stays a decision made at the Mac. Repeating a stop is safe — a session that
has already exited answers the same way — which is what lets a client retry a
request whose response it never saw.

This is the one stop operation, reached from wherever a caller can reach Latch.
`latch stop SESSION --json` on the Mac, `client.stopSession({ sessionId })`
against a loopback or tunneled gateway, and the paired proxy a phone speaks
through all run the same engine stop and answer the same `StopReport`, so an
integration such as Overlord's mission panel reads one shape whether it is
sitting on the Mac or not. `stopSession` consults discovery before posting and
refuses with its own `stop_unsupported` code when `endpoints.stopSession` is
absent, so an old gateway's bare 404 on the path is never read as the session
being gone.

Every gateway failure is `{ "error": <code>, "reason": <text> }`, and
`LatchGatewayError.code` carries the code. Branch on it, not on the text:

| Code | Status | Meaning |
| --- | --- | --- |
| `session_not_found` | 404 | Latch has no session by that id or name. |
| `session_ambiguous` | 409 | A name matched more than one session; use the id. |
| `session_still_running` | 409 | The pane survived SIGTERM and SIGKILL. |
| `request_id_conflict` | 409 | A creation request id was reused with a different `cwd`. |
| `request_failed` | any | The answer carried no more specific code. |

There is currently no `@latch/conversation-react`, remote React SDK example,
public event cursor, transcript reducer, HTTP send endpoint, compatibility mode,
or v1 fallback. Until the planned SDK ships, internal consumers needing a
conversation UI use the canonical v2 socket rather than recreating or importing
a legacy surface.

## Development

```bash
npm install
npm run typecheck
npm run build
npm test
```

The packages are private while Latch is unlicensed. The planned conversation SDK
starts from the v2 conversation protocol and generated canonical contract, not
from removed v1 APIs.
