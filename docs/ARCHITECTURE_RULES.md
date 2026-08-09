# Architecture rules

Constraints that CI enforces, or that a reviewer should reject a change for.
Everything here is derived from [`planning/PROJECT_ARCHITECTURE.md`](../planning/PROJECT_ARCHITECTURE.md)
and [`planning/IMPLEMENTATION_PLAN.md`](../planning/IMPLEMENTATION_PLAN.md); this
file exists so the rules are checkable rather than remembered.

## Layout

```text
crates/
  latch/                   # the single binary: CLI + worker modes
  latch-protocol/          # framing, control messages, codec
  latch-term/              # screen model + snapshot serialization
packages/                  # M3 onward — TypeScript
fixtures/                  # language-neutral protocol + VT fixtures
docs/
planning/
```

## Dependency direction

`latch-protocol` and `latch-term` are **leaves**. They may depend on
third-party crates; they may not depend on each other, on `latch`, or on
anything with an I/O runtime.

`latch` depends on both. Nothing else may.

*Why:* the protocol crate has to be embeddable somewhere that is not this
binary, and the screen model has to be testable without a PTY. A dependency in
the other direction makes both untrue at once and is not usually noticed until
someone tries.

## No Overlord in `crates/`

Nothing under `crates/` may import, link, or vendor an Overlord type, and no
Latch code path may require Overlord to exist. Latch is useful without Overlord;
Overlord is one client that can ask Latch to create a session.

Integration flows the other way: Overlord calls the public `latch` CLI or API,
never Latch's session directories or cloud tables.

Enforced by `scripts/check-boundaries.sh` in CI.

## Rust owns the process plane; TypeScript owns the presentation plane

No Node.js in the local plane. The deciding factor is that a terminal profile
points at `latch`, so **every terminal window pays CLI startup cost** — and a
perceptible hitch on every new window is close to disqualifying for a customer
whose stated requirement is that their terminal experience is preserved.

TypeScript is the language of `packages/` and the M4 cloud plane.

## The two implementations do not share code

`packages/protocol` and `crates/latch-protocol` are independent implementations
kept honest by `fixtures/`. Do not introduce a shared generator, a WASM build of
the Rust codec, or a schema that emits both. The fixtures are the contract, and
their value comes precisely from the implementations being written separately.

## Session state is derived, never stored

```text
socket accepts a connection      -> ask the worker (creating | running | stopping)
exit.json present, socket gone   -> exited
neither                          -> lost
```

Do not add a registry file, a status field in `meta.json`, or a cache of session
state. There is no state to diverge from reality, nothing to reconcile after a
restart, and no schema to migrate — that is the point.

The corollary is structural rather than a discipline to remember: **no stored
PID is ever consulted for a kill.** `latch stop` sends a message to the live
worker, which signals its own child's process group. A session whose socket does
not answer cannot be stopped, because there is nothing left to stop.

## Secrets never reach disk or argv

Launch material arrives over stdin or the socket and lives only in worker
memory. `meta.json` holds bounded display metadata and a *redacted* command
label — never full argv, environment blocks, or tokens.

## Sanitize display metadata at ingest

Externally supplied names and titles are sanitized to printable characters **at
the boundary where they arrive**, not where they are rendered. A mission title
flows from a caller into terminal titles, `latch list` output, and later into
web and mobile clients; sanitizing at render means every future call site is a
new chance to forget.

## The PTY read never blocks on a client

A slow or hung client gets its queue dropped and a fresh snapshot. It does not
get an unbounded buffer, and it never applies backpressure to the PTY — that
would freeze the child process for everyone attached to it.

## Filesystem modes

`~/.latch` and every session directory are `0700`. Sockets are `0600`. This is
asserted by tests, not left to umask.
