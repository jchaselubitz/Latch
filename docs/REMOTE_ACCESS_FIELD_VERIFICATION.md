# Remote access field verification

Six things about the phone's remote terminal cannot be established by any test
in this repository, because what they test is a network rather than a protocol:
a carrier's CGNAT, a hotel's UDP filter, a Mac that actually sleeps, an iPhone
that actually gets suspended. This document is how those six are run and
recorded, so the corresponding rows in
[REMOTE_ACCESS_PHASE_4.md](REMOTE_ACCESS_PHASE_4.md) can be traced to a
measurement instead of to a recollection.

What is simulated is listed here too, at the bottom, so it is clear which rows
these runs are actually needed for.

## What makes the result a measurement

Both ends count paths now, and neither count contains content.

**On the Mac.** `latch remote-access diagnostics` gained a `pathSelection`
block. It is derived from the bounded audit trail and counts authorized
connections by route:

| Route | Meaning |
| --- | --- |
| `lan` | The authenticated TCP listener on this network accepted it. |
| `direct_host` | ICE nominated a host pair — same network, or a tunnel interface such as a tailnet. |
| `direct_reflexive` | ICE nominated a reflexive pair — a hole was punched through at least one NAT. |
| `relay` | ICE nominated a relayed pair — the bytes take the TURN detour. |
| `unknown` | A stream arrived without a route observation. Counted rather than dropped, so an instrumentation gap cannot quietly flatter the direct rate. |

Alongside them, `iceAnswers` and `iceAnswersConnected` give the connect rate a
denominator: an answer that never nominated a pair is a failure, and a run that
counts one relay after nine dead attempts must not read as "100% relayed, all
healthy".

Two properties of these counters matter when reading a run. They are recorded
**after** the Noise handshake and authorization, so a stream that never proved a
paired identity never moves them — anything that can open a socket must not be
able to move the rate. And they live in the audit trail, which is bounded to
1,024 events or 512 KiB, so a long-lived Mac ages the oldest rows out; the
counters describe the retained window, not all time. `field-run.sh` reports a
negative delta as negative rather than clamping it to zero, because "the
evidence rolled over" and "nothing happened" need different responses.

**On the phone.** Settings → Linked computer shows a **Paths so far** row —
`Local 4 · Direct 12 · Relay 3 · Failed 1` — with a **Reset path counters**
button. This counts every channel the phone opened, not every time the
indicator changed: a route that opens four channels over the relay relayed four
times, and deduplicating that would make a relay-only network read as a single
blip. Failures include a Mac that presence said was asleep, so a phone that
never reached its Mac at all cannot show a clean record.

The phone's counters never leave the phone; the Mac's never leave the Mac
unless someone exports the (content-free) diagnostics bundle themselves.

## Before the first run

A field run needs a build that has an ICE responder on both ends. Check the
Mac's helper first — a helper that predates this work has no `--ice-server`
flag and publishes no ICE credentials, and the phone will fall back to Bonjour
and then fail, which looks like a network result and is not one:

```
latch-remote --help | grep ice-server   # must print the flag
cat ~/.latch/remote-access/runtime/lan-ready.json  # must carry ufrag/candidates
```

If either is missing, build and install the current helper, then toggle Remote
Access off and on in the desktop app so it is relaunched. Relaunching is not
optional and cannot be scripted around: the helper reads the Mac identity from
the Keychain, which prompts, and the prompt needs a session with a window
server — launching it from a headless shell blocks forever.

Also confirm, on the Mac's Remote Access settings, that the phone being tested
has **Allow terminal** switched on. Without it the phone resolves no terminal
route at all, which is a permission result rather than a transport one.

## Running one scenario

```
scripts/field-run.sh scenarios              # the six, and what a pass looks like
scripts/field-run.sh start cellular-to-home-nat
# on the phone: Settings > Reset path counters, then run the scenario
scripts/field-run.sh finish cellular-to-home-nat --result pass \
    --phone "Direct 3 · Relay 0" \
    --note "LTE, home router in NAT mode, terminal responsive"
```

`start` snapshots the Mac's diagnostics; `finish` snapshots them again, writes
the delta to `docs/field-runs/<scenario>-<timestamp>.json`, and prints the
matrix row. `scripts/field-run.sh matrix` regenerates the whole table from the
recorded runs, keeping the most recent run per scenario — a scenario re-run
after a fix should not leave its failure standing beside the pass.

Describe networks in general terms in `--note`. The run files are committed;
"hotel Wi-Fi, UDP blocked outbound" is the useful part, and the venue is not.

## The six scenarios

**Cellular to home NAT.** Phone on cellular with Wi-Fi off, Mac on a home
router. The terminal should open and the Mac should count a
`direct_reflexive` connection. A `relay` here is not a failure of the run but
is the more expensive answer, and is worth noting alongside the carrier — some
carrier CGNATs are effectively symmetric.

**Symmetric NAT.** A network whose NAT allocates a new external port per
destination. The expected result is a `relay` connection, and *failing to
connect at all is a fail, not a relay* — the whole point of allowing relay
candidates from the first attempt is that this case still works. If the network
under test cannot be confirmed symmetric, say so in the note rather than
claiming the row.

**Hotel or corporate Wi-Fi with UDP blocked.** The expected result is a relay
connection over TURN on TCP/TLS 443. If nothing connects, capture whether the
network also intercepts TLS, which is a different problem from UDP filtering.

**Wi-Fi to cellular mid-terminal.** Open a terminal on Wi-Fi, then disable
Wi-Fi while watching it. The session should survive or reconnect without
re-pairing. A path migration is expected; the phone's counters will show a
second connection, and the Settings Path row should change to match.

**Mac sleep and wake.** With the terminal idle, let the Mac sleep. The phone
should say *"Your Mac is asleep or Latch is not running."* rather than showing a
transport error — that sentence is the whole point of the row. After wake, a
reconnect should succeed without re-pairing. (Holding a power assertion while a
phone is connected is objective coo:856.0q9v and is not part of this row.)

**Phone background and foreground.** Background the app for several minutes,
then return. It should reconnect without a stuck spinner and without
re-pairing. Note whether the terminal surface was still held on the Mac, since
iOS may have suspended the app without the Mac noticing.

## What is already measured, and where

These do not need a field run; they are named here so the field rows are not
asked to re-prove them.

- **A symmetric NAT forces the relay, and a cone NAT does not.**
  `crates/latch-transport/src/rtc/nat_tests.rs` builds a virtual WAN, two LANs
  behind configurable NATs, and a real in-process TURN server. With
  port-restricted cone NATs on both sides the nominated pair is reflexive; with
  symmetric NATs on both sides it is relayed, and records round-trip over it.
  This establishes that the transport picks the right path for a given NAT
  pair. It establishes nothing about what any particular carrier does.
- **The ICE path carries the same Noise session as the LAN path.**
  `crates/latch-remote/tests/ice_peer_stream.rs` drives an approved offer
  through a real agent to a `PeerStream`, and asserts the route reaches it.
- **A revoked device or a withdrawn terminal grant closes a live stream.**
  Rust proxy tests in `crates/latch/src/cli/remote_access.rs`.
- **Route order, LAN fall-through, and the sleeping-Mac sentence.**
  `apps/LatchMobile/Tests/LatchMobileKitTests/PairedRouteTests.swift`.
- **The counters themselves.** `RemotePathMetricsTests.swift` on the phone and
  `path_metrics_separate_direct_from_relay_and_stay_content_free` on the Mac.
