#!/usr/bin/env python3
"""Summarises phone-side diagnostics records for the physical matrix.

Input is one or more JSON Lines files (or a directory of them) written by the
app: `run-*.jsonl` from the in-app reconnect-cycle runner and
`cold-opens.jsonl` from the USB cold-open harness. Every line is one
`DiagnosticsAttempt`: a kind, a path, per-stage millisecond samples, and the
first failed stage. Nothing in a record names a session, a prompt, a path on
disk, or output, and this script adds nothing.

    scripts/diagnostics_summary.py <file-or-dir>... [--json] [--since ISO8601]
                                   [--kind cold_open|reconnect_cycle]
                                   [--label "Same LAN"]

The default output is the Markdown block pasted into the field report. `--json`
emits the same numbers for `field-run.sh finish --phone-log`.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

GATE_STAGES = ("launch", "linkReady", "applicationReady", "sessionList", "preview", "terminalFirstOutput")


def load(paths: list[str], since: datetime | None) -> list[dict]:
    attempts: list[dict] = []
    files: list[Path] = []
    for raw in paths:
        path = Path(raw)
        if path.is_dir():
            files.extend(sorted(p for p in path.rglob("*.jsonl")))
        else:
            files.append(path)
    for file in files:
        for line in file.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            if since is not None:
                started = datetime.fromisoformat(record["startedAt"].replace("Z", "+00:00"))
                if started < since:
                    continue
            record["_file"] = file.name
            attempts.append(record)
    return attempts


def percentile(values: list[int], fraction: float) -> int | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, math.ceil((len(ordered) - 1) * fraction))
    return ordered[index]


def stage_ms(attempt: dict, stage: str) -> int | None:
    for sample in attempt.get("stages", []):
        if sample.get("stage") == stage and sample.get("outcome") == "ok":
            return int(sample["milliseconds"])
    return None


def summarise(attempts: list[dict]) -> dict:
    succeeded = [a for a in attempts if a.get("succeeded")]
    failed = [a for a in attempts if not a.get("succeeded")]
    stages: dict[str, dict] = {}
    for stage in GATE_STAGES:
        values = [ms for a in succeeded if (ms := stage_ms(a, stage)) is not None]
        if values:
            stages[stage] = {
                "samples": len(values),
                "p50": percentile(values, 0.5),
                "p95": percentile(values, 0.95),
                "max": max(values),
            }
    return {
        "attempts": len(attempts),
        "succeeded": len(succeeded),
        "failed": len(failed),
        "failureStages": dict(sorted(Counter(a.get("failureStage") or "unknown" for a in failed).items())),
        "paths": dict(sorted(Counter(a.get("path") or "none" for a in attempts).items())),
        "skippedLAN": sum(1 for a in attempts if a.get("skippedLAN")),
        "stages": stages,
        "first": min((a["startedAt"] for a in attempts), default=None),
        "last": max((a["startedAt"] for a in attempts), default=None),
    }


def markdown(label: str, kind: str, summary: dict) -> str:
    lines = [f"**{label} — {kind.replace('_', ' ')}s**: {summary['succeeded']} of {summary['attempts']} succeeded"]
    if summary["failed"]:
        stages = ", ".join(f"{name} ×{count}" for name, count in summary["failureStages"].items())
        lines[0] += f" (failed at: {stages})"
    paths = ", ".join(f"{name} {count}" for name, count in summary["paths"].items())
    lines.append(f"Paths: {paths}; LAN attempt skipped on {summary['skippedLAN']} attempt(s).")
    if summary["stages"]:
        lines.append("")
        lines.append("| Stage | Samples | p50 ms | p95 ms | max ms |")
        lines.append("| --- | --- | --- | --- | --- |")
        for stage, row in summary["stages"].items():
            lines.append(f"| {stage} | {row['samples']} | {row['p50']} | {row['p95']} | {row['max']} |")
    if summary["first"]:
        lines.append("")
        lines.append(f"Window: {summary['first']} to {summary['last']}.")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("paths", nargs="+")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--since", help="ignore attempts started before this ISO 8601 instant")
    parser.add_argument("--kind", choices=["cold_open", "reconnect_cycle"])
    parser.add_argument("--label", default="Phone")
    args = parser.parse_args()
    since = None
    if args.since:
        since = datetime.fromisoformat(args.since.replace("Z", "+00:00"))
        if since.tzinfo is None:
            since = since.replace(tzinfo=timezone.utc)
    attempts = load(args.paths, since)
    kinds = [args.kind] if args.kind else sorted({a.get("kind", "unknown") for a in attempts})
    result = {kind: summarise([a for a in attempts if a.get("kind") == kind]) for kind in kinds}
    if args.json:
        print(json.dumps(result, indent=1, sort_keys=True))
        return 0
    if not attempts:
        print("no attempts found", file=sys.stderr)
        return 1
    print("\n\n".join(markdown(args.label, kind, summary) for kind, summary in result.items()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
