#!/usr/bin/env python3
"""Check documentation structure, required pages and relative links.

Paths come from ``git ls-files --cached --others --exclude-standard`` so new
Markdown is checked locally before it is committed and deleted paths are
ignored. Required pages cover the post-reset layout plus the desktop config.
Standard library only.
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
    "LICENSE",
    "README.md",
    "CONTRIBUTING.md",
    "DEVELOPMENT.md",
    "REVIEW.md",
    "justfile",
    ".github/PULL_REQUEST_TEMPLATE.md",
    ".github/ISSUE_TEMPLATE/bug.yml",
    ".github/ISSUE_TEMPLATE/feature.yml",
    ".github/workflows/ci.yml",
    ".github/workflows/ios.yml",
    "apps/desktop/README.md",
    "apps/desktop/go.mod",
    "apps/desktop/go.sum",
    "apps/desktop/mygo.json",
    "contracts/README.md",
    "contracts/agent/v1/contract.md",
    "contracts/bridge/v1/contract.md",
    "contracts/core/v1/contract.md",
    "contracts/remote/v1/contract.md",
    "contracts/world/v1/contract.md",
    "docs/coding-standard.md",
    "docs/decisions/0001-platform-and-runtime.md",
    "docs/decisions/0002-remote-control-is-plugin.md",
    "docs/goals/mygo-implementation.md",
    "docs/goals/v1-complete.md",
    "docs/references.md",
    "docs/release.md",
    "docs/requirements.md",
    "docs/roadmap.md",
    "docs/runbooks/incident.md",
    "docs/runbooks/release.md",
    "docs/status.md",
    "docs/subsystems/domain-model.md",
    "docs/subsystems/plugins.md",
    "docs/subsystems/security.md",
    "docs/tech-design.md",
]

LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)\s]+)(?:\s+['\"][^'\"]*['\"])?\)")
FENCE_RE = re.compile(r"```.*?```", re.S)
SCHEME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*:")


def tracked_markdown() -> list[Path]:
    output = subprocess.check_output(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "*.md"],
        cwd=ROOT,
    )
    pages: list[Path] = []
    for raw in output.split(b"\0"):
        if not raw:
            continue
        relative = raw.decode("utf-8", "surrogateescape")
        page = ROOT / relative
        if page.is_file():
            pages.append(page)
    return pages


def main() -> int:
    failures: list[str] = []
    for relative in REQUIRED:
        if not (ROOT / relative).exists():
            failures.append(f"missing required path: {relative}")

    agents = ROOT / "AGENTS.md"
    agents_lines = 0
    if agents.is_file():
        agents_lines = len(agents.read_text(encoding="utf-8").splitlines())
        if agents_lines > 60:
            failures.append(f"AGENTS.md is {agents_lines} lines; maximum is 60")

    checked = 0
    pages = tracked_markdown()
    for page in pages:
        text = FENCE_RE.sub("", page.read_text(encoding="utf-8"))
        for raw in LINK_RE.findall(text):
            if raw.startswith("#") or raw.startswith("/") or SCHEME_RE.match(raw):
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
    print(
        f"docs check OK: {len(REQUIRED)} required paths, "
        f"{len(pages)} markdown pages, {checked} relative links, "
        f"AGENTS.md {agents_lines} lines"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
