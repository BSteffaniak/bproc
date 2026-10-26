#!/usr/bin/env python3
"""PTY smoke test for the actual bproc executable, with isolated configuration.

Usage: python3 scripts/smoke-tui.py /absolute/path/to/bproc
Use a --no-default-features build for deterministic unprivileged lifecycle tests.
This never applies throttles or restores saved policies.
"""
import errno
import fcntl
import hashlib
import os
import pathlib
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def run_case(binary, keys, size, resize=None):
    master, slave = pty.openpty()
    before = termios.tcgetattr(slave)
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", *size, 0, 0))
    output = bytearray()
    with tempfile.TemporaryDirectory(prefix="bproc-tui-smoke-") as state:
        env = dict(os.environ, TERM="xterm-256color", HOME=state,
                   XDG_CONFIG_HOME=state, RUST_LOG="off")
        child = subprocess.Popen([str(binary), "--no-restore", "--no-save"],
                                 stdin=slave, stdout=slave, stderr=slave,
                                 env=env, start_new_session=True)
        start = time.monotonic()
        sent = False
        key_events = keys if isinstance(keys, list) else [(1.0, keys)]
        sent_count = 0
        try:
            while time.monotonic() - start < 12:
                if select.select([master], [], [], 0.05)[0]:
                    try:
                        output.extend(os.read(master, 65536))
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        break
                if not sent and time.monotonic() - start >= 1.0:
                    if resize is not None:
                        fcntl.ioctl(slave, termios.TIOCSWINSZ,
                                    struct.pack("HHHH", *resize, 0, 0))
                        os.kill(child.pid, signal.SIGWINCH)
                    sent = True
                if sent_count < len(key_events):
                    deadline, payload = key_events[sent_count]
                    if time.monotonic() - start >= deadline:
                        os.write(master, payload)
                        sent_count += 1
                if child.poll() is not None:
                    break
            if child.poll() is None:
                raise AssertionError("TUI did not exit within 12 seconds")
            assert child.returncode == 0, (child.returncode, output[-2000:])
            assert b"\x1b[?1049h" in output, "alternate screen was not entered"
            assert b"\x1b[?1049l" in output, "alternate screen was not restored"
            after = termios.tcgetattr(slave)
            mask = termios.ICANON | termios.ECHO | termios.ISIG
            assert before[3] & mask == after[3] & mask, "input modes not restored"
            assert not list(pathlib.Path(state).rglob("throttles.json")), "--no-save wrote configuration"
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            os.close(slave)


def main():
    binary = pathlib.Path(sys.argv[1]).resolve(strict=True)
    print(f"artifact: {binary}")
    print(f"sha256: {hashlib.sha256(binary.read_bytes()).hexdigest()}")
    for label, keys, size, resize in [
        ("quit", b"q", (24, 80), None),
        ("modal-and-resize", [(1.1, b"h"), (1.5, b"\x1b"), (1.9, b"q")], (40, 120), (24, 80)),
        ("ctrl-c", b"\x03", (24, 80), None),
        ("tiny-terminal", b"q", (3, 8), None),
    ]:
        run_case(binary, keys, size, resize)
        print(f"PASS {label}")


if __name__ == "__main__":
    main()
