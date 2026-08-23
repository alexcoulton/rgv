#!/usr/bin/env python3
"""Generate deterministic release SBOM, notices, and build provenance without external tools."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.parse
import uuid


ROOT = Path(__file__).resolve().parent.parent
SBOM_NAME = "RGV.cdx.json"
NOTICES_NAME = "THIRD_PARTY_LICENSES.txt"
PROVENANCE_NAME = "BUILD-PROVENANCE.json"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def command_output(arguments: list[str], *, required: bool = True) -> str | None:
    try:
        result = subprocess.run(
            arguments,
            cwd=ROOT,
            check=required,
            capture_output=True,
            text=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        if required:
            raise
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def cargo_metadata() -> dict:
    verbose_version = command_output(["rustc", "--version", "--verbose"])
    assert verbose_version is not None
    host = next(
        (line.split(":", 1)[1].strip() for line in verbose_version.splitlines() if line.startswith("host:")),
        None,
    )
    if host is None:
        raise ValueError("rustc did not report its host target")
    raw = command_output(
        [
            "cargo",
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            host,
        ]
    )
    assert raw is not None
    return json.loads(raw)


def reachable_package_ids(metadata: dict, root_name: str = "rgv-desktop") -> set[str]:
    packages = metadata["packages"]
    roots = [package["id"] for package in packages if package["name"] == root_name]
    if len(roots) != 1:
        raise ValueError(f"expected exactly one {root_name} package, found {len(roots)}")
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = roots[:]
    reachable: set[str] = set()
    while pending:
        package_id = pending.pop()
        if package_id in reachable:
            continue
        reachable.add(package_id)
        node = nodes.get(package_id)
        if node is not None:
            pending.extend(
                dependency["pkg"]
                for dependency in node.get("deps", [])
                if any(kind.get("kind") != "dev" for kind in dependency.get("dep_kinds", [{}]))
            )
    return reachable


def lock_checksums() -> dict[tuple[str, str, str | None], str]:
    # Cargo.lock uses a deliberately small, generated TOML subset. Parsing only these four scalar
    # package keys keeps release staging compatible with the Python 3.9 shipped by older macOS
    # runners without adding an unaudited packaging dependency.
    packages = []
    package: dict[str, str] | None = None
    for raw_line in (ROOT / "Cargo.lock").read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line == "[[package]]":
            if package is not None:
                packages.append(package)
            package = {}
            continue
        if package is None or "=" not in line:
            continue
        key, value = (part.strip() for part in line.split("=", 1))
        if key in {"name", "version", "source", "checksum"} and value.startswith('"'):
            package[key] = json.loads(value)
    if package is not None:
        packages.append(package)
    return {
        (package["name"], package["version"], package.get("source")): package["checksum"]
        for package in packages
        if all(key in package for key in ("name", "version", "checksum"))
    }


def package_purl(package: dict) -> str:
    name = urllib.parse.quote(package["name"], safe="._-")
    version = urllib.parse.quote(package["version"], safe="._-+")
    source = package.get("source") or "workspace"
    qualifier = urllib.parse.urlencode({"source": source})
    return f"pkg:cargo/{name}@{version}?{qualifier}"


def build_sbom(metadata: dict, version: str, timestamp: str) -> tuple[dict, list[dict]]:
    reachable = reachable_package_ids(metadata)
    packages = [package for package in metadata["packages"] if package["id"] in reachable]
    packages.sort(key=lambda package: (package["name"], package["version"], package["id"]))
    checksum_by_package = lock_checksums()
    refs = {package["id"]: package_purl(package) for package in packages}
    components = []
    for package in packages:
        component = {
            "type": "application" if package["name"] == "rgv-desktop" else "library",
            "bom-ref": refs[package["id"]],
            "name": package["name"],
            "version": package["version"],
            "purl": refs[package["id"]],
        }
        if package.get("license"):
            component["licenses"] = [{"expression": package["license"]}]
        checksum = checksum_by_package.get(
            (package["name"], package["version"], package.get("source"))
        )
        if checksum and len(checksum) == 64:
            component["hashes"] = [{"alg": "SHA-256", "content": checksum}]
        references = []
        if package.get("repository"):
            references.append({"type": "vcs", "url": package["repository"]})
        if package.get("homepage"):
            references.append({"type": "website", "url": package["homepage"]})
        if references:
            component["externalReferences"] = references
        components.append(component)

    node_by_id = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    dependencies = []
    for package in packages:
        node = node_by_id.get(package["id"], {})
        dependency_refs = sorted(
            refs[dependency["pkg"]]
            for dependency in node.get("deps", [])
            if dependency["pkg"] in refs
            and any(kind.get("kind") != "dev" for kind in dependency.get("dep_kinds", [{}]))
        )
        dependencies.append({"ref": refs[package["id"]], "dependsOn": dependency_refs})
    dependencies.sort(key=lambda dependency: dependency["ref"])

    root_package = next(package for package in packages if package["name"] == "rgv-desktop")
    serial_seed = "\n".join(component["bom-ref"] for component in components)
    serial = uuid.uuid5(uuid.NAMESPACE_URL, f"rgv:{version}:{serial_seed}")
    sbom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "serialNumber": f"urn:uuid:{serial}",
        "version": 1,
        "metadata": {
            "timestamp": timestamp,
            "component": next(
                component for component in components if component["name"] == root_package["name"]
            ),
            "tools": {
                "components": [
                    {
                        "type": "application",
                        "name": "RGV release metadata generator",
                        "version": "1",
                    }
                ]
            },
        },
        "components": components,
        "dependencies": dependencies,
    }
    return sbom, packages


def package_license_files(package: dict) -> list[Path]:
    package_root = Path(package["manifest_path"]).parent
    candidates = []
    if package.get("license_file"):
        candidates.append(package_root / package["license_file"])
    for pattern in ("LICENSE*", "COPYING*", "NOTICE*", "UNLICENSE"):
        candidates.extend(package_root.glob(pattern))
        candidates.extend((package_root / "LICENSES").glob(pattern))
    return sorted({path.resolve() for path in candidates if path.is_file()})


def build_notices(packages: list[dict]) -> str:
    material_by_digest: dict[str, tuple[str, str]] = {}
    package_materials: dict[str, list[str]] = {}
    total_bytes = 0
    for package in packages:
        if package.get("source") is None:
            continue
        key = package["id"]
        digests = []
        for path in package_license_files(package):
            data = path.read_bytes()
            if len(data) > 1024 * 1024:
                raise ValueError(f"dependency license file exceeds 1 MiB: {key} {path.name}")
            digest = hashlib.sha256(data).hexdigest()
            if digest not in material_by_digest:
                total_bytes += len(data)
                if total_bytes > 64 * 1024 * 1024:
                    raise ValueError("combined dependency license material exceeds 64 MiB")
                material_by_digest[digest] = (
                    path.name,
                    data.decode("utf-8", errors="replace").rstrip(),
                )
            digests.append(digest)
        package_materials[key] = sorted(set(digests))
    material_ids = {
        digest: f"L{index:04d}"
        for index, digest in enumerate(sorted(material_by_digest), start=1)
    }
    lines = [
        "RGV third-party dependency notices",
        "==================================",
        "",
        "This inventory and deduplicated license-text bundle are generated from the locked",
        "dependency graph and the exact Cargo source distributions used to build rgv-desktop.",
        "",
    ]
    for package in packages:
        if package.get("source") is None:
            continue
        lines.append(f"{package['name']} {package['version']}")
        lines.append(f"  License: {package.get('license') or 'NOT DECLARED'}")
        materials = package_materials[package["id"]]
        lines.append(
            "  Bundled texts: "
            + (", ".join(material_ids[digest] for digest in materials) if materials else "NONE")
        )
        if package.get("repository"):
            lines.append(f"  Source: {package['repository']}")
        lines.append("")
    lines.extend(["Bundled license and attribution texts", "====================================", ""])
    for digest in sorted(material_by_digest):
        filename, text = material_by_digest[digest]
        lines.extend(
            [
                f"[{material_ids[digest]}] {filename}",
                f"SHA-256: {digest}",
                "-" * 72,
                text,
                "",
            ]
        )
    return "\n".join(lines)


def build_timestamp() -> str:
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    instant = (
        dt.datetime.fromtimestamp(int(epoch), tz=dt.timezone.utc)
        if epoch is not None
        else dt.datetime.now(tz=dt.timezone.utc)
    )
    return instant.replace(microsecond=0).isoformat().replace("+00:00", "Z")


def generate(binary: Path, target: str, version: str, output: Path) -> None:
    binary = binary.resolve(strict=True)
    output.mkdir(parents=True, exist_ok=True)
    timestamp = build_timestamp()
    metadata = cargo_metadata()
    workspace_versions = {
        package["version"] for package in metadata["packages"] if package["name"] == "rgv-desktop"
    }
    if workspace_versions != {version}:
        raise ValueError(
            f"package version {version} does not match rgv-desktop metadata {sorted(workspace_versions)}"
        )
    sbom, packages = build_sbom(metadata, version, timestamp)
    sbom_path = output / SBOM_NAME
    sbom_path.write_text(json.dumps(sbom, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    (output / NOTICES_NAME).write_text(build_notices(packages), encoding="utf-8")
    provenance = {
        "schema_version": 1,
        "product": "RGV",
        "version": version,
        "target": target,
        "created": timestamp,
        "source": {
            "commit": command_output(["git", "rev-parse", "HEAD"], required=False),
            "cargo_lock_sha256": sha256(ROOT / "Cargo.lock"),
        },
        "toolchain": {
            "rustc": command_output(["rustc", "--version", "--verbose"]),
            "cargo": command_output(["cargo", "--version", "--verbose"]),
        },
        "staged_input_binary": {
            "name": binary.name,
            "bytes": binary.stat().st_size,
            "sha256": sha256(binary),
        },
        "sbom": {"name": SBOM_NAME, "sha256": sha256(sbom_path)},
        "dependency_components": len(packages),
    }
    (output / PROVENANCE_NAME).write_text(
        json.dumps(provenance, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def check(directory: Path) -> None:
    sbom = json.loads((directory / SBOM_NAME).read_text(encoding="utf-8"))
    provenance = json.loads((directory / PROVENANCE_NAME).read_text(encoding="utf-8"))
    notices = (directory / NOTICES_NAME).read_text(encoding="utf-8")
    if sbom.get("bomFormat") != "CycloneDX" or sbom.get("specVersion") != "1.6":
        raise ValueError("release SBOM is not CycloneDX 1.6")
    if not sbom.get("components") or not sbom.get("dependencies"):
        raise ValueError("release SBOM has no component/dependency graph")
    expected = provenance.get("sbom", {}).get("sha256")
    if expected != sha256(directory / SBOM_NAME):
        raise ValueError("provenance SBOM checksum does not match")
    if "RGV third-party dependency notices" not in notices:
        raise ValueError("third-party notice inventory is malformed")


def main() -> int:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    generate_parser = subparsers.add_parser("generate")
    generate_parser.add_argument("--binary", type=Path, required=True)
    generate_parser.add_argument("--target", required=True)
    generate_parser.add_argument("--version", required=True)
    generate_parser.add_argument("--output", type=Path, required=True)
    check_parser = subparsers.add_parser("check")
    check_parser.add_argument("--directory", type=Path, required=True)
    arguments = parser.parse_args()
    if arguments.command == "generate":
        generate(arguments.binary, arguments.target, arguments.version, arguments.output)
    else:
        check(arguments.directory)
    return 0


if __name__ == "__main__":
    sys.exit(main())
