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
latch events auth-refactor --json  # normalized live agent events
printf '%s' 'continue' | latch send auth-refactor --message -
latch send auth-refactor --resolve permission-1='Allow once'
latch remove old-session    # remove one exited/lost session
latch update                # replace the CLI, remote helper, and tmux payload
```

Point your terminal profile's command at `latch` and every window you open is
already a persistent session, with nothing about using your terminal feeling
different. That is the intended adoption path, and it is configuration rather
than code.

## Install

Download the standalone CLI payload and the universal macOS desktop app from the same
[GitHub Release](https://github.com/jchaselubitz/Latch/releases/latest). The
current desktop archive is
[Latch-0.2608181100.0-macos.zip](https://github.com/jchaselubitz/Latch/releases/download/v0.2608181100.0/Latch-0.2608181100.0-macos.zip);
it contains only `Latch.app`. Drag the app to Applications, then install the CLI
independently with:

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The installer chooses the Apple Silicon or Intel archive, verifies it against
the release's `checksums.txt`, verifies both Developer ID signatures, and
installs `latch`, the `latch-remote` helper, and its pinned private `latch-tmux` kernel in
`~/.local/bin`. Latch always invokes that sibling tmux binary by absolute path; it
never discovers or joins a tmux server the user runs.

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

The session kernel is a private, pinned tmux server behind the Latch CLI.
Multiple terminals can attach, closing a terminal does not stop the process,
and session state is queried directly from tmux.

When Latch launches Claude Code directly, it adds an owner-only observation
plugin for that process only. The plugin records pending permission and
question hooks beside the session transcript; it never changes the user's
global Claude settings. `latch events` combines those hooks with transcript
messages in a persistent event ledger, so numeric cursors remain stable even
when Claude writes transcript records late.

`latch capabilities <session> --json` reports the schema-defined interaction
capabilities for the current screen. `latch send` reads messages from stdin,
sends explicit key names with `--keys`, and binds `--resolve` to the exact
pending request id. `--message` and `--resolve` are offered only when Latch
knows the session is hosting Claude Code (a harness marker written at launch
from `claude` argv, or the session's hook sidecar). Plain shells — including
ones whose prompt uses the same `❯` glyph as Claude's composer — report
`sendMessage=false` and keep `--keys` as the explicit caller-owns-the-risk
path. Latch captures the live tmux pane before every operation: an empty
Claude composer and a visible numbered prompt are accepted, while typed text,
stale requests, exited sessions, and unrecognized screens are refused rather
than receiving input.

## Layout

```text
crates/
  latch/                   # CLI, metadata, and private tmux engine
apps/LatchDesktop/         # native macOS session manager + menu-bar extra
packages/                  # generated harness contract and TypeScript clients
fixtures/                  # harness schemas, raw transcripts, retained VT recordings
docs/
planning/
```

## Development

```bash
cargo test --workspace          # unit, integration, and fixture suites
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
python3 scripts/generate-harness-types.py  # regenerate Rust + TypeScript contracts
./scripts/check-boundaries.sh   # dependency and layering rules
swift test --package-path apps/LatchDesktop  # desktop contract tests (macOS)
```

The rules CI enforces are written down in
[`docs/ARCHITECTURE_RULES.md`](docs/ARCHITECTURE_RULES.md). Read that before
adding a dependency between crates.

The optional native companion is documented in
[`apps/LatchDesktop/README.md`](apps/LatchDesktop/README.md). It invokes the
same JSON CLI contracts as every other client; the CLI remains fully usable
without the app installed.

## Planning

- [`planning/PROJECT_ARCHITECTURE.md`](planning/PROJECT_ARCHITECTURE.md) — the
  product and its boundaries
- [`planning/ENGINE_PLAN.md`](planning/ENGINE_PLAN.md) — current engine sequence
- [`planning/OVERLORD_INTEGRATION.md`](planning/OVERLORD_INTEGRATION.md) — how
  Overlord uses Latch without either product requiring the other
- [`planning/ARCHITECTURE_REVIEW.md`](planning/ARCHITECTURE_REVIEW.md) — the
  analysis that produced this sequence
