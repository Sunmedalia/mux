#!/usr/bin/env python3
"""Isolated native CLI handoff smoke test: no real credentials or API calls."""
import errno
import fcntl
import json
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

mux = str(pathlib.Path(sys.argv[1]).resolve())


def scenario(target, exit_mode, surface="quick"):
    with tempfile.TemporaryDirectory(prefix="mux-session-pty-") as root:
        root = pathlib.Path(root)
        project = root / "project 中文 ' $(literal)"
        project.mkdir()
        home = root / "home"
        logs = home / ".claude/projects/test"
        logs.mkdir(parents=True)
        for directory in [home / ".codex/sessions", home / ".grok/sessions", home / ".pi/agent/sessions"]:
            directory.mkdir(parents=True)
        records = [
            {"type": "user", "sessionId": "original-id", "cwd": str(project), "uuid": "u", "parentUuid": None, "message": {"role": "user", "content": "original task"}},
            {"type": "assistant", "sessionId": "original-id", "cwd": str(project), "uuid": "a", "parentUuid": "u", "timestamp": "2026-10-16T00:00:00Z", "message": {"id": "a", "model": "fixture", "role": "assistant", "content": [{"type": "text", "text": "completed step"}], "usage": {"input_tokens": 1, "output_tokens": 2}}},
            {"type": "custom-title", "sessionId": "original-id", "customTitle": "Fixture session"},
        ]
        (logs / "original-id.jsonl").write_text("".join(json.dumps(r) + "\n" for r in records))
        bindir = root / "bin"
        bindir.mkdir()
        record = root / "child.json"
        helper = '''#!PYTHON
import json, os, pathlib, signal, sys, termios, time
if '--help' in sys.argv:
 if os.environ['CHILD_EXIT']=='launch-failure': pathlib.Path(sys.argv[0]).unlink()
 print('Usage: cli [PROMPT] [messages...] --resume SESSION_ID --session PATH')
 sys.exit(0)
args=sys.argv[1:]
transcripts=list(pathlib.Path(os.environ['XDG_STATE_HOME']).rglob('conversation.md'))
flags=termios.tcgetattr(0)[3]
pathlib.Path(os.environ['CHILD_RECORD']).write_text(json.dumps({'args':args,'cwd':os.getcwd(),'canonical':bool(flags & termios.ICANON),'echo':bool(flags & termios.ECHO),'transcripts':[p.read_text() for p in transcripts]}))
print('FIXTURE_CHILD_STARTED',flush=True)
mode=os.environ['CHILD_EXIT']
if mode=='terminal-interrupt': time.sleep(10)
time.sleep(0.2)
if mode=='interrupt': os.kill(os.getpid(),signal.SIGINT)
sys.exit(7 if mode=='failure' else 0)
'''.replace("PYTHON", sys.executable)
        for cli in ["claude", "pi", "codex", "grok"]:
            path = bindir / cli
            path.write_text(helper)
            path.chmod(0o755)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("HERDR_", "MUX_", "CODEX_", "GROK_", "PI_", "CLAUDE_", "ANTHROPIC_"))}
        env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_STATE_HOME=str(home / ".state"), XDG_CACHE_HOME=str(home / ".cache"), PATH=str(bindir) + os.pathsep + os.environ["PATH"], TERM="xterm-256color", CHILD_RECORD=str(record), CHILD_EXIT=exit_mode)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 36, 120, 0, 0))
        proc = subprocess.Popen([mux] + (["quick"] if surface == "quick" else []), cwd=project, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        output = bytearray()

        def pump(seconds=0.2):
            until = time.monotonic() + seconds
            while time.monotonic() < until:
                ready, _, _ = select.select([master], [], [], min(0.05, max(0, until - time.monotonic())))
                if ready:
                    try:
                        output.extend(os.read(master, 65536))
                    except OSError as e:
                        if e.errno != errno.EIO:
                            raise
                        break

        def wait(predicate, label):
            until = time.monotonic() + 15
            while time.monotonic() < until:
                pump()
                if predicate():
                    return
                if proc.poll() is not None:
                    break
            raise AssertionError(f"{surface}/{target}/{exit_mode}: {label}\n{bytes(output[-10000:])!r}")

        try:
            pump(1)
            if surface == "quick":
                os.write(master, b"s")
            else:
                os.write(master, b"\x1b[17~")  # F6: Usage
                pump(0.4)
                os.write(master, b"g")
            wait(lambda: b"Fixture session" in output, "session not loaded")
            os.write(master, b"a")
            wait(lambda: b"Take over" in output, "menu not opened")
            if target == "codex":
                os.write(master, b"jj")
            os.write(master, b"\r")
            if exit_mode == "launch-failure":
                wait(lambda: b"Could not launch AI CLI" in output, "launch failure not shown")
                assert not record.exists()
                assert proc.poll() is None
                flags = termios.tcgetattr(slave)[3]
                assert not flags & termios.ICANON and not flags & termios.ECHO
                assert not list((home / ".state").rglob("conversation.md"))
                os.write(master, b"\x03")
                wait(lambda: proc.poll() is not None, "Mux did not exit after launch failure")
                print(f"PASS {surface}: {target} {exit_mode}")
                return
            wait(record.exists, "CLI not launched")
            child = json.loads(record.read_text())
            if exit_mode == "terminal-interrupt":
                os.killpg(proc.pid, signal.SIGINT)
            assert os.path.samefile(child["cwd"], project), child
            assert child["canonical"] and child["echo"], child
            if target == "claude":
                assert child["args"] == ["--resume", "original-id"], child
                assert not child["transcripts"], child
            else:
                assert len(child["args"]) == 1, child
                assert "original task" in child["transcripts"][0], child
                assert "completed step" in child["transcripts"][0], child
            pump(1)
            assert proc.poll() is None, "Mux exited with child"
            flags = termios.tcgetattr(slave)[3]
            assert not flags & termios.ICANON and not flags & termios.ECHO, "Mux did not restore raw mode"
            assert not list((home / ".state").rglob("conversation.md")), "handoff file leaked"
            os.write(master, b"\x03")
            wait(lambda: proc.poll() is not None, "Mux did not exit")
            flags = termios.tcgetattr(slave)[3]
            assert flags & termios.ICANON and flags & termios.ECHO, "Mux did not restore terminal on exit"
            assert proc.returncode == 0, proc.returncode
            print(f"PASS {surface}: {target} {exit_mode}")
        finally:
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait()
            os.close(master)
            os.close(slave)


for surface in ["quick", "main"]:
    for target, mode in [("claude", "success"), ("codex", "success"), ("codex", "failure"), ("codex", "interrupt"), ("codex", "terminal-interrupt"), ("codex", "launch-failure")]:
        scenario(target, mode, surface)
