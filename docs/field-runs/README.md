# Recorded field runs

One JSON file per run of a scenario in
[../REMOTE_ACCESS_FIELD_VERIFICATION.md](../REMOTE_ACCESS_FIELD_VERIFICATION.md),
written by `scripts/field-run.sh finish`. Each holds the scenario, the result,
what the Mac's path counters gained during the run, and whatever the person
running it typed about the network and the phone.

A scenario without a record here is "not yet run" in the field report; that
is the honest state and the report says so rather than filling the row.

`scripts/field-run.sh matrix` renders everything here as a table, keeping the
most recent run per scenario.

Describe networks in general terms. These files are committed; "hotel Wi-Fi,
UDP blocked outbound" is the useful part and the venue is not.

The phone-side counterpart is the in-app diagnostics runner (Settings →
Diagnostics, or started by `scripts/phone-diagnostics.sh cycles` over USB),
which writes one JSON Lines file per run under the app's
Documents/latch-diagnostics with per-attempt stage timings (`linkReady`,
`discovery`, `applicationReady`, `sessionList`, `preview`, and optionally
`terminalFirstOutput`), the path used, and whether the LAN attempt was skipped,
plus `cold-opens.jsonl` with one `cold_open` line per launch driven by
`scripts/phone-diagnostics.sh cold-opens`. `scripts/phone-diagnostics.sh pull`
copies them to the Mac and `scripts/field-run.sh finish --phone-log` embeds
their summary (counts, failure stages, p50/p95/max per stage) in the run
record here, so one file holds both sides of a matrix row.
