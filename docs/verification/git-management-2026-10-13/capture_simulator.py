#!/usr/bin/env python3
"""Capture the temporary harness against live_gateway.py --keep.

python3 capture_simulator.py /tmp/latch-git-verification-*/runtime.json <simulator-uuid>
The installed app must include simulator_harness.swift.txt (never ship it).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

runtime = json.loads(Path(sys.argv[1]).read_text())
device = sys.argv[2]
out = Path(__file__).resolve().parent
screens = sys.argv[3:] or ["repository", "repositories", "diff", "commit", "branches", "sync", "push", "conflicts", "stale", "rejected", "refresh-required"]


def sim(*args, env=None):
    return subprocess.run(["xcrun", "simctl", *args], env=env, capture_output=True, text=True, check=True)


for appearance, size, suffix in [("light", "large", "light"), ("dark", "accessibility-extra-extra-extra-large", "dark-ax5")]:
    sim("ui", device, "appearance", appearance)
    sim("ui", device, "content_size", size)
    for screen in screens:
        env = dict(os.environ, SIMCTL_CHILD_LATCH_VERIFY_URL=runtime["url"],
                   SIMCTL_CHILD_LATCH_VERIFY_TOKEN=runtime["token"],
                   SIMCTL_CHILD_LATCH_VERIFY_PATH=runtime["conflict"] if screen == "conflicts" else runtime["repo"],
                   SIMCTL_CHILD_LATCH_VERIFY_SCREEN=screen)
        sim("launch", "--terminate-running-process", device, "dev.cooperativ.latch.mobile", env=env)
        time.sleep(3)
        sim("io", device, "screenshot", str(out / f"{screen}-{suffix}.png"))
        print(f"captured {screen}-{suffix}", flush=True)
