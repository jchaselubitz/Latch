# coo:758.64z7 — Implement Latch Desktop memory recommendations

Implemented the four leverage items from the memory-forensics recommendations artifact.

## Changes

1. **Stop republishing unchanged state** (`SessionStore`, `Models`)
   - Session list assignment is gated on `SessionSummary.isDisplayEqual`, which uses the same coarse idle buckets the sidebar draws (`3m idle`) and ignores raw `idle_ms` / `last_activity_at` churn.
   - `InspectReport` is `Equatable`; details and error clears only publish when values change.
   - Background polls no longer flip `@Published isRefreshing` (private `refreshInFlight` coalesces instead).

2. **Split chrome from session data** (`AppChromeState`, `LatchDesktopApp`)
   - Menu bar label and `.commands` observe `AppChromeState` only (`runningCount`, `canCreateSessions`).
   - `SessionStore` / `UpdateController` / `RemoteAccessController` live in a non-observed `DesktopRuntime` held via `@State`, so session churn cannot invalidate `App.body` or rebuild `-[NSApplication setMainMenu:]`.

3. **Back the poll off when nothing is on screen**
   - 5s while the app is active with a visible window; 60s otherwise.
   - `didBecomeActive` still forces an immediate refresh.

4. **Merge the two pollers**
   - Removed `RemoteAccessController`'s independent 5s loop.
   - Remote status refresh rides `SessionStore.addCompanionRefresh`.
   - Remote `refresh()` also gates `@Published` writes with `Equatable` comparisons.

## Verification

- `swift test` in `apps/LatchDesktop`: new display-equality / chrome tests pass.
- Pre-existing failures in `ControlPlaneHostTests.testANameIsReducedToTheLabelSetTheServiceAccepts` (emoji sanitization) are unrelated and unchanged by this work.
