#!/usr/bin/env bash
# Drives the Objective 3 physical matrix from the Mac over USB.
#
# Cold opens are real launches: each iteration terminates the app and starts
# it again with `xcrun devicectl`, and the app itself records one
# `cold_open` line (launch to usable gateway, or the failure that ended it)
# because it was launched with `-latchDiagnosticsColdOpen 1`. Reconnect cycles
# use the in-app runner (Settings › Diagnostics) started by launch argument so
# no tap is needed. Results are pulled from the app's Documents container.
#
#   scripts/phone-diagnostics.sh cold-opens 30 [--wait 25] [--skip-lan]
#   scripts/phone-diagnostics.sh cycles 30 [--skip-lan] [--terminal] [--pause 2]
#   scripts/phone-diagnostics.sh pull <dest-dir>
#   scripts/phone-diagnostics.sh summary <dest-dir> [--label "Same LAN"] ...
#
# LATCH_PHONE_DEVICE selects the CoreDevice identifier (default: the first
# iPhone devicectl lists). LATCH_PHONE_BUNDLE defaults to the app's bundle id.
# Nothing here reads or writes session content; the records are stage names
# and milliseconds.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bundle="${LATCH_PHONE_BUNDLE:-dev.cooperativ.latch.mobile}"

device() {
    if [[ -n "${LATCH_PHONE_DEVICE:-}" ]]; then
        printf '%s' "$LATCH_PHONE_DEVICE"
        return
    fi
    local found
    found="$(xcrun devicectl list devices --json-output /dev/stdout 2>/dev/null \
        | python3 -c 'import json,sys; d=json.load(sys.stdin)["result"]["devices"]; p=[x for x in d if x.get("hardwareProperties",{}).get("deviceType")=="iPhone" and x.get("connectionProperties",{}).get("pairingState")=="paired"]; print(p[0]["identifier"] if p else "")')"
    if [[ -z "$found" ]]; then
        echo "no paired iPhone found; set LATCH_PHONE_DEVICE" >&2
        exit 1
    fi
    printf '%s' "$found"
}

usage() {
    sed -n '2,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

launch() {
    # Prints the launched pid. `--terminate-existing` makes every launch a
    # launch from not-running, which is what a cold open means.
    local dev="$1"; shift
    local out
    out="$(mktemp)"
    xcrun devicectl device process launch --device "$dev" --terminate-existing \
        --json-output "$out" "$bundle" -- "$@" >/dev/null
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"]["process"]["processIdentifier"])' "$out"
    rm -f "$out"
}

terminate() {
    local dev="$1" pid="$2"
    xcrun devicectl device process signal --device "$dev" --pid "$pid" --signal SIGKILL >/dev/null 2>&1 || true
}

cmd_cold_opens() {
    local count="$1"; shift
    local wait=25 skip_lan=0
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --wait) wait="$2"; shift 2 ;;
            --skip-lan) skip_lan=1; shift ;;
            *) usage ;;
        esac
    done
    local dev; dev="$(device)"
    local started; started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "cold opens: $count launches on $dev, $wait s each, skip-lan=$skip_lan, from $started"
    local i pid
    for ((i = 1; i <= count; i++)); do
        pid="$(launch "$dev" -latchDiagnosticsColdOpen 1 -latchDiagnosticsSkipLAN "$skip_lan")"
        printf '  %3d/%d launched pid %s\n' "$i" "$count" "$pid"
        sleep "$wait"
        terminate "$dev" "$pid"
        # Let the OS finish tearing the process down before the next launch.
        sleep 2
    done
    echo "done; pull with: $0 pull <dir> && $0 summary <dir> --since $started --kind cold_open"
}

cmd_cycles() {
    local count="$1"; shift
    local skip_lan=0 terminal=0 pause=2
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --skip-lan) skip_lan=1; shift ;;
            --terminal) terminal=1; shift ;;
            --pause) pause="$2"; shift 2 ;;
            *) usage ;;
        esac
    done
    local dev; dev="$(device)"
    local started; started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    local pid
    pid="$(launch "$dev" -latchDiagnosticsCycles "$count" -latchDiagnosticsSkipLAN "$skip_lan" \
        -latchDiagnosticsTerminal "$terminal" -latchDiagnosticsPause "$pause")"
    echo "reconnect cycles: $count started in pid $pid on $dev at $started (skip-lan=$skip_lan terminal=$terminal pause=${pause}s)"
    echo "the app writes run-<stamp>.jsonl as it goes; pull with: $0 pull <dir> && $0 summary <dir> --since $started --kind reconnect_cycle"
}

cmd_pull() {
    local dest="$1"
    local dev; dev="$(device)"
    mkdir -p "$dest"
    xcrun devicectl device copy from --device "$dev" --domain-type appDataContainer \
        --domain-identifier "$bundle" --source Documents/latch-diagnostics --destination "$dest" >/dev/null
    echo "pulled into $dest:"
    find "$dest" -name '*.jsonl' -exec wc -l {} +
}

cmd_summary() {
    python3 "$repo_root/scripts/diagnostics_summary.py" "$@"
}

case "${1:-}" in
    cold-opens) [[ $# -ge 2 ]] || usage; shift; cmd_cold_opens "$@" ;;
    cycles) [[ $# -ge 2 ]] || usage; shift; cmd_cycles "$@" ;;
    pull) [[ $# -eq 2 ]] || usage; cmd_pull "$2" ;;
    summary) [[ $# -ge 2 ]] || usage; shift; cmd_summary "$@" ;;
    *) usage ;;
esac
