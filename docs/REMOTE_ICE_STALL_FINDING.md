# Phone ICE trace: stalled TURN writes block nomination

7 September 2026. Base source `1d73ecc`. This continues the 6 September fix
notes using the newly supplied phone trace and matching local Mac trace.

## Field evidence

The phone trace identifies `lifecycle-v2`, confirming the previous native
lifecycle changes are present. It contains successful LAN and remote
transports as well as failures; a blanket cellular UDP blockage is not the
explanation.

For attempt `22e01f4111dc3054` at **09:25:18 CEST**:

- Phone line 1333 starts ICE; the Mac receives that same attempt at line
  33565 and starts its responder at line 33566.
- Phone lines 1344–1347 show a TURN permission transaction to UDP/3478
  receiving a response. Lines 1348–1355 begin a permission transaction on
  the other allocation, UDP/53, and start retransmitting it.
- The phone remains able to receive and answer incoming Mac checks on other
  sockets (lines 1356–1415), but its own check pass stops advancing at the
  blocked TURN send. Later retransmissions appear at lines 1433, 1434, 1442,
  1444 and 1886. There is no nomination for this attempt.
- The phone times out at line 2593, **09:25:33.442**. The Mac's longer timeout
  follows at lines 41785–41786.

Attempt `eabfbdc5ba4b5fba` exhibits the same UDP/53 permission stall starting
at phone line 2493. The final failure also has valid relay pairs while the
check loop is stalled, so an incoming successful check alone does not ensure
that nomination will run.

Separately, attempt `16689d4de946eb74` reaches the Mac at 09:25:38.912 before
replacement gathering finishes at 09:25:39.060. The audit records
`rendezvous_offer/rejected` at timestamp 1788765938. Multiple phone provider
instances were using separate sequencers for the same single-agent Mac.

## Reproduction and fix

The pinned webrtc-ice 0.17.2 check pass calls `ping_candidate` sequentially,
which awaits `send_stun`, which awaits `Candidate::write_to`. A TURN candidate
can wait inside CreatePermission while holding its allocation mutex. That
wait blocks the common check/nomination loop, even when other sockets work.

A new virtual-network regression permits TURN gathering, drops the phone's
CreatePermission requests, and delays the Mac's initial checks by 600 ms.
Against the unmodified dependency it fails the six-second connection bound.
Against the fix it completes in about one second, exchanges records both
ways, and closes promptly. A second regression verifies the same behavior
through the Mac's healthy relay with symmetric NATs on both sides.

The dependency is vendored at `vendor/webrtc-ice`, with upstream licenses,
archive checksum, and patch notes retained. Only its STUN dispatch source
changes: bounded per-local-candidate queues keep stalled allocations out of
other sockets' check and response paths. Each queue has at most 32 packets;
entries older than one second are skipped. Workers stop on candidate close
and are aborted before candidate cleanup. ICE priorities, nomination rules,
authentication/integrity checks and application-data transport are unchanged.

Mobile providers now share one sequencer per pinned Mac public key, including
its last-used-agent state across route rebuilds. This addresses the competing
providers seen in the trace. It does not turn the control plane's presence
snapshot into a transactional agent reservation; multi-phone races remain a
separate signaling limitation.

The diagnostic filter also suppresses TURN's `try_send data = ...` records.
Those upstream records dump packet bytes, including encrypted transport
payloads. Earlier claims that the logger excluded all packet payloads were
too strong. The supplied trace should remain local; the new writer retains
connection/transaction events without those byte dumps.

## Validation and deployed artifacts

- 27 Rust tests passed: 21 transport, 4 FFI, 2 helper integration.
- 246 Swift tests passed: 240 MobileKit, 5 emulator, 1 native boundary.
- After rebuilding the XCFramework, reran the native boundary test and
  verified the new `stun-queues-v1` diagnostic marker.
- All five XCFramework target architectures rebuilt; the actual iPhone app
  scheme built successfully (compile verification, signing disabled).
- Architecture boundary check and `git diff --check` passed.

Installed the Developer ID signed helper and restarted it under the existing
user authorization. There were zero active connections before restart.

Installed helper SHA-256:
`677e11c85b45f721cc61afda0567639d25937d9a72aeaca674bfd4d48c11989f`

Rebuilt iOS native archive SHA-256:
`bbcb18d01ad344d85fb64d18c882f3aadd97a53e3d9cc0d183830bc9a9a1fd20`

Rollback binary: `/private/tmp/latch-remote-before-stun-queues-20260907`.

## Physical phone follow-up

The next supplied phone log (`E559AB7C-0C4B-474A-B4BB-F4392F585734/1-ice-debug.log`)
includes a new run starting at Unix timestamp 1788767149.296, with build
`0.2609061156.0 lifecycle-v2 stun-queues-v1`. Earlier failures remain in the
same file; these results cover only the new run.

All four new transports connected, in approximately 1.3–2.4 seconds:

| Attempt | Mac-selected path | Phone connect duration |
| --- | --- | --- |
| `77c9f0ee8d28ad23` | Direct reflexive | 1.381 s |
| `87bb0e8c9b52c3ff` | Relay | 2.363 s |
| `3a15025485931a87` | Direct reflexive | 1.306 s |
| `e11f912a9c0abd49` | Direct reflexive | 1.488 s |

The Mac audit independently records successful rendezvous offers, connected
ICE answers, and `connection_opened/ok` for all four. That last event occurs
after peer authentication, request authorization, and forwarding the initial
request to the loopback gateway. No ICE timeout appears in this new run.

The relay connection also records `connection_rejected/rejected` after its
successful opening. This is a generic proxy-error label and does not establish
an authentication rejection; the audit does not retain the underlying error.
The user subsequently confirmed that the phone still showed a request timeout;
transport establishment alone did not verify the application flow.
These results verify repeated transport establishment on the physical phone,
but do not establish sustained terminal operation. TURN TCP/TLS support is
still absent from the pinned transport.


## Follow-up: the final HTTP response was discarded on close

The fresh log `9CE594C8-57AE-4ACB-9869-B4F2BAB9C593/1-ice-debug.log`
adds three attempts at Unix timestamps 1788767879.951–1788767895.087.
All three complete ICE and authenticate on the Mac, with `connection_opened/ok`
and direct-reflexive paths. The phone nevertheless displays “Cannot reach that
computer. The request timed out.”

The proxy requests `Connection: close` from the loopback HTTP gateway. On
response EOF it previously dropped both peer halves immediately. SCTP's
`write` only queues the encrypted response; dropping the halves invokes
`RtcConnection::close`, which can destroy the association before that queued
response is delivered. A regression using the real `IceResponder` reproduced
this: after the final write and immediate proxy teardown, the phone read an
empty record instead of the response (`/tmp/latch-response-before.log`).

Successful response EOF now calls `PeerWriter::finish`. The ICE implementation
waits for SCTP's queued-byte count to reach zero, which the pinned stack updates
on peer acknowledgment, before releasing the connection. This wait is bounded
to five seconds. Immediate error, cancellation, and permission-revocation
cleanup still closes without draining. TCP transports retain their socket
write behavior. No mobile protocol or pairing change is required.

The real ICE regression passes with draining (`/tmp/latch-response-after.log`).
A separate Noise/HTTP proxy regression uses a queued response writer and an
HTTP server that immediately closes after its response, verifying that the
proxy invokes completion and delivers the entire response. All 39 remote-access
tests pass, including terminal revocation and permission downgrade checks.

The helper's diagnostic header includes `response-drain-v1`, and successful
response delivery records `response drain started` / `response drain completed`
without recording request or response contents. Physical app success after
this fix still needs confirmation; the prior logs establish the symptom and
transport success, while the regression establishes the response-loss bug.

All 28 transport/FFI/helper tests also pass (67 targeted Rust tests total).
The signed helper was installed and restarted with zero active connections;
Remote Access reports enabled with listener and ICE readiness restored.
The installed helper SHA-256 is
`b2c5d0273942937c08668a3ead19aeee559a4435cbce80651e94cb1835c2ab3c`.
Rollback binary: `/private/tmp/latch-remote-before-response-drain-20260907`.
The existing phone build can test this Mac-only change without reinstalling.


## Physical follow-up after response draining

The user reports that folder browsing works and sessions eventually appear,
but requests are slow and starting a session can still show an ICE connectivity
check timeout. This is partial application success, not a complete fix.

In the Mac log between Unix timestamps 1788768558 and 1788768715, six offers
include a phone relay candidate and all six establish ICE. Three offers omit
phone relay candidates (`9b6b6c958b971f16`, `1b59e0affeb2f2ea`,
`22aa1bca42eeba1b`) and all three time out. This is a correlation in this run;
it does not establish why phone relay gathering failed or rule out signaling
races. The current phone log is needed to inspect allocation and credential
errors for those exact attempts. The previous uploaded log ends before them.

Several successfully authenticated relay requests begin response draining and
then end through the generic proxy-error path before a drain-completed event.
Client closure after receiving a complete response can also trigger that path,
so the generic rejection audit is insufficient to label these as lost responses.
No further speculative transport changes were made from this evidence alone.


## Phone log resolves the relay-gathering gap

`/Users/jake/Downloads/ice-debug.log` contains the failed terminal attempts.
For example, gathering for `b4e226973a601abc` starts around Unix timestamp
1788768787.617. IPv4 TURN Allocate requests to ports 3478 and 53 repeatedly
retransmit without a response; both fail around 1788768795.459. IPv4 STUN also
times out while IPv6 STUN candidates are successfully gathered. The candidate
set has IPv6 reflexive addresses and no relay, and connectivity subsequently
times out. This establishes missing IPv4 UDP responses in this run; it does
not distinguish carrier filtering from routing failure or another network cause.

The vendored ICE library hardcoded the TURN client socket to `0.0.0.0`, so
it never attempted the still-available IPv6 family. Cloudflare explicitly
supports IPv6 client-to-TURN connections with IPv4 relay allocations:
https://developers.cloudflare.com/realtime/turn/faq/

Relay gathering now attempts configured IPv4 and IPv6 UDP transports. The
candidate's address family comes from the allocated relay address, independently
of the socket used to contact the server. A local IPv6-only TURN listener
regression fails before this change and passes afterward; it also forwards
real data from the IPv6 TURN client to an IPv4 UDP peer through the relay.

A separate delay came from sending plaintext UDP STUN probes to TCP/TLS URLs.
Those ports do not answer such probes, charging the five-second STUN timeout
to gathering. Gathering now excludes those URLs from UDP STUN probes. A local
silent-port regression fails its two-second bound before the change and passes
afterward. This does not implement TURN over TCP/TLS and does not eliminate
waits for real UDP endpoints that are unreachable.

The diagnostic marker for the new native build is `turn-ipv6-v1`. The physical
phone must be rebuilt/reinstalled to exercise this change; changing only the
Mac helper cannot enable the phone's IPv6 TURN socket. Physical cellular
terminal success remains unverified until that build is tested.

Validation: all 30 transport/FFI/helper tests pass; the rebuilt native Swift
boundary test passes and asserts `turn-ipv6-v1`; all five XCFramework
architectures rebuilt; the iPhone app build succeeds (signing disabled for
compile verification). Boundary checks and `git diff --check` pass.

The signed helper was installed with zero active connections and Remote Access
readiness restored. Installed helper SHA-256:
`569ab837d641fae2a7f885915bf1e7d244a4325bccf76bd4120f5086fe113929`.
Rebuilt iOS native archive SHA-256:
`9e6ce2494233da38e8b30bac855e2a1fc84ab338a656701462657e9d37743d17`.
Rollback helper: `/private/tmp/latch-remote-before-turn-ipv6-20260907`.
The physical phone installation and cellular terminal test remain outstanding.


## Phone confirmation and the remaining handover delay

The user reports the terminal now works, with some remaining startup delay.
`/Users/jake/Downloads/ice-debug 2.log` confirms `turn-ipv6-v1` and seven new
attempts. Six establish transport in approximately 1.3–1.5 seconds; one times
out and its retry succeeds. Relay candidates are now present in every attempt.
This run's successful paths do not by themselves prove IPv6 relay was selected;
the local regression separately establishes that capability.

The exceptional attempt, `6bee45b3dcb2990c`, reaches the helper at Unix timestamp
1788769696.169. The helper has no idle agent and rejects the offer; its prior
replacement gather completes only at 1788769700.613, after a stalled TURN
allocation exhausts its retries. The phone spends its 15-second ICE deadline
checking an offer the Mac never answered, then gathers again and retries.
The same phone log also contains one roughly 5.5-second wait between gathering
and offering, consistent with the bounded presence-replacement wait.

The helper now retains an offer while waiting up to eight seconds for the
idle agent. Waiting and connecting run outside its accept loop. A semaphore
bounds pending/connecting offers to four, and a notification wakes waiters
when gathering completes. The eight-second bound leaves time inside the
phone's 15-second check deadline for nomination. Failed replacement waits
are recorded explicitly rather than misreported as ICE checks.

A regression takes away the published idle agent, submits an offer during
the gap, verifies that admission returns promptly, then supplies a replacement.
The phone starts from the old published addresses and connects via the new
agent's checks, exchanging data both ways. It fails with the prior immediate
rejection and passes after the change. This fixes the observed rejection gap;
it does not make directory presence a transactional agent reservation.

The helper marks this change with `responder lifecycle=offer-handover-v1`.
No further mobile rebuild is required for this helper-only improvement.

All four helper tests pass (the new handover regression and three integration
tests); boundary and diff checks pass. The signed helper was installed with
zero active connections, and Remote Access listener/ICE readiness recovered.
Installed helper SHA-256:
`7bfc23be1c0cae159b38e9109c782d640a563f06f7dc1d39eb4e6497e42923eb`.
Rollback helper: `/private/tmp/latch-remote-before-handover-20260907`.
The specific handover improvement has regression coverage; its effect on the
next physical phone run was not yet measured at the time of that installation.

## Final physical baseline after helper handover

Before starting the WSS replacement on 7 September 2026, the owner repeated
the complete physical flow four times on an iPhone 16 with Wi-Fi disabled and
cellular data active. Every pass loaded the session list, created a shell,
attached its terminal, accepted `pwd`, and rendered
`/Users/jake/Development`. Each individual step took approximately one to
three seconds. The phone's retained path counter read `Direct 193` after the
run.

The corresponding Mac-side field record is
[`field-runs/cellular-to-home-nat-20260907T095841Z.json`](field-runs/cellular-to-home-nat-20260907T095841Z.json).
During its measurement window the Mac recorded 26 authenticated connections:
22 `direct_reflexive` and four `lan`, with 22 of 22 ICE answers connected.
The four LAN entries fall outside the cellular-only phone actions and are kept
in the raw delta rather than silently removed. No failure was reported in the
four owner-observed cellular flows.

The running helper for this final confirmation was
`/Users/jake/.local/bin/latch-remote` version `0.2609070836.0`, signed by team
`X84RPB4674`, SHA-256
`27a1fa65486dbef7e82a0c6d157b8aa7de05e0fcf8db0054b64dfff299b981cb`.
It carries `offer-handover-v1`, `response-drain-v1`, and `turn-ipv6-v1`.
The archived source revision is
`a2ab11dd44c8f68a887f6d276daa3af8b0ca7e97`, tagged
`remote-ice-baseline-2026-09`. This closes the replacement start gate; it is
historical ICE baseline evidence, not WSS release evidence or a measured p95.
