# Cellular “Mac offline” investigation — 2026-09-09

Mission: coo:994; objective: coo:994.49ba.

## Finding

The strongest explanation is a stale, idle Mac-to-relay connection that the
helper does not detect. This is a pre-authentication recovery gap, not evidence
that the computer lacks internet connectivity. The code gap is confirmed;
attribution of the reported phone attempt is strongly supported but not fully
reproduced because the available phone export contains no September 9 attempt.

## Evidence

- The supplied 11:28 screenshot shows 5G and a pairing created September 8 at
  11:07. Its Mac identity abbreviation, `5995…a45a`, matches the local Mac.
  The screenshot shows saved pairing information, not live link health.
- Local CLI version: `0.2609081113.0`. Remote access is enabled. Of twelve
  recorded devices, eleven are revoked, leaving one non-revoked record.
- Latch Desktop and `latch-remote` were running. The helper (PID 3784) retained
  its LAN listener and an OS-reported ESTABLISHED outbound TLS socket.
- The audit records successful relay links on September 8, last at 13:50:22
  Berlin time. The final recorded events are September 9, 01:15:57
  `link_offline/socket_closed`, then 01:15:58 `link_connecting` and
  `link_waiting_for_peer`. No subsequent authentication or recovery appears.
- Around 13:56–13:57 Berlin time, two TCP counter readings more than 30 seconds
  apart were identical: 26,290 received bytes and 12,472 sent bytes. Thus the
  retained socket was not receiving the expected 15-second relay heartbeat
  traffic during observation. ESTABLISHED alone does not establish liveness.
- Both public production readiness endpoints returned healthy responses.
  Railway reported the relay and control plane running on successful
  deployments of `ebd4c8f9c79507466a89acb649d0d310f12bcddc`, with one relay
  replica. Bounded runtime logs contained September 8 restart-test activity,
  not a trace of the reported September 9 attempt.
- Retrieved the paired iPhone's content-free diagnostics. All available files
  concern September 8; the latest cold-open record succeeded over relay at
  11:50:18 UTC. They cannot establish today's phone-side failure sequence.

## Code path

1. `crates/latch-remote/src/link.rs`, `run_wss_acceptor`, calls
   `records.wait_for_peer(None)`. It requests a fresh admission only after
   that wait returns an error or the later authenticated link ends.
2. `crates/latch-transport/src/link.rs`, `WssRecordIo::wait_for_peer`, has no
   local deadline when passed `None`. `next_frame` has no inactivity timer.
   It can wait indefinitely when a previously working network path silently
   stops delivering data or close frames.
3. `services/relay/src/server.ts` sends WebSocket pings every 15 seconds and
   closes rooms for missed pongs. That protects server state, but a close
   cannot wake a client whose path silently discards traffic.
4. The encrypted 45-second dead-peer timeout in `run_transport` starts only
   after `SecureLink::establish`; it does not protect idle peer waiting.
5. Desktop lease renewal stops if renewal fails and relies on the relay to
   close the socket (`RemoteAccessSupervisor.renew`). There is no local lease
   expiry enforcement in the idle wait to cover an undeliverable close.
6. The phone bounds peer waiting to 12 seconds and maps `PeerUnavailable` to
   `macOffline`. “Your Mac is offline” therefore overstates the observation:
   the phone did not find its Mac on the relay within the deadline.

## Recovery and durable correction

Restart Latch Desktop to replace the stuck helper and request a fresh relay
admission, then retry on cellular. This is a proposed recovery, not a verified
repair; this investigation did not restart applications or change deployment
state, pairing, or permissions.

The durable correction should bound **relay silence**, while allowing an
indefinite wait for an absent phone as long as relay heartbeats continue.
Apply an inactivity deadline during pre-authentication reads and bounded
connect/write operations, then request a fresh admission on expiry. Enforce
local lease expiry and consider wake/network-change reconnects as additional
recovery paths. Preserve the authenticated Noise identity checks.

Regression coverage should establish an idle relay connection, silently stop
all inbound traffic without a FIN/RST, verify bounded reconnect/re-admission,
and then connect the phone. Also verify that an idle host receiving relay
pings remains available without needless reconnection. Existing encrypted
link keepalive tests exercise the post-authentication case only.

Use “Mac unavailable through the relay” for this failure, with the existing
retry behavior, rather than asserting that the computer itself is offline.

## Limits

No transport changes or tests were run as part of this diagnostic objective.
The original network event that stranded the socket is unknown. Current
service health does not prove service health at 11:28. A fresh cellular
attempt and helper restart comparison remain necessary to verify recovery.
