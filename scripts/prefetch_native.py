#!/usr/bin/env python3
"""Pre-seed MyGo's pinned terminal libraries before packaging.

MyGo downloads these from GitHub releases while building; that CDN intermittently
times out on hosted runners. This script uses gh/curl with retries, verifies the
published SHA-256, and installs the file where MyGo's cache expects it.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

TAG = "libghostty-vt-befcdfd2c3a1"
FILES = {
    "darwin-arm64": ("libghostty-vt-darwin-arm64.dylib", "libghostty-vt.dylib",
                     "6f385d3221f8c776ba40530db9baf8c71478907897959849896eeb0ddbf16b41"),
    "darwin-amd64": ("libghostty-vt-darwin-amd64.dylib", "libghostty-vt.dylib",
                     "421e98d2683b39555209c59395655b4a217da0382e7669c92f6dff3949832664"),
    "linux-amd64": ("libghostty-vt-linux-amd64.so", "libghostty-vt.so",
                    "9dcd8af0de67778a561f436cb673a9fb1738be8fa00849dad49606baed1e57d7"),
    "linux-arm64": ("libghostty-vt-linux-arm64.so", "libghostty-vt.so",
                    "fb163f8f123206956751f1c4f0303b40899513b7dc171a8ab8d85ba7cd541bc0"),
    "windows-amd64": ("libghostty-vt-windows-amd64.dll", "ghostty-vt.dll",
                      "53e8e2dd979702e8f2c48107af19be5a76efc2e88eb7cf1097218e899665da41"),
    "windows-arm64": ("libghostty-vt-windows-arm64.dll", "ghostty-vt.dll",
                      "de56a362f12cce8be38e883ffc8aec90a890e0574eb73aac6e833a60fda26064"),
}


def cache_root() -> Path:
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches" / "mygo" / "natives"
    if os.name == "nt":
        return Path(os.environ.get("LOCALAPPDATA", Path.home())) / "mygo" / "natives"
    base = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return base / "mygo" / "natives"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(remote: str, destination: Path) -> None:
    url = f"https://github.com/egoist/mygo/releases/download/{TAG}/{remote}"
    for attempt in range(1, 6):
        if shutil.which("gh"):
            tmp = destination.parent / remote
            result = subprocess.run(
                ["gh", "release", "download", TAG, "-R", "egoist/mygo",
                 "-p", remote, "-D", str(destination.parent), "--clobber"],
                capture_output=True, text=True,
            )
            if result.returncode == 0 and tmp.exists():
                tmp.replace(destination)
                return
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "ukk-release"})
            with urllib.request.urlopen(request, timeout=60) as response:
                with destination.open("wb") as handle:
                    shutil.copyfileobj(response, handle)
            return
        except Exception as error:  # network errors are retried below
            print(f"attempt {attempt} failed for {remote}: {error}", file=sys.stderr)
            time.sleep(attempt * 5)
    raise SystemExit(f"could not download {remote}")


def targets_for(platform: str) -> list[str]:
    if platform == "darwin/universal":
        return ["darwin-arm64", "darwin-amd64"]
    os_name, arch = platform.split("/", 1)
    mapping = {"darwin": "darwin", "linux": "linux", "windows": "windows"}
    if os_name not in mapping:
        raise SystemExit(f"unsupported target {platform}")
    return [f"{mapping[os_name]}-{arch}"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, help="GOOS/GOARCH, or darwin/universal")
    args = parser.parse_args()
    root = cache_root()
    root.mkdir(parents=True, exist_ok=True)
    for target in targets_for(args.target):
        if target not in FILES:
            raise SystemExit(f"no pinned library for {target}")
        remote, local_name, expected = FILES[target]
        directory = root / expected
        directory.mkdir(parents=True, exist_ok=True)
        destination = directory / local_name
        if destination.exists() and sha256(destination) == expected:
            print(f"{target}: already cached")
            continue
        with tempfile.TemporaryDirectory(dir=directory) as tmpdir:
            download = Path(tmpdir) / remote
            fetch(remote, download)
            actual = sha256(download)
            if actual != expected:
                raise SystemExit(f"{target}: checksum mismatch: {actual}")
            download.replace(destination)
        print(f"{target}: installed {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
