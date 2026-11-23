#!/usr/bin/env python3
# Capture hook: appends the hook's stdin to $CAP_OUT and optionally answers.
import json, os, sys
raw = sys.stdin.read()
try: rec = json.loads(raw)
except Exception: rec = {"unparsed": raw}
with open(os.environ["CAP_OUT"], "a") as f: f.write(json.dumps(rec) + "\n")
ev = rec.get("hook_event_name")
mode = os.environ.get("CAP_MODE", "")
if ev == "Elicitation" and "elicit-accept" in mode:
    if rec.get("mode") == "url":
        out = {"action": "decline"}
    else:
        out = {"action": "accept", "content": {"name": "Latch phone", "count": 3, "color": "blue"}}
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "Elicitation", **out}}))
elif ev == "PermissionRequest" and "perm-allow-suggest" in mode:
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
        "decision": {"behavior": "allow", "updatedPermissions": [dict(u, destination=os.environ.get("CAP_DEST", u.get("destination"))) for u in (rec.get("permission_suggestions") or [])]}}}))
elif ev == "PermissionRequest" and "plan-approve" in mode and rec.get("tool_name") == "ExitPlanMode":
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
        "decision": dict({"behavior": "allow", "updatedInput": rec.get("tool_input")}, **({"updatedPermissions": [{"type": "setMode", "mode": os.environ["CAP_PLAN_MODE"], "destination": "session"}]} if os.environ.get("CAP_PLAN_MODE") else {}))}}))
