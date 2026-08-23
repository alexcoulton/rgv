# scenario-mf-01: Core alignments and navigation

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-01`
- **Feature group:** `MF-01`
- **Project:** `mf-01-alignment-core`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-01.json`

## Why this exists

Core alignments and navigation

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-01-alignment-core.rgv.json
```

## Expected truth

- Structural pair classes are visible and inspectable.
- Horizontal navigation does not move the read stack vertically.
- Reference and coverage stay fixed while the read stack scrolls.

## Manual actions

1. Confirm paired, structural, deep, long-read, supplementary, modified-base, and pathological-CIGAR review loci are present.
2. At each locus pan and zoom with the top navigator, then vertically scroll without moving reference or coverage tracks.
3. Switch grouping, sorting, pairing, color, density, and base-display settings and inspect named reads.
4. Use mate and supplementary-chain navigation, then return through locus history and the review queue.
5. Save, reopen, and export the selected read/evidence; confirm track identity and settings persist.

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
