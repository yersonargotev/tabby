#!/usr/bin/env python3
"""Validate Tabby's canonical and Homebrew Herdr manifest adapters."""

from __future__ import annotations

import argparse
import ast
import copy
import sys
from pathlib import Path
from typing import Any, Sequence


CANONICAL_MANIFEST = Path("herdr-plugin.toml")
HOMEBREW_MANIFEST = Path("packaging/herdr/herdr-plugin.toml")
CARGO_MANIFEST = Path("Cargo.toml")
CANONICAL_BINARY = ".herdr/bin/tabby"
HOMEBREW_BINARY = "../../bin/tabby"
COLLECTIONS = ("build", "startup", "actions", "events")
ACTION_COMMANDS = {
    "start": ["start"],
    "refresh": ["refresh"],
    "config-path": ["config", "path"],
    "config-check": ["config", "check"],
    "config-reload": ["config", "reload"],
    "unlock-focused": ["unlock-focused"],
    "unlock-all": ["unlock-all"],
}
EVENT_COMMANDS = {
    "pane.focused": ["signal-focus"],
    "workspace.created": ["signal-created"],
    "tab.created": ["signal-created"],
}


def parse_value(raw: str) -> Any:
    value = raw.strip()
    if value.startswith("["):
        return ast.literal_eval(value)
    if value.startswith('"') and value.endswith('"'):
        return value[1:-1]
    return value


def load_manifest(path: Path) -> dict[str, Any]:
    manifest: dict[str, Any] = {name: [] for name in COLLECTIONS}
    current_section: dict[str, Any] | None = None

    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("[[") and stripped.endswith("]]"):
            section_name = stripped[2:-2]
            if section_name not in COLLECTIONS:
                raise ValueError(
                    f"{path}:{line_number}: unsupported TOML section: {stripped!r}"
                )
            current_section = {}
            manifest[section_name].append(current_section)
            continue
        if "=" not in stripped:
            raise ValueError(f"{path}:{line_number}: unsupported TOML line: {line!r}")

        key, raw_value = stripped.split("=", 1)
        target = current_section if current_section is not None else manifest
        target[key.strip()] = parse_value(raw_value)

    return manifest


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


def entry_name(collection: str, entry: dict[str, Any], index: int) -> str:
    if collection == "actions":
        return str(entry.get("id", index))
    if collection == "events":
        return str(entry.get("on", index))
    return str(index)


def check_binary_contract(
    errors: list[str],
    manifest_path: Path,
    manifest: dict[str, Any],
    expected_binary: str,
) -> None:
    for collection in ("startup", "actions", "events"):
        for index, entry in enumerate(manifest[collection]):
            command = entry.get("command")
            name = entry_name(collection, entry, index)
            if not isinstance(command, list) or not command:
                errors.append(
                    f"{manifest_path} {collection} {name!r} must declare a command"
                )
            elif command[0] != expected_binary:
                errors.append(
                    f"{manifest_path} {collection} {name!r} must invoke "
                    f"{expected_binary!r}, got {command!r}"
                )
            if not isinstance(command, list) or not command:
                continue
            expected_args = None
            if collection == "startup":
                expected_args = ["ensure-started"]
            elif collection == "actions":
                expected_args = ACTION_COMMANDS.get(entry.get("id"))
            elif collection == "events":
                expected_args = EVENT_COMMANDS.get(entry.get("on"))
            if expected_args is not None and command[1:] != expected_args:
                errors.append(
                    f"{manifest_path} {collection[:-1]} {name!r} must run "
                    f"{expected_args!r}, got {command[1:]!r}"
                )


def normalized_product_semantics(manifest: dict[str, Any]) -> dict[str, Any]:
    normalized = copy.deepcopy(manifest)
    normalized.pop("build", None)
    for collection in ("startup", "actions", "events"):
        for entry in normalized[collection]:
            command = entry.get("command")
            if isinstance(command, list) and command:
                command[0] = "<distribution-binary>"
    return normalized


def check_manifests(
    canonical_path: Path, homebrew_path: Path, cargo_path: Path
) -> list[str]:
    canonical = load_manifest(canonical_path)
    homebrew = load_manifest(homebrew_path)
    package_version = cargo_package_version(cargo_path)
    errors: list[str] = []

    allowed_top_level_keys = {
        "id",
        "name",
        "version",
        "min_herdr_version",
        "description",
        "platforms",
        *COLLECTIONS,
    }
    for manifest_path, manifest in (
        (canonical_path, canonical),
        (homebrew_path, homebrew),
    ):
        extra_keys = set(manifest) - allowed_top_level_keys
        if extra_keys:
            errors.append(
                f"{manifest_path} has unsupported top-level keys: {sorted(extra_keys)}"
            )

    if homebrew["build"]:
        errors.append(f"{homebrew_path} must not declare distribution build commands")
    expected_build = [{"command": ["python3", "scripts/install-herdr-plugin.py"]}]
    if canonical["build"] != expected_build:
        errors.append(
            f"{canonical_path} must declare exactly one distribution build command: "
            f"{expected_build[0]['command']!r}"
        )

    if normalized_product_semantics(canonical) != normalized_product_semantics(homebrew):
        errors.append(
            "product semantics differ after allowing only distribution build and "
            f"executable paths: {canonical_path} != {homebrew_path}"
        )

    check_binary_contract(errors, canonical_path, canonical, CANONICAL_BINARY)
    check_binary_contract(errors, homebrew_path, homebrew, HOMEBREW_BINARY)

    if canonical.get("min_herdr_version") != "0.8.0":
        errors.append(
            f"{canonical_path} min_herdr_version must be '0.8.0', "
            f"got {canonical.get('min_herdr_version')!r}"
        )
    if canonical.get("version") != package_version:
        errors.append(
            f"canonical manifest version {canonical.get('version')!r} must match "
            f"Cargo package version {package_version!r}"
        )
    if homebrew.get("version") != package_version:
        errors.append(
            f"Homebrew manifest version {homebrew.get('version')!r} must match "
            f"Cargo package version {package_version!r}"
        )
    if len(canonical["startup"]) != 1 or len(homebrew["startup"]) != 1:
        errors.append("each manifest must declare exactly one startup command")

    expected_actions = {
        "start",
        "refresh",
        "config-path",
        "config-check",
        "config-reload",
        "unlock-focused",
        "unlock-all",
    }
    for manifest_path, manifest in (
        (canonical_path, canonical),
        (homebrew_path, homebrew),
    ):
        action_ids = [action.get("id") for action in manifest["actions"]]
        if len(action_ids) != len(set(action_ids)):
            errors.append(f"{manifest_path} action ids must be unique")
        if set(action_ids) != expected_actions:
            errors.append(
                f"{manifest_path} action ids must be {sorted(expected_actions)}, "
                f"got {sorted(str(value) for value in set(action_ids))}"
            )
        event_names = [event.get("on") for event in manifest["events"]]
        expected_events = {"pane.focused", "workspace.created", "tab.created"}
        if len(event_names) != len(set(event_names)):
            errors.append(f"{manifest_path} event hooks must be unique")
        if set(event_names) != expected_events:
            errors.append(
                f"{manifest_path} event hooks must be {sorted(expected_events)}, "
                f"got {sorted(str(value) for value in set(event_names))}"
            )

    return errors


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--canonical-manifest", type=Path, default=CANONICAL_MANIFEST)
    parser.add_argument("--homebrew-manifest", type=Path, default=HOMEBREW_MANIFEST)
    parser.add_argument("--cargo-manifest", type=Path, default=CARGO_MANIFEST)
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        errors = check_manifests(
            args.canonical_manifest, args.homebrew_manifest, args.cargo_manifest
        )
        package_version = cargo_package_version(args.cargo_manifest)
    except (OSError, SyntaxError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1

    print(
        f"{args.canonical_manifest} and {args.homebrew_manifest} have identical product "
        f"semantics and match Cargo package version {package_version}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
