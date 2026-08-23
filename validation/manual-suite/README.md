# RGV manual fixture suite

The suite is fixture-ready: its hashes, portable projects, exact semantic truth, production readers, relocation, coverage catalog, and controlled failure inputs pass mechanically. Manual scenario results remain `not run` until a tester records them; synthetic readiness does not replace real-data or platform qualification.

## Generate and verify

```bash
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- verify --require-complete
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list
```

`cargo run` builds changed binaries automatically; a separate `cargo build` is optional. Use release mode for visual testing. The expected CRAM MD5 diagnostic during verification is the deliberate wrong-reference rejection.

## Short pass

Run MF-01, MF-02, MF-03, MF-09, MF-10, MF-12, and one MF-14 outage/recovery profile. This covers ordinary alignments, base events, RNA, cohorts, formats, workspaces, and isolation without attempting every specialist mode.

## Remote lab

```bash
cargo build --release -p rgv-validation-runner --bins
target/release/rgv-fixture-server-check --suite benchmark-data/manual-suite
cargo run --release -p rgv-validation-runner --bin rgv-fixture-server -- --suite benchmark-data/manual-suite --profile healthy --write-endpoint /tmp/rgv-fixture-endpoint.json
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- materialize-remote --endpoint /tmp/rgv-fixture-endpoint.json --output /tmp/rgv-remote-projects
```

The endpoint document contains runtime-only credentials and must remain outside the repository.

## Full pass

- [scenario-mf-01: Core alignments and navigation](scenarios/scenario-mf-01.md)
- [scenario-mf-02: Alignment events and reference bases](scenarios/scenario-mf-02.md)
- [scenario-mf-03: RNA and Sashimi](scenarios/scenario-mf-03.md)
- [scenario-mf-04: Bisulfite contexts](scenarios/scenario-mf-04.md)
- [scenario-mf-05: PacBio SMRT kinetics](scenarios/scenario-mf-05.md)
- [scenario-mf-06: SBX evidence](scenarios/scenario-mf-06.md)
- [scenario-mf-07: Ultima evidence](scenarios/scenario-mf-07.md)
- [scenario-mf-08: Tagged single-cell alignments](scenarios/scenario-mf-08.md)
- [scenario-mf-09: Cohort variants and genotypes](scenarios/scenario-mf-09.md)
- [scenario-mf-10: Mixed auxiliary tracks](scenarios/scenario-mf-10.md)
- [scenario-mf-11: Genome and review](scenarios/scenario-mf-11.md)
- [scenario-mf-12: Workspaces](scenarios/scenario-mf-12.md)
- [scenario-mf-13: Sessions, IGV import, and recovery](scenarios/scenario-mf-13.md)
- [scenario-mf-14: Remote sources and failure isolation](scenarios/scenario-mf-14.md)
- [scenario-mf-15: Automation, export, and accessibility](scenarios/scenario-mf-15.md)

Synthetic truth proves deterministic mechanics; it does not replace the representative private-technology matrix in `../MANUAL_QUALIFICATION.md`.
