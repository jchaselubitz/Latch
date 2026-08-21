# packages/

TypeScript protocol contracts and the terminal presentation client.

```text
client/            # protocol-major-2 discovery, sessions, and terminal
terminal-react/    # xterm.js behind a Latch renderer API
```

The local plane remains Rust only. `@latch/client` exposes discovery, sessions,
and terminal attachment; `<LatchTerminal>` wires the v2 terminal WebSocket to
an embedder-supplied renderer. Conversation behavior is not a TypeScript
package API: the native client consumes the canonical v2 Hub schema directly.
No event/cursor API, chat package, harness-schema package, or compatibility
surface remains.

## Testing

```bash
npm run typecheck
npm run build
npm test
```

The packages are private while Latch is unlicensed.
