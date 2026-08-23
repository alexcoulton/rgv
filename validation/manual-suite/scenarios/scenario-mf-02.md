# scenario-mf-02: Alignment events and reference bases

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-02`
- **Feature group:** `MF-02`
- **Project:** `mf-02-alignment-events`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-02.json`

## Why this exists

Alignment events and reference bases

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-02-alignment-events.rgv.json
```

## Expected truth

- Small and large insertions/deletions remain anchored at their named positions.
- Reference and read letters remain legible and use stable event colours.
- I1000 denotes a 1,000-base insertion between reference bases.

## Manual actions

1. Inspect each named match, SNV, small-indel, 1,000-base insertion, and 2,000-base deletion record.
2. Zoom across base-letter, event-label, and aggregate thresholds without changing event identity or colour.
3. Select insertion and deletion markers and compare positions, CIGAR, sequence, qualities, and pileup with truth-mf-02.
4. Pan slightly while zoomed in and confirm reads do not jump away or restack vertically.
5. Export the selected evidence and reopen the project to confirm the exact locus and base-display settings.

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
