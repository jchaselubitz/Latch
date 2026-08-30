# Latch Desktop

Latch Desktop is the native macOS companion to the standalone `latch` CLI. It
lists, creates, opens, resizes, stops, renames, removes, and prunes local
sessions from a SwiftUI window and a menu-bar extra. It uses the CLI's JSON
contracts; it does not read `~/.latch` directly and does not own session
lifetime.

## Install and set up

Download `Latch.app` for Apple Silicon from the current
[Latch release](https://github.com/jchaselubitz/Latch/releases/latest), drag it
to Applications, and open it. Install the CLI separately with the command in
[Getting started](GETTING_STARTED.md). The app discovers `latch` through the
user's login shell and lets you choose an executable if more than one is found.

Use **File → New Session…** to create a session. In **Latch → Settings…** (or
Command-Comma), choose the terminal Latch uses when it opens an attachment:

- Terminal, iTerm2, and Ghostty are built in.
- Custom lets you select another `.app` and provide its safe argument template.
- Choose New Window or New Tab, plus an optional background launch. Terminal
  and iTerm2 support both shapes; Ghostty and custom terminals use whatever
  their launch arguments produce.

The CLI remains fully usable without the desktop app. A desktop restart or
update does not stop the sessions it displays.

## Updates and diagnostics

Use **Latch → Check for Updates…** or the menu-bar extra to update the app.
The app compares against the GitHub release and accepts an update only when it
is signed by the same Team ID and passes Gatekeeper. It refuses to update a
translocated app or an app in a directory it cannot write; move the app to
Applications first.

The desktop app updates itself only. It can separately check or install a CLI
update when the selected CLI reports `selfUpdate` support. Use `latch doctor`
from Settings or a terminal to diagnose the complete CLI payload.

## Remote access

**Settings → Remote Access** enables the paired-device service. Latch Desktop
starts and supervises `latch remote-access lan-serve`, which in turn owns a
short-lived, loopback-only `latch serve` gateway. The app never holds the
gateway bearer token or makes that plaintext gateway public.

To pair a phone, turn Remote Access on and select **Pair a Device**. The app
creates a five-minute QR-compatible pairing record. If a control-plane address
is configured, it registers the pairing so the mobile app can locate this Mac;
otherwise the address must be supplied to the phone separately. Confirm the
pairing phrase on both devices before using the new device.

Each paired device has an access grant: `observe`, `interact`, or `control`.
Control permits terminal access, which remains exclusive: opening it on the
phone takes the terminal surface from the current Mac viewer. Grant changes
and revocation take effect on active connections.

Remote access requires Latch Desktop to be running and the Mac to be awake.
While a paired device is connected, the app prevents idle sleep; it cannot
prevent a lid close, manual sleep, reboot, or quitting Latch Desktop. The
**Never relay** preference rejects TURN fallback and is useful with a Tailscale
network, but it does not make an offline Mac reachable.

The detailed process, trust boundary, pairing protocol, and audit behaviour are
in [REMOTE_ACCESS_DESKTOP.md](REMOTE_ACCESS_DESKTOP.md).

## Development

Latch Desktop requires macOS 13 or later and Xcode 15 or later:

```bash
swift test --package-path apps/LatchDesktop
apps/LatchDesktop/build-app.sh
open apps/LatchDesktop/.build/release/Latch.app
```

For signing, notarization, and release publishing, use the maintained
[desktop README](../apps/LatchDesktop/README.md) and
[CLI release guide](CLI_RELEASES.md).
