#!/usr/bin/env python3
"""Package unsigned ukk release binaries into a per-OS archive.

The archive contains the Host and Desktop binaries, README.md, LICENSE,
VERSION.txt build identity and an internal SHA-256 manifest. It never signs,
notarizes, installs services or uploads anything.

Usage (from the repository root):

    python3 scripts/package_release.py --os macos --arch aarch64 \
        --version 0.1.0 --commit "$(git rev-parse HEAD)"

Archive metadata is deterministic: entries are sorted, tar mtimes default to
SOURCE_DATE_EPOCH (or 0) and zip timestamps are fixed at 1980-01-01. Packaging
the same binaries and identity twice therefore produces the same bytes.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import platform
import re
import shutil
import sys
import tarfile
import tempfile
import zipfile
from datetime import datetime, timezone
from pathlib import Path

OS_BINARY_SUFFIX = {"macos": "", "linux": "", "windows": ".exe"}
OS_ARCHIVE_KIND = {"macos": "tar.gz", "linux": "tar.gz", "windows": "zip"}
OS_TARGET_PREFIX = {
    "macos": {"aarch64": "aarch64-apple-darwin", "x86_64": "x86_64-apple-darwin"},
    "linux": {
        "aarch64": "aarch64-unknown-linux-gnu",
        "x86_64": "x86_64-unknown-linux-gnu",
    },
    "windows": {
        "aarch64": "aarch64-pc-windows-msvc",
        "x86_64": "x86_64-pc-windows-msvc",
    },
}
SAFE_VERSION = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]*$")
REQUIRED_FILES = ("README.md", "LICENSE")


def normalize_arch(value: str) -> str:
    normalized = value.strip().lower()
    return {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "x64": "x86_64",
        "amd64": "x86_64",
        "x86_64": "x86_64",
    }.get(normalized, normalized)


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def source_epoch() -> int:
    raw = os.environ.get("SOURCE_DATE_EPOCH", "").strip()
    return int(raw) if raw.isdigit() else 0


def manifest_text(files: dict[str, bytes]) -> str:
    lines = [f"{sha256_bytes(files[name])}  {name}" for name in sorted(files)]
    return "\n".join(lines) + "\n"


def write_tar_gz(archive: Path, root: str, files: dict[str, bytes]) -> None:
    epoch = source_epoch()
    with archive.open("wb") as raw:
        # A fixed gzip mtime keeps repeated packaging byte-identical.
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
                for name in sorted(files):
                    data = files[name]
                    info = tarfile.TarInfo(f"{root}/{name}")
                    info.size = len(data)
                    info.mtime = epoch
                    info.mode = 0o755 if name.startswith("ukk-") else 0o644
                    info.uid = 0
                    info.gid = 0
                    info.uname = ""
                    info.gname = ""
                    tar.addfile(info, io.BytesIO(data))


def write_zip(archive: Path, root: str, files: dict[str, bytes]) -> None:
    epoch = source_epoch()
    if epoch >= 315_532_800:  # 1980-01-01, the earliest zip timestamp
        stamp = datetime.fromtimestamp(epoch, tz=timezone.utc).timetuple()[:6]
    else:
        stamp = (1980, 1, 1, 0, 0, 0)
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as bundle:
        for name in sorted(files):
            info = zipfile.ZipInfo(f"{root}/{name}", date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (
                (0o755 if name.startswith("ukk-") else 0o644) << 16
            ) | 0o100000
            bundle.writestr(info, files[name])


def read_archive(archive: Path, root: str) -> dict[str, bytes]:
    prefix = f"{root}/"
    members: dict[str, bytes] = {}
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as bundle:
            for info in bundle.infolist():
                if info.is_dir():
                    continue
                if not info.filename.startswith(prefix):
                    raise ValueError(f"unexpected archive member {info.filename!r}")
                members[info.filename[len(prefix) :]] = bundle.read(info)
    else:
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                if not member.isfile():
                    continue
                if not member.name.startswith(prefix):
                    raise ValueError(f"unexpected archive member {member.name!r}")
                handle = tar.extractfile(member)
                if handle is None:
                    raise ValueError(f"unreadable archive member {member.name!r}")
                members[member.name[len(prefix) :]] = handle.read()
    return members


def verify_archive(archive: Path, root: str, expected_files: dict[str, bytes]) -> None:
    members = read_archive(archive, root)
    expected_names = set(expected_files) | {"SHA256SUMS.txt"}
    if set(members) != expected_names:
        raise ValueError(
            f"archive members differ from staged files: "
            f"missing={sorted(expected_names - set(members))} "
            f"extra={sorted(set(members) - expected_names)}"
        )
    for name, data in expected_files.items():
        if members[name] != data:
            raise ValueError(f"archived {name} differs from staged content")
    for name, data in members.items():
        if not data:
            raise ValueError(f"archived {name} is empty")
    manifest = members["SHA256SUMS.txt"].decode("utf-8")
    declared = {}
    for line in manifest.splitlines():
        digest, _, name = line.partition("  ")
        if len(digest) != 64 or not name:
            raise ValueError(f"malformed SHA256SUMS.txt line: {line!r}")
        declared[name] = digest
    if set(declared) != set(expected_files):
        raise ValueError("SHA256SUMS.txt does not cover exactly the staged files")
    for name, digest in declared.items():
        actual = sha256_bytes(members[name])
        if actual != digest:
            raise ValueError(f"SHA256SUMS.txt mismatch for {name}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--os", required=True, choices=sorted(OS_BINARY_SUFFIX))
    parser.add_argument("--arch", default=normalize_arch(platform.machine()))
    parser.add_argument("--target", default="")
    parser.add_argument(
        "--version",
        default=os.environ.get("UKK_BUILD_VERSION", ""),
        help="release version embedded in the binaries (default: UKK_BUILD_VERSION)",
    )
    parser.add_argument(
        "--commit",
        default=os.environ.get("UKK_BUILD_COMMIT", "unknown"),
        help="source commit (default: UKK_BUILD_COMMIT)",
    )
    parser.add_argument("--host-bin", default="")
    parser.add_argument("--desktop-bin", default="")
    parser.add_argument("--out-dir", default="dist")
    parser.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    repo = Path(args.root).resolve()
    os_name = args.os
    arch = normalize_arch(args.arch)
    suffix = OS_BINARY_SUFFIX[os_name]
    version = args.version.strip()
    if not version:
        raise SystemExit("package_release.py: --version or UKK_BUILD_VERSION is required")
    if not SAFE_VERSION.fullmatch(version):
        raise SystemExit(
            f"package_release.py: version {version!r} is not a safe archive name"
        )
    if arch not in OS_TARGET_PREFIX[os_name]:
        raise SystemExit(f"package_release.py: unsupported arch {arch!r} for {os_name}")
    target = args.target.strip() or OS_TARGET_PREFIX[os_name][arch]

    host_bin = Path(args.host_bin or repo / "target/release" / f"ukk-host{suffix}")
    desktop_bin = Path(
        args.desktop_bin or repo / "apps/desktop/target/release" / f"ukk-desktop{suffix}"
    )
    host_name = f"ukk-host{suffix}"
    desktop_name = f"ukk-desktop{suffix}"
    for path, expected in ((host_bin, host_name), (desktop_bin, desktop_name)):
        if not path.is_file():
            raise SystemExit(f"package_release.py: missing binary {path}")
        if path.stat().st_size == 0:
            raise SystemExit(f"package_release.py: empty binary {path}")
        if path.name != expected:
            raise SystemExit(
                f"package_release.py: {path} must be named {expected} for {os_name}"
            )

    root = f"ukk-{version}-{os_name}-{arch}"
    archive_name = f"{root}.{OS_ARCHIVE_KIND[os_name]}"
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    archive = out_dir / archive_name

    staged_binaries = {
        host_name: host_bin.read_bytes(),
        desktop_name: desktop_bin.read_bytes(),
    }
    staged_docs = {}
    for name in REQUIRED_FILES:
        source = repo / name
        if not source.is_file():
            raise SystemExit(f"package_release.py: required file missing: {source}")
        staged_docs[name] = source.read_bytes()

    version_file = (
        "artifact=ukk\n"
        f"version={version}\n"
        f"commit={args.commit.strip() or 'unknown'}\n"
        f"target={target}\n"
        f"os={os_name}\n"
        f"arch={arch}\n"
        "profile=release\n"
        f"host_binary={host_name}\n"
        f"desktop_binary={desktop_name}\n"
        "signing=none\n"
    ).encode("utf-8")

    files = {**staged_docs, **staged_binaries, "VERSION.txt": version_file}
    payload = dict(files)
    files["SHA256SUMS.txt"] = manifest_text(files).encode("utf-8")

    staging = Path(tempfile.mkdtemp(prefix="ukk-release-"))
    try:
        for name, data in files.items():
            staged = staging / name
            staged.write_bytes(data)
            if name.startswith("ukk-"):
                staged.chmod(0o755)
        if OS_ARCHIVE_KIND[os_name] == "zip":
            write_zip(archive, root, files)
        else:
            write_tar_gz(archive, root, files)
        verify_archive(archive, root, payload)
    finally:
        shutil.rmtree(staging, ignore_errors=True)

    archive_hash = sha256_file(archive)
    checksum = archive.with_name(f"{archive.name}.sha256")
    # Always write LF: text mode would turn this into CRLF on Windows and break
    # `sha256sum -c`/`shasum -c` after downloading the artifact elsewhere.
    checksum.write_bytes(f"{archive_hash}  {archive.name}\n".encode("utf-8"))

    print(f"Packaged {archive}")
    print(f"  sha256={archive_hash}")
    print(f"  size={archive.stat().st_size}")
    print(f"  target={target}")
    print(f"  files={', '.join(sorted(files))}")
    print("Verified archive members, internal SHA256SUMS.txt and non-empty binaries.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except ValueError as error:
        print(f"package_release.py: {error}", file=sys.stderr)
        sys.exit(1)
