#!/usr/bin/env python3

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "herdr_release_harness.py"


class ReleaseHarnessPlanTests(unittest.TestCase):
    def test_plan_isolates_native_install_and_covers_the_release_gate(self) -> None:
        sandbox_root = Path("/tmp/tabby-release-harness-contract")
        completed = subprocess.run(
            [
                sys.executable,
                str(HARNESS),
                "--plan",
                "--root",
                str(sandbox_root),
            ],
            cwd=REPO_ROOT,
            check=True,
            capture_output=True,
            text=True,
            env={
                "PATH": "/operator/bin:/usr/bin:/bin",
                "HOME": "/Users/operator",
                "HERDR_SESSION": "operator-session",
                "HERDR_SOCKET_PATH": "/Users/operator/.config/herdr/real.sock",
                "HERDR_PLUGIN_STATE_DIR": "/Users/operator/.local/state/tabby",
            },
        )

        plan = json.loads(completed.stdout)
        self.assertEqual(plan["transcript_schema_version"], 1)
        self.assertEqual(plan["repository"], "yersonargotev/tabby")
        self.assertEqual(plan["plugin_id"], "yersonargotev.tabby")
        self.assertEqual(
            plan["install_command"],
            ["herdr", "plugin", "install", "yersonargotev/tabby", "--yes"],
        )
        self.assertEqual(
            plan["scenarios"],
            [
                "clean-prebuilt-install-without-rust",
                "manifest-registration-and-immediate-activation",
                "manual-refresh-and-manual-lock",
                "reinstall-and-cooperative-runtime-handoff",
                "read-only-runtime-status",
                "client-detach-session-stop-and-session-restore",
                "uninstall-and-retained-session-state",
            ],
        )

        environment = plan["environment"]
        expected_paths = {
            "HOME": sandbox_root / "home",
            "XDG_CONFIG_HOME": sandbox_root / "xdg-config",
            "XDG_STATE_HOME": sandbox_root / "xdg-state",
            "XDG_CACHE_HOME": sandbox_root / "xdg-cache",
            "TMPDIR": sandbox_root / "tmp",
            "HERDR_CONFIG_PATH": sandbox_root / "config" / "herdr" / "config.toml",
        }
        for name, expected in expected_paths.items():
            self.assertEqual(Path(environment[name]), expected)

        self.assertEqual(environment["PATH"], "/operator/bin:/usr/bin:/bin")
        self.assertNotIn("HERDR_SOCKET_PATH", environment)
        self.assertNotIn("HERDR_PLUGIN_STATE_DIR", environment)
        self.assertNotIn("HERDR_SESSION", environment)
        self.assertEqual(
            plan["removed_inherited_herdr_variables"],
            ["HERDR_PLUGIN_STATE_DIR", "HERDR_SESSION", "HERDR_SOCKET_PATH"],
        )


if __name__ == "__main__":
    unittest.main()
