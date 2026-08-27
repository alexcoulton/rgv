# Adaptive layout qualification evidence

Reviewed: 2026-08-27

`report.json` is produced by `validation/adaptive_layout_mcp_qualification.py` from a release build.
The latest run passed all structural assertions and all eleven reversible interaction probes. The
probes verify retained selections in both MF-12 panels, perform reversible viewport pan/zoom,
annotation-scrollbar and read-panel-scrollbar changes, activate every panel, and resize a read
track. A held Properties resize produced one final resolution-dependent query refresh across its
start/midpoint/end sequence.

The following native client-area captures were visually reviewed at Retina scale (2 native pixels
per logical point):

- `mf12-default-expanded.png`
- `mf12-collapsed.png`
- `mf12-minimum-expanded.png`
- `mf12-maximum-expanded.png`
- `mf12-resize-start.png`
- `mf12-resize-midpoint.png`
- `mf12-resize-end.png`
- `mf12-narrow-enlarged-text.png`
- `mf12-one-panel-collapsed.png`
- `mf12-one-panel-expanded.png`
- `mixed-cytobands.png`

The review found no header/data split drift, Properties-boundary gap, stale midpoint text,
scrollbar displacement, row collision, fabricated absent-panel ideogram, or cross-panel vertical
misalignment. Selection content remains bounded inside Properties during reflow. The narrow 150%
interface-scale state keeps metadata in its named row and elides toolbar content rather than
allowing it to enter the workspace.

`mf12-default-logical-1x.png` and `mixed-cytobands-logical-1x.png` are deterministic offscreen
visible-workspace exports at one logical pixel per point. They are cropped and rebased to the
committed 940 × 714-point workspace. Exact standard- and Retina-scale physical rounding is also
covered by the automated 1×/2× scissor and layout matrix recorded by the report and Rust tests.

The matching full native 1× qualification is retained in
`../adaptive-layout-evidence-standard-1x/`. It uses macOS low-resolution app mode around the same
release binary, not an image downscale.
