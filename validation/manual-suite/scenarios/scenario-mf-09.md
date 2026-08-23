# scenario-mf-09: Cohort variants and genotypes

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-09`
- **Feature group:** `MF-09`
- **Project:** `mf-09-cohort-variants`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-09.json`

## Why this exists

Cohort variants and genotypes

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-09-cohort-variants.rgv.json
```

## Expected truth

- SNV, multi-allelic, insertion, deletion, inversion, symbolic deletion, and BND records are present.
- All 128 samples retain phased, unphased, missing, partial no-call, and DP fields.

## Manual actions

1. Compare the same eight records in bgzip/tabix VCF and BCF/CSI tracks.
2. Inspect SNV, MNV, multi-allelic, indel, inversion, symbolic deletion, and breakend spans.
3. Filter and scroll 128 samples; verify pedigree order and phased, unphased, haploid, polyploid, and no-call genotypes.
4. Inspect INFO, FILTER, GT, DP, AD, GQ, and PL fields and use next/previous variant navigation.
5. Export selected records/genotypes and confirm the two source formats remain semantically identical.

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
