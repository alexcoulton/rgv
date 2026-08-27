# scenario-mf-12: Workspaces

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-12`
- **Feature group:** `MF-12`
- **Project:** `mf-12-workspaces`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-12.json`

## Why this exists

Workspaces

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-12-workspaces.rgv.json
```

## Expected truth

- Four alignment tracks retain stable IDs across two panels.
- The intentionally missing source fails locally while healthy tracks remain usable.

## Manual actions

1. Open the four-track, eight-track, two-panel, and failure-isolation projects in turn.
2. Add, remove, reorder, hide, collapse, resize, and bulk-edit tracks while preserving stable identities.
3. Group by metadata and compare linked versus independent panel navigation and reference persistence.
4. Open failure isolation and confirm exactly one missing source does not blank the three healthy tracks.
5. Save/reopen, relocate the portable project bundle, and compare the exported track manifest.
6. In both the four- and eight-track projects, resize several read panels from the left gutter,
   group/hide/minimize/reorder tracks in each channel, and verify literal sibling heights and bottom
   slack do not renormalize after an eye change.
7. In the two-panel project, give each panel different annotation-stack, read-stack, and per-track
   read offsets/densities. Resize the shared workspace divider, then confirm each right-hand
   scrollbar controls only its owning panel and scales with that panel's bounds.
8. Save and reopen the two-panel project. Confirm the shared presentation tree and divider restore
   once, while every panel-local camera/density restores to its own value; removing a source is the
   only action that deletes its saved read-panel state.

## Common failure signatures

- Named truth changes, disappears, or moves after pan/zoom/save/reopen.
- A malformed record crashes the app or hides unrelated healthy evidence.

## Result

```text
commit:
platform/GPU/display scaling:
result: not run
notes:
screenshot or issue:
```
