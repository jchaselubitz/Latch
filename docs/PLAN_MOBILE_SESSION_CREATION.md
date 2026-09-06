# Plan: mobile folder picker and remote session creation

**Status:** all four slices delivered. The repository gates in phase 8 pass;
the live paired-path checks at the end of it are recorded, still to run, in
[`FIELD_CHECK_MOBILE_SESSION_CREATION.md`](FIELD_CHECK_MOBILE_SESSION_CREATION.md).
**Feature:**
[`FEATURE_MOBILE_SESSION_CREATION.md`](FEATURE_MOBILE_SESSION_CREATION.md).
**Scope:** the protocol-major-2 schema, the Rust gateway and engine boundary,
and `apps/LatchMobile`.

## Design summary

Add two independently advertised, control-granted gateway operations:

```text
GET  /v2/directories?path=<absolute-path>&cursor=<opaque>
POST /v2/sessions
```

The phone uses the first to navigate the Mac's filesystem and the second to
create a standard Latch shell in the selected directory. Both routes travel
through the existing HTTP gateway, paired Noise tunnel, and remote request
authorization. No control-plane or relay API changes are required.

The creation request is intentionally small:

```json
{
  "requestId": "8cba5d78-79a0-4a55-9047-f77e57e463c7",
  "cwd": "/Users/jake/Development/Cooperativ/Latch"
}
```

The response reuses the existing `CreateReport` shape returned by
`latch create --json`. The gateway supplies the shell, environment, display
metadata defaults, and initial terminal size.

## Phase 1 — define and generate the wire contract

Add canonical request and response definitions under
`schemas/remote-access/v2/` for:

- a directory page containing `path`, nullable `parent`, directory `entries`,
  and nullable `nextCursor`;
- a directory entry containing a display `name` and canonical absolute `path`;
- a create-session request containing a UUID `requestId` and absolute `cwd`;
  and
- the existing create report fields needed by the phone.

Extend `gateway-capabilities.schema.json` with optional
`endpoints.browseDirectories` and `endpoints.createSession` booleans. Update
the generator so both become cases of `GatewayEndpointsName`, default to
`false` when absent, and participate in the normal `isEnabled` check.

Regenerate the vendored schemas, manifest, and
`Generated/LatchContract.swift`. Update schema fixtures and compatibility tests
to prove that current documents advertise both routes and pre-feature
documents still decode with both disabled.

Keep creation and browsing as separate flags. A future policy may allow
creation from known locations without permitting arbitrary browsing, and a
partially upgraded gateway must describe what it actually serves.

## Phase 2 — add the gateway routes and authorization

Extend `crates/latch/src/cli/serve/routes.rs` with:

- `GET /v2/directories`, requiring `Grant::Control`; and
- `POST /v2/sessions`, requiring `Grant::Control`.

Register both in `cli/serve/http.rs`, advertise them from capabilities, and
add `POST` to the CORS allow-methods response. The paired proxy already
understands bounded requests with `Content-Length`; extend its route-table
tests to prove it forwards these two operations at control and rejects them at
observe or interact. Continue rejecting transfer encoding, duplicate content
lengths, pipelining, caller-supplied authorization, and oversized initial
requests.

Use the gateway's existing JSON error envelope with stable error codes for an
invalid path, unavailable path, unreadable directory, stale cursor, request-ID
conflict, and session creation failure. Do not expose raw filesystem or engine
errors to the phone.

## Phase 3 — implement bounded directory browsing

Create a small filesystem boundary under `crates/latch/src/cli/serve/` rather
than placing traversal logic in the Axum handler. It should:

1. Treat an omitted `path` as the current user's home directory.
2. Require an absolute path with bounded UTF-8 input length.
3. Canonicalize the path and verify that it is an accessible directory.
4. Enumerate directories only, resolving each returned path on the Mac.
5. Sort case-insensitively with a deterministic bytewise tie-breaker.
6. Return at most 200 entries per page with an opaque continuation cursor.
7. Derive `parent` from the canonical path and return `null` at the filesystem
   root.

Unreadable children may be listed but opening one must return the stable
unreadable-directory error. A single broken symlink or racing deletion must
not fail the whole parent listing. Do not inspect file contents or recursively
walk the tree.

Unit-test home resolution, canonicalization, parent navigation, symlinks,
Unicode names, deterministic pagination, permission errors, disappearing
entries, and response bounds using temporary directories.

## Phase 4 — create exactly one standard shell

Implement the POST handler by decoding and validating the bounded JSON body,
then canonicalizing `cwd` again immediately before creation. Build the manifest
with `create::shell_manifest`; do not accept an argv, environment, shell,
display metadata, or terminal type from the request. Use the same standard
initial size as a local desktop-created shell so the unattended pane begins in
a known geometry.

Set launch provenance to:

```text
source.kind = "mobile"
source.external_run_id = requestId
```

Use that persisted, opaque correlation ID for idempotency. Under a Latch-home
creation lock, scan session metadata before creating:

- an existing `mobile` session with the same request ID and canonical `cwd`
  returns its create report;
- the same request ID paired with another `cwd` returns `409
  request_id_conflict`; and
- no match proceeds through the existing `create::create_session` path.

The lock must cover lookup through durable metadata creation so concurrent
requests and a retry after a lost response cannot create duplicates. Keep the
idempotency logic at the create boundary rather than in the mobile client; only
the Mac can know whether a process started.

Test a successful shell launch, invalid and inaccessible paths, a request ID
retry, conflicting reuse, concurrent duplicate requests, launch failure, and a
lost-response simulation. Assert that no attach is spawned and no existing
session surface changes.

## Phase 5 — add the mobile gateway client

Extend `LatchGateway` with typed methods to:

- fetch the first or next directory page using `URLComponents` for query
  encoding; and
- POST a create request as JSON and decode `CreateReport`.

Refactor the private GET-only helper into a bounded typed request helper that
preserves the current authorization behavior and error mapping. The manual
HTTPS route and paired loopback route must continue to use the same client.

Add gateway tests for discovery gating, path encoding, pagination, request
encoding, decoding, permission errors, stable path errors, and a response lost
after creation. The phone generates one UUID when the user presses Start and
retains it across retries until that attempt resolves or the user cancels it.

## Phase 6 — model the default folder and creation state

Add a `NewSessionFolderStoring` seam beside the existing presentation and
terminal-size preference stores:

- `UserDefaultsNewSessionFolderStore` for the app;
- an in-memory store for tests; and
- `nil` as the unsaved value, meaning the Mac user's home directory.

Add an isolated folder-browser model that owns the current page, navigation
stack, pagination, loading state, error, selection mode, and pending creation
request ID. Keep this out of `SessionsView` so navigation and retry behavior
can be tested without SwiftUI.

`AppModel` should expose capability- and grant-derived availability, invoke
the existing device-owner gate before constructing the browser, create the
session through `LatchGateway`, refresh sessions on success, and retain the
created session ID long enough for the list to highlight it. Generalize the
name and prompt of `TerminalUnlock` only as needed so one five-minute owner
check can cover terminal access, folder browsing, and session creation.

Test default loading and saving, one-off choices that do not mutate the
default, invalid-default fallback to home, grant changes while the sheet is
open, cancellation, retry with the same request ID, successful refresh, and
the rule that success does not create a terminal connection.

## Phase 7 — build the SwiftUI flow

Add a New session toolbar control to `SessionsView`, including the empty-list
state. Its visibility and disabled explanation come from the model's
discovered endpoints and current device grant.

Build a reusable folder-picker screen with two modes:

- **Create:** navigation plus **Start session here**.
- **Choose default:** navigation plus **Use as default**.

Show folder rows, parent navigation, the absolute current path, loading and
inline retry states, and incremental loading for a paginated directory. Keep a
valid current listing on screen during transient connection failures. When a
saved default is unavailable, present one concise explanation before opening
home.

On create success, dismiss the sheet, refresh the list, and briefly highlight
the returned session. Do not push `TerminalView`, call `openTerminal`, or
perform an attach. Add **New sessions → Default folder** to `SettingsView`,
using the same browser and owner-authentication gate.

Cover accessibility labels, Dynamic Type, long and Unicode paths, an empty
directory, the filesystem root, more than 200 child directories, offline
recovery, and a permission downgrade while presented. Update the Xcode project
if new app-target files are introduced.

## Phase 8 — document and verify the complete flow

Update the mobile README, gateway documentation, and user-facing remote-access
guide with the capability requirements, control grant, owner check, default
folder behavior, and the fact that creation starts only a shell.

Run the focused gates after each layer, then the repository gates:

```bash
cargo test -p latch
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
apps/LatchMobile/Tools/generate-contract.py --check --upstream .
swift test --package-path apps/LatchMobile --filter LatchMobileKitTests
./scripts/check-boundaries.sh
```

Finish with a live paired-path field check on a physical phone:

1. Set a nested folder as the default.
2. Start a session there over local network, direct ICE, and relay paths.
3. Confirm the session exists once, has the chosen `cwd`, and is not attached.
4. Open it from the list and launch an agent manually.
5. Repeat after moving the saved default and after dropping the response to a
   create request.
6. Confirm observe and interact phones see neither directory data nor a usable
   creation route.

## Expected change surface

The implementation should remain concentrated in:

- `schemas/remote-access/v2/` and the generated Swift contract;
- `crates/latch/src/cli/serve/{routes,http}.rs` plus focused directory/create
  modules;
- `crates/latch/src/cli/create.rs` or a nearby engine-level idempotency helper;
- `apps/LatchMobile/Sources/LatchMobileKit/` for transport, preferences, and
  state;
- `apps/LatchMobile/App/LatchMobile/` for the picker and entry points; and
- focused Rust and Swift contract/model tests.

No changes should be needed in `latchd`, the terminal WebSocket, Conversation
Hub, the control-plane service, relay service, ICE/Noise framing, or the
exclusive-attach model.

## Delivery slices

The work is reviewable in four slices:

1. Schema, route registry, directory browser, and authorization.
2. Idempotent shell creation and gateway integration tests.
3. Mobile client, stored default, and testable browser/creation model.
4. SwiftUI, documentation, contract drift checks, and physical-device field
   verification.

