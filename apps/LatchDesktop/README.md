# Latch Desktop

Latch Desktop is the native macOS companion for the standalone `latch` CLI. It
shows, creates, opens, resizes, stops, renames, removes, and prunes local
sessions from a SwiftUI window and menu-bar extra. It never reads `~/.latch`
directly and does not own session lifetime.

Use **File → New Session…** to create a session and **Latch → Settings…** (or
Command-Comma) to choose the default terminal Latch uses for attachments.
Terminal, iTerm2, and Ghostty are built-in options. Choose **Custom** to select
any other `.app`; Latch resolves its bundled executable and lets you set the
safe argument template it needs to launch `latch attach`.

Settings also chooses whether a session opens as a **New Window** or a **New
Tab**, and whether it **opens in the background** instead of taking focus. The
**Open in …** control is a split button: clicking it uses that default, and its
menu opens a single session the other way without changing the setting. Terminal
and iTerm2 support both shapes; Terminal has no scripting command for tabs, so a
tab is opened with a Command-T keystroke, which asks for Automation access to
System Events once and falls back to a new window if that is refused. Ghostty
cannot be told to open a tab from another app, and a custom terminal is launched
entirely by its argument template, so both always open whatever their launch
arguments produce. Background opens leave the terminal behind the app you were
using; a Terminal tab may flash briefly so Command-T can run.

## Remote access

Settings → Remote Access turns on a paired-phone gateway: the app spawns a
helper that owns a loopback-only `latch serve` gateway and a WebRTC ICE
responder, and lets a paired iPhone reach it either on the LAN or off it. Each
paired device's row separates two decisions — a base Observe/Interact picker
and an explicit "Allow terminal" switch mapped to the `control` permission —
and a change to either takes effect immediately, including on a connection
that is already open. A "never relay" switch pairs with
`latch remote-access relay disable` to keep TURN out of the offer entirely and
restrict the Mac's published presence to host candidates, which is what makes
a Tailscale/tailnet address a working path with the relay refused outright.

While Remote Access is on and at least one phone is connected, the app holds
an `IOPMAssertion` to prevent idle sleep, released the moment the last phone
disconnects or the feature is turned off. That narrows, but does not remove,
the hard constraint underneath all of this: **the phone can only reach a Mac
that is running Latch Desktop and awake.** There is no server component
independent of this app. `docs/REMOTE_ACCESS_DESKTOP.md` in the Latch
repository is the full design record — process boundaries, pairing, the
control-plane mirror, and what each side of the audit trail records.

## Development

Open `Package.swift` in Xcode 15 or newer and run the `LatchDesktop` scheme on
macOS 13 or newer. On first launch the app asks the user's login shell to run
`where latch`, presents every executable path it returns, and saves the path the
user chooses. When no CLI is found, the setup flow presents the standalone CLI
install command and can run it in Terminal. The same discovery, selection, and
installation controls remain available in Settings. Settings also shows the
selected CLI and private tmux versions, reports `latch doctor` findings, and can
check for or install a CLI update when that installation advertises
`selfUpdate` support.

To assemble a local Apple Silicon `.app` with no bundled CLI, run:

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

The script signs the Apple Silicon app, submits a ZIP archive to Apple, staples the
accepted ticket to the app, verifies the signature and Gatekeeper assessment,
and recreates the archive with the stapled app. The ZIP contains only
`Latch.app`; `LATCH_NOTARY_PROFILE` requires `LATCH_CODESIGN_IDENTITY`.

For a signed local build without notarization, set only
`LATCH_CODESIGN_IDENTITY`. A locally-built ad-hoc or unsigned app will continue
to show Gatekeeper warnings when opened outside a developer workflow.

## Updating in place

Latch Desktop updates itself from the same GitHub release the CLI is published
to. **Latch → Check for Updates…** (also in the menu-bar extra) compares
`CFBundleShortVersionString` with the newest release and offers the
`Latch-<version>-macos.zip` attached to it. Settings has a *Check for updates
automatically* toggle; automatic checks run at launch and once a day, and only
open a window when something is actually available.

Installing expands the archive with `ditto` into a replacement directory on the
app's own volume, requires the download to be signed by the same Team ID as the
running app and to pass Gatekeeper assessment, then swaps the bundle with
`replaceItemAt`. **Install and Relaunch** closes the update sheet, quits Latch,
and reopens the replacement through normal Launch Services once the old process
has exited. Sessions are owned by their workers, so they keep running across the
relaunch.

Two cases are refused before anything is downloaded: an app running from a
quarantine translocation mount (move it to Applications first) and an app in a
directory the user cannot write.

`build-app.sh` stamps the workspace version from `Cargo.toml` into the bundle
before signing it. A bundle built without that stamp reports the placeholder
version in `Info.plist` and would offer itself an update forever.

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

The release script refuses a dirty worktree, removes any previous local
`apps/LatchDesktop/.build/release/Latch.app` bundle, notarizes and staples
`dist/Latch-<version>-macos.zip`, creates an annotated `v<version>` tag on the
current commit, then pushes both the current branch and that tag to `origin`.

Pushing the tag is what starts the CLI release workflow, and that workflow is
what creates the GitHub Release. The desktop archive is notarized on this
machine rather than in CI, so the script then waits for the release to appear
and attaches `Latch-<version>-macos.zip` with `gh`. It adds the desktop digest
to the release's `checksums.txt` instead of publishing a fifth sidecar asset.
Without that attachment the in-app updater has nothing to find; if the upload
does not happen the script prints the recovery commands.

A complete release contains exactly the Apple Silicon desktop ZIP, the two
architecture-specific CLI ZIPs, and `checksums.txt`. The CLI still ships Intel
and Apple Silicon archives; the desktop app does not. The CLI is installed
independently. The app validates protocol version 1 and requires CLI version
`0.2608132217.0` or newer before its first refresh; this is the first release
whose tmux-backed list/inspect contracts match the desktop models. A self-update
initiated in Settings delegates to `latch update`, which atomically replaces
the CLI, the remote-access helper, and its pinned tmux payload.

The Swift package tests cover the current list, stop, resize, doctor,
capabilities, and update JSON contracts; manifest wire names; terminal command
escaping/template parsing; the per-terminal window/tab support matrix and its
new-window fallback; background-launch AppleScript that omits `activate`; and
the updater's release resolution, version ordering, and pre-download refusals.
Run them on macOS with:

```sh
swift test --package-path apps/LatchDesktop
```
