#!/usr/bin/env python3
"""Soak the exclusive-attach kernel against agent-shaped and redraw workloads.

The conformance suites answer "is each invariant correct once". This answers
"does it stay correct for a long time, under the shapes a real session has":
a full-screen redraw at desk geometry, an agent that streams and then blocks on
a prompt for a long idle, and surfaces stealing each other back and forth.

Usage:
  scripts/soak-exclusive-attach.py --tmux <patched-kernel> --latch <latch>
                                   [--minutes 20] [--steals 1000]

Reports byte identity, queue high-water, kernel RSS drift, steal and eviction
counts, and any pane that stopped making progress. Exits non-zero on any
violation, so it can be a release gate rather than a thing somebody reads.
"""

from __future__ import annotations

import argparse
import json
import os
import pty
import random
import shutil
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

# A session's home must be short: the kernel's socket path is a sockaddr_un,
# and a temp directory under a long path overflows it.
HOME_ROOT = Path("/tmp")


class Session:
    def __init__(self, latch: str, tmux: str, home: Path, shell: str, cols=272, rows=59):
        self.latch = latch
        self.tmux = tmux
        self.home = home
        self.env = dict(os.environ, LATCH_HOME=str(home), LATCH_TMUX_BIN=tmux)
        self.env.pop("LATCH_SESSION_ID", None)
        self.env.pop("TMUX", None)
        manifest = {
            "format_version": 1,
            "launch": {
                "argv": ["/bin/sh", "-c", shell],
                "cwd": "/tmp",
                "env": {},
                "inherit_env": True,
                "size": {"cols": cols, "rows": rows},
            },
            "display": {"name": "soak", "source": {"kind": "test"}},
        }
        done = subprocess.run(
            [latch, "create", "--manifest-file", "-", "--json"],
            input=json.dumps(manifest),
            capture_output=True,
            text=True,
            env=self.env,
        )
        if done.returncode != 0:
            raise SystemExit(f"create failed: {done.stdout}{done.stderr}")
        self.id = json.loads(done.stdout)["session"]["id"]

    def visible(self) -> str:
        done = subprocess.run(
            [self.tmux, "-S", str(self.home / "server"), "capture-pane", "-p", "-t", self.id],
            capture_output=True,
            text=True,
            timeout=15,
        )
        return done.stdout

    def server_rss_kib(self) -> int:
        done = subprocess.run(["/bin/ps", "-Ao", "rss=,command="], capture_output=True, text=True)
        socket = str(self.home / "server")
        sizes = [
            int(line.split()[0])
            for line in done.stdout.splitlines()
            if socket in line and "new-session" in line
        ]
        return max(sizes, default=0)

    def remove(self) -> None:
        subprocess.run(
            [self.latch, "remove", self.id, "--force"],
            capture_output=True,
            env=self.env,
            timeout=30,
        )


class Surface:
    """A `latch attach` on its own pty, drained by a reader thread.

    The thread matters. A terminal emulator reads continuously, and a soak
    client that polls its pty every couple of seconds is not a slow client in
    any interesting sense -- it is simply not a terminal. Under a full-screen
    redraw it fills the pty buffer in milliseconds and the kernel evicts it,
    correctly, for something the real surface would never do.
    """

    def __init__(self, session: Session, cols=272, rows=59, read: bool = True):
        self.master, slave = pty.openpty()
        _set_winsize(self.master, cols, rows)
        self.process = subprocess.Popen(
            [session.latch, "attach", session.id],
            stdin=slave,
            stdout=slave,
            stderr=subprocess.PIPE,
            preexec_fn=os.setsid,
            env=dict(session.env, TERM="xterm-256color"),
        )
        os.close(slave)
        os.set_blocking(self.master, False)
        self.read = read
        self.output = bytearray()
        self.received = 0
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._reader = None
        if read:
            self._reader = threading.Thread(target=self._drain, daemon=True)
            self._reader.start()

    def _drain(self) -> None:
        while not self._stop.is_set():
            try:
                readable, _, _ = select.select([self.master], [], [], 0.05)
            except (OSError, ValueError):
                return
            if not readable:
                continue
            try:
                chunk = os.read(self.master, 1 << 16)
            except BlockingIOError:
                continue
            except OSError:
                return
            if not chunk:
                return
            with self._lock:
                self.received += len(chunk)
                # Keep only a recent window: a long redraw soak delivers
                # gigabytes, and holding all of it would measure this script's
                # memory rather than the kernel's.
                self.output.extend(chunk)
                if len(self.output) > 1 << 20:
                    del self.output[: len(self.output) - (1 << 19)]

    def pump(self) -> bytes:
        with self._lock:
            return bytes(self.output)

    def delivered(self) -> int:
        with self._lock:
            return self.received

    def wait_for(self, needle: bytes, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if needle in self.pump():
                return True
            time.sleep(0.01)
        return False

    def type_bytes(self, data: bytes) -> None:
        try:
            os.set_blocking(self.master, True)
            os.write(self.master, data)
        except OSError:
            pass
        finally:
            try:
                os.set_blocking(self.master, False)
            except OSError:
                pass

    def release(self, timeout: float):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.pump()
            code = self.process.poll()
            if code is not None:
                return code
            time.sleep(0.01)
        return None

    def close(self) -> None:
        self._stop.set()
        if self._reader is not None:
            self._reader.join(timeout=2)
        try:
            self.process.kill()
            self.process.wait(timeout=10)
        except Exception:
            pass
        try:
            os.close(self.master)
        except OSError:
            pass


def _set_winsize(fd: int, cols: int, rows: int) -> None:
    import fcntl
    import struct
    import termios

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def fresh_home() -> Path:
    home = Path(tempfile.mkdtemp(prefix="soak", dir=HOME_ROOT))
    return home


class Report:
    def __init__(self) -> None:
        self.violations: list[str] = []
        self.notes: dict[str, object] = {}

    def check(self, condition: bool, message: str) -> None:
        if not condition:
            self.violations.append(message)

    def note(self, key: str, value: object) -> None:
        self.notes[key] = value
        print(f"  {key}: {value}", flush=True)


# One redraw iteration: a clear-and-home, then 59 lines of "line NNN " plus 60
# `x` characters, each ended with CRLF. Counted rather than measured so the
# soak can state amplification without a second capture path to get wrong.
REDRAW_BYTES_PER_ITERATION = len("\033[2J\033[H") + 59 * (len("line 001 ") + 60 + 2)


def bytes_written(iterations: int) -> int:
    return iterations * REDRAW_BYTES_PER_ITERATION


def soak_redraw(latch: str, tmux: str, minutes: float, report: Report) -> None:
    """Full-screen redraw at desk geometry, repeated for the duration.

    The pane repaints its whole screen in a loop and stamps a counter, so a
    stalled pane and a corrupted frame are both detectable afterwards.
    """
    print(f"redraw soak at 272x59 for {minutes:g} min", flush=True)
    home = fresh_home()
    counter = home / "ticks"
    shell = (
        "stty raw -echo; i=0; "
        "while :; do i=$((i+1)); "
        "printf '\\033[2J\\033[H'; "
        "j=0; while [ $j -lt 59 ]; do j=$((j+1)); "
        "printf 'line %03d %s\\r\\n' $j "
        "'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done; "
        f"echo $i > {counter}; done"
    )
    session = Session(latch, tmux, home, shell)
    try:
        surface = Surface(session, 272, 59)
        try:
            if not surface.wait_for(b"line 001", 30):
                report.check(False, "redraw soak: the first frame never painted")
                return
            # Scrollback saturates to the pane's history limit within the
            # first few seconds of a full-screen redraw, so a sample taken at
            # the first frame measures warm-up, not drift. Let it settle first;
            # what matters afterwards is that the plateau holds.
            settle = min(20.0, max(5.0, minutes * 60 * 0.1))
            time.sleep(settle)
            warm_rss = session.server_rss_kib()
            deadline = time.monotonic() + minutes * 60
            last = 0
            stalls = 0
            while time.monotonic() < deadline:
                surface.pump()
                time.sleep(2)
                try:
                    now = int(counter.read_text().strip() or 0)
                except (OSError, ValueError):
                    now = last
                if now == last:
                    stalls += 1
                last = now
            end_rss = session.server_rss_kib()
            report.note("redraw_iterations", last)
            report.note("redraw_bytes_to_surface", surface.delivered())
            report.note("redraw_rss_warm_kib", warm_rss)
            report.note("redraw_rss_end_kib", end_rss)
            report.note("redraw_stall_samples", stalls)
            report.check(last > 0, "redraw soak: the pane never advanced")
            report.check(stalls == 0, f"redraw soak: pane stalled in {stalls} samples")
            # Past the plateau, any sustained growth is the thing to catch.
            report.check(
                end_rss - warm_rss < 4 * 1024,
                f"redraw soak: kernel RSS grew {end_rss - warm_rss}KiB after warmup",
            )
            amplification = surface.delivered() / max(1, bytes_written(last))
            report.note("redraw_amplification", round(amplification, 4))
            report.check(
                amplification < 1.02,
                f"redraw soak: delivered {amplification:.4f}x the bytes the pane wrote",
            )
            report.check(
                surface.process.poll() is None,
                "redraw soak: the surface was released while it was reading",
            )
        finally:
            surface.close()
    finally:
        session.remove()
        shutil.rmtree(home, ignore_errors=True)


def soak_agent_idle(latch: str, tmux: str, minutes: float, report: Report) -> None:
    """An agent that streams, then blocks on a prompt through a long idle.

    This is the case the product exists for: the prompt has to still be on the
    stealing surface after the pane has written nothing for a long time.
    """
    print(f"agent/idle-prompt soak for {minutes:g} min", flush=True)
    home = fresh_home()
    shell = (
        "stty raw -echo; "
        "i=0; while [ $i -lt 200 ]; do i=$((i+1)); "
        "printf 'tool call %d: reading src/lib.rs\\r\\n' $i; done; "
        "printf 'Do you trust the files in this folder? [y/n] '; "
        "while :; do sleep 3600; done"
    )
    session = Session(latch, tmux, home, shell)
    try:
        deadline = time.monotonic() + minutes * 60
        rounds = 0
        while time.monotonic() < deadline:
            rounds += 1
            surface = Surface(session, 100, 30)
            try:
                painted = surface.wait_for(b"Do you trust the files", 30)
                report.check(
                    painted,
                    f"idle soak: round {rounds} did not repaint the blocked prompt",
                )
                if not painted:
                    return
                # Quiet-pane check: a blocked agent must cost nothing to hold.
                #
                # `wait_for` returns on the prompt text, which is in the middle
                # of the frame -- its epilogue (cursor, modes, scroll region)
                # is still in flight. Waiting for the byte count to stop moving
                # first is what makes the next reading about live paint rather
                # than about where the frame happened to be cut.
                settled = _quiesce(surface)
                time.sleep(3)
                after = surface.delivered()
                report.check(
                    after == settled,
                    f"idle soak: a silent pane produced {after - settled} live paint bytes "
                    f"in round {rounds}",
                )
            finally:
                surface.close()
            # Idle with no surface at all, the way a closed laptop leaves it.
            time.sleep(min(20, max(2, (deadline - time.monotonic()) / 4)))
        report.note("idle_prompt_rounds", rounds)
    finally:
        session.remove()
        shutil.rmtree(home, ignore_errors=True)


def _quiesce(surface: Surface, quiet: float = 1.0, limit: float = 15.0) -> int:
    """Waits until the surface has stopped receiving, and returns the total."""
    deadline = time.monotonic() + limit
    last = surface.delivered()
    stable_since = time.monotonic()
    while time.monotonic() < deadline:
        time.sleep(0.1)
        now = surface.delivered()
        if now != last:
            last = now
            stable_since = time.monotonic()
        elif time.monotonic() - stable_since >= quiet:
            return now
    return surface.delivered()


def soak_steals(latch: str, tmux: str, steals: int, report: Report) -> None:
    """Alternating desk/phone steals, checking ordering every time."""
    print(f"{steals} alternating desk/phone steals", flush=True)
    home = fresh_home()
    shell = (
        "stty raw -echo; "
        "printf 'Do you trust the files in this folder? [y/n] '; "
        "while :; do sleep 3600; done"
    )
    session = Session(latch, tmux, home, shell)
    stolen = 0
    unreasoned: list[int] = []
    try:
        current = Surface(session, 272, 59)
        if not current.wait_for(b"Do you trust", 30):
            report.check(False, "steal soak: the first surface never painted")
            return
        warm_rss = session.server_rss_kib()
        for round_index in range(steals):
            desk = round_index % 2 == 0
            cols, rows = (60, 20) if desk else (272, 59)
            nxt = Surface(session, cols, rows)
            if not nxt.wait_for(b"Do you trust", 30):
                report.check(
                    False, f"steal soak: round {round_index} did not repaint the prompt"
                )
                nxt.close()
                break
            code = current.release(30)
            if code == 75:
                stolen += 1
            else:
                unreasoned.append(code if code is not None else -1)
            current.close()
            current = nxt
        end_rss = session.server_rss_kib()
        current.close()
        report.note("steals_completed", stolen)
        report.note("steals_unreasoned", unreasoned[:10])
        report.note("steal_rss_warm_kib", warm_rss)
        report.note("steal_rss_end_kib", end_rss)
        report.check(
            stolen == steals,
            f"steal soak: {steals - stolen} of {steals} steals did not report `stolen`",
        )
        report.check(
            end_rss - warm_rss < 8 * 1024,
            f"steal soak: kernel RSS grew {end_rss - warm_rss}KiB over {steals} steals",
        )
        report.check(
            "Do you trust" in session.visible(),
            "steal soak: the prompt was lost from the pane",
        )
    finally:
        session.remove()
        shutil.rmtree(home, ignore_errors=True)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tmux", required=True, help="patched latch-tmux")
    parser.add_argument("--latch", required=True, help="latch binary")
    parser.add_argument("--minutes", type=float, default=20.0, help="per workload")
    parser.add_argument("--steals", type=int, default=1000)
    arguments = parser.parse_args()

    tmux = str(Path(arguments.tmux).resolve())
    latch = str(Path(arguments.latch).resolve())
    if subprocess.run([tmux, "-R", "-V"], capture_output=True).returncode != 0:
        print(f"{tmux} does not advertise latch-raw-attach-v1", file=sys.stderr)
        return 2

    random.seed(0)
    report = Report()
    started = time.time()
    soak_redraw(latch, tmux, arguments.minutes, report)
    soak_agent_idle(latch, tmux, arguments.minutes, report)
    soak_steals(latch, tmux, arguments.steals, report)
    report.note("soak_seconds", round(time.time() - started))

    print()
    if report.violations:
        print(f"SOAK FAILED ({len(report.violations)} violations)")
        for violation in report.violations:
            print(f"  - {violation}")
        return 1
    print("SOAK PASSED")
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGPIPE, signal.SIG_DFL)
    sys.exit(main())
