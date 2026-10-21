#!/usr/bin/env python3
"""Training-only PTY input driver; runs the same instrumented editor.

The parent training process owns the sandbox and profile directory. This
driver adds no profiling flags and uses no held-out benchmark fixtures.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import termios
import time


def key_script():
    steps = []
    for _ in range(4):
        steps.extend(("self-insert-command", bytes([char])) for char in b"local42")
        steps.extend(("delete-backward-char", b"\x7f") for _ in b"local42")
        steps.extend([
            ("next-line", b"\x0e"), ("next-line", b"\x0e"),
            ("forward-word", b"\x1bf"), ("backward-word", b"\x1bb"),
            ("previous-line", b"\x10"), ("previous-line", b"\x10"),
            ("move-end-of-line", b"\x05"),
            ("move-beginning-of-line", b"\x01"),
            ("scroll-up-command", b"\x16"),
            ("scroll-down-command", b"\x1bv"),
            ("nm-pgo--tty-search", b"\x03s"),
            ("nm-pgo--tty-switch", b"\x03b"),
        ])
    return steps


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--editor", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=180)
    args = parser.parse_args()
    editor = args.editor.resolve(strict=True)
    fixture = args.fixture.resolve(strict=True)
    if not os.access(editor, os.X_OK) or not 0 < args.timeout <= 300:
        parser.error("executable editor and timeout in (0, 300] required")
    args.output.mkdir(parents=True, exist_ok=False)
    output = args.output.resolve()
    steps = key_script()
    argv = [str(editor), "-nw", "-Q", "-l", str(fixture)]
    child_env = dict(os.environ, TERM="xterm-256color",
                     NEOMACS_PGO_TTY="1", NEOMACS_PGO_TRAIN_DIR=str(output))
    record = {"argv": argv, "fixture": str(fixture),
              "expected_commands": len(steps), "status": "starting",
              "profile_file": child_env.get("LLVM_PROFILE_FILE"),
              "scope": "training only; parent owns the sandbox",
              "commands": []}
    pid, fd = pty.fork()
    if pid == 0:
        os.execve(editor, argv, child_env)
    record["child_pid"] = pid
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 104, 0, 0))
    deadline = time.monotonic() + args.timeout
    stream = bytearray()
    pending = bytearray()
    marker = re.compile(rb"\x1b\]777;NMPGO;([^\x07]*)\x07")
    child_status = None
    sent = 0
    ready = False
    error = None
    try:
        while time.monotonic() < deadline:
            readable, _, _ = select.select([fd], [], [], 0.1)
            if readable:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    chunk = b""
                stream.extend(chunk)
                pending.extend(chunk)
                if len(stream) > 8 * 1024 * 1024:
                    raise RuntimeError("training terminal output exceeded 8 MiB")
                while True:
                    match = marker.search(pending)
                    if not match:
                        if len(pending) > 65536:
                            del pending[:-65536]
                        break
                    message = match[1].decode("utf-8", errors="strict")
                    del pending[:match.end()]
                    if message == "READY":
                        if ready:
                            raise RuntimeError("duplicate READY")
                        ready = True
                    elif message.startswith("ACK;"):
                        fields = message.split(";")
                        if len(fields) != 3 or int(fields[1]) != sent:
                            raise RuntimeError("unexpected command sequence: " + message)
                        expected = steps[sent - 1][0]
                        if fields[2] != expected:
                            raise RuntimeError("expected " + expected + ", got " + message)
                        record["commands"].append({"sequence": sent, "command": fields[2]})
                    else:
                        raise RuntimeError("fixture failed: " + message)
                    if sent < len(steps):
                        os.write(fd, steps[sent][1])
                        sent += 1
                    else:
                        os.write(fd, b"\x03q")
            done, status = os.waitpid(pid, os.WNOHANG)
            if done:
                child_status = status
                break
        if child_status is None:
            raise RuntimeError("training PTY timeout")
        record["returncode"] = os.waitstatus_to_exitcode(child_status)
        result = json.loads((output / "tty-result.json").read_text())
        if (record["returncode"] != 0 or not ready
                or len(record["commands"]) != len(steps)
                or result.get("commands") != len(steps)
                or result.get("status") != "complete"
                or result.get("buffers_restored") is not True):
            raise RuntimeError("incomplete or incorrect training session")
        record["result"] = result
        record["status"] = "complete"
    except Exception as exc:
        error = str(exc)
        record["status"] = "failed"
        record["error"] = error
    finally:
        if child_status is None:
            # This is only the private PTY child/process group created above.
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, child_status = os.waitpid(pid, 0)
        os.close(fd)
        (output / "terminal.ansi").write_bytes(stream)
        (output / "driver-result.json").write_text(json.dumps(record, indent=2) + "\n")
    if error:
        raise SystemExit("PGO TTY training failed: " + error)
    print("pgo-tty-train: complete", len(steps), "commands")


if __name__ == "__main__":
    main()
