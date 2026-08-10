# Latch Desktop

Latch Desktop is the native macOS companion for the standalone `latch` CLI. It
shows, creates, opens, stops, renames, removes, and prunes local sessions from a
SwiftUI window and menu-bar extra. It never reads `~/.latch` directly and does
not own session lifetime.

Use **File → New Session…** to create a session and **Latch → Settings…** (or
Command-Comma) to choose the default terminal Latch uses for attachments.
Terminal, iTerm2, and Ghostty are built-in options. Choose **Custom** to select
any other `.app`; Latch resolves its bundled executable and lets you set the
safe argument template it needs to launch `latch attach`.

## Development

Open `Package.swift` in Xcode 15 or newer and run the `LatchDesktop` scheme on
macOS 13 or newer. Development builds locate `latch` in this order:

1. an auxiliary executable named `latch` in the app bundle;
2. the `latchExecutablePath` user default;
3. `/opt/homebrew/bin/latch` or `/usr/local/bin/latch`.

To assemble a local `.app` containing a same-architecture release helper, run:

```sh
apps/LatchDesktop/build-app.sh
open apps/LatchDesktop/.build/release/Latch.app
```

For a distributable build, use a **Developer ID Application** certificate and
notarize it. Signing alone identifies the developer but does not make a new app
trusted by Gatekeeper. First import the certificate into your keychain and save
Apple-notary credentials in a keychain profile (for example with
`xcrun notarytool store-credentials`). Then run:

```sh
LATCH_CODESIGN_IDENTITY='Developer ID Application: Your Company (TEAMID)' \
LATCH_NOTARY_PROFILE='latch-notary' \
LATCH_APP_ARCHIVE='dist/Latch-macos.zip' \
apps/LatchDesktop/build-app.sh
```

The script signs the bundled `latch` helper before signing the app, submits a
ZIP archive to Apple, staples the accepted ticket to the app, verifies the
signature and Gatekeeper assessment, and recreates the archive with the
stapled app. `LATCH_NOTARY_PROFILE` requires `LATCH_CODESIGN_IDENTITY`.

For a signed local build without notarization, set only
`LATCH_CODESIGN_IDENTITY`. A locally-built ad-hoc or unsigned app will continue
to show Gatekeeper warnings when opened outside a developer workflow.

## Publishing a desktop release

Desktop release publishing deliberately does not change the version. First run
the normal version-bump workflow, review and commit the result. Then supply the
Developer ID identity and notarytool keychain profile and run:

```sh
just release-desktop
```

The script reads `LATCH_CODESIGN_IDENTITY` and `LATCH_NOTARY_PROFILE` from the
ignored repository-root `.env`; environment variables supplied at invocation
time take precedence. Do not commit the `.env` file.

The release script refuses a dirty worktree, notarizes and staples
`dist/Latch-<version>-macos.zip`, creates an annotated `v<version>` tag on the
current commit, then pushes both the current branch and that tag to `origin`.

Release packaging should copy the matching universal `latch` binary into the
app's auxiliary executables directory, then sign and notarize the app and helper
together. The app validates protocol version 1 before its first refresh.

The Swift package tests cover JSON compatibility, manifest wire names, and
terminal command escaping/template parsing.
Run them on macOS with:

```sh
swift test --package-path apps/LatchDesktop
```
