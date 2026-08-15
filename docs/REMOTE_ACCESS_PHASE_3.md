# Remote access Phase 3: opaque relay fallback

Phase 3 adds a transport-neutral relay protocol model to the existing paired
remote-access boundary. It is intentionally a headless foundation: it does
not add a phone UI or a public listener for `latch serve`.

## Trust boundary

The relay receives a short-lived admission ticket, opaque device identifiers,
and encrypted frames. It does not receive a Latch gateway token, session name,
terminal output, transcript data, or either endpoint's Noise static private
key. The existing gateway remains bound to loopback and is reached only after
the desktop endpoint has authenticated the paired device and authorized its
request.

Each relay ticket is issued only after the authenticated control plane has
checked that the two opaque device identities are paired. It expires after one
minute. The relay authenticates each endpoint against that ticket but does not
participate in the Noise XX application handshake. The handshake transcript is
bound to the relay identifier and both endpoints verify the peer static key
against the identity pinned during pairing.

## Path selection and recovery

Direct probing remains preferred. When it fails, callers record the direct
failure and enter the relay path. The connection cannot carry an application
request until gateway capability discovery is refreshed. The same refresh is
required after migration back to direct, preventing a stale stream from
replaying assumptions across paths.

Relay admission, quota, and availability failures are represented separately
from authorization failures. The diagnostic counters contain only connection
and byte/frame totals; no relay payload, endpoint address, or credential is
included.

## Resource controls

The reference relay expires tickets after one minute and enforces a bounded
number of active tickets, two connected endpoints per ticket, a maximum frame
size, bounded frame/byte totals in a one-minute window, and a bounded queued
byte total before it accepts an opaque frame. A hosted region-aware relay
should apply equivalent per-account and regional limits at its TLS ingress,
retain only operational aggregates, and never log frame contents.

The focused forced-relay test validates that an unpaired request is denied,
the relay observes ciphertext rather than the representative terminal/gateway
plaintext, the destination decrypts the original bytes, quotas reject excess
traffic, and application forwarding waits for the capability refresh.
