#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import importlib.util
import io
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
INSTALLER = REPO_ROOT / "scripts" / "install-herdr-plugin.py"
ASSET = "tabby-aarch64-apple-darwin.tar.xz"
RELEASE_ROOT = "https://github.com/yersonargotev/tabby/releases/download/v0.1.12"

installer_spec = importlib.util.spec_from_file_location("install_herdr_plugin", INSTALLER)
if installer_spec is None or installer_spec.loader is None:
    raise RuntimeError(f"cannot load installer from {INSTALLER}")
installer = importlib.util.module_from_spec(installer_spec)
installer_spec.loader.exec_module(installer)


def release_archive(contents: bytes = b"release-tabby") -> bytes:
    archive = io.BytesIO()
    with tarfile.open(fileobj=archive, mode="w:xz") as tar:
        info = tarfile.TarInfo("tabby-aarch64-apple-darwin/tabby")
        info.mode = 0o755
        info.size = len(contents)
        tar.addfile(info, io.BytesIO(contents))
    return archive.getvalue()


def release_responses(archive: bytes, checksum: bytes | None = None) -> dict[str, bytes]:
    if checksum is None:
        digest = hashlib.sha256(archive).hexdigest()
        checksum = f"{digest} *{ASSET}\n".encode()
    return {
        f"{RELEASE_ROOT}/{ASSET}": archive,
        f"{RELEASE_ROOT}/{ASSET}.sha256": checksum,
    }


class FakeAdapter:
    def __init__(
        self,
        responses: dict[str, bytes],
        source_binary: bytes | None = None,
        host: tuple[str, str] = ("Darwin", "arm64"),
    ) -> None:
        self.responses = responses
        self.source_binary = source_binary
        self.host = host
        self.downloads: list[str] = []
        self.source_builds = 0

    def platform(self) -> tuple[str, str]:
        return self.host

    def download(self, url: str, destination: Path) -> None:
        self.downloads.append(url)
        if url not in self.responses:
            raise installer.ArtifactNotFound(f"release asset not found: {url}")
        destination.write_bytes(self.responses[url])

    def build_from_source(self, plugin_root: Path) -> Path:
        self.source_builds += 1
        if self.source_binary is None:
            raise installer.InstallError("cargo is required to build Tabby from source")
        binary = plugin_root / "target/release/tabby"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_bytes(self.source_binary)
        binary.chmod(0o755)
        return binary


class InstallHerdrPluginTests(unittest.TestCase):
    def plugin_root(self, root: Path) -> None:
        (root / "herdr-plugin.toml").write_text(
            'id = "yersonargotev.tabby"\nversion = "0.1.12"\n'
        )

    def test_installs_the_manifest_version_release_atomically(self) -> None:
        archive = release_archive()
        adapter = FakeAdapter(release_responses(archive))

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)
            destination = root / ".herdr/bin/tabby"
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b"previous-tabby")

            installed = installer.install(root, adapter)

            self.assertEqual(installed, destination.resolve())
            self.assertEqual(destination.read_bytes(), b"release-tabby")
            self.assertTrue(os.access(destination, os.X_OK))
            self.assertEqual(adapter.source_builds, 0)
            self.assertEqual(
                sorted(path.name for path in destination.parent.iterdir()),
                ["tabby"],
            )

    def test_builds_from_source_only_when_the_release_artifact_is_missing(self) -> None:
        adapter = FakeAdapter({}, source_binary=b"source-tabby")

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)

            installer.install(root, adapter)

            self.assertEqual(
                (root / ".herdr/bin/tabby").read_bytes(),
                b"source-tabby",
            )
            self.assertEqual(adapter.source_builds, 1)
            self.assertEqual(len(adapter.downloads), 1)

    def test_missing_artifact_requires_the_documented_rust_toolchain(self) -> None:
        adapter = FakeAdapter({})

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)

            with self.assertRaisesRegex(installer.InstallError, "cargo is required"):
                installer.install(root, adapter)

        self.assertEqual(adapter.source_builds, 1)

    def test_rejects_unsupported_platforms_before_downloading(self) -> None:
        adapter = FakeAdapter({}, host=("Darwin", "x86_64"))

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)

            with self.assertRaisesRegex(installer.InstallError, "unsupported platform Darwin/x86_64"):
                installer.install(root, adapter)

        self.assertEqual(adapter.downloads, [])
        self.assertEqual(adapter.source_builds, 0)

    def test_missing_checksum_rejects_the_download_without_source_fallback(self) -> None:
        adapter = FakeAdapter({f"{RELEASE_ROOT}/{ASSET}": release_archive()})

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)

            with self.assertRaisesRegex(installer.InstallError, "published checksum is missing"):
                installer.install(root, adapter)

        self.assertEqual(adapter.source_builds, 0)

    def test_network_failure_does_not_trigger_source_fallback(self) -> None:
        class NetworkFailureAdapter(FakeAdapter):
            def download(self, url: str, destination: Path) -> None:
                self.downloads.append(url)
                raise installer.InstallError("network failure: connection timed out")

        adapter = NetworkFailureAdapter({})

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)

            with self.assertRaisesRegex(installer.InstallError, "network failure"):
                installer.install(root, adapter)

        self.assertEqual(adapter.source_builds, 0)

    def test_malformed_or_mismatched_checksums_reject_the_download(self) -> None:
        archive = release_archive()
        cases = {
            "malformed": b"not-a-checksum\n",
            "wrong asset": f"{'0' * 64} *another.tar.xz\n".encode(),
            "mismatch": f"{'0' * 64} *{ASSET}\n".encode(),
        }

        for name, checksum in cases.items():
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temp_dir:
                root = Path(temp_dir)
                self.plugin_root(root)
                destination = root / ".herdr/bin/tabby"
                destination.parent.mkdir(parents=True)
                destination.write_bytes(b"previous-tabby")
                adapter = FakeAdapter(release_responses(archive, checksum))

                with self.assertRaises(installer.InstallError):
                    installer.install(root, adapter)

                self.assertEqual(destination.read_bytes(), b"previous-tabby")
                self.assertEqual(adapter.source_builds, 0)

    def test_invalid_archive_preserves_the_previous_executable(self) -> None:
        archive = b"not-an-xz-archive"
        adapter = FakeAdapter(release_responses(archive))

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)
            destination = root / ".herdr/bin/tabby"
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b"previous-tabby")

            with self.assertRaisesRegex(installer.InstallError, "could not extract"):
                installer.install(root, adapter)

            self.assertEqual(destination.read_bytes(), b"previous-tabby")
            self.assertEqual(
                sorted(path.name for path in destination.parent.iterdir()),
                ["tabby"],
            )

    def test_permission_failure_preserves_the_previous_executable(self) -> None:
        archive = release_archive()
        adapter = FakeAdapter(release_responses(archive))

        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.plugin_root(root)
            destination = root / ".herdr/bin/tabby"
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b"previous-tabby")

            with mock.patch.object(
                installer.os,
                "replace",
                side_effect=PermissionError("permission denied"),
            ), self.assertRaisesRegex(installer.InstallError, "permission denied"):
                installer.install(root, adapter)

            self.assertEqual(destination.read_bytes(), b"previous-tabby")
            self.assertEqual(
                sorted(path.name for path in destination.parent.iterdir()),
                ["tabby"],
            )

    def test_command_reports_a_capped_actionable_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            (root / "herdr-plugin.toml").write_text('id = "yersonargotev.tabby"\n')

            completed = subprocess.run(
                [sys.executable, str(INSTALLER), "--plugin-root", str(root)],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
            )

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("manifest is missing a top-level version", completed.stderr)
        self.assertLessEqual(len(completed.stderr), installer.DIAGNOSTIC_MAX_CHARS + 100)
        self.assertNotIn("Traceback", completed.stderr)


if __name__ == "__main__":
    unittest.main()
