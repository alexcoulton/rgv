# scenario-mf-03: RNA and Sashimi

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-03`
- **Feature group:** `MF-03`
- **Project:** `mf-03-rna-sashimi`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-03.json`

## Why this exists

RNA and Sashimi

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-03-rna-sashimi.rgv.json
```

## Expected truth

- Sample A and B show exact shared, sample-specific, alternative, and multi-exon junction counts.
- Annotated and unannotated junctions plus valid, inferred, and invalid strand evidence remain distinct.

## Manual actions

1. Compare RNA sample A and B at the shared and sample-specific junctions.
2. Confirm annotated A-specific and unannotated B-specific links plus alternative donor/acceptor and multi-exon reads.
3. Switch strand handling and inspect positive, negative, inferred-missing, and invalid XS evidence.
4. Compare fixed and per-track automatic Sashimi scaling while panning and zooming both panels.
5. Filter junction counts, select links, and export vector evidence without changing the exact support counts.

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
