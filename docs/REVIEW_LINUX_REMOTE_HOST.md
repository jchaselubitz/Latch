# Linux remote host: plan review and remote pairing recommendation

Review for `coo:1255.2mbq`, 7 November 2026. This amends
[the implementation plan](PLAN_LINUX_REMOTE_HOST.md); it does not implement it.

## Recommendation

Keep the Rust coordinator, existing encrypted transport, and existing cloud
services. Resolve the following gaps before treating the plan as ready for
execution. In particular, remote pairing should work through an authenticated
SSH terminal and a first-class **Paste pairing code** action on the phone.
No browser on the server, public host port, or new web service is needed.
Terminal QR scanning remains an optional convenience when a second screen exists.

## Significant findings

### 1. QR-only pairing and wording-only mobile work do not meet the remote requirement

**Evidence:** D6/P2 require terminal QR or a PNG and prohibit printing secret
text. However, mobile already has `ManualCodeEntry`, feeding the same
`PairingPayload.parse` path as the scanner. It is hidden inside the scanning
flow, whose `beginScanning()` requests camera permission. See
[PairingView.swift](../apps/LatchMobile/App/LatchMobile/PairingView.swift),
[PairingModel.swift](../apps/LatchMobile/Sources/LatchMobileKit/PairingModel.swift),
and [PairingPayload.swift](../apps/LatchMobile/Sources/LatchMobileKit/PairingPayload.swift).

**Impact:** scanning the screen of the phone running SSH is impossible. Moving a
PNG between machines is unnecessary friction. Changing labels alone leaves this
workflow awkward and dependent on camera UI.

**Resolution:** replace D6 with the SSH/paste flow below. “QR-only” should mean
that the independent enrollment secret stays out of the control plane; it need
not mean that a camera is its only carrier. Update protocol/privacy wording to
say out-of-band pairing material. Preserve the same 256-bit secret, host-key
pinning, five-minute expiry, single-use admission, Noise handshake, and explicit
comparison/approval. P2 and mobile pairing changes must ship together.

### 2. The permission option cannot work as described with today's phone

**Evidence:** `PairingModel.confirm()` requests `.control`, and the comparison
view displays Control. The helper's `validate_decision` requires the approved
permission to equal the proposal; permission is also bound into the comparison
code. See [enrollment.rs](../crates/latch-remote/src/link/enrollment.rs).

**Impact:** a host `--permission observe` cannot silently substitute Observe
for the phone's Control request without rejecting or breaking the exact-match
contract.

**Resolution:** add a phone permission selector before the handshake. Define
the host option as an exact expected permission for this enrollment; on mismatch,
reject with instructions to select the matching level and start a new attempt.
Display the actual proposed permission on both devices. Test all three levels
and mismatches. Do not enroll at Control and downgrade afterwards.

### 3. Enrollment completion needs a recovery contract, not just cancellation

**Evidence:** the helper calls `authorize_enrollment` before emitting
`enrollment_committed`; only after remote completion and a matching mirror does
it send `pairing_approved` to the phone. The control-plane complete endpoint
returns 409 when finalization has already happened, and cancellation cannot
undo a completed enrollment. See
[api.ts](../services/control-plane/src/api.ts) (`/v1/enrollments/:enrollmentId/complete`)
and [memory.ts](../services/control-plane/src/store/memory.ts)
(`finalizeRemoteEnrollment`, `cancelRemoteEnrollment`).

**Impact:** losing the HTTP response, killing SSH after local commit, or losing
the final phone receipt can leave local authority, cloud state, and phone state
disagreeing. The P2 instruction to DELETE on interruption is insufficient.

**Resolution:** add a durable enrollment-operation record keyed by enrollment,
controller key, permission and revision. Specify recovery at each boundary:
before local commit, after local commit, after cloud commit, and after phone
receipt. Prefer an authenticated idempotent completion/receipt lookup that
returns the original result only for an exact request match, with bounded
retention and conflict rejection. This is a change to the existing control
plane, not new infrastructure. Never treat a generic 409 as success.

Until completion is verified, prevent the incomplete enrollment from becoming
an ordinary usable session grant. On abandoned or unverifiable attempts, revoke
locally and durably queue remote pairing removal if it may have completed; cancel
only an uncommitted cloud enrollment. Re-pair after cleanup when the phone never
stored its receipt. Account for concurrent revoke so a retry cannot resurrect
access. Fault-inject every boundary, including a crash before the coordinator
records the helper's local commit; reconcile from durable local evidence on
startup. This may require a narrowly scoped helper/store change, so D2's
“unchanged helpers” must be conditional on the recovery design.

### 4. Atomic file replacement and SIGHUP do not serialize administration

**Evidence:** D5 allows independent writers. Local `grant`/`revoke` perform
read-modify-write in
[devices.rs](../crates/latch/src/cli/remote_access/devices.rs); atomic rename
protects a single write, not the whole transaction. D7 queues only failed
revokes, although failed grant mirrors also leave revisions inconsistent.

**Impact:** concurrent pair/grant/revoke operations can lose updates; a delayed
grant retry can overwrite a later revoke. A crash between mutation and recording
the retry loses the repair intent. A stale PID file can signal another process.

**Resolution:** serialize all grant writers with a shared store lock or one
authoritative local admin service, including legacy `remote-access` commands
and enrollment helpers. A warning on the legacy CLI is insufficient. Use a
durable desired-state reconciliation mechanism for every mirrored mutation,
ordered by device and monotonic revision, with revocation tombstones and startup
repair. Make its persistence atomic with authority changes or derive missing
work reliably from authority state. Use an owner-only local control channel or
systemd reload; validate process identity if retaining direct PID signaling.
Test simultaneous writers, disk failure, restart, stale retries and offline revoke.
Define `disable`, `forget`, and account relay-off precisely, including stopping
links and handling pending mirrors before credentials disappear.

### 5. Session survival and unattended boot are prerequisite experiments

**Evidence:** D10 chooses scopes before P5 proves them, while the critical path
omits that experiment. Latch requires a pinned daemon, owner-only IPC and stdin/FIFO
launch-secret handling in [ARCHITECTURE_RULES.md](ARCHITECTURE_RULES.md).
Systemd scopes manage externally created processes; service and scope launches
have different parentage and execution behavior. See the official
[systemd-run documentation](https://www.freedesktop.org/software/systemd/man/253/systemd-run.html).

**Resolution:** move the lifetime spike before finalizing P3. Verify daemon
detachment, readiness, inherited descriptors, launch secrets, environment and
cgroup membership using the actual launcher. Choose scopes or transient services
based on that evidence; double-fork alone is not a cgroup escape strategy.
Require no leaked helpers after restart and live terminal reattachment after
both coordinator and gateway failure. Detect the user manager and bus explicitly;
`INVOCATION_ID` alone is not proof the necessary capabilities exist.

The installer must detect lingering and report whether unattended boot is
actually configured. Lingering starts a user manager at boot and keeps it after
logout ([loginctl](https://www.freedesktop.org/software/systemd/man/249/loginctl.html)).
Do not promise universally rootless installation when host policy can prevent
enabling it. Test SSH logout and cold boot without first logging in. Reboot
acceptance means the host returns online; it does not mean PTYs survive reboot.

### 6. Bootstrap assumes an invitation and has a lost-token window

**Evidence:** `/v1/operator/owner-invitations` requires the operator credential;
`/v1/accounts/claim` consumes the invitation and returns an account token only
once. P1 only persists credentials after device registration. See
[api.ts](../services/control-plane/src/api.ts).

**Resolution:** document who provisions an invitation and how the user obtains
it; never require distributing the cloud operator credential to Linux hosts.
Persist the account claim result securely before registering the host, then
resume registration from that checkpoint. Distinguish authentication rejection
from network errors before discarding credentials. Define support/operator
recovery for a lost claim response and stale host identity, including cleanup
of abandoned accounts/devices. Successful reruns are not proof of crash-safe
idempotency. Separate-account-per-host is acceptable for the stated single-host
scope, but explicitly acknowledge separate entitlements and administration.

### 7. Signed archives alone do not establish a safe service upgrade

**Evidence:** P6 promises upgrades and rollback without killing sessions; the
architecture pins daemon protocol/version to the CLI. Replacing binaries does
not replace the already running coordinator, and old sessions retain old daemons.

**Resolution:** define an upgrade transaction: verify the complete payload,
stage a coherent version, switch paths, restart the coordinator/helpers, probe
health, and restore the old payload/service on failure. Establish which old
daemon protocols the new CLI can attach to. If cross-version compatibility is
not supported, explicitly defer upgrades while sessions are active rather than
promise transparent survival. Test upgrade and rollback with live sessions.

Specify detached signatures (an archive cannot contain its own final signature),
a trusted initial verifier/public key, signed target/version metadata, key
rotation and downgrade policy. Verify Linux runtime dependencies on clean
supported images, including OpenSSL and the pinned Linux `latchd` artifact.
Do not infer Debian support solely from a glibc baseline.

## Proposed remote pairing experience

These are proposed commands and UI, not existing functionality.

1. Over authenticated SSH, complete `latch remote-host setup`, then start the
   user service and confirm it is healthy. A first-run guide explains invitation
   acquisition separately from phone pairing.
2. Run `latch remote-host pair --format text --permission control` in an
   interactive TTY. Print the existing version-1 JSON pairing document as one
   selectable block, with host name, fingerprint and expiry outside the block.
   Keep QR as `--format qr`; never open a browser on the server. Do not add
   raw gateway-address or bearer-token entry.
3. On the phone choose **Pair a computer → Paste pairing code**, without a
   camera permission request. Paste explicitly from the SSH app's clipboard,
   choose the expected access level, review the host identity, then connect.
   Use the existing parser and its lifetime validation. Raw JSON is already
   compatible; do not require a new URI scheme for the first version.
4. Both devices show the transcript comparison code and exact permission.
   Confirm on the phone, return to SSH, compare, and explicitly approve there
   (default No). Show the phone's key fingerprint alongside its untrusted name.
   Never auto-approve possession of the pairing document.
5. Report success only after the matching cloud receipt and encrypted phone
   completion. Show whether the host service is online and immediately offer
   connection to the host. On expiry/cancel, invalidate the attempt and explain
   how to retry; after partial commit, run the recovery procedure above.

**Same-phone SSH requires explicit lifecycle work.** While switching back to the
SSH app, iOS may suspend Latch. Keep the short enrollment interaction alive with
bounded background time where available and handle expiration. Verify on a
physical phone, including a slow approval. If completion cannot finish before
suspension, fail with cleanup and a retry; do not assume foreground Swift state
survives. Reliable reconnect/resume would need a designed protocol change and
cannot be claimed from paste support alone. This acceptance gate belongs before
shipping remote pairing, not in optional UI polish.

**Secret handling:** text carries exactly the same enrollment secret as QR.
Reveal it only at the operator's explicit interactive request, not in status,
logs, journald, argv, environment, analytics or ordinary redirected output.
Do not automatically read the mobile clipboard. Clear transient input on
completion/cancel and avoid secret-bearing diagnostics. Explain that terminal
recording, scrollback and clipboard synchronization may retain the material;
expiry and single-use semantics limit that exposure. Do not promise terminal
output can be erased. Never upload the document to a paste service or encode
it into an ordinary HTTP query string.

A short human-entered code would be more convenient to type but cannot simply
replace the high-entropy secret. It would require a separate, reviewed rendezvous
and authentication design. Defer that option. A deep link can be considered later
with explicit confirmation and safe ingress validation; it is not needed to
make the first release usable remotely.

If the phone already has a paired host, show an explicit replacement flow and
explain the single-host limitation. Do not silently overwrite it; account for
old-key/grant cleanup and preserve the old pairing until the user deliberately
begins replacement. Multi-host switching remains a separate project.

## Revised execution order and acceptance gates

| Stage | Changes to the baseline | Required evidence |
| --- | --- | --- |
| Prerequisites | P0 plus early P5 lifetime spike; define mutation/recovery contracts and bootstrap checkpoints | Actual Linux launcher/cgroup experiment; approved failure-state transitions |
| Bootstrap | P1 with invitation runbook and crash recovery | Dropped responses and restart at claim/register boundaries |
| Pairing | P2 plus functional mobile work from P7, permission selection and enrollment recovery | Laptop SSH + phone, same-phone SSH switching, no camera permission, all grants, expiry, rejection and interrupted commits |
| Service/admin | P3 + P4 with shared writer serialization and ordered reconciliation | Offline grant/revoke, concurrent operations, reconnect, lease/attention behavior, active-stream revocation |
| Runtime/distribution | Remaining P5 + P6 with upgrade compatibility and lingering checks | Clean supported VMs, logout/cold boot, dependencies, signed install, active-session upgrade/rollback |
| Release acceptance | P8 and operator guide | Physical phone over cellular with inbound ports closed; existing macOS regression coverage |

The first development demo can still end at P3. Do not describe it as an
unattended usable release until admin/recovery, mobile switching, lifecycle and
distribution gates pass. P7 is no longer a small wording task, and P5's lifetime
work is on the critical path. No implementation estimates are asserted here.

## Review validation

Findings are grounded in the current plan and source linked above, with official
systemd documentation used for lifecycle semantics. Documentation links and
formatting were checked. No product code changed, and no Linux runtime,
physical-phone pairing, release, deployment, or fault-injection tests were run.
All proposed behavior and gates above remain implementation work.
