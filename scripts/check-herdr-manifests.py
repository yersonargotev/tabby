#!/usr/bin/env python3
"""Check that Tabby's dev and release Herdr manifests only differ where intended."""

from __future__ import annotations

import ast
import sys
from pathlib import Path
from typing import Any

DEV_MANIFEST = Path("herdr-plugin.toml")
RELEASE_MANIFEST = Path("packaging/herdr/herdr-plugin.toml")
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
    manifest: dict[str, Any] = {"actions": []}
    current_action: dict[str, Any] | None = None

    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped == "[[actions]]":
            current_action = {}
            manifest["actions"].append(current_action)
            continue
        if "=" not in stripped:
            raise ValueError(f"{path}:{line_number}: unsupported TOML line: {line!r}")

        key, raw_value = stripped.split("=", 1)
        target = current_action if current_action is not None else manifest
        target[key.strip()] = parse_value(raw_value)

    return manifest


def action_map(manifest: dict[str, Any]) -> dict[str, dict[str, Any]]:
    actions = manifest.get("actions", [])
    return {action["id"]: action for action in actions}


def main() -> int:
    dev = load_manifest(DEV_MANIFEST)
    release = load_manifest(RELEASE_MANIFEST)
    errors: list[str] = []

    for key in ["id", "name", "version", "min_herdr_version", "platforms"]:
        if dev.get(key) != release.get(key):
            errors.append(
                f"{key} differs: {DEV_MANIFEST} has {dev.get(key)!r}, "
                f"{RELEASE_MANIFEST} has {release.get(key)!r}"
            )

    dev_actions = action_map(dev)
    release_actions = action_map(release)
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
        if not dev_command or dev_command[0] != DEV_BINARY:
            errors.append(
                f"dev action {action_id!r} must invoke {DEV_BINARY!r}, got {dev_command!r}"
            )
        if not release_command or release_command[0] != RELEASE_BINARY:
            errors.append(
                f"release action {action_id!r} must invoke {RELEASE_BINARY!r}, got {release_command!r}"
            )
        if dev_command[1:] != release_command[1:]:
            errors.append(
                f"action {action_id!r} command args differ after binary path: "
                f"{dev_command[1:]!r} != {release_command[1:]!r}"
            )

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(f"{DEV_MANIFEST} and {RELEASE_MANIFEST} are in sync")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
