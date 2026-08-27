#!/usr/bin/env python3
"""Qualify the adaptive workspace layout against a running RGV control service."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path
import subprocess
from typing import Any

from rgv_mcp_qualification import (
    McpClient,
    png_size,
    save_mcp_window_capture,
    structured,
)


EPSILON = 0.05
STATUS_HEIGHT = 24.0


def close(left: float, right: float, tolerance: float = EPSILON) -> bool:
    return abs(float(left) - float(right)) <= tolerance


def rect_contains(outer: dict[str, float], inner: dict[str, float]) -> bool:
    return (
        inner["left"] >= outer["left"] - EPSILON
        and inner["right"] <= outer["right"] + EPSILON
        and inner["top"] >= outer["top"] - EPSILON
        and inner["bottom"] <= outer["bottom"] + EPSILON
    )


def require_equal_edge(
    first: dict[str, float], first_edge: str, second: dict[str, float], second_edge: str
) -> None:
    if not close(first[first_edge], second[second_edge]):
        raise RuntimeError(
            f"layout edge mismatch: {first_edge}={first[first_edge]} "
            f"versus {second_edge}={second[second_edge]}"
        )


def layout_snapshot(workspace: dict[str, Any]) -> dict[str, Any]:
    layout = workspace.get("workspace_layout")
    if not isinstance(layout, dict):
        raise RuntimeError("automation schema omitted the committed workspace_layout")
    return layout


def assert_layout(
    workspace: dict[str, Any],
    *,
    properties_expanded: bool,
    properties_width: float,
    cytobands: list[str],
    accessibility_scale: float,
) -> dict[str, Any]:
    if workspace.get("schema_version") != 4:
        raise RuntimeError(f"expected automation schema 4, got {workspace.get('schema_version')}")
    layout = layout_snapshot(workspace)
    properties = layout["properties"]
    workspace_rect = layout["workspace"]
    genome_context = layout["genome_context"]
    track_viewport = layout["track_viewport"]
    if layout["properties_expanded"] is not properties_expanded:
        raise RuntimeError("Properties expansion state disagrees with committed layout")
    if not close(properties["right"] - properties["left"], properties_width, 0.75):
        raise RuntimeError(
            f"Properties width was {properties['right'] - properties['left']}, "
            f"expected {properties_width}"
        )
    if not close(layout["accessibility_scale"], accessibility_scale, 0.01):
        raise RuntimeError("committed accessibility scale is stale")
    require_equal_edge(workspace_rect, "right", properties, "left")
    require_equal_edge(genome_context, "bottom", track_viewport, "top")
    require_equal_edge(layout["context_gutter"], "right", layout["track_gutter"], "right")

    panels = layout["panels"]
    if len(panels) != len(cytobands):
        raise RuntimeError("layout panel count disagrees with qualification state")
    expected_optional_row = any(state != "absent" for state in cytobands)
    if layout["has_cytoband_row"] is not expected_optional_row:
        raise RuntimeError("workspace-wide cytoband row policy is incorrect")
    previous_right = layout["track_gutter"]["right"]
    for panel, expected_cytobands in zip(panels, cytobands):
        if panel["cytobands"] != expected_cytobands:
            raise RuntimeError("panel cytoband availability is incorrect")
        require_equal_edge(panel["column"], "left", {"edge": previous_right}, "edge")
        require_equal_edge(panel["header_genomic"], "left", panel["canvas"], "left")
        require_equal_edge(panel["header_genomic"], "right", panel["canvas"], "right")
        require_equal_edge(panel["header_scrollbar"], "left", panel["scrollbar"], "left")
        require_equal_edge(panel["header_scrollbar"], "right", panel["scrollbar"], "right")
        require_equal_edge(panel["canvas"], "top", track_viewport, "top")
        require_equal_edge(panel["canvas"], "bottom", track_viewport, "bottom")
        rows = [panel["metadata_row"]]
        if panel["cytoband_row"] is not None:
            rows.append(panel["cytoband_row"])
        rows.extend([panel["overview_row"], panel["span_row"], panel["ruler_row"]])
        if any(not rect_contains(genome_context, row) for row in rows):
            raise RuntimeError("a navigation row escaped the committed genome context")
        if any(first["bottom"] > second["top"] + EPSILON for first, second in zip(rows, rows[1:])):
            raise RuntimeError("adaptive navigation rows overlap")
        if panel["cytoband_row"] is None and expected_optional_row:
            raise RuntimeError("mixed cytoband workspace lost shared row alignment")
        previous_right = panel["column"]["right"]
    if not close(previous_right, workspace_rect["right"]):
        raise RuntimeError("panel columns do not tile through the workspace right edge")
    return layout


def stable_track_state(workspace: dict[str, Any]) -> dict[str, Any]:
    return {
        "read_pane_height": workspace["read_pane_height"],
        "presentation_nodes": [
            {
                "id": node["id"],
                "desired_content_visible": node["desired_content_visible"],
                "preferred_height": node.get("preferred_height"),
                "parent_group_id": node.get("parent_group_id"),
                "parent_composite_id": node.get("parent_composite_id"),
            }
            for node in workspace["presentation_nodes"]
        ],
        "panels": [
            {
                "id": panel["id"],
                "contig": panel["contig"],
                "start": panel["start"],
                "end": panel["end"],
                "annotation_scroll_points": panel["annotation_scroll_points"],
                "read_stack_scroll_points": panel["read_stack_scroll_points"],
                "read_panel_states": [
                    {
                        "track_id": state["track_id"],
                        "scroll_points": state["scroll_points"],
                        "density": state["density"],
                        "effective_density": state["effective_density"],
                    }
                    for state in panel["read_panel_states"]
                ],
            }
            for panel in workspace["panels"]
        ],
        "selection": workspace.get("selection"),
    }


def physical_edges(layout: dict[str, Any], scale: float) -> dict[str, Any]:
    return {
        "effective_pixels_per_point": scale,
        "workspace_right": round(layout["workspace"]["right"] * scale),
        "properties_left": round(layout["properties"]["left"] * scale),
        "context_bottom": round(layout["genome_context"]["bottom"] * scale),
        "track_top": round(layout["track_viewport"]["top"] * scale),
        "panel_edges": [
            {
                "header_left": round(panel["header_genomic"]["left"] * scale),
                "canvas_left": round(panel["canvas"]["left"] * scale),
                "header_right": round(panel["header_genomic"]["right"] * scale),
                "canvas_right": round(panel["canvas"]["right"] * scale),
                "scrollbar_right": round(panel["scrollbar"]["right"] * scale),
            }
            for panel in layout["panels"]
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mcp-binary", type=Path, required=True)
    parser.add_argument("--control-url", required=True)
    parser.add_argument("--secret-file", type=Path, required=True)
    parser.add_argument("--control-root", type=Path, required=True)
    parser.add_argument(
        "--evidence-directory",
        default="validation/adaptive-layout-evidence",
    )
    arguments = parser.parse_args()

    root = arguments.control_root.resolve(strict=True)
    evidence = (root / arguments.evidence_directory).resolve()
    if root != evidence and root not in evidence.parents:
        raise RuntimeError("evidence directory escapes the control root")
    evidence.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(
        [
            str(arguments.mcp_binary.resolve(strict=True)),
            "--control-url",
            arguments.control_url,
            "--control-secret-file",
            str(arguments.secret_file.resolve(strict=True)),
            "--control-root",
            str(root),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None,
        text=True,
        encoding="utf-8",
        bufsize=1,
    )
    client = McpClient(process)
    captures: list[dict[str, Any]] = []
    logical_captures: list[dict[str, Any]] = []
    states: list[dict[str, Any]] = []

    def call(name: str, values: dict[str, Any] | None = None) -> dict[str, Any]:
        return structured(client.call_tool(name, values))

    def workspace() -> dict[str, Any]:
        value = call("rgv_get_workspace")["workspace"]
        if not value["settled"] or any(not panel["settled"] for panel in value["panels"]):
            raise RuntimeError("workspace did not settle before observation")
        return value

    def configure(change: dict[str, Any]) -> None:
        call("rgv_configure", {"change": change})

    def capture(name: str, current: dict[str, Any]) -> None:
        layout = layout_snapshot(current)
        result = client.call_tool("rgv_capture_window", {"wait_until_settled": True})
        saved = save_mcp_window_capture(evidence / f"{name}.png", result)
        effective_scale = layout["display_scale"] * layout["accessibility_scale"]
        expected_width = round(layout["properties"]["right"] * effective_scale)
        expected_height = round(
            (layout["workspace"]["bottom"] + STATUS_HEIGHT) * effective_scale
        )
        if abs(saved["width"] - expected_width) > 2 or abs(saved["height"] - expected_height) > 2:
            raise RuntimeError(
                f"native capture dimensions {saved['width']}x{saved['height']} disagree with "
                f"committed logical bounds {expected_width}x{expected_height}"
            )
        saved["layout_revision"] = layout["revision"]
        saved["logical_layout"] = layout
        saved["expected_physical_edges"] = physical_edges(layout, effective_scale)
        captures.append(saved)

    def capture_logical(name: str, current: dict[str, Any]) -> None:
        result = client.call_tool(
            "rgv_capture_view",
            {"scope": "visible_workspace", "wait_until_settled": True},
        )
        images = [item for item in result.get("content", []) if item.get("type") == "image"]
        if len(images) != 1 or images[0].get("mimeType") != "image/png":
            raise RuntimeError("rgv_capture_view did not return one PNG")
        data = base64.b64decode(images[0]["data"], validate=True)
        path = evidence / f"{name}.png"
        path.write_bytes(data)
        width, height = png_size(path)
        metadata = structured(result)
        if metadata.get("width") != width or metadata.get("height") != height:
            raise RuntimeError("logical capture metadata disagrees with PNG dimensions")
        layout = layout_snapshot(current)
        logical_captures.append(
            {
                "name": path.name,
                "bytes": len(data),
                "width": width,
                "height": height,
                "sha256": hashlib.sha256(data).hexdigest(),
                "scope": metadata.get("scope"),
                "layout_revision": layout["revision"],
                "expected_logical_edges": physical_edges(layout, 1.0),
            }
        )

    try:
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "adaptive-layout-qualification", "version": "1"},
            },
        )
        client.notification("notifications/initialized")
        tools = client.request("tools/list")["tools"]
        schema = next(tool for tool in tools if tool["name"] == "rgv_configure")["inputSchema"]
        encoded_schema = json.dumps(schema, sort_keys=True)
        for required in [
            "window_size",
            "interface_scale_percent",
            "properties_panel_expanded",
            "properties_panel_width",
        ]:
            if required not in encoded_schema:
                raise RuntimeError(f"MCP configure schema omitted {required}")

        configure({"setting": "window_size", "width": 1280, "height": 800})
        configure({"setting": "interface_scale_percent", "value": 100})
        call(
            "rgv_open_workspace",
            {
                "path": "benchmark-data/manual-suite/projects/mf-12-workspaces.rgv.json",
                "confirm_replace": True,
                "unsaved_changes": "discard",
            },
        )
        first = workspace()
        alignment_ids = [
            track["id"] for track in first["tracks"] if track.get("alignment") is not None
        ]
        panel_ids = [panel["id"] for panel in first["panels"]]
        if len(alignment_ids) != 4 or len(panel_ids) != 2:
            raise RuntimeError("MF-12 did not restore the required four tracks and two panels")
        configure(
            {
                "setting": "presentation_node_height",
                "target": {
                    "kind": "track",
                    "channel": "reads",
                    "track_id": alignment_ids[0],
                },
                "value": 420.0,
            }
        )
        for index, panel_id in enumerate(panel_ids):
            configure(
                {
                    "setting": "read_panel_density",
                    "panel_id": panel_id,
                    "track_id": alignment_ids[0],
                    "value": 1.5 + index * 0.5,
                }
            )
            configure(
                {
                    "setting": "read_panel_scroll_points",
                    "panel_id": panel_id,
                    "track_id": alignment_ids[0],
                    "value": 30.0 + index * 20.0,
                }
            )

        configure({"setting": "properties_panel_width", "value": 340.0})
        configure({"setting": "properties_panel_expanded", "value": True})
        default = workspace()
        assert_layout(
            default,
            properties_expanded=True,
            properties_width=340.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.0,
        )
        preserved_state = stable_track_state(default)
        capture("mf12-default-expanded", default)
        capture_logical("mf12-default-logical-1x", default)
        states.append({"name": "default-expanded", "layout": layout_snapshot(default)})

        for name, expanded, width in [
            ("collapsed", False, 30.0),
            ("minimum-expanded", True, 280.0),
            ("maximum-expanded", True, 520.0),
        ]:
            if expanded:
                configure({"setting": "properties_panel_width", "value": width})
            configure({"setting": "properties_panel_expanded", "value": expanded})
            current = workspace()
            assert_layout(
                current,
                properties_expanded=expanded,
                properties_width=width,
                cytobands=["absent", "absent"],
                accessibility_scale=1.0,
            )
            if stable_track_state(current) != preserved_state:
                raise RuntimeError(f"track navigation/presentation state changed after {name}")
            capture(f"mf12-{name}", current)
            states.append({"name": name, "layout": layout_snapshot(current)})

        before_drag = layout_snapshot(workspace())["settled_resize_query_refreshes"]
        for width in [300.0, 360.0, 420.0, 480.0]:
            call(
                "rgv_configure",
                {
                    "change": {"setting": "properties_panel_width", "value": width},
                    "wait_until_settled": False,
                },
            )
        call(
            "rgv_execute_batch",
            {
                "commands": [
                    {"command": "wait_until_settled", "timeout_millis": 600_000}
                ]
            },
        )
        after_drag_state = workspace()
        drag_layout = assert_layout(
            after_drag_state,
            properties_expanded=True,
            properties_width=480.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.0,
        )
        resize_queries = drag_layout["settled_resize_query_refreshes"] - before_drag
        if resize_queries > 1:
            raise RuntimeError(
                f"continuous Properties resize triggered {resize_queries} settled query refreshes"
            )
        if stable_track_state(after_drag_state) != preserved_state:
            raise RuntimeError("continuous Properties resize changed track state")
        capture("mf12-resize-end", after_drag_state)
        states.append({"name": "resize-end", "layout": drag_layout})

        configure({"setting": "properties_panel_expanded", "value": False})
        configure({"setting": "window_size", "width": 800, "height": 600})
        configure({"setting": "interface_scale_percent", "value": 150})
        narrow = workspace()
        assert_layout(
            narrow,
            properties_expanded=False,
            properties_width=30.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.5,
        )
        if stable_track_state(narrow) != preserved_state:
            raise RuntimeError("narrow/enlarged-text reflow changed track state")
        capture("mf12-narrow-enlarged-text", narrow)
        states.append({"name": "narrow-enlarged-text", "layout": layout_snapshot(narrow)})

        configure({"setting": "window_size", "width": 1280, "height": 800})
        configure({"setting": "interface_scale_percent", "value": 100})
        configure({"setting": "properties_panel_width", "value": 340.0})
        configure({"setting": "properties_panel_expanded", "value": True})
        restored = workspace()
        assert_layout(
            restored,
            properties_expanded=True,
            properties_width=340.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.0,
        )
        if stable_track_state(restored) != preserved_state:
            raise RuntimeError("restoring the shell changed track state")

        call(
            "rgv_open_workspace",
            {
                "path": "validation/adaptive-layout-mixed-cytobands.rgv.json",
                "confirm_replace": True,
                "unsaved_changes": "discard",
            },
        )
        mixed = workspace()
        assert_layout(
            mixed,
            properties_expanded=True,
            properties_width=340.0,
            cytobands=["present", "absent"],
            accessibility_scale=1.0,
        )
        capture("mixed-cytobands", mixed)
        capture_logical("mixed-cytobands-logical-1x", mixed)
        states.append({"name": "mixed-cytobands", "layout": layout_snapshot(mixed)})

        report = {
            "result": "passed",
            "automation_schema_version": mixed["schema_version"],
            "display_scale": layout_snapshot(mixed)["display_scale"],
            "automated_physical_scale_matrix": [1.0, 2.0],
            "native_capture_effective_scale": layout_snapshot(mixed)["display_scale"],
            "logical_capture_scale": 1.0,
            "resize_query_refreshes": resize_queries,
            "states": states,
            "captures": captures,
            "logical_captures": logical_captures,
        }
        (evidence / "report.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0
    finally:
        process.terminate()
        process.wait(timeout=5)


if __name__ == "__main__":
    raise SystemExit(main())
