#!/usr/bin/env python3
"""Focused PTY conformance tests for the Latch tmux 3.7b raw-attach patch.

Set LATCH_TMUX_PHASE0_BIN to a tmux 3.7b binary built with
patches/tmux/0001-latch-exclusive-raw-attach.patch. These tests deliberately
exercise the kernel primitive directly; they do not use Latch's Rust attach
path, which is owned by a later implementation phase.
"""

from __future__ import annotations

import base64
import fcntl
import hashlib
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tty
import unittest
from pathlib import Path


TMUX = os.environ.get("LATCH_TMUX_PHASE0_BIN")
TIMEOUT = 8.0


def wait_until(predicate, message: str, timeout: float = TIMEOUT) -> None:
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError(message)
        time.sleep(0.02)


class Surface:
    def __init__(
        self,
        process: subprocess.Popen[bytes],
        master: int,
        check_fd: int,
        original_termios: list[object],
    ):
        self.process = process
        self.master = master
        self.check_fd = check_fd
        self.original_termios = original_termios
        self.output = bytearray()

    def read_for(self, seconds: float) -> bytes:
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            readable, _, _ = select.select([self.master], [], [], 0.02)
            if not readable:
                continue
            try:
                chunk = os.read(self.master, 65536)
            except (BlockingIOError, OSError):
                break
            if not chunk:
                break
            self.output.extend(chunk)
        return bytes(self.output)

    def read_until(self, marker: bytes, timeout: float = TIMEOUT) -> bytes:
        deadline = time.monotonic() + timeout
        while marker not in self.output and time.monotonic() < deadline:
            self.read_for(0.05)
        if marker not in self.output:
            raise AssertionError(
                f"surface never emitted {marker!r}; tail={bytes(self.output[-300:])!r}"
            )
        return bytes(self.output)

    def write(self, data: bytes) -> None:
        view = memoryview(data)
        while view:
            try:
                written = os.write(self.master, view)
                view = view[written:]
            except BlockingIOError:
                select.select([], [self.master], [], 0.05)

    def close(self) -> None:
        for fd in (self.master, self.check_fd):
            try:
                os.close(fd)
            except OSError:
                pass


class Kernel:
    def __init__(self, test: unittest.TestCase):
        if not TMUX:
            test.skipTest("set LATCH_TMUX_PHASE0_BIN to the patched tmux binary")
        self.test = test
        self.temp = tempfile.TemporaryDirectory(prefix="latch-tmux-phase0-")
        self.root = Path(self.temp.name)
        self.socket = self.root / "socket"
        self.env = os.environ.copy()
        self.env["TERM"] = "xterm-256color"
        self.surfaces: list[Surface] = []

    def command(self, *arguments: str, check: bool = True) -> subprocess.CompletedProcess[bytes]:
        result = subprocess.run(
            [TMUX, "-S", str(self.socket), *arguments],
            env=self.env,
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if check and result.returncode != 0:
            raise AssertionError(
                f"tmux command failed: {arguments!r}\n"
                f"stdout={result.stdout!r}\nstderr={result.stderr!r}"
            )
        return result

    def create(self, code: str, name: str = "phase0") -> None:
        encoded = base64.b64encode(code.encode()).decode()
        command = f'''python3 -c "import base64;exec(base64.b64decode('{encoded}'))"'''
        self.command(
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-x",
            "80",
            "-y",
            "24",
            "-s",
            name,
            command,
        )

    def attach(self, name: str = "phase0", cols: int = 80, rows: int = 24) -> Surface:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        check_fd = os.dup(slave)
        original_termios = termios.tcgetattr(check_fd)
        os.set_blocking(master, False)
        process = subprocess.Popen(
            [TMUX, "-S", str(self.socket), "-R", "attach-session", "-t", name],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env=self.env,
            cwd=self.root,
            close_fds=True,
        )
        os.close(slave)
        surface = Surface(process, master, check_fd, original_termios)
        self.surfaces.append(surface)
        return surface

    def stop(self) -> None:
        self.command("kill-server", check=False)
        for surface in self.surfaces:
            if surface.process.poll() is None:
                surface.process.terminate()
                try:
                    surface.process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    surface.process.kill()
            surface.close()
        self.temp.cleanup()


class RawAttachConformance(unittest.TestCase):
    def setUp(self) -> None:
        self.kernel = Kernel(self)

    def tearDown(self) -> None:
        self.kernel.stop()

    def test_snapshot_raw_boundary_byte_identity_input_and_query_ownership(self) -> None:
        headless_done = self.kernel.root / "headless-query-done"
        output = bytes(range(256)) * 8 + b"\x1b[31" + b"mSPLIT\x1b[0m"
        terminal_input = bytes(reversed(range(256))) * 4
        encoded_output = base64.b64encode(output).decode()
        code = (
            "import base64,fcntl,os,time,tty; tty.setraw(0); "
            "os.write(1,bytes([27])+b'[c'); "
            "head=os.read(0,7); open(%r,'wb').write(head); "
            "os.write(1,b'HEADLESS='+head.hex().encode()+b'\\nPROMPT'); "
            "os.read(0,1); data=base64.b64decode(%r); "
            "os.write(1,b'BEGIN-RAW'); "
            "[os.write(1,data[i:i+17]) for i in range(0,len(data),17)]; "
            "os.write(1,b'END-RAW'+bytes([27])+b'[c'); "
            "live=os.read(0,7); os.write(1,b'LIVE='+live.hex().encode()+b'\\nINPUT:'); "
            "received=b''; target=%d; "
            "[(lambda:None)() for _ in ()]; "
            "exec(\"while len(received) < target:\\n received += os.read(0,target-len(received))\"); "
            "os.write(1,b'INPUT-SHA='+__import__('hashlib').sha256(received).hexdigest().encode()); "
            "time.sleep(.3)"
        ) % (str(headless_done), encoded_output, len(terminal_input))
        self.kernel.create(code)
        wait_until(headless_done.exists, "tmux did not answer the headless DA query")
        surface = self.kernel.attach()
        snapshot = surface.read_until(b"PROMPT")

        self.assertEqual(snapshot.count(b"PROMPT"), 1)
        self.assertIn(b"HEADLESS=1b5b3f313b3263", snapshot)
        for probe in (b"\x1b[c", b"\x1b[>c", b"\x1b[?996n", b"\x1b[18t"):
            self.assertNotIn(probe, snapshot[: snapshot.index(b"PROMPT")])

        surface.write(b"G")
        stream = surface.read_until(b"END-RAW")
        begin = stream.index(b"BEGIN-RAW") + len(b"BEGIN-RAW")
        end = stream.index(b"END-RAW", begin)
        self.assertEqual(stream[begin:end], output)
        self.assertEqual(stream.count(b"BEGIN-RAW"), 1)

        live_query = stream.index(b"\x1b[c", end)
        self.assertGreater(live_query, end)
        surface.write(b"\x1b[?1;2c")
        live = surface.read_until(b"INPUT:")
        self.assertIn(b"LIVE=1b5b3f313b3263", live)
        surface.write(terminal_input)
        final = surface.read_until(b"INPUT-SHA=")
        surface.read_for(0.3)
        digest = hashlib.sha256(terminal_input).hexdigest().encode()
        self.assertIn(b"INPUT-SHA=" + digest, bytes(surface.output))

    def test_failed_preflight_preserves_owner_then_steal_orders_and_resizes(self) -> None:
        code = (
            "import os,time,tty; tty.setraw(0); os.write(1,b'OWNER-READY'); "
            "exec(\"while True:\\n b=os.read(0,1)\\n os.write(1,b'ECHO-'+b)\")"
        )
        self.kernel.create(code)
        owner = self.kernel.attach(cols=90, rows=30)
        owner.read_until(b"OWNER-READY")

        failed = self.kernel.command("-R", "attach-session", "-t", "phase0", check=False)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn(b"latch_raw_kernel_failure", failed.stderr)
        self.assertIsNone(owner.process.poll())
        owner.write(b"A")
        owner.read_until(b"ECHO-A")

        replacement = self.kernel.attach(cols=61, rows=19)
        replacement.read_until(b"OWNER-READY")
        owner.read_for(0.3)
        owner.process.wait(timeout=TIMEOUT)
        owner.read_for(0.2)
        self.assertEqual(owner.process.returncode, 75)
        self.assertIn(b"latch_raw_stolen", owner.output)

        replacement.write(b"B")
        replacement.read_until(b"ECHO-B")
        size = self.kernel.command(
            "display-message", "-p", "-t", "phase0", "#{client_width}x#{client_height}"
        )
        self.assertEqual(size.stdout.strip(), b"61x19")

    def test_terminal_query_fixtures_have_exactly_one_owner(self) -> None:
        headless_results = self.kernel.root / "headless-query-fixtures"
        fixtures = [
            (b"\x1b[c", b"\x1b[?1;2c"),
            (b"\x1b[5n", b"\x1b[0n"),
            (b"\x1b[?1004$p", b"\x1b[?1004;2$y"),
            (b"\x1b[?1006$p", b"\x1b[?1006;2$y"),
            (b"\x1b[?2004$p", b"\x1b[?2004;2$y"),
        ]
        encoded_fixtures = repr(fixtures)
        code = (
            "import os,time,tty; tty.setraw(0); time.sleep(.2); fixtures=%s; "
            "readn=lambda n: exec(\"global got\\ngot=b''\\nwhile len(got)<n: got+=os.read(0,n-len(got))\",globals(),{'n':n}); "
            "head=[]; "
            "exec(\"for query,expected in fixtures:\\n os.write(1,query)\\n readn(len(expected))\\n head.append(got)\"); "
            "open(%r,'wb').write(b'|'.join(value.hex().encode() for value in head)); "
            "os.write(1,bytes([27])+b'[?1004h'+bytes([27])+b'[?1006h'+bytes([27])+b'[?2004h'+bytes([27])+b'[>4;2mQUERY-PROMPT'); "
            "os.read(0,1); live=[]; "
            "exec(\"for query,expected in fixtures:\\n os.write(1,query)\\n readn(len(expected))\\n live.append(got)\"); "
            "os.write(1,b'LIVE-QUERIES='+b'|'.join(value.hex().encode() for value in live)); "
            "time.sleep(.3)"
        ) % (encoded_fixtures, str(headless_results))
        self.kernel.create(code)
        self.kernel.command("set-option", "-g", "extended-keys", "on")
        wait_until(headless_results.exists, "headless query fixtures did not complete")
        expected_hex = b"|".join(reply.hex().encode() for _, reply in fixtures)
        self.assertEqual(headless_results.read_bytes(), expected_hex)

        surface = self.kernel.attach()
        surface.read_until(b"QUERY-PROMPT")
        snapshot = surface.read_until(b"\x1b[>4;2m")
        for enabled_mode in (
            b"\x1b[?1004h",
            b"\x1b[?1006h",
            b"\x1b[?2004h",
            b"\x1b[>4;2m",
        ):
            self.assertIn(enabled_mode, snapshot)
        surface.write(b"G")
        search_from = len(surface.output)
        for query, reply in fixtures:
            deadline = time.monotonic() + TIMEOUT
            while bytes(surface.output).find(query, search_from) == -1:
                if time.monotonic() >= deadline:
                    pane = self.kernel.command("capture-pane", "-p", "-e", "-t", "phase0")
                    self.fail(
                        f"live query was not forwarded: {query!r}; "
                        f"surface_tail={bytes(surface.output[-300:])!r}; "
                        f"pane={pane.stdout!r}"
                    )
                surface.read_for(0.05)
            search_from = bytes(surface.output).find(query, search_from) + len(query)
            surface.write(reply)
        surface.read_until(b"LIVE-QUERIES=" + expected_hex)

    def test_slow_client_is_evicted_without_stopping_the_pane(self) -> None:
        completed = self.kernel.root / "pane-completed"
        code = (
            "import os,tty; tty.setraw(0); os.write(1,b'SLOW-READY'); os.read(0,1); "
            "chunk=b'X'*65536; [os.write(1,chunk) for _ in range(256)]; "
            f"open({str(completed)!r},'wb').write(b'ok'); os.read(0,1)"
        )
        self.kernel.create(code)
        surface = self.kernel.attach()
        surface.read_until(b"SLOW-READY")
        surface.write(b"G")
        # Deliberately stop reading the PTY master.
        surface.process.wait(timeout=30)
        self.assertEqual(surface.process.returncode, 76)
        wait_until(completed.exists, "pane did not progress after slow-client eviction")

    def test_tty_restoration_and_session_exit_reason(self) -> None:
        code = "import os,tty; tty.setraw(0); os.write(1,b'EXIT-READY'); os.read(0,1)"
        self.kernel.create(code)
        surface = self.kernel.attach()
        surface.read_until(b"EXIT-READY")
        surface.write(b"Q")
        surface.process.wait(timeout=TIMEOUT)
        surface.read_for(0.2)
        self.assertEqual(surface.process.returncode, 77)
        self.assertIn(b"latch_raw_session_exit", surface.output)
        expected = list(surface.original_termios)
        # macOS may add EXTPROC while a subprocess owns a PTY. It is not a
        # canonical/raw-mode bit and is orthogonal to tmux's saved settings.
        extproc = 0x20000000 if sys.platform == "darwin" else getattr(termios, "EXTPROC", 0)
        expected[3] &= ~extproc
        wait_until(
            lambda: (
                lambda current: current[:3]
                + [current[3] & ~extproc]
                + current[4:]
            )(list(termios.tcgetattr(surface.check_fd)))
            == expected,
            "surface tty attributes were not restored after session exit",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
