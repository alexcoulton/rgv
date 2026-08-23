# scenario-mf-15: Automation, export, and accessibility

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-15`
- **Feature group:** `MF-15`
- **Project:** `runtime-generated`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-15.json`

## Why this exists

Automation, export, and accessibility

## Launch

```bash
cargo run --release -p rgv-desktop -- --batch validation/automation-smoke.json --automation-report benchmark-results/manual-suite-automation.json
```

## Expected truth

- Automation reports each command with stable identity and status.
- Keyboard focus and accessible names expose every common workflow.

## Manual actions

1. Run every supplied automation batch and inspect stable command IDs, settled-state waits, results, timeouts, and errors.
2. Exercise locus, panel, track, selection, and settings operations through authenticated local control.
3. Generate screenshots and every claimed structured, vector, semantic, and tabular export and validate each output.
4. Complete the common workflow using only the keyboard; check focus order, shortcuts, names, high zoom, high DPI, and reduced motion.
5. Save/reopen and inspect logs/reports to confirm runtime credentials and sensitive query values are redacted.

## Common failure signatures

- An operation reports settled before the requested state is visible.
- Keyboard focus becomes lost, hidden, unnamed, or trapped.

## Result

```text
commit:
platform/GPU/display scaling:
result: not run
notes:
screenshot or issue:
```
