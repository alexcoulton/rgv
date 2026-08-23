# scenario-mf-10: Mixed auxiliary tracks

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-10`
- **Feature group:** `MF-10`
- **Project:** `mf-10-mixed-tracks`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-10.json`

## Why this exists

Mixed auxiliary tracks

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-10-mixed-tracks.rgv.json
```

## Expected truth

- Every supported generated auxiliary source queries non-empty evidence.
- Both TDF channels open independently and retain channel provenance.

## Manual actions

1. Open every declared auxiliary track and inspect its named exact record.
2. Exercise search, labels, exon/strand structure, inspectors, and next/previous feature navigation.
3. Navigate both endpoints of BEDPE/interact records and compare SEG/CBS/MAF/GWAS behavior.
4. Compare independent/shared signal scales, overlay groups, arithmetic output, and both TDF channels.
5. Export SVG and tables, save/reopen, and confirm every typed format setting persists.

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
