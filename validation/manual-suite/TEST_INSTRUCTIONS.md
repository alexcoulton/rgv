# RGV manual fixture test instructions

This guide explains how to run the checked-in RGV fixture suite and record a manual test pass. Run all commands from the repository root.

The suite contains known biological truth, preconfigured RGV projects, failure cases, scenario cards, remote-source simulations, and desktop automation batches. The fixtures prove deterministic behavior; they do not replace later testing with representative real Illumina, ONT, PacBio, RNA, cohort, and institutional remote data.

## 1. Prerequisites

- A working Rust toolchain compatible with the workspace.
- The RGV repository and its checked-in `benchmark-data` files.
- A desktop session with a supported GPU for visual and automation tests.
- Loopback networking for the remote-source tests.

`cargo run` builds any changed binary automatically. A separate `cargo build` is optional. Use `--release` for visual testing because debug builds do not represent normal application performance.

Record the commit before beginning:

```bash
git rev-parse HEAD
git status --short
```

Also record the operating system, CPU, memory, GPU, display resolution/scaling, and whether a mouse or trackpad is being used.

## 2. Verify the committed fixtures

Verify the suite without rewriting it:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixture-check -- \
  verify --require-complete
```

A successful run reports:

- 116 generated artifacts;
- 35 reused dependencies;
- 17 projects;
- 34 semantic checks;
- 59 coverage rows;
- 0 planned rows.

The CRAM checksum error printed during this command is expected. It proves that the deliberately wrong reference is rejected. Any final `Error:` or non-zero exit status is not expected.

For a deeper maintainer check, run the disposable determinism and tamper qualification:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixtures -- qualify
```

Use `generate` only after changing fixture-generator code or when intentionally restoring the generated suite:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixtures -- generate
```

`generate` rewrites the checked-in fixture artifacts. Ordinary manual testing should start with `verify`, not `generate`.

## 3. List and launch the projects

Print every project, its initial locus, and an exact launch command:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixtures -- list
```

Launch a project by copying its command. For example:

```bash
cargo run --release -p rgv-desktop -- \
  --session benchmark-data/manual-suite/projects/mf-01-alignment-core.rgv.json
```

Keep the corresponding scenario card open while testing. The cards are under `validation/manual-suite/scenarios/` and contain the expected truth, actions, and common failure signatures.

For MF-01, use:

```text
validation/manual-suite/scenarios/scenario-mf-01.md
```

Follow the same naming pattern through `scenario-mf-15.md`.

Do not edit the generated scenario cards to record results. They are checksum-protected fixture artifacts, and editing them makes strict verification fail. Copy the result block into an issue, test report, or a separate results document instead.

## 4. Recommended short pass

Run these scenarios first:

| Scenario | Main coverage |
| --- | --- |
| MF-01 | Short, paired, deep, long, supplementary, modified-base, and structural alignments |
| MF-02 | Reference bases, mismatches, every supported CIGAR event, and small/large indels |
| MF-03 | RNA reads, junction truth, strand handling, and Sashimi display |
| MF-09 | Cohort variants, genotypes, phasing, malformed boundaries, VCF, and BCF |
| MF-10 | Mixed annotation, quantitative, interaction, copy-number, mutation, and binary tracks |
| MF-12 | Multiple tracks, multiple panels, persistence, scrolling, and source-failure isolation |
| MF-14 | One remote outage/recovery case in addition to the local CRAM project |

For each scenario:

1. Open its scenario card.
2. Launch the project shown by `rgv-manual-fixtures list`.
3. Confirm the initial locus and named truth are visible.
4. Perform every numbered action in the card.
5. Compare the result with the card's expected truth.
6. Record pass, fail, or blocked, plus the exact commit and platform.
7. Capture a screenshot and logs for any failure before moving to the next scenario.

Do not stop the whole pass for a cosmetic defect. Stop and preserve evidence if a problem could cause incorrect interpretation, a crash, data loss, stale evidence, credential exposure, or unbounded resource use.

For an agent-driven MF-01 pass, use MCP for every viewer mutation and structured observation.
Obtain the PID from `rgv_status`; on macOS focus that exact process and call `screencapture` in the
same AppleScript invocation. Preserve a unique image at every checkpoint. Record the application
instance ID, workspace revision, selection/evidence revision, render generations, scale factor,
and image digest beside each screenshot. Do not focus by application name when more than one RGV
process may exist.

## 5. Full biological and workflow pass

After the short pass, run all scenario cards in order:

| Scenario | Area |
| --- | --- |
| MF-01 | Core alignments and navigation |
| MF-02 | Alignment events and reference bases |
| MF-03 | RNA and Sashimi |
| MF-04 | Bisulfite contexts and paired orientation |
| MF-05 | PacBio SMRT kinetics |
| MF-06 | SBX insertions, tails, and quality boundaries |
| MF-07 | Ultima flow-indel evidence |
| MF-08 | Tagged single-cell alignments |
| MF-09 | Cohort variants and genotypes |
| MF-10 | Mixed auxiliary tracks |
| MF-11 | Genome bundles, search, translation, ROI, and review queue |
| MF-12 | Multi-track and multi-panel workspaces |
| MF-13 | Sessions, IGV import, relocation, autosave, and recovery |
| MF-14 | Remote sources and failure isolation |
| MF-15 | Automation, export, and accessibility |

Some feature groups have more than one project. In particular, MF-12 includes four-track, eight-track, multi-panel, and failure-isolation projects. Run every project printed by the `list` command during a full pass.

## 6. What to check in every visual scenario

Regardless of the specialist feature, confirm the following:

- The reference, ruler, coverage, and other independent tracks remain fixed while the read stack scrolls vertically.
- Horizontal navigation never changes the vertical read position.
- Zooming while holding the overview control does not show stale data, make reads disappear until release, or cause the viewport to jump.
- Colours, base letters, event labels, and read identity remain stable across zoom thresholds.
- A malformed record is reported locally and does not hide or destabilize healthy evidence.
- Grouping, sorting, density, pairing, and display settings affect only the intended track.
- Save/reopen preserves project loci, panels, track order, visibility, and settings.
- Inspectors and exports report the same coordinates and evidence shown on screen.
- Text remains readable at the display's normal scale and under high-DPI or UI scaling.

## 7. Run the remote fixture lab

First run the automated protocol qualification. It covers bounded ranges, authentication, redirects, retry, validators, corruption, truncation, outage, slow responses, recovery, BCF/index access, and clean shutdown:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-fixture-server-check -- \
  --suite benchmark-data/manual-suite
```

For an interactive healthy remote session, use two terminals.

In terminal 1:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-fixture-server -- \
  --suite benchmark-data/manual-suite \
  --profile healthy \
  --write-endpoint /tmp/rgv-fixture-endpoint.json
```

Leave that process running. In terminal 2:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixtures -- \
  materialize-remote \
  --endpoint /tmp/rgv-fixture-endpoint.json \
  --output /tmp/rgv-remote-projects
```

The materializer prints commands for seven runtime projects. Start with:

```bash
cargo run --release -p rgv-desktop -- \
  --session /tmp/rgv-remote-projects/mf-14-remote-bam.rgv.json

cargo run --release -p rgv-desktop -- \
  --session /tmp/rgv-remote-projects/mf-14-remote-cram.rgv.json

cargo run --release -p rgv-desktop -- \
  --session /tmp/rgv-remote-projects/mf-14-remote-indexed.rgv.json
```

Then test the four expected failure projects in the same directory: missing index, corrupt index, wrong CRAM reference, and mixed healthy/broken-source isolation.

Stop the server with `Ctrl+C` when finished. Remove the endpoint document and runtime projects because the endpoint document contains runtime-only credentials:

```bash
rm -f /tmp/rgv-fixture-endpoint.json
rm -rf /tmp/rgv-remote-projects
```

To inspect a particular server behavior manually, restart the server and rematerialize the projects with one of these profiles:

```text
auth-required
redirect
fail-first-n
outage
slow
no-range
short-body
changed-etag
corrupt
invalid-content-type
```

For example:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-fixture-server -- \
  --suite benchmark-data/manual-suite \
  --profile slow \
  --slow-ms 1000 \
  --write-endpoint /tmp/rgv-fixture-endpoint.json
```

For `fail-first-n`, set the number of initial failures with `--fail-first`.

## 8. Run the desktop automation fixtures

The successful alignment/export batch is:

```bash
cargo run --release -p rgv-desktop -- \
  --batch validation/manual-suite/batches/mf15-complete-exports.json \
  --automation-report /tmp/rgv-mf15-alignment-report.json
```

The variant and auxiliary-track export batch is:

```bash
cargo run --release -p rgv-desktop -- \
  --batch validation/manual-suite/batches/mf15-variant-track-exports.json \
  --automation-report /tmp/rgv-mf15-variant-report.json
```

Both reports should contain:

```json
"succeeded": true
```

Every command result should have `"status": "succeeded"`, and every exported file should exist and be non-empty. The batches write export artifacts under `benchmark-results/manual-suite/`; these files are disposable test output and are not part of the fixture suite.

The controlled-failure batch is expected to stop at its deliberately missing BAM:

```bash
cargo run --release -p rgv-desktop -- \
  --batch validation/manual-suite/batches/mf15-controlled-failure.json \
  --automation-report /tmp/rgv-mf15-failure-report.json
```

For this batch, a top-level failure is the expected result. Confirm that the report identifies the failing command and that later commands are skipped rather than incorrectly reported as successful. Because the deliberate failure occurs before the batch's exit command, close the application manually if it remains open.

After inspecting the exports, clean up disposable output:

```bash
rm -rf benchmark-results/manual-suite
rm -f /tmp/rgv-mf15-alignment-report.json
rm -f /tmp/rgv-mf15-variant-report.json
rm -f /tmp/rgv-mf15-failure-report.json
```

## 9. Optional code-level regression checks

These are useful before handing a build to another tester, but they are not a substitute for the visual pass:

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## 10. Record results

Use one result block per scenario:

```text
scenario:
commit:
date and tester:
operating system:
CPU and memory:
GPU and backend:
display resolution and scaling:
mouse or trackpad:
result: pass | fail | blocked
notes:
screenshot or screen recording:
log or automation report:
issue:
```

For restricted biological data, record only a safe dataset label, checksum, sequencing technology, reference build, index type, and tested loci. Do not copy private sequence, read names, credentials, bearer tokens, or signed URLs into reports.

When reporting a visual or interaction problem, include:

- the project and scenario ID;
- the starting locus;
- the exact pointer, wheel, keyboard, or menu actions;
- whether the mouse button was still held;
- the zoom level before and after;
- whether releasing the mouse changed the result;
- a screenshot before and after;
- the application log and commit hash.

## 11. Final integrity check

After a pass, make sure manual testing did not alter the declared fixture suite:

```bash
cargo run --release -p rgv-validation-runner \
  --bin rgv-manual-fixture-check -- \
  verify --require-complete

git status --short
```

The verifier should pass. Any remaining files under `benchmark-results/` or `/tmp` are test evidence or disposable runtime output, not fixture inputs.
