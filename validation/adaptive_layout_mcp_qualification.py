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
# The capture tools downscale to `max_width` (default 1600 px) before inlining. This script
# compares captured dimensions with the committed layout pixel for pixel, so it asks for the
# largest width the tools accept and therefore always receives the native-size image.
NATIVE_CAPTURE_MAX_WIDTH = 16384


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
    properties_resize_active: bool = False,
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
    if layout.get("properties_resize_active") is not properties_resize_active:
        raise RuntimeError("Properties resize gesture state disagrees with committed layout")
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
    }


def selection_signature(selection: dict[str, Any] | None) -> dict[str, Any] | None:
    if selection is None:
        return None
    return {
        "selection_id": selection.get("selection_id"),
        "panel_id": selection.get("panel_id"),
        "contig": selection.get("contig"),
        "track_id": selection.get("track_id"),
        "kind": selection.get("kind"),
        "granularity": selection.get("granularity"),
        "anchor_position": selection.get("anchor_position"),
        "evidence_start": selection.get("evidence_start"),
        "evidence_end": selection.get("evidence_end"),
        "position": selection.get("position"),
        "start": selection.get("start"),
        "end": selection.get("end"),
        "visible_in_active_view": selection.get("visible_in_active_view"),
        "stale": selection.get("stale"),
    }


def alternate_scroll(current: float, maximum: float) -> float | None:
    if maximum <= 0.0:
        return None
    if current + 1.0 <= maximum:
        return current + 1.0
    if current >= 1.0:
        return current - 1.0
    return maximum


def physical_edges(
    layout: dict[str, Any],
    scale: float,
    *,
    origin_x: float = 0.0,
    origin_y: float = 0.0,
) -> dict[str, Any]:
    def x(value: float) -> int:
        return round((value - origin_x) * scale)

    def y(value: float) -> int:
        return round((value - origin_y) * scale)

    return {
        "effective_pixels_per_point": scale,
        "origin_x": origin_x,
        "origin_y": origin_y,
        "workspace_right": x(layout["workspace"]["right"]),
        "properties_left": x(layout["properties"]["left"]),
        "context_bottom": y(layout["genome_context"]["bottom"]),
        "track_top": y(layout["track_viewport"]["top"]),
        "panel_edges": [
            {
                "header_left": x(panel["header_genomic"]["left"]),
                "canvas_left": x(panel["canvas"]["left"]),
                "header_right": x(panel["header_genomic"]["right"]),
                "canvas_right": x(panel["canvas"]["right"]),
                "scrollbar_right": x(panel["scrollbar"]["right"]),
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
    interaction_probes: list[dict[str, Any]] = []

    def call(name: str, values: dict[str, Any] | None = None) -> dict[str, Any]:
        return structured(client.call_tool(name, values))

    def workspace() -> dict[str, Any]:
        value = call("rgv_get_workspace")["workspace"]
        if not value["settled"] or any(not panel["settled"] for panel in value["panels"]):
            raise RuntimeError("workspace did not settle before observation")
        return value

    def current_workspace() -> dict[str, Any]:
        return call("rgv_get_workspace")["workspace"]

    def configure(change: dict[str, Any]) -> None:
        call("rgv_configure", {"change": change})

    def exercise_reflow_controls(
        label: str,
        current: dict[str, Any],
        expected_track_state: dict[str, Any] | None,
        resize_track_id: int | None,
        selection_positions: dict[int, tuple[int, int]],
        expected_selections: dict[int, dict[str, Any]],
    ) -> None:
        activated: list[int] = []
        viewport_pan_zoom_panels = 0
        adjusted_annotation_scrollbars = 0
        adjusted_read_stacks = 0
        adjusted_read_panels = 0
        verified_selection_panels = 0
        for panel in current["panels"]:
            panel_id = panel["id"]
            call(
                "rgv_select",
                {"selection": {"selection": "panel", "panel_id": panel_id}},
            )
            activated.append(panel_id)
            selected = workspace()
            if panel_id in expected_selections and selection_signature(
                selected.get("selection")
            ) != expected_selections[panel_id]:
                raise RuntimeError(f"selection did not survive the {label} reflow")
            if panel_id in expected_selections:
                verified_selection_panels += 1

            original_start = panel["start"]
            original_end = panel["end"]
            midpoint = original_start + (original_end - original_start) // 2
            call(
                "rgv_change_viewport",
                {
                    "panel_id": panel_id,
                    "operation": {"operation": "pan_fraction", "fraction": 0.02},
                    "wait_until_settled": False,
                },
            )
            call(
                "rgv_change_viewport",
                {
                    "panel_id": panel_id,
                    "operation": {
                        "operation": "zoom",
                        "factor": 1.05,
                        "anchor_position": midpoint,
                    },
                    "wait_until_settled": False,
                },
            )
            call(
                "rgv_change_viewport",
                {
                    "panel_id": panel_id,
                    "operation": {
                        "operation": "fit_interval",
                        "start": original_start,
                        "end": original_end,
                    },
                    "wait_until_settled": True,
                },
            )
            viewport_pan_zoom_panels += 1

            annotation_target = alternate_scroll(
                panel["annotation_scroll_points"],
                panel["maximum_annotation_scroll_points"],
            )
            if annotation_target is not None:
                configure(
                    {
                        "setting": "annotation_scroll_points",
                        "panel_id": panel_id,
                        "value": annotation_target,
                    }
                )
                configure(
                    {
                        "setting": "annotation_scroll_points",
                        "panel_id": panel_id,
                        "value": panel["annotation_scroll_points"],
                    }
                )
                adjusted_annotation_scrollbars += 1

            read_stack_target = alternate_scroll(
                panel["read_stack_scroll_points"],
                panel["maximum_read_stack_scroll_points"],
            )
            if read_stack_target is not None:
                configure(
                    {
                        "setting": "read_stack_scroll_points",
                        "panel_id": panel_id,
                        "value": read_stack_target,
                    }
                )
                configure(
                    {
                        "setting": "read_stack_scroll_points",
                        "panel_id": panel_id,
                        "value": panel["read_stack_scroll_points"],
                    }
                )
                adjusted_read_stacks += 1

            if not panel["read_panel_states"]:
                pass
            else:
                read_state = panel["read_panel_states"][0]
                original_scroll = read_state["scroll_points"]
                configure(
                    {
                        "setting": "read_panel_scroll_points",
                        "panel_id": panel_id,
                        "track_id": read_state["track_id"],
                        "value": original_scroll + 1.0,
                    }
                )
                configure(
                    {
                        "setting": "read_panel_scroll_points",
                        "panel_id": panel_id,
                        "track_id": read_state["track_id"],
                        "value": original_scroll,
                    }
                )
                adjusted_read_panels += 1

            if panel_id in selection_positions:
                track_id, position = selection_positions[panel_id]
                call(
                    "rgv_select",
                    {
                        "selection": {
                            "selection": "position",
                            "panel_id": panel_id,
                            "track_id": track_id,
                            "position": position,
                            "granularity": "visible_bin",
                        }
                    },
                )
                restored_selection = selection_signature(workspace().get("selection"))
                if restored_selection is None:
                    raise RuntimeError(f"selection could not be restored after {label}")
                expected_selections[panel_id] = restored_selection
        if resize_track_id is not None:
            configure(
                {
                    "setting": "presentation_node_height",
                    "target": {
                        "kind": "track",
                        "channel": "reads",
                        "track_id": resize_track_id,
                    },
                    "value": 421.0,
                }
            )
            configure(
                {
                    "setting": "presentation_node_height",
                    "target": {
                        "kind": "track",
                        "channel": "reads",
                        "track_id": resize_track_id,
                    },
                    "value": 420.0,
                }
            )
        observed = workspace()
        if activated and observed["active_panel_id"] != activated[-1]:
            raise RuntimeError(f"panel activation failed after {label}")
        if (
            expected_track_state is not None
            and stable_track_state(observed) != expected_track_state
        ):
            raise RuntimeError(f"reversible control probes changed track state after {label}")
        interaction_probes.append(
            {
                "state": label,
                "activated_panel_ids": activated,
                "viewport_pan_zoom_panels": viewport_pan_zoom_panels,
                "adjusted_annotation_scrollbars": adjusted_annotation_scrollbars,
                "adjusted_read_stacks": adjusted_read_stacks,
                "adjusted_read_panels": adjusted_read_panels,
                "verified_selection_panels": verified_selection_panels,
                "resized_track_id": resize_track_id,
            }
        )

    def capture(
        name: str,
        current: dict[str, Any],
        *,
        wait_until_settled: bool = True,
    ) -> None:
        layout = layout_snapshot(current)
        result = client.call_tool(
            "rgv_capture_window",
            {
                "wait_until_settled": wait_until_settled,
                "max_width": NATIVE_CAPTURE_MAX_WIDTH,
            },
        )
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
        saved["requested_settled_capture"] = wait_until_settled
        saved["logical_layout"] = layout
        saved["expected_physical_edges"] = physical_edges(layout, effective_scale)
        captures.append(saved)

    def capture_logical(name: str, current: dict[str, Any]) -> None:
        result = client.call_tool(
            "rgv_capture_view",
            {
                "scope": "visible_workspace",
                "wait_until_settled": True,
                "max_width": NATIVE_CAPTURE_MAX_WIDTH,
            },
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
        workspace_rect = layout["workspace"]
        expected_width = round(workspace_rect["right"] - workspace_rect["left"])
        expected_height = round(workspace_rect["bottom"] - workspace_rect["top"])
        if width != expected_width or height != expected_height:
            raise RuntimeError(
                f"visible-workspace capture was {width}x{height}, expected committed "
                f"workspace bounds {expected_width}x{expected_height}"
            )
        logical_captures.append(
            {
                "name": path.name,
                "bytes": len(data),
                "width": width,
                "height": height,
                "sha256": hashlib.sha256(data).hexdigest(),
                "scope": metadata.get("scope"),
                "layout_revision": layout["revision"],
                "expected_logical_edges": physical_edges(
                    layout,
                    1.0,
                    origin_x=workspace_rect["left"],
                    origin_y=workspace_rect["top"],
                ),
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
            "properties_panel_resize_active",
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

        selection_positions: dict[int, tuple[int, int]] = {}
        expected_selections: dict[int, dict[str, Any]] = {}
        selected_workspace = workspace()
        for panel in selected_workspace["panels"]:
            position = panel["start"] + (panel["end"] - panel["start"]) // 2
            selection_positions[panel["id"]] = (alignment_ids[0], position)
            call(
                "rgv_select",
                {
                    "selection": {
                        "selection": "position",
                        "panel_id": panel["id"],
                        "track_id": alignment_ids[0],
                        "position": position,
                        "granularity": "visible_bin",
                    }
                },
            )
            selected = selection_signature(workspace().get("selection"))
            if selected is None:
                raise RuntimeError("MF-12 did not expose a selectable alignment position")
            expected_selections[panel["id"]] = selected

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
        exercise_reflow_controls(
            "default-expanded",
            default,
            preserved_state,
            alignment_ids[0],
            selection_positions,
            expected_selections,
        )

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
            exercise_reflow_controls(
                name,
                current,
                preserved_state,
                alignment_ids[0],
                selection_positions,
                expected_selections,
            )

        configure({"setting": "properties_panel_resize_active", "value": True})
        resize_start = workspace()
        assert_layout(
            resize_start,
            properties_expanded=True,
            properties_width=520.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.0,
            properties_resize_active=True,
        )
        before_drag = layout_snapshot(resize_start)["settled_resize_query_refreshes"]
        capture("mf12-resize-start", resize_start)
        states.append({"name": "resize-start", "layout": layout_snapshot(resize_start)})
        exercise_reflow_controls(
            "resize-start",
            resize_start,
            preserved_state,
            alignment_ids[0],
            selection_positions,
            expected_selections,
        )
        call(
            "rgv_configure",
            {
                "change": {"setting": "properties_panel_width", "value": 390.0},
                "wait_until_settled": False,
            },
        )
        resize_midpoint = current_workspace()
        midpoint_layout = assert_layout(
            resize_midpoint,
            properties_expanded=True,
            properties_width=390.0,
            cytobands=["absent", "absent"],
            accessibility_scale=1.0,
            properties_resize_active=True,
        )
        if stable_track_state(resize_midpoint) != preserved_state:
            raise RuntimeError("Properties resize midpoint changed track state")
        capture(
            "mf12-resize-midpoint",
            resize_midpoint,
            wait_until_settled=False,
        )
        states.append({"name": "resize-midpoint", "layout": midpoint_layout})
        exercise_reflow_controls(
            "resize-midpoint",
            resize_midpoint,
            preserved_state,
            alignment_ids[0],
            selection_positions,
            expected_selections,
        )
        call(
            "rgv_configure",
            {
                "change": {"setting": "properties_panel_width", "value": 480.0},
                "wait_until_settled": False,
            },
        )
        call(
            "rgv_configure",
            {
                "change": {
                    "setting": "properties_panel_resize_active",
                    "value": False,
                },
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
        exercise_reflow_controls(
            "resize-end",
            after_drag_state,
            preserved_state,
            alignment_ids[0],
            selection_positions,
            expected_selections,
        )

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
        exercise_reflow_controls(
            "narrow-enlarged-text",
            narrow,
            preserved_state,
            alignment_ids[0],
            selection_positions,
            expected_selections,
        )

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
            "rgv_execute_batch",
            {
                "commands": [
                    {"command": "close_panel", "panel_id": panel_ids[1]},
                    {"command": "wait_until_settled", "timeout_millis": 600_000},
                ]
            },
        )
        expected_one_panel_state = dict(preserved_state)
        expected_one_panel_state["panels"] = preserved_state["panels"][:1]
        for name, expanded, width in [
            ("one-panel-collapsed", False, 30.0),
            ("one-panel-expanded", True, 340.0),
        ]:
            if expanded:
                configure({"setting": "properties_panel_width", "value": width})
            configure({"setting": "properties_panel_expanded", "value": expanded})
            current = workspace()
            assert_layout(
                current,
                properties_expanded=expanded,
                properties_width=width,
                cytobands=["absent"],
                accessibility_scale=1.0,
            )
            if stable_track_state(current) != expected_one_panel_state:
                raise RuntimeError(f"one-panel track state changed after {name}")
            capture(f"mf12-{name}", current)
            states.append({"name": name, "layout": layout_snapshot(current)})
            exercise_reflow_controls(
                name,
                current,
                expected_one_panel_state,
                alignment_ids[0],
                {panel_ids[0]: selection_positions[panel_ids[0]]},
                expected_selections,
            )

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
        exercise_reflow_controls(
            "mixed-cytobands", mixed, None, None, {}, {}
        )

        if not all(probe["viewport_pan_zoom_panels"] for probe in interaction_probes):
            raise RuntimeError("a reflow state skipped viewport navigation probes")
        if not any(probe["adjusted_annotation_scrollbars"] for probe in interaction_probes):
            raise RuntimeError("MF-12 never exposed an adjustable annotation scrollbar")
        if not any(probe["adjusted_read_panels"] for probe in interaction_probes):
            raise RuntimeError("MF-12 never exposed an adjustable read-panel scrollbar")

        report = {
            "result": "passed",
            "automation_schema_version": mixed["schema_version"],
            "display_scale": layout_snapshot(mixed)["display_scale"],
            "automated_physical_scale_matrix": [1.0, 2.0],
            "native_capture_effective_scale": layout_snapshot(mixed)["display_scale"],
            "logical_capture_scale": 1.0,
            "resize_query_refreshes": resize_queries,
            "interaction_probes": interaction_probes,
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
