#!/usr/bin/env python3
"""Compare a resident-scene navigation result with a clean canonical entry."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


ALIGNMENT_FIELDS = (
    "complete_read_records",
    "row_layout_scale_class",
    "row_layout_policy_fingerprint",
    "row_layout_projection_fingerprint",
    "row_layout_stable_rows",
    "row_layout_canonical_rows",
    "row_layout_query_occupied_row_span",
    "row_layout_query_empty_rows",
    "row_layout_horizon_start",
    "row_layout_horizon_end",
    "row_layout_horizon_records",
    "row_layout_horizon_truncated",
)

READ_FIELDS = (
    "queried_items",
    "rendered_items",
    "packed_row_count",
    "visible_occupied_rows",
    "visible_row_span",
    "natural_stack_height",
    "truncated",
)


def settled_snapshot(path: Path) -> dict[str, Any]:
    report = json.loads(path.read_text(encoding="utf-8"))
    if not report.get("succeeded"):
        raise ValueError(f"qualification failed: {path}")
    descriptions = [
        result["data"]
        for result in report.get("results", [])
        if result.get("command") == "describe_workspace"
        and result.get("status") == "succeeded"
    ]
    if not descriptions:
        raise ValueError(f"report contains no settled workspace description: {path}")
    workspace = descriptions[-1]
    alignment = workspace["tracks"][0]["alignment"]
    panel = workspace["panels"][0]
    reads = next(lane for lane in panel["render_lanes"] if lane["lane"] == "reads")
    return {
        "locus": panel["locus"],
        "alignment": {field: alignment[field] for field in ALIGNMENT_FIELDS},
        "reads": {field: reads[field] for field in READ_FIELDS},
    }


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("clean_report", type=Path)
    parser.add_argument("resident_report", type=Path)
    parser.add_argument("clean_png", type=Path)
    parser.add_argument("resident_png", type=Path)
    args = parser.parse_args()

    clean = settled_snapshot(args.clean_report)
    resident = settled_snapshot(args.resident_report)
    clean_png_sha256 = sha256(args.clean_png)
    resident_png_sha256 = sha256(args.resident_png)
    differences: dict[str, Any] = {}
    if clean != resident:
        differences["structured_snapshot"] = {"clean": clean, "resident": resident}
    if clean_png_sha256 != resident_png_sha256:
        differences["png_sha256"] = {
            "clean": clean_png_sha256,
            "resident": resident_png_sha256,
        }

    result = {
        "equivalent": not differences,
        "locus": clean["locus"],
        "clean_report": str(args.clean_report),
        "resident_report": str(args.resident_report),
        "png_sha256": clean_png_sha256,
        "differences": differences,
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return int(bool(differences))


if __name__ == "__main__":
    raise SystemExit(main())
