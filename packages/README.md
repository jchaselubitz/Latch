# packages/

TypeScript, starting at M3.

```text
protocol/          # TypeScript codec, fixture-verified against the Rust one
session-client/    # attachment client with reconnect
terminal-react/    # xterm.js behind a stable Latch renderer API
```

Deliberately empty until then. The local plane is Rust only — with a terminal
profile pointed at `latch`, every window pays CLI startup cost, and Node's
startup is not affordable there.

Two rules apply when this fills in:

**`protocol/` does not share code with `crates/latch-protocol`.** They are
independent implementations kept honest by `fixtures/`. No shared generator, no
WASM build of the Rust codec, no schema that emits both — the value comes from
them being written separately.

**xterm.js is not the public surface.** It sits behind a Latch renderer API so
it can be replaced without breaking every embedder.
