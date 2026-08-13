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
latch --version
```

Ensure `~/.local/bin` is on your `PATH`. The ZIP is Developer ID signed and
Apple-notarized; GitHub also publishes a provenance attestation for each
archive in the release workflow. Verify it with
`gh attestation verify <archive> --repo jchaselubitz/Latch`.

## Upgrading

After the first install, `latch update` does the same work in one command: it
reads the newest release, downloads the archive for this Mac, verifies it
against the release's own `checksums.txt`, and replaces the running binary
with an atomic rename. `latch update --check` reports what is available
without installing it, and `latch update --force` reinstalls the published
version over a copy that has gone wrong.

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
