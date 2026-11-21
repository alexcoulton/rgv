# ADR 0009: Headless session crate

Status: accepted in part, 21 November 2026. Stage 2 of the plan below is in progress.

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
rgv-core, rgv-automation, rgv-engine, rgv-hts, rgv-alignment-scene
└── rgv-session      session state, Action reducer, undo, reference/track engines
    └── rgv-desktop  window, GPU, egui, alignment sources and scene publication
```

### What the session holds now

- **Document and lists:** `DocumentState` (unsaved-changes tracking), autosave marker, current
  session path, unresolved sources, migration notices, IGV import report and pending imported
  locus/feature search, regions of interest, review items, sample metadata.
- **Panels:** `PanelWorkspace<P>`, the navigation gesture awaiting commit, navigation contract
  revision and stale-result counter, workspace revision.
- **Tracks:** the `TrackCollection`, the `WorkspacePresentation` arrangement, suppressed built-in
  gene bundles, the track id allocator, and the track engine.
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

### What the desktop still owns

Alignment sources (`AlignmentTrackController`), their scene and coverage engines, scene
dispatch and settlement, GPU publication, the track tile pipeline, display lists, landmarks,
export and session save/load tasks, automation and control service plumbing, and every
window, GPU and egui object. Actions the session cannot reduce yet (`ApplyTrackIntents`,
`RestorePresentation`, `RemoveTracks`, `RestoreTracks`) are carried out by the desktop inside
`ActionHost::perform_action`, so they are still recorded in the one undo history.

## Plan

1. Crate skeleton, pure state and the action model with re-exports; boundary test. Done.
2. Move engines, lifecycle and non-UI background work into the session in slices, each ending
   green: document/panels/tracks/reference state, track queries, track-engine event receipt
   and the reference lifecycle are done. Routing track events into panels, track load, the
   reference query dispatch (which reads alignment scene requirements), then alignment query
   dispatch and settlement (not GPU publication) remain.
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
- Behaviour is unchanged by each move; tests moved with their code.
