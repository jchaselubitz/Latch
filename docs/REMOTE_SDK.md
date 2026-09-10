# Latch TypeScript integration

This is the repository-level TypeScript gateway guide. For the complete
provider boundary, launch manifest, and Overlord example, start with
[Integrations](INTEGRATIONS.md).

Latch protocol major 2 deliberately has a small TypeScript surface:

- `@latch/client` discovers sessions and opens a terminal WebSocket.
- `@latch/terminal-react` renders a terminal handle through an embedder-supplied
  xterm renderer.

Both packages are private workspaces. They are terminal integrations, not a
conversation SDK. The only supported conversation client is the native mobile
client, which speaks the canonical v2 conversation socket directly.

## Protocol boundary

Use `GET /v2/capabilities` before opening a gateway connection. It reports
protocol major 2, the gateway instance ID, operation retention, and the
available `sessions`, `terminal`, `preview`, `conversation`,
`browseDirectories`, and `createSession` endpoints. A consumer must
require `protocolVersion: 2`; v1 has been removed and is not negotiated,
probed, or adapted.

`@latch/client` supports session listing, inspection, discovery, and terminal
attachment. The terminal socket is
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

Creation takes only two fields:

```json
{ "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7", "cwd": "/Users/jake/src" }
```

`requestId` is a canonical hyphenated UUID and is the idempotency key. The Mac
holds a creation lock across the lookup and the durable metadata write, so
repeating one request id returns the session it already created rather than a
second one; the same id against a different `cwd` is refused as
`request_id_conflict`. Only the working directory crosses the boundary. The
gateway builds the same interactive login shell `latch shell` does, at the
standard initial geometry, with no command, agent, name, environment, or shell
path supplied by the caller, and does not attach — the new session appears in
`GET /v2/sessions` with no surface taken from anyone. The response is the
existing `CreateReport`.

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

There is no `@latch/chat-react`, `@latch/harness-schema`, remote React SDK
example, event cursor, transcript reducer, HTTP send endpoint, compatibility
mode, or v1 fallback. Consumers needing a conversation UI should implement the
v2 socket from the canonical schema; they must not recreate or import a legacy
surface.

## Development

```bash
npm install
npm run typecheck
npm run build
npm test
```

The packages are private while Latch is unlicensed. A future public SDK needs a
separate versioned design; it must start from the v2 conversation protocol, not
from removed v1 APIs.
