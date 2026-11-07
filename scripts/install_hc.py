#!/usr/bin/env python3
"""Install a pinned, checksum-verified herdr-core release for CI acceptance.

Prints the directory containing the `hc` executable so a workflow can add it
to PATH. The download URL and SHA-256 are pinned; the release is never fetched
from an unverified manifest at test time.
"""
from __future__ import annotations

import hashlib
import os
import pathlib
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

VERSION = "0.1.18"
TARGETS = {
    "darwin-arm64": (
        "https://releases.herdr.dev/core/0.1.18/herdr-core-macos-aarch64.tar.gz",
        "77d8af092e25ae650f4424d6dad27514f6916a11d807fb0752858cd27c21e4fc",
    ),
    "darwin-x86_64": (
        "https://releases.herdr.dev/core/0.1.18/herdr-core-macos-x86_64.tar.gz",
        "15a12b80e93ec4e074dbdb82fa8a66e529857abfbdfb456f94d5116dc438fedc",
    ),
    "linux-arm64": (
        "https://releases.herdr.dev/core/0.1.18/herdr-core-linux-aarch64.tar.gz",
        "cf33393cf950bb763734dc3e16610ab9daf8492391a4ef0de56026cd7f752853",
    ),
    "linux-x86_64": (
        "https://releases.herdr.dev/core/0.1.18/herdr-core-linux-x86_64.tar.gz",
        "5b2f97cb4b2a9d7a6b2cc5942628203ed9fbb7cc0c2b4b09ad8a6f5648e246b7",
    ),
    "win32-x86_64": (
        "https://releases.herdr.dev/core/0.1.18/herdr-core-windows-x86_64.zip",
        "2e54e0b1dcbd652a7e89b14c588178f8241b9732fdc4e5f2b27208829b0bfbbc",
    ),
}


def target_key() -> str:
    system = sys.platform
    if system.startswith("win"):
        system = "win32"
    machine = platform.machine().lower()
    if machine in ("arm64", "aarch64"):
        machine = "arm64"
    elif machine in ("x86_64", "amd64"):
        machine = "x86_64"
    return f"{system}-{machine}"


def main() -> int:
    key = target_key()
    if key not in TARGETS:
        print(f"unsupported CI platform for hc: {key}", file=sys.stderr)
        return 1
    url, expected = TARGETS[key]

    work = pathlib.Path(tempfile.mkdtemp(prefix="ukk-hc-install-"))
    archive = work / pathlib.Path(url).name
    print(f"downloading herdr-core {VERSION} from {url}", file=sys.stderr)
    # curl is present on every GitHub-hosted runner and is also what the
    # upstream installer uses. Python's urllib stopped mid-stream against this
    # endpoint on both local hosts and runners.
    subprocess.run(
        [
            "curl",
            "-fsSL",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
            "-o",
            str(archive),
            url,
        ],
        check=True,
    )

    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    if digest != expected:
        print(f"checksum mismatch: {digest} != {expected}", file=sys.stderr)
        return 1

    extracted = work / "extracted"
    extracted.mkdir()
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            bundle.extractall(extracted, filter="data")
    else:
        with tarfile.open(archive) as bundle:
            bundle.extractall(extracted, filter="data")

    suffix = ".exe" if key.startswith("win32") else ""
    source = next(
        (
            path
            for path in extracted.rglob(f"herdr-core{suffix}")
            if path.is_file()
        ),
        None,
    )
    if source is None:
        print("herdr-core executable missing from archive", file=sys.stderr)
        return 1

    install = pathlib.Path(".hc") / "bin"
    install.mkdir(parents=True, exist_ok=True)
    destination = install / f"hc{suffix}"
    shutil.copy2(source, destination)
    destination.chmod(0o755)

    version = os.popen(f'"{destination}" --version').read().strip()
    print(f"installed: {version}", file=sys.stderr)
    if VERSION not in version:
        print(f"unexpected hc version: {version}", file=sys.stderr)
        return 1
    # The workflow appends stdout to GITHUB_PATH.
    print(install.resolve())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
