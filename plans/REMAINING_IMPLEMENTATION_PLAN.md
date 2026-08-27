# RGV Remaining Implementation and Qualification Plan

> **Superseded on 22 August 2026.** This is a historical checkpoint, not the current execution
> order. Continue from `RGV_IGV_PARITY_ROADMAP.md`. In particular, the local-BAM checkpoint below
> no longer pauses major feature delivery; broad hands-on qualification is Phase 8 of the current
> roadmap.

Status date: 21 August 2026

This document starts from the current implemented prototype. It does not repeat the completed
architecture and feature work in `IMPLEMENTATION_PLAN.md`. The immediate next step is a hands-on
local BAM acceptance test. Further implementation should pause until that checkpoint has been run
and its findings have been recorded.

## 1. Current baseline

The repository currently provides:

- A native Rust desktop application using `winit`, `wgpu`, and `egui`.
- Indexed local BAM and CRAM access through HTSlib.
- An observable pure-Rust remote BAM backend with BAI/CSI support, bounded range caching, request
  coalescing, authentication redaction, and live I/O counters.
- Packed columnar alignment batches covering names, coordinates, flags, mates, CIGAR operations,
  sequence, qualities, and arbitrary BAM auxiliary fields.
- Coverage, read, reference, splice, insertion/deletion, clipping, mismatch, modified-base, and
  supplementary/secondary rendering semantics.
- Deterministic deep-coverage sampling and generation-based cancellation.
- BED, GFF3, GTF, VCF/BCF, BigWig, and BigBed tracks.
- Sessions, partial IGV session import, image export, settings, cache controls, file dialogs,
  drag-and-drop, recent items, and keyboard navigation.
- Bounded engine queues and caches. Unread viewport results coalesce to the newest generation while
  control and error events are retained, and full queues cannot deadlock shutdown.

The locked workspace suite, strict Clippy, fuzz-target compilation, dependency policy, synthetic
semantic validation, interaction latency, and remote-range gates are maintained as release gates.

This is a strong engineering checkpoint, but it is not yet a qualified beta or a finished IGV
replacement.

## 2. Immediate checkpoint: test a local BAM

### 2.1 Prepare the files

Use an indexed BAM together with its BAI or CSI. Prefer a BAM you know well enough to recognize
expected reads and coverage. If base-level comparison or mismatch display is required, also provide
the matching FASTA and FAI.

Examples of conventional layouts:

```text
sample.bam
sample.bam.bai
reference.fa
reference.fa.fai
```

If the BAM is not indexed, create an index outside RGV before testing. RGV should report the absent
index cleanly rather than scanning the whole file.

### 2.2 Launch the desktop application

From the repository root, use either the command line:

```sh
cargo run --release -p rgv-desktop -- --bam /absolute/path/to/sample.bam
```

or launch without a file and select **Open BAM/CRAM…**:

```sh
cargo run --release -p rgv-desktop
```

To supply a reference at launch:

```sh
cargo run --release -p rgv-desktop -- \
  --bam /absolute/path/to/sample.bam \
  --reference /absolute/path/to/reference.fa
```

Use absolute paths for this checkpoint so failures cannot be confused with the working directory.

### 2.3 Acceptance scenarios

Run the following scenarios in order and record pass, fail, and any surprising behavior.

1. **Open and initial view**
   - The application opens without a panic or terminal error.
   - The reference dictionary loads and a valid locus is shown.
   - Coverage and alignments appear within a reasonable time.
   - The status line reports emitted records or deterministic sampling at deep loci.

2. **Known locus correctness**
   - Navigate to a locus with known reads.
   - Confirm approximate coverage depth and alignment positions against a trusted expectation.
   - Check forward/reverse strands, mapping qualities, and paired-read state.
   - At a suitable zoom, inspect CIGAR-derived insertions, deletions, clipping, and splice junctions.

3. **Read inspection**
   - Select representative reads.
   - Confirm read name, flags, coordinates, mate fields, CIGAR, sequence/quality state, and tags.
   - If present, check `RG`, `HP`, `MM`, and `ML` fields.

4. **Short- and long-read behavior**
   - For short reads, check dense paired-end packing and mate semantics.
   - For long reads, check long CIGARs, large indels, supplementary alignments, and smooth panning.
   - If the first BAM covers only one technology, repeat later with a BAM from the other category.

5. **Navigation and stale-result safety**
   - Pan and zoom rapidly across several loci.
   - Stop at a recognizable locus and confirm an earlier result never replaces it.
   - Confirm the UI remains responsive while decoding a deep region.

6. **Deep coverage**
   - Open a high-depth locus.
   - Confirm memory remains bounded and navigation remains usable.
   - Confirm any truncation is explicitly reported as a deterministic sample.

7. **Reference behavior**
   - With a matching FASTA, confirm bases and mismatches appear at base-level zoom.
   - With a deliberately mismatched reference dictionary, confirm RGV reports the mismatch rather
     than displaying misleading bases.

8. **Error and recovery paths**
   - Try a copy without its index and confirm a useful error.
   - Open the valid BAM again and confirm the application recovers without restarting.
   - Close the application while a deep query is running and confirm shutdown completes normally.

### 2.4 Evidence to retain

For each problem, retain:

- The exact launch command, excluding credentials.
- Operating system, CPU, memory, GPU, and display scaling.
- BAM/BAI/CSI and reference sizes; a checksum is useful if the data may be reused.
- The locus and zoom level.
- Expected and observed behavior.
- A screenshot for visual issues.
- Terminal output or a backtrace for crashes, after checking it contains no sensitive read data.

Do not add private genomic data, read names, sequences, authentication values, or signed URLs to
the repository.

### 2.5 Checkpoint exit criteria

This checkpoint is complete when:

- At least one indexed local BAM opens in the release build.
- A known locus has been manually checked for coverage and alignment semantics.
- Rapid navigation does not display stale results.
- A representative read can be inspected without missing or corrupted fields.
- The application exits normally during or after a deep query.
- Every observed defect is either fixed and retested or recorded with a reproducible description.

## 3. Resolve findings from the local BAM checkpoint

No new format work should begin while a correctness, crash, stale-display, or data-corruption issue
from the checkpoint remains unexplained.

For each finding:

1. Reduce it to the smallest shareable fixture or synthetic equivalent.
2. Add a failing unit, integration, golden, or screenshot test at the lowest appropriate layer.
3. Fix the production path without bypassing bounded queues, caches, or generation checks.
4. Run the focused regression test.
5. Run the full locked workspace suite and strict Clippy.
6. Repeat the original manual scenario in the release desktop build.

Performance-only findings should include timings on the same hardware before and after the change.

## 4. Remote CRAM and reference qualification

Remote BAM is observable in-process; remote CRAM still uses HTSlib and lacks equivalent direct
request and byte accounting. This is the next format/backend implementation milestone after local
BAM acceptance.

### 4.1 Backend decision spike

Evaluate two bounded options against the same CRAM, CRAI, FASTA, and FAI fixtures:

- A `noodles-cram` reader over RGV's existing `HttpRangeReader`.
- The current HTSlib reader measured through the controlled loopback proxy specified by the IGV
  comparison protocol.

Measure correctness, reference resolution, first-query latency, warm-query requests, transferred
bytes, cancellation response, and implementation risk. Select the pure-Rust backend only if it
preserves the required CRAM semantics and meets the latency budget; otherwise retain HTSlib with an
explicit observable proxy boundary.

### 4.2 Required behavior

- Explicit and adjacent CRAI discovery.
- Local and remote reference configuration with dictionary validation.
- Missing-reference and checksum failures reported clearly.
- Bearer/header authentication applied without persistence or logging.
- Request, response-byte, retry, and cache metrics exposed to the desktop where the backend permits.
- Bounded cache use and generation-aware cancellation.
- Normalized record parity with the local HTSlib path.

### 4.3 Tests and gate

- Generate or pin a redistributable CRAM/CRAI/reference fixture containing paired, spliced,
  supplementary, long-CIGAR, missing-sequence, and modified-base cases where supported.
- Serve the fixture with injected latency and one transient failure.
- Query known shallow and deep loci, then repeat them warm.
- Compare normalized digests with the local source.
- Enforce explicit request, transferred-byte, wall-time, retry, and warm-cache thresholds.

Exit criterion: remote CRAM/reference behavior is correct and its network cost is reproducibly
observable, whether in-process or at the controlled proxy.

## 5. Native desktop runtime qualification

The code compiles in CI-oriented checks, but the native application still needs deliberate runtime
qualification on supported hardware.

### 5.1 Platforms

- macOS on Apple Silicon and, if distributed, Intel.
- Windows 11 on a representative integrated GPU and a discrete GPU.
- A supported Linux distribution under Wayland and X11 where practical.

### 5.2 Runtime matrix

- Window creation, resize, minimize/restore, display scaling, and multi-monitor movement.
- GPU adapter selection, device loss, surface recreation, and unsupported-adapter errors.
- Mouse, trackpad, keyboard, text scaling, and accessibility labels.
- File dialogs, drag-and-drop, recent items, sessions, screenshots, and cache clearing.
- Repeated open/close cycles and application exit during active local and remote work.
- One-hour pan/zoom soak with RSS, GPU memory, frame time, and error count recorded.

### 5.3 Runtime budgets

- No storage or network I/O on the UI thread.
- Cached p95 interaction below 16.7 ms on named reference hardware.
- First useful local frame below 250 ms for the agreed warm indexed locus.
- No monotonically growing memory use during the soak.
- No stale viewport display, deadlocked shutdown, panic, or unrecovered device loss.

Exit criterion: the platform matrix and soak run pass with raw reports retained.

## 6. Real-world technology corpus

The synthetic corpus is necessary but not sufficient. Build a pinned, access-controlled release
manifest covering:

- Illumina paired-end.
- Oxford Nanopore.
- PacBio HiFi and CLR.
- RNA-seq.
- Amplicon.
- Bisulfite.
- Single-cell tagged BAM.
- Very high depth.
- Structural-variant-heavy alignments.
- CRAM with external reference requirements.

Each entry must include checksums, declared loci, minimum counts, expected semantics, and permitted
use. Run the validation tool with `--require-complete-matrix`. Retain only aggregate, non-sensitive
reports in normal build artifacts.

Exit criterion: every technology category passes normalization and visual review at its declared
loci, with no record type or supported field silently discarded.

## 7. Fuzz, corruption, and recovery qualification

- Run bounded live fuzz campaigns for normalized records, CIGARs, auxiliary tags, `MM`/`ML`,
  filters, and malformed inputs on an approved nightly Rust toolchain.
- Seed fuzzing with minimized, redistributable examples from corpus defects.
- Test truncated BAM/CRAM/index/reference files and inconsistent headers.
- Reopen a valid file after each recoverable failure.
- Exercise disk-cache corruption, stale validators, interrupted writes, and retry exhaustion.
- Verify crash markers and recovery messaging without opt-out diagnostics becoming enabled.

Exit criterion: the agreed campaign duration completes without an unresolved crash, memory-safety
issue, infinite loop, unbounded allocation, or silent data corruption.

## 8. Reproducible RGV versus IGV comparison

Use the protocol in `benchmarks/IGV_COMPARISON.md` on named, otherwise-idle hardware.

### 8.1 Lock the environment

- Pin exact RGV and IGV versions.
- Record OS, CPU, memory, storage, GPU, Java version, display scaling, and cache state.
- Use identical local and remote files, loci, viewport sizes, and warm/cold rules.
- Retain raw samples rather than only summaries.

### 8.2 Compare

- Startup and time to first useful frame.
- Pan/zoom p50 and p95 latency.
- Deep-locus settle time.
- CPU and resident memory.
- Remote request count, bytes, retries, and wall time.
- Correctness screenshots and semantic summaries at the same loci.

Do not claim that RGV is faster than IGV from the current self-only benchmark. Make that claim only
when the matched comparison passes its predetermined thresholds and the raw evidence is reviewable.

Exit criterion: a reproducible report establishes where RGV is faster, equivalent, or slower and
identifies any remaining release-blocking regressions.

## 9. Packaging, signing, and clean-machine installation

- Build release artifacts for macOS, Windows, and Linux from pinned CI inputs.
- Sign Windows artifacts and macOS code; notarize macOS distributions.
- Generate checksums, license notices, and provenance/SBOM artifacts.
- Install, launch, open a local BAM, save/reopen a session, and uninstall on clean machines.
- Confirm no undeclared runtime dependency, writable install-directory assumption, or credential
  persistence.
- Document update and rollback behavior before enabling any updater.

Exit criterion: signed artifacts pass clean-machine smoke tests on every supported platform and can
be verified independently.

## 10. Beta exit criteria

RGV may be called a beta when all of the following are true:

- The local BAM checkpoint and all resulting correctness fixes are complete.
- Remote BAM and CRAM/reference paths meet observable WAN budgets.
- The complete technology corpus passes.
- Native platform, soak, fuzz, corruption, and recovery gates pass.
- A matched IGV report exists and performance claims match its evidence.
- Signed installers pass clean-machine tests.
- User documentation covers supported formats, coordinates, sampling, references, authentication,
  privacy, sessions, limitations, and troubleshooting.
- Session-schema compatibility and migration rules are committed.

## 11. V1 exit criteria

After beta feedback:

- Resolve all release-blocking correctness, crash, accessibility, and data-loss defects.
- Re-run the entire qualification matrix on the release candidate.
- Freeze supported platform and format guarantees.
- Publish reproducible performance results with hardware and methodology.
- Publish signed artifacts, checksums, notices, compatibility policy, and known limitations.
- Tag the release only after all retained reports refer to the exact release commit and artifacts.

## 12. Execution order

The remaining work should proceed in this order:

1. Run the local BAM acceptance checkpoint.
2. Fix and retest every blocking local-BAM finding.
3. Qualify remote CRAM and reference resolution.
4. Complete native runtime and soak testing.
5. Run the full real-world technology corpus.
6. Complete fuzz, corruption, and recovery campaigns.
7. Run the matched RGV-versus-IGV benchmark.
8. Build, sign, and clean-machine-test release artifacts.
9. Close beta findings and repeat the release-candidate qualification matrix.

The first action when work resumes is therefore not additional backend implementation: it is
launching the release desktop application against the user's chosen indexed local BAM and recording
the acceptance results from section 2.
