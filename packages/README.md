# packages/

TypeScript contracts and presentation clients.

```text
harness-schema/    # generated normalized-event and interaction types
client/            # remote session client (`attachTerminal`, session list/inspect)
terminal-react/    # xterm.js behind a Latch renderer API
```

The local plane remains Rust only — with a terminal profile pointed at `latch`,
every window pays CLI startup cost, and Node's startup is not affordable there.
`harness-schema` is compile-time consumer material; it is never loaded by the
CLI.

`harness-schema/src/generated.ts` and
`crates/latch/src/harness/generated.rs` come from the same JSON schemas under
`fixtures/harness/`. Regenerate both with
`scripts/generate-harness-types.py`; do not edit either output by hand.

**xterm.js is not the public surface.** It sits behind a Latch renderer API so
it can be replaced without breaking every embedder. `@latch/client` talks to
`latch serve` with a URL and a bearer token; `<LatchTerminal>` wires the
terminal WebSocket to that renderer.
