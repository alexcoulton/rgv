# scenario-mf-05: PacBio SMRT kinetics

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-05`
- **Feature group:** `MF-05`
- **Project:** `mf-05-smrt-kinetics`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-05.json`

## Why this exists

PacBio SMRT kinetics

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-05-smrt-kinetics.rgv.json
```

## Expected truth

- The initial track is already in subread IPD mode.
- Kinetic values remain attached to bases through zoom and reverse orientation.

## Manual actions

1. Open each of the six subread/CCS IPD and pulse-width modes.
2. Inspect forward and reverse records and compare exact per-base values with truth-mf-05.
3. Check missing, wrong-length, wrong-type, and malformed evidence is reported locally rather than crashing the track.
4. Pan and zoom through thresholds and confirm kinetic colour scales do not reinterpret the same value.
5. Save/reopen all modes and export one selected kinetic series.

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
