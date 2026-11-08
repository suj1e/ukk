#!/usr/bin/env python3
"""Check the desktop MyGo configuration and cross-client versions.

Validates the exact ``apps/desktop/mygo.json`` contract and compares its
version with the actual iOS ``MARKETING_VERSION`` in both ``project.yml`` and
``project.pbxproj``. Standard library only; no YAML dependency.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MYGO_JSON = ROOT / "apps/desktop/mygo.json"
IOS_PROJECT_YML = ROOT / "apps/ios/project.yml"
IOS_PBXPROJ = ROOT / "apps/ios/UkkCompanion.xcodeproj/project.pbxproj"

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")
YAML_VERSION_RE = re.compile(r"(?m)^\s*MARKETING_VERSION:\s*[\"']?([^\"'\s#]+)")
PBX_VERSION_RE = re.compile(r"MARKETING_VERSION\s*=\s*([^;]+);")

EXPECTED = {
    "name": "ukk",
    "slug": "ukk",
    "identifier": "dev.ukk.desktop",
    "main": "./cmd/ukk",
    "icon": "../../assets/icon/ukk-macos-1024.png",
    "out": "../../dist/desktop",
    "license": "MIT",
    "macos.minimumSystemVersion": "13.0",
    "macos.signingIdentity": "-",
    "linux.webview": False,
}


def lookup(data: object, dotted: str) -> object | None:
    value = data
    for part in dotted.split("."):
        if not isinstance(value, dict) or part not in value:
            return None
        value = value[part]
    return value


def normalize(value: object) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    return str(value)


def main() -> int:
    failures: list[str] = []

    if not MYGO_JSON.is_file():
        print(f"version check failed: missing {MYGO_JSON.relative_to(ROOT)}", file=sys.stderr)
        return 1

    try:
        config = json.loads(MYGO_JSON.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        print(f"version check failed: invalid mygo.json: {error}", file=sys.stderr)
        return 1
    if not isinstance(config, dict):
        print("version check failed: mygo.json must be a JSON object", file=sys.stderr)
        return 1

    for key, expected in EXPECTED.items():
        actual = lookup(config, key)
        if actual is None:
            failures.append(f"apps/desktop/mygo.json: missing {key}")
        elif normalize(actual) != normalize(expected):
            failures.append(
                f"apps/desktop/mygo.json: {key} must be {expected!r}, found {actual!r}"
            )

    version = config.get("version")
    if not isinstance(version, str) or not SEMVER_RE.fullmatch(version):
        failures.append(
            f"apps/desktop/mygo.json: version must be semantic (for example 0.1.0), "
            f"found {version!r}"
        )
        version = None

    icon = config.get("icon")
    if isinstance(icon, str):
        icon_path = (MYGO_JSON.parent / icon).resolve()
        if not icon_path.is_file():
            failures.append(f"apps/desktop/mygo.json: icon target is missing: {icon}")

    out = config.get("out")
    if isinstance(out, str):
        out_path = (MYGO_JSON.parent / out).resolve()
        expected_out = (ROOT / "dist/desktop").resolve()
        if out_path != expected_out:
            failures.append(
                f"apps/desktop/mygo.json: out must resolve to dist/desktop, found {out}"
            )

    project_versions: list[str] = []
    if IOS_PROJECT_YML.is_file():
        project_versions = YAML_VERSION_RE.findall(
            IOS_PROJECT_YML.read_text(encoding="utf-8")
        )
        if not project_versions:
            failures.append("apps/ios/project.yml: missing MARKETING_VERSION")
    else:
        failures.append("missing apps/ios/project.yml")

    pbx_versions: list[str] = []
    if IOS_PBXPROJ.is_file():
        pbx_versions = [
            value.strip().strip('"')
            for value in PBX_VERSION_RE.findall(
                IOS_PBXPROJ.read_text(encoding="utf-8")
            )
        ]
        if not pbx_versions:
            failures.append("apps/ios/UkkCompanion.xcodeproj: missing MARKETING_VERSION")
    else:
        failures.append("missing apps/ios/UkkCompanion.xcodeproj/project.pbxproj")

    if version is not None:
        for value in project_versions:
            if value != version:
                failures.append(
                    f"apps/ios/project.yml: MARKETING_VERSION {value} does not match "
                    f"mygo.json version {version}"
                )
        for value in pbx_versions:
            if value != version:
                failures.append(
                    "apps/ios/UkkCompanion.xcodeproj: MARKETING_VERSION "
                    f"{value} does not match mygo.json version {version}"
                )

    if failures:
        print("version check failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print(
        f"version check OK: {version} in mygo.json, ios project.yml and "
        f"project.pbxproj ({len(pbx_versions)} build settings)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
