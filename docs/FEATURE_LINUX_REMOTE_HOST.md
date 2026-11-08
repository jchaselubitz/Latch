# Linux remote host: existing infrastructure and required work

Repository assessment for `coo:1255.d0vd`, 6 November 2026.

## Conclusion

Latch has much of the underlying Linux infrastructure already. The terminal
kernel, application gateway, local grant authority, and encrypted Remote Link
transport are Rust components with Linux support in the source. However, the
complete remote host is currently assembled and operated by Latch Desktop on
macOS. A Linux server cannot obtain the same mobile experience simply by
building the CLI and running `latch remote-access enable`.

The recommended change is a headless Rust host coordinator that reuses the
existing helpers, gateway, control plane, relay, and mobile protocol. It must
replace Desktop's enrollment, connection supervision, credential persistence,
and notification forwarding responsibilities. Linux release/install/update
support and a Linux service lifecycle are also required.

This is an assessment and implementation proposal. No Linux host service or
Linux release was implemented, deployed, or tested as part of this objective.

## What already exists

| Component | Existing implementation | Consequence for Linux |
| --- | --- | --- |
| Persistent sessions | [`latchd`](../crates/latchd/README.md) owns one PTY and child process per session, independently of the attached terminal. [`pty.rs`](../crates/latchd/src/pty.rs) has Linux-specific PTY support; [`peer.rs`](../crates/latchd/src/peer.rs) uses Linux `SO_PEERCRED` to enforce the same-user socket boundary. | Reuse the kernel; a Linux GUI or replacement terminal engine is unnecessary. |
| Mobile application API | [`serve/routes.rs`](../crates/latch/src/cli/serve/routes.rs) and [`serve/http.rs`](../crates/latch/src/cli/serve/http.rs) provide v2 session listing/creation/stopping, terminal access, conversations/actions, attachments, directories, agent/model discovery, Git operations, and notification-key registration. | Reuse these routes and contracts for the existing phone app. Individual features still need Linux acceptance coverage. |
| Git session association | [`serve/git/session.rs`](../crates/latch/src/cli/serve/git/session.rs) reads `/proc/<pid>/cwd` on Linux. | Current-directory-based repository grouping already has a Linux implementation. |
| Local authority and identity | [`remote_access/devices.rs`](../crates/latch/src/cli/remote_access/devices.rs) stores identities, current controller grants/revisions, revocations, and bounded audit events. Non-macOS builds store the host private key in `identity.key`, with private-directory/file checks and modes. | Host Noise identity storage is already present. This does **not** provide storage for the missing coordinator's account/device API credentials. |
| Encrypted connection | [`latch-transport`](../crates/latch-transport/src/lib.rs) implements Remote Link; [`latch-remote`](../crates/latch-remote/src/link/mod.rs) operates enrollment and paired links over WSS and authenticated LAN carriers. | Reuse encryption, pinning, multiplexing, LAN discovery, and gateway proxying. |
| Shared gateway owner | [`remote_access/gateway.rs`](../crates/latch/src/cli/remote_access/gateway.rs) supervises one private loopback gateway, with a separate owner lock and device-scoped authenticated streams. | Keep one gateway per Latch home and independent per-phone link helpers. |
| Control plane | [`api.ts`](../services/control-plane/src/api.ts) manages accounts, devices, enrollment, grants, admissions, leases, revocation, and attention. Device registration accepts a bounded platform string; `linux` is valid and host authorization uses `role: host`. | No new Linux-specific cloud host model appears necessary. Add Linux integration coverage rather than assuming this source-level compatibility proves an end-to-end flow. |
| Relay | [`services/relay`](../services/relay/README.md) forwards opaque encrypted records between outbound WebSockets. | Reuse the existing relay. A remote Linux box should not need a public application port, inbound SSH, or its own public TLS endpoint for relay access. |
| CI configuration | [Rust CI](../.github/workflows/ci.yml) configures macOS and Ubuntu workspace checks/tests and a real `latchd` parity gate. | There is an established Linux verification lane, but its configuration is not evidence that the latest run passed or that a physical phone works with Linux. |

The architecture to retain is:

```text
Latch mobile app
  -> outbound WSS relay OR authenticated LAN connection
  -> existing encrypted Remote Link / latch-remote
  -> authenticated proxy and private loopback v2 gateway
  -> local Latch sessions, latchd PTYs, agent CLIs, and Git repositories

New Linux host coordinator
  -> existing HTTPS control-plane API
  -> existing latch-remote helper stdin/stdout protocol
  -> local operator commands and notification forwarding
```

The relay and control plane remain shared services. Session processes,
repositories, transcripts, gateway credentials, and host private keys remain
on the Linux endpoint, subject to the existing encrypted attachment and
notification flows. There is no reason to put the server's shell or v2
plaintext gateway directly on the Internet.

## The missing host coordinator

[`RemoteAccessController.swift`](../apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessController.swift),
[`ControlPlaneHost.swift`](../apps/LatchDesktop/Sources/LatchDesktop/ControlPlaneHost.swift),
and [`RemoteAccessSupervisor.swift`](../apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessSupervisor.swift)
currently connect the reusable pieces. A Linux implementation needs the
following equivalent behavior:

1. **Bootstrap and credentials.** Configure the control-plane URL and host
   name, obtain an account through the existing one-use owner invitation flow,
   register a Linux host using its local public key, and persist account/device
   credentials securely across unattended restarts. Desktop currently stores
   these credentials in Keychain and hardcodes `platform: macos` in registration.
   Do not copy the Mac's identity or credentials to the Linux host.
2. **Headless pairing.** Open a short-lived enrollment and render its existing
   QR payload in an authenticated operator workflow, such as a terminal over
   SSH or a locally generated QR image viewed on another screen. Show the
   proposed phone, requested grant, and transcript comparison code; require
   explicit operator approval. Drive the helper's existing
   `enrollment_decision` and `enrollment_mirrored` exchanges and verify the exact
   control-plane receipt. Preserve the QR-only secret and provisional authority
   boundary. The Rust enrollment helper already commits the local grant; the
   coordinator supplies the owner decision and mirrors that grant remotely.
3. **Connection ownership.** Reconcile paired controllers, supervise the one
   shared gateway and independent per-controller helpers, consume readiness and
   status events, and restart failures with bounded backoff. Initial admissions
   and subsequent commands must travel through inherited pipes, as they do
   today. Starting `latch-remote --link-serve` with a saved admission is not an
   unattended service: admissions are single use.
4. **Renewal and recovery.** Handle `lease_started` and `admission_needed`,
   renew leases, obtain fresh admissions, and recover after network loss,
   helper failure, and control-plane outages. The Rust helper asks its parent
   for admissions; it does not independently perform those API calls.
5. **Authority administration.** Provide status, device listing, grant changes,
   revoke/unpair, disable, and identity lifecycle operations. Existing CLI
   commands cover local state, but Desktop currently mirrors permission and
   revocation changes to the control plane and reconciles running helpers.
   Linux administration must perform that complete workflow and preserve
   fail-closed behavior when local and remote state differ.
6. **Phone notifications.** Port the behavior of
   [`AttentionForwarder.swift`](../apps/LatchDesktop/Sources/LatchDesktop/AttentionForwarder.swift):
   read spooled attention events, map local controller IDs to directory IDs,
   forward the opaque sealed payload through the control plane, acknowledge
   successes/permanent refusals, and retain transient failures for retry.
   Without this, foreground mobile access may work while background agent
   alerts remain absent. APNs remains a cloud-to-iOS service; Linux does not
   need its own APNs implementation or signing key.

[`latch remote-access`](../crates/latch/src/main.rs) currently exposes enable,
status, disable, devices, grant, revoke, attention spool commands, device-key
rotation, and audit. It does not expose a complete control-plane bootstrap,
pairing, or long-running host coordinator command. `enable` writes local
settings and creates an identity; it does not start the full remote service.

## Linux operational and distribution requirements

- **A service per Unix user.** Run as the user whose sessions, repositories,
  agent configuration, and credentials the phone should access, with the same
  `HOME` and `LATCH_HOME` as their CLI. Existing kernel authorization is
  same-UID. Running a separate root service would create the wrong authority
  and session boundary. This proposal does not combine several Unix users
  into one remote host identity.
- **Unattended lifetime.** Supply a systemd user unit with restart policy,
  boot/logout persistence (including user lingering where needed), explicit
  executable paths/environment, diagnostics, and graceful shutdown. Verify
  process-group/cgroup behavior: restarting the connection coordinator must
  preserve live `latchd` sessions; stopping an individual session must still
  terminate its process tree. A machine reboot cannot preserve running PTYs.
- **Server environment.** Configure a valid login shell, `PATH`, Git/SSH
  credentials, agent CLIs and authentication, readable repositories, and a
  writable persistent state root. `create.rs::login_shell_path` currently
  defaults to `/bin/zsh` when `SHELL` is absent; a Linux service needs a valid
  configured shell or a portable fallback. Test actual shell/agent launches
  under the service environment, not only interactive SSH.
- **Credential policy.** Extend private credential storage to coordinator
  tokens and provider keys. Owner-only files are consistent with the existing
  Linux Noise-key implementation; encrypted/system-managed credential delivery
  is an alternative where the deployment requires it. Keep secrets out of
  argv, logs, QR diagnostics, and generic status output. Define loss, rotation,
  and re-enrollment behavior.
- **Networking.** Permit outbound HTTPS to the control plane and WSS to the
  relay, with working certificate trust and long-lived WebSocket support.
  Relay access does not require an inbound firewall opening. LAN discovery
  uses mDNS and an ephemeral TCP listener and therefore needs a reachable local
  network and appropriate firewall rules; a remote data center can use relay
  access without phone-to-server mDNS reachability. Test that unavailable LAN
  advertisement does not prevent relay operation.
- **Linux release payloads.** The current release workflow publishes only
  Apple Silicon macOS. `release-cli.sh` accepts Darwin targets only, the
  installer selects Darwin archives and invokes Apple verification/extraction
  tools, and `update/release.rs::host_target` rejects Linux. Add a coordinated
  `latch`/`latch-remote`/`latchd` Linux payload, manifest/version checks,
  Linux extraction and publisher verification, installation, and update/rollback
  behavior. Start with x86_64 GNU/Linux; add ARM64 explicitly if required by
  the server fleet. Specify the supported distribution/glibc baseline and
  native TLS dependencies instead of assuming a portable static binary.

## What “identical functionality” additionally requires

| Mobile behavior | Linux implication |
| --- | --- |
| Sessions, terminal, conversation actions, uploads, directories, agent launches, Git | Existing Rust endpoints are reusable. Validate each with the Linux user's real environment and the phone's existing grants/capability checks. |
| Background agent attention | Implement the coordinator's attention forwarding loop, including sealed payload and retry behavior. |
| AI commit-message drafting | A concrete portability gap: [`commit_message.rs`](../crates/latch/src/cli/commit_message.rs) loads Gemini credentials only from macOS Keychain; non-macOS loading returns `Missing`, and credential setup is refused. Add Linux credential storage and setup to achieve this parity. Leaving the feature unavailable is a reduced-scope release, not identical functionality. |
| Pairing and troubleshooting copy | Mobile's [`PairingPayload`](../apps/LatchMobile/Sources/LatchMobileKit/PairingPayload.swift) requires `macName`, and [`PairedMac`](../apps/LatchMobile/Sources/LatchMobileKit/PairedDevice.swift) is a name/key/id record rather than an OS check. Linux can supply its host name using the current wire field. Generalize visible “Mac” and “open Latch on your Mac” instructions to computer/server and CLI administration. Preserve stored-record compatibility; renaming every wire field is unnecessary. |
| Keeping a Mac and Linux server paired concurrently | The phone's `PairedDeviceStoring` API saves/loads one `PairedDeviceRecord`, and `AppModel` has one current pairing. A host switcher requires a separate collection/selection/storage and connection-state change, with caches, pending actions, credentials, notification routing, and terminal state scoped to the selected host. Linux as the one paired computer does not require that expansion. |

## Implementation choice and recommended sequence

Prefer a Rust coordinator, exposed through a deliberate CLI/service interface,
which supervises existing helpers and owns control-plane integration. Keep
`latchd` as the per-session kernel and retain the existing parser/process
boundaries. Desktop can eventually consume the same coordinator, reducing
duplicated lifecycle logic; full macOS migration need not block the initial
Linux host.

Porting the Swift controller into a Linux executable is another option, but it
still requires replacing Apple UI/Keychain dependencies and maintaining a
second host runtime/toolchain. SSH is useful for initial administration or
terminal-only access, but it does not supply the phone app's Remote Link
pairing, conversation, Git, and notification experience by itself. Building a
second relay or public HTTP gateway would duplicate infrastructure already
available.

Recommended implementation phases, without creating or scheduling objectives:

1. Add coordinator credential/config storage and control-plane client, with
   Linux host bootstrap and secure headless enrollment/approval.
2. Add gateway/helper supervision, admissions/leases, reconnect/reconciliation,
   local-plus-directory grant administration, and attention forwarding.
3. Add Linux provider credentials and service-environment fixes, then mobile
   host-neutral wording while preserving existing pairing contracts.
4. Ship Linux payload/install/update support and the systemd user service,
   including session-preserving coordinator restart behavior.
5. Validate the complete flow on a clean Linux VM and a physical phone. Add
   multiple saved hosts as a separate product slice if concurrent Mac/Linux
   selection is desired.

Release acceptance must cover: first enrollment and rejection/expiry paths;
each grant level; terminal detach/reattach; shell and Claude/Codex creation;
conversation prompts and uploads; Git read/mutation/AI flows; background sealed
alerts; cellular relay access with inbound ports closed; reachable-LAN operation;
network and service restarts; lease expiry/re-admission; revocation during an
active stream; and upgrade/rollback without accidental session termination.
Repeat focused Rust/helper tests on Linux and retain macOS/mobile regression
coverage.

## Evidence and limits

This assessment follows current repository source rather than historical
ICE/TURN plans. Remote Link v1's WSS/LAN implementation is the relevant path.
Source references above establish existing components and explicit gaps.
Only documentation was added for this objective. Relative documentation links
were checked for local targets. No application tests, Linux build/runtime,
current CI result, cloud deployment health, live APNs/Gemini call, or physical
phone acceptance was verified. The existing CI matrix and operations documents
describe intended checks/deployment architecture, not newly observed live state.
