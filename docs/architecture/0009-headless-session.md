# ADR 0009: Headless session crate

Status: accepted in part, 21 November 2026. Stage 2 of the plan below is done except alignment
query dispatch and settlement, which stay in the desktop for the reasons given under "What the
desktop still owns".

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
└── rgv-session      session state, Action reducer, undo, reference/track/alignment engines
    └── rgv-desktop  window, GPU, egui, alignment dispatch, settlement and scene publication
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

### What the desktop still owns

GPU publication, the track tile pipeline, display lists and pane layout, landmarks, export
and session save/load tasks, automation and control service plumbing, and every window, GPU
and egui object. Actions the session cannot reduce yet (`ApplyTrackIntents`,
`RestorePresentation`, `RemoveTracks`, `RestoreTracks`) are carried out by the desktop inside
`ActionHost::perform_action`, so they are still recorded in the one undo history.

Three functions read the session's alignment state but stay in the desktop, because each
interleaves window or GPU work with the session-level work too tightly to split safely without
checking the running application:

- `dispatch_alignment_query` (`workspace/alignment_management/dispatch.rs`) takes each lane's
  read and derived-lane clips from the egui-typed pane layout
  (`alignment_scene_lane_capture_for_panel_track`), clears lanes of the panel scene stores that
  GPU publication shares, writes diagnostics through the desktop facade, allocates owners and
  request tokens from desktop counters, and ends by preparing the GPU frame.
- `poll_alignment_events` (`workspace/queries.rs`, about 1,000 lines) settles frames,
  refusals, pressure and budget events, and in the same arms yields GPU speculation for source
  pressure, observes cross-owner GPU capacity reclaim, writes diagnostics, rebuilds displays
  and prepares the GPU frame.
- `poll_reference_events` installs FASTA slices and settles reference dependencies, updates
  landmark resolution and the imported locus, then rebuilds displays and prepares the GPU
  frame.

Moving them needs a device-free lane layout (stage 3) and a narrow host interface for GPU
admission results, speculation and diagnostics.

## Plan

1. Crate skeleton, pure state and the action model with re-exports; boundary test. Done.
2. Move engines, lifecycle and non-UI background work into the session in slices, each ending
   green. Done: document/panels/tracks/reference state, track queries, track-engine event
   receipt and routing into panels, the reference lifecycle and query dispatch, annotation
   track load, alignment controllers and their lifecycle, the coverage and navigation state
   settlement reads, and the settlement decisions. Remaining: `dispatch_alignment_query`,
   `poll_alignment_events` and `poll_reference_events` (see above).
3. A device-free `PanelPresentation` published by the session; alignment GPU publication stays
   in the desktop and consumes it.
4. The remaining `Action` domains (track load, alignment display settings, session
   open/save/new, preferences, selection, list imports) with automation-versus-UI
   document-equivalence tests, and undo for track load and display settings.
5. A headless `rgv-snapshot` binary and an end-to-end headless test that renders an image.

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
