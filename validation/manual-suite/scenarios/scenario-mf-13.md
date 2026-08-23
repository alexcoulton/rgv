# scenario-mf-13: Sessions, IGV import, and recovery

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-13`
- **Feature group:** `MF-13`
- **Project:** `mf-13-recovery`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-13.json`

## Why this exists

Sessions, IGV import, and recovery

## Launch

```bash
cargo run --release -p rgv-desktop -- --session benchmark-data/manual-suite/projects/mf-13-recovery.rgv.json
```

## Expected truth

- Native schema versions migrate or reject exactly as declared.
- IGV import reports exact, approximated, and ignored items explicitly.

## Manual actions

1. Open native schema versions 1, 2, and 3, then confirm the future version is rejected explicitly.
2. Import the representative IGV XML and compare every imported, approximated, and ignored notice with the expected report.
3. Open missing-source, changed-source, partial-workspace, and invalid-setting cases and confirm failures remain isolated.
4. Materialize the controlled runtime state in a disposable settings directory and verify one autosave recovery choice appears.
5. Recover the autosave and also test continue-without-recovery; use Save project as… to preserve only the chosen result.

## Common failure signatures

- Compatibility is silently approximated or a future schema is accepted.
- Recovery overwrites the original project or offers an invalid autosave.

## Result

```text
commit:
platform/GPU/display scaling:
result: not run
notes:
screenshot or issue:
```
