# Latch Desktop App Plan

Status: proposed  
Mission: coo:672  
Scope: local macOS session management and terminal launching  
Deferred: the chat surface planned in coo:620

## Decision

Build a small native macOS application in SwiftUI, with AppKit only where macOS
integration requires it. The first release is a companion to the existing Rust
`latch` binary, not a replacement for it and not a second implementation of the
session service.

The app invokes the existing machine-readable CLI commands and decodes their
JSON responses. It does not read or modify `~/.latch/sessions` directly, keep a
parallel registry, embed the Rust crates through FFI, or sit on the terminal
data path. A terminal window still runs `latch attach <session>`, so closing the
app has no effect on any session or attachment.

Swift is a better fit than Tauri for this scope because:

- Latch currently targets macOS, and the requested menu-bar and terminal-app
  integrations are macOS-native concerns.
- SwiftUI provides the window, settings, confirmation dialogs, and menu-bar
  extra without shipping a browser runtime or adding a frontend build system.
- Rust compatibility does not require an in-process Rust host. The CLI's
  `--json` contract was explicitly designed for external callers and is the
  safer boundary.
- A Tauri shell would become more compelling only when Latch commits to a
  cross-platform desktop UI or needs to reuse a substantial web presentation
  layer. The future chat UI can be embedded later without changing the session
  management boundary.

## Product boundaries

### Included in the first release

- A compact main window showing all local sessions, most recently active first.
- Search and filters for running, exited, and lost sessions.
- Session details: name, title, command label, working directory, state, last
  activity, size, exit result, and current attachments.
- Create a shell session, with an advanced option to run a command.
- Rename, stop, force-stop, remove, and prune sessions.
- Open a session in the user's selected terminal application.
- A menu-bar extra that works while the main window is closed.
- Installation diagnostics and clear recovery actions when the bundled CLI or
  Latch home is unavailable.

### Explicitly excluded

- An embedded terminal emulator.
- Chat or harness-specific presentation from coo:620.
- Overlord mission state or connector lifecycle.
- Remote/cloud session discovery.
- A resident registry daemon or database.
- Windows and Linux desktop packages.

## Architecture

```text
SwiftUI window + menu-bar extra
            |
            v
      SessionStore (UI state)
            |
            v
       LatchClient actor
            |
            | Process + JSON over stdout/stdin
            v
 bundled `latch` binary --------> ~/.latch/sessions + worker sockets
            |
            | `latch attach <id>` in a new terminal window
            v
 iTerm / Terminal / Ghostty / configured terminal
```

### Repository layout

Add the native app under a new top-level directory without changing the Rust
crate dependency graph:

```text
apps/LatchDesktop/
  LatchDesktop.xcodeproj
  LatchDesktop/
    App/
    Models/
    Services/
    Features/Sessions/
    Features/Settings/
    TerminalLaunchers/
  LatchDesktopTests/
  LatchDesktopUITests/
```

The release build embeds the matching universal `latch` executable in the app
bundle. App operations always use that known-compatible binary by default. A
developer setting may point at an external binary, but the app must validate it
with `latch capabilities --json` before use. The CLI continues to be packaged
separately and remains fully usable with no app installed.

### CLI boundary

Implement `LatchClient` as a Swift actor so only one owner starts processes,
captures output, applies timeouts, and maps failures into typed errors. Use
`Process` with an explicit executable URL and argument array; never invoke a
shell to construct management commands.

Use the existing contracts:

| App action | CLI contract |
| --- | --- |
| Refresh sessions | `latch list --json` |
| Load details | `latch inspect <id> --json` |
| Create | `latch create --manifest-file - --json` with JSON on stdin |
| Rename | `latch rename <id> <name> --json` |
| Stop | `latch stop <id> --json` |
| Force-stop | `latch stop <id> --force --json` |
| Resize | `latch resize <id> --cols N --rows N [--pin] --json` |
| Preview prune | `latch prune --dry-run --json` |
| Prune | `latch prune --json` or `--all` after confirmation |
| Diagnose | `latch doctor --json` and `latch capabilities --json` |

Creation uses manifest format version 1 and sends it over stdin. The UI never
places environment values or launch material in the `latch` process arguments,
logs, analytics, or error reports.

### Required CLI addition: targeted removal

The current CLI can stop one session and prune all eligible exited/lost
sessions, but cannot remove one selected session. Add this public command before
wiring the Remove button:

```text
latch remove <session> [--force] --json
```

Its behavior should be:

- Refuse to remove a creating, running, or stopping session unless `--force`
  was explicitly supplied.
- With `--force`, request the worker to stop its own process group, wait for the
  terminal state, and only then remove the session directory.
- Remove an exited or lost session directly through the Rust-owned filesystem
  abstraction.
- Return `{ "id": "...", "removed": true }` on success.
- Preserve the existing rule that no caller reads a stored PID or deletes a
  session directory behind a live worker.

The desktop app must never emulate this by deleting `~/.latch` content itself.

## User experience

### Main window

Use a simple two-column SwiftUI layout:

- Sidebar/list: state indicator, display name, optional title, command label,
  and relative last activity.
- Detail pane: metadata and attachments followed by the primary Open in
  Terminal action and a compact action menu.

Running sessions are visually primary. Exited sessions remain visible and
attachable read-only until removed or pruned. Lost sessions show a diagnostic
state rather than looking like stopped sessions.

Destructive actions require confirmation that states the effect precisely:

- Stop ends the child process but retains the session's final screen.
- Remove deletes the retained screen and metadata.
- Prune shows the dry-run result before executing.

After every action, refresh immediately and preserve the current selection when
the selected session still exists.

### Creating sessions

The default New Session sheet creates a login shell and asks only for:

- optional name;
- optional working directory;
- optional title.

An Advanced disclosure adds a command field and initial terminal size. Resolve
the account's login shell from the local user record rather than trusting the
GUI app's `SHELL` environment. Use `inherit_env: true`, with a login shell as
the normal way to establish the user's interactive environment. Mark the
manifest source as `desktop`.

Creation returns without attaching. On success, select the new session and,
when the user chose “Create and Open,” launch the preferred terminal as a
separate best-effort step. A terminal launch failure must not stop or remove the
new session.

### Preferred terminal

Define a small `TerminalLauncher` protocol with one implementation per
supported app. Discover installed applications through `NSWorkspace`; store the
selected bundle identifier in app preferences.

Ship tested adapters for:

- iTerm2;
- Apple Terminal;
- Ghostty.

Add a Custom option using a user-authored argument template with explicit
placeholders for the executable path and session ID. Parse the template into an
argument array; do not pass it through `/bin/sh -c`.

Each adapter opens a new window or tab that runs the bundled executable's
equivalent of:

```text
latch attach <session-id>
```

Prefer supported command-line or URL interfaces. Use Apple Events only where a
terminal offers no reliable argument-based launch, and surface the macOS
Automation permission failure with a link to the relevant System Settings
pane. Quote and escaping behavior must be covered by adapter tests, including
app bundle paths containing spaces.

The app does not count as an attachment merely because it lists a session.

### Menu-bar extra

Use SwiftUI `MenuBarExtra` backed by the same `SessionStore` as the main window.
The menu shows:

- count of running sessions in the label or first row;
- the most recently active sessions with state and Open action;
- New Session;
- Refresh;
- Prune preview/action;
- Open Latch and Quit.

The app remains active when its last window closes. It is not a daemon and does
not own session lifetime; quitting it stops only UI refreshes.

Refresh on app activation, after actions, and on a modest timer while the app is
running. Start with a five-second interval. Filesystem watching alone is not
sufficient because live state is derived from worker socket responses and may
change without a useful directory event. Coalesce overlapping refreshes and
keep displaying the last good snapshot if a refresh fails.

## Error handling and security

- Decode stdout only when the process exits successfully. Preserve bounded
  stderr for a user-facing diagnostic, but redact manifest content and never
  log stdin.
- Put a timeout on noninteractive management commands and terminate only the
  stuck client process, never a session worker.
- Treat unknown JSON fields as forward-compatible. Treat missing required
  fields and unknown session states as an app/CLI compatibility error with a
  useful version report.
- Use session IDs, not mutable names, for actions after selection.
- Disable App Sandbox for the initial Developer ID distribution: the app must
  execute the bundled helper, inspect local user state through that helper, and
  launch terminal applications. Enable Hardened Runtime, sign the app and
  helper together, and notarize releases.
- Do not request Full Disk Access. Do not read session journals or socket files
  from Swift.
- Do not add analytics in the first release; names, cwd values, command labels,
  and terminal output can be sensitive.

## Delivery phases

### Phase 0 — stabilize the app-facing CLI contract

1. Add `latch remove --json` and its typed response.
2. Add fixture tests for all JSON shapes the Swift app decodes, including
   malformed/partial responses and every session state.
3. Document the minimum compatible product/protocol version.
4. Produce a universal macOS `latch` binary suitable for app embedding.

Exit: every app operation is possible through a supported CLI command, with no
direct session-directory mutation.

### Phase 1 — native shell and read-only session browser

1. Create the SwiftUI app, main window, menu-bar extra, and shared store.
2. Implement capability checks, list refresh, session models, filters, search,
   empty states, and diagnostics.
3. Add inspect/details and attachment display.
4. Establish signing, bundle-helper copying, and local release builds.

Exit: the app and menu-bar extra accurately show running, exited, and lost
sessions; quitting either has no effect on them.

### Phase 2 — terminal launch and creation

1. Add terminal discovery, preference UI, and the three launch adapters.
2. Add the New Session sheet and stdin manifest creation.
3. Add Create and Create and Open flows with independent error reporting.
4. Add a first-run screen for terminal selection and CLI diagnostics.

Exit: a user can create a session and open any listed session in their selected
supported terminal, while the standalone CLI remains unchanged and usable.

### Phase 3 — management and polish

1. Add rename, stop, force-stop, targeted remove, resize, and prune preview.
2. Add confirmations, progress states, keyboard shortcuts, and accessibility
   labels.
3. Add launch-at-login as an opt-in setting using `SMAppService`.
4. Exercise stale sockets, CLI timeouts, rapid actions, terminal permission
   denial, and app upgrades with live workers.

Exit: all scoped management operations are safe, recoverable where possible,
and available from the main app; common Open/New actions are available from the
menu bar.

### Phase 4 — release readiness

1. Notarize a universal app and verify on clean Apple Silicon and Intel Macs.
2. Verify migration across an app upgrade while sessions from the previous
   version remain alive.
3. Publish CLI/app compatibility and troubleshooting documentation.
4. Run a small dogfood period before making launch-at-login discoverable by
   default.

Exit: a clean Mac can install, create, manage, reopen, and prune sessions with
no developer tools present.

## Verification strategy

### Rust contract tests

- Existing list, inspect, create, stop, rename, resize, prune, doctor, and
  capabilities JSON schemas remain stable.
- New remove tests cover exited, lost, live-refusal, force-stop, nonexistent,
  ambiguous name, and concurrent worker exit cases.
- App-driven creation proves manifest data travels over stdin and does not
  appear in `latch` argv or stored metadata.

### Swift unit tests

- Decode representative JSON for every state and report type.
- Verify command arguments and stdin without executing a shell.
- Verify timeout, cancellation, nonzero exit, invalid JSON, and version mismatch
  mappings.
- Verify refresh coalescing and stale-result ordering in `SessionStore`.
- Verify every terminal adapter with spaces and punctuation in paths/IDs.

### Integration and UI tests

- Run the built Rust helper against an isolated `LATCH_HOME` and exercise the
  real Swift client.
- Create, list, inspect, rename, stop, remove, and prune through the app.
- Close the window, quit the app, relaunch it, and prove the worker survived.
- Launch each supported terminal and verify its child command is an ordinary
  `latch attach` client.
- Verify VoiceOver names, keyboard navigation, reduced motion, light/dark mode,
  empty state, and a list of at least 100 sessions.

## Release acceptance criteria

- The main window and menu-bar extra show the same complete local session list.
- Session state is obtained from the CLI and never persisted by the app as
  authority.
- Create, rename, stop, force-stop, targeted remove, and prune succeed with
  clear confirmations and errors.
- Open launches the selected terminal without restarting or transferring
  ownership of the underlying process.
- Terminal-launch failure never terminates a session.
- Closing or upgrading the app leaves existing sessions alive and attachable
  from the standalone CLI.
- No launch manifest, environment block, terminal output, or scrollback is
  written by the app or included in logs.
- The signed/notarized release works without Xcode or a separate Rust toolchain.

## Tradeoffs and follow-on work

- Native Swift keeps the first release small and excellent on macOS, at the
  cost of not creating a reusable Windows/Linux shell. Reassess Tauri only when
  cross-platform desktop support becomes an active objective.
- Calling a subprocess is less direct than Rust FFI, but preserves the public
  CLI boundary, avoids ABI and signing complexity, and dogfoods the same API
  other integrations depend on.
- A five-second poll is deliberately simple and may perform unnecessary probes.
  Measure it before considering notifications or a resident service.
- Terminal applications do not share one launch API. Keep adapters isolated and
  test them against supported terminal versions rather than hiding differences
  in an opaque shell command.
- When coo:620 begins, add chat as another presentation feature backed by the
  worker protocol or its intended client library. Do not route terminal bytes
  through `LatchClient`, change the CLI management boundary, or make the chat
  view authoritative over the PTY.
