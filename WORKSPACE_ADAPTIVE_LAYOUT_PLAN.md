# Adaptive workspace layout architecture plan

Status: planned

Plan date: 2026-08-27

## Purpose

This plan replaces the independently calculated, fixed-coordinate geometry used by the genome
context header and track viewport with one adaptive workspace layout. The objective is broader than
fixing the current text collision and split-panel boundary mismatch: navigation, tracks, scrollbars,
GPU clipping, pointer interaction, accessibility, automation, and exports must all consume the same
per-frame geometry.

The work should eliminate the recurring class of bugs in which one part of the interface moves,
resizes, or disappears without every other consumer applying the same transform.

## Current failures and architectural cause

The current two-locus view exposes three immediate defects:

1. The total contig length, such as `3,000,000 bp`, intersects the coordinate-ruler baseline.
2. The panel boundary in the genome-context header does not match the boundary in the data viewport.
3. Omitting an unavailable cytoband representation leaves more unused vertical space than necessary.

These are symptoms of the same underlying design:

- `GENOME_CONTEXT_HEIGHT` is fixed at 79 points.
- `apps/rgv-desktop/src/ui/navigation.rs` positions labels, bars, span text, ticks, and the ruler using
  independent constants rather than measured rows.
- In a single-panel view, the contig-length label is protected by the shared left gutter. In a
  multi-panel view there is no per-panel gutter, but the same label position is used while the ruler
  begins at the panel's left edge.
- `apps/rgv-desktop/src/ui/frame.rs` allocates the genome-context top panel before allocating the
  expanded or collapsed Properties panel. The context therefore sees a wider rectangle than the
  later central data viewport.
- The multi-panel context duplicates panel-width calculations rather than using
  `workspace_column_rect`, `workspace_genomic_rect`, and `workspace_scrollbar_rect` from
  `apps/rgv-desktop/src/ui/mod.rs`.
- `CanvasGeometry`, GPU scissoring, native pointer mapping, track chrome, and viewport resizing are
  downstream consumers of geometry assembled at different points in the frame lifecycle.

A local offset correction would leave the width divergence, fixed-height assumptions, stale-frame
risk, narrow-window failures, and future optional-row failures intact.

## Target behaviour

The following invariants define the finished system:

1. Every locus panel owns one horizontal workspace column, including its genomic area and its own
   vertical scrollbar reservation.
2. A panel's navigation area and data area have exactly the same left edge, right edge, and split
   boundaries.
3. The Properties panel is reserved before workspace columns are calculated, whether it is expanded,
   collapsed, or being resized.
4. The track gutter is represented once and shared by the navigation and track areas where
   appropriate.
5. Optional navigation rows affect layout explicitly. An absent cytoband source never produces a
   fabricated white ideogram.
6. All visible panel data canvases begin at the same vertical coordinate.
7. Text is measured, clipped, elided, or suppressed by priority; it is never allowed to wrap into or
   paint over a neighbouring row.
8. Rendering and interaction use the same immutable geometry snapshot for a displayed frame.
9. A presentation-only resize does not automatically become a data-source reload.
10. Calculated geometry is transient UI state and is not persisted in project sessions.

## Proposed architecture

### 1. One pure workspace layout stage

Introduce a pure layout calculation, provisionally named `WorkspaceLayout::compute`. It must perform
no painting, mutate no viewer state, and issue no queries.

Its input should contain only layout facts:

- the final rectangle remaining after menu, navigation toolbar, status bar, and Properties panel;
- panel count and stable panel identities;
- measured font and control metrics from the active egui style;
- requested track-gutter and scrollbar widths with their minimum constraints;
- per-panel contig metadata and cytoband availability; and
- display scale and accessibility text scale where they affect measured sizes.

Its output should be an immutable `WorkspaceLayoutSnapshot` containing:

- the complete workspace, genome-context, and track-viewport rectangles;
- the shared track-gutter rectangle;
- one `PanelLayout` per locus, containing its complete column, header, genomic data rectangle, and
  scrollbar rectangle;
- named navigation-row rectangles for every panel;
- the Properties boundary used for this layout; and
- a monotonically increasing layout revision for diagnostics and frame consistency.

The existing `CanvasGeometry` can initially wrap or project from this snapshot. It must not remain a
second authority once migration is complete.

### 2. Allocate the outer shell before panel geometry

The shell should retain a full-width application menu and navigation toolbar. Below the toolbar:

1. Allocate the expanded or collapsed Properties area.
2. Treat the remaining rectangle as the workspace.
3. Divide that workspace vertically into the adaptive genome context and the track viewport.
4. Divide both regions horizontally using the same panel-column results.

Whether the Properties UI visually begins beside the genome context or only beside the tracks is a
painting decision. Its width must still be reserved before either region calculates panel columns.

### 3. Use explicit adaptive genome-context rows

The genome-context layout should use measured rows in this order:

1. **Contig metadata row**
   - Contig name left-aligned.
   - Total contig length right-aligned.
   - At constrained widths, retain the contig name, elide or suppress the length first, and expose
     the complete values through accessibility text and a tooltip.
2. **Optional cytoband row**
   - Present only when at least one displayed panel has cytobands or an explicitly loading cytoband
     source.
   - Panels without cytobands paint no rectangle in this row.
3. **Coverage overview row**
   - Always present and backed by the existing overview interaction.
4. **Visible-span row**
   - Centred label for the displayed interval length.
5. **Coordinate-ruler row**
   - Baseline, tick marks, and collision-filtered coordinate labels.

Header height is the sum of the active measured rows and their declared gaps. There should be no
global fixed `GENOME_CONTEXT_HEIGHT` after migration.

### 4. Define stable optional-row policy

Cytoband availability must be represented as `Loading`, `Present`, or `Absent`, rather than inferred
only from whether an iterator currently returns a segment.

- If every visible panel is `Absent`, omit the row globally and reduce the genome-context height.
- If any panel is `Present`, retain one shared row so every panel's overview and ruler remain
  vertically aligned. Draw only the panels that have data.
- If a source is genuinely `Loading`, reserve the row without drawing a fake ideogram. Reflow once
  when the state settles; do not alternate between present and absent on intermediate frames.
- Navigating between contigs may produce one intentional layout change, but ordinary repaints must
  not move the track viewport.

### 5. Make text layout width-aware without free wrapping

Genome navigation requires column alignment, so browser-style free wrapping is inappropriate.
Instead:

- measure all candidate labels against their assigned rectangle;
- use a deterministic priority order for retention;
- elide metadata labels with a tooltip and complete accessibility name;
- reduce the number of ruler ticks until adjacent measured labels no longer intersect;
- clip every glyph to its row and panel; and
- keep the ruler line out of all text bounds rather than masking collisions with opaque backgrounds.

### 6. Commit geometry atomically per frame

The displayed frame must have one geometry revision. The same snapshot must drive:

- egui painting and widget rectangles;
- track and genome primitive layout;
- Metal viewport and scissor rectangles;
- physical/logical point conversion;
- pointer, wheel, drag, resize, and selection hit testing;
- per-panel vertical scrollbar placement;
- accessibility bounds;
- MCP structured snapshots and window captures; and
- image/SVG export cropping where it represents the visible workspace.

If a new layout is computed while the GPU still displays the previous render list, interaction must
remain attached to the displayed snapshot until the new render list and geometry are committed
together. This prevents one-frame click-through and boundary drift during Properties-panel dragging.

### 7. Separate geometry invalidation from query invalidation

A change in workspace width always requires layout and render invalidation, but it does not always
require new genomic data.

Introduce or preserve separate change signals for:

- shell/layout geometry;
- viewport interval;
- render resolution and packing;
- source query interval; and
- source lifecycle.

Properties-panel dragging should update visual geometry immediately. Expensive source requests
should occur only when the changed pixel width crosses an actual query-resolution or prefetch
requirement, and should be coalesced during a continuous resize gesture. The final release must not
issue a new alignment/reference/annotation query on every pointer-move event merely because a panel
became a few points narrower.

## Implementation phases

### Phase 0: Characterise the existing failures

- Add failing geometry tests for the current two-panel boundary mismatch with collapsed and expanded
  Properties states.
- Add a regression demonstrating the contig-length/ruler collision in a panel without a private
  gutter.
- Record MCP structured state and native screenshots of the current MF-12 two-locus session.
- Instrument layout revisions and query requests during a Properties-panel resize to establish the
  current behaviour.

**Exit criteria**

- Every reported defect has a deterministic non-screenshot assertion.
- Baseline evidence records logical dimensions as well as native-pixel captures.

### Phase 1: Introduce shared geometry without changing appearance

- Add `WorkspaceLayoutInput`, `WorkspaceLayoutSnapshot`, and `PanelLayout` as pure geometry types.
- Calculate panel columns once and project the existing fixed-height header and track rectangles from
  the snapshot.
- Replace the duplicated multi-panel width calculation in `ui/frame.rs`.
- Make existing `CanvasGeometry` a projection of the snapshot during migration.
- Add invariant assertions for tiling, scrollbar reservation, and header/data horizontal alignment.

**Exit criteria**

- Single- and multi-panel views retain their existing visual structure.
- Header and data panel boundaries match to within 0.5 logical point at every tested Properties
  width.
- There is one authoritative implementation of workspace-column division.

### Phase 2: Correct the outer shell and frame lifecycle

- Reserve the Properties width before calculating the genome context and track viewport.
- Ensure collapsed, expanded, and interactively resized Properties states all use the same path.
- Associate the computed snapshot with the display-list/layout generation consumed by the GPU.
- Route native cursor conversion, scissoring, and viewport-width synchronization through the committed
  snapshot.
- Add a debug diagnostic that reports the active layout revision and key rectangles.

**Exit criteria**

- No one-frame mismatch occurs while opening, closing, or dragging the Properties panel.
- Panel activation and pointer hit testing agree with the panel visibly under the pointer.
- Physical scissor bounds are exact at 1x and 2x scale factors.

### Phase 3: Replace fixed genome-context coordinates with adaptive rows

- Move contig name and total length into the measured metadata row.
- Implement the three-state cytoband policy and remove the fabricated fallback rectangle.
- Place overview, visible-span, and ruler content into named row rectangles.
- Replace fixed tick-label assumptions with measured collision filtering.
- Derive the total context height from active rows and remove `GENOME_CONTEXT_HEIGHT` from initial
  canvas calculations.
- Preserve keyboard, pointer, drag, zoom, and accessibility semantics of the coverage overview.

**Exit criteria**

- No metadata text intersects a ruler, bar, boundary, or neighbouring label.
- An all-absent workspace contains no cytoband row or unexplained replacement gap.
- Mixed present/absent panels remain vertically aligned and draw no fake bar.
- Changing font scale or panel count cannot cause text to escape its assigned panel.

### Phase 4: Migrate every geometry consumer

- Update track chrome, group chrome, annotations, read panels, and per-panel scrollbars to consume the
  shared snapshot.
- Update GPU display-list clipping and physical scissoring.
- Update all pointer mapping, resize handles, selection, panel activation, and wheel routing.
- Expose the layout revision and named rectangles in MCP automation state.
- Update visible-workspace exports to use the same committed bounds.
- Delete remaining ad hoc reconstruction helpers and obsolete fixed offsets.

**Exit criteria**

- A repository search finds no independent recreation of panel divisions or genome-context origin.
- Hidden or clipped geometry cannot retain an interactive hit target.
- MCP bounds, accessibility bounds, and rendered boundaries agree.

### Phase 5: Control resize cost and visual stability

- Split layout/render invalidation from source-query invalidation.
- Coalesce resolution-dependent queries during continuous shell resizing.
- Ensure cytoband availability cannot oscillate across repaint frames.
- Verify that selection, read scroll cameras, annotation scroll, density, and track sizes survive every
  shell reflow.
- Add performance counters for layout recomputation and resize-triggered query counts.

**Exit criteria**

- Properties-panel dragging remains visually immediate without a query storm.
- A pure shell resize does not reset panel navigation or vertical track state.
- Settled output is deterministic after repeated open/close and resize cycles.

## Regression matrix

Automated geometry tests must cover the cross-product of the important layout dimensions without
requiring every combination to become a golden image:

| Dimension | Required cases |
|---|---|
| Locus panels | 1, 2, 3, and the supported maximum |
| Properties | collapsed, minimum expanded, default, maximum, active drag |
| Cytobands | all absent, all present, mixed, loading to absent, loading to present |
| Window width | minimum supported, narrow, default, wide |
| Display scale | 1x and 2x physical conversion |
| Text scale | default and enlarged accessibility scale |
| Contig metadata | short names, long names, small and large contig lengths |
| Track layout | short annotation stack, overflowing annotation stack, several read panels |

The tests should assert:

- columns tile the workspace without gaps or overlaps;
- every header genomic rectangle equals its data genomic rectangle horizontally;
- every header scrollbar reservation equals its data scrollbar reservation;
- all panels share one track-viewport top;
- all row rectangles are ordered, bounded, and non-overlapping;
- text-placement bounds do not intersect the ruler baseline;
- physical rectangles are valid after scaling and rounding;
- the pointer resolves to the same panel represented by rendered geometry; and
- no presentation-only gesture produces unbounded query generations.

## Manual and MCP qualification

Use the existing MF-12 two-locus session as the primary regression because it combines split panels,
per-panel scrollbars, a collapsed Properties rail, absent cytobands, and dense annotation/read data.

Capture and inspect:

1. One panel with Properties collapsed and expanded.
2. Two panels with Properties collapsed, default-expanded, and maximum-width.
3. A continuous Properties-panel resize at its start, midpoint, and end.
4. Two panels with mixed cytoband availability.
5. A narrow window and enlarged text scale.
6. Standard- and Retina-scale native captures.

For each state, compare the MCP-reported rectangles with screenshot pixel boundaries. Exercise
overview clicking/dragging, ruler navigation, read and annotation scrollbars, panel activation, and
track resizing after each reflow.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Adaptive height makes the track viewport jump | Use a workspace-wide optional-row policy and stable `Loading/Present/Absent` transitions. |
| Header and GPU briefly use different frames | Commit geometry and display-list generations atomically. |
| Properties resizing triggers excessive I/O | Separate invalidation kinds and coalesce resolution-dependent requests. |
| Mixed cytobands misalign panels | Decide optional rows globally; paint availability locally. |
| Narrow panels reintroduce collisions | Measure labels, apply priority/elision, and collision-filter ruler ticks. |
| DPI rounding creates one-pixel seams | Derive all physical bounds from shared logical edges before rounding. |
| Existing track interaction regresses | Migrate hit testing and rendering together and retain pane-specific interaction tests. |
| New geometry becomes persisted accidentally | Keep snapshots transient and persist only user choices such as panel count and Properties width. |
| The migration creates two authorities | Make compatibility projections one-way and delete them after Phase 4. |

## Acceptance criteria

The work is complete only when all of the following are true:

- `3,000,000 bp` and equivalent metadata never intersect the coordinate ruler.
- Navigation and data boundaries match for every panel and every Properties-panel width.
- No white ideogram or cytoband row appears when all displayed contigs lack cytobands.
- Mixed cytoband availability preserves cross-panel vertical alignment.
- Opening, collapsing, and resizing Properties causes no transient boundary drift.
- Panel scrollbars remain aligned with and control only their corresponding data panels.
- Track selection, resizing, scrolling, and overview navigation remain correct after a layout change.
- Layout-only changes do not cause an uncontrolled series of data queries.
- Accessibility and MCP bounds describe the rectangles actually visible on screen.
- The full workspace test suite, formatting check, all-target check, and relevant render golden tests
  pass.
- MCP qualification at 1x and 2x produces reviewed captures with no gaps, overlaps, clipping errors,
  or one-frame stale geometry.

## Non-goals

This work does not change genomic coordinate semantics, read packing, coverage calculation, track
grouping behaviour, density controls, cytoband source contents, or Properties-panel feature content.
It also does not introduce a general CSS-like layout engine. It introduces a small domain-specific
workspace layout layer that preserves the strict cross-panel alignment required by a genome viewer.
