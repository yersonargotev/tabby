#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CHECKER = REPO_ROOT / "scripts" / "check-herdr-manifests.py"
CANONICAL_BINARY = ".herdr/bin/tabby"

checker_spec = importlib.util.spec_from_file_location("check_herdr_manifests", CHECKER)
if checker_spec is None or checker_spec.loader is None:
    raise RuntimeError(f"cannot load manifest checker from {CHECKER}")
checker = importlib.util.module_from_spec(checker_spec)
checker_spec.loader.exec_module(checker)


class HerdrManifestContractTests(unittest.TestCase):
    def run_checker(
        self,
        canonical_manifest: Path | None = None,
        homebrew_manifest: Path | None = None,
        cargo_manifest: Path | None = None,
    ) -> subprocess.CompletedProcess[str]:
        command = [sys.executable, str(CHECKER)]
        if canonical_manifest is not None:
            command.extend(["--canonical-manifest", str(canonical_manifest)])
        if homebrew_manifest is not None:
            command.extend(["--homebrew-manifest", str(homebrew_manifest)])
        if cargo_manifest is not None:
            command.extend(["--cargo-manifest", str(cargo_manifest)])
        return subprocess.run(
            command,
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )

    def test_manifests_match_package_version_and_runtime_contract(self) -> None:
        completed = self.run_checker()

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("match Cargo package version", completed.stdout)

    def test_canonical_manifest_uses_one_stable_plugin_root_binary(self) -> None:
        manifest = checker.load_manifest(REPO_ROOT / "herdr-plugin.toml")

        commands = [entry["command"] for entry in manifest["startup"]]
        commands.extend(action["command"] for action in manifest["actions"])
        commands.extend(event["command"] for event in manifest["events"])

        self.assertTrue(commands)
        self.assertEqual({command[0] for command in commands}, {CANONICAL_BINARY})
        self.assertNotIn("scaffold", manifest["description"].lower())

    def test_validator_allows_a_canonical_build_adapter(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            canonical = root / "herdr-plugin.toml"
            homebrew = root / "homebrew-plugin.toml"
            cargo = root / "Cargo.toml"
            shutil.copy2(REPO_ROOT / "herdr-plugin.toml", canonical)
            shutil.copy2(REPO_ROOT / "packaging/herdr/herdr-plugin.toml", homebrew)
            shutil.copy2(REPO_ROOT / "Cargo.toml", cargo)
            canonical.write_text(
                canonical.read_text()
                + '\n[[build]]\ncommand = ["python3", "scripts/install-herdr-plugin.py"]\n'
            )

            completed = self.run_checker(canonical, homebrew, cargo)

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn(str(canonical), completed.stdout)

    def test_validator_rejects_product_semantic_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            canonical = root / "herdr-plugin.toml"
            homebrew = root / "homebrew-plugin.toml"
            cargo = root / "Cargo.toml"
            shutil.copy2(REPO_ROOT / "herdr-plugin.toml", canonical)
            shutil.copy2(REPO_ROOT / "packaging/herdr/herdr-plugin.toml", homebrew)
            shutil.copy2(REPO_ROOT / "Cargo.toml", cargo)
            homebrew.write_text(
                homebrew.read_text().replace(
                    'title = "Refresh Tabby Label"',
                    'title = "Refresh a different product"',
                )
            )

            completed = self.run_checker(canonical, homebrew, cargo)

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("product semantics differ", completed.stderr)

    def test_validator_rejects_shared_command_semantic_drift(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            canonical = root / "herdr-plugin.toml"
            homebrew = root / "homebrew-plugin.toml"
            cargo = root / "Cargo.toml"
            shutil.copy2(REPO_ROOT / "herdr-plugin.toml", canonical)
            shutil.copy2(REPO_ROOT / "packaging/herdr/herdr-plugin.toml", homebrew)
            shutil.copy2(REPO_ROOT / "Cargo.toml", cargo)
            for manifest_path in (canonical, homebrew):
                manifest_path.write_text(
                    manifest_path.read_text().replace(
                        ', "refresh"]',
                        ', "status"]',
                    )
                )

            completed = self.run_checker(canonical, homebrew, cargo)

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("action 'refresh' must run ['refresh']", completed.stderr)

    def test_validator_rejects_duplicate_public_entrypoints(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            canonical = root / "herdr-plugin.toml"
            homebrew = root / "homebrew-plugin.toml"
            cargo = root / "Cargo.toml"
            shutil.copy2(REPO_ROOT / "herdr-plugin.toml", canonical)
            shutil.copy2(REPO_ROOT / "packaging/herdr/herdr-plugin.toml", homebrew)
            shutil.copy2(REPO_ROOT / "Cargo.toml", cargo)
            for manifest_path in (canonical, homebrew):
                manifest_path.write_text(
                    manifest_path.read_text()
                    + '\n[[actions]]\nid = "refresh"\ntitle = "Refresh Tabby Label"\n'
                    + 'contexts = ["tab", "workspace"]\n'
                    + f'command = ["{CANONICAL_BINARY if manifest_path == canonical else "../../bin/tabby"}", "refresh"]\n'
                )

            completed = self.run_checker(canonical, homebrew, cargo)

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("action ids must be unique", completed.stderr)


if __name__ == "__main__":
    unittest.main()
