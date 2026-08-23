# Latch TypeScript integration

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
available `sessions`, `terminal`, and `conversation` endpoints. A consumer must
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
