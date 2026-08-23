# Validation matrix

RGV correctness is checked at three levels. Unit and property-style tests cover coordinates,
CIGAR/reference consumption, filtering, cache eviction, packing, base orientation, modified-base
decoding, feature formats, session migration, remote ranges, and deterministic raster export.
Differential alignment benchmarks compare independent HTS backends over identical records. Golden
display-list tests avoid GPU and platform variance.

The GUI redesign adds deterministic tests for adaptive ruler spacing, cytoband/fallback ideograms,
locus history, dynamic gutter/scissor geometry at 1× and 2× scale, track selection/resize/reorder,
deep-scroll alignment, diagnostics visibility, command discovery, and bounded event labels. Raster
fixtures cover short, long, paired, spliced, supplementary, and deep alignment presentations plus
all major event classes.

The 21 August 2026 GUI-redesign audit passed 123 locked workspace tests, strict Clippy with all
targets and features, and a release desktop build. On `benchmark-data/mixed.bam`, the regional
interaction run measured 1.115 ms median total, 1.217 ms p95 total, and 43.975 ms to first useful
frame. The 5 kb read-detail run measured 0.529 ms median total, 0.595 ms p95 total, and 35.647 ms to
first useful frame. Reports are archived as `benchmark-results/gui-redesign-regional.json` and
`benchmark-results/gui-redesign-reads.json`.

The allele-aware pileup and semantic-event candidate passed 157 workspace tests, strict Clippy across all targets and
features, and a release desktop build. On the same 20 kb mixed-BAM regional workload, first useful
frame was 77.163 ms, median cached interaction was 5.783 ms, and p95 was 6.651 ms after pileup work
was restricted to the level of detail that can expose it. The 30-base generated variant-truth
workload, with exact allele decoding active, measured 0.487 ms to first useful frame, 0.062 ms
median, and 0.075 ms p95. Exact truth tests independently verify A/C counts, strand support, four
insertions, five deletions, minimum-base-quality behavior, full-evidence coverage under visible-read
sampling, and local/remote BAM equivalence.

The deterministic `rgv-alignment-bench generate` corpus includes paired short reads, deep coverage,
spliced reads, secondary and supplementary records, long reads, modified bases, valid PacBio SMRT
kinetic arrays, exact SBX insertion/tail quality states, Ultima flow-indel truth, missing fields,
and a 70,000-operation CIGAR. Before beta, the external validation set must additionally include
real Illumina paired-end, ONT, PacBio HiFi and CLR, PacBio kinetic, RNA, amplicon, bisulfite,
Ultima, SBX, tagged single-cell, high-depth, SV-heavy, and reference-dependent CRAM datasets. Only
redistributable fixtures may enter the repository; restricted fixtures are identified by SHA-256
in private runner configuration. The complete-matrix gate checks normalized specialist semantics,
not only category strings.

The 21 August 2026 modified-base, supplementary-chain, paired-read, and splice-junction candidate passed 205
workspace tests, strict Clippy across all targets and features, an optimized desktop build, and the pinned synthetic
corpus manifest. At `chr2:1-200000`, the active aggregate-modification workload retained 412 calls
in two series and measured 17.907 ms to first useful layout, 0.241 ms median cached interaction, and
0.585 ms p95. Pair view at `chr1:1-200000` classified 8,161 eligible proper templates from a bounded
4,096-template inference sample and measured 26.707 ms to first useful layout, 1.649 ms median, and
4.026 ms p95. At `chr1:2100001-2301100`, the RNA workload aggregated all 10,000 generated
junctions before display sampling, rendered a deterministic 4,096-junction subset, and measured
32.276 ms first useful, 2.691 ms median, and 3.724 ms p95. The retained reports are
`benchmark-results/interaction-modifications-active.json`,
`benchmark-results/interaction-paired-active.json`,
`benchmark-results/interaction-junctions-active.json`, and
`benchmark-results/validation-synthetic-latest.json`. Resident-memory samples were unavailable in
the sandbox and must be captured during native release qualification.

Useful commands:

```sh
cargo test --workspace
cargo run --release -p rgv-validation-runner --bin rgv-demo-corpus
cargo fuzz run filter_expression
cargo fuzz run modified_bases
cargo fuzz run session_input
cargo fuzz run text_features
cargo fuzz run alignment_file
cargo fuzz run normalized_record
cargo run --release -p rgv-alignment-bench -- run --iterations 5
cargo run --release -p rgv-interaction-bench -- \
  benchmark-data/mixed.bam --maximum-median-ms 16.7 --maximum-p95-ms 16.7 \
  --maximum-layout-ms 16.7 --maximum-first-useful-ms 250
```

The scheduled `Fuzz resilience` workflow runs every boundary above for a bounded campaign and
retains any minimized reproducer. Pull-request CI separately compiles all fuzz targets with their
locked dependency graph.

Release qualification also covers rapid navigation, reopening, empty loci, missing indexes,
malformed files, reference mismatch, HTTP latency/bandwidth disruption, crash recovery, and clean
installation on each supported platform.

The controlled Phase 8 backend slice passed on 22 August 2026. It covers local BAM with BAI/CSI,
local reference-dependent CRAM/CRAI, every supported conventional and custom remote BAM/CRAM index
name, authenticated signed remote FASTA/FAI, local/remote semantic parity, redirects, retry,
cancellation, validators, short bodies, corrupt/truncated data, mismatched reference, bounded disk
eviction, and reopen recovery. The gate passed 398 workspace tests, strict Clippy, the release
desktop build, synthetic validation, and the refreshed simulated-WAN report. Its exact scope,
results, fixes, and remaining real-data limitations are recorded in
[`benchmark-results/BACKEND_QUALIFICATION.md`](../benchmark-results/BACKEND_QUALIFICATION.md).

The post-feature headless performance matrix is also recorded. It covers 1x1, 1x2, 4x2, and 8x2
short-read projections plus the user's real ONT demo locus, with raw cached, vertical-density, and
selected-to-top samples, RSS, record, primitive, and modification counts. See
[`benchmark-results/MULTI_TRACK_SCALING.md`](../benchmark-results/MULTI_TRACK_SCALING.md). A targeted
post-freeze packaged Metal presentation pass is now retained separately; ordinary packaged
interaction, export timing, soak, and matched IGV measurements remain Phase 9 gates.

The executable corpus manifest and release matrix are documented in
[`validation/README.md`](../validation/README.md). A release record is incomplete unless the real
corpus runner passes with `--require-complete-matrix` and its manifest plus JSON report are archived.
