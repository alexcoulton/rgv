#!/usr/bin/env python3
"""Qualify an rgv-mcp release binary against an explicitly started RGV window."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import platform
import subprocess
import sys
from typing import Any, Optional


EXPECTED_TOOLS = {
    "rgv_capture_view",
    "rgv_change_viewport",
    "rgv_clear_selection",
    "rgv_configure",
    "rgv_execute_batch",
    "rgv_export",
    "rgv_filter_alignments",
    "rgv_find_alignments",
    "rgv_get_navigation_history",
    "rgv_get_related_alignments",
    "rgv_get_review_queue",
    "rgv_get_selection",
    "rgv_get_workspace",
    "rgv_group_alignments",
    "rgv_load_track",
    "rgv_navigate",
    "rgv_navigate_history",
    "rgv_navigate_related_alignment",
    "rgv_navigate_review_item",
    "rgv_open_workspace",
    "rgv_refresh_selection",
    "rgv_remove_track",
    "rgv_save_workspace",
    "rgv_select",
    "rgv_select_alignment",
    "rgv_sort_alignments",
    "rgv_status",
}


def png_size(path: Path) -> tuple[int, int]:
    prefix = path.read_bytes()[:24]
    if len(prefix) != 24 or prefix[:8] != b"\x89PNG\r\n\x1a\n" or prefix[12:16] != b"IHDR":
        raise RuntimeError("native screenshot is not a PNG")
    return int.from_bytes(prefix[16:20], "big"), int.from_bytes(prefix[20:24], "big")


def macos_pid_screenshot(path: Path, process_id: int) -> dict[str, Any]:
    path.parent.mkdir(parents=True, exist_ok=True)
    script = (
        'on run argv\n'
        'set capturePath to item 1 of argv\n'
        'set targetPid to item 2 of argv as integer\n'
        'tell application "System Events" to set frontmost of first application process whose unix id is targetPid to true\n'
        'delay 1\n'
        'do shell script "/usr/sbin/screencapture -x " & quoted form of capturePath\n'
        'end run'
    )
    subprocess.run(
        ["osascript", "-e", script, "--", str(path), str(process_id)],
        check=True,
    )
    data = path.read_bytes()
    width, height = png_size(path)
    return {
        "name": path.name,
        "bytes": len(data),
        "width": width,
        "height": height,
        "sha256": hashlib.sha256(data).hexdigest(),
    }


class McpClient:
    def __init__(self, process: subprocess.Popen[str]) -> None:
        self.process = process
        self.next_id = 1

    def request(self, method: str, params: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        payload: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        }
        if params is not None:
            payload["params"] = params
        self._write(payload)
        assert self.process.stdout is not None
        while True:
            line = self.process.stdout.readline()
            if line == "":
                raise RuntimeError("rgv-mcp closed stdout before replying")
            message = json.loads(line)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RuntimeError(f"MCP protocol error for {method}: {message['error']}")
            return message["result"]

    def notification(self, method: str, params: Optional[dict[str, Any]] = None) -> None:
        payload: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        self._write(payload)

    def call_tool(self, name: str, arguments: Optional[dict[str, Any]] = None) -> dict[str, Any]:
        result = self.request(
            "tools/call",
            {"name": name, "arguments": arguments or {}},
        )
        if result.get("isError"):
            structured = result.get("structuredContent", {})
            raise RuntimeError(f"{name} failed: {structured.get('error', structured)}")
        return result

    def _write(self, payload: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
        self.process.stdin.flush()


def structured(result: dict[str, Any]) -> dict[str, Any]:
    value = result.get("structuredContent")
    if not isinstance(value, dict):
        raise RuntimeError("tool result omitted structuredContent")
    return value


def workspace_interval(result: dict[str, Any]) -> tuple[int, int, int]:
    workspace = structured(result)["workspace"]
    active_panel_id = workspace["active_panel_id"]
    panel = next(panel for panel in workspace["panels"] if panel["id"] == active_panel_id)
    return active_panel_id, panel["start"], panel["end"]


def summarize_tool_result(name: str, result: dict[str, Any]) -> dict[str, Any]:
    content_types = [item.get("type", "unknown") for item in result.get("content", [])]
    value = structured(result)
    if name == "rgv_capture_view":
        value = {
            "mime_type": value.get("mime_type"),
            "bytes": value.get("bytes"),
            "scope": value.get("scope"),
            "width": value.get("width"),
            "height": value.get("height"),
            "truncated": value.get("truncated"),
            "genome": value.get("genome"),
            "loci": value.get("loci"),
            "application_instance_id": value.get("application_instance_id"),
            "workspace_revision": value.get("workspace_revision"),
            "selection_id": value.get("selection_id"),
            "evidence_revision": value.get("evidence_revision"),
            "panels": value.get("panels"),
        }
    return {"tool": name, "content_types": content_types, "structured": value}


def confined_output(root: Path, relative: str) -> Path:
    candidate = (root / relative).resolve()
    if root != candidate and root not in candidate.parents:
        raise ValueError("report path must remain below the control root")
    candidate.parent.mkdir(parents=True, exist_ok=True)
    return candidate


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mcp-binary", type=Path, required=True)
    parser.add_argument("--control-url", default="http://127.0.0.1:60151")
    parser.add_argument("--secret-file", type=Path, required=True)
    parser.add_argument("--control-root", type=Path, required=True)
    parser.add_argument("--locus", required=True)
    parser.add_argument("--report", default="mcp-qualification/report.json")
    parser.add_argument("--saved-workspace", default="mcp-qualification/workspace.rgv.json")
    parser.add_argument("--track-names-export", default="mcp-qualification/track-names.tsv")
    parser.add_argument("--selected-evidence-prefix", default="mcp-qualification/selected-evidence")
    parser.add_argument("--screenshot-directory", default="mcp-qualification/screenshots")
    parser.add_argument("--read-name", action="append", default=[])
    parser.add_argument(
        "--evidence-position",
        type=int,
        help="zero-based MF-01 pileup position; defaults to the active interval midpoint",
    )
    arguments = parser.parse_args()

    root = arguments.control_root.resolve(strict=True)
    report_path = confined_output(root, arguments.report)
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
    actions: list[dict[str, Any]] = []
    screenshots: list[dict[str, Any]] = []
    try:
        initialized = client.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "rgv-native-qualification", "version": "1"},
            },
        )
        client.notification("notifications/initialized")
        listed = client.request("tools/list")
        names = {tool["name"] for tool in listed["tools"]}
        if names != EXPECTED_TOOLS:
            raise RuntimeError(
                f"tool contract mismatch: missing={sorted(EXPECTED_TOOLS - names)} "
                f"unexpected={sorted(names - EXPECTED_TOOLS)}"
            )

        status_result = client.call_tool("rgv_status")
        actions.append(summarize_tool_result("rgv_status", status_result))
        status = structured(status_result)["status"]
        instance_id = status.get("application_instance_id")
        process_id = status.get("process_id")
        if not instance_id or not isinstance(process_id, int) or process_id <= 0:
            raise RuntimeError("status omitted application instance identity or process ID")

        def checkpoint(name: str) -> None:
            if platform.system() != "Darwin":
                return
            observed = client.call_tool("rgv_get_workspace")
            workspace = structured(observed)["workspace"]
            if not workspace["settled"] or any(
                not panel["settled"] for panel in workspace["panels"]
            ):
                raise RuntimeError(f"checkpoint {name} observed an unsettled workspace")
            selection = workspace.get("selection") or {}
            screenshot_root = confined_output(root, arguments.screenshot_directory)
            screenshot = macos_pid_screenshot(
                screenshot_root / f"{len(screenshots):02d}-{name}.png",
                process_id,
            )
            screenshot.update(
                {
                    "application_instance_id": workspace["application_instance_id"],
                    "workspace_revision": workspace["workspace_revision"],
                    "selection_id": selection.get("selection_id"),
                    "evidence_revision": selection.get("evidence_revision"),
                    "active_panel_id": workspace["active_panel_id"],
                    "panels": [
                        {
                            "id": panel["id"],
                            "start": panel["start"],
                            "end": panel["end"],
                            "render_generation": panel["render_generation"],
                            "settled": panel["settled"],
                        }
                        for panel in workspace["panels"]
                    ],
                }
            )
            screenshots.append(screenshot)

        checkpoint("initial")
        review_result = client.call_tool("rgv_get_review_queue")
        actions.append(summarize_tool_result("rgv_get_review_queue", review_result))
        review_items = structured(review_result)["review_queue"]["items"]
        if len(review_items) != 5:
            raise RuntimeError(f"MF-01 review queue contained {len(review_items)} items, expected 5")
        if any("notes" in item or "assignment" in item for item in review_items):
            raise RuntimeError("review queue exposed private notes or assignments")
        for item in review_items:
            navigated = client.call_tool(
                "rgv_navigate_review_item",
                {"action": {"action": "item", "review_item_id": item["id"]}},
            )
            actions.append(summarize_tool_result("rgv_navigate_review_item", navigated))
            current = client.call_tool("rgv_get_workspace")
            current_panel_id, _, _ = workspace_interval(current)
            zoomed = client.call_tool(
                "rgv_change_viewport",
                {
                    "panel_id": current_panel_id,
                    "operation": {"operation": "zoom", "factor": 1.25},
                    "wait_until_settled": True,
                },
            )
            actions.append(summarize_tool_result("rgv_change_viewport", zoomed))
            panned = client.call_tool(
                "rgv_change_viewport",
                {
                    "panel_id": current_panel_id,
                    "operation": {"operation": "pan_fraction", "fraction": 0.05},
                    "wait_until_settled": True,
                },
            )
            actions.append(summarize_tool_result("rgv_change_viewport", panned))
            checkpoint(f"review-{item['id']}")

        deep_item = next(item for item in review_items if item["label"] == "Deep coverage")
        deep_navigation = client.call_tool(
            "rgv_navigate_review_item",
            {"action": {"action": "item", "review_item_id": deep_item["id"]}},
        )
        actions.append(summarize_tool_result("rgv_navigate_review_item", deep_navigation))
        deep_workspace = client.call_tool("rgv_get_workspace")
        deep_state = structured(deep_workspace)["workspace"]
        deep_panel_id = deep_state["active_panel_id"]
        deep_track_id = next(
            track["id"]
            for track in deep_state["tracks"]
            if str(track["kind"]).lower() == "alignment"
        )
        enlarged = client.call_tool(
            "rgv_configure",
            {
                "change": {
                    "setting": "track_height",
                    "track_id": deep_track_id,
                    "value": 1200.0,
                }
            },
        )
        actions.append(summarize_tool_result("rgv_configure", enlarged))
        checkpoint("deep-coverage-before-scroll")
        scrolled = client.call_tool(
            "rgv_configure",
            {
                "change": {
                    "setting": "vertical_scroll_points",
                    "panel_id": deep_panel_id,
                    "value": 120.0,
                }
            },
        )
        actions.append(summarize_tool_result("rgv_configure", scrolled))
        scroll_data = structured(scrolled)["report"]["results"][0]["data"]
        if float(scroll_data["effective"]) <= 0.0:
            raise RuntimeError("overflowing MF-01 read stack did not accept vertical scrolling")
        checkpoint("deep-coverage-scrolled")
        for change in [
            {
                "setting": "vertical_scroll_points",
                "panel_id": deep_panel_id,
                "value": 0.0,
            },
            {"setting": "track_height", "track_id": deep_track_id, "value": 420.0},
        ]:
            restored_layout = client.call_tool("rgv_configure", {"change": change})
            actions.append(summarize_tool_result("rgv_configure", restored_layout))

        for name, tool_arguments in [
            ("rgv_get_workspace", {}),
            (
                "rgv_navigate",
                {"loci": [arguments.locus], "wait_until_settled": True},
            ),
            ("rgv_get_workspace", {}),
        ]:
            result = client.call_tool(name, tool_arguments)
            actions.append(summarize_tool_result(name, result))

        before = client.call_tool("rgv_get_workspace")
        panel_id, start, end = workspace_interval(before)
        zoom = client.call_tool(
            "rgv_change_viewport",
            {
                "panel_id": panel_id,
                "operation": {"operation": "zoom", "factor": 2.0},
                "wait_until_settled": True,
            },
        )
        actions.append(summarize_tool_result("rgv_change_viewport", zoom))
        after_zoom = client.call_tool("rgv_get_workspace")
        _, zoom_start, zoom_end = workspace_interval(after_zoom)
        if zoom_end - zoom_start >= end - start:
            raise RuntimeError("zoom-in did not reduce the exact active interval")

        pan = client.call_tool(
            "rgv_change_viewport",
            {
                "panel_id": panel_id,
                "operation": {"operation": "pan_fraction", "fraction": 0.25},
                "wait_until_settled": True,
            },
        )
        actions.append(summarize_tool_result("rgv_change_viewport", pan))
        after_pan = client.call_tool("rgv_get_workspace")
        _, pan_start, pan_end = workspace_interval(after_pan)
        if (pan_start, pan_end) == (zoom_start, zoom_end):
            raise RuntimeError("pan did not change the exact active interval")
        actions.append(summarize_tool_result("rgv_get_workspace", after_pan))

        workspace = structured(after_pan)["workspace"]
        alignment = next(
            (
                track
                for track in workspace["tracks"]
                if str(track["kind"]).lower() == "alignment"
            ),
            None,
        )
        if alignment is not None:
            track_id = alignment["id"]
            for name, tool_arguments in [
                (
                    "rgv_group_alignments",
                    {
                        "track_ids": [track_id],
                        "mode": "strand",
                        "direction": "reverse",
                    },
                ),
                (
                    "rgv_sort_alignments",
                    {
                        "track_ids": [track_id],
                        "mode": "mapping_quality",
                        "direction": "natural",
                    },
                ),
                (
                    "rgv_filter_alignments",
                    {"track_ids": [track_id], "expression": ""},
                ),
                ("rgv_select", {"selection": {"selection": "track", "track_id": track_id}}),
                ("rgv_get_selection", {}),
            ]:
                result = client.call_tool(name, tool_arguments)
                actions.append(summarize_tool_result(name, result))
                if name in {
                    "rgv_group_alignments",
                    "rgv_sort_alignments",
                    "rgv_filter_alignments",
                }:
                    mutation = structured(result)["report"]["results"][0]["data"]
                    required = {"requested", "effective", "changed", "clamped", "no_op"}
                    if not required.issubset(mutation):
                        raise RuntimeError(f"{name} omitted mutation outcome fields")

            for change in [
                {"setting": "view_as_pairs", "value": False},
                {"setting": "alignment_density", "value": 2.0},
                {"setting": "alignment_color_mode", "track_ids": [track_id], "value": "pair_orientation"},
                {"setting": "read_base_display_mode", "value": "all"},
            ]:
                configured = client.call_tool("rgv_configure", {"change": change})
                actions.append(summarize_tool_result("rgv_configure", configured))

            configured_workspace = client.call_tool("rgv_get_workspace")
            actions.append(summarize_tool_result("rgv_get_workspace", configured_workspace))
            configured_track = next(
                track
                for track in structured(configured_workspace)["workspace"]["tracks"]
                if track["id"] == track_id
            )["alignment"]
            expected_settings = {
                "group_mode": "strand",
                "group_order": "reverse",
                "sort_mode": "mapping_quality",
                "sort_order": "natural",
                "view_as_pairs": False,
                "alignment_density": 2.0,
                "color_mode": "pair_orientation",
                "read_base_display_mode": "all",
            }
            for key, expected in expected_settings.items():
                if configured_track[key] != expected:
                    raise RuntimeError(
                        f"effective alignment setting {key} was {configured_track[key]!r}, "
                        f"expected {expected!r}"
                    )

            for read_name in arguments.read_name:
                found = client.call_tool(
                    "rgv_find_alignments",
                    {
                        "panel_id": panel_id,
                        "track_id": track_id,
                        "selector": {"selector": "exact_read_name", "read_name": read_name},
                        "maximum_results": 2,
                    },
                )
                actions.append(summarize_tool_result("rgv_find_alignments", found))
                matches = structured(found)["matches"]["hits"]
                if not matches:
                    raise RuntimeError("named MF-01 alignment control was not found")
                serialized_match = json.dumps(structured(found))
                if read_name in serialized_match:
                    raise RuntimeError("sensitive read name was echoed in an alignment result")
                selected = client.call_tool("rgv_select_alignment", {"handle": matches[0]["handle"]})
                actions.append(summarize_tool_result("rgv_select_alignment", selected))
                related = client.call_tool("rgv_get_related_alignments")
                actions.append(summarize_tool_result("rgv_get_related_alignments", related))

            mate_search = client.call_tool(
                "rgv_find_alignments",
                {
                    "panel_id": panel_id,
                    "track_id": track_id,
                    "selector": {"selector": "semantic", "has_mapped_mate": True},
                    "maximum_results": 8,
                },
            )
            actions.append(summarize_tool_result("rgv_find_alignments", mate_search))
            mate_hits = structured(mate_search)["matches"]["hits"]
            if not mate_hits:
                raise RuntimeError("MF-01 structural locus exposed no mapped-mate handle")
            mate_selection = client.call_tool(
                "rgv_select_alignment", {"handle": mate_hits[0]["handle"]}
            )
            actions.append(summarize_tool_result("rgv_select_alignment", mate_selection))
            mate_related = client.call_tool("rgv_get_related_alignments")
            actions.append(summarize_tool_result("rgv_get_related_alignments", mate_related))
            if not structured(mate_related)["related"]["mate_available"]:
                raise RuntimeError("mapped-mate search returned a handle without a navigable mate")
            mate_navigation = client.call_tool(
                "rgv_navigate_related_alignment",
                {"relationship": "mate", "destination": "current_panel"},
            )
            actions.append(
                summarize_tool_result("rgv_navigate_related_alignment", mate_navigation)
            )
            history = client.call_tool("rgv_get_navigation_history", {"panel_id": panel_id})
            actions.append(summarize_tool_result("rgv_get_navigation_history", history))
            history_state = structured(history)["history"]
            if not history_state["can_go_back"]:
                raise RuntimeError("mate navigation did not create a history entry")
            returned = client.call_tool(
                "rgv_navigate_history",
                {"panel_id": panel_id, "action": {"action": "back"}},
            )
            actions.append(summarize_tool_result("rgv_navigate_history", returned))

            long_item = next(
                item
                for item in review_items
                if item["label"] == "Long/supplementary/modifications"
            )
            long_navigation = client.call_tool(
                "rgv_navigate_review_item",
                {"action": {"action": "item", "review_item_id": long_item["id"]}},
            )
            actions.append(summarize_tool_result("rgv_navigate_review_item", long_navigation))
            supplementary_search = client.call_tool(
                "rgv_find_alignments",
                {
                    "panel_id": panel_id,
                    "track_id": track_id,
                    "selector": {
                        "selector": "semantic",
                        "has_supplementary_chain": True,
                    },
                    "maximum_results": 8,
                },
            )
            actions.append(summarize_tool_result("rgv_find_alignments", supplementary_search))
            supplementary_hits = structured(supplementary_search)["matches"]["hits"]
            if not supplementary_hits:
                raise RuntimeError("MF-01 long-read locus exposed no supplementary-chain handle")
            chain_selection = client.call_tool(
                "rgv_select_alignment", {"handle": supplementary_hits[0]["handle"]}
            )
            actions.append(summarize_tool_result("rgv_select_alignment", chain_selection))
            chain_related = client.call_tool("rgv_get_related_alignments")
            actions.append(summarize_tool_result("rgv_get_related_alignments", chain_related))
            segments = structured(chain_related)["related"]["supplementary_segments"]
            destination_segment = next(
                (segment for segment in segments if not segment["current"]), None
            )
            if destination_segment is None:
                raise RuntimeError("supplementary-chain handle had no alternate segment")
            chain_navigation = client.call_tool(
                "rgv_navigate_related_alignment",
                {
                    "relationship": "supplementary_segment",
                    "segment_index": destination_segment["segment_index"],
                    "destination": "current_panel",
                },
            )
            actions.append(
                summarize_tool_result("rgv_navigate_related_alignment", chain_navigation)
            )
            chain_return = client.call_tool(
                "rgv_navigate_history",
                {"panel_id": panel_id, "action": {"action": "back"}},
            )
            actions.append(summarize_tool_result("rgv_navigate_history", chain_return))
            long_navigation = client.call_tool(
                "rgv_navigate_review_item",
                {"action": {"action": "item", "review_item_id": long_item["id"]}},
            )
            actions.append(summarize_tool_result("rgv_navigate_review_item", long_navigation))
            for _ in range(4):
                long_workspace = client.call_tool("rgv_get_workspace")
                long_state = structured(long_workspace)["workspace"]
                active_panel = next(
                    panel
                    for panel in long_state["panels"]
                    if panel["id"] == long_state["active_panel_id"]
                )
                reads_lane = next(
                    lane for lane in active_panel["render_lanes"] if lane["lane"] == "reads"
                )
                if reads_lane["rendered"]:
                    break
                zoom_to_reads = client.call_tool(
                    "rgv_change_viewport",
                    {
                        "panel_id": panel_id,
                        "operation": {"operation": "zoom", "factor": 10.0},
                        "wait_until_settled": True,
                    },
                )
                actions.append(summarize_tool_result("rgv_change_viewport", zoom_to_reads))
            else:
                raise RuntimeError("MF-01 long-read lane remained omitted after bounded zooming")
            checkpoint("long-reads-rendered")

            evidence_navigation = client.call_tool(
                "rgv_navigate",
                {"loci": [arguments.locus], "wait_until_settled": True},
            )
            actions.append(summarize_tool_result("rgv_navigate", evidence_navigation))

            current = client.call_tool("rgv_get_workspace")
            _, selected_start, selected_end = workspace_interval(current)
            position = (
                arguments.evidence_position
                if arguments.evidence_position is not None
                else selected_start + (selected_end - selected_start) // 2
            )
            selected = client.call_tool(
                "rgv_select",
                {
                    "selection": {
                        "selection": "position",
                        "panel_id": panel_id,
                        "track_id": track_id,
                        "position": position,
                        "granularity": "exact_base",
                    }
                },
            )
            actions.append(summarize_tool_result("rgv_select", selected))
            evidence_before = client.call_tool("rgv_get_selection")
            actions.append(summarize_tool_result("rgv_get_selection", evidence_before))
            before_selection = structured(evidence_before)["selection"]
            eligible_reads = next(
                (
                    field["value"]
                    for field in before_selection["fields"]
                    if field["key"] == "Evidence"
                ),
                "",
            )
            if " 0 eligible reads" in f" {eligible_reads}":
                raise RuntimeError("MF-01 evidence position selected a zero-depth pileup")
            export_before = client.call_tool(
                "rgv_export",
                {"path": f"{arguments.selected_evidence_prefix}-before.tsv", "kind": "selected_evidence"},
            )
            actions.append(summarize_tool_result("rgv_export", export_before))
            moved = client.call_tool(
                "rgv_change_viewport",
                {
                    "panel_id": panel_id,
                    "operation": {"operation": "zoom", "factor": 0.25},
                    "wait_until_settled": True,
                },
            )
            actions.append(summarize_tool_result("rgv_change_viewport", moved))
            evidence_after = client.call_tool("rgv_get_selection")
            actions.append(summarize_tool_result("rgv_get_selection", evidence_after))
            after_selection = structured(evidence_after)["selection"]
            for key in ("selection_id", "evidence_revision", "evidence_start", "evidence_end", "fields"):
                if before_selection.get(key) != after_selection.get(key):
                    raise RuntimeError(f"retained selection changed after viewport mutation: {key}")
            export_after = client.call_tool(
                "rgv_export",
                {"path": f"{arguments.selected_evidence_prefix}-after.tsv", "kind": "selected_evidence"},
            )
            actions.append(summarize_tool_result("rgv_export", export_after))
            if structured(export_before).get("inline_text") != structured(export_after).get("inline_text"):
                raise RuntimeError("selected-evidence export changed after viewport mutation")
            refreshed = client.call_tool("rgv_refresh_selection")
            actions.append(summarize_tool_result("rgv_refresh_selection", refreshed))
            refreshed_data = structured(refreshed)["report"]["results"][0]["data"]
            if refreshed_data["selection"]["selection_id"] == before_selection["selection_id"]:
                raise RuntimeError("explicit refresh did not create a new selection ID")
            checkpoint("retained-evidence")

        capture = client.call_tool(
            "rgv_capture_view",
            {"scope": "visible_workspace", "wait_until_settled": True},
        )
        actions.append(summarize_tool_result("rgv_capture_view", capture))
        cleared = client.call_tool("rgv_clear_selection")
        actions.append(summarize_tool_result("rgv_clear_selection", cleared))
        cleared_selection = client.call_tool("rgv_get_selection")
        actions.append(summarize_tool_result("rgv_get_selection", cleared_selection))
        if structured(cleared_selection)["selection"] is not None:
            raise RuntimeError("clear-selection left retained evidence visible")
        exported = client.call_tool(
            "rgv_export",
            {
                "path": arguments.track_names_export,
                "kind": "track_names",
                "wait_until_settled": True,
            },
        )
        actions.append(summarize_tool_result("rgv_export", exported))
        saved = client.call_tool(
            "rgv_save_workspace",
            {"path": arguments.saved_workspace},
        )
        actions.append(summarize_tool_result("rgv_save_workspace", saved))
        reopened = client.call_tool(
            "rgv_open_workspace",
            {
                "path": arguments.saved_workspace,
                "confirm_replace": True,
                "unsaved_changes": "discard",
                "wait_until_settled": True,
            },
        )
        actions.append(summarize_tool_result("rgv_open_workspace", reopened))
        restored = client.call_tool("rgv_get_workspace")
        actions.append(summarize_tool_result("rgv_get_workspace", restored))
        restored_workspace = structured(restored)["workspace"]
        if restored_workspace.get("application_instance_id") != instance_id:
            raise RuntimeError("in-process workspace open changed application instance")
        restored_review = client.call_tool("rgv_get_review_queue")
        if len(structured(restored_review)["review_queue"]["items"]) != 5:
            raise RuntimeError("save/open did not preserve the MF-01 review queue")
        checkpoint("reopened")

        report = {
            "schema_version": 2,
            "qualified": True,
            "commit": subprocess.run(
                ["git", "rev-parse", "HEAD"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip(),
            "mcp_protocol": initialized.get("protocolVersion"),
            "server": initialized.get("serverInfo"),
            "tools": sorted(names),
            "actions": actions,
            "application_instance_id": instance_id,
            "process_id": process_id,
            "platform": platform.platform(),
            "renderer_backend": status.get("renderer_backend"),
            "surface_width_pixels": status.get("surface_width_pixels"),
            "surface_height_pixels": status.get("surface_height_pixels"),
            "scale_factor": status.get("scale_factor"),
            "screenshots": screenshots,
        }
        serialized = json.dumps(report, indent=2, sort_keys=True) + "\n"
        for prohibited in (str(root), str(arguments.secret_file), "Bearer "):
            if prohibited in serialized:
                raise RuntimeError("qualification report contains a prohibited path or secret marker")
        report_path.write_text(serialized, encoding="utf-8")
        print(report_path.relative_to(root).as_posix())
        return 0
    finally:
        if process.stdin is not None:
            process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.terminate()
            process.wait(timeout=5)


if __name__ == "__main__":
    sys.exit(main())
