#!/usr/bin/env python3
"""Turns two diagnostics snapshots into one recorded field-run result.

`scripts/field-run.sh` calls this; it is separate only because computing a
delta between two JSON documents in shell is worse than it sounds.

What it emits is deliberately narrow. The diagnostics bundle is content-free by
contract, and this must not become the place that widens it: only the
path-selection counters and the coarse event names are read out, and the note
is whatever the person running the scenario typed.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROUTES = ("lan", "direct_host", "direct_reflexive", "relay", "unknown")


def delta(before: dict, after: dict) -> dict:
    """Counters gained between the two snapshots.

    The audit trail is bounded, so a long-running Mac can age rows out and make
    a counter fall. A negative delta is reported as such rather than clamped:
    silently showing zero would turn "the evidence rolled over" into "nothing
    happened", and those need different responses from whoever reads it.
    """
    before_paths = before.get("pathSelection", {})
    after_paths = after.get("pathSelection", {})
    before_routes = before_paths.get("routes", {})
    after_routes = after_paths.get("routes", {})

    result = {
        "routes": {
            route: after_routes.get(route, 0) - before_routes.get(route, 0)
            for route in ROUTES
        }
    }
    for field in ("connections", "direct", "relay", "iceAnswers", "iceAnswersConnected"):
        result[field] = after_paths.get(field, 0) - before_paths.get(field, 0)

    before_events = before.get("eventCounts", {})
    after_events = after.get("eventCounts", {})
    events = {}
    for name in set(before_events) | set(after_events):
        gained = after_events.get(name, 0) - before_events.get(name, 0)
        if gained:
            events[name] = gained
    result["events"] = dict(sorted(events.items()))
    return result


def summarize(routes: dict) -> str:
    counted = ", ".join(f"{name} {count}" for name, count in routes.items() if count)
    return counted or "none counted"


def row(run: dict) -> str:
    macs = run["macDelta"]
    measured = f'{macs["connections"]} connection(s): {summarize(macs["routes"])}'
    cells = [
        run["title"],
        run["result"],
        measured,
        run.get("phone") or "-",
        run.get("note") or "-",
    ]
    return "| " + " | ".join(cell.replace("|", "\\|") for cell in cells) + " |"


def matrix(runs_dir: Path) -> str:
    runs = []
    for path in sorted(runs_dir.glob("*.json")):
        if path.name.startswith("."):
            continue
        runs.append(json.loads(path.read_text()))
    # Latest run per scenario wins: a scenario re-run after a fix should not
    # leave the failure standing beside the pass in the same table.
    latest: dict[str, dict] = {}
    for run in sorted(runs, key=lambda run: run["recordedAt"]):
        latest[run["scenario"]] = run
    lines = [
        "| Scenario | Result | Measured on the Mac | Phone counters | Notes |",
        "| --- | --- | --- | --- | --- |",
    ]
    lines.extend(row(run) for run in latest.values())
    return "\n".join(lines)


def main() -> int:
    if len(sys.argv) == 3 and sys.argv[1] == "--matrix":
        print(matrix(Path(sys.argv[2])))
        return 0
    if len(sys.argv) == 3 and sys.argv[1] == "--row":
        print(row(json.loads(Path(sys.argv[2]).read_text())))
        return 0
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2

    before = json.loads(Path(sys.argv[1]).read_text())
    after = json.loads(Path(sys.argv[2]).read_text())
    run = {
        "schemaVersion": 1,
        "scenario": os.environ["LATCH_SCENARIO"],
        "title": os.environ["LATCH_TITLE"],
        "result": os.environ["LATCH_RESULT"],
        "phone": os.environ.get("LATCH_PHONE", ""),
        "note": os.environ.get("LATCH_NOTE", ""),
        "recordedAt": os.environ["LATCH_STAMP"],
        "macDelta": delta(before, after),
    }
    print(json.dumps(run, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
