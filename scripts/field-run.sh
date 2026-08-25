#!/usr/bin/env bash
# Records one remote-access field scenario as evidence rather than as a memory.
#
# The physical rows in docs/REMOTE_ACCESS_PHASE_4.md cannot be produced by any
# test in this repository: they need a phone on a carrier, a hotel Wi-Fi, a Mac
# that actually goes to sleep. What this script does is make the run leave a
# record — a before/after diff of the Mac's own path counters, plus what the
# person saw — so a filled-in matrix row can be traced back to a measurement
# instead of a recollection.
#
#   scripts/field-run.sh start  cellular-to-home-nat
#   ... run the scenario on the phone ...
#   scripts/field-run.sh finish cellular-to-home-nat --result pass \
#       --phone "Direct 3 · Relay 0" --note "AT&T LTE, home router in NAT mode"
#
# Results land in docs/field-runs/ as JSON, one file per run, and `finish`
# prints the matrix row to paste into Phase 4.
#
# Nothing here uploads anything. The diagnostics bundle it reads is the
# content-free one: switches, counts, and coarse event names, no addresses,
# names, keys, or session content. The scenario note is written by whoever runs
# it, so keep networks described in general terms rather than named.

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
                                 [--note "<what happened>"]
  field-run.sh matrix

Scenarios are the six physical rows in docs/REMOTE_ACCESS_PHASE_4.md; run
`field-run.sh scenarios` for the list and what each one is looking for.
EOF
    exit 2
}

# The scenario list is here rather than in the doc so the script can refuse a
# typo. Each line is: id|title|what a pass looks like.
scenarios() {
    cat <<'EOF'
cellular-to-home-nat|Cellular to home NAT|Terminal opens off-LAN; the Mac counts a direct_reflexive connection and the phone shows Direct.
symmetric-nat|Symmetric NAT|Terminal opens; the Mac counts a relay connection and the phone shows Relay. Failing to connect at all is a fail, not a relay.
udp-blocked|Hotel or corporate Wi-Fi with UDP blocked|Terminal opens over TURN on TCP/TLS 443; the Mac counts a relay connection.
wifi-to-cellular|Wi-Fi to cellular mid-terminal|The terminal survives the interface change, or reconnects without losing the session; a path migration is expected and acceptable.
mac-sleep-wake|Mac sleep and wake|While asleep the phone says the Mac is asleep rather than showing a transport error; after wake the terminal reconnects.
phone-background|Phone background and foreground|Returning to the app reconnects without re-pairing and without a stuck spinner.
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
    if ! "$latch_bin" remote-access diagnostics 2>/dev/null; then
        echo "cannot read diagnostics; set LATCH_BIN to the latch executable" >&2
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
    echo "On the phone, Settings > Linked computer > Reset path counters, then run"
    echo "the scenario. Finish with:"
    echo "  scripts/field-run.sh finish $scenario --result pass --phone \"...\" --note \"...\""
}

cmd_finish() {
    local scenario="$1"
    shift
    require_scenario "$scenario"
    local result="" phone="" note=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --result) result="${2:-}"; shift 2 ;;
            --phone) phone="${2:-}"; shift 2 ;;
            --note) note="${2:-}"; shift 2 ;;
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
