# scenario-mf-04: Bisulfite contexts

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-04`
- **Feature group:** `MF-04`
- **Project:** `mf-04-bisulfite`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-04.json`

## Why this exists

Bisulfite contexts

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-04-bisulfite.rgv.json
```

## Expected truth

- The initial track is already in CG bisulfite mode.
- Converted and protected reads are distinguishable on both strands.

## Manual actions

1. Inspect all six preconfigured CG, CHG, CHH, HCG, GCH, and WCG tracks.
2. For every context confirm protected and converted observations against the matching reference.
3. Compare forward and reverse reads, then inspect the low-quality and missing-evidence controls.
4. Toggle pairing, grouping, and zoom thresholds without falling back to ordinary mismatch colouring.
5. Save and reopen; confirm every track retains its distinct bisulfite context.
6. In the Reads channel, select tracks B through F, group them, set mixed child-eye states, hide the
   group with its eye, and minimize it with its chevron. Hide and restore track A's read content;
   confirm the group header never disappears, moves to an unrelated edge, or changes either gate.
7. Scroll and change density independently in all five grouped read panels before minimizing them.
   Expand/show the group and confirm every offset, density, and preferred height is restored.
8. Repeat grouping and visibility in Annotation and confirm no Reads selection, eye, height, or
   camera changes.

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
