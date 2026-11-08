#!/usr/bin/env python3
"""Package unsigned ukk release binaries into a per-OS archive.

The archive contains the Host and Desktop binaries, README.md, LICENSE,
VERSION.txt build identity and an internal SHA-256 manifest. On macOS the
Desktop binary is staged inside an unsigned ``ukk.app`` bundle with a
deterministic ``Contents/Info.plist``; the Host stays a sibling executable
because it is a background service/CLI, not a GUI app. The script never signs,
notarizes, installs services or uploads anything.

Usage (from the repository root):

    python3 scripts/package_release.py --os macos --arch aarch64 \
        --version 0.1.0 --commit "$(git rev-parse HEAD)"

Archive metadata is deterministic: entries are sorted, tar mtimes default to
SOURCE_DATE_EPOCH (or 0), zip timestamps are fixed at 1980-01-01 and the macOS
Info.plist is generated from a fixed key order. Packaging the same binaries and
identity twice therefore produces the same bytes.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import os
import platform
import plistlib
import re
import shutil
import sys
import tarfile
import tempfile
import zipfile
from datetime import datetime, timezone
from pathlib import Path, PurePosixPath
from xml.sax.saxutils import escape

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
BUNDLE_VERSION = re.compile(r"^(\d+(?:\.\d+){0,2})")
REQUIRED_FILES = ("README.md", "LICENSE")

# macOS bundle identity. The identifier follows the reverse-DNS prefix already
# used by the iOS project (dev.ukk) and makes no signing or distribution claim.
MACOS_BUNDLE_NAME = "ukk.app"
MACOS_BUNDLE_ID = "dev.ukk.desktop"
MACOS_BUNDLE_DISPLAY_NAME = "ukk"
MACOS_BUNDLE_EXECUTABLE = "ukk-desktop"
MACOS_BUNDLE_INFO_PLIST = f"{MACOS_BUNDLE_NAME}/Contents/Info.plist"
MACOS_BUNDLE_ICON = f"{MACOS_BUNDLE_NAME}/Contents/Resources/ukk.icns"
MACOS_ICON_SOURCE = "assets/icon/ukk.icns"
MACOS_BUNDLE_BINARY = (
    f"{MACOS_BUNDLE_NAME}/Contents/MacOS/{MACOS_BUNDLE_EXECUTABLE}"
)


def normalize_arch(value: str) -> str:
    normalized = value.strip().lower()
    return {
        "arm64": "aarch64",
        "aarch64": "aarch64",
        "x64": "x86_64",
        "amd64": "x86_64",
        "x86_64": "x86_64",
    }.get(normalized, normalized)


def bundle_version(version: str) -> str:
    """Return the numeric CFBundle version derived from a release version.

    Apple requires CFBundleShortVersionString/CFBundleVersion to be
    period-separated integers. Tag versions like ``1.2.3`` pass through
    unchanged; development versions like ``0.1.0-dev.abc1234`` use their
    numeric prefix (``0.1.0``). The full build identity stays in VERSION.txt.
    """
    match = BUNDLE_VERSION.match(version.strip())
    return match.group(1) if match else "0.0.0"


def info_plist(version: str) -> bytes:
    """Render a deterministic unsigned macOS Info.plist."""
    short_version = bundle_version(version)
    entries: list[tuple[str, str | bool]] = [
        ("CFBundleDisplayName", MACOS_BUNDLE_DISPLAY_NAME),
        ("CFBundleExecutable", MACOS_BUNDLE_EXECUTABLE),
        ("CFBundleIconFile", "ukk.icns"),
        ("CFBundleIdentifier", MACOS_BUNDLE_ID),
        ("CFBundleInfoDictionaryVersion", "6.0"),
        ("CFBundleName", MACOS_BUNDLE_DISPLAY_NAME),
        ("CFBundlePackageType", "APPL"),
        ("CFBundleShortVersionString", short_version),
        ("CFBundleVersion", short_version),
        ("NSHighResolutionCapable", True),
    ]
    lines = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
        '<plist version="1.0">',
        "<dict>",
    ]
    for key, value in sorted(entries):
        lines.append(f"\t<key>{escape(key)}</key>")
        if isinstance(value, bool):
            lines.append("\t<true/>" if value else "\t<false/>")
        else:
            lines.append(f"\t<string>{escape(value)}</string>")
    lines.extend(["</dict>", "</plist>", ""])
    return "\n".join(lines).encode("utf-8")


def is_executable_member(name: str) -> bool:
    return PurePosixPath(name).name.startswith("ukk-")


def validate_member_name(name: str) -> None:
    """Reject archive names that could escape the extraction root."""
    if not name or name.startswith("/") or "\\" in name:
        raise ValueError(f"unsafe archive member name {name!r}")
    if any(part in ("", ".", "..") for part in name.split("/")):
        raise ValueError(f"unsafe archive member name {name!r}")


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
    for name in files:
        validate_member_name(name)
    lines = [f"{sha256_bytes(files[name])}  {name}" for name in sorted(files)]
    return "\n".join(lines) + "\n"


def write_tar_gz(archive: Path, root: str, files: dict[str, bytes]) -> None:
    epoch = source_epoch()
    with archive.open("wb") as raw:
        # A fixed gzip mtime keeps repeated packaging byte-identical.
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.PAX_FORMAT) as tar:
                for name in sorted(files):
                    validate_member_name(name)
                    data = files[name]
                    info = tarfile.TarInfo(f"{root}/{name}")
                    info.size = len(data)
                    info.mtime = epoch
                    info.mode = 0o755 if is_executable_member(name) else 0o644
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
            validate_member_name(name)
            info = zipfile.ZipInfo(f"{root}/{name}", date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (
                (0o755 if is_executable_member(name) else 0o644) << 16
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
                name = info.filename[len(prefix) :]
                validate_member_name(name)
                if name in members:
                    raise ValueError(f"duplicate archive member {info.filename!r}")
                members[name] = bundle.read(info)
    else:
        with tarfile.open(archive, "r:gz") as tar:
            for member in tar.getmembers():
                if not member.isfile():
                    continue
                if not member.name.startswith(prefix):
                    raise ValueError(f"unexpected archive member {member.name!r}")
                name = member.name[len(prefix) :]
                validate_member_name(name)
                if name in members:
                    raise ValueError(f"duplicate archive member {member.name!r}")
                handle = tar.extractfile(member)
                if handle is None:
                    raise ValueError(f"unreadable archive member {member.name!r}")
                members[name] = handle.read()
    return members


def verify_macos_bundle(files: dict[str, bytes], version: str) -> None:
    """Verify the staged bundle before it is written to the archive."""
    if MACOS_BUNDLE_INFO_PLIST not in files:
        raise ValueError(f"macOS archive is missing {MACOS_BUNDLE_INFO_PLIST}")
    try:
        info = plistlib.loads(files[MACOS_BUNDLE_INFO_PLIST])
    except Exception as error:  # normalize any parser failure into ValueError
        raise ValueError(f"Info.plist is not a valid property list: {error}") from error
    if not isinstance(info, dict):
        raise ValueError("Info.plist must contain a dictionary")
    expected = {
        "CFBundleDisplayName": MACOS_BUNDLE_DISPLAY_NAME,
        "CFBundleExecutable": MACOS_BUNDLE_EXECUTABLE,
        "CFBundleIconFile": "ukk.icns",
        "CFBundleIdentifier": MACOS_BUNDLE_ID,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": MACOS_BUNDLE_DISPLAY_NAME,
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": bundle_version(version),
        "CFBundleVersion": bundle_version(version),
        "NSHighResolutionCapable": True,
    }
    for key, value in expected.items():
        if info.get(key) != value:
            raise ValueError(f"Info.plist {key} is {info.get(key)!r}, expected {value!r}")
    executable = f"{MACOS_BUNDLE_NAME}/Contents/MacOS/{MACOS_BUNDLE_EXECUTABLE}"
    if executable not in files:
        raise ValueError(f"macOS bundle is missing its executable {executable}")
    if not files[executable]:
        raise ValueError(f"macOS bundle executable {executable} is empty")
    icon = files.get(MACOS_BUNDLE_ICON)
    if not icon:
        raise ValueError(f"macOS bundle is missing its icon {MACOS_BUNDLE_ICON}")
    if not icon.startswith(b"icns"):
        raise ValueError(f"macOS bundle icon {MACOS_BUNDLE_ICON} is not an icns file")
    for name in files:
        if "/_CodeSignature/" in f"/{name}" or name.endswith(".dsig"):
            raise ValueError("unsigned macOS archive must not contain code signatures")


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
        validate_member_name(name)
        if name in declared:
            raise ValueError(f"duplicate SHA256SUMS.txt entry: {name!r}")
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
    }
    desktop_data = desktop_bin.read_bytes()
    if os_name == "macos":
        icon_source = repo / MACOS_ICON_SOURCE
        if not icon_source.is_file() or icon_source.stat().st_size == 0:
            raise SystemExit(f"package_release.py: missing icon {icon_source}")
        staged_binaries[MACOS_BUNDLE_BINARY] = desktop_data
        staged_binaries[MACOS_BUNDLE_ICON] = icon_source.read_bytes()
        staged_binaries[MACOS_BUNDLE_INFO_PLIST] = info_plist(version)
    else:
        staged_binaries[desktop_name] = desktop_data
    staged_docs = {}
    for name in REQUIRED_FILES:
        source = repo / name
        if not source.is_file():
            raise SystemExit(f"package_release.py: required file missing: {source}")
        staged_docs[name] = source.read_bytes()

    version_lines = [
        "artifact=ukk",
        f"version={version}",
        f"commit={args.commit.strip() or 'unknown'}",
        f"target={target}",
        f"os={os_name}",
        f"arch={arch}",
        "profile=release",
        f"host_binary={host_name}",
        f"desktop_binary={desktop_name}",
    ]
    if os_name == "macos":
        version_lines.extend(
            [
                f"desktop_bundle={MACOS_BUNDLE_NAME}",
                f"desktop_bundle_path={MACOS_BUNDLE_BINARY}",
                f"desktop_bundle_icon={MACOS_BUNDLE_ICON}",
                f"bundle_id={MACOS_BUNDLE_ID}",
                f"bundle_version={bundle_version(version)}",
            ]
        )
    version_lines.append("signing=none")
    version_file = ("\n".join(version_lines) + "\n").encode("utf-8")

    files = {**staged_docs, **staged_binaries, "VERSION.txt": version_file}
    if os_name == "macos":
        verify_macos_bundle(files, version)
    payload = dict(files)
    files["SHA256SUMS.txt"] = manifest_text(files).encode("utf-8")

    staging = Path(tempfile.mkdtemp(prefix="ukk-release-"))
    try:
        for name, data in files.items():
            staged = staging / name
            staged.parent.mkdir(parents=True, exist_ok=True)
            staged.write_bytes(data)
            if is_executable_member(name):
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
    if os_name == "macos":
        print(f"Verified unsigned {MACOS_BUNDLE_NAME} layout and Info.plist values.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except ValueError as error:
        print(f"package_release.py: {error}", file=sys.stderr)
        sys.exit(1)
