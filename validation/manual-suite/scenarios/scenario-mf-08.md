# scenario-mf-08: Tagged single-cell alignments

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-08`
- **Feature group:** `MF-08`
- **Project:** `mf-08-single-cell`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-08.json`

## Why this exists

Tagged single-cell alignments

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-08-single-cell.rgv.json
```

## Expected truth

- Tracks are grouped by corrected cell barcode at startup.
- CB/CR/UB/UR tags and linked sample metadata are visible in inspection.

## Manual actions

1. Group, sort, colour, and filter by RG, CB, CR, UB, and UR.
2. Confirm repeated and distinct molecules for CELL_A-1, CELL_B-1, and CELL_C-1 remain obvious.
3. Inspect and copy corrected/raw cell and UMI tags from named reads.
4. Check the missing-tag and numeric malformed-CB records remain local, explicit failures.
5. Apply bulk actions, save, reopen, and confirm tag expressions and metadata links persist.

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
