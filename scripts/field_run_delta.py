#!/usr/bin/env python3
"""Turns two Remote Link audit snapshots into one recorded field-run result.

`scripts/field-run.sh` calls this; it is separate only because computing a
delta between two JSON documents in shell is worse than it sounds.

What it emits is deliberately narrow: only coarse event names and stream-open
counts are read out, and the note is whatever the person running the scenario
typed. The Objective 2 diagnostics runner will add per-stage timings without
widening this content boundary.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from collections import Counter
from pathlib import Path

def delta(before: list[dict], after: list[dict]) -> dict:
    """Coarse events gained between two bounded audit snapshots."""
    before_events = Counter(item.get("event") for item in before if item.get("event"))
    after_events = Counter(item.get("event") for item in after if item.get("event"))
    gained = {
        name: count - before_events.get(name, 0)
        for name, count in after_events.items()
        if count - before_events.get(name, 0)
    }
    return {
        "streamsOpened": gained.get("remote_link_stream_opened", 0),
        "streamsClosed": gained.get("remote_link_stream_closed", 0),
        "events": dict(sorted(gained.items())),
    }


def phone_summary(log: str, since: str) -> dict | None:
    """Per-kind attempt counts and stage percentiles from the phone's records."""
    if not log:
        return None
    script = Path(__file__).with_name("diagnostics_summary.py")
    args = [sys.executable, str(script), log, "--json"]
    if since:
        args += ["--since", since]
    output = subprocess.run(args, check=True, capture_output=True, text=True).stdout
    return json.loads(output)


def phone_cell(run: dict) -> str:
    summary = run.get("phoneSummary")
    if not summary:
        return run.get("phone") or "-"
    parts = []
    for kind, body in summary.items():
        p95 = body.get("stages", {}).get("applicationReady", {}).get("p95")
        launch = body.get("stages", {}).get("launch", {}).get("p95")
        detail = f'{body["succeeded"]}/{body["attempts"]} {kind.replace("_", " ")}s'
        if launch is not None:
            detail += f", p95 launch {launch} ms"
        elif p95 is not None:
            detail += f", p95 ready {p95} ms"
        parts.append(detail)
    return "; ".join(parts) or "-"


def row(run: dict) -> str:
    macs = run["macDelta"]
    if "streamsOpened" in macs:
        measured = f'{macs["streamsOpened"]} stream(s) opened, {macs["streamsClosed"]} closed'
    else:
        # Historical schema-v1 field runs stay readable after the transport
        # replacement without teaching new tooling the retired path taxonomy.
        measured = f'{macs.get("connections", 0)} historical connection(s)'
    cells = [
        run["title"],
        run["result"],
        measured,
        phone_cell(run),
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
        "schemaVersion": 2,
        "scenario": os.environ["LATCH_SCENARIO"],
        "title": os.environ["LATCH_TITLE"],
        "result": os.environ["LATCH_RESULT"],
        "phone": os.environ.get("LATCH_PHONE", ""),
        "note": os.environ.get("LATCH_NOTE", ""),
        "recordedAt": os.environ["LATCH_STAMP"],
        "macDelta": delta(before, after),
    }
    summary = phone_summary(os.environ.get("LATCH_PHONE_LOG", ""), os.environ.get("LATCH_PHONE_SINCE", ""))
    if summary is not None:
        run["phoneLog"] = os.path.basename(os.environ["LATCH_PHONE_LOG"].rstrip("/"))
        run["phoneSummary"] = summary
    print(json.dumps(run, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
