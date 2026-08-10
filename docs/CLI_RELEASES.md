# Latch CLI releases

Latch is distributed as one signed macOS binary. The Swift desktop app is an
optional companion and has its own release lifecycle; it is not bundled into
the CLI archive. Keeping the two separate lets terminal-first users install a
small, dependency-free CLI and lets the app ship only when its UX is ready.

## Install

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
`gh attestation verify <archive> --repo Cooperativ/Latch`.

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
just release-cli
```

Set `LATCH_CODESIGN_IDENTITY` to apply local code signing and
`LATCH_NOTARY_PROFILE` to notarize the ZIP with a notarytool keychain profile.
Set `LATCH_RELEASE_TAG=v<version>` to require that a release tag matches the
Cargo version. The generated archive and adjacent `.sha256` file are placed in
`dist/`.
