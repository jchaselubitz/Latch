# Paired LAN Remote Access Prototype

Phase 1 adds a headless, desktop-side LAN prototype. It intentionally does
not add the phone chat UI, Internet rendezvous, or relay fallback.

## Operator flow

```text
latch remote-access enable
latch remote-access pair create --json
# Encode that JSON in a QR code and scan it on the phone.
latch remote-access pair confirm --pairing-id … --secret … \
  --device-public-key … --name "Jake's phone"
latch remote-access lan-serve
```

New devices receive `interact`, which includes `observe` plus structured
message and prompt-resolution operations. `control` is a separate grant for
arbitrary terminal input and resize. `latch remote-access revoke <device-id>`
marks the identity revoked on disk; active proxy connections re-check that
record every 250 ms and close when it changes.

`lan-serve` advertises `_latch-remote._tcp.local.` using Bonjour. Discovery is
only a reachability hint: clients must pin the Mac static key in the pairing
record and complete the authenticated handshake before forwarding any bytes.

## Boundary and wire format

The LAN listener is an encrypted Noise
`XX_25519_ChaChaPoly_BLAKE2s` transport. Each handshake and encrypted record
is length-prefixed with an unsigned big-endian 16-bit length. The client uses
the Mac public key received in QR material to verify the responder static key;
the listener accepts a peer only if the revealed initiator static key is an
unrevoked record in the Mac's owner-only device allowlist.

The encrypted plaintext is a single existing `/v1` HTTP or WebSocket request.
The proxy permits only known Latch `/v1` operations and adds the per-launch
gateway bearer credential itself. Remote peers cannot present, read, or choose
that credential, and cannot select any destination other than the supervised
`127.0.0.1:0` gateway. After the HTTP/WebSocket upgrade, the same encrypted
stream carries the existing gateway bytes unchanged.

`observe` allows lists, inspection, capabilities, events, and only
`mode=read-only` terminals. `interact` also permits message and prompt
resolution submissions. `control` is required for terminal control and
`keys` submissions. The proxy examines the complete initial HTTP request
before opening its loopback connection and rejects an attempted gateway
`Authorization` header.

## Local state and audit data

All remote-access state is placed in `$LATCH_HOME/remote-access` with 0700
directories and 0600 files. The Mac Noise private key, gateway token, pairing
secrets, and device public keys are never emitted in the device list or audit
output. `latch remote-access audit --json` records only timestamp, opaque
device identifier, event category, and outcome. It excludes request bodies,
terminal bytes, session names, endpoints, and credentials.

The supervised `latch serve` process continues to bind only to an ephemeral
loopback address and publishes its selected address through the Phase 0
structured readiness file. If it exits, `lan-serve` starts a replacement and
uses the new readiness address. This does not expose or invoke
`latch serve --allow-remote`.

## Explicit Phase 1 limits

This is a LAN-only prototype. Device identity uses owner-only file storage,
not Keychain/Secure Enclave; platform secret storage and key rotation belong to
Phase 4. It has no account directory, outbound control channel, NAT traversal,
path migration, relay, or native phone UI. The headless CLI is the validation
surface for this phase.
