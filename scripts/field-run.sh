#!/usr/bin/env bash
# Records one remote-access field scenario as evidence rather than as a memory.
#
# The physical rows of the Remote Link matrix cannot be produced by any
# test in this repository: they need a phone on a carrier, a hotel Wi-Fi, a Mac
# that actually goes to sleep. What this script does is make the run leave a
# record — a before/after diff of the Mac's own path counters, plus what the
# person saw — so a filled-in matrix row can be traced back to a measurement
# instead of a recollection. The phone's own per-attempt stage timings (from
# scripts/phone-diagnostics.sh) attach to the same record with --phone-log.
#
#   scripts/field-run.sh start  cellular
#   ... run the scenario on the phone ...
#   scripts/field-run.sh finish cellular --result pass \
#       --phone-log /tmp/phone-diag --since 2026-09-08T10:00:00Z \
#       --note "carrier LTE, Wi-Fi off, 30 cold opens and 30 cycles"
#
# Results land in docs/field-runs/ as JSON, one file per run, and `finish`
# prints the matrix row to paste into the field report.
#
# Nothing here uploads anything. It reads the content-free local Remote Link
# audit: coarse event names and opaque device identifiers, with no addresses,
# keys, gateway credentials, or session content. The scenario note is written
# by whoever runs it, so keep networks general rather than named.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runs_dir="${LATCH_FIELD_RUNS_DIR:-$repo_root/docs/field-runs}"
latch_bin="${LATCH_BIN:-latch}"

usage() {
    cat >&2 <<'EOF'
usage:
  field-run.sh scenarios
  field-run.sh start  <scenario>
  field-run.sh finish <scenario> --result pass|fail|partial
                                 [--phone "<the phone's Paths so far row>"]
                                 [--phone-log <dir or .jsonl pulled by
                                               scripts/phone-diagnostics.sh pull>]
                                 [--since <ISO 8601: ignore older phone attempts>]
                                 [--note "<what happened>"]
  field-run.sh matrix

Scenarios are the rows of the Remote Link physical matrix; run
`field-run.sh scenarios` for the list and what each one is looking for.
EOF
    exit 2
}

# The scenario list is here rather than in the doc so the script can refuse a
# typo. Each line is: id|title|what a pass looks like. These are the rows of
# the Remote Link physical matrix in docs/PLAN_REMOTE_RELAY_REPLACEMENT.md
# section 9; the retired ICE rows stay readable in docs/field-runs/ history.
scenarios() {
    cat <<'EOF'
same-lan|Same LAN|Cold opens and reconnect cycles reach a usable gateway over the LAN entry point; p95 within the gate.
cellular|Phone on cellular, Wi-Fi off|Cold opens and reconnect cycles reach a usable gateway through the relay on TCP 443; p95 within the gate.
unrelated-wifi|Unrelated Wi-Fi|Same as cellular from a network that is not the Mac's, with the LAN attempt naturally absent.
udp-blocked|UDP blocked, HTTPS allowed|Relay path measured with the diagnostics-only skip-LAN setting on a hotspot that drops non-DNS UDP.
ipv6-only|IPv6-only (NAT64) network|Cold opens and reconnect cycles succeed; the actual client-to-relay family is recorded, not inferred.
network-switch|Wi-Fi to cellular and back mid-session|Twenty switches; the owner reconnects without re-pairing, no terminal input replay, no automatic takeover.
long-suspension|Long background suspension|Twenty background/foreground cycles after long suspensions recover within the explicit-event gate.
mac-sleep-wake|Mac sleep and wake|Ten cycles; the phone shows the Mac offline while asleep and recovers after wake from the point networking is usable.
relay-restart|Relay restart|Ten restarts; both endpoints re-admit through the same gateway without losing local sessions.
helper-restart|Helper restart|Ten restarts; Desktop restarts the helper and the phone recovers without re-pairing.
gateway-restart|Gateway restart|Ten restarts; discovery notices the new instance and re-bases sockets; no duplicate side effects.
lease-and-outage|Lease expiry, renewal, and control-plane outage|One expiry, one normal renewal, and one control-plane outage; the link fails closed and recovers.
soak|24-hour soak|Repeated streams plus a high-output workload; handles, tasks, sockets, and RSS return to baseline after streams close.
cellular-to-home-nat|Cellular to home NAT (retired ICE row)|Historical; kept so the recorded baseline stays renderable.
EOF
}

scenario_title() {
    scenarios | awk -F'|' -v id="$1" '$1 == id { print $2 }'
}

require_scenario() {
    if [[ -z "$(scenario_title "$1")" ]]; then
        echo "unknown scenario: $1" >&2
        echo "known scenarios:" >&2
        scenarios | awk -F'|' '{ printf "  %-22s %s\n", $1, $2 }' >&2
        exit 2
    fi
}

diagnostics() {
    if ! "$latch_bin" remote-access audit --json 2>/dev/null; then
        echo "cannot read the Remote Link audit; set LATCH_BIN to the latch executable" >&2
        exit 1
    fi
}

cmd_start() {
    local scenario="$1"
    require_scenario "$scenario"
    mkdir -p "$runs_dir"
    local baseline="$runs_dir/.$scenario.baseline.json"
    diagnostics >"$baseline"
    echo "baseline recorded for $scenario ($(scenario_title "$scenario"))"
    echo
    scenarios | awk -F'|' -v id="$scenario" '$1 == id { print "looking for: " $3 }'
    echo
    echo "Run the scenario on the phone, then finish with:"
    echo "  scripts/field-run.sh finish $scenario --result pass --phone \"...\" --note \"...\""
}

cmd_finish() {
    local scenario="$1"
    shift
    require_scenario "$scenario"
    local result="" phone="" note="" phone_log="" since=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --result) result="${2:-}"; shift 2 ;;
            --phone) phone="${2:-}"; shift 2 ;;
            --note) note="${2:-}"; shift 2 ;;
            --phone-log) phone_log="${2:-}"; shift 2 ;;
            --since) since="${2:-}"; shift 2 ;;
            *) usage ;;
        esac
    done
    case "$result" in
        pass|fail|partial) ;;
        *) echo "--result must be pass, fail, or partial" >&2; exit 2 ;;
    esac

    local baseline="$runs_dir/.$scenario.baseline.json"
    if [[ ! -f "$baseline" ]]; then
        echo "no baseline for $scenario; run 'field-run.sh start $scenario' first" >&2
        exit 1
    fi

    mkdir -p "$runs_dir"
    local stamp
    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    local out="$runs_dir/$scenario-$stamp.json"
    diagnostics >"$runs_dir/.$scenario.after.json"

    LATCH_SCENARIO="$scenario" \
    LATCH_TITLE="$(scenario_title "$scenario")" \
    LATCH_RESULT="$result" \
    LATCH_PHONE="$phone" \
    LATCH_NOTE="$note" \
    LATCH_STAMP="$stamp" \
    LATCH_PHONE_LOG="$phone_log" \
    LATCH_PHONE_SINCE="$since" \
    python3 "$repo_root/scripts/field_run_delta.py" \
        "$baseline" "$runs_dir/.$scenario.after.json" >"$out"

    rm -f "$baseline" "$runs_dir/.$scenario.after.json"
    echo "recorded $out"
    echo
    python3 "$repo_root/scripts/field_run_delta.py" --row "$out"
}

cmd_matrix() {
    if ! compgen -G "$runs_dir/*.json" >/dev/null; then
        echo "no runs recorded yet" >&2
        exit 1
    fi
    python3 "$repo_root/scripts/field_run_delta.py" --matrix "$runs_dir"
}

case "${1:-}" in
    scenarios) scenarios | awk -F'|' '{ printf "%-22s %s\n    %s\n", $1, $2, $3 }' ;;
    start) [[ $# -eq 2 ]] || usage; cmd_start "$2" ;;
    finish) [[ $# -ge 2 ]] || usage; shift; cmd_finish "$@" ;;
    matrix) cmd_matrix ;;
    *) usage ;;
esac
