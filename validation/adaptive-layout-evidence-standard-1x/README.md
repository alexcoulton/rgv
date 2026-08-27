# Adaptive layout standard-scale qualification evidence

Reviewed: 2026-08-27

This directory is the full native 1× counterpart to `../adaptive-layout-evidence/`. macOS launched
the same release binary from a temporary app wrapper with `NSHighResolutionCapable` disabled, so
the application, Metal surface, egui input, scissoring, and capture all ran at one native pixel per
logical point. The images are not resized Retina captures.

`report.json` records a passed 11-state qualification with `display_scale` and
`native_capture_effective_scale` both equal to `1.0`. Its eleven interaction probes perform 20
reversible viewport pan/zoom sequences, verify 18 retained panel selections, adjust 18 annotation
scrollbars and 18 read-panel scrollbars, activate every panel, and resize the read track. The held
Properties resize produced one final resolution-dependent query refresh.

The following native client-area captures were visually reviewed:

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

The review found no header/data split drift, Properties-boundary gap, stale midpoint geometry,
scrollbar displacement, metadata/ruler collision, fabricated absent-panel ideogram, or cross-panel
vertical misalignment. The active selection stays bounded inside Properties through reflow, and
the narrow 150% text-scale capture keeps navigation labels inside their named rows.
