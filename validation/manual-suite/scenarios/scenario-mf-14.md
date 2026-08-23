# scenario-mf-14: Remote sources and failure isolation

- **Status:** Generated and machine-verified; manual result not run
- **Scenario ID:** `scenario-mf-14`
- **Feature group:** `MF-14`
- **Project:** `runtime-generated`
- **Expected truth:** `benchmark-data/manual-suite/expected/truth-mf-14.json`

## Why this exists

Remote sources and failure isolation

## Launch

```bash
cargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy
```

## Expected truth

- Healthy range requests are bounded and semantically match local sources.
- Auth, outage, corruption, validator, and retry profiles fail or recover deterministically.

## Manual actions

1. Start the loopback server, materialize runtime projects, and compare remote sources with their local equivalents.
2. Exercise HEAD, ranges, adjacent/custom indexes, redirects, signed queries, validators, and bearer authorization.
3. Switch through fail-first, outage, slow, no-range, short-body, changed-ETag, corrupt, and invalid-content-type profiles.
4. Test missing/corrupt indexes, wrong CRAM reference, cancellation, retry, and recovery while healthy panels remain usable.
5. Stop the server and confirm runtime credentials were absent from projects, logs, reports, and workspace files.

## Common failure signatures

- A failed remote source blanks or destabilizes healthy panels.
- Credentials or signed query values appear in a saved project, report, or log.

## Result

```text
commit:
platform/GPU/display scaling:
result: not run
notes:
screenshot or issue:
```
