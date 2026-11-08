# Linux remote host: implementation plan

Plan for `coo:1255.dg7k`, 7 November 2026. Builds on the assessment in
[FEATURE_LINUX_REMOTE_HOST.md](FEATURE_LINUX_REMOTE_HOST.md); read that first
for what already exists and why.

**Review amendment (coo:1255.2mbq):** Read
[REVIEW_LINUX_REMOTE_HOST.md](REVIEW_LINUX_REMOTE_HOST.md) before implementation.
It supersedes the QR-only pairing decision (D6), the wording-only mobile scope
(P7), and the assumption that recovery requires no control-plane changes.
It also adds prerequisite gates for concurrent administration, session lifetime,
bootstrap recovery, and coordinated upgrades. The original phase detail below
is retained as the baseline being reviewed, not an approved final specification.

## Goal

A Linux server running Latch is paired with, and used from, the existing Latch
mobile app with the same capabilities as a Mac running Latch Desktop:
sessions, terminal, conversations, uploads, directories, agent launches, Git
(including AI commit messages), and background attention notifications, over
the existing relay and control plane. The server runs headless and unattended
under one Unix user.

### Non-goals for this plan

- **Pairing one phone with a Mac and a Linux host simultaneously.** The phone
  stores one `PairedDeviceRecord`; a host switcher is a separate mobile
  project (see [Deferred](#deferred-work)).
- **Moving Latch Desktop onto the new Rust coordinator.** The design allows it
  later; Desktop keeps its Swift coordinator for now.
- **New cloud infrastructure.** The control plane and relay are reused as is.
- **Multi-user hosts.** One service per Unix user, each with its own identity.

## Design decisions

| # | Decision | Rationale |
| --- | --- | --- |
| D1 | Port Desktop's coordinator to Rust as a new `host` module in the **`latch-remote`** crate, run as `latch-remote host <subcommand>`. `latch remote-host <subcommand>` resolves the sibling `latch-remote` binary (as gateway launch already does) and forwards to it. | `latch-remote` is already the process boundary for remote-facing parsers. Keeping control-plane JSON parsing and helper supervision there leaves the ordinary `latch` crate free of network-facing code ([ARCHITECTURE_RULES](ARCHITECTURE_RULES.md)). A Swift-on-Linux port would need replacements for Keychain/AppKit and a second toolchain. |
| D2 | The coordinator drives **unchanged** helpers: `latch-remote --gateway-serve` and `latch-remote --link-serve`, using the existing stdin/stdout JSON-line protocol (`crates/latch-remote/src/link/ipc.rs`). | The protocol is already the contract Desktop uses. It keeps one helper per peer and single-use admissions on inherited pipes. |
| D3 | Host credentials (`address`, `accountToken`, `deviceID`, `deviceToken`, `publicKey`, the same shape as Swift `HostEnrollment`) are stored in `$LATCH_HOME/remote-access/host-credentials.json`. Mode `0600` inside a `0700` directory, written atomically, with ownership and mode checked on read like `identity.key`. Config (control-plane URL, host name) goes in a separate non-secret `host.json`. | Matches the existing Linux Noise-key storage policy. Secrets never go in argv, env dumps, logs, or status output. The owner invitation is read from a TTY prompt or stdin only. An optional `--credentials-dir` supports systemd `LoadCredential=`. |
| D4 | Each Linux host claims its **own** one-use owner invitation, which creates its own control-plane account (`POST /v1/accounts/claim` always creates an account). | Works with today's API. The phone pairs per host through enrollment, so sharing an account with the Mac is not required for single-host parity. If a shared account becomes necessary, add a "register with an existing account token" path later. |
| D5 | The service and the admin commands are separate processes. Admin commands (`pair`, `grant`, `revoke`, `disable`) change state directly and then tell a running service to reconcile with **SIGHUP**, using the PID recorded under the service's single-instance lock. The service also reconciles whenever the local grant-store revision changes. | Pairing needs an interactive operator TTY, but the service is unattended. SIGHUP works with `systemctl --user reload` and without systemd. The revision watch covers a missed signal. |
| D6 | Pairing approval needs an interactive TTY and shows the phone name, requested permission, and comparison code, with a default answer of *No*. The QR code is printed to the terminal; `--qr-png <path>` (written `0600`) is optional. | Keeps the QR-only secret and explicit owner approval. Non-interactive auto-approval would weaken the enrollment security model. |
| D7 | Revoke changes local state first and then mirrors remotely, as Desktop does. When the remote mirror fails, the coordinator records a **pending mirror** in `host-state.json`, and the service retries it with backoff. | Revocation fails closed locally. Desktop only shows an error when the mirror fails; a headless host needs a retry. |
| D8 | Linux release archives are signed with an **Ed25519 (minisign-format) signature** over the archive, and the public key is pinned in the updater and installer. | Developer ID and notarization are Apple-only. The one-payload rule needs a publisher-verification step that works on Linux. |
| D9 | The first supported target is `x86_64-unknown-linux-gnu`, built on Ubuntu 22.04 (glibc 2.35 baseline), with OpenSSL `native-tls` as now. `aarch64` is added as a follow-on matrix entry. | This is the narrowest target that covers typical servers. The baseline is stated explicitly instead of assumed. |
| D10 | Live sessions must survive a coordinator restart. Under systemd, each new `latchd` session is started in its own transient user scope (`systemd-run --user --scope`), so the service's `KillMode=control-group` does not kill it. | With the default cgroup, `systemctl restart` would kill every PTY. `KillMode=process` would orphan the gateway and helpers instead. Phase 5 starts with a spike to confirm this. |

## Component map: Swift source to Rust target

| Desktop (Swift) | Linux (Rust, new) |
| --- | --- |
| `ControlPlaneHost.swift`: `ControlPlaneHostAPI` (claim, devices, rotate-key, enrollments open/complete/cancel, pairings set/revoke, remote-links, relay-admissions, lease renew, account relay toggle, attention) | `latch-remote/src/host/api.rs`: typed `reqwest` client with the same endpoints, version checks, `wss://`-only admissions, stale-credential and permanent-attention-refusal error classification |
| `ControlPlaneHost.swift`: `enrollment()`, `freshEnrollment()`, key rotation and stale re-enroll, URL `normalize` | `host/bootstrap.rs` + `host/credentials.rs` |
| `RemoteAccessModels.swift`: `pairingDocument()` (sorted-key JSON: `version, controlPlane, enrollmentId, hostPublicKey, admissionCode, enrollmentSecret, expiresAt, macName`) | `host/pairing.rs`: same document byte-for-byte, with a 256-bit secret from the OS RNG, rendered by the `qrcode` crate |
| `RemoteAccessSupervisor.swift`: `RemoteEnrollmentSupervisor` (`enrollment_pending` → decision; `enrollment_committed` → mirror, receipt equality check → `enrollment_mirrored`) | `host/enroll.rs` |
| `RemoteAccessSupervisor.swift`: `RemoteGatewaySupervisor` (wait for `gateway_ready`), link `RemoteAccessSupervisor` (`status`, `lease_started` → renew → `lease_extension`; `admission_needed` → `admission`; `shutdown`) | `host/supervise.rs` |
| `RemoteAccessController.swift`: assignment supervision, `reconcile`, `superviseLink`, `CrashLoopBackoff`, idle and 24 h poll cadence, retaining existing assignments to avoid wasting admissions | `host/service.rs` |
| `AttentionForwarder.swift` (2 s poll, local→directory id map, ack on success/unknown/permanent refusal, retain on transient failure) | `host/attention.rs`, which calls the `latch` library's spool functions in-process instead of shelling out |
| `RemoteAccessController.grant/revoke` | `host/admin.rs` |
| Sleep/power policy | Not ported (not applicable to servers) |

## Phases

Each phase can be merged on its own and leaves `main` releasable. The phase
numbers give the order of dependencies; items marked **∥** can run in
parallel with the phase before.

### Phase 0: Portability fixes (∥, small)

1. `crates/latch/src/cli/create.rs::login_shell_path`: when `SHELL` is unset
   or not absolute, fall back to the passwd entry (`getpwuid_r`) and then
   `/bin/sh`, instead of `/bin/zsh`. Add a unit test.
2. Confirm the Ubuntu CI job runs the `latch-remote` and `remote_access` tests
   (`.github/workflows/ci.yml` matrix). Add any that are gated on macOS but
   portable.
3. Make the registration platform a parameter of the API client (`linux` /
   `macos`), not a literal.

**Exit:** a session created with `SHELL` unset on Ubuntu CI launches a valid
shell.

### Phase 1: Control-plane client, credentials, bootstrap

1. Add `host/{mod,api,credentials,config,bootstrap}.rs` to
   `crates/latch-remote` and add `reqwest` (workspace `native-tls`, `json`) to
   its manifest.
2. Add the `latch remote-host setup --control-plane <url> --name <host>`
   command:
   - It enables remote access and creates the identity through the existing
     `remote_access` library calls, the same as `latch remote-access enable`.
   - It reads the owner invitation from a TTY or stdin and validates the
     `inv_<32hex>.<64hex>` format.
   - It claims the account, registers the device with `platform: linux` and
     `role: host`, and persists the credentials.
   - Running it again is idempotent: when the key changed it rotates the key,
     and on a stale credential it clears the credentials and asks for a new
     invitation.
3. Add `latch remote-host status [--json]` to report configuration,
   enrollment, identity fingerprint, and service liveness, without secrets.
4. **Tests:** a local mock HTTP server covering each endpoint and its error
   classes, a bootstrap/rotation/stale-credential table ported from
   `ControlPlaneHostTests.swift`, file mode and ownership refusal tests, and a
   check that no secret appears in `status --json`.
5. Add the coordinator's request and response shapes to the contract checks
   that `scripts/check-remote-link-contract.sh` already runs for Swift.

**Exit:** against a local control plane (`TEST_DATABASE_URL`-gated test),
`setup` registers a `linux` host, and running `setup` again makes no changes.

### Phase 2: Headless pairing

1. Add `latch remote-host pair [--permission observe|interact|control] [--qr-png <path>]`:
   - It opens an enrollment and verifies the response: `version == 1`, the
     host key equals the local key, and the relay URL uses `wss://`.
   - It builds the QR document (D6), prints the QR code, and prints the
     expiry.
   - It spawns `latch-remote --link-serve` with `purpose: "enrollment"`.
   - On `enrollment_pending`, it prompts with the name, permission, and
     comparison code.
   - On `enrollment_committed`, it calls `POST /v1/enrollments/:id/complete`,
     requires the receipt fields to equal the local commit, and writes
     `enrollment_mirrored`.
   - Interruption, expiry, or rejection triggers `DELETE /v1/enrollments/:id`
     and stops the helper.
   - On success, it sends SIGHUP to the service.
2. **Tests:** a fake helper binary emitting scripted lines covers approve,
   reject, receipt mismatch (fails closed and does not write the mirror line),
   expiry, Ctrl-C cleanup, and refusal on a non-TTY. A golden test checks the
   QR document bytes against the Swift encoder output.

**Exit:** the phone app scans the terminal QR code from a Linux VM and
completes enrollment. The phone stays connected only after Phase 3.

### Phase 3: Host service (supervision, leases, attention)

1. Add `latch remote-host serve` (foreground, for systemd):
   - **Lock:** a single-instance lock in `$LATCH_HOME/remote-access/`, with the
     PID stored for D5.
   - **Gateway:** spawn `latch-remote --gateway-serve --latch-bin <sibling latch>`,
     wait for `gateway_ready`, and restart with crash-loop backoff. The
     gateway runs only while at least one assignment exists, as on Desktop.
   - **Assignments:** list `GET /v1/remote-links` and keep `version == 1,
     grantRevision > 0`. An assignment with the same peer key and revision is
     retained and does not use a new admission; changed ones are stopped and
     restarted. Poll every idle interval when no links exist, every 24 h when
     links are active, and immediately on SIGHUP or a grant-revision change.
   - **Per-peer link:** spawn a helper with the session config; renew leases
     on `lease_started`; answer `admission_needed` with a fresh admission; on
     exit, back off, get a new admission, and restart. Send `shutdown` on stop.
   - **Attention:** forward the spool every 2 s, following the
     `AttentionForwarder` semantics.
   - **Pending mirrors:** retry D7 entries.
   - **Status:** write content-free `host-status.json` (gateway state and each
     peer's link status) for `status`.
   - **Signals:** SIGTERM shuts down cleanly (helpers get `shutdown`, then a
     timeout, then a kill); SIGHUP triggers reconciliation.
2. **Tests:** port the reconcile and backoff cases from `RemoteAccessTests.swift`.
   Process-level tests with fake gateway and link helpers cover lease renewal,
   admission refresh, helper crash loops, and SIGHUP reconciliation. Attention
   tests cover forwarded, unknown-device, permanent-refusal, and transient
   cases.

**Exit:** on a Linux VM, the paired phone lists sessions and opens a terminal
over cellular with no inbound ports open. Killing the network and restoring
it reconnects without operator action, and lease expiry is renewed silently.

### Phase 4: Administration with mirroring

1. Add `latch remote-host devices [--json] | grant <id> <perm> | revoke <id> | relay on|off | disable | forget`:
   - `grant` changes the local grant, then `POST /v1/pairings`.
   - `revoke` revokes locally, then `DELETE /v1/pairings/:id`. When the remote
     call fails, it records a pending mirror (D7).
   - `relay on|off` maps to `PATCH /v1/account`.
   - `forget` clears the credentials after confirmation.
   - Every command except `devices` sends SIGHUP to the service.
2. When host credentials exist, make `latch remote-access grant/revoke` print a
   warning that they change local state only and point to the `remote-host`
   equivalent.
3. **Tests:** ordering (local before remote on revoke), pending-mirror
   persistence and retry, and that revocation during an active stream ends the
   stream (an existing gateway behavior, re-verified on Linux).

**Exit:** a revoke from the server ends an active phone stream within the
existing bound, and the control plane shows the pairing removed.

### Phase 5: Runtime parity on Linux (∥ with Phases 3–4)

1. **Gemini credentials:** in `crates/latch/src/cli/commit_message.rs`, on
   non-macOS builds, load the key from `$LATCH_HOME/credentials/gemini.key`
   (`0600`, with an ownership check) or from a file named by
   `LATCH_GEMINI_API_KEY_FILE` (for systemd `LoadCredential=`). Credential
   setup reads the key from a TTY or stdin. Status reports `configured` or
   `unconfigured` without showing the value.
2. **Session-lifetime spike (D10):** confirm in a VM that
   `systemctl --user restart` kills sessions under the default unit. Then
   implement scope placement in the session-creation path when
   `INVOCATION_ID`/systemd is detected, and fall back to the current behavior
   without systemd. Run the `latchd` security-review skill on this change,
   because it touches session launch.
3. **Service environment:** document and check what the service needs: `PATH`
   for agent CLIs, `SHELL`, Git/SSH credentials (agent socket or deploy keys),
   and `HOME`/`LATCH_HOME`. `latch remote-host doctor` checks each item and
   reports what is missing.

**Exit:** an AI commit-message draft works from the phone against the Linux
host, and `systemctl --user restart latch-remote-host` leaves running
sessions attachable.

### Phase 6: Distribution, install, update, service unit

1. `scripts/release-cli.sh` and `.github/workflows/release-cli.yml`: add the
   `x86_64-unknown-linux-gnu` target on `ubuntu-22.04`. The archive contains
   `latch`, `latch-remote`, and pinned `latchd`, the manifest, the checksum,
   and the minisign signature (D8). The signing key is held in a CI secret.
2. `scripts/install-cli.sh`: detect Linux and the architecture, download,
   verify the signature and checksum, extract with `tar`, and install to
   `~/.local/bin` (or a given prefix). Extend
   `scripts/test-install-verification.sh` and `test-release-archive.sh` to
   cover Linux.
3. `crates/latch/src/cli/update/release.rs::host_target`: add
   `("linux","x86_64")`. Update verification uses the pinned Ed25519 key
   instead of codesign, keeping the same staging and sibling-rollback
   behavior.
4. `latch remote-host install-service`:
   - It writes `~/.config/systemd/user/latch-remote-host.service` with an
     absolute `ExecStart`, explicit `Environment=` for `PATH`, `SHELL`, and
     `LATCH_HOME`, `Restart=on-failure`, `ExecReload=/bin/kill -HUP $MAINPID`,
     and `TimeoutStopSec`.
   - It runs `daemon-reload` and `enable --now`, and prints the
     `loginctl enable-linger` command when lingering is off.
   - It never needs root.
5. Update `docs/ARCHITECTURE_RULES.md` ("Distribution is one signed payload")
   to define Linux publisher verification.

**Exit:** on a clean Ubuntu 22.04 and 24.04 VM, the one-line installer,
`setup`, `pair`, and `install-service` complete the flow. A reboot brings the
host back online, and `latch update` upgrades and rolls back without killing
sessions.

### Phase 7: Mobile wording (∥, small)

1. Replace "Mac" in the user-visible strings in `apps/LatchMobile/Sources`
   with host-neutral wording ("computer", or the host name). Pairing help
   should say "run `latch remote-host pair` on your server or open Latch on
   your Mac".
2. Keep the wire field `macName` and the stored `PairedMac` records unchanged.
   Add a test that a record from an older build still decodes.
3. Follow the "skip simulator verification for small UI tweaks" practice:
   edit the strings and let the user verify.

**Exit:** no user-facing copy assumes the host is a Mac.

### Phase 8: Validation and operator docs

1. **CI:**
   - Add an Ubuntu job that runs the `host` integration tests against the
     control plane with a Postgres service container (replacing the
     `TEST_DATABASE_URL` gate in CI only).
   - Add a scripted end-to-end test: a host coordinator on Linux paired with
     a headless test client that uses `latch-transport` as the phone.
2. **Manual acceptance on a Linux VM with a physical phone** (the list from the
   assessment):
   - first enrollment, rejection, and expiry
   - each grant level
   - terminal detach and reattach
   - shell, Claude, and Codex session creation
   - conversation prompts and uploads
   - Git reads, mutations, and AI commit messages
   - background sealed alerts
   - cellular relay with inbound ports closed
   - LAN operation where mDNS is reachable, and relay operation when LAN
     advertising fails
   - network and service restarts
   - lease expiry and re-admission
   - revocation during an active stream
   - upgrade and rollback without killing sessions
   Repeat the macOS and mobile regression suites.
3. **Docs:** write `docs/LINUX_REMOTE_HOST.md` (operator guide covering
   install, setup, pairing over SSH, service management, credentials, firewall
   and network requirements, troubleshooting, and re-enrollment after key
   loss), and update `docs/REMOTE_LINK_OPERATIONS.md`.

**Exit:** the full acceptance list passes and is recorded with dates and the
versions tested.

## Sequencing summary

```text
P0 ─┐
    ├─ P1 ── P2 ── P3 ── P4 ─┐
P5 ─┴──────────(∥ P3–P4)─────┼─ P6 ── P8
P7 ──────────────────────────┘
```

The critical path is P1 → P2 → P3 → P6 → P8. A first usable build is the end
of P3: run from source under a hand-written unit, paired and connected. P4–P6
make it administrable, durable, and installable.

Rough size: P0 S · P1 M · P2 M · P3 L · P4 S–M · P5 M (D10 is the main
unknown) · P6 L · P7 S · P8 M.

## Risks and open questions

| Risk / question | Mitigation or owner decision |
| --- | --- |
| Session survival across coordinator restarts (D10) depends on systemd scope behavior under user managers and cgroup v2 | Spike first in Phase 5. If scopes are not viable, fall back to a `latchd` launcher that double-forks into a separate user unit. |
| The Linux signing key becomes a new secret and trust root | Store it in CI secrets, document rotation, and pin a key ID in the updater so the key can be rotated. |
| A separate account per host (D4) may matter for future features, such as a multi-host phone | Revisit when the multi-host mobile project starts. Adding an account-token import later is additive. |
| Interactive pairing over SSH requires a capable terminal for the QR code | Provide the `--qr-png` fallback and print the expiry and enrollment ID. Never print the secret as text. |
| `native-tls` ties the binary to the system OpenSSL | State the supported distributions (Ubuntu 22.04+, Debian 12+). Consider `rustls` later for a static build. |
| Duplication with Desktop's Swift coordinator | Accept it for now. Later, Desktop can call `latch-remote host serve` and drop the Swift supervision code. |

## Deferred work

- Let the phone keep several paired hosts and switch between them. This needs
  a collection in `PairedDeviceStoring`, host selection in `AppModel`, and
  per-host caches and notification routing.
- Move Latch Desktop onto the Rust coordinator.
- Add an `aarch64-unknown-linux-gnu` release target.
- Optional packaging: a `.deb` package and a container image for the host.
