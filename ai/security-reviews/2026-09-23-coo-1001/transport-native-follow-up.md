# Transport, FFI, and native-client follow-up

Objective `coo:1001.z2qn`. Offline source review of the Remote Link transport, its Swift FFI boundary, and the native-client paths that hold keys, admissions, pairing approval, the phone loopback capability, and the Desktop updater. No application code was modified. No exploits or production tests were run.

This note is additive. The four findings in `report.md` / `findings.json` from objective `coo:1001.aep3` are unchanged.

## Result

| ID | Finding | Severity | Confidence |
| --- | --- | --- | --- |
| T1 | Pairing approval interpolates the peer-supplied device name into the only readable grant sentence | medium | high |
| T2 | Pre-handshake relay records are buffered with no cap | low | high |
| T3 | One unauthenticated LAN connection stalls the only LAN acceptor | low | high |
| T4 | Bonjour coordinates are not restricted to private addresses, and an authentication failure drops the rest of the LAN list | low | high |
| T5 | Phone connector will open a cleartext relay URL; Desktop updater freshness is the GitHub tag | low | high |

Noise XX pins, the QR-only enrollment secret, service/purpose separation, the 256-bit loopback capability, and Keychain protection held under review. Details are in **Checked and sound** below.

## T1 — Pairing approval interpolates the peer-supplied device name

| Field | Value |
| --- | --- |
| Severity | medium |
| Confidence | high |
| Category | authorization / UI spoofing |
| CWE | CWE-451 |
| Affected lines | `crates/latch-remote/src/link.rs:738-754`, `crates/latch-remote/src/link.rs:668-675`, `crates/latch/src/cli/remote_access.rs:204-207`, `apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessView.swift:406-416` |

### Summary

Anyone who can see the pairing QR can complete enrollment Noise and choose both the grant and the device name. The Mac approval sentence is `Confirm \(name) requests \(permission.label)`. The host checks only that the name is non-empty and at most 80 bytes. Newlines and other control characters are accepted. The comparison code is an opaque 64-bit digest, so matching codes do not tell the owner which grant was requested. The readable grant is the sentence the name can lead.

### Why this is reachable

The QR is on the Mac's screen for the pairing window and includes the enrollment id and the 256-bit secret. A custom client, not the Latch phone app, can join that enrollment. The phone app always asks for `control` and sanitizes the name (`PairingModel.enrollableName`, `PairingModel.swift:322-323` and `372-386`). The host does not apply that character policy. `validate_proposal` accepts any name of 1–80 bytes, and `authorize_enrollment` repeats only the length check before storing `proposal.permission`.

Desktop then renders one `Text` view:

```swift
Text("Confirm \(name) requests \(permission.label), and check that the phone shows exactly:")
```

A name such as `Mom requests Watch only.\n` makes the first line read as a Watch-only request while the decoded permission, still concatenated afterward, is Full terminal control. Approving commits that decoded permission (`link.rs:668-675`). The transcript comparison binds the permission bytes, but the owner cannot read them from the displayed code.

### Counterevidence

- The owner must tap Approve. Nothing is committed on scan alone.
- The true `permission.label` is still appended after the name. This is a misleading prompt, not a hidden field.
- The decision IPC must echo the proposal's key, id, and permission (`validate_decision`). A spoofed name does not change which enum is stored.
- Noise still pins the host key from the QR, and the secret is required in the prologue.

### Fix

Render the name and the grant as separate labeled fields. Before display and before `authorize_enrollment`, reject names outside the phone's allowlist (letters, digits, space, and `. ' _ ( ) -`), including newlines and bidi controls. Add a regression that a proposal name containing `\n` or a second "requests …" phrase is rejected and that the approval view does not build one sentence from the name and the grant.

## T2 — Pre-handshake relay records are buffered with no cap

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Category | availability |
| CWE | CWE-770 |
| Affected lines | `crates/latch-transport/src/link.rs:422-434`, `crates/latch-remote/src/link.rs:534-540` |

`wait_for_peer` pushes every binary WebSocket record onto `pending` until it sees `peer_ready`. There is no count or byte cap. Each record is limited to 65,535 bytes, but the queue is not. The session helper waits with `wait_for_peer(None)`, so the wait is also unbounded in time while the relay keeps the socket audibly alive.

The relay's own 8 MiB forward-buffer and the Yamux receive window apply after the peer is present. They do not cover this queue. A relay that withholds `peer_ready` and sends binary frames can grow helper or phone memory for the life of the socket. An admitted peer cannot do this on the current relay: `peer_ready` is written before that peer's binary frames are forwarded (`services/relay/src/server.ts:194-207`). Impact is a compromised or malicious relay, which the threat model already treats as denial of service. The missing cap still contradicts the stated 8 MiB memory bound.

Fix: cap `pending` (for example 8 MiB or 32 records) and fail the wait when the cap is hit. Keep the host's no-deadline wait for an absent phone; bound the bytes, not the wait.

## T3 — One unauthenticated LAN connection stalls the LAN acceptor

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Category | availability |
| CWE | CWE-400 |
| Affected lines | `crates/latch-remote/src/link.rs:48-49`, `crates/latch-remote/src/link.rs:308-344` |

The helper binds `0.0.0.0` on an ephemeral port and accepts handshakes one at a time. `SecureLink::establish` allows 10 seconds. A caller who never finishes Noise occupies that loop until the timeout, then can connect again. Failed handshakes do not replace the authenticated link, and the relay path keeps running. The effect is that the paired phone cannot use LAN while the stall continues. Reachability of the port beyond the local network depends on the host firewall, which was not inspected.

Fix: accept concurrently with a small cap, or drop a handshake that has not completed well under the 10 second limit, without head-of-line blocking the listener.

## T4 — Bonjour coordinates are not restricted to private addresses

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Category | confused deputy |
| CWE | CWE-441 |
| Affected lines | `apps/LatchMobile/Sources/LatchMobileKit/GatewayTransport.swift:436-450`, `apps/LatchMobile/Sources/LatchTransportNative/NativeRemoteTransport.swift:360-379`, `crates/latch-transport-ffi/src/lib.rs:191-195` |

The phone treats `_latch-remote._tcp` TXT records as untrusted coordinates, which is right, but `lanAddrs` and `lanHost` are only checked for non-empty and length ≤ 45. They are not required to be IP literals or private/link-local addresses. The Mac's public key is already in the legitimate advertisement, so a LAN attacker can republish that key with their own host and port. The phone opens TCP and starts Noise before the pin fails.

`connectFirstLanTarget` returns immediately on an authentication error, so one spoofed candidate aborts the remaining LAN targets, including a real Mac later in the list. The relay is then used. The Noise private key is not sent in the first handshake message, and a pin mismatch fails closed. Impact is a handful of outbound TCP connections (at most six, inside a 1.5 second budget) plus loss of the LAN path for that attempt.

Fix: accept only IP literals in private, link-local, or unique-local ranges. On authentication failure, try the next candidate instead of abandoning LAN.

## T5 — Cleartext relay URL on the phone, and tag-based Desktop updates

These are separate low issues on the native clients.

**Phone relay URL.** Desktop and `latch-remote` refuse a relay URL that does not start with `wss://` (`ControlPlaneHost.swift:603-604`, `crates/latch-remote/src/link.rs:93-94`). `RemoteLink.connectWss` and `WssRecordIo::connect` do not. The phone passes `claim.relayUrl` and `admission.relayUrl` through (`NativeRemoteTransport.swift:31-42` and `308-314`). That socket is Rust `native-tls`, so App Transport Security does not apply. A control-plane response of `ws://…` sends the single-use admission bearer in cleartext. Noise still encrypts records, and the control plane minted the admission, so this is a missing client pin rather than a new reader of terminal data.

**Desktop updater.** `UpdateResolution` treats a newer GitHub tag as a newer app (`Updater.swift:103-119`). `applicationBundle` installs the first `.app` in the archive (`Updater.swift:310-316`), not a pinned name, and does not compare `CFBundleShortVersionString` with the tag. `verify` checks the signing team against the running app and Gatekeeper (`Updater.swift:274-286`). Someone who can publish a release but does not hold a new signing key can attach a previously signed older build to a newer tag. The initial-install publisher gap from the first review is a different bug; this one is downgrade inside an already signed installation.

Fix: require `wss://` in `connect_wss` before the bearer is set. For the updater, require the bundle identifier, a `Latch.app` name, and a bundle version equal to the tag before `replaceItemAt`.

## Checked and sound

- **Session Noise.** Prologue is `latch-remote-link`, `v1`, purpose, `controller->host`, then both static keys in controller/host order (`link.rs:1104-1133`). Session config without a 32-byte pin is rejected (`link.rs:228-229`). `get_remote_static` must equal the pin (`link.rs:776-781`). A role swap changes prologue key order, so both sides claiming Controller do not complete. Pattern is `Noise_XX_25519_ChaChaPoly_BLAKE2s`.
- **Enrollment.** Prologue mixes the enrollment id and the 32-byte QR secret. The host does not pin the controller key at handshake time; it checks the proposal key against `remote_public_key` (`link.rs:750`) and waits for a Desktop decision that echoes that proposal (`link.rs:653-666`) before `authorize_enrollment`. Gateway streams are refused on an enrollment link (`link.rs:911-918`). The phone pins `payload.hostPublicKey` and checks the encrypted receipt against that key, the same permission, and a positive grant revision (`NativeRemoteTransport.swift:156-164`).
- **Framing.** LAN length `0` or above 65,535 fails. Service headers are capped at 1024 bytes and `deny_unknown_fields`. Yamux is capped at 32 streams and an 8 MiB connection receive window. Decryption failure ends the transport task.
- **FFI.** `connect_lan` rejects an empty host, a host longer than 255 bytes, and port 0 before the keys are used (`latch-transport-ffi/src/lib.rs:191-195`). Private keys are wrapped in `Zeroizing` on the Rust side. Stream read and write use separate locks. Authentication errors are typed so the phone stops retrying that attempt.
- **Phone loopback.** The listener's required local endpoint is `127.0.0.1`. The capability is 32 bytes from `SecRandomCopyBytes`, checked once, then stripped. A second `Authorization` or `Proxy-Authorization` is rejected (`GatewayTransport.swift:186-189`, `327-379`). Later bytes on an accepted connection are not a capability bypass: the first request already required the secret.
- **Keys at rest.** The X25519 identity is `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`. When the Secure Enclave is present the private key is sealed under a non-exportable P-256 key. Unwrap checks that the private key matches the stored public key (`DeviceIdentity.swift:217-220`). The paired-device record, including the control-plane token, uses the same accessibility (`PairedDevice.swift:205-216`).
- **Directory vs local grant.** The phone refuses to connect when the directory permission or peer key differs from the pinned record (`NativeRemoteTransport.swift:253-261`). The helper's proxy still loads the local device by key and grant revision (`remote_access.rs:745-757`). A directory permission is not what the proxy injects.
- **Terminal owner check.** `TerminalUnlock` is a local Face ID / passcode gate with a five-minute grace. It is not the Mac's grant check. Chat is intentionally outside it (`TerminalUnlock.swift:63-68`).
- **Control-plane URL on the phone.** Pairing QR `controlPlane` must be `https`, or `http` only for `127.0.0.1`, `localhost`, or `::1` (`PairingPayload.swift:63-67`).
- **Desktop control-plane URL.** `ControlPlaneHost.normalize` accepts any `http` host even though the error text says HTTPS (`ControlPlaneHost.swift:444-454`). The Desktop app has no ATS exception, so `URLSession` still blocks non-local cleartext. That is why this is not filed as its own finding. The validator should still reject non-loopback `http` so the check does not depend on ATS.
- **Lease ids from the relay.** `parse_relay_status` allows any 16–96 character lease id (`link.rs:517-522`). The phone interpolates it into `/v1/relay-leases/{id}/renew`. The server accepts only `lease_[0-9a-f]{32}` (`api.ts:760-763`). No alternate route is reached by that suffix. Not filed.

## Coverage of this follow-up

Fully read for this objective:

- `crates/latch-transport/src/lib.rs`
- `crates/latch-transport/src/link.rs`
- `crates/latch-transport-ffi/src/lib.rs`
- `crates/latch-transport-bindgen/src/main.rs`
- `apps/LatchMobile/Sources/LatchTransportNative/NativeRemoteTransport.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/GatewayTransport.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/DeviceIdentity.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/PairingPayload.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/ControlPlane.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/Signaling.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/PairedDevice.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/TerminalUnlock.swift`
- `apps/LatchMobile/Sources/LatchMobileKit/LatchGateway.swift`
- `apps/LatchDesktop/Sources/LatchDesktop/Updater.swift`
- `apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessSupervisor.swift`

Security paths read inside larger files, not counted as full-file coverage: `PairingModel.swift` (name policy and the control grant request), `RemoteAccessController.swift` (owner decision), `RemoteAccessView.swift` (approval sentence), `ControlPlaneHost.swift` (address normalization and `wss://` admission checks), `ConversationSocket.swift` and `TerminalSocket.swift` (framing only). `crates/latch-remote/src/link.rs` was already fully reviewed in the first scan; the enrollment, LAN, and relay-wait paths were re-read here.

Still not fully reviewed: SwiftUI conversation rendering, generated UniFFI bindings, Desktop `LatchClient.swift` beyond its `Process` argument arrays, and the test and script paths left in `coverage.json`. Conversation markdown uses Foundation inline-only `AttributedString` and documents that raw HTML is not interpreted (`ConversationMarkdown.swift:8-19`); that file was not fully audited. No full-repository completeness claim is made.
