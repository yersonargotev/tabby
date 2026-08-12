#!/usr/bin/env python3
"""Prove a published Tabby release through a clean Herdr-managed install."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, Optional

from herdr_lifecycle_harness import (
    HarnessFailure,
    ReadyRuntime,
    Recorder,
    SessionCase,
    process_exists,
    require_descendant,
    wait_for,
)


REPO_ROOT = Path(__file__).resolve().parents[1]
PLUGIN_ID = "yersonargotev.tabby"
REPOSITORY = "yersonargotev/tabby"
TRANSCRIPT_SCHEMA_VERSION = 1


READY_RE = re.compile(
    r"^Session Runtime: Ready pid=(?P<pid>\d+).*\n"
    r"(?:.*\n)*?"
    r"^Session Runtime details: launch_id=(?P<launch>\S+).*"
    r"last_evaluation_unix_ms=(?P<evaluation>\S+)",
    re.MULTILINE,
)


def build_environment(root: Path) -> Dict[str, str]:
    environment = dict(os.environ)
    for name in [name for name in environment if name.startswith("HERDR_")]:
        environment.pop(name, None)
    environment.update(
        {
            "HOME": str(root / "home"),
            "XDG_CONFIG_HOME": str(root / "xdg-config"),
            "XDG_STATE_HOME": str(root / "xdg-state"),
            "XDG_CACHE_HOME": str(root / "xdg-cache"),
            "TMPDIR": str(root / "tmp"),
            "HERDR_CONFIG_PATH": str(root / "config" / "herdr" / "config.toml"),
        }
    )
    return environment


def plan(root: Path) -> Dict[str, Any]:
    environment = build_environment(root)
    release_tag = f"v{manifest_version()}"
    return {
        "root": str(root),
        "transcript_schema_version": TRANSCRIPT_SCHEMA_VERSION,
        "repository": REPOSITORY,
        "plugin_id": PLUGIN_ID,
        "install_command": [
            "herdr",
            "plugin",
            "install",
            REPOSITORY,
            "--ref",
            release_tag,
            "--yes",
        ],
        "environment": {
            name: environment[name]
            for name in (
                "HOME",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
                "TMPDIR",
                "HERDR_CONFIG_PATH",
                "PATH",
            )
        },
        "removed_inherited_herdr_variables": sorted(
            name for name in os.environ if name.startswith("HERDR_")
        ),
        "scenarios": [
            "clean-prebuilt-install-without-rust",
            "manifest-registration-and-immediate-activation",
            "manual-refresh-and-manual-lock",
            "reinstall-and-cooperative-runtime-handoff",
            "read-only-runtime-status",
            "client-detach-session-stop-and-session-restore",
            "uninstall-and-retained-session-state",
        ],
    }


def manifest_version() -> str:
    for line in (REPO_ROOT / "herdr-plugin.toml").read_text().splitlines():
        match = re.fullmatch(r'version\s*=\s*"([^"]+)"', line.strip())
        if match:
            return match.group(1)
    raise HarnessFailure("root Herdr manifest has no version")


def restricted_prebuilt_path() -> str:
    required = [shutil.which("herdr"), shutil.which("python3"), shutil.which("tmux")]
    if any(path is None for path in required):
        raise HarnessFailure("herdr, python3, and tmux are required for release validation")
    directories = [str(Path(path).resolve().parent) for path in required if path is not None]
    directories.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"])
    unique = list(dict.fromkeys(directories))
    path = os.pathsep.join(unique)
    if shutil.which("cargo", path=path) is not None or shutil.which("rustc", path=path) is not None:
        raise HarnessFailure("the prebuilt validation PATH unexpectedly contains Rust")
    return path


def plugin_records(case: SessionCase, step: str) -> list[Dict[str, Any]]:
    completed = case.herdr(step, "plugin", "list", "--json")
    result = json.loads(completed.stdout).get("result", {})
    plugins = result.get("plugins", [])
    if not isinstance(plugins, list):
        raise HarnessFailure(f"unexpected Herdr plugin list payload: {result}")
    return [plugin for plugin in plugins if isinstance(plugin, dict)]


def installed_plugin(case: SessionCase, step: str) -> Dict[str, Any]:
    matches = [plugin for plugin in plugin_records(case, step) if plugin.get("plugin_id") == PLUGIN_ID]
    if len(matches) != 1:
        raise HarnessFailure(f"expected one registered {PLUGIN_ID} plugin, got {len(matches)}")
    return matches[0]


def command_binary(plugin: Dict[str, Any]) -> Path:
    plugin_root = Path(str(plugin.get("plugin_root", ""))).resolve()
    actions = {
        action.get("id"): action.get("command")
        for action in plugin.get("actions", [])
        if isinstance(action, dict)
    }
    command = actions.get("start")
    if not isinstance(command, list) or len(command) != 2:
        raise HarnessFailure(f"registered start command is invalid: {command}")
    binary = (plugin_root / str(command[0])).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise HarnessFailure(f"registered Tabby binary is not executable: {binary}")
    return binary


def assert_registration(
    plugin: Dict[str, Any], expected_version: str, expected_commit: str
) -> None:
    expected_actions = {
        "start": [".herdr/bin/tabby", "start"],
        "refresh": [".herdr/bin/tabby", "refresh"],
        "config-path": [".herdr/bin/tabby", "config", "path"],
        "config-check": [".herdr/bin/tabby", "config", "check"],
        "config-reload": [".herdr/bin/tabby", "config", "reload"],
        "unlock-focused": [".herdr/bin/tabby", "unlock-focused"],
        "unlock-all": [".herdr/bin/tabby", "unlock-all"],
    }
    actions = {
        action.get("id"): action.get("command")
        for action in plugin.get("actions", [])
        if isinstance(action, dict)
    }
    events = {
        event.get("on"): event.get("command")
        for event in plugin.get("events", [])
        if isinstance(event, dict)
    }
    expected_events = {
        "pane.focused": [".herdr/bin/tabby", "signal-focus"],
        "workspace.created": [".herdr/bin/tabby", "signal-created"],
        "tab.created": [".herdr/bin/tabby", "signal-created"],
    }
    expected_startup = [{"command": [".herdr/bin/tabby", "ensure-started"]}]
    expected_build = [{"command": ["python3", "scripts/install-herdr-plugin.py"]}]
    source = plugin.get("source", {})
    if plugin.get("version") != expected_version:
        raise HarnessFailure(f"registered version drifted: {plugin.get('version')!r}")
    if actions != expected_actions or events != expected_events:
        raise HarnessFailure("registered actions or events differ from the release manifest")
    if plugin.get("startup") != expected_startup:
        raise HarnessFailure(f"registered startup hook differs: {plugin.get('startup')}")
    if plugin.get("build") != expected_build:
        raise HarnessFailure(f"registered build command differs: {plugin.get('build')}")
    if (
        not isinstance(source, dict)
        or source.get("kind") != "github"
        or source.get("owner") != "yersonargotev"
        or source.get("repo") != "tabby"
        or source.get("resolved_commit") != expected_commit
    ):
        raise HarnessFailure(f"plugin does not match the pinned GitHub release: {source}")


def binary_environment(case: SessionCase) -> Dict[str, str]:
    if case.socket_path is None:
        raise HarnessFailure("Herdr Session socket has not been resolved")
    environment = dict(case.environment)
    environment["HERDR_SOCKET_PATH"] = case.socket_path
    return environment


def run_binary(
    case: SessionCase,
    binary: Path,
    step: str,
    *arguments: str,
) -> subprocess.CompletedProcess[str]:
    return case.run(
        step,
        [str(binary), *arguments],
        environment=binary_environment(case),
    )


def ready_runtime(case: SessionCase, binary: Path) -> ReadyRuntime:
    completed = subprocess.run(
        [str(binary), "status"],
        cwd=REPO_ROOT,
        env=binary_environment(case),
        capture_output=True,
        text=True,
    )
    if completed.returncode != 0:
        raise HarnessFailure(completed.stderr.strip())
    match = READY_RE.search(completed.stdout)
    if match is None:
        raise HarnessFailure(f"runtime is not Ready: {completed.stdout.strip()}")
    evaluation = match.group("evaluation")
    return ReadyRuntime(
        pid=int(match.group("pid")),
        launch_id=match.group("launch"),
        last_evaluation_unix_ms=None if evaluation == "<none>" else int(evaluation),
    )


def wait_ready(
    case: SessionCase,
    binary: Path,
    step: str,
    previous_launch: Optional[str] = None,
) -> ReadyRuntime:
    runtime = wait_for(
        "released Ready Session Runtime",
        10.0,
        lambda: ready_if_new(case, binary, previous_launch),
    )
    run_binary(case, binary, step, "status")
    return runtime


def ready_if_new(
    case: SessionCase,
    binary: Path,
    previous_launch: Optional[str],
) -> Optional[ReadyRuntime]:
    try:
        runtime = ready_runtime(case, binary)
    except (HarnessFailure, OSError):
        return None
    return runtime if previous_launch is None or runtime.launch_id != previous_launch else None


def runtime_after_evaluation(
    case: SessionCase,
    binary: Path,
    previous: Optional[int],
) -> Optional[ReadyRuntime]:
    runtime = ready_runtime(case, binary)
    current = runtime.last_evaluation_unix_ms
    if current is None or current == previous:
        return None
    return runtime


def observe_handoff(
    case: SessionCase,
    replacement_binary: Path,
    prior_owner: ReadyRuntime,
) -> ReadyRuntime:
    deadline = time.monotonic() + 10.0
    prior_exit_ms: Optional[int] = None
    replacement_ready_ms: Optional[int] = None
    replacement: Optional[ReadyRuntime] = None
    started = time.monotonic()
    while time.monotonic() < deadline:
        prior_exists = process_exists(prior_owner.pid)
        if not prior_exists and prior_exit_ms is None:
            prior_exit_ms = int((time.monotonic() - started) * 1000)
        try:
            observed = ready_runtime(case, replacement_binary)
        except (HarnessFailure, OSError):
            observed = None
        if observed is not None and observed.launch_id != prior_owner.launch_id:
            if prior_exists:
                raise HarnessFailure(
                    "replacement became Ready while the prior owner still existed"
                )
            replacement = observed
            replacement_ready_ms = int((time.monotonic() - started) * 1000)
            break
        time.sleep(0.01)
    if replacement is None or prior_exit_ms is None or replacement_ready_ms is None:
        raise HarnessFailure("timed out observing ordered Cooperative Runtime Handoff")
    if replacement.pid == prior_owner.pid:
        raise HarnessFailure("reinstall activation reused the prior owner PID")
    case.recorder.assertion(
        case.name,
        "cooperative-runtime-handoff-ordering",
        f"prior owner pid={prior_owner.pid} exit observed at +{prior_exit_ms} ms; "
        f"replacement pid={replacement.pid} Ready observed at +{replacement_ready_ms} ms; "
        "no sample observed overlapping owners",
    )
    run_binary(case, replacement_binary, "ready-after-reinstall", "status")
    return replacement


def state_snapshot(state_root: Path) -> Dict[str, str]:
    if not state_root.is_dir():
        raise HarnessFailure(f"Session-Scoped Tab State is missing: {state_root}")
    return {
        str(path.relative_to(state_root)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sorted(state_root.rglob("*"))
        if path.is_file()
    }


def exercise_client_detach(case: SessionCase, binary: Path, owner: ReadyRuntime) -> ReadyRuntime:
    tmux_server = f"tabby-release-harness-{os.getpid()}"
    visible_environment = {
        name: value
        for name, value in case.environment.items()
        if name
        in {
            "HOME",
            "PATH",
            "TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "HERDR_CONFIG_PATH",
        }
    }
    attach = [
        "tmux",
        "-L",
        tmux_server,
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "client",
        "env",
        *[f"{name}={value}" for name, value in sorted(visible_environment.items())],
        "herdr",
    ]
    case.run("client-attach", attach)
    time.sleep(0.5)
    case.run("client-attached", ["tmux", "-L", tmux_server, "has-session", "-t", "client"])
    before = ready_runtime(case, binary)
    case.run("client-detach", ["tmux", "-L", tmux_server, "kill-session", "-t", "client"])
    after = wait_for(
        "post-detach periodic evaluation",
        8.0,
        lambda: runtime_after_evaluation(case, binary, before.last_evaluation_unix_ms),
    )
    if (after.pid, after.launch_id) != (owner.pid, owner.launch_id):
        raise HarnessFailure("Client Detach replaced the Ready owner")
    case.recorder.assertion(
        case.name,
        "client-detach",
        "the attached client exited and the same Ready owner continued periodic freshness",
    )
    return after


def run_live(output: Path) -> None:
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise HarnessFailure("release validation requires Apple Silicon macOS")
    herdr_version = subprocess.run(
        ["herdr", "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    if herdr_version != "herdr 0.8.0":
        raise HarnessFailure(f"expected herdr 0.8.0, got {herdr_version!r}")

    expected_version = manifest_version()
    release_tag = f"v{expected_version}"
    expected_commit = subprocess.run(
        ["git", "rev-list", "-n", "1", release_tag],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if not expected_commit:
        raise HarnessFailure(f"release tag does not resolve locally: {release_tag}")
    root = Path(tempfile.mkdtemp(prefix="tabby-release-harness.", dir="/tmp"))
    recorder = Recorder(root)
    environment = build_environment(root)
    environment["PATH"] = restricted_prebuilt_path()
    case = SessionCase("release", [], environment, recorder)
    state_root = root / "xdg-state" / "herdr" / "plugins" / PLUGIN_ID / "session-tab-state"
    managed_root: Optional[Path] = None
    try:
        for path in (
            root / "home",
            root / "xdg-config",
            root / "xdg-state",
            root / "xdg-cache",
            root / "tmp",
            root / "config" / "herdr",
            root / "manual-refresh-target",
        ):
            require_descendant(path, root)
            path.mkdir(parents=True, exist_ok=True)
        Path(environment["HERDR_CONFIG_PATH"]).touch()
        recorder.assertion(
            "release",
            "isolated-prebuilt-environment",
            f"Apple Silicon macOS; {herdr_version}; PATH excludes cargo and rustc; expected release v{expected_version}",
        )

        case.start_server("clean-session-start")
        case.herdr(
            "clean-native-install",
            "plugin",
            "install",
            REPOSITORY,
            "--ref",
            release_tag,
            "--yes",
        )
        plugin = installed_plugin(case, "inspect-registration")
        assert_registration(plugin, expected_version, expected_commit)
        managed_root = Path(str(plugin["plugin_root"])).resolve()
        binary = command_binary(plugin)
        recorder.assertion(
            "release",
            "native-registration",
            "GitHub-managed manifest registered the production root, build, startup, events, actions, and canonical command",
        )

        case.herdr(
            "immediate-activation",
            "plugin",
            "action",
            "invoke",
            "start",
            "--plugin",
            PLUGIN_ID,
        )
        owner = wait_ready(case, binary, "ready-after-install")
        recorder.assertion(
            "release",
            "immediate-activation",
            "the explicit start action confirmed one Ready owner running the registered release binary",
        )

        created = case.herdr(
            "create-refresh-target",
            "workspace",
            "create",
            "--cwd",
            str(root / "manual-refresh-target"),
            "--focus",
        )
        tab_id = json.loads(created.stdout)["result"]["tab"]["tab_id"]
        case.herdr(
            "manual-refresh",
            "plugin",
            "action",
            "invoke",
            "refresh",
            "--plugin",
            PLUGIN_ID,
        )
        case.wait_for_label("manual-refresh-target")
        case.herdr("apply-manual-lock", "tab", "rename", tab_id, "release-manual-lock")
        time.sleep(6.0)
        if case.focused_tab_label() != "release-manual-lock":
            raise HarnessFailure("manual lock did not preserve the visible label")
        status = run_binary(case, binary, "manual-lock-status", "status").stdout
        if "1 Manually Locked Tabs" not in status:
            raise HarnessFailure("manual lock was not persisted")
        recorder.assertion(
            "release",
            "manual-refresh-and-lock",
            "manual refresh applied the Working Directory Suffix and later preserved manual intent",
        )

        prior_owner = ready_runtime(case, binary)
        case.herdr(
            "reinstall-native-plugin",
            "plugin",
            "install",
            REPOSITORY,
            "--ref",
            release_tag,
            "--yes",
        )
        replacement_plugin = installed_plugin(case, "inspect-reinstalled-registration")
        assert_registration(replacement_plugin, expected_version, expected_commit)
        replacement_binary = command_binary(replacement_plugin)
        case.herdr(
            "activate-reinstalled-plugin",
            "plugin",
            "action",
            "invoke",
            "start",
            "--plugin",
            PLUGIN_ID,
        )
        replacement = observe_handoff(case, replacement_binary, prior_owner)
        recorder.assertion(
            "release",
            "cooperative-runtime-handoff",
            "reinstall registered a new binary identity; observed prior-owner exit preceded replacement readiness",
        )
        binary = replacement_binary
        owner = replacement

        before_status = state_snapshot(state_root)
        first_status = run_binary(case, binary, "runtime-status-first", "status").stdout
        second_status = run_binary(case, binary, "runtime-status-second", "status").stdout
        after_status = state_snapshot(state_root)
        if "Warnings: none" not in first_status or "Warnings: none" not in second_status:
            raise HarnessFailure("Runtime Status contains a distribution mismatch or warning")
        if before_status != after_status:
            raise HarnessFailure("Runtime Status mutated Session-Scoped Tab State")
        if ready_runtime(case, binary).launch_id != owner.launch_id:
            raise HarnessFailure("Runtime Status changed the Ready owner")
        recorder.assertion(
            "release",
            "read-only-runtime-status",
            "two status reads reported no mismatch and changed neither the owner nor Session-Scoped Tab State",
        )

        owner = exercise_client_detach(case, binary, owner)
        stopped_owner = owner
        case.stop_server("session-stop")
        wait_for("Session Runtime exit", 5.0, lambda: not process_exists(stopped_owner.pid))
        retained_at_stop = state_snapshot(state_root)
        case.start_server("session-restore")
        owner = wait_ready(case, binary, "ready-after-restore", stopped_owner.launch_id)
        if case.focused_tab_label() != "release-manual-lock":
            raise HarnessFailure("Session Restore did not retain the manual label")
        restored_status = run_binary(case, binary, "status-after-restore", "status").stdout
        if "1 Manually Locked Tabs" not in restored_status:
            raise HarnessFailure("Session Restore did not retain the manual lock")
        if state_snapshot(state_root) != retained_at_stop:
            raise HarnessFailure("Session Restore unexpectedly rewrote retained tab state")
        recorder.assertion(
            "release",
            "session-stop-and-restore",
            "Session Stop ended the owner; Session Restore started a new owner and retained manual intent",
        )

        case.herdr("uninstall-native-plugin", "plugin", "uninstall", PLUGIN_ID)
        if any(plugin.get("plugin_id") == PLUGIN_ID for plugin in plugin_records(case, "registration-after-uninstall")):
            raise HarnessFailure("uninstall left the GitHub-managed plugin registered")
        retained_after_uninstall = state_snapshot(state_root)
        if retained_after_uninstall != retained_at_stop:
            raise HarnessFailure("uninstall changed retained Session-Scoped Tab State")
        if managed_root is not None and (managed_root == state_root or managed_root in state_root.parents):
            raise HarnessFailure("managed source root contains durable Session-Scoped Tab State")
        recorder.assertion(
            "release",
            "uninstall-and-retained-state",
            "uninstall removed registration while retained Session-Scoped Tab State remained outside the managed source root",
        )
    finally:
        try:
            if case.status_json().get("server", {}).get("running"):
                case.stop_server("cleanup-stop")
        except Exception as error:
            recorder.records.append(
                {
                    "schema_version": TRANSCRIPT_SCHEMA_VERSION,
                    "case": "release",
                    "step": "cleanup-stop",
                    "error": str(error),
                }
            )
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            "".join(json.dumps(record, sort_keys=True) + "\n" for record in recorder.records)
        )
        require_descendant(root, Path("/tmp"))
        shutil.rmtree(root)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", action="store_true", help="print the isolated plan without running it")
    parser.add_argument("--root", type=Path, help="sandbox root used only by --plan")
    parser.add_argument(
        "--output",
        type=Path,
        default=REPO_ROOT / ".scratch" / "herdr-release-transcript.jsonl",
        help="JSONL transcript destination for a live run",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.plan:
            root = args.root or Path("/tmp/tabby-release-harness.PLAN")
            print(json.dumps(plan(root), indent=2, sort_keys=True))
            return 0
        if args.root is not None:
            raise HarnessFailure("--root is accepted only with --plan")
        run_live(args.output.resolve())
        print(f"Herdr release harness passed; transcript: {args.output.resolve()}")
        return 0
    except (HarnessFailure, OSError, subprocess.SubprocessError) as error:
        print(f"Herdr release harness failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
