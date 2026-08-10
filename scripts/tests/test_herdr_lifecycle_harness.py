#!/usr/bin/env python3

import json
import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "herdr_lifecycle_harness.py"


class HarnessPlanTests(unittest.TestCase):
    def test_plan_isolates_default_and_named_sessions(self) -> None:
        sandbox_root = Path("/tmp/tabby-harness-contract")
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
                "PATH": "/usr/bin:/bin",
                "HOME": "/Users/operator",
                "HERDR_SESSION": "operator-session",
                "HERDR_SOCKET_PATH": "/Users/operator/.config/herdr/real.sock",
                "HERDR_PLUGIN_STATE_DIR": "/Users/operator/.local/state/tabby",
            },
        )

        plan = json.loads(completed.stdout)
        self.assertEqual([case["name"] for case in plan["cases"]], ["default", "named"])
        self.assertEqual(plan["cases"][0]["herdr_session_args"], [])
        self.assertEqual(
            plan["cases"][1]["herdr_session_args"],
            ["--session", "tabby-lifecycle-named"],
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

        self.assertNotIn("HERDR_SOCKET_PATH", environment)
        self.assertNotIn("HERDR_PLUGIN_STATE_DIR", environment)
        self.assertNotIn("HERDR_SESSION", environment)
        self.assertEqual(
            plan["removed_inherited_herdr_variables"],
            ["HERDR_PLUGIN_STATE_DIR", "HERDR_SESSION", "HERDR_SOCKET_PATH"],
        )


if __name__ == "__main__":
    unittest.main()
