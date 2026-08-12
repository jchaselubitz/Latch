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
latch update                # replace this binary with the newest release
```

Point your terminal profile's command at `latch` and every window you open is
already a persistent session, with nothing about using your terminal feeling
different. That is the intended adoption path, and it is configuration rather
than code.

## Install

Download the standalone CLI and the universal macOS desktop app from the same
[GitHub Release](https://github.com/jchaselubitz/Latch/releases/latest). The
current desktop archive is
[Latch-0.2608121827.0-macos.zip](https://github.com/jchaselubitz/Latch/releases/download/v0.2608121827.0/Latch-0.2608121827.0-macos.zip);
it contains only `Latch.app`. Drag the app to Applications, then install the CLI
independently with:

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The installer chooses the Apple Silicon or Intel archive, verifies it against
the release's `checksums.txt`, verifies its Developer ID signature, and installs
`latch` in `~/.local/bin`. Latch Desktop runs `where latch` on first launch and
lets you choose among every installed copy it finds.

## Updating

`latch update` replaces the running binary with the newest release published
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

M1 is implemented and verified on macOS: local workers own persistent PTYs,
iTerm-compatible clients can detach and reattach, and the management command
surface is live. M2's SSH/phone path is implemented for dogfooding but remains
explicitly non-shippable; see [`docs/M2_FIELD_REPORT.md`](docs/M2_FIELD_REPORT.md).

## Layout

```text
crates/
  latch/                   # the single binary: CLI + worker modes
  latch-protocol/          # framing, control messages, codec
  latch-term/              # screen model + snapshot serialization
apps/LatchDesktop/         # native macOS session manager + menu-bar extra
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
- [`planning/IMPLEMENTATION_PLAN.md`](planning/IMPLEMENTATION_PLAN.md) —
  milestones M1–M6, and the decisions each one rests on
- [`planning/OVERLORD_INTEGRATION.md`](planning/OVERLORD_INTEGRATION.md) — how
  Overlord uses Latch without either product requiring the other
- [`planning/ARCHITECTURE_REVIEW.md`](planning/ARCHITECTURE_REVIEW.md) — the
  analysis that produced this sequence
