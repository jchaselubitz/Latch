# Architecture rules

Constraints derived from
[`planning/ENGINE_PLAN.md`](../planning/ENGINE_PLAN.md) and enforced in review
or CI.

## Layout

```text
crates/
  latch/                   # CLI, metadata sidecar, latchd engine
apps/LatchDesktop/         # native macOS client
packages/                  # TypeScript presentation clients
services/control-plane/    # cloud control plane (independent deployable)
fixtures/conversation/     # raw Claude/Codex connector corpus
fixtures/vt/               # irreplaceable recorded harness streams
```

The retired Rust terminal server is archived at the
`archive/latch-term-v1` tag. Do not recreate worker, framing, attachment
registry, screen-model, or resize-authority modules in the active workspace.

## The session kernel is private and pinned

Latch invokes the bundled `latchd` executable by absolute path. One daemon
owns one session PTY and authenticates clients on an owner-only Unix socket.
There is no system-kernel discovery, shared server, window, tab, or pane model.

The coordinated payload pins the daemon version and protocol to the CLI.
Doctor, installer, and updater reject a missing or mixed-version daemon before
creating a session. The child receives a deliberate terminal type; nesting is
detected only through `LATCH_SESSION_ID`.

## A session has at most one human surface

Every human attach — `latch attach`, bare `latch`, `latch open`, a Desktop
viewer, an SSH or Termius session, and the gateway's WebSocket terminal — goes
through the same exclusive attach. It preflights, then atomically takes the
surface: the previous holder is detached with a reason, its terminal is
restored, and the pane adopts the new geometry before a single complete frame
is painted. After that frame the tty receives the pane's own bytes.

A failed preflight leaves the current surface live and untouched. There is no
mirrored attach, no watch mode, no read-only live terminal, and no ordinary
second live attach. Someone who only wants to observe uses
Conversation Hub or `latch inspect`.

`GET /v2/sessions/{id}/preview` does not contradict that rule. It is a
structured snapshot query — the same one-shot grid read the Conversation Hub already
performs to observe a pane's screen — returning the cells as they stood at one
instant, with the time they were read. It does not enter the exclusive attach,
does not paint continuously, does not follow the session, and cannot carry
input. It is therefore allowed at the `observe` grant, which the terminal route
refuses. A still is not a second live surface; the moment a client wants the
next frame it must take the surface like everyone else.

Latch remains the execution/session provider, not a terminal emulator. iTerm,
Terminal, Termius, and the mobile terminal are viewers; changing or stealing
the viewer must never recreate the agent session.

## Cloud services are independent deployables

`services/` holds cloud services. Each has its own manifest, dependency set,
migrations, credentials, and deployment, and each ships without the others.
They are not root workspace members and must not depend on an `@latch/`
package from `packages/`: the local plane is a client of a cloud API, never a
build-time dependency of one.

The control plane and the relay stay separate deployables. Their scaling and
bandwidth profiles differ, and the split is also the privacy boundary: the
control plane authorizes relay admission and never forwards frames, while the
relay forwards opaque frames and never learns a pairing, a device key, or an
account. Neither may store terminal content, transcripts, session names, or a
Latch gateway token; `services/control-plane/src/privacy.test.ts` enforces
that mechanically for the control plane.

## No Overlord in `crates/`

Nothing under `crates/` may import, link, or vendor an Overlord type. Latch is
useful without Overlord; Overlord is one client of the public CLI.

## Rust owns the local engine

No Node.js process sits on the every-window path. A terminal profile invokes
`latch`, so startup latency remains a product requirement. TypeScript belongs
in `packages/` and presentation clients.

## Conversation Hub is schema-first

`schemas/remote-access/v2/` owns the public conversation and gateway contract.
The Hub owns projection ordering, revisions, generations, operation epochs,
pending-request derivation, and durable operation outcomes. Clients consume one
server-first conversation socket; they do not fold harness events or maintain
event cursors.

Connector fixtures retain bounded raw Claude and Codex records alongside their
expected normalized projections in `fixtures/conversation/`. Agent-specific
source binding, transcript parsing, hooks, and action execution stay behind the
connector boundary. The owner-only Claude hook must not modify global Claude
settings.

## Session state is queried, never stored

```text
latchd reports a live child -> running
latchd retains child exit   -> exited
metadata without a daemon   -> lost
```

Do not add a stored status field or process id. The authenticated daemon stat
reply is authoritative. A child process id may be queried for an immediate
signal operation but is never persisted.

## Launch secrets never reach disk

`latch create` accepts launch material over stdin. The engine transfers it to
the pane launcher over an owner-only FIFO, then removes the FIFO. `meta.json`
contains only bounded display metadata, a redacted command label, and an
opaque external correlation id.

## Sanitize display metadata at ingest

Names, titles, command labels, source kinds, and external ids are reduced to
bounded printable text before they are stored or rendered.

## Distribution is one signed payload

Every release archive contains `latch`, `latch-remote`, and pinned `latchd`. All three
binaries carry the same Developer ID and are covered by the notarized archive. The
updater verifies the archive checksum and all signatures, stages every file,
and rolls back the siblings if replacing the CLI fails.

## Filesystem modes

`~/.latch` and every session directory are `0700`. Metadata, generated config,
and launch FIFOs are owner-only.
