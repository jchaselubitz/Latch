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
| Source | commit of the Objective 3 delivery (recorded when committed) |
| Mac payload | version, and SHA-256 of `latch`, `latch-remote`, `latchd` from the signed archive (recorded at install) |
| Desktop | `/Applications/Latch.app` `CFBundleShortVersionString` (recorded at install) |
| Phone | `dev.cooperativ.latch.mobile` 0.1.0 (1), development-signed with the team profile, built with Xcode 27 beta against the iOS 27.0 SDK; executable and `LatchTransportFFI` SHA-256 recorded at install. **Interim build has no `aps-environment` entitlement** (see outstanding) |
| Control plane | `release` from `GET /health/ready` |
| Relay | Railway deployment id |

## Results

No Remote Link matrix row has been run yet. The table is regenerated by
`scripts/field-run.sh matrix` from `docs/field-runs/` and pasted here after
each run; until a row has a record it reads "not yet run".

| Scenario | Automated attempts | Manual subset | Result |
| --- | --- | --- | --- |
| Same LAN | not yet run | not yet run | — |
| Cellular | not yet run | not yet run | — |
| Unrelated Wi-Fi | not yet run | not yet run | — |
| UDP blocked, HTTPS allowed | not yet run | not yet run | — |
| IPv6-only (NAT64) | not yet run | not yet run | — |
| Network switches (20) | — | not yet run | — |
| Long suspensions (20) | not yet run | not yet run | — |
| Mac sleep/wake (10) | — | not yet run | — |
| Relay/helper/gateway restarts (10 each) | — | not yet run | — |
| Lease expiry, renewal, control-plane outage | — | not yet run | — |
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
