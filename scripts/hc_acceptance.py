#!/usr/bin/env python3
"""Real herdr-core acceptance for the ukk Host on a CI runner.

Exercises the committed read-only Host path against a real `hc` binary:

1. two persistent terminals are created by herdr-core;
2. `ukk-host doctor` and `ukk-host list` see both;
3. `ukk-host watch` observes the snapshot and a live `created` event;
4. stopping the Host viewer does not stop any herdr-core terminal.

The terminal program is the running Python interpreter, so this works on
macOS, Linux and Windows without shell utilities. All terminals created by
this script use the `ukk-ci-` prefix and are cleaned up in a finally block.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time

PREFIX = "ukk-ci-"


def hc_binary() -> str:
    candidate = os.environ.get("HC_BIN") or shutil.which("hc")
    if not candidate:
        raise SystemExit("hc executable not found on PATH (set HC_BIN)")
    return candidate


def run(command: list[str], *, timeout: float = 60.0) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise SystemExit(
            f"command failed ({result.returncode}): {' '.join(command)}\n"
            f"stdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result


def live_terminals(hc: str) -> list[dict]:
    result = run([hc, "ls"])
    payload = json.loads(result.stdout.strip().splitlines()[-1])
    return payload.get("terminals", [])


def cleanup(hc: str) -> None:
    for terminal in live_terminals(hc):
        name = terminal.get("name") or ""
        if name.startswith(PREFIX):
            subprocess.run([hc, "kill", name], capture_output=True, text=True)


def create_terminal(hc: str, name: str, seconds: int = 180) -> None:
    program = [
        hc,
        "run",
        "--bg",
        "--name",
        name,
        "--",
        sys.executable,
        "-c",
        f"import time; time.sleep({seconds})",
    ]
    run(program)


def wait_live(hc: str, names: set[str], timeout: float = 15.0) -> list[dict]:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        terminals = live_terminals(hc)
        visible = {terminal.get("name") for terminal in terminals}
        if names <= visible:
            return terminals
        time.sleep(0.25)
    raise SystemExit(f"terminals did not become live: {sorted(names)}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--host",
        default=os.path.join("target", "debug", "ukk-host" + (".exe" if os.name == "nt" else "")),
    )
    args = parser.parse_args()
    host = pathlib.Path(args.host)
    if not host.is_file():
        raise SystemExit(f"ukk-host binary missing: {host}")

    hc = hc_binary()
    version = run([hc, "--version"]).stdout.strip() or run([hc, "--version"]).stderr.strip()
    print(f"hc: {version}")
    cleanup(hc)

    first, second = PREFIX + "a", PREFIX + "b"
    try:
        create_terminal(hc, first)
        create_terminal(hc, second)
        terminals = wait_live(hc, {first, second})
        assert len(terminals) >= 2, terminals

        doctor = run([str(host), "doctor"]).stdout.strip()
        print(f"doctor: {doctor}")
        assert "2 visible terminals" in doctor, doctor

        listed = json.loads(run([str(host), "list"]).stdout)
        names = {terminal.get("name") for terminal in listed.get("terminals", [])}
        assert {first, second} <= names, names
        print(f"list: {len(listed['terminals'])} terminals, both fixtures present")

        watcher = subprocess.Popen(
            [str(host), "watch"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            time.sleep(2.0)
            third = PREFIX + "c"
            create_terminal(hc, third)
            wait_live(hc, {third})
            time.sleep(2.0)
        finally:
            watcher.terminate()
            try:
                stdout, stderr = watcher.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                watcher.kill()
                stdout, stderr = watcher.communicate()

        assert '"kind": "snapshot"' in stdout or '"kind":"snapshot"' in stdout, stdout
        assert '"type": "created"' in stdout or '"type":"created"' in stdout, stdout
        assert third in stdout, stdout
        print(f"watch: snapshot and created event observed ({len(stdout.splitlines())} lines)")
        if stderr.strip():
            print(f"watch stderr: {stderr.strip()}")

        surviving = {terminal.get("name") for terminal in live_terminals(hc)}
        assert {first, second} <= surviving, surviving
        print("survival: terminals remained live after the Host viewer stopped")
    finally:
        cleanup(hc)

    print("REAL HC ACCEPTANCE PASSED")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
