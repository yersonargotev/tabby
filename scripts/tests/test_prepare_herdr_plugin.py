#!/usr/bin/env python3

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
PREPARE = REPO_ROOT / "scripts" / "prepare-herdr-plugin.py"


class PrepareHerdrPluginTests(unittest.TestCase):
    def test_prepares_the_canonical_binary_from_a_debug_build(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "target" / "debug" / "tabby"
            source.parent.mkdir(parents=True)
            source.write_bytes(b"debug-tabby")
            source.chmod(0o755)

            completed = subprocess.run(
                [
                    sys.executable,
                    str(PREPARE),
                    "--plugin-root",
                    str(root),
                    "--debug-binary",
                    str(source),
                ],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
            )

            prepared = root / ".herdr" / "bin" / "tabby"
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(prepared.read_bytes(), b"debug-tabby")
            self.assertTrue(os.access(prepared, os.X_OK))
            self.assertEqual(
                sorted(path.relative_to(root) for path in root.rglob("*") if path.is_file()),
                [Path(".herdr/bin/tabby"), Path("target/debug/tabby")],
            )

    def test_missing_debug_binary_has_an_actionable_failure(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            completed = subprocess.run(
                [
                    sys.executable,
                    str(PREPARE),
                    "--plugin-root",
                    str(root),
                    "--debug-binary",
                    str(root / "target/debug/tabby"),
                ],
                cwd=REPO_ROOT,
                capture_output=True,
                text=True,
            )

        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("cargo build", completed.stderr)


if __name__ == "__main__":
    unittest.main()
