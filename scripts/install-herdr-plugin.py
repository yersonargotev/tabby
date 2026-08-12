#!/usr/bin/env python3
"""Install Tabby's verified release binary into its canonical plugin root."""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
from pathlib import Path
from typing import Protocol


REPO_ROOT = Path(__file__).resolve().parents[1]
CANONICAL_BINARY = Path(".herdr/bin/tabby")
RELEASE_BASE_URL = "https://github.com/yersonargotev/tabby/releases/download"
DIAGNOSTIC_MAX_CHARS = 4096
DOWNLOAD_MAX_BYTES = 128 * 1024 * 1024


class InstallError(RuntimeError):
    pass


class ArtifactNotFound(InstallError):
    pass


class InstallerAdapter(Protocol):
    def platform(self) -> tuple[str, str]:
        ...

    def download(self, url: str, destination: Path) -> None:
        ...

    def build_from_source(self, plugin_root: Path) -> Path:
        ...


class SystemAdapter:
    def platform(self) -> tuple[str, str]:
        return platform.system(), platform.machine()

    def download(self, url: str, destination: Path) -> None:
        request = urllib.request.Request(url, headers={"User-Agent": "tabby-herdr-installer"})
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                total = 0
                with destination.open("wb") as output:
                    while True:
                        chunk = response.read(64 * 1024)
                        if not chunk:
                            break
                        total += len(chunk)
                        if total > DOWNLOAD_MAX_BYTES:
                            raise InstallError(
                                f"download exceeded {DOWNLOAD_MAX_BYTES} bytes: {url}"
                            )
                        output.write(chunk)
        except urllib.error.HTTPError as error:
            if error.code == 404:
                raise ArtifactNotFound(f"release asset not found: {url}") from error
            raise InstallError(f"download failed with HTTP {error.code}: {url}") from error
        except urllib.error.URLError as error:
            raise InstallError(f"network failure downloading {url}: {error.reason}") from error
        except OSError as error:
            raise InstallError(f"could not save download {url}: {error}") from error

    def build_from_source(self, plugin_root: Path) -> Path:
        cargo = shutil.which("cargo")
        rustc = shutil.which("rustc")
        if cargo is None or rustc is None:
            missing = " and ".join(
                f"`{tool}`" for tool, path in (("cargo", cargo), ("rustc", rustc)) if path is None
            )
            raise InstallError(
                f"no matching release artifact exists and {missing} was not found; "
                "install the documented stable Rust toolchain or use Apple Silicon macOS"
            )
        try:
            with tempfile.TemporaryFile() as output:
                completed = subprocess.run(
                    [cargo, "build", "--release", "--locked"],
                    cwd=plugin_root,
                    stdin=subprocess.DEVNULL,
                    stdout=output,
                    stderr=subprocess.STDOUT,
                    check=False,
                )
                if completed.returncode != 0:
                    output.seek(0, os.SEEK_END)
                    size = output.tell()
                    output.seek(max(0, size - DIAGNOSTIC_MAX_CHARS))
                    diagnostic = output.read().decode("utf-8", errors="replace").strip()
                    raise InstallError(
                        "source build failed with "
                        f"exit code {completed.returncode}: {diagnostic or 'cargo produced no output'}"
                    )
        except OSError as error:
            raise InstallError(f"could not run cargo source build: {error}") from error

        binary = plugin_root / "target/release/tabby"
        if not binary.is_file() or not os.access(binary, os.X_OK):
            raise InstallError(
                f"source build did not produce an executable Tabby binary at {binary}"
            )
        return binary


def manifest_version(manifest: Path) -> str:
    version: str | None = None
    for line in manifest.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("[["):
            break
        match = re.fullmatch(r'version\s*=\s*"([^"\s]+)"', stripped)
        if match:
            if version is not None:
                raise InstallError(f"manifest has more than one top-level version: {manifest}")
            version = match.group(1)
    if version is None:
        raise InstallError(f"manifest is missing a top-level version: {manifest}")
    if re.fullmatch(
        r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?",
        version,
    ) is None:
        raise InstallError(f"manifest version is not valid SemVer: {version}")
    return version


def selected_asset(adapter: InstallerAdapter) -> str:
    system, architecture = adapter.platform()
    if system == "Darwin" and architecture == "arm64":
        return "tabby-aarch64-apple-darwin.tar.xz"
    raise InstallError(
        "unsupported platform "
        f"{system}/{architecture}; Tabby's GitHub installer supports Apple Silicon macOS"
    )


def parse_checksum(raw: bytes, asset: str) -> str:
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise InstallError(f"checksum for {asset} is not valid UTF-8") from error
    lines = [line for line in text.splitlines() if line.strip()]
    if len(lines) != 1:
        raise InstallError(f"checksum for {asset} must contain exactly one entry")
    match = re.fullmatch(r"([0-9A-Fa-f]{64}) [ *](.+)", lines[0])
    if match is None or match.group(2) != asset:
        raise InstallError(f"checksum for {asset} is malformed or names a different asset")
    return match.group(1).lower()


def archive_binary(archive: Path, asset: str, destination: Path) -> None:
    member_name = f"{asset.removesuffix('.tar.xz')}/tabby"
    try:
        with tarfile.open(archive, mode="r:xz") as bundle:
            try:
                member = bundle.getmember(member_name)
            except KeyError as error:
                raise InstallError(f"archive {asset} does not contain {member_name}") from error
            if not member.isfile():
                raise InstallError(f"archive entry {member_name} is not a regular file")
            source = bundle.extractfile(member)
            if source is None:
                raise InstallError(f"archive entry {member_name} could not be read")
            with source, destination.open("wb") as output:
                shutil.copyfileobj(source, output)
    except (tarfile.TarError, OSError) as error:
        raise InstallError(f"could not extract {asset}: {error}") from error


def install_atomic(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    instances = destination.parent / ".instances"
    instances_preexisting = instances.exists()
    try:
        instances.mkdir(mode=0o700, exist_ok=True)
        if instances.is_symlink() or not instances.is_dir():
            raise InstallError(f"executable instances path is not a real directory: {instances}")
        instances.chmod(0o700)
    except OSError as error:
        raise InstallError(f"could not prepare executable instances at {instances}: {error}") from error

    instance: Path | None = None
    temporary_link: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            prefix="tabby.", dir=instances, delete=False
        ) as output:
            instance = Path(output.name)
            with source.open("rb") as input_file:
                shutil.copyfileobj(input_file, output)
            output.flush()
            os.fsync(output.fileno())
        instance.chmod(0o755)

        with tempfile.NamedTemporaryFile(
            prefix=".tabby-link.", dir=destination.parent, delete=False
        ) as link_placeholder:
            temporary_link = Path(link_placeholder.name)
        temporary_link.unlink()
        temporary_link.symlink_to(Path(".instances") / instance.name)
        os.replace(temporary_link, destination)
        temporary_link = None

        active_instance = instance
        instance = None
        for candidate in instances.iterdir():
            if candidate != active_instance and candidate.name.startswith("tabby."):
                try:
                    candidate.unlink()
                except OSError:
                    pass
    except OSError as error:
        raise InstallError(f"could not install {destination}: {error}") from error
    finally:
        if temporary_link is not None:
            temporary_link.unlink(missing_ok=True)
        if instance is not None:
            instance.unlink(missing_ok=True)
        if not instances_preexisting:
            try:
                instances.rmdir()
            except OSError:
                pass


def install(plugin_root: Path, adapter: InstallerAdapter) -> Path:
    plugin_root = plugin_root.resolve()
    version = manifest_version(plugin_root / "herdr-plugin.toml")
    asset = selected_asset(adapter)
    release_url = f"{RELEASE_BASE_URL}/v{version}"
    destination = plugin_root / CANONICAL_BINARY

    with tempfile.TemporaryDirectory(prefix="tabby-install-") as temp_dir:
        work = Path(temp_dir)
        archive = work / asset
        try:
            adapter.download(f"{release_url}/{asset}", archive)
        except ArtifactNotFound:
            source_binary = adapter.build_from_source(plugin_root)
            install_atomic(source_binary, destination)
            return destination
        checksum_file = work / f"{asset}.sha256"
        try:
            adapter.download(f"{release_url}/{asset}.sha256", checksum_file)
        except ArtifactNotFound as error:
            raise InstallError(f"published checksum is missing for {asset}") from error

        expected = parse_checksum(checksum_file.read_bytes(), asset)
        actual = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual != expected:
            raise InstallError(
                f"checksum mismatch for {asset}: expected {expected}, got {actual}"
            )

        extracted = work / "tabby"
        archive_binary(archive, asset, extracted)
        install_atomic(extracted, destination)
    return destination


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plugin-root", type=Path, default=REPO_ROOT)
    args = parser.parse_args()
    try:
        destination = install(args.plugin_root, SystemAdapter())
    except (InstallError, OSError) as error:
        message = str(error)
        if len(message) > DIAGNOSTIC_MAX_CHARS:
            prefix = "[earlier diagnostic omitted] "
            message = prefix + message[-(DIAGNOSTIC_MAX_CHARS - len(prefix)) :]
        print(f"install-herdr-plugin: {message}", file=sys.stderr)
        return 1
    print(f"installed verified Tabby executable at {destination}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
