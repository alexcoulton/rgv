# scenario-mf-07: Ultima evidence

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-07`
- **Feature group:** `MF-07`
- **Project:** `mf-07-ultima`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-07.json`

## Why this exists

Ultima evidence

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-07-ultima.rgv.json
```

## Expected truth

- The initial track is already in directional Ultima mode.
- Valid forward/reverse tp, read-group mc, and t0 evidence plus three explicit error controls remain distinguishable.

## Manual actions

1. Inspect the four valid and three invalid named Ultima records against the matching reference.
2. Compare directional and minimum-evidence tracks for forward Q40/reverse Q0 tp, read-group mc Q0, and t0 Q35 calls.
3. Check missing, wrong-length, and wrong-type tp records are reported without hiding valid evidence.
4. Pan and zoom through base/event thresholds without changing the decoded event.
5. Save/reopen and export the same selected event from both quality modes.

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
