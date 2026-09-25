# Remote Link security invariants

Remote Link bridges an authenticated remote device to the host's fixed local
gateway. These regression tests protect the boundary conditions that make that
bridge safe. A failure should be treated as a security regression, not as a
test to weaken.

- A gateway request has at most one device-grant header and one device-id
  header. Duplicate authority headers are rejected before the loopback trust
  decision.
- The proxy parses and validates caller-controlled request fields, then writes
  a fresh request. Caller-supplied header bytes are never forwarded to the
  gateway; the proxy alone adds the bearer, grant, and device-id headers.
- Every request is buffered whole, under the 32 KiB initial-request bound,
  and inspected before a byte reaches the gateway — except a route whose
  table entry declares a `streamed_body_limit`, which today is only
  `POST /v2/sessions/{id}/attachments`. That request is authorized from its
  headers alone, must declare a `Content-Length` within the limit, and the
  proxy relays exactly that many body bytes; anything after them is
  discarded, so a second request can never ride the stream unauthorized.
- An attachment lands only under the session's recorded working directory,
  in `.latch-attachments/`, under a gateway-chosen name reduced to
  `[A-Za-z0-9._-]`. The folder and file are opened relative to a directory
  descriptor with `O_NOFOLLOW`, the file with `O_EXCL`, so neither a planted
  symlink nor an existing file redirects or overwrites the write. A body that
  ends short, runs long, or is abandoned leaves no file behind.
- `latch serve` binds only a loopback address. Its plaintext bearer boundary
  must never be exposed through an opt-in public listener.
- Every upgraded relay WebSocket and its raw upgrade socket has an error
  handler, so a malformed or oversized peer closes only that peer.
- Relay capacity is reserved before asynchronous redemption begins. Pending
  redemptions count against global and per-IP limits and release their
  reservation on every exit path.

## Named CI gate

The `Remote Link security gate` workflow job runs the Rust CLI security target,
proxy tests, attachment route tests, relay tests whose names are tagged
`containment`, and the Swift `RemoteAccessTests`, `UpdaterTests`, and
`GatewayTransportTests` suites.
Run the same component tests when changing their corresponding boundary.

## Dependency-audit baseline

The first audit run on 2026-09-24 found no RustSec advisories in `Cargo.lock`
and no production dependency vulnerabilities in `services/control-plane`,
`services/relay`, `packages/client`, or `packages/terminal-react`. No advisory
exceptions are accepted. The `Dependency audits` workflow job repeats `cargo
audit` and `npm audit --omit=dev` for every pull request.
