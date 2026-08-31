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
latch remove old-session    # remove one exited/lost session
latch update                # replace the CLI, remote helper, and latchd payload
```

Point your terminal profile's command at `latch` and every window you open is
already a persistent session, with nothing about using your terminal feeling
different. That is the intended adoption path, and it is configuration rather
than code.

## Install

Download the standalone CLI payload and Apple Silicon macOS desktop app from
the [latest GitHub release](https://github.com/jchaselubitz/Latch/releases/latest).
The desktop archive contains only `Latch.app`; drag it to Applications and
install the CLI independently with:

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The installer chooses the Apple Silicon or Intel archive, verifies it against
the release's `checksums.txt`, verifies all three Developer ID signatures and
the payload manifest, and installs `latch`, `latch-remote`, and `latchd` in
`~/.local/bin`. See the [getting-started guide](docs/GETTING_STARTED.md)
for setup and first use.

## Updating

`latch update` replaces the complete three-binary payload with the newest release published
at [Latch releases](https://github.com/jchaselubitz/Latch/releases). It
verifies the archive against the checksums that release publishes, and — when
the installed binary is Developer ID signed — refuses a download signed by
anyone else. `latch update --check` reports what is available without
installing it.

A copy something else owns is refused rather than diverged from its package: a
Homebrew cellar binary says to run `brew upgrade`. Latch Desktop updates only
the app, independently of the selected CLI, from **Latch → Check for Updates…**
or its menu-bar extra. Verification there is Gatekeeper's, against the signing
identity the installed app already has.

## Status

The session kernel is the per-session `latchd` daemon behind the Latch
CLI. A session has one live terminal surface, so a new attach moves it rather
than sharing it; closing that terminal does not stop the process. The session's
recorded kernel determines all later lifecycle and attachment operations. The
former tmux fallback is no longer shipped; a pre-cutover release is the
rollback boundary for legacy tmux-hosted sessions.

When Latch launches Claude Code directly, it adds an owner-only observation
plugin for that process only. The plugin captures bounded raw source bindings
and permission observations beside the session; it never changes the user's
global Claude settings or exposes agent-specific records to clients. The v2
Conversation Hub consumes those sources behind an agent-neutral boundary.

## Layout

```text
crates/
  latch/                   # CLI, metadata, and latchd engine
apps/LatchDesktop/         # native macOS session manager + menu-bar extra
packages/                  # protocol-major-2 TypeScript contracts and terminal client
schemas/remote-access/v2/  # canonical gateway and conversation schemas
fixtures/                  # raw agent transcripts and retained VT recordings
docs/
planning/
```

## Development

```bash
cargo test --workspace          # unit, integration, and fixture suites
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python3 scripts/generate-remote-access-types.py  # regenerate v2 Rust + TypeScript contracts
./scripts/check-boundaries.sh   # dependency and layering rules
swift test --package-path apps/LatchDesktop  # desktop contract tests (macOS)
```

The rules CI enforces are written down in
[`docs/ARCHITECTURE_RULES.md`](docs/ARCHITECTURE_RULES.md). Read that before
adding a dependency between crates.

The native companion invokes the same JSON CLI contracts as every other client;
the CLI remains fully usable without the app installed. See the
[Desktop guide](docs/DESKTOP.md) for usage and
[`apps/LatchDesktop/README.md`](apps/LatchDesktop/README.md) for development
and release instructions.

## Documentation

- [Getting started](docs/GETTING_STARTED.md)
- [CLI reference](docs/CLI.md)
- [Latch Desktop](docs/DESKTOP.md)
- [Integrations](docs/INTEGRATIONS.md)
- [Documentation index](docs/README.md)

## Planning

- [`planning/PROJECT_ARCHITECTURE.md`](planning/PROJECT_ARCHITECTURE.md) — the
  product and its boundaries
- [`planning/ENGINE_PLAN.md`](planning/ENGINE_PLAN.md) — current engine sequence
- [`planning/OVERLORD_INTEGRATION.md`](planning/OVERLORD_INTEGRATION.md) — how
  Overlord uses Latch without either product requiring the other
- [`planning/ARCHITECTURE_REVIEW.md`](planning/ARCHITECTURE_REVIEW.md) — the
  analysis that produced this sequence
