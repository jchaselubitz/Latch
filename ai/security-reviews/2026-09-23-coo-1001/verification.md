# Remote Link remediation verification

Date: 2026-09-24. This is an **incomplete gate**, not an end-to-end pass.

## Revisions and versions

| Component | Version or revision used | State |
| --- | --- | --- |
| Verification source | `ff19200` on local `main`; clean worktree when tests began | The remediation commit is one commit ahead of `origin/main` (`4b7e463`). |
| Rust CLI/helper source | `0.2609240955.0` (`Cargo.toml`) | Tested from source. No signed release of this revision exists. |
| Desktop built from source | `0.2609240955.0` | Release build succeeded; resulting app is ad-hoc signed, with no Team ID. |
| Mobile simulator and device builds | `0.1.0` | Simulator build succeeded. Physical iPhone build succeeded with an Apple Development signature for Team `X84RPB4674`; it was not installed or paired. |
| Installed Mac CLI, helper, and daemon | `0.2609221322.0` | Developer ID signed release; older than the remediation source. |
| Installed `/Applications/Latch.app` | `0.2609221322.0` | Developer ID signed and notarized; older than the remediation source. |
| Deployed control plane | `ebd4c8f9c79507466a89acb649d0d310f12bcddc` from `/health/ready` | Ready, seven migrations, but not the remediation commit. |
| Deployed relay | Revision not reported by `/health/ready` | Ready; revision could not be established from that endpoint. |
| Published GitHub release | `v0.2609221322.0` latest when checked | No signed release for `0.2609240955.0`. |

## Automated verification

| Step | Result | Evidence / limit |
| --- | --- | --- |
| Rust workspace, `env -i HOME PATH TMPDIR cargo test --workspace --all-targets` | **Fail** | The first sandbox run failed on local socket and pseudo-terminal permissions. The same clean-environment run with those permissions passed the `latch` unit suite (183), `latchd_kernel_e2e` (16), `remote_link_composed` (3), `remote_link_recovery` (6), and the other targets reached before `latchd` security. It stopped at `a_cwd_that_cannot_be_entered_fails_closed`: 13 passed, 1 failed in that target. The assertion expected the screen to contain `cannot enter`, but it was blank. This test and its source are unchanged from `4b7e463`; no remediation objective introduced it. The sole remaining target, `latchd --test session`, was then run separately with the same clean environment and passed 18/18. |
| Rust doc tests, same clean environment | **Pass** | `cargo test --workspace --doc` exited 0. |
| Relay suite | **Pass** | `npm test`: 11 passed. The sandbox-only first run could not bind `127.0.0.1`; the run with loopback permission passed. |
| Control-plane suite | **Pass, one skip** | `npm test`: 47 passed, 0 failed; the PostgreSQL store suite was skipped because `TEST_DATABASE_URL` was unset. The sandbox-only first run could not bind `127.0.0.1`; the run with loopback permission passed. |
| LatchDesktop target | **Pass** | `swift test --package-path apps/LatchDesktop`: 83 passed. |
| LatchMobileKit target | **Fail** | `swift test --package-path apps/LatchMobile --filter LatchMobileKitTests`: 268 tests, 1 skipped, 2 failed. `ConversationPresentationContractTests.testPresentationNamesNoProvider` finds an existing `Codex` comment in `ConversationViewState.swift:31`; that line exists unchanged in `4b7e463`. `TerminalLifecycleTests.testOpeningChatOpensNoTerminalAndAsksForNoOwnerCheck` found no session summary; its test and model source were not changed by `ff19200`. The first full Mobile package run reported the same two failures. No remediation objective can be identified as their introducer from this diff. |
| Desktop app build | **Pass** | `apps/LatchDesktop/build-app.sh` produced `apps/LatchDesktop/.build/release/Latch.app`, version `0.2609240955.0`. It is an unsigned release build for local inspection, not a trusted installation. |
| Mobile app build | **Pass** | `xcodebuild build` succeeded for both generic iOS Simulator and the connected iPhone 16 Pro. The physical build is development signed, version `0.1.0`. |

## Real-device Remote Link checklist

The connected physical phone is an iPhone 16 Pro. The following checks were **not run** because the Mac's installed app/CLI and the deployed control plane are older than `ff19200`. Pairing them would test a mixed, older deployment instead of the hardened code required by this objective. The current Desktop source build is ad-hoc signed; substituting it for the installed signed app would also invalidate the intended release/Keychain conditions.

| Step | Result |
| --- | --- |
| Pair the physical phone through the QR flow against the deployed control plane and relay | Not run: hardened Mac endpoint and control plane are not deployed. |
| Inspect approval prompt for separate device name and requested grant fields | Not run; Desktop model tests passed, but there is no real-device observation. |
| Observe: view sessions and conversation; refuse send and resolve | Not run on a physical device. Automated composed/gateway tests passed. |
| Interact: send and resolve | Not run on a physical device. Automated composed/gateway tests passed. |
| Control: terminal input, session creation, stop work | Not run on a physical device. Automated kernel and gateway tests passed. |
| LAN discovery with both devices on one network | Not run on a physical device. Automated LAN acceptor test passed. |
| Relay path on separate networks | Not run on a physical device. Automated Remote Link recovery test passed. |
| Stale and revoked pairing refused | Not run on a physical device. Control-plane revocation tests passed. |

## Signed-release acceptance

| Step | Result | Evidence / limit |
| --- | --- | --- |
| Installer publisher verification on a legitimately signed release | **Pass for the installed older release** | Copies of the installed `0.2609221322.0` `latch`, `latch-remote`, and `latchd` binaries passed `scripts/test-install-verification.sh`. Ad-hoc, wrong-Team-ID, and mixed payloads were refused. This validates the installer check against a real signed payload, but not a new release of `ff19200`. |
| Local `dist/latch-0.2609221322.0-aarch64-apple-darwin.zip` as a release candidate | **Refused correctly** | Its `latch` binary is ad-hoc signed (`TeamIdentifier=not set`) and Gatekeeper rejects it. This local archive is not the signed installed payload and is not valid evidence of positive acceptance. |
| CLI updater and Desktop updater accept a legitimately signed new release | **Not run** | The latest published release is `v0.2609221322.0`; the installed CLI and Desktop are already at that version, and no signed `0.2609240955.0` release exists. Updater unit tests passed within the Rust and Desktop targets, but they do not prove a live update. |

## Gate decision

The final gate is **not passed**. Two full test targets remain red on cases present before the remediation commit, and the hardened revision is not available as a signed Mac release or deployed control plane. There is no observed Remote Link regression attributable to one of the remediation objectives; the required physical-device grant, LAN/relay, revocation, and live updater checks remain unverified. Repeat this gate after the test failures are resolved and the same hardened revision is available on the Mac, phone, control plane, and relay.
