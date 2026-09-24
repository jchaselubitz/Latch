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

## Continuation after signed release installation (2026-09-24)

The operator installed the new Mac payload, and the local branch and `origin/main` now point to `ab658c1`. Its only file change after `ff19200` is this verification report. The hardened application code is therefore still `ff19200`.

| Step | Result | Evidence / limit |
| --- | --- | --- |
| Installed Mac versions | **Pass** | `latch`, `latch-remote`, `latchd`, and `/Applications/Latch.app` all report `0.2609240955.0`. The installed Desktop app has Team ID `X84RPB4674` and a stapled notarization ticket. |
| CLI updater accepts signed release | **Pass** | Latch Desktop's CLI Updates pane states it updated the CLI from `0.2609221322.0` to `0.2609240955.0`. `latch update --check --json` reports `current` at the newly published `v0.2609240955.0`. This verifies the completed CLI update; it was initiated by the operator. |
| Installer publisher check on the new payload | **Pass** | `scripts/test-install-verification.sh /Users/jake/.local/bin` accepted the installed signed three-binary payload and refused ad-hoc, wrong-Team-ID, and mixed payloads. The script exercises verification but does not reinstall the payload. |
| Deployed services | **Partially verified** | Control-plane `/health/ready` reports `ab658c14925d2a7464b0c56c09d7183bd2b48164`, seven migrations, and ready. This commit contains `ff19200` and only adds this report. Relay `/health/ready` reports ready but does not expose a revision. |
| Physical phone app | **Installed** | A development-signed `LatchMobile` build from the hardened source was installed on the connected iPhone 16 Pro. The bundle reports version `0.1.0` and Team ID `X84RPB4674`. iOS rejected a remote launch while the phone was locked; the operator must unlock it. |
| QR pairing | **Blocked on physical scan** | Latch Desktop showed a fresh five-minute QR and waited for the phone; no scan, approval prompt, or successful link was observed before it expired. The short-lived QR secret is intentionally not recorded here. A new QR can be generated for the next attempt. |
| Desktop updater accepts signed new app | **Still unverified** | The operator installed the signed app, but no evidence yet shows that the in-place Desktop updater performed the installation. |

The earlier red Rust and MobileKit tests have not been changed or rerun in this continuation. The remaining real-device checklist above still requires observed phone interaction before this gate can pass.

## Physical iPhone pairing attempt (same day)

The first new pairing attempt reached `enrollment_approved` on the Mac, but the phone remained unpaired. I changed that device's grant from Control to Observe immediately after the Mac reported approval and before the phone confirmed local persistence. The phone verifies that its directory grant still matches the encrypted Control receipt before saving the record (`NativeRemoteTransport.swift`, `awaitApprovedRecord`); the premature change is a likely cause, not evidence of a code regression. The operator revoked that incomplete device and repackaged/reinstalled the app on the phone.

The second QR attempt succeeded on **both** devices. Latch Desktop reported that the phone received its encrypted receipt and showed `Connected (1 device)`. The local audit recorded `enrollment_approved` for the new device, then `link_ready` with result `relay` and successful stream opens. Thus the deployed relay path worked for this pairing. The Mac also reported `link_lan_ready`, which means its LAN listener was ready; it does **not** prove that the phone selected LAN. The phone's report of being paired is operator observation. The approval prompt's separate name/grant layout and matching comparison words were not observed or confirmed clearly enough to mark those steps passed.

Only after the phone confirmed pairing, I selected Observe in Latch Desktop's base-grant picker. The link reconnected with `link_ready: relay`, but the effective grant was still Control because **Allow terminal** remained on. The next section records the actual grant transition and its failure.

### Grant-change failure and correction

In Desktop settings, the Observe picker records a base choice while **Allow terminal** is on. `RemoteAccessView.swift` deliberately leaves the effective grant at Control until the terminal switch is turned off. The Mac device row still said “Control sessions and open the terminal,” and `latch remote-access devices --json` still reported `control`, revision 1. The operator sent terminal input from the phone that reached this session during that state; this was expected Control behavior, **not** an Observe bypass. A later test message was sent through phone chat, also while effective Control was active. My initial statement that this was a permission failure was incorrect.

At 11:35:08 UTC the same confirmed pairing reached `link_ready: lan` in the Mac audit and opened streams, so the authenticated LAN path also worked while both devices shared a network. The relay path had already reached `link_ready: relay` at 11:31:27 and again at 11:33:14. A relay connection while both devices share a network does not by itself satisfy the separate-network test.

At 11:36:18 UTC I turned **Allow terminal** off. Desktop then displayed the iPhone as “Read sessions, conversations, and events,” and `latch remote-access devices --json` reported `permission: observe`, `grantRevision: 2`, `revoked: false`. The Mac closed the prior stream and waited for a new phone link. The phone instead displayed **“This phone was unpaired”** with **“The Remote Link directory does not match this locally pinned pairing. Pair again from a new code on your Mac.”** The operator supplied a screenshot of that screen. The control plane returned HTTP 200 for the device read after the change and HTTP 201 for a new relay admission, so the service still recognized the device; the Mac's local record remained active.

Source of the failure: `NativeRemoteTransport.swift` `NativeRemoteLinkConnector.connect` requires `descriptor.permission == record.permission.rawValue` before opening LAN or relay and maps any mismatch to `RemoteLinkFailure.revoked` with exactly the message seen on the phone. The phone's saved record still held Control when the directory began reporting Observe. `PairingModel.refreshPermission` can update a saved permission from the service, but the reconnect reached the strict equality check first in this run. The relevant guard is identical at `4b7e463` and `ff19200`; this failure was **not introduced by a remediation objective**. The verification gate remains red because a live Control-to-Observe downgrade made a valid pairing unusable. I did not re-pair or alter the grant to bypass the failure.

Observe read/write denial, Interact, Control after a grant transition, relay specifically from a separate network, stale/revoked pairing refusal for this successful phone, and the Desktop updater remain unverified. The approval prompt's separate name/grant fields and comparison words were not directly observed by the verifier; the operator believed the words matched but could not confirm the layout.

Follow-up fix objective `coo:1001.hb7y` was added to this mission for the valid-grant-change false unpair. Verification objective `coo:1001.6nz9` remains open and must be rerun after that fix ships to the phone. No application-code workaround was made during this gate.

## Grant-change repair and physical reconnection (2026-09-24)

After the operator authorized unattended work and left the iPhone connected over USB, I implemented the follow-up repair in the mobile source. The connector now treats the locally saved permission as a refreshable projection, while still requiring the directory's pinned Mac device ID, exact public key, supported permission, and positive grant revision. It uses the current revision for the authenticated LAN or relay handshake. The link snapshot carries that permission, and `AppModel` applies it before fresh-link discovery; a downgrade removes terminal access. The app refreshes and saves the control-plane permission after a ready link. Missing or changed Mac identity, invalid permission, and zero revision still stop the attempt.

Focused regression tests for Control-to-Observe, Observe-to-Interact, host-key mismatch, missing/invalid grant, coordinator snapshot propagation, and app-model read-only projection passed: 13 tests, 0 failures. A development-signed physical iPhone build succeeded and was installed over the existing app, preserving its pairing. `devicectl` launched the app. The Mac audit then recorded `link_ready: lan` at 11:57:01 UTC and successful streams from the same device; the Mac's local record remained `observe`, revision 2, `revoked: false`. No new QR or enrollment was used.

I changed that device's Mac grant to Interact (revision 3), restarted the app, and the Mac recorded `link_ready: lan` at 12:39:02 UTC. I then changed it to Control (revision 4), restarted, and recorded another `link_ready: lan` at 12:39:48 UTC. Finally I restored Observe (revision 5), restarted, and recorded `link_ready: lan` at 12:40:44 UTC. The device remains active and Observe. These audit events prove authenticated reconnection across all three current grants, but there was no direct screen capture of the phone's controls or a test of forbidden operations under each grant. Device Hub's UI automation timed out, so this observation remains audit-based.

The full Mobile package suite initially exposed three failures caused by test-only connectors reporting Control for an Observe test record. Those fixtures now report the record's grant. Its final run passed **270 tests, 1 skipped, 0 failures**. A preexisting provider name in a presentation-layer comment violated an existing contract test; I made that comment provider-neutral before the final run. The iPhone build and installation preceded that comment-only change; there is no runtime difference. The earlier Rust security test failure and the remaining physical checklist items above have not been re-run or satisfied. In particular, this is not yet evidence for separate-network relay, on-screen Observe/Interact denial, real-device revocation, or the Desktop app updater. The overall verification gate remains open.

## Operator-confirmed checks (2026-09-24)

The operator subsequently confirmed that real-device revocation and pairing approval work, and that the phone works over cellular. These three checklist items are now **passed by operator observation**: revocation, pairing approval, and relay from a separate network. This confirmation supersedes the earlier unverified status for those items; no additional screen capture or independent trace was collected for these checks.

Remaining verification: the phone's Observe/Interact/Control operation permissions, an actual signed Desktop updater installation, the earlier Rust security-test failure, and the skipped PostgreSQL control-plane suite with `TEST_DATABASE_URL` configured. The final gate remains open for those items.
