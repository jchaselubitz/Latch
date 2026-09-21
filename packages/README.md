# packages/

TypeScript protocol contracts and the terminal presentation client.

```text
client/            # protocol-major-2 discovery, sessions, and terminal
terminal-react/    # xterm.js behind a Latch renderer API
```

The local plane remains Rust only. `@latch/client` currently exposes discovery,
sessions, and terminal attachment; `<LatchTerminal>` wires the v2 terminal
WebSocket to an embedder-supplied renderer. Conversation behavior is not yet a
TypeScript package API: the native client consumes the canonical v2 Hub schema
directly. A headless conversation client and optional React renderer are planned
in the
[rich conversation implementation guide](../planning/RICH_CONVERSATION_UI_IMPLEMENTATION_PLAN.md#phase-7--minimal-embeddable-web-sdk).

## Testing

```bash
npm run typecheck
npm run build
npm test
```

The packages are private while Latch is unlicensed.
