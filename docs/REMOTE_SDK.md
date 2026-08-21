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
subprotocol. `mode=read-only` is explicit and only valid when discovery
advertises the feature.

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
