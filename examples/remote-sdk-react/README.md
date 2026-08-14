# Latch Remote SDK React example

This app imports only the public exports of `@latch/client`,
`@latch/terminal-react`, and `@latch/chat-react`. It is the integration check
for the complete remote UI: choose a session, use its terminal, read its event
timeline, send a message, and answer a permission or question prompt.

Run it from the repository root after starting `latch serve`:

```bash
npm install
npm run build
npm run example
```

The connection form accepts the gateway URL and the token printed by `latch
serve token`. It saves them only in this browser's local storage. For remote
use, tunnel the loopback gateway and provide the tunnel URL; do not expose the
gateway's plaintext listener directly to the internet.
