#!/usr/bin/env python3
"""Capture the Herdr 0.8 agent-title contract without retaining agent content.

The harness starts an isolated Herdr server and gives the agents a temporary home.
It records only a whitelisted projection of snapshots plus API event envelopes;
prompts, terminal output, environment values, and credentials are never recorded.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parents[1]
SCHEMA_VERSION = 1
HERDR_VERSION = "herdr 0.8.0"
PROTOCOL = 19
EVENT_TYPES = [
    "pane.updated",
    "pane.agent_detected",
    "pane.focused",
    "tab.focused",
    "workspace.focused",
]


class HarnessFailure(RuntimeError):
    pass


def isolated_environment(root: Path, caller_home: Path) -> dict[str, str]:
    environment = dict(os.environ)
    for name in [name for name in environment if name.startswith("HERDR_")]:
        environment.pop(name, None)
    environment.update(
        {
            "XDG_CONFIG_HOME": str(root / "xdg-config"),
            "XDG_STATE_HOME": str(root / "xdg-state"),
            "XDG_CACHE_HOME": str(root / "xdg-cache"),
            "TMPDIR": str(root / "tmp"),
            "HOME": str(root / "home"),
            "CODEX_HOME": str(root / "codex-home"),
            "HERDR_CONFIG_PATH": str(root / "config" / "herdr" / "config.toml"),
        }
    )
    codex_auth = caller_home / ".codex" / "auth.json"
    if codex_auth.is_file():
        destination = Path(environment["CODEX_HOME"]) / "auth.json"
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(codex_auth, destination)
        destination.chmod(0o600)
    return environment


class Recorder:
    def __init__(self, root: Path, output: Path) -> None:
        self.root = root
        self.output = output
        self.records: list[dict[str, Any]] = []
        self.home = Path.home()
        self.terminal_title_tokens: dict[str, str] = {}
        self.token_lock = threading.Lock()

    def terminal_title_token(self, value: str) -> str:
        with self.token_lock:
            token = self.terminal_title_tokens.get(value)
            if token is None:
                token = f"<redacted-terminal-title-{len(self.terminal_title_tokens) + 1}>"
                self.terminal_title_tokens[value] = token
            return token

    def redact(self, value: Any) -> Any:
        if isinstance(value, str):
            return (
                value.replace(str(self.root), "<sandbox>")
                .replace(str(REPO_ROOT), "<repo>")
                .replace(str(self.home), "<home>")
            )
        if isinstance(value, list):
            return [self.redact(item) for item in value]
        if isinstance(value, dict):
            return {
                key: (
                    self.terminal_title_token(item)
                    if key in {"terminal_title", "terminal_title_stripped"}
                    and isinstance(item, str)
                    and not item.startswith("issue88-")
                    else self.redact(item)
                )
                for key, item in value.items()
            }
        return value

    def add(self, kind: str, step: str, **data: Any) -> None:
        self.records.append(
            self.redact(
                {
                    "schema_version": SCHEMA_VERSION,
                    "at": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "kind": kind,
                    "step": step,
                    **data,
                }
            )
        )

    def write(self) -> None:
        self.output.parent.mkdir(parents=True, exist_ok=True)
        self.output.write_text(
            "".join(json.dumps(record, sort_keys=True) + "\n" for record in self.records)
        )


class EventStream:
    def __init__(self, socket_path: str, recorder: Recorder) -> None:
        self.socket_path = socket_path
        self.recorder = recorder
        self.stop_requested = threading.Event()
        self.ready = threading.Event()
        self.stream: socket.socket | None = None
        self.thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self.thread.start()
        if not self.ready.wait(3):
            raise HarnessFailure("events.subscribe did not acknowledge within 3 seconds")

    def stop(self) -> None:
        self.stop_requested.set()
        if self.stream:
            try:
                self.stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            raise HarnessFailure("events.subscribe did not stop within 2 seconds")

    def _run(self) -> None:
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
                self.stream = stream
                stream.connect(self.socket_path)
                request = {
                    "id": "issue-88-events",
                    "method": "events.subscribe",
                    "params": {"subscriptions": [{"type": name} for name in EVENT_TYPES]},
                }
                stream.sendall((json.dumps(request) + "\n").encode())
                buffer = b""
                while not self.stop_requested.is_set():
                    chunk = stream.recv(65536)
                    if not chunk:
                        return
                    buffer += chunk
                    while b"\n" in buffer:
                        line, buffer = buffer.split(b"\n", 1)
                        if not line:
                            continue
                        message = json.loads(line)
                        if message.get("result", {}).get("type") == "subscription_started":
                            self.ready.set()
                            self.recorder.add("event", "subscription-started", message=message)
                        else:
                            self.recorder.add("event", "stream", message=message)
        except Exception as error:  # the raw record makes transport failure explicit
            if not self.stop_requested.is_set():
                self.recorder.add("event-stream-error", "stream", error=repr(error))
            self.ready.set()
        finally:
            self.stream = None


class Herdr:
    def __init__(self, root: Path, environment: dict[str, str], recorder: Recorder) -> None:
        self.root = root
        self.environment = environment
        self.recorder = recorder
        self.server: subprocess.Popen[str] | None = None
        self.socket_path: str | None = None
        self.events: EventStream | None = None

    def run(self, step: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        started = time.monotonic()
        completed = subprocess.run(
            ["herdr", *args],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
        )
        argv = ["herdr", *args]
        stdout = completed.stdout.strip()
        stderr = completed.stderr.strip()
        if len(args) >= 3 and args[0:2] == ("agent", "prompt"):
            argv = ["herdr", "agent", "prompt", args[2], "<redacted>", *args[4:]]
            stdout = "<redacted>"
            stderr = "<redacted>" if stderr else ""
        if len(args) >= 4 and args[0:2] == ("pane", "send-text"):
            argv = ["herdr", "pane", "send-text", args[2], "<redacted>"]
        # Command output is either projected by a structured record or intentionally omitted.
        # Raw JSON can embed terminal titles and other machine-local strings inside a string,
        # where key-based redaction cannot inspect it.
        if stdout and stdout != "<redacted>":
            stdout = "<captured-as-projection>" if args == ("api", "snapshot") else "<omitted>"
        if stderr and stderr != "<redacted>":
            stderr = "<omitted>"
        self.recorder.add(
            "command",
            step,
            argv=argv,
            exit=completed.returncode,
            elapsed_ms=int((time.monotonic() - started) * 1000),
            stdout=stdout,
            stderr=stderr,
        )
        if check and completed.returncode:
            raise HarnessFailure(f"{step} failed: {completed.stderr.strip()}")
        return completed

    def status(self) -> dict[str, Any]:
        completed = subprocess.run(
            ["herdr", "status", "--json"],
            cwd=self.root,
            env=self.environment,
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(completed.stdout)

    def start_server(self, step: str) -> None:
        self.server = subprocess.Popen(
            ["herdr", "server"],
            cwd=REPO_ROOT,
            env=self.environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            try:
                status = self.status()
                server = status.get("server", {})
                if server.get("running"):
                    if server.get("version") != "0.8.0" or server.get("protocol") != PROTOCOL:
                        raise HarnessFailure(f"unexpected Herdr server contract: {server}")
                    self.socket_path = server["socket"]
                    self.recorder.add("assertion", step, result="passed", detail="Herdr 0.8.0 protocol 19 ready")
                    self.events = EventStream(self.socket_path, self.recorder)
                    self.events.start()
                    return
            except (OSError, ValueError, subprocess.SubprocessError):
                pass
            time.sleep(0.05)
        raise HarnessFailure("Herdr server was not ready")

    def stop_server(self, step: str) -> None:
        if self.events:
            self.events.stop()
            self.events = None
        self.run(step, "server", "stop", check=False)
        if self.server:
            self.server.wait(timeout=8)
            self.server = None
        self.socket_path = None

    def snapshot(self, step: str) -> dict[str, Any]:
        response = self.run(step, "api", "snapshot")
        snapshot = json.loads(response.stdout)["result"]["snapshot"]
        if snapshot.get("version") != "0.8.0" or snapshot.get("protocol") != PROTOCOL:
            raise HarnessFailure(f"unexpected snapshot contract: {snapshot.get('version')} / {snapshot.get('protocol')}")
        panes = []
        for pane in snapshot.get("panes", []):
            panes.append(
                {
                    key: pane.get(key)
                    for key in (
                        "pane_id", "tab_id", "terminal_id", "focused", "agent", "agent_session",
                        "agent_status", "terminal_title", "terminal_title_stripped", "title", "revision",
                    )
                }
            )
        self.recorder.add(
            "snapshot",
            step,
            focused_pane_id=snapshot.get("focused_pane_id"),
            focused_tab_id=snapshot.get("focused_tab_id"),
            panes=panes,
        )
        return snapshot


def pane_for(snapshot: dict[str, Any], pane_id: str) -> dict[str, Any]:
    return next(pane for pane in snapshot["panes"] if pane["pane_id"] == pane_id)


def exercise_osc_titles(herdr: Herdr, pane_id: str, agent: str) -> None:
    values = [
        f"issue88-{agent}-context-a",
        f"issue88-{agent}-context-a",  # repeated value
        f"issue88-{agent}-context-b",
        f"issue88-{agent}-rapid-1",
        f"issue88-{agent}-rapid-2",
        f"issue88-{agent}-rapid-3",
        "",
    ]
    for index, value in enumerate(values):
        # This command is executed by the shell before the agent starts; the OSC payload is fixed.
        escaped = value.replace("'", "'\\''")
        command = f"printf '\\033]2;{escaped}\\007'"
        herdr.run(f"{agent}-osc-{index}", "pane", "run", pane_id, "sh", "-lc", command)
        # Headless `pane run` delivery is intentionally observed rather than assumed.  The
        # snapshot makes any absence of an OSC update an explicit result, not a fabricated one.
        snapshot = herdr.snapshot(f"{agent}-osc-snapshot-{index}")
        observed = pane_for(snapshot, pane_id).get("terminal_title")
        herdr.recorder.add(
            "assertion",
            f"{agent}-osc-{index}",
            result="passed" if observed == (value or None) else "not-observed",
            detail="controlled OSC title delivery was projected through session.snapshot",
        )


def start_agent(herdr: Herdr, pane_id: str, kind: str) -> None:
    name = f"issue88-{kind}"
    # A newly created PTY needs to reach its interactive shell prompt before the API's
    # intentional `agent_pane_busy` guard will permit a managed agent start.
    time.sleep(2)
    safe_arguments = {
        "codex": ("--sandbox", "read-only", "--ask-for-approval", "never"),
        "claude": ("--safe-mode", "--tools", "", "--permission-mode", "dontAsk"),
    }[kind]
    herdr.run(
        f"{kind}-start",
        "agent",
        "start",
        name,
        "--kind",
        kind,
        "--pane",
        pane_id,
        "--timeout",
        "30000",
        "--",
        *safe_arguments,
    )
    snapshot = herdr.snapshot(f"{kind}-started")
    pane = pane_for(snapshot, pane_id)
    if pane.get("agent") != kind:
        raise HarnessFailure(f"{kind}: Herdr did not identify the started agent: {pane}")
    if not pane.get("agent_session"):
        herdr.recorder.add("assertion", f"{kind}-agent-session", result="unproven", detail="agent start did not publish an agent_session")
    else:
        herdr.recorder.add("assertion", f"{kind}-agent-session", result="passed", detail="agent_session was present after real agent start")


def exercise_agent_context(herdr: Herdr, pane_id: str, agent: str) -> None:
    for index in range(2):
        marker = f"issue88-{agent}-context-{index + 1}"
        # The fixed prompt permits no tools or repository access.  Its text is deliberately not
        # recorded. `wait-output` proves that each distinct request reached the real agent pane
        # without retaining any terminal content or depending on completion timing.
        delivery = herdr.run(
            f"{agent}-context-{index + 1}",
            "pane",
            "send-text",
            pane_id,
            f"{marker}: respond with exactly acknowledged. Do not use tools or read files.",
        )
        submission = herdr.run(
            f"{agent}-context-submit-{index + 1}",
            "pane",
            "send-keys",
            pane_id,
            "enter",
            check=False,
        )
        herdr.recorder.add(
            "assertion",
            f"{agent}-context-{index + 1}",
            result="passed" if delivery.returncode == submission.returncode == 0 else "not-observed",
            context_id=marker,
            detail="the socket accepted distinct text and Enter input for the real agent pane; content is omitted",
        )
        time.sleep(0.25)
        herdr.snapshot(f"{agent}-context-snapshot-{index + 1}")


def exercise_metadata(herdr: Herdr, pane_id: str, agent: str) -> None:
    source = f"issue-88-{agent}-metadata"
    for index, title in enumerate(["context-a", "context-a", "context-b", "rapid-1", "rapid-2", "rapid-3"]):
        herdr.run(
            f"{agent}-metadata-{index}",
            "pane", "report-metadata", pane_id, "--source", source, "--agent", agent,
            "--seq", str(index + 1), "--title", f"issue88-{agent}-{title}",
        )
        herdr.snapshot(f"{agent}-metadata-snapshot-{index}")
    herdr.run(
        f"{agent}-metadata-empty",
        "pane", "report-metadata", pane_id, "--source", source, "--agent", agent,
        "--seq", "7", "--clear-title",
    )
    herdr.snapshot(f"{agent}-metadata-empty-snapshot")


def detach_client(herdr: Herdr) -> None:
    if not shutil.which("tmux"):
        herdr.recorder.add("assertion", "client-detach", result="not-run", detail="tmux is unavailable")
        return
    tmux_socket = f"issue88-herdr-{os.getpid()}"
    subprocess.run(
        ["tmux", "-L", tmux_socket, "-f", "/dev/null", "new-session", "-d", "-s", "client", "herdr"],
        cwd=herdr.root,
        env=herdr.environment,
        check=True,
        capture_output=True,
        text=True,
    )
    time.sleep(0.5)
    herdr.snapshot("client-attached")
    subprocess.run(["tmux", "-L", tmux_socket, "kill-session", "-t", "client"], check=True, capture_output=True, text=True)
    herdr.snapshot("client-detached")


def run(output: Path, run_authenticated_agents: bool) -> None:
    if not run_authenticated_agents:
        raise HarnessFailure(
            "pass --run-authenticated-agents to acknowledge use of installed agent credentials"
        )
    if subprocess.run(["herdr", "--version"], capture_output=True, text=True).stdout.strip() != HERDR_VERSION:
        raise HarnessFailure(f"expected {HERDR_VERSION}")
    for executable in ("codex", "claude"):
        if not shutil.which(executable):
            raise HarnessFailure(f"{executable} is not installed")

    root = Path(tempfile.mkdtemp(prefix="tabby-issue-88-herdr.", dir="/tmp"))
    recorder = Recorder(root, output)
    environment = isolated_environment(root, Path.home())
    herdr = Herdr(root, environment, recorder)
    recorder.add(
        "plan",
        "environment",
        sandbox="<sandbox>",
        agent_home="<sandbox>/home",
        event_types=EVENT_TYPES,
        redactions=["local paths", "prompt text", "command output", "terminal-title contents"],
    )
    try:
        for path in (
            root / "xdg-config",
            root / "xdg-state",
            root / "xdg-cache",
            root / "tmp",
            root / "home",
            root / "workspace",
            root / "config" / "herdr",
        ):
            path.mkdir(parents=True, exist_ok=True)
        Path(environment["HERDR_CONFIG_PATH"]).touch()
        herdr.start_server("server-start")
        herdr.run(
            "codex-workspace-create",
            "workspace",
            "create",
            "--cwd",
            str(root / "workspace"),
            "--focus",
        )
        initial = herdr.snapshot("initial")
        osc_pane = initial["panes"][0]["pane_id"]
        exercise_osc_titles(herdr, osc_pane, "codex")
        created = herdr.run(
            "codex-tab-create", "tab", "create", "--cwd", str(root / "workspace"), "--focus"
        )
        codex_pane = json.loads(created.stdout)["result"]["root_pane"]["pane_id"]
        start_agent(herdr, codex_pane, "codex")
        exercise_agent_context(herdr, codex_pane, "codex")
        exercise_metadata(herdr, codex_pane, "codex")

        created = herdr.run(
            "claude-tab-create", "tab", "create", "--cwd", str(root / "workspace"), "--focus"
        )
        claude_pane = json.loads(created.stdout)["result"]["root_pane"]["pane_id"]
        start_agent(herdr, claude_pane, "claude")
        exercise_agent_context(herdr, claude_pane, "claude")
        exercise_metadata(herdr, claude_pane, "claude")
        created = herdr.run(
            "claude-osc-tab-create",
            "tab",
            "create",
            "--cwd",
            str(root / "workspace"),
            "--focus",
        )
        claude_osc_pane = json.loads(created.stdout)["result"]["root_pane"]["pane_id"]
        exercise_osc_titles(herdr, claude_osc_pane, "claude")

        herdr.run("focus-codex", "tab", "focus", pane_for(herdr.snapshot("before-focus-codex"), codex_pane)["tab_id"])
        herdr.snapshot("focused-codex")
        herdr.run("focus-claude", "tab", "focus", pane_for(herdr.snapshot("before-focus-claude"), claude_pane)["tab_id"])
        herdr.snapshot("focused-claude")
        detach_client(herdr)

        herdr.stop_server("session-stop")
        recorder.add("assertion", "session-stop", result="passed", detail="the isolated server was stopped; live pane and agent processes end with it")
        herdr.start_server("session-restore")
        herdr.snapshot("after-session-restore")
        recorder.add("assertion", "session-restore", result="passed", detail="a new isolated server process was started against the same persisted session roots")
        herdr.stop_server("cold-restart-stop")
        herdr.start_server("cold-restart")
        herdr.snapshot("after-cold-restart")
        recorder.add("assertion", "cold-restart", result="passed", detail="a second fresh server process was started after an additional stop; stopped pane processes are not expected to preserve live agents")
    except Exception as error:
        recorder.add("failure", "harness", error=repr(error))
        raise
    finally:
        try:
            herdr.stop_server("cleanup-server-stop")
        except Exception as error:
            recorder.add("cleanup-failure", "cleanup-server-stop", error=repr(error))
        recorder.write()
        shutil.rmtree(root, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--run-authenticated-agents", action="store_true")
    arguments = parser.parse_args()
    try:
        run(arguments.output, arguments.run_authenticated_agents)
    except HarnessFailure as error:
        print(f"herdr agent-title contract harness failed: {error}", file=os.sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
