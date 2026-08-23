# scenario-mf-06: SBX evidence

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-06`
- **Feature group:** `MF-06`
- **Project:** `mf-06-sbx`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-06.json`

## Why this exists

SBX evidence

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-06-sbx.rgv.json
```

## Expected truth

- The initial track is already in SBX mode.
- Insertion categories and highlighted tails remain distinguishable.

## Manual actions

1. Inspect concordant, partial, full, reverse, and all-tail SBX records.
2. Compare Highlight, Hide, and Off simplex-tail tracks at the same reads.
3. Check left/right tails, insertion components, and quality boundaries against truth-mf-06.
4. Inspect missing or malformed evidence and confirm neighboring healthy reads remain visible.
5. Save/reopen and export a selected SBX read in every tail mode.

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
