# Remote access security findings

Reviewed 6 September 2026. This document records the security findings from the Latch Desktop / Latch Mobile QR-linking review and provides input for a remediation-planning mission. It is not a remediation implementation or evidence that a deployed system has been exploited.

Tracking: Overlord mission **coo:949 — Plan remediation of Latch remote access security findings**, objective `coo:949.9y5b`. Created as a draft; execution has not started.

## Scope and evidence

The review covered QR enrollment, local device authorization, Noise-authenticated gateway forwarding, live conversation permissions, and control-plane TURN credential issuance and revocation. The four findings below were established by tracing source across their enforcing components. No live malicious-service, terminal-access, or relay-abuse exploit was executed.

The initial review began at `2ac99b3879a226cff89a8802c6b4bc914df362a4` and rechecked security controls at `616016c15f91b68a54a44273f09216a3234a05b8`. On preparation of this document, HEAD was `5dbbb6887b5cf4d4831802f7e53aa8642ede785c`; the six source files containing the principal controls below were unchanged from the prior recheck. No affected public-release range or deployed-binary version has been established. Revalidate against the implementation revision before planning fixes.

Severity describes the supported impact and prerequisites, not a formal CVSS score. The two additional concerns are explicitly separated from the four security findings.

| ID | Finding | Severity | Essential prerequisite |
| --- | --- | --- | --- |
| SEC-01 | Cloud directory data can become a local terminal grant during QR enrollment | Medium, with high-impact consequences | Compromised configured control plane and an active local pairing window |
| SEC-02 | An existing conversation retains action permissions after downgrade to Observe | High | Previously authorized device with an open conversation socket |
| SEC-03 | Disposable accounts bypass per-device TURN issuance limits | Medium | Public API access and a configured, funded TURN service |
| SEC-04 | Failed TURN revocation loses its retry handles | Low | Previously issued credentials and a provider/network failure during revocation |

## SEC-01: Cloud directory data becomes local terminal authority

### Expected boundary

The Mac owner should authorize the specific phone identity and grant associated with the intended QR exchange. A compromised directory should not independently choose a key that the Mac will trust for terminal access.

### Observed implementation

During enrollment, Desktop polls the directory and selects the first device ID absent from its earlier snapshot. It passes that row's public key and permission to local confirmation, together with the pairing secret already held by the Mac. A missing directory permission defaults to Control. Rust confirmation persists the device in the local allowlist. Desktop displays the comparison phrase after that write; no required owner approval precedes it.

A malicious configured service can supply an attacker-owned key during the pairing window. The Mac supplies the secret on that key's behalf and writes the grant. If the attacker subsequently establishes a transport connection, normal Noise authentication will correctly recognize the attacker key as locally authorized.

This requires a compromised configured service and an active pairing window. It is not a claim that an arbitrary unpaired internet caller can directly access any Mac. Phone-side pinning authenticates the Mac to the phone; it does not authenticate this cloud-selected phone key to the Mac owner.

### Source evidence

- [Desktop directory selection and confirmation](/Users/jake/Development/Cooperativ/Latch/apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessController.swift:837): `watchForEnrollment` and `completeEnrollment`.
- [Local allowlist persistence](/Users/jake/Development/Cooperativ/Latch/crates/latch/src/cli/remote_access.rs:1457): `confirm_pairing` validates the supplied secret and stores the supplied device key.
- [Cloud QR confirmation](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/api.ts:455): the current exchange submits the raw QR secret to the service.

### Remediation direction and acceptance criteria

Bind an enrollment proposal to the exact pairing ID, both endpoint keys, and requested grant. Require trusted Mac-side approval of matching comparison information before committing the local grant, or design an end-to-end authenticated enrollment exchange that enforces the same boundary. A proof derived solely from a secret disclosed to the service does not protect against that malicious service.

The plan should establish how cancellation, expiry, concurrent proposals, and interrupted confirmation behave. Mobile should report completed pairing only after the Mac acknowledges authorization.

Required regressions: fabricated directory rows cannot persist grants; mismatched pairing IDs or keys cannot be substituted; approval precedes persistence; cancellation and expiry cannot authorize a pending proposal; a legitimate approved flow still completes.

## SEC-02: Permission downgrade leaves existing conversation actions authorized

### Expected boundary

After the Mac owner lowers a device to Observe, that device should no longer submit actions requiring Interact or Control through an already-open conversation.

### Observed implementation

The conversation WebSocket route requires only Observe. At connection establishment, the proxy forwards the device's full initial grant and the Conversation Hub stores that grant in the subscriber. Subsequent actions are checked against the stored grant.

The proxy periodically reloads the device record, but asks only whether its current permission still satisfies the route's minimum. A downgrade from Interact or Control to Observe still satisfies the conversation route, so the socket remains open and the subscriber retains its earlier authority. Accepted actions proceed to the connector.

Full revocation and withdrawal of terminal Control have separate working checks. A newly opened Observe conversation also refuses writes. Those controls do not cover the already-open conversation case. The demonstrated source path is continued conversation action authority, not an assertion that every terminal socket survives revocation.

### Source evidence

- [Conversation route minimum](/Users/jake/Development/Cooperativ/Latch/crates/latch/src/cli/serve/routes.rs:118).
- [Proxy's live grant check](/Users/jake/Development/Cooperativ/Latch/crates/latch/src/cli/remote_access.rs:2554).
- [Action authorization using the stored subscriber grant](/Users/jake/Development/Cooperativ/Latch/crates/latch/src/conversation/hub.rs:714): `begin_action`.

### Remediation direction and acceptance criteria

Prefer a focused change that closes a connection when permission drops below the grant injected at establishment, allowing reconnection with the reduced grant. Compare this with dynamic grant propagation and per-action reauthorization only if immediate invalidation requirements justify the added coordination.

Specify the enforcement latency and treatment of already-running actions separately from new actions. The current periodic check is not instantaneous; the plan must make that boundary explicit.

Required regressions: open an Interact conversation, downgrade to Observe, and verify subsequent privileged actions are refused; repeat for Control-to-Observe; verify read-only reconnection, full revocation, and terminal permission withdrawal. Exercise the real proxy-to-hub boundary rather than only a newly constructed Observe subscriber.

## SEC-03: Disposable identities bypass relay issuance budgets

### Expected boundary

Operator-funded relay use needs an entitlement and aggregate abuse controls that cannot be reset merely by creating another account or device.

### Observed implementation

The public account endpoint issues an account credential without prior authentication. That credential can register both devices and establish their cloud pairing. Accounts default to relay enabled. An active pairing, an enabled account, and configured provider are sufficient to reach TURN issuance. The per-device limiter is keyed to identities the caller can replace.

An internet caller can create disposable accounts and pairs to obtain fresh issuance budgets without controlling a victim Mac. The supported impact is potential operator resource and cost abuse. This does not bypass a Mac's Noise identity check or local terminal grant.

Actual deployment edge controls, provider quotas, and spending limits were not inspected. They may reduce exposure and must be inventoried before assigning deployment-specific urgency.

### Source evidence

- [Anonymous account issuance](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/api.ts:199).
- [TURN eligibility and issuance](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/api.ts:768).
- [Default relay entitlement in the memory store](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/store/memory.ts:76); reconcile persistent-store defaults and deployed configuration during planning.

### Remediation direction and acceptance criteria

Separate account/device registration from funded relay entitlement. Define appropriate enrollment abuse controls and shared issuance/resource budgets, including a service-wide circuit breaker. Merely adding another limit keyed to freely created accounts is insufficient. Preserve legitimate QR linking and document how entitled devices obtain relay access.

Required regressions: fresh devices and accounts cannot reset the protected aggregate budget; legitimate entitled pairings work; disabled or unentitled accounts are denied; concurrent requests cannot overspend a shared issuance budget. Validate application behavior with a fake provider, and verify external spending controls separately without generating abuse traffic.

## SEC-04: TURN revocation loses durable retry state

### Expected boundary

Disabling relay access or revoking a device should stop new issuance and retain enough durable state to retry revocation of issued credentials when the provider is unavailable.

### Observed implementation

The API changes local account/device/pairing state, then takes credential usernames from the store and calls the provider. The PostgreSQL take operations use `DELETE ... RETURNING`. If a provider request fails, the usernames have already been removed. A subsequent retry cannot recover those handles from the store. Concurrent provider requests can also leave only part of a batch successfully revoked.

A previous credential holder may retain residual TURN use after a failed revocation. The configured credential lifetime defaults to 120 seconds and allows up to 3600 seconds. Provider behavior for existing allocations needs separate verification; credential expiry alone is not proof of an exact allocation-termination deadline. This finding does not restore the device's revoked local Mac grant.

### Source evidence

- [Account relay disable and provider revocation](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/api.ts:234), with device and pairing revocation following the same pattern.
- [Destructive username retrieval](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/store/postgres.ts:531).
- [Configured credential lifetime](/Users/jake/Development/Cooperativ/Latch/services/control-plane/src/config.ts:114).

### Remediation direction and acceptance criteria

Retain a durable revocation outbox until provider acknowledgment or a justified terminal expiry condition. Use idempotent retries with bounded backoff, retain partial-failure state, and reconcile issuance already in flight when local permission is withdrawn.

Required regressions: provider timeout, partial batch failure, and process restart preserve retryable revocations; eventual success clears them; expired work is retired deliberately; new issuance is denied after local disable; concurrent issuance cannot escape reconciliation.

## Additional concerns requiring explicit validation

### VAL-01: Proxy cleanup can fail open after local storage errors

The proxy spawns forwarding tasks, then has fallible device-store and audit operations before its explicit task-abort calls. A local storage/read/write failure can return early and detach forwarding tasks without continuing grant checks. This is a source-confirmed cleanup defect; a remote attacker trigger was not established.

Plan fault-injection tests for device-store reads and revocation-audit writes while traffic is active. Prefer scoped forwarding futures or abort-on-drop ownership so every exit stops forwarding, with logging after enforcement. [Cleanup ordering](/Users/jake/Development/Cooperativ/Latch/crates/latch/src/cli/remote_access.rs:2548).

### VAL-02: Mobile loopback caller isolation is unverified

The phone's loopback gateway shim accepts requests without a per-listener caller capability and forwards using the phone's paired identity. Whether a separate iOS app/process can reach it in the supported deployment was not tested. Do not classify this as a demonstrated cross-app exploit without that platform evidence.

Validate reachability using a controlled second app and inspect the HTTP/WebSocket boundaries. Consider a random per-listener capability verified and removed before forwarding. Preserve ordinary app networking while ensuring another local caller cannot borrow the paired identity. [Loopback listener](/Users/jake/Development/Cooperativ/Latch/apps/LatchMobile/Sources/LatchMobileKit/GatewayTransport.swift:398).

## Remediation-planning deliverable

Produce a separate plan that maps every SEC and VAL identifier to its current status, evidence, proposed design, affected components, dependencies, regression tests, rollout/migration needs, and measurable completion criteria. Revalidate findings before prescribing changes; record any finding already fixed or contradicted by newer source.

Prioritize SEC-02 and SEC-01 before broader deployment, assess SEC-03 against actual relay exposure, and include SEC-04 and fail-closed cleanup in the security work. Decide VAL-02's severity after validation. Preserve Mac-owned grants, phone-side identity pinning, authenticated encryption, fixed gateway routing, and terminal ownership rules.

Keep the intermittent cellular ICE investigation as a separate workstream. Its logs establish connectivity failures and occasional authenticated success; they do not reproduce these vulnerabilities. Broad transport refactoring is not a prerequisite for the focused security fixes. Identify shared lifecycle dependencies without making security remediation contingent on a transport rewrite.

The requested mission is planning only: its output should be a reviewable Markdown plan and ordered implementation work packages, not product-code changes, infrastructure changes, or automatically launched implementation missions.
