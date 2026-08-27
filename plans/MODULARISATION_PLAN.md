# RGV modularisation plan

Date: 2026-08-23

## Purpose

RGV already has a sensible workspace-level architecture, but the desktop application and several
domain modules have accumulated too many responsibilities in individual files. This plan describes
how to reorganise the code without changing behaviour, destabilising session compatibility, or
regressing rendering and query performance.

The work should be incremental. Mechanical file movement, state-model changes, and behavioural
changes must be reviewed separately wherever practical.

## Current architecture

The existing crate dependency direction is broadly sound:

```text
rgv-core
   ↓
rgv-hts
   ↓
rgv-engine / rgv-layout
   ↓
rgv-render
   ↓
rgv-desktop
```

This direction should be preserved. In particular:

- `rgv-core` owns shared domain and persisted configuration types.
- `rgv-hts` owns genomic file and remote I/O.
- `rgv-engine` owns background scheduling, queries, events, and caches.
- `rgv-layout` owns CPU-side genomic layout and immutable display-list construction.
- `rgv-render` owns GPU upload and rendering.
- `rgv-desktop` is the application composition root.

The principal problem is not the number of crates. It is the concentration of unrelated
responsibilities within the desktop crate and some large domain modules.

## Audit baseline

At the time of this audit, the largest Rust files are:

| File | Total lines | Production lines | Assessment |
|---|---:|---:|---|
| [`apps/rgv-desktop/src/main.rs`](../apps/rgv-desktop/src/main.rs) | 28,486 | 24,411 | Critical mixed-responsibility monolith |
| [`crates/rgv-layout/src/alignment_layout.rs`](../crates/rgv-layout/src/alignment_layout.rs) | 6,695 | 3,958 | Large but substantially cohesive |
| [`crates/rgv-hts/src/tracks.rs`](../crates/rgv-hts/src/tracks.rs) | 6,003 | 4,640 | Multiple independent format implementations |
| [`validation/runner/src/bin/rgv-manual-fixtures.rs`](../validation/runner/src/bin/rgv-manual-fixtures.rs) | 4,204 | 4,204 | Generation, truth, cataloguing, and qualification mixed |
| [`crates/rgv-core/src/session.rs`](../crates/rgv-core/src/session.rs) | 3,172 | 1,766 | Models, validation, migration, persistence, and IGV import mixed |
| [`validation/runner/src/lib.rs`](../validation/runner/src/lib.rs) | 2,688 | 2,652 | Several independent verification domains mixed |
| [`crates/rgv-layout/src/track_layout.rs`](../crates/rgv-layout/src/track_layout.rs) | 2,615 | 1,896 | Independent track-family layout algorithms mixed |

`main.rs` accounts for approximately 34% of all Rust source and approximately 92% of the desktop
crate. Its production section contains 68 top-level types, 223 top-level functions, and 110 tests.

The workspace test suite passed at the audit baseline:

```text
cargo test --workspace --locked
```

One public-network-dependent TDF test remains ignored by design.

## Main findings

### 1. `Viewer` is both the state model and almost every application service

`Viewer` currently has:

- 141 fields;
- 191 methods; and
- an implementation spanning roughly 14,300 lines.

It owns GPU and egui resources, workspace state, panels, engines, sessions, alignment settings,
selection, dialogs, file tasks, autosave, automation, remote control, input gestures, diagnostics,
and rendering dirtiness.

This makes otherwise local changes difficult to understand because nearly every operation can
read or mutate the entire application.

### 2. A small number of methods contain whole subsystems

The most significant methods are:

- `render`, at approximately 5,196 lines;
- `rebuild_genomic_display`, at approximately 860 lines;
- application construction, at approximately 466 lines;
- inspector snapshot construction, at approximately 381 lines; and
- automation command execution, at approximately 303 lines.

`render` combines view-model preparation, menu composition, dozens of windows, command handling,
state mutation, file-dialog requests, display preparation, GPU submission, and repaint scheduling.

`rebuild_genomic_display` combines cache decisions, reference layout, alignment layout, auxiliary
track layout, quantitative composition, overlays, track frames, and multi-panel composition.

### 3. Primary alignment state is duplicated

`SessionFilters` has approximately 40 persisted settings. Most are also stored as individual
`Viewer` fields. They are copied into `Viewer` by `apply_primary_alignment_config`, reconstructed by
`live_session_filters`, and copied back into the first alignment controller by
`sync_primary_alignment_controller`.

The primary alignment's query, rows, cache statistics, and remote statistics are also represented
both in `PanelWorkspace` and in the primary `AlignmentTrackController` projections. Explicit
synchronisation is required to keep these representations aligned.

This duplication creates a high risk that new settings or state transitions update only one copy.

### 4. Active-panel access is implicit

`PanelWorkspace` implements `Deref` and `DerefMut` to its active `ViewPanelController`. Expressions
such as `panels.current_query` therefore look like workspace-wide access while actually operating
on one panel.

Explicit `active()` and `active_mut()` accessors would make panel scope visible at call sites and
reduce accidental single-panel assumptions.

### 5. The UI boundary is inconsistent

The existing track viewport UI provides a good pattern: it paints from supplied data and returns
typed `TrackUiAction` values. Much of the rest of the UI directly mutates `Viewer` or starts
application operations from inside egui closures.

This makes UI behaviour hard to test without constructing much of the application and creates
borrow-checking pressure whenever state is decomposed.

### 6. Source-format policy is repeated

Track/source classification is independently implemented in several places, including:

- CLI track construction;
- session restoration;
- dropped-file and remote-source handling;
- automation track opening;
- IGV/session kind inference; and
- engine source construction.

Each new format consequently requires coordinated changes across multiple large matches. Handling
of URLs, query strings, compressed inner extensions, typed BigBed formats, and supported remote
forms can diverge.

### 7. The tests are strong but concentrated

The repository has broad unit, golden-image, automation, validation, and benchmark coverage. This
is an important asset for the refactor. However, tests in the monolithic files are far from the
code they characterize and inherit the same broad module namespace.

Tests should move with their implementation modules. Cross-module behaviour should become focused
integration tests rather than remaining in a single catch-all test module.

## Architectural principles

The modularisation should follow these principles:

1. Preserve the existing crate dependency direction.
2. Prefer feature-oriented modules over `utils`, `helpers`, or `managers` modules.
3. Move code before redesigning it.
4. Keep persisted configuration and live domain state canonical in one place.
5. Let UI functions consume focused models and return typed actions.
6. Keep genomic algorithms in `rgv-core`, `rgv-hts`, and `rgv-layout`, not in desktop widgets.
7. Keep engine and filesystem operations outside UI presentation code.
8. Avoid adding crates solely to reduce line counts; modules are the default boundary.
9. Preserve public re-exports during internal file splits to avoid workspace-wide churn.
10. Treat line count as a warning signal, not as the primary design objective.

## Target desktop structure

The exact names can evolve during implementation, but the intended responsibility boundaries are:

```text
apps/rgv-desktop/src/
  main.rs                 # Parse/run only
  lib.rs                  # Public run() entry point
  launch.rs               # CLI, startup configuration, recovery marker
  runtime.rs              # winit ApplicationHandler and GPU lifecycle

  app/
    mod.rs                # Application state and command dispatch
    actions.rs            # Typed application actions
    tasks.rs              # Save/load/export/autosave worker supervision

  workspace/
    mod.rs
    panels.rs             # Panel identity, camera, navigation, and projections
    alignments.rs         # Alignment controllers and settings application
    auxiliary_tracks.rs   # Auxiliary track configuration and snapshots
    navigation.rs         # Locus parsing and navigation operations
    sessions.rs           # Live workspace ↔ SessionDocument conversion
    sources.rs            # Desktop source requests and TrackSpec construction

  display/
    mod.rs
    compositor.rs         # Per-panel and multi-panel composition
    alignment.rs          # Desktop alignment-layout orchestration and caches
    auxiliary.rs          # Auxiliary layout orchestration
    overlays.rs           # Regions, review items, and selection overlays
    hit_test.rs           # Geometry-to-domain selection mapping

  inspection/
    mod.rs
    alignments.rs
    variants.rs
    features.rs
    quantitative.rs
    rna.rs
    export.rs

  automation/
    mod.rs
    runner.rs
    commands.rs
    export.rs

  ui/
    mod.rs
    shell.rs
    menu.rs
    navigation.rs
    tracks.rs
    settings.rs
    dialogs/
      mod.rs
      track_settings.rs
      inspectors.rs
      metadata.rs
      regions.rs
      review.rs
      remote_source.rs
      recovery.rs
```

The top-level application state should eventually be composed from focused aggregates, for
example:

- `GpuRuntime`: window, surface, device, queue, egui integration, and GPU renderer;
- `WorkspaceState`: assembly, panels, alignments, auxiliary tracks, reference, and selections;
- `UiState`: open dialogs, editors, drafts, pointer/drag state, and search text;
- `TaskState`: save, load, export, autosave, automation, and retirement tasks.

These are responsibility boundaries, not a requirement to expose every field publicly. During the
first extraction phase, `Viewer` can remain defined in a parent module with implementation blocks
in child modules to preserve Rust privacy and minimise churn.

## Phased implementation

### Phase 0: Freeze the behavioural baseline

Before moving code:

- Record the current `cargo fmt`, Clippy, and workspace test results.
- Preserve existing golden hashes and checked-in automation output.
- Identify the automation-smoke and demo-corpus commands used for desktop qualification.
- Record the relevant interaction benchmark results.
- Avoid session schema changes during the modularisation work.

This phase should produce an architecture decision record or short contributor note explaining the
intended module dependency direction.

### Phase 1: Mechanical desktop extraction

Make `main.rs` a thin entry point while preserving behaviour and existing state representation.

Suggested sequence:

1. Add a private desktop library with `pub fn run() -> anyhow::Result<()>`.
2. Move `LaunchOptions`, startup loading, source arguments, recovery discovery, and `RunMarker` to
   `launch.rs`.
3. Move `Application` and its `ApplicationHandler` implementation to `runtime.rs`.
4. Move `PanelWorkspace`, `ViewPanelController`, and panel-local helpers to `workspace/panels.rs`.
5. Move alignment controller types and pure settings helpers to `workspace/alignments.rs`.
6. Move automation runner types and pure automation export functions to `automation/`.
7. Move inspector snapshot types, field formatting, bounded-copy helpers, and structured exports to
   `inspection/`.
8. Move tests with the code they exercise.

Constraints:

- Do not change session data or runtime behaviour.
- Do not replace moved code with `include!` indirection.
- Keep commits or PRs limited to one coherent movement where possible.
- Preserve public APIs and existing crate dependencies.

Expected result: `main.rs` becomes a small wrapper and no new abstraction is required merely to
complete the move.

### Phase 2: Establish canonical alignment and panel state

Remove the most error-prone duplication in small semantic changes:

1. Make each `AlignmentTrackController.config` the canonical live alignment configuration,
   including the primary alignment.
2. Introduce an `AlignmentSettingsDraft` for temporary UI edits and commit it through one update
   operation.
3. Remove mirrored primary alignment settings from the top-level application state.
4. Route the primary alignment through the same `AlignmentPanelProjection` representation as every
   secondary alignment.
5. Remove duplicated primary query, row-layout, and statistics state from panels once all callers
   use projections.
6. Replace `PanelWorkspace`'s `Deref` and `DerefMut` implementations with explicit `active()`,
   `active_mut()`, `get()`, and `get_mut()` access.
7. Split `ViewPanelController` into focused camera/navigation, data/projection, selection, and
   display state where that reduces coupling.

Each removal of duplicated state should have tests demonstrating that session save/restore,
primary-track promotion, multi-panel queries, and settings updates remain correct.

### Phase 3: Introduce a UI action boundary

Extend the approach already used by `TrackUiAction`.

UI functions should receive focused immutable view models plus explicit editor drafts and return
actions. A possible top-level vocabulary is:

```rust
enum AppAction {
    Navigate(NavigationAction),
    Tracks(TrackAction),
    Session(SessionAction),
    Inspector(InspectorAction),
    Automation(AutomationAction),
}
```

The application dispatcher, not the widget code, should:

- mutate workspace state;
- open native file dialogs;
- submit engine requests;
- start background workers;
- invalidate layouts; and
- request redraws.

Split `render` in the following order:

1. Application menu and command search.
2. Navigation and panel selection.
3. Track settings.
4. Metadata, regions, and review queue.
5. Inspector windows.
6. Remote-source, settings, project-issue, import, and recovery dialogs.
7. Central viewport chrome and status.

The final top-level render method should primarily:

1. build UI models;
2. show the shell and dialogs;
3. dispatch returned actions;
4. prepare GPU and egui resources;
5. submit the frame; and
6. schedule the next repaint.

A reasonable target is fewer than 250 lines for the top-level render method.

### Phase 4: Separate query and display orchestration

Extract the desktop-specific orchestration currently contained in query polling and
`rebuild_genomic_display`:

- alignment, reference, and auxiliary request scheduling;
- engine-event application and generation checks;
- layout-cache key construction;
- alignment display composition;
- auxiliary track composition;
- quantitative overlays and arithmetic dependencies;
- region and review overlays;
- hit testing and semantic selection; and
- multi-panel display composition.

Keep the boundary explicit:

- `rgv-layout` creates genomic geometry and display primitives.
- desktop `display` modules choose inputs, cache results, translate panel coordinates, and compose
  outputs.
- `rgv-render` remains unaware of genomic or application state.

Display builders should prefer focused input structures over accepting the entire application
state. Pure builders and hit-testing functions should have direct unit tests.

Run interaction and rendering benchmarks after this phase because structural changes to cache or
composition code can affect allocation, query timing, and frame latency.

### Phase 5: Centralise source-format classification

Introduce one typed classification API, likely in `rgv-core`, that can be shared by session import,
desktop startup, and engine specification construction.

It should own:

- URL-safe extension parsing;
- compressed inner-extension parsing;
- `TrackKind` and concrete format mapping;
- local, remote, and compressed support declarations;
- required companion indexes;
- default display labels; and
- explicit typed formats such as bigGenePred, bigNarrowPeak, and TDF channels.

Desktop code should convert a classified source into a user request or `TrackSpec`; it should not
maintain another independent extension table. Engine code should open a typed specification rather
than infer the format again.

Add table-driven tests covering every advertised format and source form. These tests should replace
the scattered inference tests only after equivalent coverage exists.

### Phase 6: Split the large library modules

Do this after desktop state and UI boundaries are stable.

#### `rgv-hts`

Split `tracks.rs` by data family:

```text
tracks/
  mod.rs
  shared.rs
  features/
    mod.rs
    text.rs
    bigbed.rs
    parse.rs
  variants.rs
  quantitative/
    mod.rs
    text.rs
    bigwig.rs
  gwas.rs
  copy_number.rs
  mutations.rs
  interactions.rs
```

Keep the current `rgv_hts` re-exports so downstream imports remain stable. Shared indexing or
bounded parsing utilities should only be extracted when used by multiple families.

#### `rgv-layout`

Split alignment layout by rendering layer:

```text
alignment/
  mod.rs
  options.rs
  rows.rs
  coverage.rs
  junctions.rs
  reads.rs
  cigar.rs
  modifications.rs
  technologies.rs
  reference.rs
  color.rs
```

Split `track_layout.rs` into feature, variant, quantitative, GWAS, interaction, copy-number, and
mutation modules. Keep shared display primitives in the existing display module.

#### `rgv-core`

Split `session.rs` by lifecycle responsibility:

```text
session/
  mod.rs
  model.rs
  validation.rs
  persistence.rs
  migration.rs
  igv.rs
```

Migration types should remain private. Public model names and serialized representations should
remain unchanged unless a separately reviewed schema migration is intended.

#### Validation runner

Split the manual fixture binary into:

- CLI and command dispatch;
- project definitions;
- alignment fixture generation;
- variant fixture generation;
- support/reference files;
- truth documents;
- scenario documents;
- server profiles;
- dependency/catalog generation; and
- qualification and listing.

Split the validation library into manifest/schema types and focused structural, alignment, variant,
workspace, automation, and relocation verifiers.

## Suggested review boundaries

The phases should be delivered as a series of reviewable changes rather than one repository-wide
rewrite. A useful order is:

1. Thin binary and launch/runtime extraction.
2. Panel and alignment controller extraction.
3. Automation and task extraction.
4. Inspector model/export extraction.
5. Explicit active-panel access.
6. Canonical primary alignment configuration.
7. Canonical alignment projections.
8. UI action vocabulary and one dialog family at a time.
9. Display/query orchestration.
10. Shared source classification.
11. `rgv-hts`, `rgv-layout`, `rgv-core`, and validation splits.

Mechanical extraction PRs should avoid formatting or renaming unrelated code. Semantic PRs should
avoid simultaneous large file movement.

## Validation strategy

Every phase must pass the standard checks:

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

The qualification workflow should be proportional to the change:

| Change | Additional validation |
|---|---|
| Mechanical module movement | Standard checks |
| Session/configuration state | Session round trips, fixtures, automation save/load |
| UI action or dialog extraction | Desktop unit tests and accessibility tests |
| Display composition or hit testing | Golden render hashes and selection tests |
| Query scheduling or caches | Synthetic corpus and interaction benchmarks |
| Remote source classification | Remote range tests and format matrix |
| Validation runner changes | Manual fixture generation and qualification |

Performance results should be compared against the checked-in interaction baselines. Refactoring
must not silently loosen existing latency, memory, upload, or record-bound limits.

## Completion criteria

The modularisation is complete when:

- `apps/rgv-desktop/src/main.rs` contains only startup and error reporting;
- the top-level render method is a short orchestration method;
- no desktop module exceeds roughly 1,500–2,000 lines without a documented cohesive reason;
- persisted alignment settings have one canonical live representation;
- primary and secondary alignment tracks use the same projection model;
- active-panel access is explicit;
- UI modules return actions instead of starting engines or mutating unrelated state;
- source-format inference has one authoritative implementation;
- engine, layout, and renderer dependency directions remain unchanged;
- session JSON remains backward compatible;
- tests live beside their implementation or in focused integration suites;
- golden and automation outputs remain stable where intended; and
- the standard CI, qualification, and performance checks pass.

The objective is not merely to produce smaller files. It is to make ownership, state transitions,
and dependency direction obvious enough that a developer can add a track format, inspector,
settings control, or engine operation without editing a 28,000-line composition root.

## Implementation record

Implemented on 2026-08-23 without changing the session schema or crate dependency graph.

- `main.rs` is 3 lines and delegates to the desktop library; launch, runtime, application tasks,
  workspace, display, inspection, automation, UI, and tests are feature modules.
- The `render` module is 17 lines. The remaining borrow-scoped egui transaction and its
  documented size exception are described in ADR 0005; diagnostics, typed dialog drafts, track
  action dispatch, and GPU submission are separate boundaries.
- Alignment configuration and projections are canonical, and active-panel access is explicit.
- Source classification and source policy live in `rgv-core` with table-driven local, URL,
  compression, index, remote-support, and label tests.
- HTS track families, alignment layout layers, IGV session import, manual fixture generation, and
  validation domains are split behind their existing public APIs.
- Desktop tests are focused into workspace, inspection, display, source, and automation suites.

The completed quality gates were:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify
cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --iterations 10 \
  --maximum-p95-ms 16.7 --maximum-first-useful-ms 250
```

The interaction result was 5.77 ms p95 total latency and 115.30 ms first-useful latency. Manual
fixture qualification verified 116 artifacts, 35 reused dependencies, 17 projects, 34 semantic
checks, and 59 complete coverage rows, including deterministic generation and tamper rejection.
