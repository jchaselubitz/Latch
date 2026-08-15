# Remote Access Phase 2: directory and direct-path rendezvous

Phase 2 adds the control-plane boundary that sits in front of the paired
transport. It remains deliberately unable to see Latch content, gateway bearer
credentials, session names, or terminal data.

## Control-plane contract

`DeviceDirectory` is the minimal, transport-neutral directory core. A hosted
control connection authenticates its caller before it invokes `publish` or
`rendezvous`; the core accepts that paired-device authorization as an explicit
input instead of attempting to turn the directory into Latch's source of
authorization truth. The Mac remains authoritative: its per-device allowlist
and authorization checks still occur when the authenticated application stream
is established.

Presence expires after 90 seconds. A presence record contains only an opaque
device ID, a public identity key, and at most 16 short-lived UDP candidates.
`RendezvousRequest` returns the target's unexpired candidates only when the
outer control channel has authenticated a paired requester. Its typed schema
has no fields in which to put a gateway token, endpoint private key, session
metadata, transcript, request body, or terminal bytes.

The production control service is expected to put this API behind mutually
authenticated, outbound-only TLS/WebSocket connections from both devices. The
Mac makes no inbound control-plane connection and does not accept a
caller-selected local destination.

## Direct-path establishment

`probe_direct_path` provides simultaneous UDP probing using a 32-byte,
single-use rendezvous identifier delivered over the authenticated control
channel. Each side binds its own UDP socket and sends first to every validated
candidate. A matching response proves a usable peer path and creates the NAT
mapping needed by an ICE-compatible direct transport. Loopback, multicast, and
unspecified candidates are rejected so a compromised control plane cannot use
the desktop as a local UDP probe.

The probe does not carry `/v1` data. It is intentionally a path-establishment
primitive below the existing authenticated stream boundary. The selected native
WebRTC/ICE data-channel adapter will use this control contract and preserve the
same Noise-authenticated, byte-stream gateway proxy used by LAN mode; Phase 3
adds the relay path. This separation means diagnostics and application behavior
do not depend on a particular connectivity library.

For a headless direct-path smoke check, run two paired test endpoints with the
same short-lived rendezvous ID and each endpoint's candidate:

```text
latch remote-access direct-probe --rendezvous-id <64-hex> \
  --candidate <peer-public-or-LAN-ip:udp-port>
```

The command intentionally reports only `{"state":"direct","peerReachable":true}`;
peer addresses are not retained in diagnostics or audit data.

## Reconnect, migration, and diagnostics

`DirectConnection` reports only `offline`, `connecting`, `direct`, and
`authorization_failure`, plus reconnect and migration counts and a coarse
failure code. After a path migration or interruption, it refuses application
forwarding until the client has repeated `/v1/capabilities` discovery. This
prevents a reconnect from assuming capabilities that changed while the phone
was offline and retains the Phase 0 idempotency rules for submissions.

The control-plane implementation must delete expired presence records and
should never log candidates alongside user or session identifiers. Broader NAT
matrix coverage, a production ICE adapter, and forced relay behavior remain
separately testable next steps; Phase 3 owns the relay fallback.
