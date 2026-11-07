#!/usr/bin/env python3
"""PTY smoke test for the actual bproc executable, with isolated configuration.

Usage: python3 scripts/smoke-tui.py [--allow-missing-chrome] /absolute/path/to/bproc
Use a --no-default-features build for deterministic unprivileged lifecycle tests.
This never applies throttles or restores saved policies.

Besides lifecycle (alternate screen, restored input modes, clean exit), each
case checks that expected text reached the terminal: the explorer's F-key bar
("F10" / "Quit") and the legacy interface filter modal. Pass
--allow-missing-chrome to only warn when the explorer chrome is absent.
"""
import errno
import fcntl
import hashlib
import os
import pathlib
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


def run_case(binary, keys, size, resize=None, expect=()):
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
            screen = visible_text(bytes(output))
            missing = [text for text in expect if text not in screen]
            assert not missing, f"never painted {missing!r}"
            return screen
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
            os.close(slave)


def visible_text(output):
    """Terminal output with escape sequences removed (cell text runs joined)."""
    text = output.decode("utf-8", "replace")
    return re.sub(r"\x1b(\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(\x07|\x1b\\)|[@-Z\\-_])", "", text)


CHROME = ("F10", "Quit")


def main():
    args = sys.argv[1:]
    allow_missing_chrome = "--allow-missing-chrome" in args
    args = [a for a in args if a != "--allow-missing-chrome"]
    binary = pathlib.Path(args[0]).resolve(strict=True)
    print(f"artifact: {binary}")
    print(f"sha256: {hashlib.sha256(binary.read_bytes()).hexdigest()}")
    for label, keys, size, resize, expect, chrome in [
        ("quit", [(1.5, b"q")], (24, 80), None, (), True),
        # F1 opens the explorer help overlay; Esc closes it before q quits.
        ("modal-and-resize", [(1.1, b"h"), (1.5, b"\x1b"), (1.9, b"q")], (40, 120), (24, 80), (), True),
        # 'i' opens the legacy interface filter over the explorer.
        ("interfaces", [(1.1, b"i"), (1.6, b"\x1b"), (2.0, b"q")], (40, 120), None,
         ("Interface Filter",), False),
        ("ctrl-c", b"\x03", (24, 80), None, (), False),
        ("tiny-terminal", b"q", (3, 8), None, (), False),
    ]:
        screen = run_case(binary, keys, size, resize, expect)
        if chrome:
            missing = [text for text in CHROME if text not in screen]
            if missing and allow_missing_chrome:
                print(f"WARN {label}: explorer chrome {missing!r} not painted")
            else:
                assert not missing, f"{label}: explorer chrome {missing!r} not painted"
        print(f"PASS {label}")


if __name__ == "__main__":
    main()
