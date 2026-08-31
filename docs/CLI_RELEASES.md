# Latch CLI releases

Latch ships a coordinated, signed macOS payload containing `latch`,
`latch-remote`, and `latchd`. Latch Desktop has an independent release
lifecycle and uses the selected CLI installation.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The installer selects the native Apple Silicon or Intel archive, verifies its
entry in `checksums.txt`, validates the payload manifest and all three code
signatures, checks the three component versions, and installs them in
`~/.local/bin`.

For a manual install:

```bash
shasum -a 256 -c checksums.txt
unzip latch-<version>-<target>.zip
install -m 755 latch latch-remote latchd ~/.local/bin/
latch doctor
```

The ZIP is Developer ID signed and Apple-notarized. GitHub publishes a
provenance attestation for each archive; verify one with
`gh attestation verify <archive> --repo jchaselubitz/Latch`.

## Upgrade and repair

`latch update` verifies the complete archive before the first rename, replaces
`latch-remote` and `latchd` before replacing `latch`, and restores already
replaced siblings if a later rename fails. Existing sessions keep executing
the already-running daemon image while new sessions use the replacement.

```bash
latch update --check
latch update
latch update --force
```

A missing or mixed-version helper makes an otherwise current installation
incomplete, so update repairs it. Package-manager and app-bundle copies are
refused and must be updated by their owner.

## Publishing

1. Set the workspace version in `Cargo.toml` and push the matching `v<version>`
   tag.
2. Configure the Apple signing and notarization secrets named in
   `.github/workflows/release-cli.yml`.
3. The release workflow builds both macOS targets, signs and verifies all three
   binaries, notarizes the archive, verifies its member manifest, publishes
   checksums, and attaches provenance.

For a local same-architecture archive:

```bash
just release-cli
scripts/test-release-archive.sh dist/latch-<version>-<target>.zip <target>
```

The release gate is the real latchd PTY suite plus the complete workspace,
Desktop, gateway, Hub, updater, installer, and release-archive checks. The
historical tmux decisions remain in `planning/` and `docs/DECISION_*`; no tmux
source, patch, binary, or build dependency is part of the current product.

## Rollback boundary

This release cannot operate legacy tmux-hosted sessions because the fallback
binary and subprocess adapter are intentionally gone. If one must be recovered,
install the final dual-kernel release, close or export that session there, then
return to the latchd-only release. Replacing on-disk binaries never terminates
an already-running latchd session.
