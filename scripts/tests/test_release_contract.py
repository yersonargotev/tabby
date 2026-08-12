#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CHECKER = REPO_ROOT / "scripts" / "check-release-contract.py"
ASSET = "tabby-aarch64-apple-darwin.tar.xz"
CHECKSUM = f"{ASSET}.sha256"


def plan(version: str = "0.1.12") -> dict[str, object]:
    return {
        "announcement_tag": f"v{version}",
        "releases": [
            {
                "app_name": "tabby",
                "app_version": version,
                "artifacts": [ASSET, CHECKSUM],
            }
        ],
        "artifacts": {
            ASSET: {
                "name": ASSET,
                "kind": "executable-zip",
                "target_triples": ["aarch64-apple-darwin"],
                "checksum": CHECKSUM,
            },
            CHECKSUM: {
                "name": CHECKSUM,
                "kind": "checksum",
                "target_triples": ["aarch64-apple-darwin"],
            },
        },
        "ci": {"github": {"pr_run_mode": "upload"}},
    }


class ReleaseContractTests(unittest.TestCase):
    def run_checker(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(CHECKER), *arguments],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )

    def write_plan(self, root: Path, value: dict[str, object]) -> Path:
        path = root / "plan.json"
        path.write_text(json.dumps(value))
        return path

    def test_accepts_the_production_dist_plan_and_release_tag(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            path = self.write_plan(Path(temp_dir), plan())

            completed = self.run_checker("--dist-manifest", str(path), "--tag", "v0.1.12")

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("release plan matches Tabby 0.1.12", completed.stdout)

    def test_rejects_version_tag_artifact_and_pr_build_drift(self) -> None:
        cases = {
            "release version": ("app_version", "0.1.10"),
            "archive": ("remove_release_artifact", ASSET),
            "checksum sidecar": ("remove_release_artifact", CHECKSUM),
            "checksum relationship": ("checksum", "renamed.sha256"),
            "target": ("target", "x86_64-apple-darwin"),
            "PR build mode": ("pr_run_mode", "plan"),
        }
        for diagnostic, (mutation, value) in cases.items():
            with self.subTest(diagnostic=diagnostic), tempfile.TemporaryDirectory() as temp_dir:
                value_plan = plan()
                if mutation == "app_version":
                    value_plan["releases"][0]["app_version"] = value
                elif mutation == "remove_release_artifact":
                    value_plan["releases"][0]["artifacts"].remove(value)
                elif mutation == "checksum":
                    value_plan["artifacts"][ASSET]["checksum"] = value
                elif mutation == "target":
                    value_plan["artifacts"][ASSET]["target_triples"] = [value]
                else:
                    value_plan["ci"]["github"]["pr_run_mode"] = value
                path = self.write_plan(Path(temp_dir), value_plan)

                completed = self.run_checker("--dist-manifest", str(path))

                self.assertNotEqual(completed.returncode, 0)
                self.assertIn(diagnostic, completed.stderr)

        with tempfile.TemporaryDirectory() as temp_dir:
            path = self.write_plan(Path(temp_dir), plan())
            completed = self.run_checker("--dist-manifest", str(path), "--tag", "0.1.12")
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("Git tag", completed.stderr)

    def test_validates_the_built_archive_checksum_contract(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            artifacts = Path(temp_dir)
            archive = artifacts / ASSET
            archive.write_bytes(b"release archive")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            (artifacts / CHECKSUM).write_text(f"{digest} *{ASSET}\n")

            completed = self.run_checker("--artifact-dir", str(artifacts))

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("checksum matches", completed.stdout)

    def test_rejects_malformed_mismatched_or_missing_built_checksums(self) -> None:
        cases = {
            "missing": None,
            "malformed": "not-a-checksum\n",
            "different asset": f"{'0' * 64} *other.tar.xz\n",
            "digest mismatch": f"{'0' * 64} *{ASSET}\n",
        }
        for diagnostic, checksum in cases.items():
            with self.subTest(diagnostic=diagnostic), tempfile.TemporaryDirectory() as temp_dir:
                artifacts = Path(temp_dir)
                (artifacts / ASSET).write_bytes(b"release archive")
                if checksum is not None:
                    (artifacts / CHECKSUM).write_text(checksum)

                completed = self.run_checker("--artifact-dir", str(artifacts))

                self.assertNotEqual(completed.returncode, 0)
                self.assertIn(diagnostic, completed.stderr)

    def test_release_workflow_gates_plan_and_built_artifacts(self) -> None:
        workflow = (REPO_ROOT / ".github/workflows/release.yml").read_text()

        self.assertIn(
            "check-release-contract.py --artifact-dir target/distrib",
            workflow,
        )
        self.assertIn("--dist-manifest plan-dist-manifest.json", workflow)
        self.assertIn('if [[ -n "$RELEASE_TAG" ]]', workflow)
        self.assertIn("needs.plan.outputs.publishing == 'true'", workflow)

    def test_rejects_a_homebrew_postinstall_hook(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            artifacts = Path(temp_dir)
            archive = artifacts / ASSET
            archive.write_bytes(b"release archive")
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            (artifacts / CHECKSUM).write_text(f"{digest} *{ASSET}\n")
            (artifacts / "tabby.rb").write_text(
                'class Tabby < Formula\n  def post_install\n    system "tabby", "install"\n  end\nend\n'
            )

            completed = self.run_checker(
                "--artifact-dir",
                str(artifacts),
                "--require-homebrew-formula",
            )

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("Homebrew postinstall", completed.stderr)


if __name__ == "__main__":
    unittest.main()
