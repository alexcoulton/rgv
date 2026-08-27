# RGV MCP interface gap-remediation plan

## Status and scope

This plan follows the first agent-driven run of manual fixture scenario MF-01. The run proved that
the stdio MCP adapter can connect to a native Metal-rendered RGV window, navigate, change the
viewport, group and sort alignments, select pileup evidence, export, save, reconnect, and inspect a
reopened workspace. It also exposed gaps that prevent an agent from completing the scenario
reliably without reading fixture files or using the graphical interface directly.

This document plans changes to the shared automation protocol, desktop control executor, MCP tool
surface, qualification harness, and documentation. It does not propose replacing visual review
with structured assertions. Screenshots remain the authority for display behavior; structured MCP
state must identify exactly which window and state revision a screenshot represents.

## Goals

1. Make selected evidence deterministic across pan, zoom, capture, export, and save operations.
2. Let an agent discover and traverse review items and locus history without out-of-band project
   parsing.
3. Let an agent find, select, inspect, and navigate related alignment records without exposing read
   identifiers in tool results.
4. Make every configuration call valid by construction and return the effective applied value.
5. Cover pairing, colour, density, base display, and vertical scrolling through typed tools.
6. Support a root-confined, explicit save/reopen qualification workflow.
7. Make multiple RGV instances unambiguous to MCP hosts and screenshot harnesses.
8. Preserve the existing bounds, authentication, redaction, filesystem confinement, and stdio-only
   transport guarantees.

## Non-goals

- Do not expose source paths, credentials, signed URLs, read sequences, arbitrary SAM tags, or read
  names in observation results.
- Do not add mouse-coordinate or generic GUI automation to MCP.
- Do not make screenshot analysis a renderer-internal assertion.
- Do not silently change the meaning of zero-based automation coordinates or one-based display
  loci.
- Do not address every broad-review-locus visual design issue in this work. The MCP should explain
  when evidence is omitted by a detail threshold; separate UI work may improve the presentation.

## Findings to address

| Priority | Finding | Consequence |
| --- | --- | --- |
| P0 | A retained pileup selection was recomputed using the current viewport binning. After zoom and pan, its displayed/exported locus changed from `chr1:2599999-2600001` to `chr1:2599991-2600014`, eligible reads changed from 2 to 15, and collapsed mate bases changed from 0 to 100. | The same semantic selection can produce different evidence without an explicit refresh. |
| P0 | `rgv_get_selection` reports a point-shaped `start`/`end`, while its fields can describe a wider display bin. | Structured coordinates and human-readable evidence disagree. |
| P1 | There is no MCP clear-selection operation. Selecting the track did not dismiss the existing pileup inspector. | An agent cannot return the window to a known clean state. |
| P1 | The MCP cannot list or navigate review items, inspect navigation history, select a named alignment, or invoke mate/supplementary-chain navigation. | MF-01 actions require out-of-band fixture parsing or manual GUI interaction. |
| P1 | `rgv_configure` accepts any option key with any scope in its JSON Schema, although the executor has workspace-, track-, and panel-specific keys. | Calls that are schema-valid can fail as `invalid_command`. |
| P1 | Colour mode and read-base display mode are absent from the typed MCP settings surface. | MF-01's settings exercise cannot be completed. |
| P1 | Configuration reports only the requested key, not the effective state. A vertical scroll request reported success while the effective scroll remained `0.0`. | An agent cannot distinguish application, clamping, or a no-op. |
| P2 | Workspace observation omits pairing, colour, density, base-display, modification, coverage, and other effective alignment settings. | An agent cannot verify most configuration changes without saving and parsing a project. |
| P2 | Broad review intervals can intentionally omit or compress evidence, but MCP does not report lane omission, sampling, or the reason for a detail threshold. | An agent cannot tell whether evidence is absent, hidden at the current detail level, or still loading. |
| P2 | `rgv_status` does not identify the attached process/window instance. | AppleScript focus by process name is ambiguous when multiple RGV processes exist. |
| P2 | Reopening a saved project requires restarting RGV outside MCP. | A complete persistence exercise cannot be expressed through the tool contract. |

## Required invariants

These invariants are release blockers for the remediation:

1. A `selection_id` identifies one immutable evidence snapshot. Its interval, fields, provenance,
   and export bytes do not change after pan or zoom.
2. Evidence changes only after an explicit selection or refresh operation, which returns a new
   `selection_id` and `evidence_revision`.
3. `rgv_get_selection`, the visible inspector, `rgv_export(kind = selected_evidence)`, and capture
   metadata refer to the same selection ID and evidence revision.
4. The structured selection interval is the interval described by its fields. A separately named
   `anchor_position` may identify the clicked/selected zero-based position.
5. Every setting accepted by MCP can be applied to the scope represented by its schema. Invalid
   scope/key combinations cannot be generated from the published schema.
6. Every mutating response returns requested state, effective state, whether it was clamped or a
   no-op, and the resulting workspace revision.
7. Opaque record handles and selection IDs are bounded, unguessable, scoped to one application
   instance, and invalidated explicitly when their backing query is unavailable.
8. No identifier-bearing input is echoed into tool results, reports, diagnostics, or persisted
   automation reports.

## Target architecture

Keep the existing layered design:

```text
MCP host
  -> rgv-mcp typed stdio tools
  -> authenticated loopback control client
  -> shared rgv-core automation commands and snapshots
  -> rgv-desktop executor
  -> the same Viewer state and production renderer used by the GUI
```

Add two concepts at the shared automation boundary:

- **Workspace revision:** a monotonically increasing value changed by semantic workspace,
  viewport, selection, query, or configuration mutations. Every observation, capture, and mutation
  response includes it.
- **Immutable evidence selection:** a retained snapshot containing the selected interval,
  anchor/record identity, bounded public fields, evidence provenance, query generation, and an
  evidence revision. Rendering and export consume this retained snapshot rather than rebuilding it
  from the current viewport.

The MCP adapter remains an adapter rather than a second state store. Opaque alignment handles are
issued and resolved by the desktop process so reconnecting MCP clients cannot accidentally apply a
handle to another viewer instance.

## Proposed protocol and tool changes

### 1. Stable selection model

Extend the shared automation selection snapshot with:

- `selection_id`;
- `evidence_revision`;
- `workspace_revision_at_selection`;
- `anchor_position` when applicable;
- the exact zero-based, half-open `evidence_start` and `evidence_end` represented by the fields;
- `visible_in_active_view`;
- `stale` and a bounded, non-sensitive stale reason;
- query/provenance fields needed to explain bin width, filters, overlapping-mate policy, and
  sampling without exposing source data.

On selection, build and retain the inspector/evidence snapshot. Pan and zoom may update
`visible_in_active_view`, but must not change the evidence interval or fields. A filter, track, or
query mutation that invalidates the evidence marks it stale; it must not silently replace it.

Add typed MCP operations:

- `rgv_clear_selection`;
- `rgv_refresh_selection`, which explicitly computes a new snapshot and returns the old and new
  selection IDs/revisions;
- an optional `granularity` on pileup selection: `exact_base` (agent default) or `visible_bin`.

For `exact_base`, perform a bounded single-base evidence query independent of viewport binning. For
`visible_bin`, record and return the actual bin interval. Update the inspector title/body to show
whether it is an exact base or a captured visible bin.

`rgv_export(kind = selected_evidence)` must serialize the retained snapshot. It should return the
selection ID, evidence revision, byte count, and digest. If there is no selection or it is stale,
return a typed error unless the caller explicitly requests stale-snapshot export.

### 2. Alignment discovery and record selection

Add bounded shared commands and MCP tools:

- `rgv_find_alignments` with `panel_id`, `track_id`, a bounded selector, and `maximum_results`;
- `rgv_select_alignment` using an opaque record handle;
- `rgv_get_related_alignments` for mate and supplementary-chain summaries;
- `rgv_navigate_related_alignment` with relationship (`mate` or `supplementary_segment`), segment
  index where applicable, and destination (`current_panel` or `new_panel`).

Supported find selectors should include:

- exact read-name input;
- overlap with a zero-based position or half-open interval;
- primary/supplementary/secondary state;
- pair class;
- presence of mapped mate, supplementary chain, or modified-base evidence.

An exact read name is sensitive input. Match it only in the desktop process, never echo it, and
return an opaque handle plus bounded semantic facts: interval, flags summary, mapping quality,
pair class, mate availability, supplementary segment count, and modification-code count. Do not
return read names or sequences. Cap results and report truncation.

Record handles should be derived from an application-instance salt and record identity, or stored
in a bounded expiring map. They must fail with `stale_handle` after an incompatible query or app
restart rather than selecting a different record.

### 3. Review queue and navigation history

Add shared observation types and MCP tools:

- `rgv_get_review_queue`;
- `rgv_navigate_review_item` by stable review-item ID, with optional `next_pending` action;
- `rgv_get_navigation_history`;
- `rgv_navigate_history` with `back`, `forward`, and optional bounded index selection.

Review-item output should include stable ID, label, one-based locus, zero-based interval, status,
and whether it is current. Omit notes and assignment by default because they may contain personal or
clinical information. If later exposed, put them behind an explicit bounded opt-in and apply the
same redaction policy as other evidence fields.

Expose read-only history entries, cursor, `can_go_back`, and `can_go_forward`. Add a non-mutating
snapshot API to `NavigationHistory` rather than advancing its cursor while describing it.

Navigation responses return the exact resulting interval, review/history identity, workspace
revision, and settlement state.

### 4. Typed configuration by construction

Replace the MCP-facing generic `{scope, key, value}` combination with a discriminated typed change
union. Examples:

```json
{"setting":"view_as_pairs","value":false}
{"setting":"alignment_density","value":2.0}
{"setting":"alignment_color_mode","track_ids":[1],"value":"pair_orientation"}
{"setting":"read_base_display_mode","track_ids":[1],"value":"all"}
{"setting":"track_height","track_id":1,"value":420.0}
{"setting":"vertical_scroll_points","panel_id":1,"value":80.0}
```

Each variant must give JSON Schema the correct IDs, enum, numeric range, nullable behavior, and
scope. Prefer typed `AutomationSetting` variants in `rgv-core`; retain generic `SetOption` only for
control-schema compatibility and remove it from focused MCP tools.

Cover at least the MF-01 settings:

- grouping and group order;
- sorting and sort order;
- pair view and overlapping-mate policy;
- alignment colour mode, including typed tag mode;
- alignment density;
- read-base display mode;
- quality shading;
- modified-base display;
- reference, coverage, and read visibility;
- track height/collapse/visibility;
- panel vertical scroll.

Return:

```json
{
  "setting": "vertical_scroll_points",
  "requested": 80.0,
  "effective": 0.0,
  "changed": false,
  "clamped": true,
  "reason": "read stack fits in the available track height",
  "workspace_revision": 123
}
```

Where the effective value depends on layout, complete the command only after a rendered layout has
computed the clamp. Do not report the requested value as applied before then.

Extend `rgv_get_workspace` alignment-track snapshots with the complete bounded effective settings
needed to verify a change. Keep source information excluded.

### 5. Render and capture observability

Add bounded per-panel/per-track render-state metadata:

- detail level;
- which semantic lanes are configured, rendered, or omitted;
- omission reason such as `overview_detail_threshold`, `disabled`, `no_reference`, or `no_data`;
- queried and rendered alignment counts;
- sampling count and truncation;
- effective vertical scroll and maximum scroll;
- current generation and settlement state.

Include the application instance ID, workspace revision, selection ID/evidence revision, panel
intervals, and render generations in `rgv_capture_view` metadata. This lets a test report prove that
a screenshot corresponds to the structured state observed immediately before it.

When reads or modification lanes are hidden at Overview level, return that fact explicitly. The
agent can then make a reasoned zoom rather than treating a blank lane as missing evidence.

Do not use MCP metadata to declare visual correctness. Native screenshot inspection must still
verify legibility, clipping, lane positions, colour, and stale-frame behavior.

### 6. Save and reopen

Add `rgv_open_workspace` with:

- a control-root-relative project path;
- `confirm_replace: true` because it replaces visible workspace state;
- optional settlement timeout;
- an explicit policy for unsaved changes (`reject`, `save_then_open`, or `discard`), defaulting to
  `reject`.

The MCP adapter must apply its existing symlink-safe path confinement. Mark the tool destructive,
non-idempotent, and closed-world. Return the new application/workspace revision, project display
name, exact restored panels, and settlement result. Do not return the resolved absolute path.

Save should return a project digest and workspace revision. A qualification can then save, open the
same artifact, and compare effective settings and loci without restarting the process. Continue to
test adapter reconnection across a process restart separately.

### 7. Application and window identity

Extend non-sensitive status with:

- an unpredictable per-process `application_instance_id`;
- process ID on local desktop platforms;
- window count and active-window ordinal if RGV later supports multiple windows;
- renderer/backend, surface pixel dimensions, and scale factor once a frame exists.

Every mutating and observation response includes the application instance ID. The MCP adapter must
discard record handles and mutation uncertainty state when that ID changes.

Document the macOS screenshot pattern as one AppleScript invocation that focuses the PID reported
by MCP and then invokes `screencapture`. Never focus only by application name in qualification.

## Error model additions

Add stable errors with actionable structured details:

- `invalid_setting_scope`;
- `setting_out_of_range`;
- `effective_state_unavailable`;
- `no_selection`;
- `selection_stale`;
- `stale_handle`;
- `ambiguous_alignment_match`;
- `related_alignment_unavailable`;
- `review_item_not_found`;
- `history_boundary`;
- `unsaved_changes`;
- `workspace_replacement_not_confirmed`.

Do not collapse these into `invalidcommand`. MCP error codes should preserve the shared lower-case
automation code with underscores rather than converting it to a punctuation-free form.

## Implementation phases

### Phase 0 — Freeze regressions and version the contract

1. Add a deterministic regression reproducing the MF-01 pileup-selection/export change across
   viewport widths.
2. Capture the current MCP tool schemas as protocol fixtures.
3. Increment the control/automation schema for changed selection semantics while keeping existing
   v1 commands readable during a defined transition.
4. Add workspace revision and application instance ID primitives.

Exit criteria: the evidence-instability regression fails before the fix, all existing tests still
pass, and compatibility behavior is documented.

### Phase 1 — Fix evidence correctness

1. Introduce retained immutable evidence snapshots in the desktop viewer.
2. Make inspectors and selected-evidence exports consume the retained snapshot.
3. Add exact-base versus visible-bin selection semantics.
4. Add clear and explicit refresh operations.
5. Surface selection ID, evidence interval, provenance, visibility, and stale state through MCP.

Exit criteria: the same selection produces byte-identical structured fields and export after pan,
zoom, capture, and unrelated panel navigation; refresh creates a new revision explicitly.

### Phase 2 — Make configuration typed and observable

1. Add typed shared setting variants.
2. Replace the MCP configure schema with valid-by-construction variants.
3. Add missing colour and base-display settings.
4. Return requested/effective/clamped state after layout settlement.
5. Expand workspace snapshots with effective alignment settings.

Exit criteria: every generated schema example succeeds against the live viewer, every valid option
round-trips through observation/save/reopen, and invalid combinations fail schema validation before
reaching the viewer.

### Phase 3 — Add review and history workflows

1. Add bounded review-queue snapshots and navigation commands.
2. Add read-only history snapshots and back/forward commands.
3. Add privacy tests for notes, assignments, paths, and labels.

Exit criteria: an agent can discover all five MF-01 review items, navigate each by ID, and return
through history without reading the project JSON.

### Phase 4 — Add record selection and relationship navigation

1. Add bounded alignment search and opaque record handles.
2. Add exact read-name input matching with non-echo guarantees.
3. Add read selection and stable read evidence snapshots.
4. Add mate and supplementary-chain description/navigation.

Exit criteria: an agent can select each named MF-01 control, inspect its public semantic evidence,
open its mate or chain segment, and return through history without any record identifier appearing
in results or logs.

### Phase 5 — Complete persistence and render observability

1. Add explicit root-confined workspace open/replace.
2. Add render-lane, sampling, scroll-range, and omission metadata.
3. Add application PID/instance identity and enriched capture metadata.

Exit criteria: save/open is expressible through MCP; a screenshot can be correlated to one app
instance and workspace revision; overview-hidden evidence is distinguishable from missing data.

### Phase 6 — Native qualification and release integration

1. Extend `validation/rgv_mcp_qualification.py` with an MF-01 workflow.
2. On macOS, focus the MCP-reported PID and capture in the same AppleScript command at each visual
   checkpoint.
3. Preserve unique screenshots rather than overwriting a single checkpoint.
4. Record structured observations, tool results, image dimensions/digests, application instance ID,
   workspace revision, commit, platform, GPU, surface size, and scale factor in a path-free report.
5. Run save/open in-process and adapter reconnect after an application restart as separate tests.
6. Update `docs/MCP.md`, the ADR, manual fixture instructions, and release checklist.

Exit criteria: MF-01 can be completed by an agent using MCP for viewer manipulation and
PID-targeted screenshots for visual judgment, with unsupported steps reduced to zero.

## Test strategy

### Shared model tests

- Serde round-trips and validation for every new command, setting, snapshot, and error.
- Bounds for review items, history entries, alignment hits, related segments, fields, and notes.
- Compatibility tests for old automation batches.
- Redaction tests proving sensitive selector input is not persisted or returned.

### Desktop executor tests

- Pileup evidence is immutable across viewport generation changes.
- Exact-base selection is independent of display bin width.
- Visible-bin selection reports the actual bin interval.
- Clear selection clears every selection field and closes every inspector.
- Stale selections are marked rather than silently recomputed.
- Alignment handles cannot resolve after an incompatible generation or app restart.
- Mate and supplementary navigation reuse the same production methods as GUI buttons.
- History observation is non-mutating and bounded.
- Effective scroll/density/height values are reported after clamping.
- Save/open preserves panels, loci, track order, settings, and review items.

### MCP protocol tests

- Exact tool names, descriptions, annotations, schemas, ranges, defaults, and `oneOf` variants.
- Invalid configuration combinations fail JSON Schema validation.
- Tool responses include instance/workspace/evidence revisions.
- All new tools maintain response-size and artifact bounds.
- Read names, source paths, notes, assignments, and secrets do not appear in results or stderr.
- Reconnect invalidates handles and re-observes state before the next mutation.

### Native MF-01 acceptance sequence

1. Start one controlled RGV instance and initialize MCP.
2. Read the review queue through MCP and assert the five expected entries.
3. Navigate each review item; pan and zoom; capture and inspect screenshots.
4. At the deep locus, verify sampling metadata and healthy rendering.
5. At the long-read locus, use render metadata to zoom until reads/modifications are rendered.
6. Find and select named long-read controls without returning their names.
7. Select a pileup base, export it, pan/zoom, and prove the export digest is unchanged.
8. Refresh selection and prove a new evidence revision is explicit.
9. Change grouping, sorting, pairing, colour, density, and base-display settings; re-observe every
   effective value.
10. Scroll a deliberately overflowing read stack and verify screenshot lane anchors remain fixed;
    also test a fitting stack returns a clamped no-op.
11. Navigate a selected mate and supplementary chain, then return through history.
12. Save, open the saved project through MCP, and verify loci/settings/review items.
13. Export selected read/evidence and verify track identity, coordinates, selection ID, and digest.

## Files expected to change

- `crates/rgv-core/src/automation.rs`: commands, typed settings, snapshots, revisions, errors.
- `crates/rgv-core/src/navigation.rs`: read-only bounded history snapshot access.
- `crates/rgv-core/src/session.rs`: only if review targets or schema migration are required.
- `apps/rgv-desktop/src/automation/commands.rs`: command execution and expanded observations.
- `apps/rgv-desktop/src/inspection/`: retained evidence snapshots and relationship selection.
- `apps/rgv-desktop/src/workspace/`: history, selection invalidation, and open-workspace behavior.
- `apps/rgv-desktop/src/control_service.rs`: schema/version and instance identity.
- `apps/rgv-mcp/src/server.rs`: focused tools and valid-by-construction schemas.
- `apps/rgv-mcp/src/control_client.rs`: instance-change and revision handling.
- `validation/rgv_mcp_qualification.py`: complete MF-01 qualification and screenshot correlation.
- `docs/MCP.md`, architecture ADRs, manual fixture instructions, and release documentation.

## Completion gates

The remediation is complete only when all of the following pass:

- `cargo fmt --all -- --check`;
- strict workspace Clippy with all targets/features and warnings denied;
- full locked workspace tests;
- focused shared automation, desktop automation/inspection, MCP protocol, and packaging tests;
- release builds of `rgv-desktop` and `rgv-mcp`;
- native MF-01 qualification twice from clean launches;
- one save/open qualification within a running app;
- one MCP reconnect qualification across an app restart;
- manual review of every retained screenshot at the recorded scale factor;
- a privacy audit of tool schemas, successful results, failures, logs, reports, and artifacts.

No P0 or P1 finding may be waived as cosmetic. P2 render-discoverability work may be split into a
follow-up only if the MCP still reports unambiguous omission/sampling reasons and MF-01 can be
completed without out-of-band fixture parsing.
