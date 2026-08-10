#!/usr/bin/env python3

import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class HerdrManifestContractTests(unittest.TestCase):
    def test_manifests_match_package_version_and_runtime_contract(self) -> None:
        completed = subprocess.run(
            [sys.executable, "scripts/check-herdr-manifests.py"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("match Cargo package version", completed.stdout)


if __name__ == "__main__":
    unittest.main()
