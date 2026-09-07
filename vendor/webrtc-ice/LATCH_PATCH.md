# Latch patch to webrtc-ice 0.17.2

Original registry archive SHA-256:
`5b7fd30f52e6fda8664779b84b7904b2553b76fee24d9ca665e774ae32b13f53`.
Both upstream licenses are retained. Cargo's normalized standalone manifest is
used unchanged. Source patches are in `src/agent/agent_internal.rs` and
`src/agent/agent_gather.rs`, with the relay-gather test call updated for its
new network-family argument.

A local TURN candidate's `write_to` may wait for a permission transaction for
longer than the whole ICE connection deadline. Upstream awaits that write on
its sequential check loop, preventing checks/nomination on other candidates.

`send_stun` now enqueues into a bounded 32-packet queue per local candidate.
Each worker writes independently; a full queue drops the new STUN packet for
ICE's normal retransmission to replace. Entries older than one second are
skipped. Workers stop on candidate closure and are aborted before candidate
cleanup takes the TURN allocation mutex. This changes only STUN dispatch;
application data, ICE integrity checks and nomination policy are unchanged.

Regression coverage lives in `crates/latch-transport/src/rtc/nat_tests.rs`:
selectively drop the phone's TURN CreatePermission requests after successful
gathering, delay the Mac's start, then require nomination, two-way data and
prompt shutdown. Cover both a direct route and a healthy remote relay. The
direct regression fails against the unpatched registry crate and passes here.


Relay gathering now attempts each enabled UDP address family. The TURN
client-to-server connection may use IPv6 while the server allocates an IPv4
relay; candidate network type follows the allocated address, not the client
socket. Literal IPv6 TURN URLs retain bracketed host:port formatting. The
local IPv6 TURN regression allocates an IPv4 relay and forwards real UDP data
to an IPv4 peer through it. It fails before this patch and passes after it.

Server-reflexive gathering only sends UDP binding probes to UDP STUN/TURN
URLs, avoiding plaintext probes and five-second waits on TCP/TLS ports. A
silent local UDP socket regression demonstrates the old delay and verifies
that those URLs no longer trigger UDP probing. TURN over TCP/TLS itself is
still unsupported.
