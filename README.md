# Latch

Persistent terminal sessions that outlive the window showing them.

A process runs in a session on your own machine. Any terminal, web view, or
mobile client can attach to that same live session — and detaching is not
terminating. You keep iTerm; you do not adopt a new terminal.

```bash
latch                       # a persistent shell, attached
latch run -- claude         # a persistent agent session
latch list                  # what is running
latch attach auth-refactor  # from anywhere, including a phone over SSH
```

Point your terminal profile's command at `latch` and every window you open is
already a persistent session, with nothing about using your terminal feeling
different. That is the intended adoption path, and it is configuration rather
than code.

## Status

Early. The workspace scaffold is in place and the milestones are sequenced;
`crates/` is being filled in from M1 outward. Commands exist in `latch --help`
before they work.

## Layout

```text
crates/
  latch/                   # the single binary: CLI + worker modes
  latch-protocol/          # framing, control messages, codec
  latch-term/              # screen model + snapshot serialization
packages/                  # TypeScript clients, M3 onward
fixtures/                  # language-neutral protocol + VT fixtures
docs/
planning/
```

## Development

```bash
cargo test --workspace          # unit, integration, and fixture suites
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
./scripts/check-boundaries.sh   # dependency and layering rules
```

The rules CI enforces are written down in
[`docs/ARCHITECTURE_RULES.md`](docs/ARCHITECTURE_RULES.md). Read that before
adding a dependency between crates.

## Planning

- [`planning/PROJECT_ARCHITECTURE.md`](planning/PROJECT_ARCHITECTURE.md) — the
  product and its boundaries
- [`planning/IMPLEMENTATION_PLAN.md`](planning/IMPLEMENTATION_PLAN.md) —
  milestones M1–M6, and the decisions each one rests on
- [`planning/OVERLORD_INTEGRATION.md`](planning/OVERLORD_INTEGRATION.md) — how
  Overlord uses Latch without either product requiring the other
- [`planning/ARCHITECTURE_REVIEW.md`](planning/ARCHITECTURE_REVIEW.md) — the
  analysis that produced this sequence
