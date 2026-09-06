# Field check: starting a session from the phone

**Status:** not yet run.
**Feature:** [`FEATURE_MOBILE_SESSION_CREATION.md`](FEATURE_MOBILE_SESSION_CREATION.md).
**Plan:** [`PLAN_MOBILE_SESSION_CREATION.md`](PLAN_MOBILE_SESSION_CREATION.md),
phase 8.

Everything about this feature that a test can decide has been decided. The
gateway routes, the idempotency key, the grant checks, the browser state
machine, and the model's availability rules are covered by Rust and Swift tests
that run in CI, and the list at the bottom says which. What remains is what no
test in this repository can establish, because it is about a real phone on a
real network talking to a real Mac: a Face ID prompt, a relayed tunnel, a
suspended app, and a `UserDefaults` value that survives a relaunch.

This document is the checklist for that run, and the place its result is
recorded. Until the status line above says otherwise, treat the physical-device
behavior as unverified.

## Setup

- One iPhone running the app from `App/LatchMobile.xcodeproj` on a device, not
  the simulator: the owner check needs real biometrics, and the simulator
  shares the Mac's network, which erases the difference between the three
  paths.
- One Mac running Latch Desktop with Remote Access enabled, paired with that
  phone at `control`.
- A folder on the Mac nested several levels below home, with a Unicode
  component and a long name, plus one directory holding more than 200
  subfolders. `mkdir -p` is enough for the second.
- Something that separates the three paths. Same Wi-Fi gives the local network;
  cellular with the Mac on Wi-Fi usually gives direct ICE; a network that
  filters UDP gives the relay. `docs/REMOTE_ACCESS_FIELD_VERIFICATION.md`
  describes how to read which one was actually used, from Settings → Path on
  the phone and `latch remote-access diagnostics` on the Mac.

## The checks

**1. Save a nested default.** Settings → New sessions → Default folder. Confirm
the owner check is asked for before any folder name appears. Navigate down to
the nested Unicode folder, tap **Use as default**, and confirm the Settings row
shows that absolute path. Force-quit the app and reopen it: the row must still
show it.

**2. Start a session on each path.** Sessions → **New session**. It must open
at the saved default, not at home. Tap **Start session here**. Repeat once on
the local network, once on direct ICE, and once over the relay, recording which
path Settings reported for each. On every path the sheet must dismiss and the
list must refresh.

**3. Check what was created, on the Mac.** For each of the three, `latch list`
must show exactly one new session, its `cwd` must be the folder that was
chosen, and it must be unattached — nothing on the Mac may have changed hands,
and no window or pane may have moved. `latch list --json` carries the working
directory; the desktop's session list carries the attach state.

**4. Open it and run something.** Tap the new row. It must open the terminal
because that was asked for, not because creation did it. Type `claude` or
`codex` by hand and confirm it starts. Creation must never have launched an
agent on its own.

**5. Repeat after moving the default, and after losing a response.** Move or
delete the saved default folder on the Mac, then open **New session** again:
the app must say once that the saved folder is unavailable, open at the Mac's
home directory, and let a new default be saved. Then, with a session about to
be created, drop the response — airplane mode the moment **Start session here**
is tapped is the crude version, and killing the tunnel is the precise one.
Restore the connection and tap **Start session here** again without changing
folders. The list must end with **one** session, not two: the retry carries the
same request id and the Mac returns what it already made.

**6. Downgrade the grant.** On the Mac, set the phone to Interact, then to
Observe. With the picker already open, confirm the next navigation refuses and
says why rather than showing a stale listing that quietly does nothing. Close
and reopen Sessions: **New session** must be visible but refuse, and Settings →
Default folder must refuse too. Neither grant may retrieve a folder name or
create a session — check `latch remote-access diagnostics` afterwards to
confirm no creation was authorized.

## Recording a run

Add a dated section below with the app build, the Latch version on the Mac, the
three paths as reported, and one line per check. A check that did not pass gets
the observed behavior, not an interpretation of it. Then update the status line
at the top.

## What is already measured, and where

Named here so the field checks are not asked to re-prove them.

- **Bounded, folders-only, canonical directory pages, and one stable path
  error.** `crates/latch/src/cli/serve/directory.rs` and its tests: symlink
  resolution, case-insensitive ordering, 200-entry pages, opaque
  stale-detecting cursors, and the single refusal every unusable path collapses
  to.
- **One shell per request id, under concurrency.** Unit tests on the create
  boundary in `crates/latch/src/cli/create.rs`: a lost-response retry, a
  conflicting reuse, eight concurrent requests carrying one id collapsing to a
  single launch, and a failed launch leaving nothing behind while still
  allowing a retry.
- **The created session is a plain unattached login shell in the named
  directory.** The kernel parity test in
  `crates/latch/tests/latchd_kernel_e2e.rs` creates one over a real `latch
  serve` gateway and asserts the cwd, the 80x24 geometry, the unattached
  surface, and that a retry returns the same id.
- **Observe and interact are refused at the route.** Router and paired-proxy
  tests in `crates/latch/src/cli/serve/routes.rs` and
  `crates/latch/src/cli/remote_access.rs`.
- **An older gateway advertises neither route and the control stays hidden.**
  `GatewayV2Tests.swift` and `NewSessionAppModelTests.swift`.
- **The browser's behavior: home fallback, parent navigation, pagination,
  inline retry, one-off selection not overwriting the default, a grant
  downgrade refusing the next request, and a retry reusing its UUID while a
  folder change or a cancel starts a new intent.**
  `NewSessionFolderTests.swift` and `NewSessionAppModelTests.swift`.
- **Creation refreshes the list and opens no terminal.**
  `testCreationRefreshesSessionsHighlightsResultAndDoesNotOpenATerminal` in
  `NewSessionAppModelTests.swift`, which fails the test if a terminal
  connection is attempted at all.
