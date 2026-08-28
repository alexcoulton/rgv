#!/usr/bin/env python3
"""Stage the reproducible RGV landmark runtime package for app bundling."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any, Sequence

from build_bundles import DEFAULT_OUTPUT, SCHEMA_VERSION


COMPACT_ROLES = {
    "representative_genes",
    "gene_search",
    "cytobands",
    "contig_aliases",
    "chromosome_sizes",
    "notice",
}


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Stage compact RGV landmark resources and optional detailed transcripts."
    )
    parser.add_argument("destination", type=Path)
    parser.add_argument("--source-root", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--include-detail", action="store_true")
    parser.add_argument(
        "--allow-pending-review",
        action="store_true",
        help="Permit local qualification staging before the distribution review gate is closed.",
    )
    return parser.parse_args(argv)


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def safe_path(value: str) -> PurePosixPath:
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or any(part in {"", ".", ".."} for part in path.parts)
        or "\\" in value
        or ":" in value
    ):
        raise ValueError(f"unsafe catalogue path {value!r}")
    return path


def selected_assets(catalogue: dict[str, Any], include_detail: bool) -> list[dict[str, Any]]:
    roles = set(COMPACT_ROLES)
    if include_detail:
        roles.add("detailed_transcripts")
    selected: list[dict[str, Any]] = []
    for bundle in catalogue.get("bundles", []):
        for asset in bundle.get("assets", []):
            if asset.get("role") in roles:
                safe_path(asset["path"])
                selected.append(asset)
    return selected


def validate_release_gate(source_root: Path, catalogue: dict[str, Any], allow: bool) -> None:
    pending: list[str] = []
    for bundle in catalogue.get("bundles", []):
        manifest_path = source_root / bundle["id"] / "manifest.json"
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        if manifest.get("distribution_status") != "redistribution_reviewed":
            pending.append(bundle["id"])
    if pending and not allow:
        raise ValueError(
            "distribution review is still pending for "
            + ", ".join(pending)
            + "; use --allow-pending-review only for local qualification"
        )


def stage(
    source_root: Path,
    destination: Path,
    *,
    include_detail: bool,
    allow_pending_review: bool,
) -> dict[str, Any]:
    catalog_path = source_root / "catalog.json"
    catalogue = json.loads(catalog_path.read_text(encoding="utf-8"))
    if catalogue.get("schema_version") != SCHEMA_VERSION:
        raise ValueError("unsupported landmark catalogue schema")
    validate_release_gate(source_root, catalogue, allow_pending_review)
    assets = selected_assets(catalogue, include_detail)
    parent = destination.parent
    parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        raise ValueError(f"staging destination already exists: {destination}")

    temporary = Path(tempfile.mkdtemp(prefix=f".{destination.name}.", dir=parent))
    files: list[dict[str, Any]] = []
    try:
        inputs = [(catalog_path, PurePosixPath("catalog.json"))]
        inputs.extend(
            (
                source_root / bundle["id"] / "manifest.json",
                PurePosixPath(bundle["id"]) / "manifest.json",
            )
            for bundle in catalogue["bundles"]
        )
        inputs.extend((source_root / asset["path"], safe_path(asset["path"])) for asset in assets)
        seen: set[PurePosixPath] = set()
        for source, relative in inputs:
            if relative in seen:
                continue
            seen.add(relative)
            if not source.is_file():
                raise ValueError(f"required staging input is missing: {relative}")
            expected = next(
                (asset for asset in assets if asset.get("path") == relative.as_posix()), None
            )
            source_digest = digest(source)
            if expected is not None and (
                source.stat().st_size != expected["bytes"]
                or source_digest.lower() != expected["sha256"].lower()
            ):
                raise ValueError(f"staging input failed integrity verification: {relative}")
            target = temporary / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            files.append(
                {
                    "path": relative.as_posix(),
                    "bytes": target.stat().st_size,
                    "sha256": digest(target),
                }
            )
        package = {
            "schema_version": 1,
            "catalogue_schema_version": catalogue["schema_version"],
            "package": "complete" if include_detail else "compact_context",
            "distribution_status": (
                "local_qualification_only_pending_release_review"
                if allow_pending_review
                else "redistribution_reviewed"
            ),
            "bundles": [bundle["id"] for bundle in catalogue["bundles"]],
            "total_bytes": sum(entry["bytes"] for entry in files),
            "files": sorted(files, key=lambda entry: entry["path"]),
        }
        (temporary / "PACKAGE_MANIFEST.json").write_text(
            json.dumps(package, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        os.replace(temporary, destination)
        return package
    except BaseException:
        shutil.rmtree(temporary, ignore_errors=True)
        raise


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    package = stage(
        args.source_root,
        args.destination,
        include_detail=args.include_detail,
        allow_pending_review=args.allow_pending_review,
    )
    print(
        f"Staged {package['package']} landmark package at {args.destination} "
        f"({package['total_bytes'] / 1024 / 1024:.2f} MiB)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
