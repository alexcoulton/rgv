# ADR 0009: Headless session crate

Status: accepted in part, 21 November 2026. Stages 1, 2, 3 (except desktop consumption of
`PanelPresentation`) and 5 of the plan below are done; stage 4 is in progress separately.
Alignment query dispatch and settlement now run in the session against a narrow presenter
interface, and `rgv-snapshot` renders a settled session with no window and no GPU.

## Context

Review items ARCH-2 and ARCH-7 (and the product gap PROD-15, true headless rendering) found that
every piece of session-level state and logic lived on the desktop `Viewer`, next to the window,
the GPU device and the widget toolkit. Nothing could drive a browsing session without a window,
automation was a second command executor beside the UI, and a headless snapshot tool was
impossible.

## Decision

A crate `crates/rgv-session` owns the session: document state, panels and their cameras, the
action model and its undo history, and the engines that need no GPU. Its normal dependency graph
must not reach `winit`, `egui`, `wgpu` or `rfd`; `crates/rgv-session/tests/dependency_boundary.rs`
enforces this from the manifests and `Cargo.lock`.

The desktop is a shell around it. `Viewer` holds one `rgv_session::Session<ViewPanelController>`
in its `session` field. The type parameter is the host's panel type: the session works on the
device-free `PanelState` through the `Panel` trait, and the desktop's `ViewPanelController` wraps
a `PanelState` with display caches and GPU-facing state. A headless host uses `PanelState`.

```text
rgv-core, rgv-automation, rgv-engine, rgv-hts, rgv-layout, rgv-alignment-scene
└── rgv-session       session state, Action reducer, undo, engines, alignment dispatch and
    │                 settlement, lane layout, PresentationHost, PanelPresentation, HeadlessHost
    ├── rgv-desktop   window, GPU, egui; implements PresentationHost; GPU scene publication
    └── rgv-snapshot  headless CLI: HeadlessHost + CPU projectors (rgv-render) -> PNG/SVG
```

### What the session holds now

- **Document and lists:** `DocumentState` (unsaved-changes tracking), autosave marker, current
  session path, unresolved sources, migration notices, IGV import report and pending imported
  locus/feature search, regions of interest, review items, sample metadata.
- **Panels:** `PanelWorkspace<P>`, the navigation gesture awaiting commit, navigation contract
  revision and stale-result counter, workspace revision.
- **Tracks:** the `TrackCollection`, the `WorkspacePresentation` arrangement, suppressed built-in
  gene bundles, the track id allocator, and the track engine.
- **Alignment sources** (`alignment/`): `alignment_tracks: Vec<AlignmentTrackController>` with
  each controller's scene and coverage engines, filters and per-panel
  `AlignmentPanelProjection` (latest scene frame, navigation transaction, coverage request and
  presented mode, pressure and budget retries); `empty_alignment`; the shared scene
  `AllocationLedger` and its budget snapshot; the workspace alignment limit; and the scene and
  coverage reaper lists. `alignment::navigation` is the alignment navigation transaction,
  `alignment::coverage` the coverage-pyramid decisions and worker lifecycle,
  `alignment::settlement` the device-free settlement decisions (frame/refusal/pressure
  currency, `alignment_scene_frame_settlement`, the FASTA lane planner) and `alignment::track`
  the controller, projection, filter and scene-contract code.
- **Panel selections:** the seven auxiliary-lane selections (`selected_feature` …
  `selected_interaction`) are `PanelState` fields; `selection` holds their types and the
  re-resolution that keeps them on their record across lane refreshes.
- **Reference:** the reference engine, its source, revision, dictionary, remote index URL and
  open error; genome assembly, identity, contig aliases, cytoband path.
- **Shell services without a window:** settings, settings path and the coalescing settings
  writer, status line and busy state, notification log, undo history, engine retirement tasks.
- **Logic:** `Session::reduce` (navigation, panel management, region/review list edits),
  `apply_action` / `undo_action` / `redo_action` over the `ActionHost` trait (one
  implementation of recording and replaying edits for any host), engine retirement, settings
  save and failure reporting, status and notification reporting, presented track queries
  (`effective_auxiliary_presentation_configs`), receiving track-engine events and reporting
  skipped source lines once per track load (`receive_track_events`,
  `report_track_source_diagnostics`), and the reference lifecycle (`start_reference_engine`,
  `install_reference_engine`, `rerequest_panel_references`).
- **Logic moved in stage 2:** routing track-engine answers into panels
  (`Session::route_track_event`, which returns what the host still owes: a stale display, an
  error already logged, or an unrouted event); annotation track load
  (`add_track_source`, `reconcile_track_sources`, `expand_tdf_channels`, track id allocation);
  alignment controller lifecycle (`prepare_alignment_track_controllers[_for_workspace]`,
  `swap_alignment_track_controllers`, `retire_alignment_track_controller`, reaper polling);
  reference query dispatch (`request_reference_query`, `dispatch_reference_query`,
  `current_alignment_reference_requirements`, `install_reference_authority_for_panel`,
  `compatible_reference_contig_for_panel`); source open options (`sources`); and
  `Session::reduce` now finds the alignment dictionary itself from the panel's selected track.
  A coverage worker is started when its scene engine reports the source open, from that
  engine's dictionary and stamp, so a request the sidecar cache serves opens no file.

- **Stage 3 (this change):**
  - *Device-free lane layout* (`lane_layout`): `PanelLaneLayout` is one panel's committed
    layout in logical points — canvas column, gutter width, and per pane (`PaneLanes`: pane
    rectangle, scroll, content bottom) the lanes (`LaneLayout`: owner, channel, name, kind,
    row/header/content/clip rectangles, visibility, `Sublane`s with a semantic `SublaneKind`).
    The desktop converts its egui `WorkspacePaneLayout` exactly
    (`WorkspacePaneLayout::to_panel_lanes`, `Viewer::panel_lane_layout`). Lane demand
    (`alignment::lanes`: derived-lane captures, read clip,
    `Session::alignment_scene_lane_capture_for_panel_track`) reads only this type. The panel's
    alignment selections reach it through default methods on the `Panel` trait.
  - *Presenter interface* (`presentation_host::PresentationHost`): the host owns the session
    (`session()`/`session_mut()`, the `ActionHost` pattern) and provides exactly the effects
    settlement needs — `panel_lane_layout`, `prepare_alignment_scene_frame`,
    `clear_alignment_scene_lane`, `yield_speculation_for_source_pressure`,
    `observe_capacity_reclaim`, display rebuild / interaction sync / dirty / redraw, the shell
    work settlement triggers (`request_all_panel_queries`, coverage-engine start, imported
    locus, landmarks, active-panel context, settings persistence, remote IO stats),
    `promoted_read_template`, error/warning reports, and diagnostics (`diagnostics_recording`,
    `diagnostic_event`, `alignment_filter_diagnostic_state`, all defaulting to off). Admission
    results come back as session calls over device-free `AlignmentLaneAdmission`s
    (`alignment::admission`: commit, wait-for-GPU, resume, fail, budget-limited, pending
    mirrors, optional-work pause/resume). `RecordingHost` records calls for tests.
  - *Moved functions:* `dispatch_alignment_query`, `request_alignment_query`,
    `request_alignment_scene_camera_update` and `Session::queue_alignment_navigation_for_track`
    (`alignment::dispatch`), `poll_alignment_events` (`alignment::events`, with the frame
    normaliser `alignment_scene_frame_for_ui`) and `poll_reference_events`
    (`reference_events`) are generic over `PresentationHost`; the desktop methods delegate
    with `self` as host. Every host call sits where the desktop made the effect, so event
    ordering is unchanged. Two deliberate reorderings with no observable effect: a reference
    receive error is reported after the receive loop ends instead of just before it breaks, and
    in `QueryCompleted` the display is marked dirty after the status line is set (the two touch
    independent state). Scene owner and request-token clocks moved to the session
    (`AlignmentSceneIdentityClock`). Nothing was left behind.
  - *`PanelPresentation`* (`presentation`): per panel the camera, contig, lane layout, per
    alignment track the navigation phase and settled flag, the current scene as an
    `Arc<AlignmentSceneSnapshot>` (reads, row atlas and summaries), contract currency,
    completeness, refusal, overview and unavailable state and the presented coverage-pyramid
    mode; per annotation track its retained snapshot and interval; the reference slice
    `Arc`; pending flags; the status line. Built by `Session::panel_presentation`.
  - *Headless presenter* (`headless::HeadlessHost`): fixed lane layout on a canvas, CPU
    `AlignmentPanelSceneStore`s, admission of every complete lane, track-query dispatch, a
    `poll` that mirrors the desktop's frame loop, and `settle(timeout)` that fails naming what
    each panel still waits for. `apps/rgv-snapshot` builds on it (stage 5).

### What the desktop still owns

GPU publication (`prepare_alignment_scene_frame` and its residency, prefetch, speculation and
capacity-reclaim machinery, now reached through `PresentationHost`), the track tile pipeline,
display lists and the egui pane layout that `PanelLaneLayout` is converted from, landmarks,
export and session save/load tasks, automation and control service plumbing, track query
dispatch (`dispatch_track_query`, because it consults the tile pipeline's routing), and every
window, GPU and egui object. Actions the session cannot reduce yet are carried out by the
desktop inside `ActionHost::perform_action`, so they are still recorded in the one undo history.

The desktop renderers do not consume `PanelPresentation` yet: GPU publication still reads the
session's projections directly. Switching them is the remaining part of stage 3; the headless
renderer is its first consumer.

## Plan

1. Crate skeleton, pure state and the action model with re-exports; boundary test. Done.
2. Move engines, lifecycle and non-UI background work into the session in slices, each ending
   green. Done, including alignment dispatch and settlement (stage 3 work above).
3. A device-free lane layout, a narrow presenter interface, and a `PanelPresentation`
   published by the session. Done except that the desktop's GPU publication does not consume
   `PanelPresentation` yet.
4. The remaining `Action` domains (track load, alignment display settings, session
   open/save/new, preferences, selection, list imports) with automation-versus-UI
   document-equivalence tests, and undo for track load and display settings.
5. A headless `rgv-snapshot` binary and an end-to-end headless test that renders an image.
   Done: `apps/rgv-snapshot` (see `docs/USER_GUIDE.md`, "Headless snapshots") and
   `apps/rgv-snapshot/tests/headless_snapshot.rs`, which settles a session over
   `benchmark-data/mixed.bam` and small BED/VCF fixtures with no window or device, asserts on
   the presentation (committed complete scene, depth at two positions, feature and variant
   names, read rectangles in the reads pane) and runs the binary to write a PNG and an SVG.
   Rendering lives in the binary crate so `rgv-session` stays free of `wgpu`.

## Consequences

- Session state is reached as `self.session.<field>` from the desktop. New logic that needs no
  window belongs on `Session`, with the desktop method delegating.
- `PanelWorkspace` is generic and lives in the session, so desktop-only helpers over it are free
  functions (for example `workspace::panels::maximum_annotation_scroll_points`).
- Behaviour is unchanged by each move; tests moved with their code. Desktop modules that lost
  their contents re-export the session items (`pub(crate) use rgv_session::…::*`), so desktop
  paths still resolve.
- `crates/rgv-session/src` is a production root of the alignment-scene source inventory, so the
  forbidden-identifier checks still cover the moved alignment code.
