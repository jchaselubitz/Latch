# Remote access in Latch Desktop

Latch Desktop is the owner-facing supervisor for Remote Link. It does not make
the local gateway public and it never handles application plaintext in a cloud
service.

## Setup

1. Install a coordinated payload containing matching `latch`, `latchd`, and
   `latch-remote` binaries.
2. In **Settings → Remote Access**, enter the HTTPS control-plane address.
3. Paste an operator-minted one-use owner invitation. Desktop exchanges it
   once to create the owner account and host identity; anonymous bootstrap is
   not supported.
4. Turn Remote Access on.
5. Select **Pair a Device**, scan the five-minute code with the phone, compare
   the transcript-derived words, verify the displayed phone name, and approve
   the proposed key on the Mac.

Old pairing records are deliberately incompatible. Upgrade all components and
pair again; do not copy old remote credentials into the new installation.

## Process boundary

`LatchDesktop` asks the control plane for current link metadata and fresh host
admissions. For every active paired phone it supervises `latch-remote`, which
keeps an outbound WSS socket to the opaque relay and races it with the
authenticated LAN carrier. The first authenticated carrier owns one
Noise/Yamux link; losing it terminates its streams and lets supervision create
a fresh link.

`latch-remote` starts one short-lived `latch serve` process on an ephemeral
loopback port. Its bearer token stays in owner-only runtime state. Remote
requests can reach that gateway only through the ordinary `latch` authority,
which:

- pins the authenticated phone key to an active local device record;
- checks the current grant revision and route requirement;
- refuses caller-chosen destinations and unregistered routes;
- rejects pipelining, oversized headers, forged authority headers, and
  non-loopback adapter requests; and
- completes final response delivery while bounding idle and blocked writers.

No Desktop preference can publish this loopback listener.

## Pairing

The QR payload contains the HTTPS control-plane address, enrollment ID, Mac
public key, short-lived admission code, expiry, Mac name, and an independent
256-bit enrollment secret. The control plane sees the admission code but never
the enrollment secret. Both endpoints mix that secret into the enrollment
Noise prologue, so a malicious service with the admission alone cannot
substitute a controller.

The phone proposes its exact static key inside the encrypted stream. Desktop
shows transcript-derived comparison words and waits for an explicit owner
decision. Approval writes the exact proposed key and initial grant to the Mac
first, then completes the matching control-plane enrollment. Replays are
idempotent only when all security-relevant fields are identical.

Cancelling or allowing the enrollment to expire closes the helper and queues
room invalidation. Pairing material and private keys are not written to logs.

## Grants and revocation

Each device has a base Observe/Interact choice and a separate terminal switch,
represented by the existing `observe`, `interact`, or `control` grant.
Changes apply to new and active streams. The Mac's device store is the
authority; the control-plane mirror lets the phone present current UI and
prevents stale relay admission.

Revocation:

1. marks the local controller record revoked;
2. closes streams when the 250 ms current-grant check observes the change;
3. revokes the control-plane pairing; and
4. durably queues invalidation of the opaque relay room.

A mirror failure is shown to the owner and retried; it never restores local
authority.

## Status and failure behavior

Desktop reports Off, Starting, Waiting for a phone, the number of phones whose
link is authenticated, or a concrete failure. Each helper reports its own link
status (`lan_ready`, `connecting`, `waiting_for_peer`, `authenticating`,
`ready`, `link_closed`, `offline`); only `ready` counts as connected. A
waiting relay socket is not a connection.

The helper lives as long as remote access is on. It keeps its gateway child,
LAN listener, and Bonjour record across link loss; a lost or replaced link
closes that link's streams only, and the helper asks Desktop for a fresh
single-use relay admission (`admission_needed` / `admission` over the same
stdin IPC). Desktop backs off while the control plane is unreachable and never
restarts the process for a normal link end. Both the helper and the phone wait
for the relay to report the other side present before the 10-second
handshake deadline starts, so an idle Mac holds one waiting socket indefinitely
(its lease is the bound) instead of churning.

Keep-awake is opt-in (**Settings → Remote Access → Sleep**) and prevents
idle sleep only while the Mac is on external power and at least one phone is
authenticated. Lid close, manual sleep, and low battery still sleep the Mac;
the phone shows the Mac as offline until it wakes and the helper re-admits.

Attention notifications: the gateway keeps observing sessions a phone has
opened and spools one content-free event per finished turn or pending
request; Desktop forwards the opaque event id to the control plane, which
pushes a fixed generic sentence. Nothing about the session, prompt, or output
leaves the Mac.

TLS or peer-pin failures are terminal for that attempt. Transient WSS, lease,
or helper failures trigger bounded supervision backoff. Application input is
never automatically replayed merely because a carrier reconnects.

## Development verification

From the repository root:

```sh
swift test --package-path apps/LatchDesktop
cargo build -p latchd --offline
cargo test -p latch-transport -p latch-transport-ffi -p latch-remote -p latch --offline
```

The composed Rust test uses a real local TLS/WSS relay, authenticates both
endpoints, opens a Yamux stream, and reaches the authorized Mac gateway.
Control-plane behavior must also pass against disposable PostgreSQL, not only
the in-memory test store.
