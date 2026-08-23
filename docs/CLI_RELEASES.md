# Latch CLI releases

Latch is distributed as one signed macOS binary. The Swift desktop app is an
optional companion and has its own release lifecycle; it is not bundled into
the CLI archive. Keeping the two separate lets terminal-first users install a
small, dependency-free CLI and lets the app ship only when its UX is ready.

## Install

For a verified automatic install into `~/.local/bin`, run:

```bash
curl -fsSL https://raw.githubusercontent.com/jchaselubitz/Latch/main/scripts/install-cli.sh | bash
```

The desktop app offers this same command when `where latch` finds no installed
CLI. To install manually instead, continue below.

Download the archive for your Mac from the GitHub release page:

- Apple Silicon: `latch-<version>-aarch64-apple-darwin.zip`
- Intel: `latch-<version>-x86_64-apple-darwin.zip`

Verify the archive before installing it. The release includes `checksums.txt`:

```bash
shasum -a 256 -c checksums.txt
unzip latch-<version>-<target>.zip
install -m 755 latch ~/.local/bin/latch
install -m 755 latch-remote ~/.local/bin/latch-remote
install -m 755 latch-tmux ~/.local/bin/latch-tmux
latch --version
```

All three binaries are required. `latch-tmux` in particular is **not** stock
tmux and is not an optional system dependency: it is the pinned tmux source
plus Latch's own patch, which provides the exclusive attach the CLI, Desktop,
and gateway all depend on. Latch refuses to create a session or touch an
existing surface when the `latch-tmux` it finds is unpatched, so a partial
install fails closed rather than falling back to ordinary tmux behaviour.

Stop existing sessions before installing. Replacing the payload does not
restart a tmux server that is already running, and Latch refuses to attach
through a server that predates this release rather than falling back to an
ordinary tmux attach — so an upgrade over live sessions is refused with an
instruction, not silently downgraded.

There is no mixed-version operation. The CLI, Desktop, gateway, mobile
contract, and `latch-tmux` ship together and are upgraded together; an older
component paired with a newer one is refused rather than adapted.

Ensure `~/.local/bin` is on your `PATH`. The ZIP is Developer ID signed and
Apple-notarized; GitHub also publishes a provenance attestation for each
archive in the release workflow. Verify it with
`gh attestation verify <archive> --repo jchaselubitz/Latch`.

## Upgrading

After the first install, `latch update` does the same work in one command: it
reads the newest release, downloads the archive for this Mac, verifies it
against the release's own `checksums.txt`, and replaces `latch`, `latch-remote`,
and `latch-tmux` with an atomic rename of each. `latch update --check` reports
what is available without installing it, and `latch update --force` reinstalls
the published version over a copy that has gone wrong. A current CLI that is
missing `latch-remote` or `latch-tmux` is treated as incomplete and repaired.

Two things it deliberately will not do. It refuses a binary something else
owns, such as a Homebrew cellar copy. And when the installed binary is
Developer ID signed, it refuses a download that is not validly signed by the
same team, because the archive and the checksums that describe it come from the
same place and only the signature says who built them. Latch Desktop never
bundles or updates the CLI, so an app update cannot replace the selected CLI.

`latch capabilities --json` reports `selfUpdate`, so a client can tell whether
offering an in-place update would work before offering it.

## Publishing a release

1. Set the workspace version in `Cargo.toml`, then create and push the matching
   tag: `v<version>`.
2. Configure these GitHub Actions secrets:
   `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`,
   `APPLE_SIGNING_IDENTITY`, `KEYCHAIN_PASSWORD`, `APPLE_ID`,
   `APPLE_APP_SPECIFIC_PASSWORD`, and `APPLE_TEAM_ID`.
3. Pushing the tag runs the release workflow. It builds each native macOS
   target, signs the binary, submits its ZIP to Apple notarization, creates a
   SHA-256 checksum, attests the archive, and creates the GitHub Release.

For a local, same-architecture archive, run:

```bash
brew install libevent
just release-cli
```

Set `LATCH_CODESIGN_IDENTITY` to apply local code signing and
`LATCH_NOTARY_PROFILE` to notarize the ZIP with a notarytool keychain profile.
Set `LATCH_RELEASE_TAG=v<version>` to require that a release tag matches the
Cargo version. The generated archive and adjacent `.sha256` file are placed in
`dist/`.

## Building the patched session kernel

`latch-tmux` is built by `scripts/build-tmux.sh`, which is the only place that
knows how to produce it. `scripts/release-cli.sh` and the release workflow both
call it, so the kernel that ships and the kernel the conformance suites run
against are built the same way.

```bash
scripts/build-tmux.sh dist/latch-tmux
```

It reads `patches/tmux/manifest.json` for the pinned upstream version, its
SHA-256, and every Latch patch with its own checksum. It then verifies the
source archive, applies each patch with `patch -F 0` so a hunk that no longer
matches is a build failure rather than a silently relocated edit, links
libevent and utf8proc statically, and refuses to emit a binary that does not
accept the raw-attach flag. A patch that needs rebasing, a changed upstream
tarball, or a build that produced stock tmux all stop the release here.

### Updating the pinned tmux

1. Change `upstream.version`, `upstream.url`, and `upstream.sha256` in
   `patches/tmux/manifest.json`.
2. Rebase the patch until it applies with zero fuzz, then record its new
   SHA-256 in the manifest. Regenerate it as a single diff of pristine against
   patched source; a patch assembled from separately generated hunks will apply
   only with fuzz, which hides a hunk landing in the wrong place.
3. Run the kernel conformance and soak gates below against the rebuilt binary.

## Release gates

Run these against the binaries extracted from the archive that will actually be
published, not against a working-tree build.

```bash
# Kernel primitive: snapshot/raw boundary, byte identity, query ownership,
# steal ordering, slow-client eviction, tty restoration.
LATCH_TMUX_PHASE0_BIN=<latch-tmux> python3 scripts/test-latch-tmux-phase0.py

# End to end: the real CLI and gateway over real PTYs, including local steal,
# WebSocket steal and steal-back, racing owners, resize ownership, pane exit,
# eviction, and old-kernel rejection. Must run serially.
LATCH_E2E_TMUX_BIN=<latch-tmux> \
    cargo test -p latch --test exclusive_attach_e2e -- --test-threads=1

# Soak: full-screen redraw at desk geometry, an agent blocked on a prompt
# through long idle periods, and repeated desk/phone steals.
scripts/soak-exclusive-attach.py --tmux <latch-tmux> --latch <latch> \
    --minutes 20 --steals 1000
```

The end-to-end suite skips itself when `LATCH_E2E_TMUX_BIN` is unset, so a
plain `cargo test` does not need a built kernel. That is a convenience for
development, not a licence to skip it before a release.
