# Field investigation: "ICE connectivity checks timed out" off the home network

Mission coo:940, 2026-09-06. The phone on LTE could not reach the Mac; every
attempt ended in `LatchTransportNative.TransportError.Failure(message:
"WebRTC transport failed: ICE connectivity checks timed out")`. This is the
record of what was found, in the order it was found, what changed, and what
remains open. Times are UTC. Addresses are deliberately left out.

## How to read an attempt

Three sources line up by timestamp:

- **Control-plane request log** (Railway, service `Latch`, `http` stream).
  A healthy attempt reads, from the phone: `GET /v1/presence/<mac>`,
  `GET /v1/ice-servers`, `POST /v1/turn-credentials` (201),
  `POST /v1/rendezvous` (200); then the Mac's long-polled
  `GET /v1/rendezvous` returns early with the offer.
- **Mac audit trail** (`latch remote-access audit`). `rendezvous_offer/ok`
  means the helper's drain handed the offer to an ICE agent;
  `ice_answer/<result>` is how that answer ended. Since this mission the
  result names the stage: `connected`, `timeout`, `ice`, `dtls`, `sctp`,
  `channel`, `candidate`. `rendezvous_offer/rejected` means the CLI's own
  bounds check refused the offer before the helper saw it.
- **Helper ICE trace**, opt-in: `touch ~/.latch/remote-access/ice-debug.log`,
  then toggle Remote Access so the helper relaunches. The ICE and TURN
  stacks' full trace is appended, plus content-free gather/answer
  summaries. It contains addresses and ports and is never uploaded;
  delete the file to stop.

## Findings, in order

### 1. Offers never reached the Mac's agent (fixed)

The desktop approved offers correctly (after an earlier fix that compares
the control-plane device id rather than the local Noise id), then piped them
to `latch remote-access offer`, whose `RemoteOffer::validate` required both
identifiers to be bare 32-hex strings. The control plane issues
`dev_<32 hex>` device ids and the phone mints `rdv-<UUID>` request ids, so
every real offer was refused as "invalid opaque identifier". The desktop only
kept the error as a transient string; the audit showed 40 LAN connections and
zero `ice_answer` events.

Fix: `crates/latch/src/cli/remote_access.rs` — the validator mirrors the
control plane's `REQUEST_ID` and `OPAQUE_ID` rules; a refused offer is
audited as `rendezvous_offer/rejected`; tests feed the exact desktop
document shape end to end.

### 2. Presence advertised a consumed agent's ports (fixed)

The phone opens a new Noise channel — and so a new ICE session — for every
loopback connection, so a screen that makes two requests posts two offers
seconds apart. The Mac answers each offer with its one pre-gathered agent and
gathers a replacement at once, but presence was republished only on the
desktop's 30-second cadence, so the second offer was checked against ports
that now belonged to the first, connected session.

Fixes: the desktop republishes presence as soon as the helper reports a new
agent (fast status poll after each hand-off, plus on any poll that shows a
change), and withdraws presence once per stop rather than once per 250 ms
readiness poll. The phone (`RendezvousSequencer` in LatchMobileKit, used by
`NativeRemoteChannelProvider`) posts offers one at a time, waits up to five
seconds for presence to describe a replacement agent before offering, and
retries a connectivity timeout once with a fresh agent.

### 3. The ICE library abandons a pair after seven checks (hardened)

webrtc-ice 0.17.2 gives each candidate pair seven checks 200 ms apart and
never pings it again; the controlling side (the phone) does not revive a pair
on an inbound check from the Mac. The Mac's NAT admits the phone's checks
only after the Mac's own first outgoing check, which comes 0.3–1.2 s after
hand-off. Simulated networks did not reproduce a deadlock (a peer-reflexive
pair from the Mac's other socket rescues it), so this is hardening rather
than a proven fix.

Fixes: `crates/latch-transport/src/rtc.rs` derives the check budget from
the 30 s responder timeout instead of using the library default; the helper
drains offers every 100 ms instead of 500 ms; the helper gained the opt-in
ICE trace above (`crates/latch-remote/src/diagnostics.rs`).

### 4. The phone's checks did not reach the Mac at all (root of the field failure)

The first trace (08:20) settled the direction question. In both attempts the
Mac's own checks reached the phone — directly and through the phone's relay —
and were answered within 70 ms. In the other direction exactly one of the
phone's checks reached the Mac in thirty seconds. ICE cannot complete when the
controlling side's checks never arrive, and no timing change alters a path
that is one-directional. The Mac's router drops phone-originated UDP to the
Mac's reflexive address; the mechanism was not identified from the Mac side.

Fix: a relay candidate on the Mac's side as well, so the phone's traffic
arrives on the Mac's own outbound TURN flow, which every NAT admits. The
desktop fetches TURN credentials (`POST /v1/turn-credentials`, named for a
paired phone; issuance stays the policy gate) while the relay switches allow
it, refreshes them at half their life, withdraws them when the relay is
turned off, and hands them to the helper through `latch remote-access
relay-servers`, which stores them 0600 under the runtime directory. The
helper reads that file at every gather. With this in place the phone
connected over the relay in two to three seconds on two separate attempts —
the first successes on this path.

### 5. Presence carried no relay candidate (fixed)

Presence has four slots for agent candidates after the listener's. The
helper gathers one reflexive candidate per server URL, so with STUN plus
three TURN URLs all four slots held the same public address on different
ports and the relay candidates were never published. Connections still
succeeded because the phone discovered the Mac's relay from the Mac's own
checks. Fix: `RemoteAccessController.presenceCandidates` keeps one
reflexive candidate per address family ahead of the relays.

### 6. A CreatePermission storm on the relay (fixed)

The second trace (10:16): the first channel connected over the relay in two
seconds. On the second channel the phone's checks did reach the Mac's relay
candidate (19 of them), the Mac answered and held over ninety valid pairs,
and yet no nomination ever arrived, so the Mac timed out. What the trace
showed as pathological was the Mac's TURN client being refused
CreatePermission by Cloudflare 568 times in that half minute: the client asks
for a permission before every send to a peer it has none for, is refused for
the phone's private and carrier-shared host addresses, forgets the refusal,
and asks again on the next check — five refused requests a second per
address — with refusals spilling intermittently onto the phone's public
addresses that the Mac's responses needed. The phone's client does the same
toward the Mac's private hosts.

Fix: in the shared transport, a peer's host candidate on an address that is
not routable from here (private ranges, 100.64/10, 192.0.0/24, link-local,
unique-local) is skipped unless it shares a network with one of this
endpoint's own hosts. Reflexive and relay candidates are never filtered; a
list that would be emptied is kept whole.

## Current state (uncommitted at the time of writing)

- Mac-side binaries with all of the above are installed in `~/.local/bin`.
- `apps/LatchMobile/Native/LatchTransportFFI.xcframework` is rebuilt with
  the check budget and the candidate filter; the iPhone app must be rebuilt
  against it.
- The desktop app must be rebuilt for the relay credential loop and the
  presence ordering.
- Test counts at the last run: latch 152, latch-remote 15 (+1 diagnostics),
  latch-transport 13 (five simulated NAT topologies), desktop 100, mobile
  kit 235.

## Open question

Why the phone does not nominate once its checks reach the Mac's relay. The
Mac side shows the phone's checks arriving, its responses mostly leaving, and
no USE-CANDIDATE coming back. The permission storm is the only observed
pathology; if the next trace shows checks arriving and still no nomination,
the phone's own view is needed.

## Suggested next steps

1. **Verify on LTE with the trace on**, after rebuilding both apps. Expect
   `rendezvous_offer/ok` then `ice_answer/connected` per channel, and
   `path_selected/relay`.
2. **Phone-side opt-in ICE trace.** Mirror the helper's: an FFI call that
   installs a file logger under the app container, exposed via a hidden
   setting, retrievable through the Files app. This is the missing half of
   every diagnosis so far.
3. **Phone candidate publication.** `TransportCandidate.preferredForPublication`
   keeps one reflexive candidate regardless of family (an IPv6-first carrier
   dropped the IPv4 one) and publishes private hosts that are useless
   off-network. Keep one reflexive per family; drop private hosts when the
   route has already fallen through Bonjour and direct TCP.
4. **One ICE session per Mac.** Multiplex Noise channels over SCTP streams
   of a single connection so each loopback connection stops costing an ICE
   session and a TURN allocation. This removes the second-channel race
   class entirely.
5. **Credential churn.** `CLOUDFLARE_TURN_TTL_SECONDS` defaults to 120; the
   desktop refreshes at half-life, so one issuance per Mac per minute.
   Raise it (max 3600) on the Railway service.
6. **Relay-only agents when a relay is held**, as a fallback policy to
   evaluate: with a Mac relay every pair can go through Cloudflare and NAT
   behaviour stops mattering, at the cost of relay bandwidth for all
   off-network sessions.
