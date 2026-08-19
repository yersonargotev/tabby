#!/usr/bin/env python3
"""Run Tabby's Herdr 0.8 lifecycle contract in an isolated macOS sandbox."""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime as dt
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Sequence


REPO_ROOT = Path(__file__).resolve().parents[1]
DEBUG_TABBY = REPO_ROOT / "target" / "debug" / "tabby"
TABBY = REPO_ROOT / ".herdr" / "bin" / "tabby"
PREPARE_COMMAND = [sys.executable, str(REPO_ROOT / "scripts" / "prepare-herdr-plugin.py")]
PLUGIN_ID = "yersonargotev.tabby"
TRANSCRIPT_SCHEMA_VERSION = 1
READY_RE = re.compile(
    r"Session Runtime: Ready pid=(?P<pid>\d+).*\n"
    r"(?:Configuration:.*\n)?"
    r"(?:Ready owner binary:.*\n)?"
    r"Session Runtime details: launch_id=(?P<launch>\S+).*"
    r"last_evaluation_unix_ms=(?P<evaluation>\S+)"
)


class HarnessFailure(RuntimeError):
    pass


@dataclass(frozen=True)
class ExpectedHerdrContract:
    version: str
    protocol: int

    def as_dict(self) -> Dict[str, Any]:
        return {"version": self.version, "protocol": self.protocol}


def add_expected_herdr_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--expected-herdr-version",
        default="0.8.0",
        help="exact Herdr server version required by this run",
    )
    parser.add_argument(
        "--expected-herdr-protocol",
        type=int,
        default=19,
        help="exact Herdr server protocol required by this run",
    )


def expected_herdr_contract(args: argparse.Namespace) -> ExpectedHerdrContract:
    if not args.expected_herdr_version or args.expected_herdr_protocol < 1:
        raise HarnessFailure("the expected Herdr version and protocol must be positive")
    return ExpectedHerdrContract(
        version=args.expected_herdr_version,
        protocol=args.expected_herdr_protocol,
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


def plan(root: Path, expected_herdr: ExpectedHerdrContract) -> Dict[str, Any]:
    environment = build_environment(root)
    visible_environment = {
        name: environment[name]
        for name in (
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "TMPDIR",
            "HERDR_CONFIG_PATH",
        )
    }
    return {
        "root": str(root),
        "transcript_schema_version": TRANSCRIPT_SCHEMA_VERSION,
        "expected_herdr_contract": expected_herdr.as_dict(),
        "plugin_root_binary": str(TABBY),
        "prepare_command": PREPARE_COMMAND,
        "environment": visible_environment,
        "removed_inherited_herdr_variables": sorted(
            name for name in os.environ if name.startswith("HERDR_")
        ),
        "cases": [
            {"name": "default", "herdr_session_args": []},
            {
                "name": "named",
                "herdr_session_args": ["--session", "tabby-lifecycle-named"],
            },
        ],
        "scenarios": [
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
    }


def require_descendant(path: Path, root: Path) -> None:
    try:
        path.resolve().relative_to(root.resolve())
    except ValueError as error:
        raise HarnessFailure(f"refusing path outside harness root: {path}") from error


class Recorder:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.records: List[Dict[str, Any]] = []

    def sanitize(self, value: str) -> str:
        if value.startswith("PATH="):
            return "PATH=<inherited>"
        return value.replace(str(self.root), "<sandbox>").replace(str(REPO_ROOT), "<repo>")

    def add(
        self,
        case: str,
        step: str,
        argv: Sequence[str],
        completed: subprocess.CompletedProcess[str],
        elapsed_ms: int,
    ) -> None:
        self.records.append(
            {
                "schema_version": TRANSCRIPT_SCHEMA_VERSION,
                "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "case": case,
                "step": step,
                "argv": [self.sanitize(part) for part in argv],
                "exit": completed.returncode,
                "elapsed_ms": elapsed_ms,
                "stdout": self.sanitize(completed.stdout.strip()),
                "stderr": self.sanitize(completed.stderr.strip()),
            }
        )

    def assertion(self, case: str, step: str, detail: str) -> None:
        self.records.append(
            {
                "schema_version": TRANSCRIPT_SCHEMA_VERSION,
                "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "case": case,
                "step": step,
                "assertion": self.sanitize(detail),
                "result": "passed",
            }
        )


@dataclass(frozen=True)
class ReadyRuntime:
    pid: int
    launch_id: str
    last_evaluation_unix_ms: Optional[int]


class SessionCase:
    def __init__(
        self,
        name: str,
        session_args: Sequence[str],
        environment: Dict[str, str],
        recorder: Recorder,
        expected_herdr: ExpectedHerdrContract,
    ) -> None:
        self.name = name
        self.session_args = list(session_args)
        self.environment = environment
        self.recorder = recorder
        self.expected_herdr = expected_herdr
        self.server_process: Optional[subprocess.Popen[str]] = None
        self.socket_path: Optional[str] = None

    def herdr_argv(self, *args: str) -> List[str]:
        return ["herdr", *self.session_args, *args]

    def run(
        self,
        step: str,
        argv: Sequence[str],
        *,
        check: bool = True,
        environment: Optional[Dict[str, str]] = None,
    ) -> subprocess.CompletedProcess[str]:
        started = time.monotonic()
        completed = subprocess.run(
            list(argv),
            cwd=REPO_ROOT,
            env=environment or self.environment,
            capture_output=True,
            text=True,
        )
        self.recorder.add(
            self.name,
            step,
            argv,
            completed,
            int((time.monotonic() - started) * 1000),
        )
        if check and completed.returncode != 0:
            raise HarnessFailure(
                f"{self.name}/{step} failed ({completed.returncode}): "
                f"{completed.stderr.strip()}"
            )
        return completed

    def herdr(self, step: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return self.run(step, self.herdr_argv(*args), check=check)

    def snapshot(self, step: str) -> Dict[str, Any]:
        completed = self.herdr(step, "api", "snapshot")
        return json.loads(completed.stdout)["result"]["snapshot"]

    def focused_tab_label(self) -> Optional[str]:
        completed = subprocess.run(
            self.herdr_argv("api", "snapshot"),
            cwd=REPO_ROOT,
            env=self.environment,
            capture_output=True,
            text=True,
            check=True,
        )
        snapshot = json.loads(completed.stdout)["result"]["snapshot"]
        focused_tab_id = snapshot.get("focused_tab_id")
        return next(
            (
                tab.get("label")
                for tab in snapshot.get("tabs", [])
                if tab.get("tab_id") == focused_tab_id
            ),
            None,
        )

    def wait_for_label(self, label: str, timeout: float = 12.0) -> None:
        wait_for(
            f"{self.name} focused tab label {label!r}",
            timeout,
            lambda: self.focused_tab_label() == label,
        )
        self.snapshot(f"label-{label}")

    def start_server(self, step: str) -> None:
        if self.server_process is not None and self.server_process.poll() is None:
            raise HarnessFailure(f"{self.name}: server already running")
        self.server_process = subprocess.Popen(
            self.herdr_argv("server"),
            cwd=REPO_ROOT,
            env=self.environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        status = wait_for(
            f"{self.name} Herdr server readiness",
            8.0,
            self.running_status,
        )
        server = status["server"]
        if (
            server.get("version") != self.expected_herdr.version
            or server.get("protocol") != self.expected_herdr.protocol
        ):
            raise HarnessFailure(f"{self.name}: unexpected Herdr contract: {server}")
        self.socket_path = server["socket"]
        self.recorder.assertion(
            self.name,
            step,
            f"Herdr {self.expected_herdr.version} protocol "
            f"{self.expected_herdr.protocol} ready at {self.socket_path}",
        )

    def status_json(self) -> Dict[str, Any]:
        completed = subprocess.run(
            self.herdr_argv("status", "--json"),
            cwd=REPO_ROOT,
            env=self.environment,
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(completed.stdout)

    def running_status(self) -> Optional[Dict[str, Any]]:
        status = self.status_json()
        return status if status.get("server", {}).get("running") else None

    def stop_server(self, step: str) -> None:
        self.herdr(step, "server", "stop", check=False)
        wait_for(
            f"{self.name} Herdr server stop",
            8.0,
            lambda: not self.status_json().get("server", {}).get("running"),
        )
        if self.server_process is not None:
            self.server_process.wait(timeout=5)
            self.server_process = None
        self.recorder.assertion(self.name, step, "Herdr socket listener stopped")

    def tabby_environment(self) -> Dict[str, str]:
        if self.socket_path is None:
            raise HarnessFailure(f"{self.name}: socket has not been resolved")
        environment = dict(self.environment)
        environment["HERDR_SOCKET_PATH"] = self.socket_path
        return environment

    def tabby(self, step: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return self.run(
            step,
            [str(TABBY), *args],
            check=check,
            environment=self.tabby_environment(),
        )

    def ready_runtime(self) -> ReadyRuntime:
        completed = subprocess.run(
            [str(TABBY), "status"],
            cwd=REPO_ROOT,
            env=self.tabby_environment(),
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            raise HarnessFailure(completed.stderr.strip())
        match = READY_RE.search(completed.stdout)
        if not match:
            raise HarnessFailure(f"runtime is not Ready: {completed.stdout.strip()}")
        evaluation = match.group("evaluation")
        return ReadyRuntime(
            pid=int(match.group("pid")),
            launch_id=match.group("launch"),
            last_evaluation_unix_ms=None if evaluation == "<none>" else int(evaluation),
        )

    def tabby_status_text(self) -> str:
        completed = subprocess.run(
            [str(TABBY), "status"],
            cwd=REPO_ROOT,
            env=self.tabby_environment(),
            capture_output=True,
            text=True,
            check=True,
        )
        return completed.stdout

    def wait_ready(self, step: str, previous_launch: Optional[str] = None) -> ReadyRuntime:
        runtime = wait_for(
            f"{self.name} Ready Session Runtime",
            8.0,
            lambda: ready_if_new(self, previous_launch),
        )
        self.tabby(step, "status")
        return runtime


def ready_if_new(case: SessionCase, previous_launch: Optional[str]) -> Optional[ReadyRuntime]:
    try:
        runtime = case.ready_runtime()
    except OSError:
        return None
    return runtime if previous_launch is None or runtime.launch_id != previous_launch else None


def wait_for(description: str, timeout: float, probe: Callable[[], Any]) -> Any:
    deadline = time.monotonic() + timeout
    last_error: Optional[Exception] = None
    while time.monotonic() < deadline:
        try:
            value = probe()
            if value:
                return value
        except (HarnessFailure, json.JSONDecodeError, OSError, subprocess.SubprocessError) as error:
            last_error = error
        time.sleep(0.05)
    suffix = f": {last_error}" if last_error else ""
    raise HarnessFailure(f"timed out waiting for {description}{suffix}")


def process_exists(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def exercise_trigger_burst(case: SessionCase, owner: ReadyRuntime) -> ReadyRuntime:
    commands = ["ensure-started", "signal-created", "refresh", "signal-focus"] * 4
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        futures = [executor.submit(case.tabby, f"burst-{index}", command) for index, command in enumerate(commands)]
        for future in futures:
            future.result()
    after = case.ready_runtime()
    if (after.pid, after.launch_id) != (owner.pid, owner.launch_id):
        raise HarnessFailure(f"{case.name}: trigger burst replaced the Ready owner")
    case.recorder.assertion(
        case.name,
        "concurrent-trigger-burst",
        "16 startup/create/manual/focus hooks coalesced behind one Ready owner",
    )
    return after


def exercise_quiet_and_periodic(case: SessionCase) -> ReadyRuntime:
    owner = wait_for(
        f"{case.name} initial evaluation",
        5.0,
        lambda: runtime_with_evaluation(case),
    )
    case.tabby("focus-quiet-trigger", "signal-focus")
    immediately_after = case.ready_runtime()
    time.sleep(0.75)
    during_quiet = case.ready_runtime()
    if during_quiet.last_evaluation_unix_ms != immediately_after.last_evaluation_unix_ms:
        raise HarnessFailure(f"{case.name}: evaluation occurred inside the Focus Quiet Window")
    after_quiet = wait_for(
        f"{case.name} post-quiet evaluation",
        4.0,
        lambda: runtime_after_evaluation(case, during_quiet.last_evaluation_unix_ms),
    )
    if (after_quiet.pid, after_quiet.launch_id) != (owner.pid, owner.launch_id):
        raise HarnessFailure(f"{case.name}: owner changed during focus evaluation")
    periodic = wait_for(
        f"{case.name} five-second periodic evaluation",
        8.0,
        lambda: runtime_after_evaluation(case, after_quiet.last_evaluation_unix_ms),
    )
    if (periodic.pid, periodic.launch_id) != (owner.pid, owner.launch_id):
        raise HarnessFailure(f"{case.name}: owner changed during periodic evaluation")
    case.recorder.assertion(
        case.name,
        "quiet-and-periodic",
        "no evaluation during 750 ms of quiet; evaluation followed quiet and repeated on idle cadence",
    )
    return periodic


def runtime_with_evaluation(case: SessionCase) -> Optional[ReadyRuntime]:
    runtime = case.ready_runtime()
    return runtime if runtime.last_evaluation_unix_ms is not None else None


def runtime_after_evaluation(case: SessionCase, previous: Optional[int]) -> Optional[ReadyRuntime]:
    runtime = case.ready_runtime()
    return (
        runtime
        if runtime.last_evaluation_unix_ms is not None
        and runtime.last_evaluation_unix_ms != previous
        else None
    )


def exercise_focused_process_and_manual_lock(
    case: SessionCase, owner: ReadyRuntime
) -> str:
    created = case.herdr(
        "create-focused-workspace",
        "workspace",
        "create",
        "--cwd",
        str(REPO_ROOT),
        "--focus",
    )
    result = json.loads(created.stdout)["result"]
    pane_id = result["root_pane"]["pane_id"]
    tab_id = result["tab"]["tab_id"]

    expected_actions = {
        "start": [".herdr/bin/tabby", "start"],
        "refresh": [".herdr/bin/tabby", "refresh"],
        "unlock-focused": [".herdr/bin/tabby", "unlock-focused"],
        "unlock-all": [".herdr/bin/tabby", "unlock-all"],
    }
    for action_id in expected_actions:
        case.herdr(
            f"canonical-action-{action_id}",
            "plugin",
            "action",
            "invoke",
            action_id,
            "--plugin",
            PLUGIN_ID,
        )

    logs = wait_for(
        f"{case.name} canonical manifest entrypoint logs",
        5.0,
        lambda: completed_manifest_entrypoint_logs(case, expected_actions),
    )
    after_entrypoints = case.ready_runtime()
    if (after_entrypoints.pid, after_entrypoints.launch_id) != (
        owner.pid,
        owner.launch_id,
    ):
        raise HarnessFailure(f"{case.name}: manifest entrypoints replaced the Ready owner")
    case.herdr("canonical-manifest-entrypoint-logs", "plugin", "log", "list")
    case.recorder.assertion(
        case.name,
        "canonical-manifest-entrypoints",
        f"all {len(expected_actions)} actions and all 3 lifecycle hooks ran the prepared plugin-root binary successfully behind one Ready owner ({len(logs)} logs)",
    )

    case.wait_for_label(REPO_ROOT.name)
    case.herdr("run-significant-command", "pane", "run", pane_id, "nvim", "--clean", "-u", "NONE")
    case.wait_for_label("nvim", timeout=15.0)
    case.herdr("leave-significant-command", "pane", "send-keys", pane_id, "esc", ":", "q", "!", "enter")
    case.wait_for_label(REPO_ROOT.name, timeout=18.0)
    case.recorder.assertion(
        case.name,
        "fixed-focus-process-change",
        "one focused tab changed cwd fallback -> nvim -> cwd fallback without navigation",
    )

    manual_label = "manual-contract"
    case.herdr("apply-manual-label", "tab", "rename", tab_id, manual_label)
    wait_for(
        f"{case.name} persisted manual lock",
        10.0,
        lambda: "1 Manually Locked Tabs" in case.tabby_status_text(),
    )
    case.tabby("inspect-manual-lock", "status")
    time.sleep(5.5)
    if case.focused_tab_label() != manual_label:
        raise HarnessFailure(f"{case.name}: periodic refresh overwrote a manual label")
    case.recorder.assertion(
        case.name,
        "manual-lock",
        "manual label became a persisted lock and blocked a later periodic overwrite",
    )
    return tab_id


def completed_manifest_entrypoint_logs(
    case: SessionCase, expected_actions: Dict[str, List[str]]
) -> Optional[List[Dict[str, Any]]]:
    completed = subprocess.run(
        case.herdr_argv("plugin", "log", "list", "--plugin", PLUGIN_ID),
        cwd=REPO_ROOT,
        env=case.environment,
        capture_output=True,
        text=True,
        check=True,
    )
    logs = json.loads(completed.stdout)["result"]["logs"]
    successful = [log for log in logs if log.get("status") == "succeeded"]
    actions = {log.get("action_id"): log.get("command") for log in successful}
    events = {log.get("event"): log.get("command") for log in successful}
    expected_events = {
        "startup": [".herdr/bin/tabby", "ensure-started"],
        "workspace.created": [".herdr/bin/tabby", "signal-created"],
        "tab.created": [".herdr/bin/tabby", "signal-created"],
        "pane.focused": [".herdr/bin/tabby", "signal-focus"],
    }
    if all(actions.get(name) == command for name, command in expected_actions.items()) and all(
        events.get(name) == command for name, command in expected_events.items()
    ):
        return logs
    return None


def exercise_client_detach(case: SessionCase, owner: ReadyRuntime) -> ReadyRuntime:
    if shutil.which("tmux") is None:
        raise HarnessFailure("tmux is required to exercise a real client attach/detach")
    tmux_server = f"tabby-herdr-harness-{os.getpid()}"
    command_environment = {
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
    attach_argv = [
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
        *[f"{name}={value}" for name, value in sorted(command_environment.items())],
        "herdr",
        *case.session_args,
    ]
    case.run("client-attach", attach_argv)
    time.sleep(0.5)
    case.run(
        "client-attached",
        ["tmux", "-L", tmux_server, "has-session", "-t", "client"],
    )
    before_detach = case.ready_runtime()
    case.run(
        "client-detach",
        ["tmux", "-L", tmux_server, "kill-session", "-t", "client"],
    )
    after_detach = wait_for(
        f"{case.name} post-detach periodic evaluation",
        8.0,
        lambda: runtime_after_evaluation(case, before_detach.last_evaluation_unix_ms),
    )
    if (after_detach.pid, after_detach.launch_id) != (owner.pid, owner.launch_id):
        raise HarnessFailure(f"{case.name}: Client Detach replaced the Ready owner")
    case.recorder.assertion(
        case.name,
        "client-detach",
        "attached client exited; the same Ready owner continued five-second freshness",
    )
    return after_detach


def exercise_registered_binary_activation(
    root: Path, case: SessionCase, owner: ReadyRuntime
) -> ReadyRuntime:
    release_root = root / "release"
    release_binary = release_root / "bin" / "tabby"
    release_manifest = release_root / "share" / "tabby" / "herdr-plugin.toml"
    release_binary.parent.mkdir(parents=True)
    release_manifest.parent.mkdir(parents=True)
    shutil.copy2(TABBY, release_binary)
    shutil.copy2(REPO_ROOT / "packaging" / "herdr" / "herdr-plugin.toml", release_manifest)

    case.herdr("unlink-canonical-plugin", "plugin", "unlink", PLUGIN_ID)
    case.herdr(
        "link-homebrew-plugin",
        "plugin",
        "link",
        str(release_manifest.parent),
        "--enabled",
    )
    mismatch = case.tabby("homebrew-mismatch-status", "status")
    if "plugin action invoke start" not in mismatch.stdout:
        raise HarnessFailure("status did not recommend the registered start action")
    case.herdr(
        "activate-homebrew-binary",
        "plugin",
        "action",
        "invoke",
        "start",
        "--plugin",
        PLUGIN_ID,
    )
    replacement = case.wait_ready("homebrew-owner-ready", owner.launch_id)
    if process_exists(owner.pid):
        raise HarnessFailure("plugin-root owner remained alive after Homebrew activation")
    status = case.tabby("homebrew-owner-status", "status")
    if str(release_binary) not in status.stdout:
        raise HarnessFailure("Homebrew manifest did not activate its registered binary")
    if "1 Manually Locked Tabs" not in status.stdout:
        raise HarnessFailure("Homebrew activation did not preserve Session-Scoped Tab State")
    evaluation_before_action = replacement.last_evaluation_unix_ms
    case.herdr(
        "homebrew-manifest-refresh",
        "plugin",
        "action",
        "invoke",
        "refresh",
        "--plugin",
        PLUGIN_ID,
    )
    wait_for(
        "Homebrew manifest refresh action",
        4.0,
        lambda: runtime_after_evaluation(case, evaluation_before_action),
    )
    case.herdr("homebrew-manifest-action-log", "plugin", "log", "list")

    case.herdr("unlink-homebrew-plugin", "plugin", "unlink", PLUGIN_ID)
    case.herdr(
        "relink-canonical-plugin",
        "plugin",
        "link",
        str(REPO_ROOT),
        "--enabled",
    )
    case.herdr(
        "reactivate-plugin-root-binary",
        "plugin",
        "action",
        "invoke",
        "start",
        "--plugin",
        PLUGIN_ID,
    )
    restored = case.wait_ready("plugin-root-owner-ready", replacement.launch_id)
    if process_exists(replacement.pid):
        raise HarnessFailure("Homebrew owner remained alive after plugin-root activation")
    restored_status = case.tabby("plugin-root-owner-status", "status")
    if str(TABBY) not in restored_status.stdout:
        raise HarnessFailure("canonical manifest did not reactivate its registered binary")
    if "1 Manually Locked Tabs" not in restored_status.stdout:
        raise HarnessFailure("bidirectional handoff did not preserve Session-Scoped Tab State")
    case.recorder.assertion(
        case.name,
        "registered-binary-activation",
        "the registered start action moved ownership plugin-root -> Homebrew -> plugin-root, released each old owner before replacement, preserved state, and refreshed through the active manifest",
    )
    return restored


def write_records(output: Path, records: Iterable[Dict[str, Any]]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("".join(json.dumps(record, sort_keys=True) + "\n" for record in records))


def write_session_profile_config(path: Path, named_alias: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "\n".join(
            [
                "version = 1",
                "",
                "[profiles.named.directories.aliases]",
                f"{json.dumps(str(REPO_ROOT))} = {json.dumps(named_alias)}",
                "",
                "[[session_selectors]]",
                'profile = "named"',
                'named_session = "tabby-lifecycle-named"',
                "",
            ]
        )
    )


def run_live(output: Path, expected_herdr: ExpectedHerdrContract) -> None:
    if platform.system() != "Darwin":
        raise HarnessFailure("the real lifecycle harness requires macOS")
    if shutil.which("herdr") is None:
        raise HarnessFailure("herdr is not installed")
    version = subprocess.run(
        ["herdr", "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    expected_version_output = f"herdr {expected_herdr.version}"
    if version != expected_version_output:
        raise HarnessFailure(f"expected {expected_version_output}, got {version!r}")
    if not DEBUG_TABBY.is_file():
        raise HarnessFailure("target/debug/tabby is missing; run `cargo build` first")

    root = Path(tempfile.mkdtemp(prefix="tabby-herdr-harness.", dir="/tmp"))
    recorder = Recorder(root)
    environment = build_environment(root)
    default = SessionCase("default", [], environment, recorder, expected_herdr)
    named = SessionCase(
        "named",
        ["--session", "tabby-lifecycle-named"],
        environment,
        recorder,
        expected_herdr,
    )
    cases = [default, named]
    try:
        for path in (
            root / "home",
            root / "xdg-config",
            root / "xdg-state",
            root / "xdg-cache",
            root / "tmp",
            root / "config" / "herdr",
        ):
            require_descendant(path, root)
            path.mkdir(parents=True, exist_ok=True)
        Path(environment["HERDR_CONFIG_PATH"]).touch()
        tabby_config = (
            root
            / "xdg-config"
            / "herdr"
            / "plugins"
            / "config"
            / PLUGIN_ID
            / "config.toml"
        )
        write_session_profile_config(tabby_config, "named-policy")

        default.run("prepare-plugin-root", PREPARE_COMMAND)
        if not TABBY.is_file():
            raise HarnessFailure(f"preparation did not create {TABBY}")
        default.start_server("bootstrap-server")
        default.herdr("link-plugin", "plugin", "link", str(REPO_ROOT), "--enabled")
        default.herdr(
            "activate-fresh-install",
            "plugin",
            "action",
            "invoke",
            "start",
            "--plugin",
            PLUGIN_ID,
        )
        fresh_owner = default.wait_ready("fresh-install-owner-ready")
        recorder.assertion(
            "default",
            "fresh-install-activation",
            "the registered start action crossed the Startup Gate and confirmed one Ready owner",
        )
        default.stop_server("bootstrap-stop")
        wait_for(
            "fresh install owner lease release",
            5.0,
            lambda: not process_exists(fresh_owner.pid),
        )

        for case in cases:
            case.start_server("session-start")
        owners = {case.name: case.wait_ready("startup-ready") for case in cases}
        if default.socket_path == named.socket_path:
            raise HarnessFailure("default and named sessions resolved the same socket")
        recorder.assertion(
            "all",
            "session-isolation",
            "default and named Herdr Sessions have distinct socket identities and Ready owners",
        )
        if "selected_profile=global policy_source=global" not in default.tabby_status_text():
            raise HarnessFailure("default session did not select the global Label Policy")
        if "selected_profile=named policy_source=profile:named" not in named.tabby_status_text():
            raise HarnessFailure("named session did not select its named Label Policy profile")
        recorder.assertion(
            "all",
            "session-policy-selection",
            "default and named runtimes compiled distinct policies from one config.toml",
        )

        for case in cases:
            exercise_trigger_burst(case, owners[case.name])
            owners[case.name] = exercise_quiet_and_periodic(case)

        default_tab_id = exercise_focused_process_and_manual_lock(
            default, owners["default"]
        )
        named_created = named.herdr(
            "create-focused-workspace",
            "workspace",
            "create",
            "--cwd",
            str(REPO_ROOT),
            "--focus",
        )
        named_tab_id = json.loads(named_created.stdout)["result"]["tab"]["tab_id"]
        named.wait_for_label("named-policy")
        if default_tab_id != named_tab_id:
            raise HarnessFailure("expected equal first tab IDs in default and named sessions")
        state_directories = list(
            (root / "xdg-state" / "herdr" / "plugins" / PLUGIN_ID / "session-tab-state").glob("v2-*")
        )
        if len(state_directories) != 2:
            raise HarnessFailure(
                f"expected two isolated Session-Scoped Tab State directories, got {len(state_directories)}"
            )
        recorder.assertion(
            "all",
            "equal-tab-id-isolation",
            "equal tab IDs produced two state directories keyed by distinct Session Identities",
        )
        if any(REPO_ROOT in state.parents for state in state_directories):
            raise HarnessFailure("Session-Scoped Tab State resolved inside the plugin root")
        recorder.assertion(
            "all",
            "plugin-owned-path-isolation",
            "Session-Scoped Tab State resolved under isolated Herdr state, outside the plugin source root",
        )

        write_session_profile_config(tabby_config, "named-policy-v2")
        named.tabby("reload-named-policy", "config", "reload")
        named.wait_for_label("named-policy-v2")
        if default.focused_tab_label() != "manual-contract":
            raise HarnessFailure("named-session reload changed the default session label")
        tabby_config.write_text("version = 2\n")
        rejected = named.tabby("reject-named-policy-reload", "config", "reload", check=False)
        if rejected.returncode == 0:
            raise HarnessFailure("invalid named-session reload was accepted")
        if named.focused_tab_label() != "named-policy-v2":
            raise HarnessFailure("rejected reload replaced the last valid named policy")
        named_status = named.tabby_status_text()
        if "selected_profile=named policy_source=profile:named" not in named_status:
            raise HarnessFailure("rejected reload discarded the last valid profile selection")
        if "latest_error=<none>" in named_status:
            raise HarnessFailure("rejected reload was not reported through Runtime Status")
        if "latest_error=<none>" not in default.tabby_status_text():
            raise HarnessFailure("named-session reload error leaked into the default runtime")
        write_session_profile_config(tabby_config, "named-policy-v2")
        recorder.assertion(
            "all",
            "session-local-policy-reload",
            "named reload and rejection retained its policy without changing the default runtime",
        )

        owners["default"] = exercise_client_detach(default, owners["default"])

        crashed_owner = owners["named"]
        os.kill(crashed_owner.pid, signal.SIGKILL)
        wait_for(
            "crashed named owner exit",
            5.0,
            lambda: not process_exists(crashed_owner.pid),
        )
        named.tabby("recover-after-crash", "signal-created")
        owners["named"] = named.wait_ready(
            "ready-after-crash", crashed_owner.launch_id
        )
        if "selected_profile=named policy_source=profile:named" not in named.tabby_status_text():
            raise HarnessFailure("restored named runtime did not reselect its profile")
        recorder.assertion(
            "named", "runtime-crash", "supported creation hook restored a new Ready owner"
        )

        owners["default"] = exercise_registered_binary_activation(
            root, default, owners["default"]
        )

        stopped_owner = owners["default"]
        default.stop_server("session-stop")
        wait_for(
            "default owner lease release",
            5.0,
            lambda: not process_exists(stopped_owner.pid),
        )
        default.start_server("session-restore")
        owners["default"] = default.wait_ready(
            "ready-after-restore", stopped_owner.launch_id
        )
        wait_for("restored initial evaluation", 5.0, lambda: runtime_with_evaluation(default))
        if default.focused_tab_label() != "manual-contract":
            raise HarnessFailure("manual label did not survive Session Stop/Restore")
        if "1 Manually Locked Tabs" not in default.tabby_status_text():
            raise HarnessFailure("manual lock did not survive Session Stop/Restore")
        recorder.assertion(
            "default",
            "session-restore",
            "startup hook created one new owner; initial evaluation followed quiet and retained manual intent",
        )
    finally:
        for case in reversed(cases):
            try:
                if case.status_json().get("server", {}).get("running"):
                    case.stop_server("cleanup-stop")
            except Exception as error:
                recorder.records.append(
                    {
                        "schema_version": TRANSCRIPT_SCHEMA_VERSION,
                        "case": case.name,
                        "step": "cleanup-stop",
                        "error": str(error),
                    }
                )
        write_records(output, recorder.records)
        require_descendant(root, Path("/tmp"))
        shutil.rmtree(root)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", action="store_true", help="print the isolated plan without running it")
    parser.add_argument("--root", type=Path, help="sandbox root used only by --plan")
    add_expected_herdr_arguments(parser)
    parser.add_argument(
        "--output",
        type=Path,
        default=REPO_ROOT / ".scratch" / "herdr-lifecycle-transcript.jsonl",
        help="JSONL transcript destination for a live run",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        expected_herdr = expected_herdr_contract(args)
        if args.plan:
            root = args.root or Path("/tmp/tabby-herdr-harness.PLAN")
            print(json.dumps(plan(root, expected_herdr), indent=2, sort_keys=True))
            return 0
        if args.root is not None:
            raise HarnessFailure("--root is accepted only with --plan")
        run_live(args.output.resolve(), expected_herdr)
        print(f"Herdr lifecycle harness passed; transcript: {args.output.resolve()}")
        return 0
    except (HarnessFailure, OSError, subprocess.SubprocessError) as error:
        print(f"herdr lifecycle harness failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
