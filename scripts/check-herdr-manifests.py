#!/usr/bin/env python3
"""Check that Tabby's dev and release Herdr manifests only differ where intended."""

from __future__ import annotations

import ast
import sys
from pathlib import Path
from typing import Any

DEV_MANIFEST = Path("herdr-plugin.toml")
RELEASE_MANIFEST = Path("packaging/herdr/herdr-plugin.toml")
CARGO_MANIFEST = Path("Cargo.toml")
DEV_BINARY = "target/debug/tabby"
RELEASE_BINARY = "../../bin/tabby"


def parse_value(raw: str) -> Any:
    value = raw.strip()
    if value.startswith("["):
        return ast.literal_eval(value)
    if value.startswith('"') and value.endswith('"'):
        return value[1:-1]
    return value


def load_manifest(path: Path) -> dict[str, Any]:
    manifest: dict[str, Any] = {"actions": [], "events": [], "startup": []}
    current_section: dict[str, Any] | None = None

    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped == "[[actions]]":
            current_section = {}
            manifest["actions"].append(current_section)
            continue
        if stripped == "[[events]]":
            current_section = {}
            manifest["events"].append(current_section)
            continue
        if stripped == "[[startup]]":
            current_section = {}
            manifest["startup"].append(current_section)
            continue
        if "=" not in stripped:
            raise ValueError(f"{path}:{line_number}: unsupported TOML line: {line!r}")

        key, raw_value = stripped.split("=", 1)
        target = current_section if current_section is not None else manifest
        target[key.strip()] = parse_value(raw_value)

    return manifest


def action_map(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    actions = manifest.get("actions", [])
    return {action["id"]: action for action in actions}


def event_map(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    events = manifest.get("events", [])
    return {event["on"]: event for event in events}


def cargo_package_version(path: Path) -> str:
    in_package = False
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        stripped = line.strip()
        if stripped == "[package]":
            in_package = True
            continue
        if stripped.startswith("["):
            in_package = False
        if in_package and stripped.startswith("version") and "=" in stripped:
            value = parse_value(stripped.split("=", 1)[1])
            if isinstance(value, str) and value:
                return value
            raise ValueError(f"{path}:{line_number}: invalid package version")
    raise ValueError(f"{path}: missing [package] version")


def check_command_pair(
    errors: list[str],
    kind: str,
    name: str,
    dev_command: list[str],
    release_command: list[str],
    expected_args: list[str] | None = None,
) -> None:
    if not dev_command or dev_command[0] != DEV_BINARY:
        errors.append(f"dev {kind} {name!r} must invoke {DEV_BINARY!r}, got {dev_command!r}")
    if not release_command or release_command[0] != RELEASE_BINARY:
        errors.append(
            f"release {kind} {name!r} must invoke {RELEASE_BINARY!r}, got {release_command!r}"
        )
    if dev_command[1:] != release_command[1:]:
        errors.append(
            f"{kind} {name!r} command args differ after binary path: "
            f"{dev_command[1:]!r} != {release_command[1:]!r}"
        )
    if expected_args is not None and dev_command[1:] != expected_args:
        errors.append(f"{kind} {name!r} must run {' '.join(expected_args)}, got {dev_command[1:]!r}")


def main() -> int:
    dev = load_manifest(DEV_MANIFEST)
    release = load_manifest(RELEASE_MANIFEST)
    package_version = cargo_package_version(CARGO_MANIFEST)
    errors: list[str] = []

    allowed_top_level_keys = {
        "id",
        "name",
        "version",
        "min_herdr_version",
        "description",
        "platforms",
        "actions",
        "events",
        "startup",
    }
    for manifest_path, manifest in [(DEV_MANIFEST, dev), (RELEASE_MANIFEST, release)]:
        extra_keys = set(manifest) - allowed_top_level_keys
        if extra_keys:
            errors.append(
                f"{manifest_path} has unsupported top-level keys: {sorted(extra_keys)}"
            )

    for key in ["id", "name", "version", "min_herdr_version", "platforms"]:
        if dev.get(key) != release.get(key):
            errors.append(
                f"{key} differs: {DEV_MANIFEST} has {dev.get(key)!r}, "
                f"{RELEASE_MANIFEST} has {release.get(key)!r}"
            )

    if dev.get("min_herdr_version") != "0.8.0":
        errors.append(
            f"{DEV_MANIFEST} min_herdr_version must be '0.8.0', "
            f"got {dev.get('min_herdr_version')!r}"
        )

    if dev.get("version") != package_version:
        errors.append(
            f"manifest version {dev.get('version')!r} must match Cargo package version "
            f"{package_version!r}"
        )

    for manifest_path, startup_commands in [
        (DEV_MANIFEST, dev.get("startup", [])),
        (RELEASE_MANIFEST, release.get("startup", [])),
    ]:
        if len(startup_commands) != 1:
            errors.append(
                f"{manifest_path} must declare exactly one startup command, "
                f"got {len(startup_commands)}"
            )

    dev_startup = dev.get("startup", [])
    release_startup = release.get("startup", [])
    if len(dev_startup) == len(release_startup) == 1:
        for manifest_path, startup in [
            (DEV_MANIFEST, dev_startup[0]),
            (RELEASE_MANIFEST, release_startup[0]),
        ]:
            extra_keys = set(startup) - {"command"}
            if extra_keys:
                errors.append(
                    f"startup command in {manifest_path} has unsupported keys: "
                    f"{sorted(extra_keys)}"
                )
        check_command_pair(
            errors,
            "startup",
            "default",
            dev_startup[0].get("command", []),
            release_startup[0].get("command", []),
            ["ensure-started"],
        )

    expected_actions = {"start", "refresh", "unlock-focused", "unlock-all"}
    dev_actions = action_map(dev)
    release_actions = action_map(release)
    if set(dev_actions) != expected_actions:
        errors.append(
            f"dev action ids must be {sorted(expected_actions)}, got {sorted(dev_actions)}"
        )
    if set(release_actions) != expected_actions:
        errors.append(
            f"release action ids must be {sorted(expected_actions)}, got {sorted(release_actions)}"
        )
    if set(dev_actions) != set(release_actions):
        errors.append(
            "action ids differ: "
            f"{DEV_MANIFEST} has {sorted(dev_actions)}, "
            f"{RELEASE_MANIFEST} has {sorted(release_actions)}"
        )

    for action_id in sorted(set(dev_actions) & set(release_actions)):
        dev_action = dev_actions[action_id]
        release_action = release_actions[action_id]
        for key in ["title", "contexts"]:
            if dev_action.get(key) != release_action.get(key):
                errors.append(
                    f"action {action_id!r} {key} differs: "
                    f"{dev_action.get(key)!r} != {release_action.get(key)!r}"
                )

        dev_command = dev_action.get("command", [])
        release_command = release_action.get("command", [])
        check_command_pair(
            errors,
            "action",
            action_id,
            dev_command,
            release_command,
            ["ensure-started"]
            if action_id == "start"
            else (["refresh"] if action_id == "refresh" else None),
        )

    expected_events = {"pane.focused", "workspace.created", "tab.created"}
    dev_events = event_map(dev)
    release_events = event_map(release)
    if set(dev_events) != expected_events:
        errors.append(
            f"dev event hooks must be {sorted(expected_events)}, got {sorted(dev_events)}"
        )
    if set(release_events) != expected_events:
        errors.append(
            f"release event hooks must be {sorted(expected_events)}, got {sorted(release_events)}"
        )

    for event_name in sorted(set(dev_events) & set(release_events)):
        dev_event = dev_events[event_name]
        release_event = release_events[event_name]
        for manifest_path, event in [
            (DEV_MANIFEST, dev_event),
            (RELEASE_MANIFEST, release_event),
        ]:
            extra_keys = set(event) - {"on", "command"}
            if extra_keys:
                errors.append(
                    f"event {event_name!r} in {manifest_path} has unsupported keys: "
                    f"{sorted(extra_keys)}"
                )

        dev_command = dev_event.get("command", [])
        release_command = release_event.get("command", [])
        expected_args = (
            ["signal-focus"]
            if event_name == "pane.focused"
            else ["signal-created"]
        )
        check_command_pair(
            errors,
            "event",
            event_name,
            dev_command,
            release_command,
            expected_args,
        )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"{DEV_MANIFEST} and {RELEASE_MANIFEST} are in sync and match Cargo package version "
        f"{package_version}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
