# scenario-mf-11: Genome and review

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-11`
- **Feature group:** `MF-11`
- **Project:** `mf-11-genome-review`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-11.json`

## Why this exists

Genome and review

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-11-genome-review.rgv.json
```

## Expected truth

- Aliases, six translation frames, ROI entries, and review items restore.
- The portable genome bundle resolves its relative reference, cytobands, and annotation.

## Manual actions

1. Open the portable genome and confirm reference, lengths, cytobands, aliases, and default annotation.
2. Search coordinates, aliases, genes, and variants, including ambiguous and no-result cases.
3. Compare forward/reverse reference and all six translation frames at known start/stop/partial codons.
4. Import, edit, navigate, and export regions of interest and review items at their boundary coordinates.
5. Use locus history, multiple loci, whole-contig, and previous/next navigation, then save/reopen.

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
