#!/usr/bin/env python3

import json
import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "herdr_lifecycle_harness.py"
SPEC = importlib.util.spec_from_file_location("herdr_lifecycle_harness", HARNESS)
assert SPEC is not None and SPEC.loader is not None
harness = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = harness
SPEC.loader.exec_module(harness)


class HarnessPlanTests(unittest.TestCase):
    def test_ready_parser_allows_explicit_owner_binary_diagnostic(self) -> None:
        output = """Session Runtime: Ready pid=42 version=0.1.11 lease_held=true
Configuration: path=/tmp/config.toml active_schema_version=1 active_source=config.toml latest_error=<none>
Ready owner binary: /opt/tabby/bin/tabby
Session Runtime details: launch_id=launch-1 binary=/opt/tabby/bin/tabby last_evaluation_unix_ms=123 next_periodic_unix_ms=456 last_failure=<none>
"""

        match = harness.READY_RE.search(output)

        self.assertIsNotNone(match)
        assert match is not None
        self.assertEqual(match.group("pid"), "42")
        self.assertEqual(match.group("launch"), "launch-1")
        self.assertEqual(match.group("evaluation"), "123")

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
        self.assertEqual(plan["transcript_schema_version"], 1)
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
        self.assertEqual(
            plan["scenarios"],
            [
                "startup-and-session-isolation",
                "concurrent-hook-coalescing",
                "focus-quiet-and-periodic-cadence",
                "fixed-focus-command-and-cwd-fallback",
                "client-attach-detach",
                "manual-lock-stop-restore",
                "session-policy-profile-selection-and-local-reload",
                "runtime-crash-recovery",
                "registered-binary-activation-and-bidirectional-handoff",
            ],
        )
        self.assertEqual(
            plan["plugin_root_binary"],
            str(REPO_ROOT / ".herdr" / "bin" / "tabby"),
        )
        self.assertEqual(
            plan["prepare_command"],
            [sys.executable, str(REPO_ROOT / "scripts" / "prepare-herdr-plugin.py")],
        )


if __name__ == "__main__":
    unittest.main()
