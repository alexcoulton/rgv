#!/usr/bin/env python3
"""State-matched real-ONT qualification for RGV against a running IGV Desktop."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import platform
import secrets
import shlex
import socket
import subprocess
import sys
import time
from typing import Any

from rgv_mcp_qualification import McpClient, save_mcp_window_capture, structured


DATA = {
    "bam": (
        "/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.bam",
        "c48afe5d794c781582dc9e0be11d4e68e4373903f1c5f15ce8beca11643b303c",
    ),
    "bai": (
        "/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.bam.bai",
        "8fc3a0e0c1462dbcb2b687b07b6a5eb9f84e8b5f23f25ebbf02489febbc72909",
    ),
    "fasta": (
        "/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.fasta",
        "50c15bd3c4bf5a3ee288db8f48bed59801c5d522bac6ab4188a6c452fc331995",
    ),
    "fai": (
        "/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.fasta.fai",
        "5497607dbad7e45e3826663474336332af4df84cae62169081a4baa3f4047682",
    ),
}
LOCUS = "chr6:1-630038"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def png_size(path: Path) -> tuple[int, int]:
    data = path.read_bytes()[:24]
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise RuntimeError(f"{path} is not a PNG")
    return int.from_bytes(data[16:20], "big"), int.from_bytes(data[20:24], "big")


def save_mcp_view_capture(path: Path, result: dict[str, Any]) -> dict[str, Any]:
    images = [item for item in result.get("content", []) if item.get("type") == "image"]
    if len(images) != 1 or images[0].get("mimeType") != "image/png":
        raise RuntimeError("rgv_capture_view did not return one PNG")
    data = base64.b64decode(images[0]["data"], validate=True)
    path.write_bytes(data)
    width, height = png_size(path)
    metadata = structured(result)
    return {
        "path": path.name,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
        "width": width,
        "height": height,
        "application_instance_id": metadata.get("application_instance_id"),
        "workspace_revision": metadata.get("workspace_revision"),
        "panels": metadata.get("panels"),
    }


def free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def igv_command(port: int, command: str) -> str:
    with socket.create_connection(("127.0.0.1", port), timeout=5) as connection:
        connection.sendall((command + "\n").encode())
        connection.settimeout(10)
        try:
            return connection.recv(64 * 1024).decode(errors="replace").strip()
        except socket.timeout:
            # IGV occasionally applies a goto command without replying on the
            # command socket. The visible locus in the subsequent window capture
            # is the authoritative state check.
            return "command accepted; IGV returned no socket response"


def capture_igv(path: Path) -> dict[str, Any]:
    quoted = shlex.quote(str(path))
    script = [
        'tell application "IGV" to activate',
        "delay 2",
        (
            'tell application "System Events" to tell first application process whose '
            "frontmost is true\n"
            "set {windowX, windowY} to position of front window\n"
            "set {windowW, windowH} to size of front window\n"
            "end tell\n"
            f'do shell script "/usr/sbin/screencapture -x -R" & windowX & "," & '
            f'windowY & "," & windowW & "," & windowH & " {quoted}"\n'
            'return (windowX as text) & "," & (windowY as text) & "," & '
            '(windowW as text) & "," & (windowH as text)'
        ),
    ]
    command = ["osascript"]
    for expression in script:
        command.extend(["-e", expression])
    bounds = subprocess.run(command, check=True, capture_output=True, text=True).stdout.strip()
    width, height = png_size(path)
    data = path.read_bytes()
    return {
        "path": path.name,
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
        "width": width,
        "height": height,
        "window_bounds_points": [int(value) for value in bounds.split(",")],
        "command": command,
    }


def initialize(client: McpClient) -> dict[str, Any]:
    initialized = client.request(
        "initialize",
        {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "ont-igv-parity", "version": "1"},
        },
    )
    client.notification("notifications/initialized")
    return initialized


def wait_for_workspace(client: McpClient, timeout: float = 90.0) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            result = client.call_tool("rgv_get_workspace", {})
            workspace = structured(result)["workspace"]
            if workspace["settled"] and all(panel["settled"] for panel in workspace["panels"]):
                return workspace
        except Exception as error:  # The GUI may still be opening its control endpoint.
            last_error = error
        time.sleep(0.5)
    raise RuntimeError(f"RGV did not settle: {last_error}")


def active_state(workspace: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    panel = next(
        panel for panel in workspace["panels"] if panel["id"] == workspace["active_panel_id"]
    )
    track = next(track for track in workspace["tracks"] if str(track["kind"]).lower() == "alignment")
    lane = next(lane for lane in panel["render_lanes"] if lane["lane"] == "reads")
    return panel, track, lane


def assert_scenario(
    workspace: dict[str, Any], *, secondary: bool, eligible: int, maximum: int
) -> dict[str, Any]:
    panel, track, lane = active_state(workspace)
    if (panel["contig"], panel["start"], panel["end"]) != ("chr6", 0, 630_038):
        raise RuntimeError(f"state mismatch: RGV active panel is {panel}")
    alignment = track["alignment"]
    if alignment["include_secondary"] is not secondary or not alignment["include_supplementary"]:
        raise RuntimeError("state mismatch: alignment flags do not match the declared scenario")
    expected_secondary_filtered = 0 if secondary else 1_714
    checks = {
        "eligible": lane["queried_items"],
        "secondary_filtered": lane["filtered_secondary_items"],
        "maximum_coverage": lane["raw_maximum_coverage"],
        "coverage_exact_complete": lane["coverage_exact_complete"],
        "depth_segments_omitted": lane["depth_segments_omitted"],
        "regional_insertions_rendered": lane["regional_insertions_rendered"],
        "regional_insertions_collapsed": lane["regional_insertions_collapsed"],
        "regional_insertions_omitted": lane["regional_insertions_omitted"],
    }
    if checks["eligible"] != eligible:
        raise RuntimeError(f"eligible count {checks['eligible']} != {eligible}")
    if checks["secondary_filtered"] != expected_secondary_filtered:
        raise RuntimeError("secondary exclusion provenance does not match the ONT truth")
    if checks["maximum_coverage"] != maximum or not checks["coverage_exact_complete"]:
        raise RuntimeError(f"exact maximum coverage did not equal {maximum}: {checks}")
    if checks["depth_segments_omitted"] != 0:
        raise RuntimeError("real ONT coverage exhausted the exact-depth event budget")
    if not secondary and checks["regional_insertions_rendered"] <= 0:
        raise RuntimeError("Regional rendering emitted no insertion markers")
    return {"panel": panel, "track": track, "lane": lane, "checks": checks}


def assert_genome_context(workspace: dict[str, Any]) -> dict[str, Any]:
    genome = workspace["genome"]
    identity = genome.get("assembly_identity") or {}
    if (
        identity.get("stable_id") != "GRCh38.no_alt_analysis_set"
        or not genome.get("reference_is_subset")
        or genome.get("reference_contig_count") != 3
        or not genome.get("cytobands_loaded")
        or genome.get("default_annotation_count") != 1
        or genome.get("gene_model_track_count", 0) < 1
    ):
        raise RuntimeError(f"GRCh38 context is incomplete: {genome}")
    gene_tracks = [
        track for track in workspace["tracks"] if str(track["kind"]).lower() == "genemodel"
    ]
    if len(gene_tracks) != 1 or gene_tracks[0].get("lifecycle") != "ready":
        raise RuntimeError(f"MANE gene track is not ready: {gene_tracks}")
    return genome


def configure(client: McpClient, setting: str, value: Any) -> None:
    client.call_tool("rgv_configure", {"change": {"setting": setting, "value": value}})


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--desktop-binary", type=Path, default=root / "target/release/rgv-desktop")
    parser.add_argument("--mcp-binary", type=Path, default=root / "target/release/rgv-mcp")
    parser.add_argument("--genome-config", type=Path, default=root / "validation/ont-grch38-subset.rgvgenome")
    parser.add_argument("--output", type=Path, default=root / "validation/ont-igv-parity-output")
    parser.add_argument("--igv-port", type=int, default=60151)
    parser.add_argument("--skip-igv", action="store_true")
    arguments = parser.parse_args()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)

    verified: dict[str, Any] = {}
    paths: dict[str, Path] = {}
    for role, (raw_path, expected_hash) in DATA.items():
        path = Path(raw_path).resolve(strict=True)
        observed_hash = sha256(path)
        if observed_hash != expected_hash:
            raise RuntimeError(f"{role} hash changed: {observed_hash}")
        paths[role] = path
        verified[role] = {"path": str(path), "sha256": observed_hash, "bytes": path.stat().st_size}

    port = free_port()
    secret_file = output / "control.secret"
    secret_file.write_text(secrets.token_urlsafe(48) + "\n", encoding="utf-8")
    secret_file.chmod(0o600)
    settings_file = output / f"settings-{os.getpid()}.json"
    launch = [
        str(arguments.desktop_binary.resolve(strict=True)),
        "--bam", str(paths["bam"]),
        "--genome-config", str(arguments.genome_config.resolve(strict=True)),
        "--locus", LOCUS,
        "--settings-path", str(settings_file),
        "--control-port", str(port),
        "--control-secret-file", str(secret_file),
        "--control-root", str(root.parent),
    ]
    desktop = subprocess.Popen(launch, cwd=root)
    mcp = subprocess.Popen(
        [
            str(arguments.mcp_binary.resolve(strict=True)),
            "--control-url", f"http://127.0.0.1:{port}",
            "--control-secret-file", str(secret_file),
            "--control-root", str(root.parent),
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        bufsize=1,
    )
    client = McpClient(mcp)
    try:
        initialized = initialize(client)
        wait_for_workspace(client)
        client.call_tool("rgv_navigate", {"loci": [LOCUS], "wait_until_settled": True})
        for setting, value in (
            ("include_secondary", False),
            ("include_supplementary", True),
            ("coverage_scale_transform", "linear"),
            ("alignment_density", 1.0),
        ):
            configure(client, setting, value)
        default_workspace = wait_for_workspace(client)
        default_state = assert_scenario(
            default_workspace, secondary=False, eligible=1_755, maximum=167
        )
        genome = assert_genome_context(default_workspace)

        rgv_window = save_mcp_window_capture(
            output / "rgv-default-window.png",
            client.call_tool("rgv_capture_window", {"wait_until_settled": True}),
        )
        rgv_view_result = client.call_tool("rgv_capture_view", {"wait_until_settled": True})
        rgv_view = save_mcp_view_capture(output / "rgv-default-view.png", rgv_view_result)

        configure(client, "include_secondary", True)
        all_workspace = wait_for_workspace(client)
        all_state = assert_scenario(all_workspace, secondary=True, eligible=3_469, maximum=603)
        configure(client, "include_secondary", False)
        default_restored_workspace = wait_for_workspace(client)
        assert_scenario(
            default_restored_workspace, secondary=False, eligible=1_755, maximum=167
        )

        project_path = output / "restored.rgv.json"
        try:
            project_relative = str(project_path.relative_to(root.parent))
        except ValueError as error:
            raise RuntimeError(
                f"qualification output must remain under control root {root.parent}"
            ) from error
        save_result = structured(
            client.call_tool("rgv_save_workspace", {"path": project_relative})
        )
        open_result = structured(
            client.call_tool(
                "rgv_open_workspace",
                {
                    "path": project_relative,
                    "confirm_replace": True,
                    "unsaved_changes": "discard",
                    "wait_until_settled": True,
                },
            )
        )
        reopened_workspace = wait_for_workspace(client)
        assert_scenario(reopened_workspace, secondary=False, eligible=1_755, maximum=167)
        reopened_genome = assert_genome_context(reopened_workspace)

        igv: dict[str, Any] | None = None
        if not arguments.skip_igv:
            responses = {
                "version": igv_command(arguments.igv_port, "version"),
                "goto": igv_command(arguments.igv_port, f"goto {LOCUS}"),
            }
            time.sleep(2)
            capture = capture_igv(output / "igv-default-window.png")
            igv = {"responses": responses, "capture": capture, "locus": LOCUS}

        report = {
            "schema_version": 1,
            "qualified": True,
            "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "platform": platform.platform(),
            "commit": subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=root, check=True, capture_output=True, text=True
            ).stdout.strip(),
            "data": verified,
            "locus": LOCUS,
            "launch_command": launch,
            "mcp_server": initialized.get("serverInfo"),
            "default_primary_plus_supplementary": default_state,
            "all_alignments": all_state,
            "genome": genome,
            "workspace_round_trip": {
                "path": project_relative,
                "save": save_result,
                "open": open_result,
                "genome": reopened_genome,
            },
            "captures": {"rgv_window": rgv_window, "rgv_view": rgv_view, "igv": igv},
        }
        (output / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        print(output / "report.json")
        return 0
    finally:
        if mcp.stdin is not None:
            mcp.stdin.close()
        try:
            mcp.wait(timeout=5)
        except subprocess.TimeoutExpired:
            mcp.terminate()
            mcp.wait(timeout=5)
        desktop.terminate()
        try:
            desktop.wait(timeout=10)
        except subprocess.TimeoutExpired:
            desktop.kill()
            desktop.wait(timeout=5)
        try:
            secret_file.unlink()
        except FileNotFoundError:
            pass
        try:
            settings_file.unlink()
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
