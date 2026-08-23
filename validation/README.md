# Corpus validation

`rgv-validation-runner` hashes every alignment, index, and optional reference before opening it
through the production HTS path. Each named locus declares minimum record counts and semantic
features that must survive normalization. Reports contain timings and counts, never read names,
sequences, tags, paths, or credentials.

The deterministic development corpus can be generated and checked with:

```sh
cargo run --release -p rgv-alignment-bench -- generate
cargo run --release -p rgv-alignment-bench -- generate-variants
cargo run --release -p rgv-validation-runner -- \
  validation/synthetic-corpus.json \
  --output benchmark-results/validation-synthetic-latest.json
```

## Complete manual fixture suite

The focused suite under `benchmark-data/manual-suite` is the preferred starting point for broad
desktop testing. It maps every advertised desktop capability and supported format to a named
project or operational scenario, with exact truth and deliberate failures:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- \
  verify --require-complete
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- qualify
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list
```

The generated index is [`manual-suite/README.md`](manual-suite/README.md). `qualify` performs two
disposable generations and proves identical declared hashes, tamper/stale/missing rejection,
portable relocation, exact production-reader semantics, and the size budget. The deliberate CRAM
wrong-reference test prints an htslib MD5 mismatch while the verifier succeeds.

For the controlled remote matrix, build the validation binaries together and run:

```sh
cargo build --release -p rgv-validation-runner --bins
target/release/rgv-fixture-server-check --suite benchmark-data/manual-suite
```

Fixture-ready means the data and workflows are ready for a person to test. It does not mean the
manual cards, representative real-data matrix, platform checks, or release qualification have
passed.

The checked-in synthetic manifest also pins `variants.bam`, its BAI, and the matching FASTA. Its
`chrVariants:13991-15120` truth locus requires all three Ultima records to survive indexed file
decoding with usable flow-indel evidence: directional `tp` insertion Q40, read-group-`mc` deletion
Q0, and `t0` single-base deletion Q35. The alignment-benchmark tests additionally assert those
three exact evidence sources through the production normalized query path.

For the first broad desktop pass, generate or verify the two handoff workspaces and open them
separately:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-demo-corpus
target/release/rgv-desktop --session benchmark-data/demo-tracks/rgv-demo-project.json
target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-alignment-review-project.json
```

The generator writes a SHA-256 manifest, reopens the portable project and its alignment/reference,
opens both named TDF channels, and requires non-empty production-engine queries from all 19
auxiliary tracks. It also reopens the four-panel alignment project, verifies its six-item Review
queue, and requires proper pairs, 10,000 junctions, long/supplementary/modified-base/SMRT/SBX
evidence, and the 70,000-operation CIGAR to survive the production HTS query path. These projects
give manual testing common starting points; they do not replace the private representative-
technology matrix.

The focused native smoke batch proves that the desktop restores and settles that exact project,
exports its track inventory, renders a visible-workspace SVG, and exits:

```sh
target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-demo-project.json \
  --batch validation/demo-workspace-smoke.json \
  --automation-report benchmark-results/demo-workspace-smoke-report.json

target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-alignment-review-project.json \
  --batch validation/alignment-review-workspace-smoke.json \
  --automation-report benchmark-results/alignment-review-workspace-smoke-report.json
```

The repository also contains a deterministic four-controller desktop acceptance session. Three
entries open the generated mixed BAM with different settings; the fourth intentionally references a
missing BAM so failure isolation is visible and repeatable:

```sh
cargo run --release -p rgv-desktop -- \
  --session validation/four-track-session-v2.json
```

The session fixture is schema-validated in the core test suite. Manual acceptance must confirm that
the three healthy tracks remain queryable, reorderable, inspectable, and independently configurable
while the failed track reports its own error.

Session evolution has one checked-in fixture for every released boundary:

- `session-v1.json` proves deterministic version-1 migration and track-ID collision avoidance;
- `four-track-session-v2.json` proves version-2 multi-track migration;
- `multi-panel-four-track-session-v3.json` proves the current version-3 representation; and
- `future-session-v99.json` proves that unknown future schemas are rejected safely.

Core tests also relocate a complete project directory and verify project-relative source
resolution, replace a project repeatedly through the atomic writer, and prove that a failed save
leaves the previous complete document readable.

Phase 7 has a checked-in native automation smoke batch. It opens the generated mixed BAM, creates
two panels, applies grouping and centre-base sorting, waits for the exact completed generations,
writes deterministic SVG and structured outputs, saves a project, writes an atomic result report,
and exits:

```sh
cargo run --release -p rgv-desktop -- \
  --batch validation/automation-smoke.json \
  --automation-report benchmark-results/automation-smoke-report.json

cargo run --release -p rgv-validation-runner \
  --bin rgv-automation-qualification -- \
  validation/automation-smoke-qualification.json \
  benchmark-results/automation-smoke-report.json \
  --output benchmark-results/automation-smoke-verification.json
```

Run it twice and compare both `benchmark-results/automation-smoke.svg` and
`benchmark-results/automation-full-panel.svg` byte-for-byte. The reviewed hashes from the current
release build are, respectively,
`ea837d45815dba8ed16e73f3ed65f536c6010a9b9fcb76e534703033ec4d54f9` and
`027ecb709c3432fc93db082ffe00154c797cf976054ab3bf7cfcc6e5e57574df`. The batch schema itself is
parsed by the core test suite. The report must contain nineteen ordered successful command results;
the alignment, pileup, modification, junction, and selected-evidence exports must be present; the
track-name TSV must name stable track 1; and the saved project must restore both panels and the
grouping/sort settings.

The qualification command proves those requirements mechanically. It matches all nineteen report
entries to the checked-in batch by index and command identity, requires every generated file to be
present and non-empty, verifies the two reviewed SHA-256 values, checks schema-v3 panel/track and
group/sort state, recomputes frame summaries from the raw samples, enforces the declared native
frame and GPU bounds, and writes a path-free artifact-digest and telemetry record. It fails on a
changed reviewed artifact, a mismatched/failed/skipped command, missing output, invalid project
identity, inconsistent telemetry, dropped samples, truncated frames, or a breached frame budget.

Schema-v3 multi-panel acceptance uses the same four controllers across two ordered loci and starts
with the second panel active:

```sh
cargo run --release -p rgv-desktop -- \
  --session validation/multi-panel-four-track-session-v3.json
```

Confirm that `chr1:900001-920000` appears on the left, `chr2:1-200000` appears on the right, the
right panel is active, all three healthy tracks query both panels, and the fourth track's expected
failure remains local. Save and reopen the session, then verify that panel IDs, order, loci, active
panel, track order, and track settings are unchanged.

The private release manifest uses the same schema and must be invoked with
`--require-complete-matrix`. That gate requires Illumina paired-end, ONT, PacBio HiFi, PacBio CLR,
PacBio SMRT kinetics, RNA-seq, amplicon, bisulfite, tagged single-cell, high-depth, SV-heavy, SBX,
Ultima, and reference-dependent CRAM categories. Labels alone do not satisfy the gate. The named
datasets must declare the applicable paired, long-read, modified-base, spliced,
bisulfite-context, valid kinetic-array, cell-barcode/UMI, supplementary, SBX, and Ultima
expectations, which are then checked against normalized records. Amplicon and high-depth rows have
explicit minimum-record thresholds; reference-dependent CRAM must name a `.cram` source and pinned
reference. Restricted source data stays outside the repository; the manifest pins its SHA-256 and
is retained with the release qualification record.

The deterministic local/remote format and failure matrix is complementary to that private corpus.
Its current evidence is archived in
[`benchmark-results/BACKEND_QUALIFICATION.md`](../benchmark-results/BACKEND_QUALIFICATION.md).
Passing the controlled matrix does not satisfy `--require-complete-matrix` or replace broad manual
review of the private real-world data.

The user-led release pass follows
[`MANUAL_QUALIFICATION.md`](MANUAL_QUALIFICATION.md). It begins with ordinary navigation and visual
review, then covers alignment evidence, multiple tracks/loci, variants and auxiliary tracks,
projects/recovery, exports/automation, accessibility, remote failures, platforms, and soak. Its
finding template deliberately excludes private sequence, read names, credentials, and signed URLs.
