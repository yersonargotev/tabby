#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS = REPO_ROOT / "scripts" / "herdr_release_harness.py"

sys.path.insert(0, str(REPO_ROOT / "scripts"))
harness_spec = importlib.util.spec_from_file_location("herdr_release_harness", HARNESS)
if harness_spec is None or harness_spec.loader is None:
    raise RuntimeError(f"cannot load release harness from {HARNESS}")
harness = importlib.util.module_from_spec(harness_spec)
harness_spec.loader.exec_module(harness)


class ReleaseHarnessPlanTests(unittest.TestCase):
    def test_ready_status_parser_accepts_configuration_diagnostics(self) -> None:
        status = """Session Runtime: Ready pid=42 version=0.1.16 lease_held=true
Configuration: path=/tmp/config.toml active_schema_version=1 selected_profile=work
Ready owner binary: /tmp/tabby
Session Runtime details: launch_id=release-1 binary=/tmp/tabby last_evaluation_unix_ms=123 next_periodic_unix_ms=456 last_failure=<none>
"""

        match = harness.READY_RE.search(status)

        self.assertIsNotNone(match)
        self.assertEqual(match.group("pid"), "42")
        self.assertEqual(match.group("launch"), "release-1")
        self.assertEqual(match.group("evaluation"), "123")

    def test_registration_accepts_every_current_manifest_action(self) -> None:
        plugin = {
            "version": "0.1.16",
            "actions": [
                {"id": "start", "command": [".herdr/bin/tabby", "start"]},
                {"id": "refresh", "command": [".herdr/bin/tabby", "refresh"]},
                {"id": "config-path", "command": [".herdr/bin/tabby", "config", "path"]},
                {"id": "config-check", "command": [".herdr/bin/tabby", "config", "check"]},
                {"id": "config-reload", "command": [".herdr/bin/tabby", "config", "reload"]},
                {"id": "unlock-focused", "command": [".herdr/bin/tabby", "unlock-focused"]},
                {"id": "unlock-all", "command": [".herdr/bin/tabby", "unlock-all"]},
            ],
            "events": [
                {"on": "pane.focused", "command": [".herdr/bin/tabby", "signal-focus"]},
                {"on": "workspace.created", "command": [".herdr/bin/tabby", "signal-created"]},
                {"on": "tab.created", "command": [".herdr/bin/tabby", "signal-created"]},
            ],
            "startup": [{"command": [".herdr/bin/tabby", "ensure-started"]}],
            "build": [{"command": ["python3", "scripts/install-herdr-plugin.py"]}],
            "source": {
                "kind": "github",
                "owner": "yersonargotev",
                "repo": "tabby",
                "resolved_commit": "release-commit",
            },
        }

        harness.assert_registration(plugin, "0.1.16", "release-commit")

    def test_plan_isolates_native_install_and_covers_the_release_gate(self) -> None:
        sandbox_root = Path("/tmp/tabby-release-harness-contract")
        selected_path = "/operator/bin:/usr/bin:/bin"
        completed = subprocess.run(
            [
                sys.executable,
                str(HARNESS),
                "--plan",
                "--root",
                str(sandbox_root),
                "--expected-herdr-version",
                "0.8.2",
                "--expected-herdr-protocol",
                "20",
            ],
            cwd=REPO_ROOT,
            check=True,
            capture_output=True,
            text=True,
            env={
                "PATH": selected_path,
                "HOME": "/Users/operator",
                "HERDR_BIN_PATH": "/operator/unrelated/herdr",
                "HERDR_SESSION": "operator-session",
                "HERDR_SOCKET_PATH": "/Users/operator/.config/herdr/real.sock",
                "HERDR_PLUGIN_STATE_DIR": "/Users/operator/.local/state/tabby",
            },
        )

        plan = json.loads(completed.stdout)
        self.assertEqual(plan["transcript_schema_version"], 1)
        self.assertEqual(
            plan["expected_herdr_contract"],
            {"version": "0.8.2", "protocol": 20},
        )
        self.assertEqual(plan["repository"], "yersonargotev/tabby")
        self.assertEqual(plan["plugin_id"], "yersonargotev.tabby")
        self.assertEqual(
            plan["install_command"],
            [
                "herdr",
                "plugin",
                "install",
                "yersonargotev/tabby",
                "--ref",
                "v0.1.16",
                "--yes",
            ],
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

        self.assertEqual(environment["PATH"], selected_path)
        self.assertNotIn("HERDR_BIN_PATH", environment)
        self.assertNotIn("HERDR_SOCKET_PATH", environment)
        self.assertNotIn("HERDR_PLUGIN_STATE_DIR", environment)
        self.assertNotIn("HERDR_SESSION", environment)
        self.assertEqual(
            plan["removed_inherited_herdr_variables"],
            [
                "HERDR_BIN_PATH",
                "HERDR_PLUGIN_STATE_DIR",
                "HERDR_SESSION",
                "HERDR_SOCKET_PATH",
            ],
        )


if __name__ == "__main__":
    unittest.main()
