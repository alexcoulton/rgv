# Vertical scrollbar causes track sections to overlap

Status: fixed; automated and MF-10 native-window qualification complete at 2x

Date observed: 25 August 2026

## Summary

Dragging the right-side vertical track scrollbar causes scrollable track and alignment-subtrack
labels in the left gutter to enter the screen area occupied by pinned reference and gene-model
sections. Multiple labels, eye controls, and section headers are then painted at the same vertical
position. The colliding rows change as the scrollbar thumb moves, so this is a scroll transform or
clip-space defect rather than a track-height configuration problem.

The genomic canvas remains substantially better separated than the gutter in the supplied evidence.
The most obvious and consistently reproducible failure is therefore the gutter/chrome layout. The
fix must nevertheless verify canvas geometry, pointer hit testing, resize handles, and drop targets,
because those consumers also translate track-frame coordinates.

## Evidence

The three consecutive screenshots were captured from the complete mixed-format manual fixture:

- `Screenshot 2026-08-25 at 10.55.19.png`
- `Screenshot 2026-08-25 at 10.55.23.png`
- `Screenshot 2026-08-25 at 10.55.24.png`

All three show:

- project: `RGV complete mixed-format manual fixture`;
- locus: `chrVariants:8501-16000`;
- scale: approximately `7.637 bp/px` at Reads detail;
- 31 tracks and 28 loaded alignments; and
- a different, nearby vertical-scrollbar thumb position.

Visible collisions include, depending on the thumb position:

- `Plain GTF transcripts` with `Features (gzip BED)`;
- `Features (gzip BED)` with `refFlat transcripts`;
- `refFlat transcripts` with `Narrow peaks`;
- `Narrow peaks` with `Variant and specialist alignment truth`;
- alignment `Coverage` with `Broad peaks`; and
- alignment `Sashimi` or `Modifications` with `PSL target alignments`.

The overlap migrates between those boundaries as the thumb is moved. This rules out a single bad
track height and demonstrates that the screen-space positions are being derived inconsistently.

## Reproduction

1. Launch RGV with
   `benchmark-data/manual-suite/projects/mf-10-mixed-tracks.rgv.json`.
2. Keep the default `chrVariants:8501-16000` locus and the full set of visible tracks.
3. Drag the right-side vertical scrollbar thumb through several nearby positions in the upper or
   middle part of the track stack.
4. Observe the left gutter while dragging and after release.

Actual result: scrollable track labels and alignment subtrack labels paint over pinned section rows,
sometimes placing two eye controls and two labels on one line.

Expected result: every visible section owns a non-overlapping screen interval. Pinned reference and
gene-model sections reserve the top portion of the viewport; scrollable frames are clipped at the
bottom of that pinned region. The gutter, genomic canvas, hit targets, and scrollbar must all agree
on the same translated and clipped bounds.

## Likely ownership boundary

The display-list path already applies the computed pinned/scrollable boundary:

- `apps/rgv-desktop/src/display/mod.rs` calculates `scroll_clip_top` and calls
  `write_scrolled_and_clipped_vertical`, `scroll_and_clip_vertical`, or their pinned equivalents.

The gutter path does not receive that boundary:

- `apps/rgv-desktop/src/ui/tracks.rs::paint_track_viewport_chrome` receives only the canvas geometry,
  frames, and vertical scroll amount;
- `frame_screen_bounds` translates ordinary frames against the viewport top rather than the active
  pinned `scroll_clip_top`; and
- `subtrack_screen_bounds` applies its own frozen-lane rules without a shared final clip interval.

That asymmetry is the leading hypothesis: canvas primitives are clipped below the pinned region,
while gutter labels and interactions are merely clipped to the whole viewport. It must be confirmed
with a failing regression test before changing behavior.

## Fix requirements

1. Define one screen-space transform and clip interval for each pinned frame, scrollable frame, and
   alignment subtrack.
2. Pass the active pinned/scrollable boundary to gutter painting and interaction geometry instead of
   reconstructing it independently.
3. Clip labels, eye controls, resize handles, selection backgrounds, and drag/drop targets to the
   same interval as their canvas frame.
4. Preserve the intended pinned reference, gene-model, alignment-header, and alignment-summary
   behavior; do not solve the overlap by unpinning them.
5. Preserve exact scrollbar position, density-camera behavior, and content height.
6. Ensure a clipped or hidden row cannot retain an invisible interactive hit target.

## Regression coverage

- Add unit tests for frame and subtrack screen bounds immediately above, across, and below the
  pinned boundary at zero, fractional, middle, and maximum scroll offsets.
- Add a mixed pinned/scrollable frame test that proves adjacent visible intervals never overlap.
- Verify gutter hit testing, eye controls, resizing, reordering, and drop targets use the same bounds.
- Add a desktop rendering regression using the MF-10 frame structure, including alignment
  `Coverage`, `Sashimi`, `Modifications`, and `Reads` subtracks.
- Capture top, middle, and bottom scrollbar positions through MCP at both 1x and 2x scale factors.

## Acceptance criteria

- No two visible gutter sections or subtracks overlap at any scrollbar position in MF-10.
- Pinned sections remain stable and scrollable sections disappear cleanly at their shared boundary.
- Every gutter boundary aligns with the corresponding genomic canvas boundary.
- Pointer selection and controls target only the row that is visibly under the pointer.
- Existing vertical-density, frozen-header, and scrollbar tests continue to pass.
- MCP captures at top, middle, and bottom show no transient overlap during drag or settled overlap
  after release.

## Non-goals

This report does not change the chromosome overview, the vertical scrollbar density preview, track
ordering, configured track heights, or directionality glyph semantics.

## Resolution

Implemented on 25 August 2026.

The defect had two related causes:

1. The genomic display list clipped ordinary frames at `scroll_clip_top`, but gutter painting only
   clipped them at the outer viewport. It also derived header and subtrack label anchors from the
   already-clipped rectangle, causing a partially hidden row to appear to stick to the clipping
   edge.
2. Every visible `GeneModel` track was classified as pinned context. In MF-10, seven gene-model
   tracks consumed the complete viewport. Correctly applying the clip would therefore prevent the
   overlap but leave no usable scrollable area.

The final fix replaces the mixed pinned/scrollable stack with two explicit panes:

- all reference, gene-model, coverage, Sashimi, modification, secondary-alignment, and auxiliary
  annotation layers occupy a fixed-height upper pane;
- the primary read stack occupies the lower pane;
- annotation layers retain their configured heights and collapse controls, while read density
  changes only read-row geometry;
- the right edge reserves one column, split vertically into a conventional upper annotation
  scrollbar and the lower read density/scroll navigator;
- each pane owns and persists its own scroll offset;
- genomic primitives, labels, eyes, separators, resize handles, selection, and semantic hit
  testing use the same pane-specific clip rectangle; and
- track headers and subtrack anchors remain in transformed content coordinates, so partially hidden
  rows are clipped instead of being relocated onto a pane boundary.

A follow-up MCP reproduction found that annotation frame separators and their resize hit regions
still used the outer window bottom after the two-pane split. At deep annotation scroll offsets this
painted a horizontal annotation boundary inside the read pane and left an invisible annotation
resize target there. Both now use the annotation pane bottom, and the split itself is painted as one
fixed divider independent of either stack's content.

A second interaction follow-up found that egui supplies resize movement as an incremental delta for
each rendered frame, while the track handler reapplied every increment to the drag's original
height. The boundary therefore snapped back toward its starting point and appeared to stutter.
Track resizing now accumulates each delta against the last accepted height. The annotation/read
split is also an explicit draggable divider, with a vertical-resize cursor, keyboard support, and
minimum heights for both panes; its hit region stays outside the annotation-track gutter handles.

## Qualification

- `cargo test -p rgv-desktop`: 183 tests passed after replacing obsolete single-stack fill tests
  with pane-aware clipping and hit-testing coverage.
- Added pure geometry regressions for crossing, fractional, middle, and deep offsets; adjacent
  interval non-overlap; pinned/split-frame screen-to-content mapping; and hidden alignment-lane hit
  rejection.
- Added session and automation coverage for the independent annotation scroll offset and geometry
  coverage proving both controls share one x-axis.
- Rebuilt the release desktop and exercised MF-10 through the MCP at scroll offsets `0`, `1954.5`,
  and `3909` out of `3909` points.
- Captured settled 2560 x 1600 native-window images at 2x:
  `.rgv-mcp-artifacts/scroll-fixed-top.png`,
  `.rgv-mcp-artifacts/scroll-fixed-middle.png`, and
  `.rgv-mcp-artifacts/scroll-fixed-bottom.png`.
- Repeated the original overlap qualification at alignment densities `0.35x`, `1x`, and `4x` in
  `.rgv-mcp-artifacts/density-fixed-min.png`,
  `.rgv-mcp-artifacts/density-fixed-default.png`, and
  `.rgv-mcp-artifacts/density-fixed-max.png` before the final two-pane refinement. In the final
  layout, annotations remain fixed-height in the upper pane and only lower-pane read rows respond to
  density.
- A settled 2560 x 1600 MCP native-window capture of the final release build confirms the upper
  annotation/lower read order and the vertically stacked scrollbar segments.

A separate 1x native-scale capture remains useful release-matrix coverage; scale-independent bounds
and existing 1x/2x scissor tests cover the transform mechanically in this environment.
