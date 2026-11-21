# Controlling RGV from an MCP agent

`rgv-mcp` is a local stdio MCP server for inspecting and manipulating one visible RGV desktop
window. Agent operations use the same automation executor as RGV's direct control API: navigation,
zoom, track changes, selection, and configuration are immediately visible in the attached window.
The MCP process never embeds the renderer and cannot enable desktop control by itself.

The initial implementation uses MCP through `rmcp` 3.1.4 and supports the protocol versions
negotiated by that SDK. It deliberately provides stdio transport only.

## Build and start

Build both binaries from a locked checkout:

```sh
cargo build --release --locked -p rgv-desktop -p rgv-mcp
```

Choose a directory that contains every local input and output the agent may access. Create a
random bearer secret in a file readable only by your account. On Unix-like systems:

```sh
mkdir -p /path/to/review-workspace
openssl rand -hex 32 > /path/to/rgv-control.secret
chmod 600 /path/to/rgv-control.secret
```

Start the viewer explicitly:

```sh
target/release/rgv-desktop \
  --control-port 60151 \
  --control-secret-file /path/to/rgv-control.secret \
  --control-root /path/to/review-workspace
```

RGV binds this service to loopback. Without all three control options, agent control remains off.
The MCP server can start before the viewer and reports `unavailable` until the window appears.

## Configure a stdio MCP host

Configure the host to launch the packaged `rgv-mcp` executable directly. A host using the common
`mcpServers` JSON form can use:

```json
{
  "mcpServers": {
    "rgv": {
      "command": "/absolute/path/to/rgv-mcp",
      "args": [
        "--control-url", "http://127.0.0.1:60151",
        "--control-secret-file", "/path/to/rgv-control.secret",
        "--control-root", "/path/to/review-workspace"
      ]
    }
  }
}
```

The host sees the secret-file path but never receives the secret as a tool argument or result.
Do not put the secret value in JSON, an environment variable, or a command-line option. The
equivalent non-secret environment variables are `RGV_MCP_CONTROL_URL`,
`RGV_MCP_CONTROL_SECRET_FILE`, and `RGV_MCP_CONTROL_ROOT`. The optional temporary-artifact
subdirectory is selected with `--artifact-directory`; it must be relative to the control root.

The implementation's in-process MCP client test performs a real initialize handshake, lists the
complete tool contract, and calls `rgv_status` and `rgv_get_workspace`. Use those two tools first
when qualifying a new host.

### First supported host: Codex CLI

Codex accepts a stdio server command after `--`. Register the packaged adapter with:

```sh
codex mcp add rgv -- \
  /absolute/path/to/rgv-mcp \
  --control-url http://127.0.0.1:60151 \
  --control-secret-file /path/to/rgv-control.secret \
  --control-root /path/to/review-workspace
codex mcp get rgv
```

Restart or begin a fresh Codex session after changing the MCP configuration, then ask it to call
`rgv_status` and `rgv_get_workspace`. Remove the registration with `codex mcp remove rgv`.
See the [official Codex MCP documentation](https://developers.openai.com/codex/mcp/) for current
host configuration details. This example passes only the protected file's path; it does not use
`--env` to expose the secret value.

## Coordinates and waiting

- Locus strings such as `chr1:1,000,001-1,100,000` are conventional one-based, inclusive display
  coordinates.
- Numeric `position`, `start`, and `end` fields are zero-based; intervals are half-open.
- A zoom factor greater than `1` zooms in. A factor below `1` zooms out. The accepted range is
  `0.01..=100`.
- Pans and zooms clamp to contig bounds and return the exact resulting interval in the automation
  report.
- Query-triggering tools wait for generation-based settlement by default. Set
  `wait_until_settled` to `false` only when testing intermediate state. Settlement timeouts use
  milliseconds and inherit RGV's validated upper bound.
- Panel and track IDs are positive stable identities. Inspect the workspace after changes rather
  than inferring an ID from display order.
- Every workspace observation carries an unpredictable per-process `application_instance_id` and
  monotonic `workspace_revision`. Status also reports the local PID used for authenticated native
  window capture.
- A selected evidence object is immutable. `selection_id`, `evidence_revision`, fields, exact
  half-open evidence interval, and selected-evidence export do not change after pan or zoom.
  `rgv_refresh_selection` is the only operation that replaces that evidence explicitly.
- Position selection defaults to `exact_base`; choose `visible_bin` only when the current rendered
  bin is itself the evidence under review. `anchor_position` and the evidence interval are
  intentionally separate. A settled annotation snapshot is selected in its current viewport;
  only alignment evidence that lacks an exact one-base pileup bin may require a one-base query.

## Tools

| Tool | Effect |
|---|---|
| `rgv_status` | Reports reachability, versions, compatibility, and non-sensitive capabilities. |
| `rgv_get_workspace` | Returns bounded genome, ordered panel, track, readiness, and selection state. Performance telemetry is included only with `include_telemetry: true`. |
| `rgv_get_selection` | Returns bounded structured evidence for the semantic selection. |
| `rgv_get_interaction_trace` | Returns the bounded input-to-present timeline for the most recent physical vertical-scrollbar gesture. |
| `rgv_clear_selection` | Clears all selection state and closes the inspector. |
| `rgv_refresh_selection` | Explicitly creates a new immutable evidence revision. |
| `rgv_get_review_queue` | Lists review items without notes or assignments. |
| `rgv_navigate_review_item` | Navigates by item ID or to the next pending item. |
| `rgv_get_navigation_history` | Observes history without moving its cursor. |
| `rgv_navigate_history` | Moves backward, forward, or to a bounded history index. |
| `rgv_find_alignments` | Finds records in the reported settled query scope and returns opaque generation-scoped handles. |
| `rgv_select_alignment` | Selects one opaque alignment handle and can atomically leave it, center it, or fit it in view. |
| `rgv_get_related_alignments` | Describes mate and supplementary-chain availability. |
| `rgv_navigate_related_alignment` | Opens a mate or chain segment. |
| `rgv_navigate` | Replaces the visible panel arrangement with exact locus strings. |
| `rgv_change_viewport` | Pans by bases/fraction, zooms around an optional anchor, or fits an interval. |
| `rgv_load_track` | Loads a typed local path or HTTP/S remote source and returns the command report. |
| `rgv_remove_track` | Destructively removes one stable track ID. |
| `rgv_configure` | Applies a valid-by-construction typed setting and returns effective state. |
| `rgv_group_alignments` | Applies a typed grouping mode and group order to stable alignment track IDs. |
| `rgv_sort_alignments` | Applies a typed sort mode and direction. |
| `rgv_filter_alignments` | Applies RGV's validated alignment-filter expression. |
| `rgv_select` | Selects a panel, track, or zero-based genomic position. |
| `rgv_capture_view` | Waits if requested and returns a bounded PNG as MCP image content, downscaled to `max_width` (default 1600 px). |
| `rgv_capture_window` | Captures the complete native RGV client area without activating it, downscaled to `max_width` (default 1600 px). |
| `rgv_export` | Writes a structured export under the root and inlines only small UTF-8 results, with read names pseudonymized unless `include_read_names: true`. |
| `rgv_save_workspace` | Saves an RGV project under the root. |
| `rgv_open_workspace` | Explicitly replaces the window with a root-confined saved project. |
| `rgv_execute_batch` | Advanced ordered escape hatch, limited to 100 validated commands. |

`rgv_execute_batch` always rejects `exit`. It rejects `new_workspace` unless
`allow_destructive` is true. Focused tools should be preferred because their generated schemas,
annotations, defaults, and result interpretation are more precise.

Typed configuration reports `requested`, `effective`, `changed`, and `clamped`. Group, sort, and
filter reports additionally expose `no_op`; an empty filter expression is the explicit operation
for clearing an alignment filter. This makes an applied clamp or idempotent call distinguishable
from a mutation that changed the viewer.

Every argument in the generated tool schemas carries a description stating its unit and, for
genomic coordinates, whether it is zero-based or one-based and half-open or inclusive.

### Schema compatibility

`rgv-mcp` accepts a viewer whose `/v1/status` reports an automation schema version (and, when
present, a control schema version) inside the adapter's supported range. That range is
`MINIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION..=MAXIMUM_SUPPORTED_VIEWER_SCHEMA_VERSION` in
`apps/rgv-mcp/src/server.rs`; in this build both bounds are **6**, the current
`AUTOMATION_SCHEMA_VERSION`. Versions below 6 are not supported because schema 6 removed and
reshaped workspace-snapshot fields, and versions above the build's maximum are rejected because
their response shapes are unknown to it. `rgv_status` returns the range as
`supported_schema_versions`; a viewer outside it fails with `unsupported_version` and a message
naming both the reported version and the supported range. Generated batches are stamped with the
minimum supported version, which every supported viewer accepts.

The automation schema exposes the normalized presentation model and committed workspace
geometry structurally. The `workspace_layout` field reports one revision for the shared workspace,
adaptive genome-context rows, track viewport, gutter, and each panel's header, canvas, and scrollbar
rectangles together with cytoband availability. It also records the exact Properties rectangle and
expanded state, native display scale, accessibility/interface scale, layout-recomputation counter,
and settled resize-query counter. These are the same logical-point bounds used for native rendering,
interaction, accessibility, and visible-workspace export. Workspace snapshots also include ordered, channel-qualified
presentation nodes and groups; desired and effective
visibility; preferred heights; the shared immutable pane-layout rectangles; pane and stack scroll
offsets; every read panel's maximum/offset/effective density; the canonical pane selection; source
availability; and the target of any active resize or annotation/read scrollbar gesture. The typed
`presentation_node_visible`, `presentation_node_height`, `presentation_group_expanded`,
`presentation_group_visible`, `presentation_composite_visible`, `presentation_group`,
`presentation_ungroup`, `read_pane_height`,
`annotation_scroll_points`, `read_stack_scroll_points`, `read_panel_scroll_points`, and
`read_panel_density` settings all execute through the same reducer used by the GUI.
`annotation_scroll_points` addresses the workspace-wide annotation camera; its required
`panel_id` identifies the scrollbar/controller used for the request but does not create a
panel-local annotation offset. Workspace snapshots therefore report the same annotation offset
and maximum for every locus. Read-stack and read-panel cameras remain panel-local.
The qualification-only `window_size`, `interface_scale_percent`,
`properties_panel_expanded`, and `properties_panel_width` settings make shell reflow reproducible;
they affect the running window and do not persist calculated layout rectangles into a project.
The transient `properties_panel_resize_active` setting brackets held-sash qualification so a
mid-gesture capture cannot be mistaken for pointer release; setting it back to `false` schedules
the single final resolution-dependent refresh.

`rgv_capture_view` with `scope: "visible_workspace"` crops and rebases PNG output to the exact
committed workspace rectangle. `active_panel` uses the committed active canvas, while
`full_active_panel` requires the complete unscrolled content to be resident and fit the captured
canvas. If off-screen pages would be needed, it fails explicitly without writing a partial export
or moving the live camera. PNG and SVG project the captured committed alignment scene.
`rgv_capture_window` remains the complete native client area, including the application shell and
Properties panel.

Both capture tools accept `max_width` (pixels, 64 to 16384, default 1600). A capture wider than
that is downscaled before it is inlined, preserving the aspect ratio; a narrower capture is never
enlarged. The result's `width`/`height` describe the inlined PNG, `source_width`/`source_height`
the viewer's original capture, and `downscaled` whether they differ, so image pixels can be mapped
back to the reported panel geometry by the ratio `source_width / width`. Pass a larger `max_width`
when full resolution is needed. Automation `snapshot` PNGs are one pixel per logical point by default; the batch `snapshot`
command accepts an optional `scale` (PNG pixels per logical point, 0.25 to 8, default 1.0) for
sharper or smaller output, for example
`{"command":"snapshot","path":"view.png","scale":2.0}`. `scale` is ignored for SVG and must be 1
for the `native_window` scope, which is captured at the window's own pixel density. PNG snapshots
are rendered through the GPU when the device is available, so they match the screen, and fall back
to the CPU rasteriser otherwise. Exports started from the desktop menus use the display's pixel
density. The workspace state
associated with a capture is reported once, as
the top-level `panels`, `loci`, `genome`, and revision fields; the embedded command report does not
repeat the workspace snapshot.

Performance telemetry is omitted by default because it dominates the payload and carries no
genomic meaning. Two kinds exist. The command report's `telemetry` object (frame timings, GPU
counters, per-navigation timings, and environment) is returned by `rgv_capture_view`,
`rgv_capture_window`, and `rgv_execute_batch` only when called with `include_telemetry: true`;
the other tools never return it. The workspace snapshot's alignment-scene cache accounting and
diagnostic counters (every top-level `alignment_scene_*` field) are returned by
`rgv_get_workspace` only with `include_telemetry: true`.

`rgv_get_interaction_trace` is observational. Drag the vertical scrollbar in the live window, then
call the tool to inspect the first 500 ms of the gesture. The trace separates pointer receipt,
egui handling, display clipping, renderer preparation, CPU/GPU capacity growth, upload volume,
surface acquisition, queue submission, and presentation. It contains no genomic records or screen
coordinates.

## Bounds and privacy

- Control responses are capped at 80 MiB.
- Workspace snapshots contain at most 16 panels and 1,024 tracks; selection snapshots contain at
  most 128 fields, with explicit truncation flags.
- Captured PNG files are capped at 64 MiB on disk, downscaled to `max_width` before inlining, and
  removed on every outcome of the capture call, including failures.
- `rgv_capture_window` renders the complete application client area—including menus, navigation,
  tracks, status, and inspectors—to a private GPU target owned by the authenticated RGV process.
  It never activates the application and remains current when the OS window is obscured or
  minimized. The PNG intentionally excludes the operating-system title bar, border, and shadow.
- Text exports are inlined only through 256 KiB; larger results remain root-relative artifacts.
- Workspace and selection inspection omit source paths, credentials, signed URLs, read names,
  read sequences, and arbitrary alignment tags.
- Exact read names may be supplied as sensitive `rgv_find_alignments` input. They are matched only
  in the desktop process, redacted if a batch is serialized, never echoed in search or selection
  results, and replaced by random opaque handles that expire when the backing query changes.
- `rgv_export` is the one tool whose purpose is to write record-level evidence. The file it writes
  under the control root always contains the original read names (the `read_name` column of
  `alignments`, `filtered_alignments`, and `eligible_alignments`, and the `Read` row of
  `selected_evidence`). The copy inlined into the tool result as `inline_text` does **not**: by
  default each read name is replaced by an export-local pseudonym (`read_1`, `read_2`, ... in
  first-appearance order, so mates and supplementary segments of one template still share an
  identity) and selected-evidence read identity is replaced by `[redacted]`. `inline_read_names`
  reports `pseudonymized`, `included`, or `not_present`. Pass `include_read_names: true` to inline
  the original names. Selected-evidence exports still inline the read sequence, CIGAR, and
  auxiliary tags of the one selected record; that is their documented purpose.
- Tool reports recursively redact path-, URL-, and source-named fields. Viewer diagnostics
  (`message` fields and error messages) are passed through with absolute filesystem paths reduced
  to `[path]/<file name>`, URLs reduced to scheme and host, and bearer tokens and secret-named
  `key=value` pairs removed. The control secret is never logged or returned.
- Local reads and writes stay under `--control-root`. Absolute/traversing artifact arguments and
  symbolic-link escapes are rejected. Sources named inside a project opened through the control
  service are held to the same root; a project that names a file outside it is not opened and the
  command fails with `invalid_command`.
- Remote sources (`load_track`, `set_genome`, and URLs inside an opened project) must be `http` or
  `https` and may not target `localhost` or a loopback, link-local or private-network address
  literal. A batch that does is refused with HTTP 403 `source_refused` before it reaches the
  window. Start `rgv-desktop` with `--control-allow-private-urls` to permit private addresses
  (other schemes stay refused).
- stdout is reserved for MCP protocol frames. Diagnostics go to stderr.

No telemetry or genomic content is sent outside the local `rgv-mcp` and `rgv-desktop` processes.

## Errors and recovery

Tool failures contain structured `error.code` and `error.message` fields. `error.message` and each
`report.results[].message` carry the viewer's own diagnostic (for example
`RGV command load_track failed: track source [path]/sample.bam does not exist or is not a file`),
redacted as described under "Bounds and privacy" and bounded to 4 KiB. Stable adapter codes
include `unavailable`, `unauthorized`, `unsupported_version`, `invalid_input`, `invalid_batch`,
`forbidden_command`, `destructive_confirmation_required`, `invalid_response`,
`response_too_large`, `state_reobservation_failed`, and `artifact_error`. Viewer failures preserve the lower-case shared error
codes, including `invalid_setting_scope`, `setting_out_of_range`, `effective_state_unavailable`,
`no_selection`, `selection_stale`, `stale_handle`, `ambiguous_alignment_match`,
`related_alignment_unavailable`, `review_item_not_found`, `history_boundary`, `unsaved_changes`,
`workspace_replacement_not_confirmed`, and `workspace_revision_conflict`.
Selection, setting, history, review, relationship, and persistence failures additionally preserve
`no_selection`, `selection_stale`, `stale_handle`, `setting_out_of_range`,
`effective_state_unavailable`, `review_item_not_found`, `history_boundary`,
`related_alignment_unavailable`, `unsaved_changes`, and
`workspace_replacement_not_confirmed`.

- `unavailable`: start RGV with matching control options, then retry; the MCP server need not be
  restarted.
- `unauthorized`: verify that both processes read the same protected secret file.
- `unsupported_version`: the viewer's schema version is outside the adapter's supported range
  (see "Schema compatibility"); use `rgv-desktop` and `rgv-mcp` builds from compatible releases.
- `invalid_state` or `resource_not_found`: call `rgv_get_workspace`, correct the ID/resource, and
  retry.
- `timeout`: inspect workspace state before retrying. Cancellation or timeout does not roll back a
  mutation already accepted by the viewer.
- `state_reobservation_failed`: the adapter detected an interrupted mutation but could not inspect
  the resulting workspace; restore connectivity before retrying.
- `artifact_error`: verify root permissions and ensure no path component is a symbolic link.

Calls are mutually exclusive in the adapter and again in the viewer, but concurrently submitted
JSON-RPC requests have no guaranteed admission order. Await a response before issuing a dependent
operation, or put the dependent commands in `rgv_execute_batch`. Handle- and selection-derived
mutations accept an optional `expected_workspace_revision`; a mismatch fails as
`workspace_revision_conflict` without changing the viewer. After a cancelled call, observe the
workspace before issuing another mutation because the first operation may still have completed.

Alignment search results include `query_contig`, zero-based half-open `query_start`/`query_end`,
and the query generation. Ordinary hits overlap that declared scope. Mate and supplementary
records outside it are exposed only by the relationship tools. Modification hit summaries include
`present`, `absent`, `malformed`, or `unavailable`; a semantic modified-base search fails with
`effective_state_unavailable` rather than treating unloaded evidence as absent.

`rgv_select_alignment` accepts `reveal: none|center|fit` (default `none`). `center` preserves the
current span, while `fit` fits the selected record. Both return the resulting viewport and wait for
the refreshed query by default. Pathological CIGARs are summarized in structured selection fields;
`presentation_truncated` records that fact, and exact CIGAR text remains available through explicit
selected-evidence export.

## Native release qualification

Start the release viewer with the fixture explicitly named; do not rely on a project left open by a
previous run:

```sh
target/release/rgv-desktop \
  --session benchmark-data/manual-suite/projects/mf-01-alignment-core.rgv.json \
  --control-port 60151 \
  --control-secret-file /path/to/rgv-control.secret \
  --control-root /path/to/review-workspace
```

Then exercise the actual stdio protocol and rendered window with:

```sh
python3 validation/rgv_mcp_qualification.py \
  --mcp-binary target/release/rgv-mcp \
  --control-url http://127.0.0.1:60151 \
  --secret-file /path/to/rgv-control.secret \
  --control-root /path/to/review-workspace \
  --locus chr1:2590001-2650000 \
  --evidence-position 2600000
```

The harness performs MCP initialization and tool discovery, observes state, navigates, zooms,
pans, traverses the review queue, exercises typed settings, optionally finds/selects alignments,
proves selected evidence remains stable across viewport changes, captures background native macOS
window screenshots through `rgv_capture_window`, saves and opens the project in-process, and writes a path-free
structured report under the control root. Run it twice from clean launches and retain each uniquely
named screenshot. Visual correctness still comes from inspecting those images, not from structured
renderer metadata.

For the adaptive workspace-layout gate, run the focused harness against a fresh release process
whose control root is the repository root:

```sh
python3 validation/adaptive_layout_mcp_qualification.py \
  --mcp-binary target/release/rgv-mcp \
  --control-url http://127.0.0.1:60159 \
  --secret-file /tmp/rgv-adaptive-layout.secret \
  --control-root "$PWD"
```

It checks one- and two-panel shells; collapsed, minimum, default, maximum, and continuously resized
Properties states; mixed cytoband availability; a narrow 150%-scale window; retained per-panel
scroll/density/track-height state; committed export bounds; and resize-query coalescing. It writes
structured geometry plus native and logical PNG evidence under
`validation/adaptive-layout-evidence/`.
