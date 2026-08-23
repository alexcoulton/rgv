# RGV GUI Redesign Plan

Status date: 21 August 2026

This plan records the GUI and information-display changes identified by comparing
`igv_layout_gui.png` with `rgv_layout_gui.png`. The goal is not to copy IGV pixel for pixel, but to
adopt the mature desktop-genome-browser structure that makes genomic context, tracks, navigation,
and read evidence immediately understandable.

## Implementation status

All four phases were implemented and audited on 21 August 2026.

| Phase | Result | Implementation evidence |
| --- | --- | --- |
| 1. GUI shell and track framework | Complete | Compact menu/navigation chrome, dynamic canvas geometry, fixed gutter, integrated scrollbar, status bar, and optional diagnostics |
| 2. Ruler and genome context | Complete | Adaptive one-based ruler, visible-span ideogram/fallback, assembly/contig/locus navigation, bounded history, and reference-row integration |
| 3. Alignment semantics | Complete | Outlined pale reads, distinct CIGAR/base/modification events, bounded labels, event-preserving hover/selection, and deterministic visual fixtures |
| 4. Contextual controls and polish | Complete | Track context/settings actions, visibility/collapse/resize/reorder/export, command search, tooltips, keyboard traversal, and a consistent light theme |

The final runtime comparison was exercised at 3840×2160 on a Retina display with the release Metal
renderer and the indexed local `benchmark-data/mixed.bam`. The supplied IGV and pre-redesign RGV
captures are also 3840×2160. The BAM used in the IGV capture (`demo.bam`) was not present in the
workspace, so an exact post-redesign same-locus/data capture could not be reproduced; geometry and
interaction behavior are additionally covered by standard- and Retina-scale tests.

Validation evidence is recorded in `docs/VALIDATION.md` and `benchmark-results/`. The required
locked workspace tests, strict Clippy gate, release build, regional interaction benchmark, and
deep-read interaction benchmark all pass.

## 1. Baseline assessment

The alignment renderer is functional and performant, but the surrounding interface still resembles
a diagnostic prototype. Navigation, file actions, styling controls, filters, status messages, and
performance counters compete for space at the top of the window. The genomic canvas lacks the
visual hierarchy that IGV obtains from an ideogram, coordinate ruler, fixed track-label gutter,
track borders, and a compact status bar.

The most important differences are:

| Area | IGV layout | Baseline RGV layout | Required direction |
| --- | --- | --- | --- |
| Navigation | Compact assembly, chromosome, locus, history, and zoom controls | Locus, file actions, settings, and metrics share one long row | Create a dedicated navigation toolbar |
| Commands | Grouped application menus | Most commands are permanent buttons | Move infrequent actions into menus and command search |
| Genome context | Chromosome ideogram, span indicator, coordinate ticks, and ruler | No ideogram or coordinate ruler | Add persistent genomic context above tracks |
| Track identity | Fixed left label gutter with named, bordered tracks | All tracks draw into one edge-to-edge canvas | Introduce a track viewport with a fixed label gutter |
| Track controls | Context menus and track-specific configuration | Alignment style and filters permanently occupy header rows | Move controls into contextual track settings |
| Status | Compact bottom bar | Cache, layout, GPU, and frame metrics occupy primary chrome | Add a bottom status bar and optional diagnostics overlay |
| Read styling | Pale fill, fine outline, distinct event symbols and labels | Solid grey reads form a dense visual mass | Add outlined reads and clearer event semantics |
| Scrolling | Scrollbar belongs to the track viewport | Scrollbar floats over rendered data | Integrate scrolling into the track viewport |
| Vertical spacing | Tracks begin immediately below the ruler | A large unused band appears before coverage | Compute canvas origin from actual chrome height |

## 2. Target window structure

The desktop window should be organized from top to bottom as follows:

1. **Application menu bar**
   - File
   - Genomes
   - View
   - Tracks
   - Regions
   - Tools
   - Help

2. **Navigation toolbar**
   - Assembly selector.
   - Contig selector.
   - Editable locus field and Go action.
   - Home, back, and forward navigation.
   - Zoom out, zoom slider, and zoom in.
   - Loading indicator that does not change the toolbar geometry.

3. **Genome context area**
   - Chromosome ideogram when cytoband data is available.
   - Current visible-span indicator.
   - Coordinate ruler with stable, human-readable tick spacing.
   - Reference sequence row at sufficiently detailed zoom.

4. **Track viewport**
   - Fixed 150–180 point label gutter on the left.
   - Genomic drawing canvas on the right.
   - Shared horizontal genomic coordinates across every track.
   - Vertical scrolling across complete tracks and deep alignment rows.
   - Track separators, selection state, resize handles, and context menus.

5. **Status bar**
   - Track count.
   - Current locus.
   - Loading, sampling, warning, or error state.
   - Selected-read summary when applicable.
   - Performance and cache metrics only when diagnostics are enabled.

## 3. Track model and presentation

Each displayed track should have a header model independent of its genomic primitives:

- Stable track identifier and display name.
- Track type and source.
- Visible, collapsed, and selected state.
- Configurable height with minimum and maximum constraints.
- Optional subtracks, such as coverage plus reads for an alignment source.
- Per-track context actions for visibility, height, colour, grouping, sorting, filtering, renaming,
  exporting, and removal.

The label gutter should remain fixed while genomic content pans or zooms. Labels should align with
their corresponding track bounds and truncate safely with a tooltip for the full name. A selected
track should receive a restrained highlight without changing its geometry.

Alignment data should be divided visually into:

- Alignment track label.
- Coverage subtrack with its own height and maximum-depth label.
- Downsampling indicator when sampling is active.
- Scrollable packed-read area.
- Optional reference row at base-level detail.

## 4. Alignment rendering changes

The renderer should move closer to IGV's information hierarchy while retaining RGV's GPU path:

- Use a near-white or very pale grey read fill with a fine neutral outline.
- Keep mapping-quality shading subtle in the default style.
- Reserve saturated colours for mismatches, insertions, clipping, modified bases, selections, and
  explicit colour modes.
- Draw reference skips and long deletions as thin connectors rather than filled bars.
- Label sufficiently large insertions, deletions, and skips with their event type or length when
  space permits.
- Distinguish paired-read connectors from deletions and splice junctions.
- Preserve a minimum visible width for subpixel alignments without turning isolated records into
  visually dominant columns.
- Make selected and hovered reads clear without obscuring bases or event markers.

These changes will require either bordered rectangle primitives or a second outline primitive for
reads. Text labels should use a bounded glyph/text layer so dense loci cannot create an unbounded
number of labels.

## 5. Interaction model

The redesigned viewport should support:

- Drag to pan genomic coordinates.
- Wheel or trackpad gesture to zoom around the pointer.
- Vertical wheel scrolling over track content.
- Shift+wheel as an additional vertical-scroll path on macOS.
- Click to select a track or read.
- Right-click or platform-equivalent context menu for track settings.
- Dragging track separators to resize tracks.
- Dragging track labels to reorder tracks.
- Back and forward navigation through locus history.
- Keyboard focus that clearly distinguishes locus editing from canvas navigation.

The genomic canvas, ruler, labels, scrollbar, and overlays must agree on one coordinate system and
one dynamically calculated content origin. Hard-coded top offsets should not determine layout.

## 6. Implementation phases

### Phase 1: GUI shell and track framework

- Replace the current button-heavy header with a menu bar and compact navigation toolbar.
- Calculate the genomic canvas rectangle from the actual UI panels instead of
  `CANVAS_TOP_POINTS`.
- Add the fixed track-label gutter and clipped genomic-content region.
- Represent coverage and alignments as bounded track sections.
- Add track backgrounds, separators, names, and selected state.
- Attach the vertical scrollbar to the track viewport.
- Add a compact bottom status bar.
- Move cache, layout, GPU, and frame counters behind an optional diagnostics view.

**Exit criteria**

- No unexplained white band appears between the toolbar and genomic content.
- Track names and bounds stay aligned while vertically scrolling.
- Panning and zooming affect only genomic content, not the label gutter.
- The viewport remains usable at standard and Retina scale factors.
- Existing local BAM navigation and interaction performance gates continue to pass.

### Phase 2: ruler and genome context

- Add a coordinate-ruler layout module with adaptive tick intervals and labels.
- Add a visible-span annotation.
- Add assembly and contig selectors backed by the loaded reference dictionary.
- Add locus history with home, back, and forward actions.
- Define a cytoband data model and render an ideogram when assembly data is available.
- Render a clear fallback chromosome bar when cytobands are unavailable.
- Integrate the reference sequence row with the ruler at base-level zoom.

**Exit criteria**

- Tick labels remain readable and do not overlap across supported zoom levels.
- The displayed locus and ruler use consistent one-based user-facing coordinates.
- Assembly, contig, locus field, history, and canvas navigation remain synchronized.
- Missing cytoband or reference data degrades cleanly without leaving empty reserved space.

### Phase 3: IGV-like alignment semantics

- Add outlined alignment bodies.
- Refine default read and coverage colours.
- Separate insertion, deletion, reference-skip, clipping, mate-connector, and mismatch styling.
- Add bounded event labels when sufficient pixel width is available.
- Improve hover and selection rendering.
- Add visual regression fixtures for short reads, long reads, spliced RNA reads, deep coverage, and
  supplementary alignments.

**Exit criteria**

- Long deletions and skips cannot be mistaken for aligned sequence.
- Insertions and other events remain visible without overwhelming the read body.
- Dense grey reads retain visible row boundaries.
- Visual regression images are deterministic across supported GPU backends within documented
  tolerances.

### Phase 4: contextual controls and polish

- Move colour, group, sort, filter, visibility, and height controls into track context menus or a
  track settings panel.
- Keep command search as an efficient alternative to menus.
- Add track reorder and resize gestures.
- Add tooltips, accessible names, focus indicators, and keyboard traversal.
- Make platform window appearance consistent with the light application theme where supported.
- Add user-configurable compact and comfortable density modes if testing demonstrates a need.

**Exit criteria**

- The default window contains no development-only controls.
- Every displaced toolbar action remains discoverable through a menu, context action, shortcut, or
  command search.
- Track configuration works without changing the genomic viewport unexpectedly.
- Keyboard-only navigation covers menus, locus entry, tracks, settings, and the read inspector.

## 7. Likely code changes

The work should be separated so GUI layout does not become more tightly coupled to BAM querying:

- `apps/rgv-desktop/src/main.rs`
  - Replace the current monolithic immediate-mode header.
  - Integrate top, bottom, left-gutter, and central viewport panels.
  - Route menu, toolbar, status, context-menu, history, and diagnostics state.
- `crates/rgv-layout`
  - Add ruler, ideogram, track-frame, and event-label layout outputs.
  - Extend alignment layout with outline and connector semantics.
- `crates/rgv-render`
  - Support bordered rectangles or efficient outline primitives.
  - Add a bounded text/glyph path if `egui` overlay text is not sufficient.
- `crates/rgv-core`
  - Add assembly/cytoband metadata and navigation-history types where they are independent of the
    desktop frontend.

New desktop UI modules should be introduced as the shell is decomposed, rather than continuing to
expand `main.rs`. Suggested responsibilities are `ui/menu.rs`, `ui/navigation.rs`, `ui/tracks.rs`,
`ui/status.rs`, and `ui/diagnostics.rs`.

## 8. Validation strategy

Validation should include:

- Unit tests for adaptive ruler ticks, locus history, track geometry, gutter clipping, and event
  label budgets.
- Golden or hash-based rendering tests for each alignment event class.
- UI-state tests for menu commands, track selection, resizing, reordering, and diagnostics
  visibility.
- Manual comparison against `igv_layout_gui.png` at the same locus and window size.
- Manual testing on standard-density and Retina displays.
- The full locked workspace test and strict Clippy gates.
- The local interaction benchmark at the existing 16.7 ms median and p95 thresholds.
- A deep-read scrolling test confirming that track labels, content, and scrollbar remain aligned.

## 9. Recommended execution order

Begin with Phase 1. It supplies the track geometry and dynamically calculated viewport required by
the ruler, ideogram, event labels, and contextual controls. Implementing visual read refinements
before this framework would improve the canvas but leave the principal GUI hierarchy unresolved.

After Phase 1, repeat the same-locus screenshot comparison before starting Phase 2. Record any
spacing or density adjustments in this document so subsequent rendering work targets the accepted
shell rather than a moving layout.
