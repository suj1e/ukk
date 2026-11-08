#!/usr/bin/env python3
"""Check repository documentation structure and relative links.

No third-party dependencies. Fails on missing required pages, AGENTS.md over
60 lines, or a broken relative Markdown link in a tracked .md file.
"""

from __future__ import annotations

import re
import subprocess
import sys
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REQUIRED = [
    "AGENTS.md",
    "LESSONS.md",
    "REVIEW.md",
    "README.md",
    "CONTRIBUTING.md",
    "DEVELOPMENT.md",
    "justfile",
    "LICENSE",
    ".github/workflows/ci.yml",
    ".github/workflows/release.yml",
    ".github/PULL_REQUEST_TEMPLATE.md",
    ".github/ISSUE_TEMPLATE/bug.yml",
    ".github/ISSUE_TEMPLATE/feature.yml",
    "docs/requirements.md",
    "docs/tech-design.md",
    "docs/coding-standard.md",
    "docs/roadmap.md",
    "docs/release.md",
    "docs/status.md",
    "docs/decisions/0001-platform-and-runtime.md",
    "docs/decisions/0002-remote-control-is-plugin.md",
    "docs/subsystems/domain-model.md",
    "docs/subsystems/security.md",
    "docs/runbooks/host-lifecycle.md",
    "docs/runbooks/release.md",
    "docs/runbooks/incident.md",
    "docs/spikes/runtime-spike.md",
]
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+['\"][^'\"]*['\"])?\)")
FENCE_RE = re.compile(r"```.*?```", re.S)


def tracked_markdown() -> list[Path]:
    output = subprocess.check_output(
        ["git", "ls-files", "*.md"], cwd=ROOT, text=True
    )
    return [ROOT / line for line in output.splitlines() if line]


def main() -> int:
    failures: list[str] = []
    for relative in REQUIRED:
        if not (ROOT / relative).exists():
            failures.append(f"missing required path: {relative}")

    agents_lines = len((ROOT / "AGENTS.md").read_text(encoding="utf-8").splitlines())
    if agents_lines > 60:
        failures.append(f"AGENTS.md is {agents_lines} lines; maximum is 60")

    checked = 0
    for page in tracked_markdown():
        text = FENCE_RE.sub("", page.read_text(encoding="utf-8"))
        for raw in LINK_RE.findall(text):
            if raw.startswith(("#", "/", "http://", "https://", "mailto:")):
                continue
            target = urllib.parse.unquote(raw.split("#", 1)[0].split("?", 1)[0])
            if not target:
                continue
            resolved = (page.parent / target).resolve()
            try:
                resolved.relative_to(ROOT)
            except ValueError:
                failures.append(f"{page.relative_to(ROOT)}: link escapes repository: {raw}")
                continue
            checked += 1
            if not resolved.exists():
                failures.append(f"{page.relative_to(ROOT)}: broken link: {raw}")
            elif resolved.is_dir() and not any(resolved.iterdir()):
                failures.append(f"{page.relative_to(ROOT)}: empty link target: {raw}")

    if failures:
        print("docs check failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(f"docs check OK: {len(REQUIRED)} required paths, {checked} relative links, AGENTS.md {agents_lines} lines")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
