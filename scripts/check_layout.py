#!/usr/bin/env python3
"""Check the post-reset repository layout and Go module metadata.

Rules:
- ``apps/`` contains exactly ``desktop`` and ``ios``.
- Go metadata lives only at ``apps/desktop/{go.mod,go.sum,mygo.json}``;
  there is one module, no ``go.work`` and no nested module.
- Tracked Rust/Cargo source, ``crates/`` and top-level ``plugins/`` are gone.
- ``go.mod`` pins the Go/MyGo toolchain and ``go.sum`` covers every require.

Tracked and untracked-but-nonignored paths are inspected, while paths that
exist only in the Git index (deleted from the worktree) are ignored. Standard
library only; this does not build or download Go packages.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent

ALLOWED_APPS = {"desktop", "ios"}
GO_DESKTOP_METADATA = {"go.mod", "go.sum", "mygo.json"}
GO_WORKSPACE_METADATA = {"go.work", "go.work.sum"}
RUST_FILENAMES = {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"}
FORBIDDEN_TOP_LEVEL = {"crates", "plugins"}

MYGO_MODULE = "github.com/egoist/mygo"
MYGO_TOOL = "github.com/egoist/mygo/cmd/mygo"
MYGO_VERSION = "v0.8.3"
MIN_GO_VERSION = (1, 27, 3)

REQUIRE_RE = re.compile(r"^([\w./-]+)\s+(v\S+)$")
SUM_RE = re.compile(r"^(\S+)\s+(\S+)\s+(h1:[A-Za-z0-9+/]+={0,2})$")
GO_VERSION_RE = re.compile(r"^(\d+)\.(\d+)(?:\.(\d+))?$")


def repository_paths() -> list[str]:
    """Return tracked and untracked non-ignored paths that exist on disk."""
    output = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT,
    )
    paths: list[str] = []
    for raw in output.split(b"\0"):
        if not raw:
            continue
        relative = raw.decode("utf-8", "surrogateescape")
        if (ROOT / relative).exists():
            paths.append(relative)
    return paths


def parse_go_version(value: str) -> tuple[int, int, int] | None:
    match = GO_VERSION_RE.fullmatch(value)
    if match is None:
        return None
    return (int(match.group(1)), int(match.group(2)), int(match.group(3) or 0))


def parse_go_mod(text: str) -> tuple[str | None, str | None, list[tuple[str, str]], set[str]]:
    """Parse module, go directive, requires and tool paths from go.mod."""
    module: str | None = None
    go_version: str | None = None
    requires: list[tuple[str, str]] = []
    tools: set[str] = set()

    block: str | None = None
    for raw_line in text.splitlines():
        line = raw_line.split("//", 1)[0].strip()
        if not line:
            continue

        if block is not None:
            if line == ")":
                block = None
                continue
            if block == "require":
                match = REQUIRE_RE.fullmatch(line)
                if match:
                    requires.append((match.group(1), match.group(2)))
            elif block == "tool":
                tools.add(line.split()[0])
            continue

        if line in {"require (", "tool ("}:
            block = line.split()[0]
            continue
        if line.startswith("require "):
            match = REQUIRE_RE.fullmatch(line[len("require "):].strip())
            if match:
                requires.append((match.group(1), match.group(2)))
            continue
        if line.startswith("tool "):
            tools.add(line[len("tool "):].strip().split()[0])
            continue
        if line.startswith("module "):
            module = line[len("module "):].strip()
            continue
        if line.startswith("go "):
            go_version = line[len("go "):].strip()

    return module, go_version, requires, tools


def parse_go_sum(text: str) -> tuple[dict[str, set[str]], list[str]]:
    """Return module -> versions covered by well-formed sum lines."""
    covered: dict[str, set[str]] = {}
    malformed: list[str] = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        match = SUM_RE.fullmatch(line)
        if match is None:
            malformed.append(raw_line)
            continue
        module, version = match.group(1), match.group(2)
        if version.endswith("/go.mod"):
            version = version[: -len("/go.mod")]
        covered.setdefault(module, set()).add(version)
    return covered, malformed


def main() -> int:
    failures: list[str] = []

    paths = repository_paths()
    if not paths:
        failures.append("git ls-files returned no existing repository paths")

    app_entries: set[str] = set()
    go_mods: list[str] = []
    go_sums: list[str] = []
    mygo_configs: list[str] = []

    for relative in paths:
        parts = PurePosixPath(relative).parts
        if not parts:
            continue

        top = parts[0]
        if top == "apps":
            if len(parts) < 2:
                failures.append(f"{relative}: apps/ may contain only desktop/ and ios/")
            else:
                app_entries.add(parts[1])
                if parts[1] not in ALLOWED_APPS:
                    failures.append(f"{relative}: unsupported app directory apps/{parts[1]}")

        if top in FORBIDDEN_TOP_LEVEL:
            failures.append(f"{relative}: top-level {top}/ is not part of the reset layout")
        if ".cargo" in parts:
            failures.append(f"{relative}: Cargo configuration is not part of the reset layout")

        name = parts[-1]
        if name.endswith(".rs") or name in RUST_FILENAMES:
            failures.append(f"{relative}: Rust/Cargo files are not part of the reset layout")
        if name in GO_WORKSPACE_METADATA:
            failures.append(f"{relative}: Go workspace files are not allowed")
        elif name in GO_DESKTOP_METADATA:
            expected = f"apps/desktop/{name}"
            if relative != expected:
                failures.append(f"{relative}: Go metadata belongs only at {expected}")
            elif name == "go.mod":
                go_mods.append(relative)
            elif name == "go.sum":
                go_sums.append(relative)
            else:
                mygo_configs.append(relative)

    for app in sorted(ALLOWED_APPS - app_entries):
        failures.append(f"missing app directory: apps/{app}")
    if len(go_mods) != 1:
        failures.append(f"expected exactly apps/desktop/go.mod, found: {go_mods or 'none'}")
    if len(go_sums) != 1:
        failures.append(f"expected exactly apps/desktop/go.sum, found: {go_sums or 'none'}")
    if len(mygo_configs) != 1:
        failures.append(f"expected exactly apps/desktop/mygo.json, found: {mygo_configs or 'none'}")

    gomod = ROOT / "apps/desktop/go.mod"
    gosum = ROOT / "apps/desktop/go.sum"
    module: str | None = None

    if gomod.is_file():
        text = gomod.read_text(encoding="utf-8")
        module, go_version, requires, tools = parse_go_mod(text)

        if not module:
            failures.append("apps/desktop/go.mod: missing module directive")
        if not go_version:
            failures.append("apps/desktop/go.mod: missing go directive")
        else:
            parsed = parse_go_version(go_version)
            if parsed is None:
                failures.append(f"apps/desktop/go.mod: invalid go directive {go_version!r}")
            elif parsed < MIN_GO_VERSION:
                failures.append(
                    "apps/desktop/go.mod: go "
                    f"{go_version} is older than required "
                    f"{'.'.join(map(str, MIN_GO_VERSION))}"
                )

        pinned = {name: version for name, version in requires}
        if pinned.get(MYGO_MODULE) != MYGO_VERSION:
            failures.append(
                f"apps/desktop/go.mod: require {MYGO_MODULE} {MYGO_VERSION}, "
                f"found {pinned.get(MYGO_MODULE) or 'nothing'}"
            )
        if MYGO_TOOL not in tools:
            failures.append(f"apps/desktop/go.mod: missing tool {MYGO_TOOL}")

        if gosum.is_file():
            covered, malformed = parse_go_sum(gosum.read_text(encoding="utf-8"))
            for line in malformed:
                failures.append(f"apps/desktop/go.sum: malformed line: {line}")
            missing = [
                f"{name} {version}"
                for name, version in requires
                if version not in covered.get(name, set())
            ]
            for entry in missing:
                failures.append(f"apps/desktop/go.sum: missing checksum for {entry}")

    if failures:
        print("layout check failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(
        "layout check OK: apps desktop, ios; "
        f"module {module}; {len(paths)} existing paths"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
