# Vertical scrollbar causes track sections to overlap

Status: documented; fix not started

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
