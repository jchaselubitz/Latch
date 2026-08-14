# Latch Remote SDK

`latch serve` exposes a Latch session to a browser or native client through a
token-protected HTTP/WebSocket gateway. The Remote SDK packages are the public
client surface:

- `@latch/client` — session discovery, terminal, events, and interaction.
- `@latch/terminal-react` — the terminal component, with xterm private.
- `@latch/chat-react` — transcript state, composer, and request-bound prompt.
- `@latch/harness-schema` — generated event and interaction contract types.

## Gateway compatibility

`/v1` is a compatibility boundary, not a snapshot of one server release. A
gateway keeps the documented paths and fields stable for protocol major 1;
later additions are optional and additive. It never repurposes a field,
changes a WebSocket frame type, or removes an endpoint while it still reports
`protocolVersion: 1`.

`GET /v1/capabilities` is the mandatory discovery step for optional features.
It returns `protocolVersion`, `productVersion`, and an `endpoints` map. A
client may use an endpoint only when the map reports it as `true`. A missing or
`false` `events` endpoint means show the terminal instead of chat; a missing or
`false` `send` endpoint means render transcript-only UI; a missing session
capabilities endpoint means do not render interaction controls. A client must
not probe an optional endpoint and infer support from an error.

The client treats a gateway that returns 404 for `/v1/capabilities` as the
pre-discovery, terminal-only gateway: sessions and terminal remain available;
events, send, and session capabilities are disabled. A `protocolVersion` other
than 1 is unsupported rather than guessed at. The exported
`supportsGatewayEndpoint()` encodes this rule.

## Publishing decision

The packages now build standard ESM JavaScript and `.d.ts` declarations into
`dist/`; published exports never point at TypeScript source. All four packages
use the `@latch/*` npm scope, version together starting at `0.1.0`, and declare
the repository's current `UNLICENSED` license. They are deliberately still
private workspaces: the Latch project is not licensed for third-party
redistribution, and publishing a public SDK with that metadata would mislead
consumers. When Latch chooses a redistribution license and claims the npm
scope, remove `private`, set `publishConfig.access` deliberately, run
`npm run build`, and publish the four matching versions to npm.

The build and the example intentionally use only each package's exports map,
so that future publication is checked without exposing source files as API.

## Running the example

Start the gateway on the session host, then copy its token into the example.

```bash
latch serve
latch serve token
npm install
npm run build
npm run example
```

Open the example, enter the gateway URL and token, and choose a session. It
uses the public packages for the terminal, transcript, composer, and
permission/question prompt. For remote access, point the URL at an SSH tunnel,
Tailscale, or a reverse proxy that terminates TLS; `latch serve` itself is
plaintext and loopback-only by default.

## Operational decisions

Rotating the token with `latch serve token` affects new HTTP and WebSocket
handshakes immediately. Existing authenticated WebSocket connections stay
open; reconnecting clients must use the newly minted token. This avoids a
surprise terminal disconnect while still making a leaked token unusable for
new connections.

Every v1 terminal client is a controller. Its resize can affect tmux's
`window-size latest` dimensions, so a phone connection can reflow the desktop
terminal. Use `latch resize --pinned` where size stability matters. A
read-only, ignore-size viewer mode is a future protocol addition, not behavior
silently inferred from screen size.

A terminal connection receives tmux's current visible screen only; it does
not backfill scrollback. The events/chat surface is the v1 history view.
Scrollback backfill via `capture-pane -S` remains deferred.
