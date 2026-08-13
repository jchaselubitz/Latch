# Architecture rules

Constraints derived from
[`planning/ENGINE_PLAN.md`](../planning/ENGINE_PLAN.md) and enforced in review
or CI.

## Layout

```text
crates/
  latch/                   # CLI, metadata sidecar, tmux engine
apps/LatchDesktop/         # native macOS client
packages/                  # TypeScript presentation clients
fixtures/harness/          # public schemas plus raw/normalized connector corpus
fixtures/vt/               # irreplaceable recorded harness streams
```

The retired Rust terminal server is archived at the
`archive/latch-term-v1` tag. Do not recreate worker, framing, attachment
registry, screen-model, or resize-authority modules in the active workspace.

## The session kernel is private and pinned

Latch invokes the bundled `latch-tmux` executable by absolute path with both
`-S ~/.latch/server` and `-f ~/.latch/tmux.conf`. It must never discover the
user's tmux executable, configuration, socket, or sessions.

The generated configuration has no status bar, prefix, or copy-mode keys. It
sets `remain-on-exit on`, `window-size latest`, and a deliberate
`default-terminal`. The child environment removes `TMUX`; nesting is detected
only through `LATCH_SESSION_ID`.

## No Overlord in `crates/`

Nothing under `crates/` may import, link, or vendor an Overlord type. Latch is
useful without Overlord; Overlord is one client of the public CLI.

## Rust owns the local engine

No Node.js process sits on the every-window path. A terminal profile invokes
`latch`, so startup latency remains a product requirement. TypeScript belongs
in `packages/` and presentation clients.

## Harness events are schema-first

`fixtures/harness/harness-event.v1.json` and
`interaction-capabilities.v1.json` own the public observation and interaction
contracts. Rust connector types and TypeScript consumer types are generated
from those schemas. Connector fixtures retain raw records alongside normalized
events; parser changes must preserve deterministic event indexes within a
connector epoch.

Claude transcripts do not reliably contain pending input. A generated,
owner-only plugin is therefore injected into directly launched Claude
processes and captures only `PermissionRequest` records. It must not modify
global Claude settings. Transcript and hook records feed an append-only
per-session event ledger; emitted cursor positions are ledger indexes and are
never renumbered when one source writes late.

## Session state is queried, never stored

```text
tmux has a live pane       -> running
tmux has a dead pane       -> exited
metadata without a session -> lost
```

Do not add a stored status field or process id. `has-session`,
`list-sessions`, and pane formats are authoritative. A process id may be
queried from tmux for an immediate signal operation but is never persisted.

## Launch secrets never reach disk

`latch create` accepts launch material over stdin. The engine transfers it to
the pane launcher over an owner-only FIFO, then removes the FIFO. `meta.json`
contains only bounded display metadata, a redacted command label, and an
opaque external correlation id.

## Sanitize display metadata at ingest

Names, titles, command labels, source kinds, and external ids are reduced to
bounded printable text before they are stored or rendered.

## Distribution is one signed payload

Every release archive contains `latch` and pinned `latch-tmux`. Both binaries
carry the same Developer ID and are covered by the notarized archive. The
updater verifies the archive checksum and both signatures, stages both files,
and rolls back the tmux replacement if replacing the CLI fails.

## Filesystem modes

`~/.latch` and every session directory are `0700`. Metadata, generated config,
and launch FIFOs are owner-only.
