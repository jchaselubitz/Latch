# Remote Link field verification

This is the physical-device evidence record for Remote Link v1, the only
supported remote transport. It defines how the plan's physical matrix
([PLAN_REMOTE_RELAY_REPLACEMENT.md](PLAN_REMOTE_RELAY_REPLACEMENT.md)
section 9) is executed as measured attempts, what the gates are, which build
identities the numbers belong to, and the current state of each row. The
retired ICE procedure and its four-run cellular baseline remain readable in
git history and in `docs/field-runs/cellular-to-home-nat-20260907T095841Z.json`;
nothing measured on that transport is release evidence for this one.

## Definitions and gates

- **Cold open:** the app process launched from not-running to a usable
  gateway (`applicationReady`: authenticated link plus `/v2/capabilities`).
  Measured from the kernel's process start time by the app's own
  `cold_open` record (stage `launch`).
- **Foreground/reconnect cycle:** the link owner suspended and resumed the
  way backgrounding does, followed by discovery, a session list, and a
  preview; optionally a terminal attach to first pane byte. Measured by the
  in-app diagnostics runner (`reconnect_cycle` records).
- **Explicit recovery:** recovery after a foreground or network-restored
  event. **Silent break:** a path that stopped carrying bytes without any
  event; detected by the 45-second Noise dead-peer bound and the relay's
  15-second ping.

| Gate | Threshold | Where it is measured |
| --- | --- | --- |
| Cold open to usable gateway | p95 ≤ 5 s on healthy tested networks | `launch` stage of `cold_open` records |
| Explicit foreground/network-restored recovery | p95 ≤ 5 s | `applicationReady` stage of `reconnect_cycle` records |
| Silent-break recovery | ≤ 60 s | manual rows (network switch, relay restart) with the Mac audit timestamps |
| Eligible attempts eventually succeed | no re-pairing, no manual app restart | `succeeded` per record; refusals (revoked/offline) counted separately |
| Duplicate side effects, plaintext leaks, stale grants, automatic takeover | none | manual rows plus the Mac audit and gateway receipts |
| Local grant enforcement | within the documented 250 ms check | revoke-during-stream row |
| Soak | 24 h; handles, tasks, sockets, RSS return to baseline after streams close | Mac-side counters recorded before and after |

Counts required per network: at least 30 cold opens and 30 reconnect
cycles on same LAN, cellular, unrelated Wi-Fi, UDP-blocked/HTTPS-allowed,
and IPv6-only (NAT64); separately 20 network switches, 20 long-suspension
foreground cycles, 10 Mac sleep/wake cycles, 10 each relay/helper/gateway
restarts, one lease expiry, one normal renewal, and one control-plane outage.

## Tooling

Phone side, driven from the Mac over USB (no taps):

```sh
scripts/phone-diagnostics.sh cold-opens 30 --wait 25 [--skip-lan]
scripts/phone-diagnostics.sh cycles 30 [--skip-lan] [--terminal] [--pause 2]
scripts/phone-diagnostics.sh pull /tmp/phone-diag
scripts/phone-diagnostics.sh summary /tmp/phone-diag --since <ISO8601> --label "Cellular"
```

`cold-opens` terminates and relaunches the app with `xcrun devicectl`
(`--terminate-existing`, so every iteration starts from not-running) and the
launch argument `-latchDiagnosticsColdOpen 1`, which arms the app's
`ColdOpenRecorder` to append one line to `Documents/latch-diagnostics/cold-opens.jsonl`
on the first usable gateway or on the first state that ends the attempt
(Mac offline, revoked, pairing required, third failed connect). A launch that
produced no line within the wait is counted as a failure by the harness's
window. `cycles` launches the app with `-latchDiagnosticsCycles N` so the
in-app runner starts without a tap and writes `run-<stamp>.jsonl`; the
diagnostics-only `-latchDiagnosticsSkipLAN 1` selects the relay entry point
on networks where the Mac is also visible locally. Records contain stage
names, milliseconds, `local`/`relay`, and outcome; never a session, prompt,
path, or output.

The plan named an XCUITest harness for cold opens. The delivered harness
launches the real app process over USB with `devicectl` instead: it is the
same real launch from not-running, it needs no UI-test runner installed on
the phone, and the app records the measurement itself rather than a test
process timing a screen. This substitution is recorded in the plan's
Objective 3 record.

Mac side:

```sh
scripts/field-run.sh start <scenario>
# run the phone harness and any manual steps
scripts/field-run.sh finish <scenario> --result pass|fail|partial \
  --phone-log /tmp/phone-diag --since <ISO8601> --note "<network, in general terms>"
scripts/field-run.sh matrix
```

`start`/`finish` diff the Mac's content-free Remote Link audit (stream
opens/closes and coarse events) and embed the phone summary (attempt counts,
failure stages, p50/p95/max per stage) in one JSON record under
`docs/field-runs/`. `scenarios` lists the rows.

Terminal input/output, conversation sends and approvals, preview, and the
takeover/replay rules are exercised by hand in a recorded subset per
network; the record's note says what was done and which attempts were
automated.

## Network recipes

- **Same LAN:** phone on the Mac's Wi-Fi. Both entry points are reachable;
  run one set with the LAN attempt and one with `--skip-lan` so the relay
  path is measured from here as well.
- **Cellular:** Wi-Fi off on the phone, USB still attached for the harness.
  Record the carrier only as "carrier LTE/5G".
- **Unrelated Wi-Fi:** any network that is not the Mac's; the LAN attempt
  is naturally absent.
- **UDP blocked, HTTPS allowed:** a Mac or router hotspot with a firewall
  rule dropping UDP other than DNS (`pf`: `block out proto udp to any port
  != 53`) and the phone on that hotspot with `--skip-lan`.
- **IPv6-only:** macOS Internet Sharing → "Create NAT64 Network" with the
  phone joined to it. The relay hostname's A/AAAA answers (section 2 of the
  operations doc) decide whether the client-to-relay leg is native IPv6 or
  NAT64; record the actual family observed from the phone's `path` and the
  relay's connection log, never inferred from the relay address.
- **Network switch:** open a terminal on Wi-Fi, toggle Wi-Fi off and on;
  count each direction as one switch.
- **Long suspension:** background the app for ≥ 10 minutes between
  foregrounds; iOS will have suspended it.
- **Mac sleep/wake:** `pmset sleepnow`, wait ≥ 2 minutes, wake; the phone
  must show the Mac offline while asleep.
- **Restarts:** relay via Railway `restart-service`; helper via Desktop
  Remote Access off/on (or `kill` of `latch-remote`, which Desktop
  relaunches); gateway via `kill` of the supervised `latch serve` child.
- **Lease and outage:** hold a link past 10 minutes (renewal), block the
  Mac's route to the control plane for > 10 minutes (expiry), and stop the
  Railway control plane for two minutes (outage), observing fail-closed
  admission and recovery.

## Installed identities for this record

| Component | Identity |
| --- | --- |
| Source | `main`, final commit of the Objective 3 session (see plan section 16) |
| Mac payload | `0.2609081113.0`, Developer ID signed and notarized: `latch` `8bb10b62…7eafd2`, `latch-remote` `b47716c5…676d01`, `latchd` `21b00361…4422f` |
| Desktop | `/Applications/Latch.app` `0.2609081113.0`, executable `ea5d6f42…7224`, notarized and stapled |
| Phone | `dev.cooperativ.latch.mobile` 0.1.0 (1), development-signed with the team profile, executable `c7c25fa3…f6594` (final measured build; earlier rows name their build). **No `aps-environment` entitlement** (see outstanding) |
| Control plane | `release` `0e5d4c547f7c4c45ab9cc0129fb33422334671c9`, 7 migrations, `relayConfigured: true`, `apnsConfigured: false` |
| Relay | Railway service `latch-relay`, deployment `43ee28fe-57f6-44cb-aa9b-67a6530708bf` from the same commit |

## Results

The table is regenerated by `scripts/field-run.sh matrix` from
`docs/field-runs/` and pasted here after each run; until a row has a record
it reads "not yet run". Per-stage numbers are p95 in milliseconds over the
successful automated attempts unless stated.

**Same LAN, 8 September 2026**, phone on the Mac's Wi-Fi (192.168.0.108 and
192.168.0.160), VPN off, USB-driven. Two records, because the first run
exposed that the LAN carrier was never used:

- Relay path (`docs/field-runs/same-lan-20260908T085431Z.json`): 30 real
  cold opens, all succeeded, launch-to-usable-gateway p50 923 / p95 1500 /
  max 1617 ms; 60 reconnect cycles (30 with the LAN attempt allowed, 30
  relay-only), all succeeded, explicit-resume-to-usable-gateway p50 769 /
  p95 1030 / max 1231 ms, session list p95 265, preview p95 344. Every
  attempt authenticated over the relay: the phone's browser discarded the
  Mac it found (no TXT record with the descriptor it used), so "no LAN
  peer" and the relay by default. Fixed the same day (plan section 16).
- LAN path (`docs/field-runs/same-lan-20260908T094151Z.json`, after the
  fixes): 30 reconnect cycles, all over the LAN entry, resume-to-usable
  gateway p50 357 / p95 429 / max 461 ms, session list p95 115, preview
  p95 46; 30 real cold opens, all over the LAN entry, launch-to-usable
  gateway p50 385 / p95 474 / max 543 ms (link ready p95 103, discovery
  complete p95 175).

**Cellular, 8 September 2026** (`docs/field-runs/cellular-*.json`, Wi-Fi
off, VPN off, USB-driven): 30 reconnect cycles, all succeeded, resume to
usable gateway p50 3357 / p95 3493 / max 3513 ms (link ready p95 792,
session list p95 268, preview p95 282); 30 real cold opens, all succeeded,
launch to usable gateway p50 3597 / p95 3789 / max 4219 ms (link ready p95
986, discovery complete p95 1162). The owner verified a terminal by hand
with Wi-Fi off. The cycle figure is larger than the cold-open link figure
because it includes closing the previous link over cellular within its
bounded drain; that is the explicit-event recovery as the user experiences
it and is what the gate measures.

Restart rows, 8 September 2026, same LAN, phone recovering on its own with
the app in front and the content-free trace on (recovery is measured on the
phone from its first `connecting` after the loss to the next applied
`ready`; the Mac audit records each `link_closed`/`link_ready`):

- Relay restarts (10, `railway restart` 75 s apart, phone forced onto the
  relay path): 10 of 10 recovered, p50 1428 / p95 1804 / max 1804 ms. The Mac
  helper re-admitted itself each time without a gateway restart.
- Helper restarts and gateway restarts: the first attempt at ten kills 30 s
  apart measured Desktop's crash-loop backoff instead (31–35 s per restart),
  which exposed that the restart schedule never reset after a healthy run
  (plan section 16). Rerun after that fix with 75 s spacing: 4.0–4.5 s per
  recovery (helper p95 4.3 s over 6 observed losses, the phone having been
  backgrounded for four of the kills; gateway p95 4.5 s over 10), the time
  going into one-second connect bounds on two unreachable bridge addresses
  the Mac had published. Rerun again with `en*`-only addresses and the
  1.5 s LAN-phase budget: helper restarts recovered in 2.2–2.6 s (p95 2.6 s)
  and gateway restarts in 2.1–2.5 s (p95 2.5 s over 8 observed). Every
  recovery ended on the relay carrier because
  the restarted helper's LAN listener moves to a new port and the phone's
  cached Bonjour record still names the old one; the next LAN attempt after
  the record refreshes returns to the LAN.

Also measured on this network: pairing, approval to first served request
3 s; helper restart (Desktop app swap), phone streams served again 2 s after
the helper came up; lease renewal, three renewals at the 5-minute marks with
no link interruption.

| Scenario | Automated attempts | Manual subset | Result |
| --- | --- | --- | --- |
| Same LAN | LAN path: 30 cold opens 30/30, p95 474 ms; 30 cycles 30/30, p95 429 ms. Relay path: 30 cold opens 30/30, p95 1500 ms; 60 cycles 60/60, p95 1030 ms | terminal `pwd` verified by the owner (after the stream-lock fix) | gates met on both carriers |
| Cellular (Wi-Fi off) | 30 cold opens 30/30, p95 3789 ms (max 4219); 30 cycles 30/30, p95 3493 ms; all relay | terminal `pwd` verified by the owner | gates met |
| Unrelated Wi-Fi | not yet run | not yet run | — |
| UDP blocked, HTTPS allowed | not yet run | not yet run | — |
| IPv6-only (NAT64) | not yet run | not yet run | — |
| Network switches (20) | — | not yet run | — |
| Long suspensions (20) | not yet run | not yet run | — |
| Mac sleep/wake (10) | — | not yet run | — |
| Relay restarts (10) | 10/10 recovered, phone loss-to-usable-gateway p50 1428 / p95 1804 / max 1804 ms (relay path, LAN skipped) | — | gate met |
| Helper / gateway restarts (10 each) | helper p95 2.6 s, gateway p95 2.5 s on the final build | — | silent-break gate met |
| Lease expiry, renewal, control-plane outage | renewal: 3 renewals at the 5-minute marks, no interruption; control-plane restart: no observable unavailability at 2 s polling, link unaffected | — | renewal met; expiry and a real outage not yet run |
| Real APNs attention delivery | — | blocked: no APNs key, no push entitlement in the interim build | — |
| 24-hour soak with high output | — | not yet run | — |

## Outstanding before this record is complete

1. The phone build with the push entitlement needs the owner: sign in to
   Xcode with the Apple ID for team `X84RPB4674`, enable Push Notifications
   on the App ID `dev.cooperativ.latch.mobile`, create an APNs
   authentication key, and place `APNS_KEY_ID`, `APNS_TEAM_ID`,
   `APNS_PRIVATE_KEY_PEM`, `APNS_TOPIC`, `APNS_ENVIRONMENT=sandbox` in the
   control-plane secret store. Until then the interim build is installed
   without `aps-environment` and the APNs row cannot run.
2. Re-pairing is a physical step: Desktop → Pair a Device, scan on the
   phone (or paste the code), compare the words, approve on the Mac.
3. Wi-Fi off for the cellular rows, joining the hotspot and NAT64 networks,
   the Mac sleep cycles, and the manual terminal/approval subsets need a
   person at the phone; the harness is run from the Mac while they are.

## What is already measured, and where

These do not need a field run; they are named so the rows are not asked to
re-prove them.

- **Composed real-socket WSS through both authenticated endpoints to the
  gateway**, on IPv4 and IPv6 with real TLS:
  `crates/latch-remote/tests/remote_link_composed.rs`.
- **Relay kill and re-admission through the same gateway, controller
  replacement, the Mac-offline bound, and foreign-thread close with a
  blocked writer:** `crates/latch-remote/tests/remote_link_recovery.rs`.
- **Keepalive, dead-peer detection, closed-signal, cancel-safe LAN
  framing:** `crates/latch-transport` unit tests.
- **Terminal resume capability, refusal without takeover, no input replay;
  Hub operation scoping and epoch rotation; creation receipts; attention
  spool:** `crates/latch` tests.
- **Single-use tickets, generation replacement, lease extension, room
  invalidation, backpressure bound:** `services/relay/src/server.test.ts`.
- **Enrollment atomicity, admission races, revocation outbox, push token
  lifecycle, migration 0007 retirement:** `services/control-plane` suite in
  memory and on disposable PostgreSQL.
- **The coordinator's states, backoff, foreground/network triggers, and
  discovery per generation; the cold-open recorder and launch options:**
  `apps/LatchMobile/Tests/LatchMobileKitTests`.
