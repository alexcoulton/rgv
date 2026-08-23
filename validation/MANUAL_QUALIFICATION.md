# RGV broad manual qualification

Status: ready for the first complete user-led pass after the major feature implementation.

This checklist is for judging the finished application as a coherent genome viewer. It is not a
request to time every click. Use the app normally, record anything that is wrong, confusing,
jumpy, illegible, unexpectedly slow, or unlike the underlying evidence, and keep going unless the
problem risks incorrect interpretation, a crash, data loss, stale evidence, or runaway resource
use.

## 0. Start here

Generate and strictly verify the complete focused fixture suite first:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- generate
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixture-check -- \
  verify --require-complete
cargo run --release -p rgv-validation-runner --bin rgv-manual-fixtures -- list
```

The copyable project commands and the short/full scenario indexes are in
[`manual-suite/README.md`](manual-suite/README.md). Begin with MF-01, MF-02, MF-03, MF-09, MF-10,
MF-12, and one MF-14 outage/recovery profile. The remaining cards cover each specialist mode,
genome/review workflows, recovery/import, exports, automation, and accessibility. Each card starts
as `manual result not run`; fixture readiness is not a manual pass result.

`cargo run` builds changed binaries automatically, so a separate build is optional. Release mode
is still the default for visual testing. The older two broad handoff projects below remain useful
complements: one combines supported auxiliary tracks and one stresses alignment behavior.

First open the mixed-format project:

```sh
cargo build --release -p rgv-desktop
target/release/rgv-desktop --session benchmark-data/demo-tracks/rgv-demo-project.json
```

It opens `chrVariants:8501-16000` with the synthetic alignment/reference truth plus 19 auxiliary
tracks: compressed BED/GTF, genePred, narrowPeak/broadPeak, PSL, GWAS, indexed BCF, BigWig, TDF,
bedGraph, WIG, BEDPE/interact, SEG, mutation MAF, BigBed, bigGenePred, and bigNarrowPeak. Every
generated source is checksum-manifested, and the generator verifies that RGV can reopen and query
non-empty evidence from every track before it succeeds.

Then open the alignment-review project:

```sh
target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-alignment-review-project.json
```

It starts with four panels containing paired short reads, 10,000 RNA junctions, long and
supplementary reads with modified-base/SMRT/SBX evidence, and a 70,000-operation CIGAR. The Review
queue contains those four loci plus the 150,000-record deep-coverage locus and the structural-pair
truth. The generator reopens this project and rejects it unless the panels, queue, and named
alignment semantics remain queryable through the production HTS path.

For a simpler base-level alignment view, use:

```sh
target/release/rgv-desktop \
  --bam benchmark-data/variants.bam \
  --reference benchmark-data/variants.fa \
  --locus chrVariants:9941-10080
```

Use `chrVariants:9401-12600` for the deliberately large 100/1,000 bp insertions and 500/2,000 bp
deletions. Regenerate and mechanically re-verify the complete project when needed with:

```sh
cargo run --release -p rgv-validation-runner --bin rgv-demo-corpus
```

Begin by using both projects normally and reporting anything obviously wrong. The remainder of this
document is a later coverage checklist, not a requirement to complete every row before reporting a
finding. These synthetic projects prove mechanics and known truth; representative private short-
read, long-read, RNA, cohort, and remote data still matter during the broader pass.

## 1. Record the test build once

Create one evidence directory for the run and record:

```text
RGV version/build:
source commit or archive identifier:
package SHA-256:
operating system:
CPU / memory:
GPU:
display resolution and scaling:
mouse / trackpad / keyboard:
date:
tester:
```

Use the exact staged or installed release candidate throughout a pass. A development binary may be
used to reproduce and fix a finding, but do not silently substitute it for the release candidate.

## 2. Data matrix

For restricted data, record only a safe dataset label, SHA-256, technology, reference build, index
type, and tested loci. Do not put read names, credentials, signed URLs, or private sequence into the
log.

| Category | Dataset ID / SHA-256 | Reference | Loci and known truth | Result |
| --- | --- | --- | --- | --- |
| Illumina paired-end |  |  | ordinary, deep, discordant pairs, indels | Not run |
| ONT |  |  | SNVs/indels, long clips, supplementary reads, modified bases | Not run |
| PacBio HiFi |  |  | long reads, indels, supplementary reads | Not run |
| PacBio CLR |  |  | noisy long reads, large indels/clips | Not run |
| PacBio SMRT kinetics |  |  | valid `ip`/`pw` or CCS strand-specific kinetic arrays | Not run |
| RNA-seq |  |  | known exon junctions and strand evidence | Not run |
| Amplicon |  |  | extreme depth and read boundaries | Not run |
| Bisulfite |  |  | known protected/converted contexts on both strands | Not run |
| SBX |  |  | simplex tails and concordant/discordant insertion evidence | Not run |
| Ultima |  |  | directional/minimum flow-indel and `t0` deletion evidence | Not run |
| Tagged single-cell BAM |  |  | RG/CB/UB or equivalent tags | Not run |
| High-depth |  |  | at least 100,000 observed alignments at a pinned locus | Not run |
| SV-heavy |  |  | supplementary chains, discordant pairs, large events | Not run |
| Variant/cohort VCF or BCF |  |  | SNV/MNV/indel/SV/BND, phased and missing GT | Not run |
| Quantitative/copy-number |  |  | BigWig/bedGraph/WIG and SEG/CBS | Not run |
| Annotation/interaction |  |  | BED/GFF/GTF/BigBed/BEDPE/interact | Not run |
| Reference-dependent CRAM |  |  | correct and deliberately wrong reference | Not run |
| Remote BAM/CRAM/reference |  |  | authenticated or signed test endpoint | Not run |
| Malformed/failure fixtures | generated | synthetic | missing/corrupt index, truncated source, outage | Not run |

The checked-in synthetic BAM and variant truth data can establish mechanics, but they do not replace
the real Illumina, ONT, PacBio, RNA, bisulfite, SBX, Ultima, single-cell, cohort, CRAM, or remote
rows.

## 3. First-use smoke pass

Start with one ordinary BAM and use RGV without consulting implementation details.

- Open the application, choose a BAM/CRAM, enter a locus, and reach interpretable reads.
- Pan and zoom with the wheel, Shift+wheel, top overview control, and right overview control.
- Drag each overview control slowly and quickly, starting both fully zoomed out and deeply zoomed
  in. The pointer must not hit an artificial screen edge during a held zoom gesture.
- Scroll through a deep read stack. Reference, ruler, coverage, junction, and other independent
  tracks must remain fixed; only the read stack should move vertically.
- Resize the window and track heights, use Retina/fractional scaling if available, minimize and
  restore, and move the window between displays if available.
- Close and reopen the source and application. There must be no flicker loop, unexplained blank
  frame, stale locus, or crash.

If this pass feels fundamentally wrong, log the concrete behaviors before moving to specialist
features. Do not stop merely because a measured load is around 500 ms.

## 4. Alignment evidence

Repeat on representative short-read and long-read data.

- Confirm reference letters, read letters, strand colours, base-quality shading, mapping quality,
  and mismatch colours remain stable as scale changes.
- Inspect all CIGAR forms: match/equality/difference, insertion, deletion, skip, soft clip, hard
  clip, and pad where present. Small and large indels must be distinguishable and correctly
  anchored; labels such as `I1000` must not masquerade as a reference base.
- Compare coverage A/C/G/T/N, insertions, deletions, strands, quality weighting, and consensus with
  known truth or an independent calculation.
- Test paired view, connectors, orientation/template-length classifications, mate navigation, and
  overlapping-mate counting.
- Inspect supplementary-chain lanes, previous/next segment navigation, all-segment panels, and
  selected-read-to-top behavior.
- Inspect modified-base codes, probability threshold, strand, frequency aggregation, legend, and
  malformed `MM`/`ML` handling.
- Test spliced reads, junction counts, strand evidence, filters, Sashimi layout, and export on RNA
  data.
- Exercise grouping and sorting by base, strand, pair strand, read group, haplotype, tag, mate,
  clips, name, length, quality, and position in both directions. Changing display-only settings
  must not reread or vertically reshuffle unrelated evidence.
- Test collapsed, squished, expanded, and vertical zoom independently.

## 5. Multiple tracks and loci

- Load at least four alignment tracks and two loci; then repeat with eight tracks if resources
  allow.
- Reorder, resize, hide, collapse, remove, and bulk-configure tracks. Track identity and settings
  must not migrate to another row.
- Pan one independent panel, then use linked navigation deliberately. Unlinked panels must not move.
- Navigate to a mate, supplementary segment, structural endpoint, feature, ROI, and review item.
  The origin selection must remain intelligible.
- Make one track fail or point to an empty locus. Healthy tracks and other panels must remain
  visible, interactive, and correctly labelled.
- Zoom while data is loading and while keeping the pointer held down. An older query must never
  replace the newer view, and reads must not vanish until mouse release.

## 6. Variants and other track types

- Inspect SNV, MNV, insertion, deletion, multi-allelic, symbolic SV, inversion, and interchromosomal
  BND records. Confirm coordinates, REF/ALT, filters, INFO types, endpoints, and orientation.
- Search, subset, group, and colour genotypes; inspect phased, unphased, missing, filtered, and
  multi-allelic calls. Large sample lists must remain scrollable and bounded.
- Apply QUAL, FILTER, AF, variant-type, genotype, INFO, FORMAT, and GT filters and compare the
  remaining records with expected truth.
- Test BED/BED12, GFF3/GTF, genePred/refFlat, narrowPeak, BigBed, BigWig, bedGraph/WIG,
  BEDPE/interact, SEG/CBS, and MAF data that are in the declared beta scope.
- Exercise automatic/fixed/shared scales, transforms, overlays, arithmetic composition, copy-number
  sample sorting, arc/block interaction display, selection, inspectors, and linked endpoints.
- Load a standalone FASTA/genome bundle, aliases, default annotation, feature and variant-ID search,
  reverse reference orientation, and six-frame translation.
- Create/edit/import/export ROIs and review queues; filter, assign, annotate, and navigate them.

## 7. Projects, recovery, automation, and exports

- Save and reopen a project containing multiple panels and mixed track types. Verify loci, order,
  active panel, scroll positions, settings, selections that are intended to persist, and relative
  paths.
- Move a self-contained project directory and reopen it. Missing external sources must be listed
  independently without destroying the rest of the workspace.
- Enable recovery snapshots, make changes, simulate an unclean exit in a disposable project, and
  choose or reject recovery explicitly. A recovered document must not overwrite the original until
  manually saved.
- Import a representative IGV XML session and inspect the imported/approximated/ignored report.
- Run the checked-in automation smoke batch twice. Structured outputs and deterministic SVGs must
  match; errors must stop the batch with a clear command index and stable error code. Run
  `rgv-automation-qualification` after each pass so command correspondence, all generated files,
  reviewed hashes, saved project state, and native frame/GPU telemetry are checked mechanically.
- Export visible PNG/SVG, full-panel SVG, alignments, pileup, modifications, junctions,
  variants/genotypes, quantitative windows, selected evidence, regions, review items, and track
  names. Open every output in an independent viewer or parser and confirm coordinates/provenance.
- Try a deliberately oversized export and confirm bounded or streamed behavior is disclosed rather
  than freezing or silently truncating.

## 8. Keyboard, accessibility, and operating-system behavior

- Complete the basic open, navigate, panel, track, evidence, settings, export, and close workflow
  without a mouse.
- Check focus order, visible focus, accessible names/values, status announcements, 75–200% UI
  scaling, increased contrast, and reduced motion.
- Use the platform screen reader on the navigation controls, overview sliders, selected track,
  resize handle, status, inspectors, and supplementary segments.
- Test mouse and trackpad, sleep/wake, display scaling changes, device loss if reproducible,
  repeated open/close, shutdown while work is active, and at least one one-hour mixed-use soak.
- Watch resident memory during repeated navigation. It may warm to bounded caches, but it must not
  rise without limit once the same workload repeats.

## 9. Remote and failure behavior

- Open remote BAM and CRAM using conventional and custom indexes, with any required authentication
  and explicit CRAM reference.
- Pan away rapidly during a slow request. Obsolete data must not appear later; cancellation and
  retry counters should remain understandable.
- Test a transient outage, longer outage, changed object/validator, missing index, short response,
  and corrupt source through the controlled endpoint.
- Reopen after restoring the endpoint. Recovery must not require restarting unrelated tracks.
- Inspect logs, diagnostics, sessions, projects, reports, and exports for credential, signed-query,
  read-name, or sequence leakage.

## 10. Finding record

Use one entry per behavior:

```text
ID:
build/package SHA-256:
severity: blocker / high / normal / low
dataset ID and SHA-256 (no private path or sequence):
locus and displayed coordinate convention:
track/panel settings:
exact actions:
expected result:
actual result:
repeatability:
screenshot/video/log name:
memory/timing observation if relevant:
workaround:
```

A fix is closed only after the original workflow passes again and a minimal automated regression
fixture/test is added where practical.

## 11. Beta decision

The beta is not ready while any open finding can cause incorrect genomic meaning, a crash, data
loss, stale evidence, credential leakage, or unbounded work. Ordinary usability findings are
triaged and retested as a coherent pass. Performance is tuned from recorded finished-workflow
measurements, not from an isolated number that already feels fast.

The final evidence bundle must include the completed data matrix, finding log, exact package hashes,
real-corpus report, backend/WAN report, platform/package results, raw performance samples, matched
IGV procedure/results, accessibility notes, and soak result.
