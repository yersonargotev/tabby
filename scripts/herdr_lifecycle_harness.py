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
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, List, Optional, Sequence, Tuple


REPO_ROOT = Path(__file__).resolve().parents[1]
TABBY = REPO_ROOT / "target" / "debug" / "tabby"
PLUGIN_ID = "yersonargotev.tabby"
READY_RE = re.compile(
    r"Session Runtime: Ready pid=(?P<pid>\d+).*\n"
    r"Session Runtime details: launch_id=(?P<launch>\S+).*"
    r"last_evaluation_unix_ms=(?P<evaluation>\S+)"
)


class HarnessFailure(RuntimeError):
    pass


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
                "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "case": case,
                "step": step,
                "assertion": self.sanitize(detail),
                "result": "passed",
            }
        )


class SessionCase:
    def __init__(
        self,
        name: str,
        session_args: Sequence[str],
        environment: Dict[str, str],
        recorder: Recorder,
    ) -> None:
        self.name = name
        self.session_args = list(session_args)
        self.environment = environment
        self.recorder = recorder
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
            lambda: self.status_json() if self.status_json().get("server", {}).get("running") else None,
        )
        server = status["server"]
        if server.get("version") != "0.8.0" or server.get("protocol") != 19:
            raise HarnessFailure(f"{self.name}: unexpected Herdr contract: {server}")
        self.socket_path = server["socket"]
        self.recorder.assertion(
            self.name,
            step,
            f"Herdr 0.8.0 protocol 19 ready at {self.socket_path}",
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

    def ready_runtime(self) -> Tuple[int, str, Optional[int]]:
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
        return (
            int(match.group("pid")),
            match.group("launch"),
            None if evaluation == "<none>" else int(evaluation),
        )

    def wait_ready(self, step: str, previous_launch: Optional[str] = None) -> Tuple[int, str, Optional[int]]:
        runtime = wait_for(
            f"{self.name} Ready Session Runtime",
            8.0,
            lambda: ready_if_new(self, previous_launch),
        )
        self.tabby(step, "status")
        return runtime


def ready_if_new(case: SessionCase, previous_launch: Optional[str]) -> Optional[Tuple[int, str, Optional[int]]]:
    try:
        runtime = case.ready_runtime()
    except (HarnessFailure, OSError):
        return None
    return runtime if previous_launch is None or runtime[1] != previous_launch else None


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


def exercise_trigger_burst(case: SessionCase, owner: Tuple[int, str, Optional[int]]) -> Tuple[int, str, Optional[int]]:
    commands = ["ensure-started", "signal-created", "refresh", "signal-focus"] * 4
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        futures = [executor.submit(case.tabby, f"burst-{index}", command) for index, command in enumerate(commands)]
        for future in futures:
            future.result()
    after = case.ready_runtime()
    if after[:2] != owner[:2]:
        raise HarnessFailure(f"{case.name}: trigger burst replaced the Ready owner")
    case.recorder.assertion(
        case.name,
        "concurrent-trigger-burst",
        "16 startup/create/manual/focus hooks coalesced behind one Ready owner",
    )
    return after


def exercise_quiet_and_periodic(case: SessionCase) -> Tuple[int, str, Optional[int]]:
    owner = wait_for(
        f"{case.name} initial evaluation",
        5.0,
        lambda: runtime_with_evaluation(case),
    )
    case.tabby("focus-quiet-trigger", "signal-focus")
    immediately_after = case.ready_runtime()
    time.sleep(0.75)
    during_quiet = case.ready_runtime()
    if during_quiet[2] != immediately_after[2]:
        raise HarnessFailure(f"{case.name}: evaluation occurred inside the Focus Quiet Window")
    after_quiet = wait_for(
        f"{case.name} post-quiet evaluation",
        4.0,
        lambda: runtime_after_evaluation(case, during_quiet[2]),
    )
    if after_quiet[:2] != owner[:2]:
        raise HarnessFailure(f"{case.name}: owner changed during focus evaluation")
    periodic = wait_for(
        f"{case.name} five-second periodic evaluation",
        8.0,
        lambda: runtime_after_evaluation(case, after_quiet[2]),
    )
    if periodic[:2] != owner[:2]:
        raise HarnessFailure(f"{case.name}: owner changed during periodic evaluation")
    case.recorder.assertion(
        case.name,
        "quiet-and-periodic",
        "no evaluation during 750 ms of quiet; evaluation followed quiet and repeated on idle cadence",
    )
    return periodic


def runtime_with_evaluation(case: SessionCase) -> Optional[Tuple[int, str, Optional[int]]]:
    runtime = case.ready_runtime()
    return runtime if runtime[2] is not None else None


def runtime_after_evaluation(case: SessionCase, previous: Optional[int]) -> Optional[Tuple[int, str, Optional[int]]]:
    runtime = case.ready_runtime()
    return runtime if runtime[2] is not None and runtime[2] != previous else None


def write_records(output: Path, records: Iterable[Dict[str, Any]]) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text("".join(json.dumps(record, sort_keys=True) + "\n" for record in records))


def run_live(output: Path) -> None:
    if platform.system() != "Darwin":
        raise HarnessFailure("the real lifecycle harness requires macOS")
    if shutil.which("herdr") is None:
        raise HarnessFailure("herdr is not installed")
    version = subprocess.run(
        ["herdr", "--version"], capture_output=True, text=True, check=True
    ).stdout.strip()
    if version != "herdr 0.8.0":
        raise HarnessFailure(f"expected herdr 0.8.0, got {version!r}")
    if not TABBY.is_file():
        raise HarnessFailure("target/debug/tabby is missing; run `cargo build` first")

    root = Path(tempfile.mkdtemp(prefix="tabby-herdr-harness.", dir="/tmp"))
    recorder = Recorder(root)
    environment = build_environment(root)
    default = SessionCase("default", [], environment, recorder)
    named = SessionCase(
        "named", ["--session", "tabby-lifecycle-named"], environment, recorder
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

        default.start_server("bootstrap-server")
        default.herdr("link-plugin", "plugin", "link", str(REPO_ROOT), "--enabled")
        default.stop_server("bootstrap-stop")

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

        for case in cases:
            exercise_trigger_burst(case, owners[case.name])
            owners[case.name] = exercise_quiet_and_periodic(case)

        crashed_pid, crashed_launch, _ = owners["named"]
        os.kill(crashed_pid, signal.SIGKILL)
        wait_for("crashed named owner exit", 5.0, lambda: not process_exists(crashed_pid))
        named.tabby("recover-after-crash", "signal-created")
        owners["named"] = named.wait_ready("ready-after-crash", crashed_launch)
        recorder.assertion(
            "named", "runtime-crash", "supported creation hook restored a new Ready owner"
        )

        stopped_pid, stopped_launch, _ = owners["default"]
        default.stop_server("session-stop")
        wait_for("default owner lease release", 5.0, lambda: not process_exists(stopped_pid))
        default.start_server("session-restore")
        owners["default"] = default.wait_ready("ready-after-restore", stopped_launch)
        wait_for("restored initial evaluation", 5.0, lambda: runtime_with_evaluation(default))
        recorder.assertion(
            "default",
            "session-restore",
            "startup hook created one new owner and initial evaluation followed quiet",
        )
    finally:
        for case in reversed(cases):
            try:
                if case.status_json().get("server", {}).get("running"):
                    case.stop_server("cleanup-stop")
            except Exception as error:
                recorder.records.append(
                    {"case": case.name, "step": "cleanup-stop", "error": str(error)}
                )
        write_records(output, recorder.records)
        require_descendant(root, Path("/tmp"))
        shutil.rmtree(root)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", action="store_true", help="print the isolated plan without running it")
    parser.add_argument("--root", type=Path, help="sandbox root used only by --plan")
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
        if args.plan:
            root = args.root or Path("/tmp/tabby-herdr-harness.PLAN")
            print(json.dumps(plan(root), indent=2, sort_keys=True))
            return 0
        if args.root is not None:
            raise HarnessFailure("--root is accepted only with --plan")
        run_live(args.output.resolve())
        print(f"Herdr lifecycle harness passed; transcript: {args.output.resolve()}")
        return 0
    except (HarnessFailure, OSError, subprocess.SubprocessError) as error:
        print(f"herdr lifecycle harness failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
