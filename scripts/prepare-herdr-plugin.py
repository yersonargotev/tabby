#!/usr/bin/env python3
"""Prepare Tabby's canonical plugin-root executable from a debug build."""

from __future__ import annotations

import argparse
import os
import shutil
import sys
import tempfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
CANONICAL_BINARY = Path(".herdr/bin/tabby")


class PreparationError(RuntimeError):
    pass


def prepare(plugin_root: Path, debug_binary: Path) -> Path:
    plugin_root = plugin_root.resolve()
    debug_binary = debug_binary.resolve()
    destination = plugin_root / CANONICAL_BINARY

    if not debug_binary.is_file():
        raise PreparationError(
            f"debug binary not found at {debug_binary}; run `cargo build` first"
        )
    if not os.access(debug_binary, os.X_OK):
        raise PreparationError(f"debug binary is not executable: {debug_binary}")

    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            prefix=".tabby.", dir=destination.parent, delete=False
        ) as temporary:
            temporary_path = Path(temporary.name)
            with debug_binary.open("rb") as source:
                shutil.copyfileobj(source, temporary)
        shutil.copymode(debug_binary, temporary_path)
        os.replace(temporary_path, destination)
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)

    return destination


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plugin-root", type=Path, default=REPO_ROOT)
    parser.add_argument("--debug-binary", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    debug_binary = args.debug_binary or args.plugin_root / "target/debug/tabby"
    try:
        destination = prepare(args.plugin_root, debug_binary)
    except (OSError, PreparationError) as error:
        print(f"prepare-herdr-plugin: {error}", file=sys.stderr)
        return 1
    print(f"prepared Tabby plugin executable at {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
