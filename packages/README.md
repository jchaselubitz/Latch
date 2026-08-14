# packages/

TypeScript contracts and presentation clients.

```text
harness-schema/    # generated normalized-event and interaction types
client/            # remote session client (terminal, events, send, session list/inspect)
terminal-react/    # xterm.js behind a Latch renderer API
chat-react/        # transcript store, `useTranscript`, AwaitingInputPrompt, Composer
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
terminal WebSocket to that renderer. `useTranscript` folds the events
WebSocket into a renderable timeline. `<Composer>` and `<AwaitingInputPrompt>`
submit through `send()`; Composer disables with a reason when `canSend.ok` is
false, and a prompt widget is bound to the `requestId` it was rendered for.
Chat surfaces should still be able to flip to the terminal.

`useTranscript` persists a snapshot — timeline, cursor, and the connector
epoch that minted it — so a reload resumes instead of re-deriving. It uses
`localStorage` when the host has one; pass `storage: null` for memory-only, or
your own `TranscriptStorage` (React Native, a test double). A snapshot with an
unknown epoch or a different envelope version is dropped, not migrated: the
transcript is derivable from the stream, so replaying from cursor 0 is always
the cheaper repair.

## Testing

```bash
npm test             # node --test across the workspaces
```

`client/src/gateway.test.ts` is the end-to-end one: it builds a temp
`LATCH_HOME` on the fake tmux in `fixtures/testing/`, starts the real
`latch serve`, and drives it through `@latch/client` over Node's WebSocket —
sessions, capabilities, events with cursor resume and 4422 re-sync, a 409
refusal and a permitted send, and terminal bytes in both directions. Every
other test in the package fakes the socket, so this is what would notice a
renamed field or a moved path. It skips itself when `target/debug/latch` has
not been built; `just check` runs `cargo test` first, so it does not skip
there.

## Working on them

```bash
npm install          # workspaces; the lockfile is checked in
npm run typecheck    # tsc --noEmit per package
npm run build         # ESM + declarations in each package's dist/
npm test             # node --test, no bundler
just check-web       # all four, and what `just check` runs
```

The package exports point at built ESM and declarations under `dist/`, never at
the source tree. TypeScript rewrites the source's explicit `.ts` and `.tsx`
relative imports to runtime `.js` and `.jsx` specifiers during the build. The
workspaces remain private because Latch is currently `UNLICENSED`; see
[`docs/REMOTE_SDK.md`](../docs/REMOTE_SDK.md) for the publication decision and
the React composition example.

## The renderer contract

A renderer is duplex. `write` paints bytes from the session; `onInput` hands
back keystrokes and pasted bytes, which the component forwards to the terminal
socket. A renderer that implements only `write` produces a viewer, not a
terminal — `latch attach` is on the other end waiting for stdin.
