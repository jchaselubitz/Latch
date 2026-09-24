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
- `latch serve` binds only a loopback address. Its plaintext bearer boundary
  must never be exposed through an opt-in public listener.
- Every upgraded relay WebSocket and its raw upgrade socket has an error
  handler, so a malformed or oversized peer closes only that peer.
- Relay capacity is reserved before asynchronous redemption begins. Pending
  redemptions count against global and per-IP limits and release their
  reservation on every exit path.

## Named CI gate

The `Remote Link security gate` workflow job runs the Rust CLI security target
and proxy tests, relay tests whose names are tagged `containment`, and the
Swift `RemoteAccessTests`, `UpdaterTests`, and `GatewayTransportTests` suites.
Run the same component tests when changing their corresponding boundary.

## Dependency-audit baseline

The first audit run on 2026-09-24 found no RustSec advisories in `Cargo.lock`
and no production dependency vulnerabilities in `services/control-plane`,
`services/relay`, `packages/client`, or `packages/terminal-react`. No advisory
exceptions are accepted. The `Dependency audits` workflow job repeats `cargo
audit` and `npm audit --omit=dev` for every pull request.
