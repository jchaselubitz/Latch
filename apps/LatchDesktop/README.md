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

Set `LATCH_CODESIGN_IDENTITY` to a Developer ID Application identity to sign
the helper and app during assembly. Production distribution still needs the
usual universal builds, notarization, and stapling.

Release packaging should copy the matching universal `latch` binary into the
app's auxiliary executables directory, then sign and notarize the app and helper
together. The app validates protocol version 1 before its first refresh.

The Swift package tests cover JSON compatibility, manifest wire names, and
terminal command escaping/template parsing.
Run them on macOS with:

```sh
swift test --package-path apps/LatchDesktop
```
