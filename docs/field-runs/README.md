# Recorded field runs

One JSON file per run of a scenario in
[../REMOTE_ACCESS_FIELD_VERIFICATION.md](../REMOTE_ACCESS_FIELD_VERIFICATION.md),
written by `scripts/field-run.sh finish`. Each holds the scenario, the result,
what the Mac's path counters gained during the run, and whatever the person
running it typed about the network and the phone.

The directory is empty until someone runs one. That emptiness is the honest
state of the physical rows in
[../REMOTE_ACCESS_PHASE_4.md](../REMOTE_ACCESS_PHASE_4.md), and it is why those
rows read "not yet run".

`scripts/field-run.sh matrix` renders everything here as a table, keeping the
most recent run per scenario.

Describe networks in general terms. These files are committed; "hotel Wi-Fi,
UDP blocked outbound" is the useful part and the venue is not.
