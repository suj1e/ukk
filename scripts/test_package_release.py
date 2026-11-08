#!/usr/bin/env python3
"""Focused tests for scripts/package_release.py.

Standard library only, so CI can run this on macOS, Linux and Windows without
extra dependencies. The tests build archives from fake binaries and prove the
macOS ``ukk.app`` layout, Info.plist values, checksum coverage, executable
modes, deterministic bytes and the unchanged Windows/Linux archives.
"""

from __future__ import annotations

import contextlib
import io
import os
import plistlib
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import package_release as packaging  # noqa: E402


def build_repo(tmp: Path, os_name: str) -> tuple[Path, Path, Path]:
    """Create a minimal repository fixture with named release binaries."""
    repo = tmp / "repo"
    build = repo / "build"
    build.mkdir(parents=True, exist_ok=True)
    (repo / "README.md").write_bytes(b"# ukk\n")
    (repo / "LICENSE").write_bytes(b"license text\n")
    (repo / "assets/icon").mkdir(parents=True, exist_ok=True)
    (repo / "assets/icon/ukk.icns").write_bytes(b"icns-fake-icon")
    suffix = packaging.OS_BINARY_SUFFIX[os_name]
    host = build / f"ukk-host{suffix}"
    desktop = build / f"ukk-desktop{suffix}"
    host.write_bytes(b"host binary\n")
    desktop.write_bytes(b"desktop binary\n")
    return repo, host, desktop


def package(
    tmp: Path,
    os_name: str,
    *,
    arch: str = "aarch64",
    version: str = "0.1.0-dev.abc1234",
    out_name: str = "dist",
) -> tuple[Path, Path, str, Path]:
    repo, host, desktop = build_repo(tmp, os_name)
    out_dir = tmp / out_name
    argv = [
        "--os",
        os_name,
        "--arch",
        arch,
        "--version",
        version,
        "--commit",
        "abc1234",
        "--host-bin",
        str(host),
        "--desktop-bin",
        str(desktop),
        "--out-dir",
        str(out_dir),
        "--root",
        str(repo),
    ]
    with contextlib.redirect_stdout(io.StringIO()):
        packaging.main(argv)
    root = f"ukk-{version}-{os_name}-{arch}"
    archive = out_dir / f"{root}.{packaging.OS_ARCHIVE_KIND[os_name]}"
    return archive, out_dir, root, desktop


def manifest_entries(manifest: bytes) -> dict[str, str]:
    entries: dict[str, str] = {}
    for line in manifest.decode("utf-8").splitlines():
        digest, _, name = line.partition("  ")
        if len(digest) != 64 or not name:
            raise AssertionError(f"malformed manifest line: {line!r}")
        entries[name] = digest
    return entries


class MacosBundleTest(unittest.TestCase):
    def test_layout_plist_and_checksum_coverage(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            archive, _, root, desktop = package(tmp, "macos")
            members = packaging.read_archive(archive, root)

            expected = {
                "LICENSE",
                "README.md",
                "SHA256SUMS.txt",
                "VERSION.txt",
                "ukk-host",
                packaging.MACOS_BUNDLE_INFO_PLIST,
                packaging.MACOS_BUNDLE_ICON,
                packaging.MACOS_BUNDLE_BINARY,
            }
            self.assertEqual(set(members), expected)
            self.assertEqual(
                members[packaging.MACOS_BUNDLE_BINARY], desktop.read_bytes()
            )
            self.assertEqual(members["ukk-host"], b"host binary\n")

            info = plistlib.loads(members[packaging.MACOS_BUNDLE_INFO_PLIST])
            self.assertEqual(info["CFBundleIdentifier"], "dev.ukk.desktop")
            self.assertEqual(info["CFBundleName"], "ukk")
            self.assertEqual(info["CFBundleDisplayName"], "ukk")
            self.assertEqual(info["CFBundleExecutable"], "ukk-desktop")
            self.assertEqual(info["CFBundleIconFile"], "ukk.icns")
            self.assertEqual(info["CFBundlePackageType"], "APPL")
            self.assertEqual(info["CFBundleShortVersionString"], "0.1.0")
            self.assertEqual(info["CFBundleVersion"], "0.1.0")
            self.assertIs(info["NSHighResolutionCapable"], True)
            self.assertEqual(info["CFBundleIconFile"], "ukk.icns")
            self.assertFalse(
                [key for key in info if "Sign" in key or "Entitlement" in key]
            )

            # SHA256SUMS.txt covers every member except itself, with real hashes.
            declared = manifest_entries(members["SHA256SUMS.txt"])
            self.assertEqual(set(declared), expected - {"SHA256SUMS.txt"})
            for name, digest in declared.items():
                self.assertEqual(packaging.sha256_bytes(members[name]), digest)
            self.assertNotIn(b"\r", members["SHA256SUMS.txt"])
            manifest_lines = members["SHA256SUMS.txt"].decode("utf-8").splitlines()
            self.assertEqual(
                manifest_lines, sorted(manifest_lines, key=lambda line: line[66:])
            )
            checksum = archive.with_name(archive.name + ".sha256").read_bytes()
            self.assertNotIn(b"\r", checksum)
            self.assertTrue(checksum.endswith(b"\n"))

            version_text = members["VERSION.txt"].decode("utf-8")
            self.assertIn("desktop_bundle=ukk.app\n", version_text)
            self.assertIn(
                "desktop_bundle_path=ukk.app/Contents/MacOS/ukk-desktop\n",
                version_text,
            )
            self.assertIn("bundle_id=dev.ukk.desktop\n", version_text)
            self.assertIn("signing=none\n", version_text)

    def test_tar_modes_make_bundle_executable(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            archive, _, root, _ = package(tmp, "macos")
            with tarfile.open(archive, "r:gz") as tar:
                modes = {
                    member.name[len(root) + 1 :]: member.mode
                    for member in tar.getmembers()
                    if member.isfile()
                }
            self.assertEqual(modes[packaging.MACOS_BUNDLE_BINARY], 0o755)
            self.assertEqual(modes[packaging.MACOS_BUNDLE_INFO_PLIST], 0o644)
            self.assertEqual(modes[packaging.MACOS_BUNDLE_ICON], 0o644)
            self.assertEqual(modes["ukk-host"], 0o755)
            self.assertEqual(modes["README.md"], 0o644)

    def test_extracted_bundle_is_a_directory_with_plist(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            archive, _, root, _ = package(tmp, "macos")
            extract = tmp / "extract"
            extract.mkdir()
            with tarfile.open(archive, "r:gz") as tar:
                for member in tar.getmembers():
                    if not member.isfile():
                        continue
                    target = extract / member.name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    handle = tar.extractfile(member)
                    assert handle is not None
                    target.write_bytes(handle.read())
                    target.chmod(member.mode & 0o777)
            bundle = extract / root / packaging.MACOS_BUNDLE_NAME
            self.assertTrue(bundle.is_dir())
            plist = bundle / "Contents/Info.plist"
            self.assertTrue(plist.is_file())
            executable = bundle / "Contents/MacOS/ukk-desktop"
            self.assertTrue(executable.is_file())
            if os.name == "posix":
                self.assertTrue(executable.stat().st_mode & 0o111)

            if sys.platform == "darwin" and shutil.which("plutil"):
                subprocess.run(
                    ["plutil", "-lint", str(plist)],
                    check=True,
                    capture_output=True,
                )
                bundle_id = subprocess.run(
                    ["plutil", "-extract", "CFBundleIdentifier", "raw", str(plist)],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()
                self.assertEqual(bundle_id, "dev.ukk.desktop")
            if sys.platform == "darwin" and shutil.which("shasum"):
                subprocess.run(
                    ["shasum", "-a", "256", "-c", "SHA256SUMS.txt"],
                    cwd=extract / root,
                    check=True,
                    capture_output=True,
                )

    def test_archive_is_deterministic(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            first, _, _, _ = package(tmp, "macos", out_name="dist-one")
            second, _, _, _ = package(tmp, "macos", out_name="dist-two")
            self.assertEqual(first.read_bytes(), second.read_bytes())
            self.assertEqual(
                first.with_name(first.name + ".sha256").read_bytes(),
                second.with_name(second.name + ".sha256").read_bytes(),
            )


class OtherPlatformTest(unittest.TestCase):
    def test_windows_zip_layout_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            archive, _, root, _ = package(tmp, "windows", arch="x86_64")
            self.assertEqual(archive.suffix, ".zip")
            members = packaging.read_archive(archive, root)
            self.assertEqual(
                set(members),
                {
                    "LICENSE",
                    "README.md",
                    "SHA256SUMS.txt",
                    "VERSION.txt",
                    "ukk-host.exe",
                    "ukk-desktop.exe",
                },
            )
            self.assertFalse(any(".app" in name for name in members))
            with zipfile.ZipFile(archive) as bundle:
                modes = {
                    info.filename[len(root) + 1 :]: info.external_attr >> 16
                    for info in bundle.infolist()
                    if not info.is_dir()
                }
            self.assertEqual(modes["ukk-host.exe"], 0o755)
            self.assertEqual(modes["ukk-desktop.exe"], 0o755)
            self.assertEqual(modes["README.md"], 0o644)

    def test_linux_tar_layout_unchanged(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            archive, _, root, _ = package(tmp, "linux", arch="x86_64")
            members = packaging.read_archive(archive, root)
            self.assertEqual(
                set(members),
                {
                    "LICENSE",
                    "README.md",
                    "SHA256SUMS.txt",
                    "VERSION.txt",
                    "ukk-host",
                    "ukk-desktop",
                },
            )
            self.assertNotIn("desktop_bundle", members["VERSION.txt"].decode("utf-8"))


class ValidationTest(unittest.TestCase):
    def test_bundle_version_mapping(self) -> None:
        self.assertEqual(packaging.bundle_version("1.2.3"), "1.2.3")
        self.assertEqual(packaging.bundle_version("0.1.0-dev.abc1234"), "0.1.0")
        self.assertEqual(packaging.bundle_version("2.4"), "2.4")
        self.assertEqual(packaging.bundle_version("7"), "7")
        self.assertEqual(packaging.bundle_version("nightly"), "0.0.0")

    def test_member_name_validation_rejects_escapes(self) -> None:
        for name in ("../evil", "/absolute", "a\\b", "", "a/../../b"):
            with self.subTest(name=name):
                with self.assertRaises(ValueError):
                    packaging.validate_member_name(name)
        packaging.validate_member_name("ukk.app/Contents/Info.plist")

    def test_verify_archive_rejects_extra_member(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            root = "ukk-1.0.0-test"
            files = {"README.md": b"readme\n"}
            files["SHA256SUMS.txt"] = packaging.manifest_text(files).encode("utf-8")
            tampered = dict(files)
            tampered["extra.txt"] = b"extra\n"
            archive = tmp / "tampered.tar.gz"
            packaging.write_tar_gz(archive, root, tampered)
            with self.assertRaises(ValueError):
                packaging.verify_archive(archive, root, files)

    def test_verify_archive_rejects_wrong_digest(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            root = "ukk-1.0.0-test"
            files = {"README.md": b"readme\n"}
            files["SHA256SUMS.txt"] = (
                f"{'0' * 64}  README.md\n".encode("utf-8")
            )
            archive = tmp / "bad-manifest.tar.gz"
            packaging.write_tar_gz(archive, root, files)
            with self.assertRaises(ValueError):
                packaging.verify_archive(archive, root, {"README.md": b"readme\n"})

    def test_verify_macos_bundle_rejects_bad_executable(self) -> None:
        plist = plistlib.loads(packaging.info_plist("1.0.0"))
        plist["CFBundleExecutable"] = "not-ukk-desktop"
        files = {
            packaging.MACOS_BUNDLE_INFO_PLIST: plistlib.dumps(plist),
            packaging.MACOS_BUNDLE_BINARY: b"binary\n",
        }
        with self.assertRaises(ValueError):
            packaging.verify_macos_bundle(files, "1.0.0")
        del files[packaging.MACOS_BUNDLE_BINARY]
        files[packaging.MACOS_BUNDLE_INFO_PLIST] = packaging.info_plist("1.0.0")
        with self.assertRaises(ValueError):
            packaging.verify_macos_bundle(files, "1.0.0")

    def test_missing_binary_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            repo, host, desktop = build_repo(tmp, "macos")
            desktop.unlink()
            argv = [
                "--os",
                "macos",
                "--arch",
                "aarch64",
                "--version",
                "0.1.0",
                "--host-bin",
                str(host),
                "--desktop-bin",
                str(desktop),
                "--out-dir",
                str(tmp / "dist"),
                "--root",
                str(repo),
            ]
            with contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(SystemExit):
                    packaging.main(argv)


if __name__ == "__main__":
    unittest.main(verbosity=2)
