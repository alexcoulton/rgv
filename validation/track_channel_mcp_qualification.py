#!/usr/bin/env python3
"""Structural and native-window qualification for track-channel presentation."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
from typing import Any

from rgv_mcp_qualification import McpClient, save_mcp_window_capture, structured


def alignment_ids(workspace: dict[str, Any]) -> list[int]:
    return [track["id"] for track in workspace["tracks"] if track.get("alignment") is not None]


def presentation_group(
    workspace: dict[str, Any], channel: str, members: list[int]
) -> dict[str, Any]:
    expected = set(members)
    return next(
        group
        for group in workspace["presentation_groups"]
        if group["channel"] == channel and set(group["members"]) == expected
    )


def read_state(workspace: dict[str, Any], panel_id: int, track_id: int) -> dict[str, Any]:
    panel = next(panel for panel in workspace["panels"] if panel["id"] == panel_id)
    return next(state for state in panel["read_panel_states"] if state["track_id"] == track_id)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mcp-binary", type=Path, required=True)
    parser.add_argument("--control-url", required=True)
    parser.add_argument("--secret-file", type=Path, required=True)
    parser.add_argument("--control-root", type=Path, required=True)
    parser.add_argument(
        "--evidence-directory",
        default="validation/manual-suite/evidence/track-channel-architecture",
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

    def call(name: str, arguments: dict[str, Any] | None = None) -> dict[str, Any]:
        return structured(client.call_tool(name, arguments))

    def workspace() -> dict[str, Any]:
        value = call("rgv_get_workspace")["workspace"]
        if not value["settled"] or any(not panel["settled"] for panel in value["panels"]):
            raise RuntimeError("workspace did not settle before observation")
        return value

    def configure(change: dict[str, Any]) -> dict[str, Any]:
        return call("rgv_configure", {"change": change})

    def capture(name: str) -> None:
        result = client.call_tool("rgv_capture_window", {"wait_until_settled": True})
        captures.append(save_mcp_window_capture(evidence / f"{name}.png", result))

    try:
        client.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "track-channel-qualification", "version": "1"},
            },
        )
        client.notification("notifications/initialized")
        tools = client.request("tools/list")["tools"]
        configure_schema = next(tool for tool in tools if tool["name"] == "rgv_configure")
        encoded_schema = json.dumps(configure_schema["inputSchema"], sort_keys=True)
        for required in [
            "presentation_section_expanded",
            "presentation_node_visible",
            "presentation_group_expanded",
            "presentation_group_visible",
            "presentation_composite_visible",
            "read_pane_height",
            "read_stack_scroll_points",
            "read_panel_scroll_points",
            "read_panel_density",
        ]:
            if required not in encoded_schema:
                raise RuntimeError(f"MCP configure schema omitted {required}")

        call(
            "rgv_open_workspace",
            {
                "path": "benchmark-data/manual-suite/projects/mf-04-bisulfite.rgv.json",
                "confirm_replace": True,
                "unsaved_changes": "discard",
            },
        )
        mf04 = workspace()
        ids = alignment_ids(mf04)
        if len(ids) != 6:
            raise RuntimeError(f"MF-04 expected six alignment tracks, observed {ids}")
        first, grouped = ids[0], ids[1:]
        configure({"setting": "presentation_group", "channel": "reads", "track_ids": grouped})
        configure(
            {
                "setting": "presentation_node_visible",
                "target": {"kind": "track", "channel": "reads", "track_id": grouped[1]},
                "value": False,
            }
        )
        mf04 = workspace()
        read_group = presentation_group(mf04, "reads", grouped)
        group_target = {
            "kind": "group",
            "channel": "reads",
            "group_id": read_group["id"],
        }
        configure({"setting": "presentation_group_visible", "target": group_target, "value": False})
        configure({"setting": "presentation_group_expanded", "target": group_target, "value": False})
        configure(
            {
                "setting": "presentation_node_visible",
                "target": {"kind": "track", "channel": "reads", "track_id": first},
                "value": False,
            }
        )
        hidden = workspace()
        hidden_group = presentation_group(hidden, "reads", grouped)
        if hidden_group["content_visible"] or hidden_group["expanded"]:
            raise RuntimeError("hiding the ungrouped read mutated the hidden group gates")
        layout = next(
            layout
            for layout in hidden["pane_layouts"]
            if layout["panel_id"] == hidden["active_panel_id"] and layout["channel"] == "reads"
        )
        group_layout = next(
            item
            for item in layout["items"]
            if item["id"].get("kind") == "group"
            and item["id"].get("group_id") == read_group["id"]
        )
        if group_layout["header"]["bottom"] - group_layout["header"]["top"] < 21.5:
            raise RuntimeError("read group restore header fell below its interaction minimum")
        capture("mf04-hidden-read-group")

        configure(
            {
                "setting": "presentation_node_visible",
                "target": {"kind": "track", "channel": "reads", "track_id": first},
                "value": True,
            }
        )
        configure({"setting": "presentation_group_visible", "target": group_target, "value": True})
        configure({"setting": "presentation_group_expanded", "target": group_target, "value": True})
        panel_id = hidden["active_panel_id"]
        expected_density: dict[int, float] = {}
        for index, track_id in enumerate(grouped):
            density = 1.0 + index * 0.25
            configure(
                {
                    "setting": "read_panel_density",
                    "panel_id": panel_id,
                    "track_id": track_id,
                    "value": density,
                }
            )
            expected_density[track_id] = density
        before_minimize = workspace()
        configure({"setting": "presentation_group_expanded", "target": group_target, "value": False})
        configure({"setting": "presentation_group_expanded", "target": group_target, "value": True})
        after_expand = workspace()
        for track_id, density in expected_density.items():
            before = read_state(before_minimize, panel_id, track_id)
            after = read_state(after_expand, panel_id, track_id)
            if before["effective_density"] != density or after != before:
                raise RuntimeError(f"read camera/density changed across minimize for track {track_id}")
        configure(
            {"setting": "presentation_group", "channel": "annotation", "track_ids": grouped}
        )
        independent = workspace()
        if len([group for group in independent["presentation_groups"] if group["channel"] == "reads"]) != 1:
            raise RuntimeError("annotation grouping mutated the Reads group")
        capture("mf04-restored-independent-channels")

        call(
            "rgv_open_workspace",
            {
                "path": "benchmark-data/manual-suite/projects/mf-12-workspaces.rgv.json",
                "confirm_replace": True,
                "unsaved_changes": "discard",
            },
        )
        mf12 = workspace()
        mf12_ids = alignment_ids(mf12)
        if len(mf12_ids) != 4 or len(mf12["panels"]) != 2:
            raise RuntimeError("MF-12 did not restore four tracks across two panels")
        configure({"setting": "read_pane_height", "value": 480.0})
        configure(
            {"setting": "presentation_group", "channel": "reads", "track_ids": mf12_ids[1:]}
        )
        configure(
            {
                "setting": "presentation_group",
                "channel": "annotation",
                "track_ids": mf12_ids[:3],
            }
        )
        panel_ids = [panel["id"] for panel in mf12["panels"]]
        configure(
            {
                "setting": "read_panel_density",
                "panel_id": panel_ids[0],
                "track_id": mf12_ids[0],
                "value": 1.25,
            }
        )
        configure(
            {
                "setting": "read_panel_density",
                "panel_id": panel_ids[1],
                "track_id": mf12_ids[0],
                "value": 2.25,
            }
        )
        configure(
            {
                "setting": "read_panel_scroll_points",
                "panel_id": panel_ids[0],
                "track_id": mf12_ids[0],
                "value": 40.0,
            }
        )
        configure(
            {
                "setting": "read_panel_scroll_points",
                "panel_id": panel_ids[1],
                "track_id": mf12_ids[0],
                "value": 80.0,
            }
        )
        split = workspace()
        if split["read_pane_height"] != 480.0:
            raise RuntimeError("workspace-owned read pane height did not apply")
        if read_state(split, panel_ids[0], mf12_ids[0])["effective_density"] == read_state(
            split, panel_ids[1], mf12_ids[0]
        )["effective_density"]:
            raise RuntimeError("split panels did not retain independent read density")
        if [
            read_state(split, panel_id, mf12_ids[0])["scroll_points"]
            for panel_id in panel_ids
        ] != [40.0, 80.0]:
            raise RuntimeError("split panels did not retain independent read scroll offsets")
        if len(split["pane_layouts"]) != 4:
            raise RuntimeError("split workspace omitted a pane layout/scrollbar owner")
        capture("mf12-split-panel-cameras")
        call(
            "rgv_save_workspace",
            {"path": f"{arguments.evidence_directory}/mf12-roundtrip.rgv.json"},
        )
        call(
            "rgv_open_workspace",
            {
                "path": f"{arguments.evidence_directory}/mf12-roundtrip.rgv.json",
                "confirm_replace": True,
                "unsaved_changes": "discard",
            },
        )
        restored = workspace()
        if restored["presentation_groups"] != split["presentation_groups"]:
            raise RuntimeError("presentation groups changed across save/reopen")
        for panel_id in panel_ids:
            if read_state(restored, panel_id, mf12_ids[0]) != read_state(
                split, panel_id, mf12_ids[0]
            ):
                raise RuntimeError(f"panel-local read state changed for panel {panel_id}")
        capture("mf12-save-reopen")

        report = {
            "result": "passed",
            "mf04_application_instance_id": mf04["application_instance_id"],
            "mf12_application_instance_id": restored["application_instance_id"],
            "automation_schema_version": restored["schema_version"],
            "captures": captures,
            "mf04": {
                "alignment_ids": ids,
                "read_group": hidden_group,
                "pane_layout_count": len(independent["pane_layouts"]),
            },
            "mf12": {
                "alignment_ids": mf12_ids,
                "panel_ids": panel_ids,
                "read_pane_height": restored["read_pane_height"],
                "presentation_groups": restored["presentation_groups"],
                "read_panel_states": [
                    read_state(restored, panel_id, mf12_ids[0]) for panel_id in panel_ids
                ],
            },
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
