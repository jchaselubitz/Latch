# Latch Remote Access Implementation Plan

## Status

Proposed, and partly superseded.

The read-only terminal capability this plan lists as future protocol work was
designed and then removed: a session has exactly one human surface, and a
terminal connection requires the `control` grant. Observation is Conversation
Hub and `latch inspect`. See
[`DECISION_EXCLUSIVE_ATTACH.md`](DECISION_EXCLUSIVE_ATTACH.md); the rest of
this plan still stands.

This plan describes how an iOS client can securely attach to Latch sessions on
a user's Mac without requiring the user to configure SSH, router port
forwarding, dynamic DNS, or a VPN. The Mac remains the session host and source
of truth. Remote infrastructure is limited to discovery, connection
coordination, and an encrypted fallback relay when a direct connection cannot
be established.

## Product outcome

A user installs Latch on a Mac and the Latch mobile app on an iPhone, pairs the
devices once, and can subsequently:

- See the Mac and its available Latch sessions while away from the local
  network.
- Open a chat-oriented session view backed by Latch events and interactions.
- Fall back to a full terminal view when the session has no structured chat
  capability.
- Send messages, answer permission or question prompts, and control a terminal
  according to explicit per-device permissions.
- Connect without exposing tmux, the Latch state directory, or the existing
  plaintext gateway to the public Internet.
- Revoke a lost phone from the Mac and render its credentials unusable.

Direct peer-to-peer traffic is preferred. If network topology prevents a
direct connection, an end-to-end encrypted relay carries opaque traffic. The
relay never receives terminal plaintext or credentials that can decrypt it.

## Constraints and decisions

### The Mac remains the server

The Mac owns the process, tmux session, event ledger, authorization policy, and
session lifecycle. The mobile app is a remote client. A cloud service must not
become the system of record for session content.

### The existing gateway remains loopback-only

`latch serve` already provides the `/v1` HTTP and WebSocket API used by the
Remote SDK. It currently uses plaintext HTTP with a bearer token and is safe by
default only on loopback or inside a trusted tunnel. The remote-access feature
must not expose this listener directly or weaken its loopback default.

The desktop-side remote-access component connects to `latch serve` over
loopback and places an authenticated, encrypted transport in front of it.
Longer term, the gateway can be moved in-process behind a private interface,
but that is not required for the first release.

### Zero configuration requires connection coordination

A connection cannot always be made directly between a phone and a Mac because
one or both devices may be behind NAT, carrier-grade NAT, or a restrictive
firewall. The production design therefore uses:

1. An outbound control connection from the Mac.
2. An outbound control connection from the phone.
3. A rendezvous service that helps the paired devices attempt a direct path.
4. An end-to-end encrypted relay when a direct path cannot be established.

There may also be an advanced, explicitly unsupported initial mode using a
user-managed VPN or port forward, but it is not the primary product path.

### Device identity replaces shared bearer tokens

The existing gateway token is an internal hop credential. It must not be the
credential copied to or stored by the phone. Each paired device receives its
own cryptographic identity and authorization record so it can be individually
audited and revoked.

### Ship chat first, retain terminal compatibility

The mobile experience should use the existing capability discovery contract:

- Use events and interaction endpoints for the chat experience when supported.
- Render transcript-only chat if sending is unavailable.
- Fall back to the terminal when structured events are unavailable.
- Never guess that an optional endpoint exists; honor `/v1/capabilities`.

## Architecture

```text
                         Latch control plane
                    +---------------------------+
                    | account/device directory  |
                    | presence + rendezvous     |
                    | encrypted relay fallback  |
                    +-------------+-------------+
                                  |
                    outbound only | outbound only
                                  |
+-----------------------+         |         +-----------------------+
| iOS app               |<========+========>| Latch Desktop / agent |
|                       | direct when able  |                       |
| Secure Enclave key    |                   | device allowlist      |
| chat + terminal UI    | E2E encrypted     | remote transport      |
+-----------------------+ application link  +-----------+-----------+
                                                      loopback
                                                         |
                                             +-----------v-----------+
                                             | latch serve /v1       |
                                             | HTTP + WebSocket      |
                                             +-----------+-----------+
                                                         |
                                             +-----------v-----------+
                                             | Latch CLI + private   |
                                             | tmux session kernel   |
                                             +-----------------------+
```

The remote transport is a byte-preserving authenticated tunnel for the
existing `/v1` API. The first version should avoid inventing a second session
or terminal protocol.

## Components

### 1. Desktop remote-access agent

Implement the host component as part of Latch Desktop or as a tightly scoped
signed helper managed by Latch Desktop. It is responsible for:

- Starting and supervising `latch serve` on a random loopback port.
- Holding the internal gateway token in owner-only storage.
- Maintaining the Mac's long-lived device identity in Keychain.
- Maintaining an outbound authenticated connection to the control plane.
- Advertising availability without uploading session names or content.
- Establishing direct or relayed encrypted connections to authorized phones.
- Authorizing requests before forwarding them to the loopback gateway.
- Recording local security and connection audit events.
- Preventing sleep while an explicitly configured long-running remote session
  requires availability, without silently changing the user's power policy.

The helper must run with the user's privileges. It must not require root and
must not grant access to arbitrary local TCP ports, files, or processes.

### 2. iOS application

The iOS app is responsible for:

- Creating a non-exportable signing/key-agreement key in Secure Enclave when
  available, with Keychain fallback only where necessary.
- Pairing with a Mac by scanning a short-lived QR code.
- Validating the Mac identity pinned during pairing.
- Establishing a direct connection or encrypted relay session.
- Using the Latch client contract for capabilities, sessions, events, send,
  prompt resolution, and terminal attachment.
- Keeping secrets out of logs, analytics, screenshots, and pasteboards.
- Applying normal iOS background limits honestly; remote connections may need
  to reconnect when the app returns to the foreground.

### 3. Control plane

The control plane stores the minimum data needed to locate and authenticate
paired devices:

- Account identifier.
- Opaque Mac and phone device identifiers.
- Public identity keys and signed device metadata.
- Pairing/revocation state.
- Short-lived presence and connection candidates.
- Push notification routing identifiers, if notifications are enabled.

It must not store Latch gateway tokens, terminal output, transcripts, working
directories, session names, commands, prompts, or decryption keys.

### 4. Relay

The relay accepts authenticated, rate-limited connections from paired devices
and forwards opaque framed bytes. Encryption is established end to end between
the phone and Mac before application data is sent through the relay.

The relay must not terminate the application encryption layer. TLS to the
relay is still required to protect metadata and provide defense in depth.

## Identity, pairing, and authorization

### Device identities

On first launch, each installation creates a long-lived asymmetric device key.
Private keys remain in Keychain or Secure Enclave. Public keys are registered
with the control plane and are never sufficient by themselves to authorize a
new pairing.

Use an audited protocol or library for authenticated key agreement. Do not
design a custom cryptographic construction. The concrete choice should support:

- Mutual authentication.
- Forward secrecy for connection traffic.
- Transcript binding to the intended Mac and phone identities.
- Key rotation without requiring every device to be paired again.
- Independent revocation of a single phone.

### Pairing flow

1. The user opens **Remote Access** in Latch Desktop and explicitly enables it.
2. The Mac creates a single-use pairing record with a five-minute expiration.
3. Latch Desktop displays a QR code containing the control-plane address,
   opaque pairing identifier, Mac public-key fingerprint, and a high-entropy
   one-time secret.
4. The phone scans the code, authenticates to the control plane, and proves
   possession of its device private key and the one-time secret.
5. The Mac displays the phone name and a short authentication phrase derived
   from the pairing transcript.
6. The user confirms the phone on the Mac.
7. Both devices persist the other's identity and the Mac persists the granted
   permission set.
8. The control plane consumes the pairing record so it cannot be replayed.

Do not support remote-only pairing in the first release. Requiring access to
the unlocked Mac sharply limits account-takeover impact.

### Authorization model

Store authorization on the Mac, keyed by phone identity:

- `observe`: list sessions and read events or terminal output.
- `interact`: send structured messages and answer prompts.
- `control`: send arbitrary terminal bytes and resize the terminal.
- `manage`: optional future capability for creating or ending sessions.

Default new phones to `observe` plus `interact`. Require a separate explicit
grant for arbitrary terminal control. Do not include `manage` in the first
mobile release.

Authorization is checked at connection establishment and again for each
privileged operation. Revocation closes active connections immediately and
blocks future handshakes.

## Transport and connectivity

### Transport abstraction

Create a transport interface shared by the desktop and mobile implementations
with these connection modes:

- `local`: Bonjour-discovered direct connection on the same LAN.
- `direct`: Internet peer-to-peer connection established through rendezvous
  and NAT traversal.
- `relay`: end-to-end encrypted connection carried by the Latch relay.

All modes expose the same authenticated byte-stream semantics to the `/v1`
client. The UI may show the active mode for diagnostics, but application
behavior must not depend on it.

### Protocol selection

Start with a mature, audited connectivity layer rather than implementing NAT
traversal from first principles. Evaluate candidates against:

- Native macOS and iOS support.
- Direct peer-to-peer establishment on common home and mobile networks.
- Relay fallback.
- Stable reconnection after network changes.
- End-to-end identity binding and certificate/key pinning.
- License and operational cost.
- The ability to keep the Latch application protocol independent of the
  chosen transport.

If the selected layer does not provide application-level end-to-end encryption
across its relay, add it before carrying Latch traffic.

### Local-network fast path

When both devices are on the same network, discover the Mac via Bonjour and
connect directly after verifying its pinned device identity. Local discovery
must not bypass pairing or authorization. Failure of Bonjour must fall through
to rendezvous rather than appearing as an offline Mac.

### Reconnection

Connections must tolerate transitions between Wi-Fi and cellular networks.
After interruption:

1. Reauthenticate the device.
2. Re-run capability discovery.
3. Resume event consumption from the last acknowledged event cursor.
4. Reattach terminal streams from the current visible screen.
5. Never replay terminal input or interaction submissions automatically unless
   the operation has a protocol-level idempotency key.

## Gateway integration

The desktop agent should start the gateway with an ephemeral loopback address,
for example `127.0.0.1:0`, and read the selected address through a structured
startup channel. Avoid parsing human-oriented stderr in the production path.

Add an internal authentication mode suitable for the supervised helper:

- Prefer a per-launch, high-entropy credential passed through a protected file
  descriptor or owner-only file.
- Keep the current token mode for CLI and SDK development compatibility.
- Never send this internal token to the phone or control plane.

The transport proxy maps authenticated phone identity to allowed gateway
operations. It should reject unauthorized requests before they reach the
gateway and apply defense-in-depth limits inside the gateway where practical.

Required protocol additions:

- A stable device/session identifier in connection diagnostics without
  revealing personally identifying information.
- Idempotency keys for message send and prompt resolution.
- A read-only terminal mode that ignores client resize and input.
- Explicit connection and protocol error codes suitable for mobile recovery.
- A structured health/readiness signal for supervised gateway startup.

These additions must remain optional and additive within protocol version 1,
or trigger a new protocol major if they alter existing behavior.

## Security requirements

### Network exposure

- Preserve the default loopback-only bind for `latch serve`.
- Do not use `--allow-remote` in the product flow.
- Require encrypted and mutually authenticated remote connections.
- Reject unknown devices before forwarding any HTTP or WebSocket data.
- Do not allow the remote tunnel to choose an arbitrary destination.
- Apply per-device and per-account connection and request rate limits.

### Local security

- Store device keys and internal tokens with owner-only access.
- Keep `~/.latch`, the private tmux socket, and session directories private.
- Redact credentials, terminal bytes, messages, and working directories from
  logs by default.
- Use signed, notarized desktop and helper binaries with a defined update path.
- Treat a locked Mac according to an explicit setting. The safe default is to
  keep existing jobs running but reject new remote control connections while
  locked; users may opt into continued remote access.

### Mobile security

- Protect the device key with the strongest available Keychain access class.
- Require local device authentication before first connection after app launch
  and before high-risk operations.
- Disable sensitive notification previews by default.
- Avoid persistent transcript caching in the first release. If offline history
  is later added, encrypt it with a device-bound key and provide a clear erase
  control.

### Control-plane security

- Make all authorization decisions based on signed device identity, not a
  caller-supplied device ID.
- Use short-lived access tokens for control-plane connections.
- Make pairing records single-use and short-lived.
- Make revocation monotonic and immediately visible to connected agents.
- Separate control-plane, relay, and operational-administration credentials.
- Define retention limits for IP addresses, presence records, and audit data.

### Threats that must be covered in review

- Stolen or replayed QR pairing material.
- A malicious or compromised relay/control-plane service.
- A stolen, unlocked, or restored-from-backup phone.
- DNS, certificate, and local-network impersonation.
- Cross-site WebSocket or browser-origin attacks against the gateway.
- Brute force and connection-exhaustion attacks.
- Confused-deputy access to another local service.
- Duplicate message or prompt submission after reconnect.
- Terminal escape sequences and hostile terminal output displayed on mobile.
- Dependency or update-channel compromise.

## Delivery phases

### Phase 0: Contract stabilization and threat model

Deliverables:

- Document the trust boundaries, data classification, abuse cases, and
  recovery behavior.
- Freeze the minimum `/v1` client surface needed by mobile.
- Add protocol idempotency and read-only terminal designs.
- Select the transport/NAT traversal dependency through a written decision
  record and small macOS/iOS connectivity prototype.
- Define observable service-level targets for connection success, direct-path
  rate, relay latency, reconnect time, and crash-free sessions.

Exit criteria:

- Security review approves the pairing and transport design.
- A prototype establishes direct and relayed encrypted byte streams between an
  iPhone and Mac on different networks.
- No terminal or transcript data is visible to the relay in a packet capture or
  service logs.

### Phase 1: LAN-only paired prototype

Deliverables:

- Desktop Remote Access settings and enable/disable control.
- Device identities and QR pairing.
- Bonjour discovery and authenticated direct transport.
- Supervision of an ephemeral loopback gateway.
- iOS device list, session list, chat view, and terminal fallback.
- Per-device `observe`, `interact`, and `control` authorization.
- Device revocation and basic local audit log.

Exit criteria:

- A newly installed phone can pair and control a test session on the same LAN.
- An unpaired phone cannot enumerate sessions or reach the gateway.
- Revocation closes an active connection and prevents reconnection.
- The existing web Remote SDK and CLI behavior remain compatible.

### Phase 2: Remote direct connectivity

Deliverables:

- Minimal account and device directory.
- Outbound presence/rendezvous connections from Mac and phone.
- Peer-to-peer NAT traversal.
- Path migration and reconnect across Wi-Fi/cellular changes.
- Push notification that wakes the user to a pending Latch prompt without
  including prompt content.

Exit criteria:

- Remote connections work without router or SSH configuration across the
  supported network test matrix.
- Connection UI distinguishes offline, connecting, direct, and authorization
  failure states.
- Duplicate interactions are prevented during reconnect tests.

### Phase 3: Encrypted relay fallback

Deliverables:

- Region-aware opaque relay.
- End-to-end encryption independent of relay TLS.
- Relay authentication, quotas, abuse controls, and operational dashboards.
- Automatic direct-to-relay and relay-to-direct path changes where supported.

Exit criteria:

- Sessions connect through hard-NAT test environments where direct setup
  fails.
- Relay compromise tests cannot recover Latch content or impersonate either
  paired endpoint.
- Resource limits prevent one account or connection from exhausting the relay.

### Phase 4: Production hardening

Deliverables:

- External security review and remediation.
- Key rotation, device recovery, account recovery, and incident runbooks.
- Signed/notarized helper installation and safe automatic updates.
- Privacy controls, retention policy, diagnostics export, and support tooling.
- Load, soak, fuzz, and failure-injection testing.
- Gradual rollout flags and the ability to disable relay or remote access
  independently.

Exit criteria:

- Security review has no unresolved critical or high-severity findings.
- Availability and latency targets pass sustained preproduction testing.
- Recovery and revocation procedures have been exercised end to end.

## Test strategy

### Unit and contract tests

- Pairing expiration, replay rejection, confirmation, and cancellation.
- Device authorization and immediate revocation.
- Capability-driven chat/terminal behavior.
- Request signing, transcript binding, and key rotation.
- Idempotent interaction submission.
- Permission-denied behavior at both proxy and gateway layers.

### Integration tests

- Real `latch serve` behind the desktop transport proxy.
- HTTP endpoints and WebSocket terminal/events streams through all transport
  modes.
- Gateway crash/restart and stale internal-token rejection.
- Phone app suspension and foreground reconnection.
- Desktop sleep/wake, screen lock, logout, and network transitions.

### Network matrix

Test at minimum:

- Same LAN with and without Bonjour availability.
- Typical home IPv4 NAT.
- IPv6-capable networks.
- Phone on cellular and Mac on home broadband.
- Double NAT and carrier-grade NAT.
- Symmetric/hard NAT forcing relay fallback.
- Captive portals and networks blocking UDP.
- High latency, packet loss, reordering, and intermittent connectivity.

### Security testing

- Fuzz all externally reachable framing and gateway proxy parsing.
- Attempt credential replay, device-ID substitution, downgrade, and
  man-in-the-middle attacks.
- Verify unknown origins cannot use a browser to control the loopback gateway.
- Verify relays and control-plane operators cannot decrypt captured sessions.
- Verify logs and crash reports contain no transcript, token, or terminal data.
- Exercise rate limits and resource exhaustion defenses.

## Observability and privacy

Collect enough metadata to operate connectivity without collecting session
content. Useful metrics include:

- Connection attempt result and broad failure category.
- Time to direct connection or relay fallback.
- Selected transport type and coarse network class.
- Reconnect count and duration.
- Bytes transferred as aggregate counters.
- Gateway and app versions.

Never collect terminal payloads, messages, prompt answers, session names,
repository paths, commands, environment variables, or gateway tokens. Make
diagnostic upload explicit and inspectable by the user.

## Operational fallback and advanced mode

Retain SSH tunneling and user-managed private networking as documented advanced
options. They are valuable when the Latch control plane is unavailable or a
user does not want any vendor-operated coordination.

An optional direct-port mode may be considered later, but only with TLS,
device identity, revocation, firewall guidance, and prominent reachability
diagnostics. It must never expose the current plaintext bearer-token gateway
directly, and it cannot promise operation behind carrier-grade or hard NAT.

## Initial work breakdown

The first implementation cycle should produce these independently reviewable
changes:

1. Add a threat-model document for remote access.
2. Add protocol-level idempotency keys and a read-only terminal capability.
3. Add structured gateway readiness and ephemeral-port supervision.
4. Define a transport-neutral authenticated stream interface.
5. Prototype device keys and QR pairing between macOS and iOS.
6. Proxy `/v1` through that interface on a local network.
7. Build the minimal native session list and chat interaction flow.
8. Add terminal fallback and explicit terminal-control permission.
9. Add revocation and audit UI to Latch Desktop.
10. Evaluate direct traversal and relay options using the network test matrix.

The LAN-only paired prototype is the first meaningful product milestone. It
validates identity, authorization, gateway supervision, and the mobile UX
before introducing Internet reachability and relay operations.

## Related documents

- [Remote SDK](REMOTE_SDK.md)
- [Architecture rules](ARCHITECTURE_RULES.md)
- [SSH setup](SSH_SETUP.md)
- [xterm compatibility decision](DECISION_XTERM_COMPATIBILITY.md)

