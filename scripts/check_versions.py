#!/usr/bin/env python3
"""Verify that every Cargo package in the two workspaces shares one version."""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def package_version(path: Path) -> str:
    with path.open("rb") as handle:
        data = tomllib.load(handle)
    version = data.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        raise SystemExit(f"{path.relative_to(ROOT)}: missing package.version")
    return version


def workspace_members(manifest: Path) -> list[Path]:
    with manifest.open("rb") as handle:
        data = tomllib.load(handle)
    members = data.get("workspace", {}).get("members", [])
    return [manifest.parent / member / "Cargo.toml" for member in members]


def main() -> int:
    manifests = workspace_members(ROOT / "Cargo.toml")
    manifests.append(ROOT / "apps/desktop/Cargo.toml")
    versions = {path.relative_to(ROOT): package_version(path) for path in manifests}
    unique = sorted(set(versions.values()))
    if len(unique) != 1:
        print("version check failed: package versions differ", file=sys.stderr)
        for path, version in sorted(versions.items()):
            print(f"  {path}: {version}", file=sys.stderr)
        return 1
    print(f"version check OK: {unique[0]} across {len(versions)} packages")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
