#!/usr/bin/env python3
"""Validate the cargo-dist plan and built assets consumed by Herdr installation."""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from types import ModuleType
from typing import Any, Sequence


REPO_ROOT = Path(__file__).resolve().parents[1]
TARGET = "aarch64-apple-darwin"


def load_script(name: str, path: Path) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


manifest_checker = load_script(
    "tabby_check_herdr_manifests", REPO_ROOT / "scripts/check-herdr-manifests.py"
)
installer = load_script(
    "tabby_install_herdr_plugin", REPO_ROOT / "scripts/install-herdr-plugin.py"
)


class AppleSiliconAdapter:
    def platform(self) -> tuple[str, str]:
        return "Darwin", "arm64"


def installer_asset() -> str:
    return str(installer.selected_asset(AppleSiliconAdapter()))


def release_plan_errors(plan: dict[str, Any], version: str, tag: str | None) -> list[str]:
    errors: list[str] = []
    asset = installer_asset()
    checksum = f"{asset}.sha256"
    expected_tag = f"v{version}"

    releases = plan.get("releases")
    matching_release = None
    if isinstance(releases, list):
        matching = [release for release in releases if release.get("app_name") == "tabby"]
        if len(matching) == 1:
            matching_release = matching[0]
    if matching_release is None:
        errors.append("release plan must contain exactly one Tabby release")
    else:
        if matching_release.get("app_version") != version:
            errors.append(
                "release version must match Cargo package version "
                f"{version!r}, got {matching_release.get('app_version')!r}"
            )
        release_artifacts = matching_release.get("artifacts", [])
        if asset not in release_artifacts:
            errors.append(f"release archive is missing: {asset}")
        if checksum not in release_artifacts:
            errors.append(f"release checksum sidecar is missing: {checksum}")

    if plan.get("announcement_tag") != expected_tag:
        errors.append(
            f"planned announcement tag must be {expected_tag!r}, "
            f"got {plan.get('announcement_tag')!r}"
        )
    if tag is not None and tag != expected_tag:
        errors.append(f"Git tag must be {expected_tag!r}, got {tag!r}")

    artifacts = plan.get("artifacts", {})
    archive = artifacts.get(asset, {}) if isinstance(artifacts, dict) else {}
    sidecar = artifacts.get(checksum, {}) if isinstance(artifacts, dict) else {}
    if archive.get("checksum") != checksum:
        errors.append(
            f"checksum relationship for {asset} must name {checksum!r}, "
            f"got {archive.get('checksum')!r}"
        )
    if archive.get("target_triples") != [TARGET]:
        errors.append(
            f"archive target must be exactly {TARGET!r}, "
            f"got {archive.get('target_triples')!r}"
        )
    if sidecar.get("kind") != "checksum":
        errors.append(f"checksum sidecar {checksum} must be a cargo-dist checksum artifact")
    if sidecar.get("target_triples") != [TARGET]:
        errors.append(
            f"checksum target must be exactly {TARGET!r}, "
            f"got {sidecar.get('target_triples')!r}"
        )

    pr_mode = plan.get("ci", {}).get("github", {}).get("pr_run_mode")
    if pr_mode != "upload":
        errors.append(
            "PR build mode must be 'upload' so pull requests build the production "
            f"archive, got {pr_mode!r}"
        )
    return errors


def built_artifact_errors(
    artifact_dir: Path, require_homebrew_formula: bool = False
) -> list[str]:
    asset = installer_asset()
    checksum_name = f"{asset}.sha256"
    archives = list(artifact_dir.rglob(asset))
    checksums = list(artifact_dir.rglob(checksum_name))
    errors: list[str] = []
    if len(archives) != 1:
        errors.append(f"built archive must exist exactly once, found {len(archives)}")
    if len(checksums) != 1:
        errors.append(f"built checksum is missing or duplicated, found {len(checksums)}")
    if errors:
        return errors

    try:
        expected = installer.parse_checksum(checksums[0].read_bytes(), asset)
    except (OSError, installer.InstallError) as error:
        message = str(error)
        if "names a different asset" in message:
            errors.append(f"built checksum names a different asset: {message}")
        else:
            errors.append(f"built checksum is malformed: {message}")
        return errors
    actual = hashlib.sha256(archives[0].read_bytes()).hexdigest()
    if actual != expected:
        errors.append(f"built checksum digest mismatch: expected {expected}, got {actual}")
    if require_homebrew_formula:
        formulas = list(artifact_dir.rglob("tabby.rb"))
        if len(formulas) != 1:
            errors.append(
                f"Homebrew formula must exist exactly once, found {len(formulas)}"
            )
        else:
            formula = formulas[0].read_text()
            if "def post_install" in formula or "def postinstall" in formula:
                errors.append(
                    "Homebrew postinstall hooks are forbidden; registration and "
                    "runtime activation must remain explicit"
                )
    return errors


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--dist-manifest", type=Path)
    mode.add_argument("--artifact-dir", type=Path)
    parser.add_argument("--tag")
    parser.add_argument("--require-homebrew-formula", action="store_true")
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        if args.dist_manifest is not None:
            version = manifest_checker.cargo_package_version(REPO_ROOT / "Cargo.toml")
            errors = manifest_checker.check_manifests(
                REPO_ROOT / "herdr-plugin.toml",
                REPO_ROOT / "packaging/herdr/herdr-plugin.toml",
                REPO_ROOT / "Cargo.toml",
            )
            plan = json.loads(args.dist_manifest.read_text())
            errors.extend(release_plan_errors(plan, version, args.tag))
            success = f"release plan matches Tabby {version} and the native Herdr contract"
        else:
            errors = built_artifact_errors(
                args.artifact_dir, args.require_homebrew_formula
            )
            success = f"built archive checksum matches {installer_asset()}"
    except (OSError, ValueError, json.JSONDecodeError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(success)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
