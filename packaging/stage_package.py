#!/usr/bin/env python3
"""Stage a self-describing RGV package tree for macOS, Linux, or Windows."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import plistlib
import shutil
import stat
import sys
from typing import Optional

import release_metadata


ROOT = Path(__file__).resolve().parent.parent
MANIFEST_NAME = "PACKAGE-MANIFEST.sha256"


def copy(source: Path, destination: Path, *, executable: bool = False) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)
    if executable:
        destination.chmod(destination.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def require_empty(destination: Path) -> None:
    if destination.exists() and any(destination.iterdir()):
        raise ValueError(f"staging destination is not empty: {destination}")
    destination.mkdir(parents=True, exist_ok=True)


def stage_macos(
    binary: Path,
    destination: Path,
    version: str,
    build_number: str,
    mcp_binary: Optional[Path] = None,
) -> Path:
    root = destination / "RGV.app"
    executable = root / "Contents" / "MacOS" / "rgv-desktop"
    resources = root / "Contents" / "Resources"
    copy(binary, executable, executable=True)
    if mcp_binary is not None:
        copy(mcp_binary, root / "Contents" / "MacOS" / "rgv-mcp", executable=True)
    plist = plistlib.loads((ROOT / "packaging/macos/Info.plist").read_bytes())
    plist["CFBundleShortVersionString"] = version
    plist["CFBundleVersion"] = build_number
    plist["CFBundleExecutable"] = executable.name
    plist_path = root / "Contents" / "Info.plist"
    plist_path.parent.mkdir(parents=True, exist_ok=True)
    plist_path.write_bytes(plistlib.dumps(plist, sort_keys=True))
    copy(ROOT / "packaging/icons/rgv.icns", resources / "rgv.icns")
    stage_common(binary, resources, "macos", version, mcp_binary)
    return root


def stage_linux(
    binary: Path,
    destination: Path,
    architecture: str,
    version: str,
    mcp_binary: Optional[Path] = None,
) -> Path:
    root = destination / f"RGV-linux-{architecture}"
    copy(binary, root / "bin" / "rgv-desktop", executable=True)
    if mcp_binary is not None:
        copy(mcp_binary, root / "bin" / "rgv-mcp", executable=True)
    copy(
        ROOT / "packaging/linux/rgv.desktop",
        root / "share" / "applications" / "rgv.desktop",
    )
    copy(
        ROOT / "packaging/icons/rgv.svg",
        root / "share" / "icons" / "hicolor" / "scalable" / "apps" / "rgv.svg",
    )
    stage_common(
        binary,
        root / "share" / "doc" / "rgv",
        f"linux-{architecture}",
        version,
        mcp_binary,
    )
    return root


def stage_windows(
    binary: Path,
    destination: Path,
    architecture: str,
    version: str,
    mcp_binary: Optional[Path] = None,
) -> Path:
    root = destination / f"RGV-windows-{architecture}"
    copy(binary, root / "rgv-desktop.exe", executable=True)
    if mcp_binary is not None:
        copy(mcp_binary, root / "rgv-mcp.exe", executable=True)
    stage_common(binary, root / "licenses", f"windows-{architecture}", version, mcp_binary)
    return root


def stage_common(
    binary: Path,
    resources: Path,
    target: str,
    version: str,
    mcp_binary: Optional[Path] = None,
) -> None:
    resources.mkdir(parents=True, exist_ok=True)
    copy(ROOT / "LICENSE-MIT", resources / "LICENSE-MIT")
    copy(ROOT / "LICENSE-APACHE", resources / "LICENSE-APACHE")
    copy(ROOT / "README.md", resources / "README.md")
    release_metadata.generate(
        binary,
        target,
        version,
        resources,
        additional_binaries=() if mcp_binary is None else (mcp_binary,),
    )
    release_metadata.check(resources)


def file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def included_in_manifest(root: Path, path: Path) -> bool:
    relative = path.relative_to(root)
    if path.name == MANIFEST_NAME or "_CodeSignature" in relative.parts:
        return False
    # The Mach-O code signature is embedded in the main executable after staging. The signed DMG
    # and GitHub provenance cover that final code object; this inner manifest covers the immutable
    # resources without creating an impossible signature/hash cycle.
    return not (root.suffix == ".app" and relative.parts[:2] == ("Contents", "MacOS"))


def manifest_path(root: Path) -> Path:
    if root.suffix == ".app":
        return root / "Contents" / "Resources" / MANIFEST_NAME
    return root / MANIFEST_NAME


def write_manifest(root: Path) -> Path:
    entries = []
    for path in sorted(
        path for path in root.rglob("*") if path.is_file() and included_in_manifest(root, path)
    ):
        relative = path.relative_to(root).as_posix()
        entries.append(f"{file_hash(path)}  {relative}")
    manifest = manifest_path(root)
    manifest.parent.mkdir(parents=True, exist_ok=True)
    manifest.write_text("\n".join(entries) + "\n", encoding="utf-8")
    return manifest


def verify_manifest(root: Path) -> None:
    manifest = manifest_path(root)
    if not manifest.is_file():
        raise ValueError(f"package manifest is missing from {root}")
    expected_paths = set()
    for line in manifest.read_text(encoding="utf-8").splitlines():
        digest, separator, relative = line.partition("  ")
        if not separator or len(digest) != 64 or relative.startswith(("/", "../")):
            raise ValueError(f"invalid package manifest line: {line}")
        path = root / relative
        if not path.is_file() or file_hash(path) != digest:
            raise ValueError(f"package manifest mismatch: {relative}")
        expected_paths.add(relative)
    actual_paths = {
        path.relative_to(root).as_posix()
        for path in root.rglob("*")
        if path.is_file() and included_in_manifest(root, path)
    }
    if actual_paths != expected_paths:
        raise ValueError("package contains files absent from its manifest")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", choices=["macos", "linux", "windows"], required=True)
    parser.add_argument("--architecture", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--build-number", default="1")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--mcp-binary", type=Path)
    parser.add_argument("--destination", type=Path, required=True)
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    mcp_binary = arguments.mcp_binary.resolve(strict=True) if arguments.mcp_binary else None
    require_empty(arguments.destination)
    if arguments.platform == "macos":
        root = stage_macos(
            binary,
            arguments.destination,
            arguments.version,
            arguments.build_number,
            mcp_binary,
        )
    elif arguments.platform == "linux":
        root = stage_linux(
            binary,
            arguments.destination,
            arguments.architecture,
            arguments.version,
            mcp_binary,
        )
    else:
        root = stage_windows(
            binary,
            arguments.destination,
            arguments.architecture,
            arguments.version,
            mcp_binary,
        )
    write_manifest(root)
    verify_manifest(root)
    print(root)
    return 0


if __name__ == "__main__":
    sys.exit(main())
