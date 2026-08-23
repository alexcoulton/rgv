# RGV IGV-Parity Execution Roadmap

Status date: 23 August 2026

## 1. Authority and purpose

This document is the current forward plan for turning RGV into a fast, native desktop genome
viewer that can replace IGV Desktop for the most common short-read, long-read, RNA, variant, and
comparative review workflows.

The delivery policy is **feature first**: implement every major workflow in the agreed product
boundary with automated correctness and regression coverage, then perform the broad hands-on
usability, real-data, performance-tuning, and platform qualification pass. Manual checks during
feature development are used to investigate a concrete defect, not as phase gates that stop
unrelated feature work. A fast-looking measurement is never a reason to interrupt missing-feature
work unless a regression makes development impractical.

It reconciles the implemented state with `REMAINING_IMPLEMENTATION_PLAN.md`,
`PRODUCT_PARITY_IMPLEMENTATION_PLAN.md`, and `RGV_FORWARD_IMPLEMENTATION_PLAN.md`. Those documents
remain useful design records, but this roadmap is the ordering and completion authority for new
work. `RGV_BETA_EXECUTION_PLAN.md` is the concise current queue derived from this roadmap. When
implementation changes the baseline, update both files in the same change.

The objective is workflow parity, not a pixel-for-pixel clone. An RGV feature may differ from IGV
when the result is clearer, faster, safer, or better suited to a native GPU application. It may not
omit analytical meaning, hide incomplete evidence, or silently change genomic coordinates.

## 2. Product definition

RGV will be successful when a researcher can:

1. open local or remote indexed genomic data without converting it to a proprietary project;
2. inspect Illumina, ONT, PacBio, RNA, amplicon, and tagged single-cell alignments accurately;
3. compare several samples at one locus or several linked loci at once;
4. inspect variants and genotypes beside the reads that support or contradict them;
5. navigate by coordinate, feature, mate, supplementary segment, breakend, ROI, or review item;
6. save, reopen, automate, and export the settled analytical state reproducibly; and
7. do all of this with bounded memory, responsive interaction, and measurable advantages over IGV.

The primary product is a native desktop application. The core, I/O, layout, and rendering crates
must remain reusable so a command runner or another front end does not require duplicating genomic
logic.

## 3. Progress language

Implementation and final qualification are reported separately. A feature is not described as
unfinished merely because the broad manual release pass has deliberately not happened yet.

**Implementation status**

- **Complete**: the planned workflow is implemented, documented, persisted where applicable, and
  protected by appropriate automated tests.
- **In progress**: an end-to-end portion is usable, but named feature work remains.
- **Planned**: the required workflow has not yet been implemented end to end.
- **Deferred**: deliberately outside the stated beta or version-1 scope.

**Qualification status**

- **Passed**: the completed feature has passed its declared representative-data, manual, platform,
  and performance checks.
- **Targeted only**: focused checks have investigated concrete implementation defects, but broad
  release qualification is still scheduled for Phase 9.
- **Pending**: the declared broad qualification has not yet been completed.

The programme therefore has two delivery stages: first implement all major feature phases with
automated correctness and regression coverage; then qualify the resulting product broadly and tune
the performance of the workflows that actually exist. Qualification work that began before the
feature boundary was re-audited is retained as useful evidence, but further broad qualification was
paused until Phase 8 closed on 23 August 2026. A public-beta workflow is **Done** only when both its
required implementation and qualification are complete.

Rendering once is not completion. A track or action is complete only when its model, query path,
layout, rendering, interaction, persistence, error behavior, documentation, and performance are
covered where applicable.

## 4. Current baseline

### 4.1 Candidate capabilities already present

- Native `winit`, `wgpu`, and `egui` desktop shell with an IGV-like white workspace.
- Indexed local BAM/CRAM plus observable bounded HTTP range access for BAM, CRAM/CRAI, and
  reference FASTA/FAI.
- Reference sequence, allele-aware coverage, read packing, all CIGAR operations, base-quality
  shading, indel and clipping semantics, splice junctions, and modified-base overlays.
- Paired-read classification, supplementary-chain inspection, and panel-linked navigation to a
  mate or supplementary segment.
- Deterministic deep-coverage sampling, bounded queues and caches, generation cancellation, and
  retained-frame navigation.
- Several alignment tracks with stable track identity, independent engines, shared workspace
  budgets, failure isolation, ordering, visibility, and session representation.
- Several independently navigable loci with stable panel identity, aligned track geometry, active
  panel state, and schema-v3 session persistence.
- BED, BED12, GFF3, GTF, VCF/BCF, BigWig, bedGraph, WIG, BigBed, BEDPE/interact,
  and multi-sample SEG/CBS tracks.
- A ready-to-open, checksum-manifested mixed-format project combines the alignment/reference truth
  with 19 auxiliary tracks; its generator reopens and queries every source through the production
  engine before succeeding.
- A companion four-panel alignment project and six-item Review queue expose the checked-in paired,
  deep, RNA, structural-pair, long/supplementary/modified/SMRT/SBX, and pathological long-CIGAR
  truth; its generator rejects semantic or project-wiring regressions before handoff.
- Local settings, recent items, command search, diagnostics, PNG export, JSON sessions, and partial
  IGV XML-session import.
- Interaction, range-I/O, synthetic semantics, multi-track, and multi-panel benchmark programs.

The latest complete Rust close gate was run on 23 August 2026 after the final desktop feature-access
audit: formatting passed, all 505 active workspace tests passed, strict
all-target/all-feature Clippy passed, the release desktop built, and the regenerated pinned
synthetic validation corpus passed. The opt-in live compatibility test also read and queried IGV's
official public compressed version-4 TDF fixture. The current post-freeze release binary was also
staged into a self-describing unsigned macOS arm64 application and sealed archive. Both payloads
passed manifest, SBOM, and provenance verification; the application executable completed the
nineteen-command two-panel Metal batch with unchanged reviewed SVG hashes. Broad signed-package and
manual acceptance remains scheduled for Phase 9.

The manual-test handoff project subsequently passed a focused native Metal smoke: all 19 configured
auxiliary sources opened without failure, the TDF expanded to both named channels, the workspace
settled, its complete track inventory and visible SVG exported, and the application exited through
the deterministic batch path. This proves the ordinary project-to-desktop wiring, not the visual
and representative-data judgments reserved for the user's pass.

The companion alignment-review project passed the same native path with all four panels settled in
489 ms, one alignment track exported, a non-empty visible-workspace SVG, no dropped frame samples,
and no truncated GPU frames. Its generator independently proves the named semantic evidence, so
the smoke is retained as desktop wiring evidence rather than treated as broad manual acceptance.

### 4.2 Current performance evidence

The post-feature optimized benchmark matrix on the development Apple Silicon machine reports:

| Workload | Cached median | Cached p95 | Density relayout median | Selected-to-top median | Peak RSS | Cold complete frame |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 alignment track x 2 panels | 3.09 ms | 3.23 ms | 2.99 ms | 3.51 ms | 48.03 MiB | 114.14 ms |
| 4 alignment tracks x 2 panels | 4.46 ms | 10.82 ms | 3.36 ms | 3.87 ms | 48.34 MiB | 443.73 ms |
| 8 alignment tracks x 2 panels | 3.97 ms | 4.44 ms | 3.66 ms | 4.63 ms | 61.53 MiB | 811.66 ms |
| Real ONT, 1 track x 1 panel | 0.31 ms | 0.35 ms | 0.25 ms | 0.33 ms | 16.34 MiB | 22.14 ms |

The ONT run used the user's 67 MiB `wf-human-variation-demo/demo.bam`, saw and retained 1,332 reads,
rendered 3,847 primitives, and aggregated 1,714 modified-base calls. Every cached scaling workload
is within the 16 ms median and 33 ms p95 budgets. These are headless data-to-display-list results;
the post-freeze packaged Metal automation pass additionally recorded 31 native presented frames at
7.19 ms median and 10.24 ms p95, no dropped samples, no truncated frames, and no GPU buffer
reallocations. Its startup-to-first-presented and startup-to-first-settled observations were 1.68 s
and 1.82 s; these are retained acceptance telemetry, not yet a matched cold-start protocol.
Ordinary packaged pan/zoom, longer resource sampling, export timing, and matched IGV comparison
still belong to Phase 9. No speculative cold-load tuning was performed, and the roughly
400--800 ms multi-track cold-complete measurements are not feature-development problems.

## 5. Updated parity-gap audit

The audit uses current official IGV Desktop behavior as a workflow reference. It adds several gaps
that were understated in the older RGV plans: alignment sorting and grouping, bulk track actions,
sample attributes, recovery autosaves, consensus and clipboard actions, complete quantitative
composition, and specialist sequencing modes.

| Workflow | Implementation | Qualification | Required outcome |
| --- | --- | --- | --- |
| Core pileup and CIGAR display | Complete | Pending | Qualify on a real technology and corruption matrix in Phase 9 |
| Paired and chimeric reads | Complete | Pending | Broad representative-data acceptance in Phase 9 |
| Multi-track and multi-locus | Complete | Pending | Broad release acceptance in Phase 9 |
| Alignment grouping and sorting | Complete | Pending | Broad release acceptance in Phase 9 |
| Collapsed/squished/expanded display | Complete | Pending | Broad release acceptance in Phase 9 |
| Coverage inspection and consensus | Complete | Pending | Broad real-data and release acceptance in Phase 9 |
| Sequencing-specific BAM semantics | Complete | Pending | Qualify bisulfite, PacBio kinetics, SBX, and Ultima flow-indel evidence on representative data in Phase 9 |
| VCF and genotype review | Complete | Pending | Qualify cohort and pedigree workflows in Phase 9 |
| RNA and Sashimi | Complete | Pending | Qualify representative RNA data and interaction in Phase 9 |
| Quantitative comparison | Complete | Pending | Broad representative-data and release acceptance in Phase 9 |
| Structural/interaction formats | Complete | Pending | Qualify BEDPE/interact arcs and linked endpoint panels in Phase 9 |
| Common remaining IGV data tracks | Complete | Pending | bigGenePred, bigNarrowPeak, broadPeak, nucleotide PSL, GWAS, and indexed TDF are implemented; broadly qualify them in Phase 9 |
| Genome and annotation management | Complete | Pending | Qualify portable genomes and translation in Phase 9 |
| Feature search | Complete | Pending | Qualify annotation and variant-ID navigation in Phase 9 |
| ROIs and review queues | Complete | Pending | Qualify the complete review workflow in Phase 9 |
| Sample attributes and bulk actions | Complete | Pending | Qualify the cross-type cohort workflow in Phase 9 |
| Portable sessions and recovery | Complete | Pending | Qualify relocation, recovery, and imported projects broadly in Phase 9 |
| Batch automation | Complete | Pending | Qualify reproducible clean-install review batches in Phase 9 |
| Local external control | Complete | Pending | Qualify authenticated loopback control in packaged builds in Phase 9 |
| Structured and vector export | Complete | Pending | Qualify bounded snapshots, evidence exports, and provenance in Phase 9 |
| Accessibility | Complete | Pending | Qualify platform screen readers and hands-on keyboard workflows in Phase 9 |
| Remote CRAM/reference | Complete | Targeted only | Controlled backend matrix passed; broad WAN and real-data parity remains |
| Platform packaging | Complete | Targeted only | Unsigned staged macOS app passed; signed clean-install artifacts remain |
| Reproducible IGV comparison | Planned | Pending | Matched procedures and narrow, defensible speed claims in Phase 9 |

The current IGV guide also documents bisulfite coloring, PacBio SMRT kinetics, Ultima flow-based
indel evidence, and SBX read behavior. Those are no longer treated as optional because RGV's stated
alignment goal covers short reads, long reads, and other BAMs a researcher may inspect. They must
reuse the general alignment, CIGAR, reference, tag, legend, and inspector abstractions rather than
creating separate rendering systems.

## 6. Architectural rules

Every phase must preserve these invariants:

1. Genomic intervals are zero-based, half-open internally; one-based inclusive text is converted
   only at explicit boundaries.
2. `PanelId`, `TrackId`, source identity, and generation are stable identities, never vector
   positions.
3. Every source can project independently into every visible panel while immutable decoded data is
   shared safely where possible.
4. A slow, failed, hidden, or removed track cannot blank, reorder, or block another track or panel.
5. Aggregate evidence comes from all eligible records, not merely retained display rows.
6. Query work, decoded records, memory, GPU primitives, labels, inspectors, clipboard operations,
   and exports have deterministic workspace-wide bounds.
7. Obsolete results can never replace a newer view, including after track or panel deletion.
8. Display-only changes relayout retained normalized data without re-reading the source.
9. Reference, coverage, junction, modification, alignment, annotation, and overlay layers retain
   separate semantic ownership.
10. Missing, sampled, truncated, malformed, or unsupported evidence is visible to the user.
11. Credentials, signed URLs, private sequence, and read names are excluded from persisted or
    diagnostic output unless the user explicitly exports them.
12. Unsupported future session versions are rejected before viewer state or the source file is
    mutated.
13. The UI thread performs no file, decompression, reference, or network I/O.

## 7. Delivery sequence

```text
lock the automated multi-panel baseline
  -> finish alignment-review features
  -> complete variants, genotypes, and structural navigation
  -> complete RNA review
  -> complete quantitative and interaction comparison
  -> complete genome, search, ROI, and review workflows
  -> complete metadata, sessions, recovery, automation, export, and accessibility
  -> complete remote data and packaging features
  -> close sequencing-specific BAM modes and common remaining data-track gaps
  -> freeze the major feature set
  -> perform broad manual, real-data, performance, and platform qualification
  -> public beta
```

Correctness, crash, stale-view, data-loss, credential-leak, and unbounded-work defects interrupt
feature development. Performance work interrupts feature delivery only for a serious regression
from the recorded baseline or behavior that makes development impractical.

## 8. Phase 0 — lock the automated multi-panel baseline

**Implementation: Complete. Qualification: Pending Phase 9.**

### User outcome

The multi-track and multi-locus foundation is protected well enough by automated tests and
benchmarks that the remaining major product workflows can be built on it.

### Work

- Keep formatting, full workspace tests, strict all-feature Clippy, release build, and synthetic
  validation green.
- Protect schema-v3 restoration, stable panel/track identity, independent result routing, linked
  evidence navigation, panel geometry, and reviewed multi-panel rendering with automated tests.
- Retain the 1-track x 2-panel and 4-track x 2-panel benchmark reports as a regression baseline.
- Record but do not block on interaction or loading results that are already acceptable for active
  development.

### Gate

- **Done on 22 August 2026:** the initial complete automated gate passed with 244 tests, strict
  Clippy, a release desktop build, and synthetic validation. Subsequent feature slices continue to
  rerun and extend this gate; the current count is recorded in the baseline above.
- Schema v1, v2, and v3 restoration, two-track x two-panel routing, failure isolation, stable GPU
  identity, linked navigation, common geometry, and a reviewed raster are covered.
- Workspace record, primitive, query, cache, and RSS evidence is recorded and bounded.

Thorough short-read/ONT manual acceptance and any resulting interaction refinements are deliberately
moved to Phase 9, after the major feature phases.

## 9. Phase 1 — finish alignment review

**Implementation: Complete. Qualification: Pending Phase 9.** The technology-neutral alignment
workflow and all four sequencing-specific BAM modes are implemented; their broad representative-
data acceptance remains deliberately scheduled for Phase 9.

### 1.1 Grouping, sorting, packing, and selection

- Group alignments by strand, first-of-pair strand, read group, sample, haplotype, tag, pair
  orientation, mate chromosome, and chimeric chain where applicable.
- Sort within each group at an explicit center/cursor locus by start, base, strand, mapping quality,
  insert size, mate chromosome, tag, and left/right clip length.
- Support reverse group and sort order, rerun sort at the new locus, and deterministic repacking.
- Implement collapsed, squished, and expanded density modes independently of vertical zoom.
- Add selection by read/template name, clear selection, selected-to-top, and linked-read highlighting
  across panels without persisting private names by default.
- Apply compatible settings to selected tracks or all alignment tracks as an explicit bulk action.

### 1.2 Coverage and read actions

- Provide a copyable coverage inspector with A/C/G/T/N, deletion, insertion, quality-weighted and
  unweighted totals, forward/reverse counts, and sampling provenance.
- Make the allele-fraction threshold and quality weighting explicit per track and as a default.
- Add bounded copy actions for read details, CIGAR, sequence, selected tags, reference sequence, and
  a clearly specified consensus algorithm.
- Expose view-as-pairs, mate connectors, soft clips, center line, mismatch policy, base-quality
  shading, insert-size inference, visibility window, and downsampling controls coherently.
- Keep coverage and junction visibility independent from the read-stack visibility.

### 1.3 Long-read and specialist review

- Replace the text-only supplementary list with a bounded read-order/alignment-order chain diagram.
- Navigate previous/next segment or open selected/all segments in panels, retaining chain identity.
- Finish modified-base legends, code selection, probability thresholding, strand handling,
  frequency aggregation, and malformed `MM`/`ML` behavior.
- Implement bisulfite contexts through the shared alignment/CIGAR/reference model with explicit
  conversion semantics. This is now complete; the Phase 8 record below is authoritative.
- Implement PacBio SMRT kinetic colouring only with defined tags, normalization, legends, and test
  fixtures. This is now complete; the Phase 8 record below is authoritative.

### 1.4 Prepare the real-data and failure matrix

- Pin safe manifests for Illumina, ONT, PacBio HiFi/CLR and SMRT kinetics, RNA-seq, amplicon,
  bisulfite, SBX, Ultima, single-cell tagged BAM, deep coverage, SV-heavy reads, and
  reference-dependent CRAM.
- Verify all CIGAR operations, both strands, paired states, long indels, clips, skips, supplementary
  records, missing sequence/qualities, uncommon tags, and malformed records.
- Test absent/corrupt index, truncated source, mismatched reference, reopen after error, and exit
  during active work.
- Keep the manifests, loci, truth notes, and automated fixture equivalents ready for the final
  qualification pass. The one-hour soak and complete visual review occur in Phase 9.

### Gate

- Group and sort truth fixtures produce exact row/group order.
- Coverage and consensus output match independently calculated truth.
- Every fixed issue has a minimal regression fixture or deterministic test.
- The complete hands-on technology matrix is a Phase 9 release gate rather than a blocker for
  variant, RNA, or other major feature implementation.

## 10. Phase 2 — complete variants, genotypes, and structural evidence

**Core implementation: Complete. Qualification: Pending Phase 9.** Pedigree-aware ordering will be
connected when Phase 6 introduces the shared sample-metadata model; it does not block Phase 3.

### User outcome

A reviewer can select a variant, understand its annotations and sample genotypes, filter it, and
compare it with supporting reads at every linked locus.

### Work

- Preserve typed REF/ALT, ID, QUAL, FILTER, INFO, FORMAT, genotype, phasing, symbolic allele, END,
  confidence interval, and breakend semantics.
- Render SNVs, MNVs, small and large indels, multi-allelic sites, symbolic SVs, and BNDs with
  distinct, scale-stable visual grammar.
- Add a structured, searchable, copyable inspector using header-derived descriptions.
- Virtualize genotype rows and fields so a large cohort never creates one GUI widget per sample.
- Add sample search/subset, genotype grouping, phasing display, pedigree-aware ordering when
  metadata is supplied, and genotype-aware colors.
- Add persisted filters for locus, type, QUAL, FILTER, allele frequency, genotype, and selected
  INFO/FORMAT values with explicit missing-value rules.
- Use the common panel command for BND partner, symbolic endpoint, and selected evidence navigation.
- Link variants to pileup, pair, split-read, and modification evidence without pretending to call
  or clinically interpret variants.
- Add bounded variant, genotype, and selected-evidence exports.

### Gate

- Generated truth covers all supported record forms, missing/malformed fields, phased and unphased
  genotypes, large cohorts, symbolic endpoints, and interchromosomal breakends.
- Filter output matches declared truth exactly.
- Breakend navigation preserves the origin selection and opens the correct partner panel.
- Large cohorts remain responsive and memory-bounded.

### Implementation progress

- The typed query, structural rendering, semantic selection, header-described inspector,
  virtualized genotype rows, BND navigation, and generated indexed truth corpus are implemented.
- Per-track session state now drives query-time type, QUAL, FILTER, AF, genotype, INFO, FORMAT, and
  GT filtering with explicit missing-value rules. Searched sample subsets reduce genotype decoding;
  genotype grouping and cohort-aware colours are available, with hard header/sample/cell caps and
  truncation provenance.
- The selected variant links to the current pileup/indel bin, modification co-location,
  pair/read context, and structural endpoint panels without claiming to call variants. Bounded
  variant/genotype and evidence TSV exports write outside the UI thread.
- Pedigree-aware ordering awaits the shared sample-metadata model. Final representative cohort,
  malformed-field, visual, and hands-on qualification remains part of Phase 9; this does not
  reopen or block the completed core Phase 2 implementation.

## 11. Phase 3 — complete RNA review

**Implementation: Complete. Qualification: Pending Phase 9.**

### Work

- Preserve exact junction endpoints, count, minimum flank, strand provenance, annotation status,
  filter state, and source track.
- Support collapsed and expanded junction tracks, selectable arcs, copyable details, minimum count
  and flank filters, and explicit strand inference.
- Add single- and multi-sample Sashimi views using selected alignment and annotation tracks.
- Add fixed, automatic, and shared scales plus explicit raw/normalized count provenance.
- Make junction aggregation independent of retained read sampling.
- Export deterministic Sashimi PNG/SVG and bounded junction BED.

### Gate

- Generated RNA truth gives exact, sampling-independent endpoints, strand, and counts.
- Single- and multi-sample plots preserve their scale and filters across panels and sessions.
- RNA interaction remains responsive at high junction density.

### Implementation progress

- Junction collection now preserves exact endpoints, adjacent aligned flanks, raw depth, strand
  evidence, malformed-tag provenance, filtering, source identity, and whole-file normalization
  denominators independently of retained read sampling.
- Persisted single- and multi-sample Sashimi views support collapsed and expanded layouts,
  selectable arcs, automatic/fixed/shared scales, raw/CPM counts, annotation-aware status, labels,
  and explicit fallback when normalization is unavailable.
- Bounded junction BED and deterministic Sashimi PNG/SVG exports run outside the UI thread. The
  generated RNA corpus covers tagged, inferred, unknown, malformed, filtered, and high-density
  evidence. The automated close gate passed with 266 workspace tests on 22 August 2026.
- Broad representative-data, visual, and interaction qualification remains in Phase 9 and does not
  reopen the completed implementation phase.

## 12. Phase 4 — quantitative, copy-number, and interaction comparison

**Implementation: Complete. Qualification: Pending Phase 9.**

### 4.1 Common quantitative model

- Implement automatic and fixed ranges, baseline, linear/log scale, positive/negative colors,
  missing values, and displayed range labels.
- Support bar, line, point, and heatmap styles where meaningful.
- Support minimum, mean, maximum, and raw/no-window aggregation with visible provenance.
- Add selected-track group autoscaling, overlays, separation, transparency, and bounded add,
  subtract, multiply, and divide transforms.
- Apply compatible scales consistently across tracks and panels without coupling source queries.

### 4.2 Formats in dependency order

1. bedGraph and WIG through the common quantitative model;
2. BEDPE/interact as arcs or blocks with endpoint-panel navigation;
3. SEG and multi-sample copy-number matrices with locus sorting;
4. MAF mutation tracks;
5. genePred/refFlat and narrowPeak;
6. GWAS results, promoted into the required Phase 8 boundary after the parity audit;
7. TDF compatibility, likewise promoted because archived IGV projects must open without conversion.

### Gate

- Zoomed-out aggregation is deterministic and testable.
- Group scales, overlays, transforms, and session restoration preserve exact settings.
- Remote reads are bounded and linked endpoints use the common panel mechanism.
- Large sample matrices virtualize rows and remain responsive.

### Implementation progress

- BigWig, bedGraph, and WIG now share persisted automatic/fixed/shared ranges, baseline and signed
  transforms, bar/line/point/heatmap modes, deterministic raw/minimum/base-weighted-mean/maximum
  windows, explicit missing values, colours, opacity, bounded arithmetic, exact inspection, and
  truncation provenance. Explicit persisted overlay groups share a frame, range, and transform;
  exact member selection, separation, independent scale groups, and bounded gutter provenance are
  implemented. Phase 6 will add the metadata-driven multi-track selection entry point for applying
  these existing group operations in bulk.
- BEDPE and official UCSC interact records now retain typed endpoints and optional names, strands,
  score, value, experiment, colour, and direction. Bounded arcs/blocks, filtering, exact selection,
  cross-contig markers, inspection, and linked endpoint panels are implemented.
- SEG/CBS now has a bounded multi-sample model with one-based coordinate conversion and `coords=0`
  override, marker counts, ignored-row and cap provenance, automatic log-ratio/absolute semantics,
  virtualized heatmap rows, shared/fixed scales, persisted base-weighted locus sorting, selection,
  and inspection.
- GDC/TCGA Mutation Annotation Format now has header-name parsing, stable tumour-sample rows,
  one-based coordinate conversion, plain/gzip input, bounded ordered annotations, row virtualization,
  classification/type colours, repeated-call semantics, exact selection, full inspection and
  session persistence. The `.maf` ambiguity is explicit: cancer mutation MAF is supported; UCSC
  multiple-alignment MAF is not.
- UCSC genePred/refFlat and ENCODE narrowPeak now have typed, bounded plain/gzip parsing. Gene
  models preserve transcript/gene identity, optional table bins, exon arrays, coding intervals,
  extended status/frame fields, and thick-CDS/thin-UTR rendering. narrowPeak preserves and renders
  score strength and the exact called summit while retaining signal and p/q statistics. Both use
  exact packed-row selection and a bounded copyable feature inspector.
- On 22 August 2026, `cargo fmt --all -- --check`, all 320 workspace tests, strict workspace
  all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation passed.

## 13. Phase 5 — genome, annotation, search, ROIs, and review queues

**Implementation: Complete. Qualification: Pending Phase 9.**

### Work

- Open an indexed FASTA and genome metadata without requiring an alignment.
- Support explicit canonical contig aliases without silently resolving ambiguity.
- Load cytobands and default annotations from a portable genome configuration.
- Index feature names, gene aliases, transcript IDs, and variant IDs for autocomplete.
- Present deterministic choices for ambiguous names and retain navigation history.
- Add next/previous feature actions and a structured annotation inspector.
- Keep reference as an independent pinned track with sequence copy, strand flip, configurable
  translation frames, and explicit genetic code.
- Create, edit, name, color, import, export, navigate, and delete regions of interest.
- Draw ROI boundaries across every visible track and panel.
- Add locus/gene review lists with next/previous, status, notes, optional assignment, and local
  filtering; private evidence is excluded from saved review state by default.

### Gate

- A custom genome, aliases, annotation, ROIs, and review list restore on a clean installation.
- Search results and ambiguity resolution are deterministic.
- Reference translation is verified against known coding fixtures on both strands.
- Review state does not leak read names or sequence unintentionally.

### Progress

- **Standalone FASTA slice complete (22 August 2026):** `--reference`, **Genomes → Open FASTA**,
  drag-and-drop, and saved sessions can open a FASTA/FAI without an alignment. A bounded background
  actor owns FASTA indexing and base-detail fetches, its dictionary drives contig/locus navigation,
  and the sequence renders in an independent pinned reference track with bounded sequence copy.
  Focused desktop, engine, and layout tests (159 tests) and strict desktop Clippy passed.
- **Portable genome metadata slice complete (22 August 2026):** schema-versioned genome bundles
  load a FASTA, assembly name, local cytobands, exact canonical contig aliases, and default
  annotations with bundle-relative paths. Conflicting or missing alias targets are explicit errors;
  genome metadata and aliases persist in sessions.
- **Text-annotation search slice complete (22 August 2026):** bounded background indexes cover
  feature names, gene names, aliases, IDs, and transcript IDs across text annotation tracks. The
  locus bar autocompletes deterministically, prefers a unique exact match, and presents track and
  coordinates for ambiguous choices; Regions actions move to the deterministic nearest previous or
  next feature. Submitted searches lazily build a bounded VCF/BCF record-ID index without adding a
  whole-file scan to normal startup.
- **Reference orientation/translation slice complete (22 August 2026):** reference settings persist
  forward/reverse-strand display, any combination of six absolute genomic translation frames, and
  an explicit standard or vertebrate-mitochondrial genetic code. Both strands and mitochondrial
  code differences have truth tests; visible-reference copy follows the chosen strand.
- **ROI and review-queue slice complete (22 August 2026):** named, colored, relocatable regions can
  be created, navigated, deleted, imported from BED, and exported to BED. Their clipped translucent
  spans and boundaries project across every track in every matching panel. Locus entries and
  selected gene/annotation features can be added to a bounded review queue with deterministic
  previous/next navigation, local status filtering, editable status, notes, and optional assignment.
  A versioned TSV preserves review queues for explicit interchange; both lists persist in validated
  sessions without automatically capturing read names or sequence. File reads and writes run off
  the UI thread and inputs, records, strings, notes, rendering, and background tasks are bounded.
- **Automated Phase 5 close gate passed (22 August 2026):** formatting, all 337 workspace tests,
  strict all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation
  passed. Broad hands-on product qualification remains in Phase 9 by design.

## 14. Phase 6 — workspace metadata, sessions, and recovery

**Implementation: Complete. Qualification: Pending Phase 9.**

### 6.1 Sample attributes and bulk track operations

- Import a simple sample-attribute table with explicit stable sample keys.
- Show/hide a resizable metadata panel and select tracks through attribute values.
- Bulk rename, color, height, visibility, grouping, filtering, autoscaling, overlay, and removal only
  when the selected track types support the action.
- Preserve track/sample association for the alignment, coverage, and junction projections of one
  sample.

### 6.2 Portable projects and session evolution

- Persist every analysis-changing genome, panel, track, filter, grouping, scale, ROI, review, and
  visualization setting.
- Separate global defaults, workspace overrides, and per-track overrides.
- Support relative paths rooted in an explicit project directory and report unresolved paths
  individually.
- Improve IGV XML import while reporting every ignored or approximated semantic.
- Add migration fixtures for every released schema boundary and reject future schemas safely.
- Add atomic saves, periodic opt-in autosave, clean-shutdown markers, crash recovery selection, and
  retention limits.

### Gate

- A project moves to a clean installation and reopens to the same settled state using documented
  relative-path rules.
- Simulated interrupted saves and crashes recover without corrupting the last good session.
- Bulk actions are undoable or explicitly confirmed when destructive.
- Credentials and unintended private evidence never appear in project files.

### Progress

- **Sample metadata and bulk cohort slice complete (22 August 2026):** a bounded generic TSV model
  preserves a stable sample key, up to 256 named attributes, 100,000 rows, one million cells, and a
  64 MiB source. A resizable virtualized metadata window filters by attribute values and selects
  associated alignment and auxiliary tracks across types. Every track can retain an explicit
  sample key, including unresolved keys, and the complete table and associations survive validated
  sessions.
- Metadata-selected tracks support compatible show/hide, expand/collapse, sample-key rename,
  bounded height, full alignment setting/filter/group copy, quantitative autoscale/overlay/
  separation, homogeneous auxiliary appearance/color/filter copy, and two-step confirmed removal.
  Unsupported height or appearance operations are not applied to track types whose geometry or
  settings do not support them.
- Variant genotype rows can use deterministic family/child/father/mother ordering when imported
  metadata provides recognized parent columns. Missing pedigree columns retain VCF header order
  with an explicit explanation rather than inventing relationships.
- **Portable project and recovery slice complete (22 August 2026):** project JSON files are rooted
  in their containing directory. Sources within that root persist relatively and relocate with the
  project; external sources remain canonical absolute paths. Every missing local reference,
  cytoband, alignment, or auxiliary source is reported separately while healthy tracks remain
  available. Global desktop preferences remain in the settings file, workspace state remains in
  the project, and independent track overrides remain in stable-ID track records.
- Manual project saves and opt-in periodic recovery snapshots serialize off the UI thread and use
  a flushed same-directory atomic replacement. Autosave interval and per-project retention are
  bounded. A PID-only clean-shutdown marker triggers explicit recovery selection after an unclean
  exit, and recovered state opens unsaved so it cannot overwrite the original accidentally.
- Ordered loci now persist independent read-stack scroll positions in addition to their cameras.
  Checked-in version-1, version-2, and version-3 fixtures cover every released schema boundary; a
  future-version fixture proves safe rejection. Relocation, repeat replacement, failed-save
  last-good preservation, retention scoping, opt-in defaults, and credential redaction have direct
  automated coverage.
- IGV XML resources resolve relative to the XML directory. Supported locus, genome/reference,
  resource name/path, track name/visibility/height, and alignment coverage/junction visibility are
  translated. A bounded visible report lists every unique imported, approximated, and ignored XML
  semantic category rather than silently claiming renderer equivalence.
- **Automated Phase 6 close gate passed (22 August 2026):** formatting, all 351 workspace tests,
  strict all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation
  passed. Broad portable-project, crash, and clean-install acceptance remains in Phase 9 by design.

## 15. Phase 7 — automation, export, and accessibility

**Implementation: Complete. Qualification: Pending Phase 9.**

### 7.1 Deterministic automation

- Define a versioned batch format shared by CLI execution and an optional local command service.
- Support new workspace, genome, load/remove track, goto one/many loci, add/close panel, set option,
  group, sort, filter, select, wait until settled, snapshot, structured export, save, and exit.
- Define settled state from generations, queues, uploads, fonts, and layout rather than sleeping.
- Return structured results and stable error codes.
- Keep external control disabled by default; when enabled, bind locally and require an explicit
  session secret or equivalent protection for mutating commands.

### 7.2 Export

- Keep deterministic viewport PNG and add faithful SVG for supported semantic layers.
- Support exact visible-area and bounded full-panel snapshots.
- Add bounded or streamed exports for alignments, pileup, selected evidence, modifications,
  junctions, variants, quantitative windows, ROIs, and track names.
- Record source-safe provenance: RGV build, genome, loci, tracks, filters, scales, sampling, and
  truncation without embedding credentials or private sequence unexpectedly.

### 7.3 Accessibility

- Complete keyboard operation for tracks, panels, overview bars, inspectors, dialogs, menus,
  metadata, ROIs, and review lists.
- Add accessible names, values, focus state, selection state, and asynchronous error announcements.
- Validate focus order, high contrast, text scaling, color independence, reduced motion, and the
  screen-reader behavior supported by the desktop stack.

### 7.4 Data access and distributable application

- Complete observable, bounded remote BAM/CRAM access with BAI, CSI, CRAI, redirects, custom
  indexes, authentication, cancellation, cache invalidation, and explicit reference handling.
- Preserve local/remote semantic parity and report partial, corrupt, changed, or unsupported data
  without blanking unrelated tracks.
- Produce the macOS, Windows, and Linux package definitions, license bundle, checksums, SBOM, and
  build provenance needed for later release qualification.

### Gate

- Automation reconstructs a multi-track, multi-panel review and produces matching evidence on a
  clean installation.
- Repeated settled snapshots are byte-identical where the format permits.
- All core review workflows can be completed without a mouse.
- Remote sources expose bounded request, byte, cache, retry, and cancellation behavior.
- Installable artifacts can be produced for every intended beta platform; signing, clean-install,
  hardware-matrix, and soak qualification remain Phase 9 work.

### Implementation progress

- **Versioned batch/executor foundation complete (22 August 2026):** `rgv-core` defines a bounded
  schema-versioned command and structured-result model with credential-redacted serialization and
  batch-relative input/output resolution. The native desktop executor runs commands in order,
  waits for actual background source readiness, routes asynchronous file completions by command
  identity, stops at the first stable error, and atomically writes its report before an automated
  exit.
- Settled state now compares completed alignment, reference, auxiliary, and panel generations with
  the active generation in addition to file tasks, layout, and rendered-display dirtiness. It does
  not use a guessed loading sleep or accept an old same-locus frame after a filter change.
- Visible-workspace, visible-active-panel, and 16,384-logical-pixel bounded offscreen full-panel
  PNG/SVG snapshots; bounded alignment, pileup, modified-base, junction, variant/genotype,
  quantitative-window, selected-evidence, region BED, and track-name exports; and project saves are
  connected. A checked-in nineteen-command release-mode two-panel BAM batch completed repeatedly
  with byte-identical visible SVG SHA-256
  (`ea837d45815dba8ed16e73f3ed65f536c6010a9b9fcb76e534703033ec4d54f9`) and full-panel SVG
  SHA-256 (`027ecb709c3432fc93db082ffe00154c797cf976054ab3bf7cfcc6e5e57574df`).
- **Automation/export close gate passed (22 August 2026):** formatting, all 363 workspace tests,
  strict all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation
  passed.
- **Safe local control implemented (22 August 2026):** external control is disabled by default and
  can be enabled only with an explicit loopback port and protected secret file. Authenticated,
  bounded HTTP requests reuse the versioned batch vocabulary and real settled executor, serialize
  command batches, return structured reports, constrain local paths to a selected root, and shut
  down with the window. The release application rejected an unauthenticated mutation with HTTP 401,
  then completed an authenticated navigate/settle/export/exit batch end to end.
- **Local-control close gate passed (22 August 2026):** formatting, all 370 workspace tests, strict
  all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation passed.
- **Accessibility implementation complete (22 August 2026):** the native AccessKit window
  integration is enabled. Persistent 75–200% interface scaling, increased contrast, reduced motion,
  accessible asynchronous status events, named/value-bearing custom overview controls, visible
  focus outlines, keyboard overview navigation, track management, loaded-evidence traversal,
  supplementary-chain navigation, and command-search access to panels, metadata, ROIs, and review
  queues are implemented. The focus/action/name/contrast audit covered every custom pointer control,
  inspector, and specialist dialog; previously unnamed metadata, ROI, review, track-setting, and
  variant controls now have explicit semantics. Automated tests inspect the actual AccessKit nodes
  for the overview sliders, selected track, resize handle, status, and supplementary segments and
  verify scaling, contrast, reduced motion, keyboard ordering, and settings compatibility.
- **Accessibility close gate passed (22 August 2026):** formatting, all 379 workspace tests, strict
  all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation passed.
  Platform screen-reader and broad hands-on keyboard qualification remain Phase 9 work.
- **Remote data implementation complete (22 August 2026):** remote BAM and reference-dependent
  CRAM now share the pure-Rust bounded range path with observable CRAM, CRAI, FASTA, and FAI
  traffic. Both conventional index filename forms and custom signed index URLs are supported.
  Exact range geometry, response sizes, validators, changed objects, bearer authentication,
  credential-safe errors, bounded redirects, HTTPS downgrade rejection, transient probe/data
  retries, disk-backed LRU caching, generation cancellation, and request/byte/cache/retry/
  cancellation diagnostics are implemented. CRAM decoder reference state is capped at 512 MiB per
  query and cleared on completion or error. A generated reference-dependent CRAM fixture verifies
  remote/local core alignment, CIGAR, pileup, junction, and pair parity; malformed CRAI, signed
  queries, custom/conventional indexes, and authenticated remote FASTA are covered.
- **Post-freeze desktop-access audit (23 August 2026):** remote BAM/CRAM, FASTA, indexed VCF/BCF,
  BigWig, TDF, BigBed, bigGenePred, and bigNarrowPeak sources now have a first-class **File → Open URL** dialog
  rather than requiring CLI, automation, or a prepared project. It accepts custom indexes and an
  optional CRAM reference, persists a distinct index URL for each alignment plus the reference
  FAI, and redacts signed queries from names and status. The same audit fixed the advertised
  `.bed.gz`, `.gff3.gz`, and `.gtf.gz` desktop inference paths and signed-URL format inference.
- **Distributable package implementation complete (22 August 2026):** one standard-library-only
  staging path produces and verifies macOS app, Linux tree, and Windows portable layouts. The
  development workflow now emits an app ZIP, Linux tarball, Windows ZIP/MSI, and their package
  metadata instead of raw binaries. Signed workflows reuse the same layouts for DMG, MSI, tarball,
  and AppImage production. Every package carries the dual RGV licenses, full deduplicated
  third-party license/attribution texts, a locked-graph CycloneDX 1.6 SBOM, build provenance, and an
  internal SHA-256 manifest; final release checksums and GitHub attestations cover signed artifacts.
- **Phase 7 close gate passed (22 August 2026):** formatting, all 390 Rust workspace tests, four
  packaging tests, strict all-target/all-feature Clippy, the release desktop build, verified
  unsigned macOS staging, and synthetic-corpus validation passed. Signing, clean installation,
  broad WAN/real-data checks, screen readers, and hands-on product acceptance remain Phase 9 work
  by design.

## 16. Phase 8 — close the remaining major feature gaps

**Implementation: Complete and feature-frozen on 23 August 2026. Qualification: Pending Phase 9.**

The earlier roadmap moved into broad qualification too soon. The current guide and the repository
prove that the technology-neutral alignment core and Phases 2--7 are substantial, but they do not
prove that every major feature in the agreed product boundary exists. Phase 8 closes that gap
before asking the user to perform a broad manual pass.

### 8.1 Sequencing-specific BAM semantics

These modes are required because RGV promises useful inspection of short-read, long-read, and other
BAM/CRAM data, not merely the ability to decode their generic SAM fields.

1. **Complete on 22 August 2026 — bisulfite and related conversion assays:** persisted CG, CHG,
   CHH, HCG, GCH, and WCG
   contexts; reference-aware top/bottom-strand interpretation; protected/converted base colouring
   at both base-detail and zoomed-out scales; first-of-pair strand body colours; explicit missing-
   reference behavior; a copyable legend and inspector; and exact forward/reverse truth fixtures.
   Biological classification lives in `rgv-hts`; the existing bounded CIGAR renderer draws exact
   base marks and read-scale lines, reports omitted marks at its cap, requests reference context at
   read detail, and leaves reads visible with an explicit unavailable state when FASTA is absent.
   Its feature close gate passed on 22 August 2026: formatting, all 407 workspace tests, strict
   all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation passed.
2. **Complete on 22 August 2026 — PacBio SMRT kinetics:** persisted subread/CLR IPD and pulse-width
   modes decode bounded `ip` and `pw` integer arrays; four CCS modes select `fi`, `ri`, `fp`, or
   `rp` according to the requested molecule strand and the record's alignment orientation. Arrays
   are indexed in the same reference-forward orientation as the CIGAR walk. The renderer applies
   the documented 0–100 red-opacity, 100–600 red-to-yellow, and 600–6000 yellow-to-cyan scale at
   both read and base detail through the shared display list, with a separate 50,000-mark cap.
   Missing sequence/tags and malformed type, negative, over-bound, or length-mismatched arrays
   leave reads visible and produce exact gutter/inspector status. Sessions, a copyable legend,
   physical-tag/value summaries, forward/reverse truth tests, and generated long-read BAM truth
   arrays cover the complete path. Its feature close gate passed on 22 August 2026: formatting, all
   415 workspace tests, strict all-target/all-feature Clippy, the release desktop build, and
   synthetic-corpus validation passed.
3. **Complete on 22 August 2026 — Ultima flow-based evidence:** an explicit persisted mode decodes
   bounded signed `tp:B:c` direction arrays, base qualities, printable Phred+33 `t0` deletion
   qualities, and bounded read-group `PL`/`mc` metadata. Directional and minimum-homopolymer
   quality methods are selectable, and an explicit track override can replace `mc`. Insertion and
   deletion confidence uses the 44-step Q0–Q42 jet scale plus orthogonal marker position, so colour
   is not the only channel. Exact signed direction, quality, source, homopolymer base, effective
   `mc`, and unavailable reasons appear in the inspector; missing/malformed records and unsupported
   events leave ordinary indels visible and are counted in the gutter. Reverse orientation,
   fallback and symmetric adjustment, reference-dependent deletions, maximum-homopolymer Q0,
   scale-stable read/base rendering, layout-cache identity, and a separate 50,000-mark cap have
   exact tests. The pinned indexed BAM/FASTA truth locus requires directional insertion Q40,
   read-group-`mc` deletion Q0, and `t0` deletion Q35 through the production query path, and a
   reviewed raster protects their visual grammar. Its feature close gate passed on 22 August 2026:
   formatting, all 442 workspace tests, strict all-target/all-feature Clippy, the release desktop
   build, and synthetic-corpus validation passed.
4. **Complete on 22 August 2026 — SBX evidence:** an explicit persisted mode classifies insertion
   markers as concordant, partly discordant, or fully discordant from inserted, adjacent, and
   containing-homopolymer base qualities. Configurable discordant and simplex-tail thresholds are
   visible beside a copyable legend; contiguous low-quality tails can be highlighted, hidden, or
   left unchanged at read and base detail. CIGAR clipping, deletion boundaries, base and modified-
   base overlays, reverse-aligned quality orientation, all-tail reads, missing/malformed qualities,
   inspector summaries, gutter status, layout-cache identity, and a separate 50,000-tail-mark cap
   use the shared alignment path. The regenerated BAM truth corpus contains exact full, partial,
   and concordant insertions with known tails and the normal validation runner requires that
   evidence. Its feature close gate passed on 22 August 2026: formatting, all 427 workspace tests,
   strict all-target/all-feature Clippy, the release desktop build, and synthetic-corpus validation
   passed. No instrument type is inferred from qualities; users select the mode explicitly.

All four modes reuse the existing alignment batch, CIGAR walk, reference slice, auxiliary-tag,
display-list, picking, session, and export paths. They may not fork an independent renderer or
silently infer an experiment type from unreliable evidence.

### 8.2 Common remaining IGV data tracks

Implement the remaining broadly useful formats that users are likely to encounter in ordinary IGV
projects:

1. **Complete on 22 August 2026 — `bigGenePred` and `bigNarrowPeak`:** canonical 20-field
   BED12+8 and ten-field BED6+4 schemas run through the existing local/HTTP indexed BigBed reader
   without eager loading. Transcript blocks, coding intervals, strand, score, identities, CDS
   states, exon frames and types feed the typed gene model; signal, p/q values and absolute summit
   feed the typed peak model. Both therefore reuse deterministic layout, CDS/UTR or summit
   rendering, selection, inspection, and export. Declared field counts, per-record bytes, integer
   scores, coordinates, arrays, statuses, frames, peak values, query counts, bounded name search,
   and previous/next navigation fail explicitly at their limits. Dedicated CLI flags and long
   extensions cover file opening and drag/drop, while an explicit session format preserves an
   ambiguous `.bb` schema. Generated indexed binary truth covers both formats, malformed indexed
   records and schema mismatch. The close gate passed with formatting, all 449 workspace tests,
   strict all-target/all-feature Clippy, the release desktop build, and synthetic validation.
2. **Complete on 22 August 2026 — text `broadPeak` and nucleotide PSL:** bounded plain/gzip text
   parsing uses the existing annotation model and background query path. `broadPeak` preserves
   BED6+3 score, strand, signal, and optional p/q statistics without inventing a point summit. PSL
   accepts the canonical 21 columns, optional leading table bin, and standard headers; validates
   spans, insertion counts, block arrays, query/target orientation, and reverse-coordinate rules;
   and rejects translated-protein PSL explicitly. Both formats support file dialog, drag/drop,
   CLI, session restoration, search, previous/next navigation, selection, exact inspection,
   export, and collapsed/squished/expanded display. Generated semantic and malformed fixtures cover
   forward and reverse PSL blocks, peak values, compressed input, headers, array bounds, and the
   unsupported protein case. The close gate passed with formatting, all 458 workspace tests,
   strict all-target/all-feature Clippy, the release desktop build, and regenerated synthetic
   validation.
3. **Complete on 23 August 2026 — GWAS association summaries:** flexible, case-insensitive
   IGV/PLINK header aliases locate chromosome, one-based position, identifier, and p-value columns
   in plain or gzip text while retaining every bounded source column and the original p-value
   spelling. Missing p-values are counted and ignored; malformed coordinates, probabilities,
   schemas, rows, fields, and resource excess fail explicitly. The typed source converts to
   zero-based positions and `-log10(p)`, preserves zero-p values as exact positive infinity, and
   caps drawing safely. Queries above 250,000 associations keep the strongest point from each
   stable positional chunk with visible provenance. A first-class Manhattan layout supports
   two-dimensional point selection, deterministic chromosome, alternating, and uniform colours,
   point-size and vertical-maximum controls, exact inspection, copied/structured selected evidence,
   identifier search, previous/next navigation, CLI, file/drop inference, and project persistence.
   Parser, gzip, malformed, bounding, search/navigation, layout identity, colour, picking,
   inspector, CLI, and session truth are covered; the text fuzz target includes this source. The
   close gate passed with formatting, all 470 workspace tests, strict all-target/all-feature
   Clippy, the release desktop build, and regenerated synthetic validation.
4. **Complete on 23 August 2026 — TDF:** versions 1 through 4 open as read-only quantitative
   sources through local file or bounded HTTP range access. The reader loads only the fixed header,
   bounded master index, selected dataset descriptor, and overlapping tiles; it does not eagerly
   read the source. Compressed and uncompressed fixedStep, variableStep, BED, and named-BED tiles
   feed the existing quantitative range, windowing, display, overlay, arithmetic, selection,
   inspector, and export paths. Resolution selection chooses a bounded raw or precomputed dataset,
   non-finite values remain missing with provenance, and a query retains at most 250,000 records.
   Version, genome, data type, dataset, window function, compression, embedded channel, channel
   index, and channel count remain visible. Multi-channel sources expand asynchronously into one
   stable, independently configurable quantitative track per embedded channel in source order.
   They share one reader and bounded decompressed-tile cache, and exact channel indices persist so
   project restoration neither guesses nor duplicates channels. Distinct signals are never averaged.
   CLI, file dialog, drag/drop, automation inference, IGV/native project restoration, malformed and
   resource-limit behavior, binary fuzzing, generated compressed/uncompressed truth, every tile
   encoding, local HTTP ranges, and engine/desktop boundaries are covered. The standard gate passed
   with formatting, all 483 active workspace tests, strict all-target/all-feature Clippy, the
   release desktop build, and regenerated synthetic validation; a separate opt-in test passed
   against IGV's official public compressed version-4 fixture.

Every format must work through file dialog, drag-and-drop, command line where appropriate, project
save/restore, deterministic layout, selection/inspection, explicit errors, and bounded background
I/O. Indexed formats must not be eagerly loaded in full.

### 8.3 Feature-freeze gate

- Maintain a checked-in support matrix that distinguishes complete, deliberately unsupported, and
  not-yet-implemented behavior; file extensions alone are not evidence of support.
- Add generated truth and malformed-input coverage for every new semantic mode and parser.
- Rerun formatting, all workspace tests, strict all-feature Clippy, the release desktop build, and
  synthetic validation after each independently reviewable slice.
- Perform only focused visual checks needed to establish a concrete new renderer behavior. Do not
  start the broad ordinary-use, soak, platform, or matched-performance pass yet.
- Freeze the major feature boundary only when every Phase 8 item is implemented, documented,
  persisted, and covered automatically. Phase 9 then begins with the user's broad manual pass.

**Feature-freeze audit passed on 23 August 2026:** `docs/FORMAT_SUPPORT.md` was checked against the
source kinds, desktop inference/CLI paths, session import and restoration, inspectors, exports, and
generated parser truth. Every item in sections 8.1 and 8.2 is implemented and no format inside the
agreed major boundary remains marked planned. Multi-channel TDF expansion and exact persisted
channel identity are covered explicitly; the separate analysis products and specialist formats in
section 8.4 remain deliberate deferrals. A subsequent invariant audit corrected auxiliary-track
removal so the bounded actor, every panel, stale result generations, search/selection state,
quantitative operands, and shared TDF-source lifetime all follow the same stable-ID deletion.
Alignment-track deletion now applies the same all-panel identity cleanup, promotes retained primary
queries in every locus, rebalances surviving queries, and retires cancelled BAM/CRAM actors away
from the native event thread. Hidden auxiliary tracks are now excluded from actor viewport queries,
except when a visible quantitative arithmetic track requires one as an operand; visibility changes
invalidate unread auxiliary and alignment results in every affected panel. Packaged automation
evidence now has a strict verifier for exact command correspondence, artifact hashes, saved project
semantics, and native telemetry, including rejecting tamper tests. The earlier 493-test close gate passed
on that audited tree. The major feature
boundary is therefore frozen and broad qualification may begin.

### 8.4 Deliberate non-major boundary

The following are useful IGV capabilities, but they are separate analysis products, hosted
services, or specialist/legacy formats outside RGV's current native genome-review boundary: BLAT
and motif-search services, bundled igvtools transformations, hosted genome/track catalogues, Hi-C
matrices and circular views, multiple-alignment MAF, RNA secondary-structure/chemical-reactivity
tracks, Birdsuite/GISTIC/LOH specialist formats, a public plugin API, and clinical interpretation.
They remain visible deferrals rather than being misreported as completed features. Evidence of
ordinary user demand can promote any of them into a later feature phase.

### Phase 8 implementation order

1. **Done:** bisulfite mode.
2. **Done:** PacBio SMRT kinetics.
3. **Done:** SBX evidence.
4. **Done:** Ultima flow-indel evidence.
5. **Done:** bigGenePred and bigNarrowPeak.
6. **Done:** broadPeak and nucleotide PSL.
7. **Done:** GWAS.
8. **Done:** TDF.
9. **Done:** feature-support audit and automated feature-freeze gate.

## 17. Phase 9 — manual, performance, platform, and release qualification

**Qualification: ready for the user's broad manual pass.** The 23 August 2026 post-freeze desktop
access/persistence audit found and corrected opening gaps for remote sources and advertised
compressed annotations, then passed the complete 503-test correctness gate, strict Clippy, release
build, and pinned semantic corpus. A controlled backend matrix, headless baselines, and an unsigned
macOS package check were completed early and remain valid evidence; they are not a reason to delay
the user's pass or tune acceptable load times.

The concise evidence/gap audit is maintained in
`benchmark-results/PHASE9_QUALIFICATION_STATUS.md`.

### 9.1 Backend qualification

- Qualify local BAM/CRAM and remote BAM/CRAM with BAI, CSI, CRAI, redirects, custom indexes,
  authentication, interrupted requests, validators, changed objects, and partial/corrupt data.
- Verify normalized local/remote semantic equivalence for every mutually supported format.
- Test latency, loss, retries, cancellation, cache invalidation, disk limits, and reopen recovery.

**Controlled matrix passed on 22 August 2026:** local BAM BAI/CSI, local reference-dependent
CRAM/CRAI, remote BAM with both BAI and CSI conventional filename forms plus custom indexes, and
remote CRAM with both CRAI filename forms plus a custom index are covered. Authenticated signed
remote FASTA/FAI, local/remote normalized parity, redirects, transient failures, cancellation
before and during a request, validators, short bodies, corrupt/truncated sources and indexes,
mismatched CRAM references, bounded disk eviction, and reopen after object replacement passed.
Qualification found and fixed a leaked disk-cache eviction file/counter and a null-header panic on
severely truncated indexed BAM. The exact controlled evidence is recorded in
`benchmark-results/BACKEND_QUALIFICATION.md`; the pinned private real-data corpus and controlled
external-WAN run remain before this subsection can be marked Passed.

### 9.2 Performance program

- Keep single-panel benchmarks and add fixed 1x2, 4x2, 8x2, and deep long-read workloads.
- Measure startup, time to first useful partial frame, time to complete frame, cached interaction,
  relayout-only settings, selection, export, RSS, GPU resources, records, primitives, labels,
  requests, and transferred bytes.
- Profile and optimize cold and warm paths only after the major feature set is present, so effort is
  spent on the workloads the finished product actually performs.
- Retain raw samples and machine-readable threshold reports.

**Headless pre-freeze baseline recorded on 22 August 2026:** fixed 1x1, 1x2, 4x2, and 8x2
short-read workloads plus the user's real ONT BAM now retain all ten raw samples, source SHA-256,
loci, workspace budgets, cache hits, records, primitives, modification counts, and RSS. The runner
also measures vertical-density and selected-to-top relayout entirely from retained queries. Every
cached matrix entry passed the 16 ms median and 33 ms p95 budgets; no cold-path tuning was
performed. Exact reports and the summary are under `benchmark-results/`.

**Post-freeze packaged presentation slice passed on 23 August 2026:** the exact executable inside
the verified unsigned arm64 `.app` completed the nineteen-command two-panel batch through Metal.
All commands and structured exports succeeded, reviewed SVG hashes were unchanged, and 31 presented
frames measured 7.19 ms median, 10.24 ms p95, and 23.14 ms maximum with no dropped samples,
truncated frames, or GPU buffer reallocations. Startup to the first presented and first settled
frames measured 1.68 s and 1.82 s. These observations are retained rather than tuned speculatively;
ordinary packaged pan/zoom, explicit export timing, longer soak/resource sampling, and matched IGV
measurements remain.

### 9.3 Matched IGV comparison

- Pin exact RGV and IGV builds, hardware, OS, source checksums, loci, viewport, track settings,
  warm/cold state, automation, and measurement definitions.
- Compare startup, first useful frame, cached pan/zoom, deep-locus interaction, memory, remote
  requests/bytes, and export time.
- Publish only reproducible, bounded claims such as “RGV p95 cached pan was X under protocol Y.”
  Do not publish an unqualified “faster than IGV” claim.

### 9.4 Platform and packaging

- Test declared macOS, Windows, and Linux releases across integrated/discrete GPUs, Retina and
  fractional scaling, mouse, trackpad, keyboard, multiple displays, sleep/wake, and device loss.
- Run repeated open/close, shutdown-under-load, resize, and one-hour soak scenarios.
- Produce signed/notarized packages, checksums, license bundle, SBOM, and provenance.
- Verify clean install, upgrade, schema migration, cache location, uninstall, and failure recovery.

**Current-platform post-freeze unsigned package passed on 23 August 2026:** all four host-independent
packaging tests passed, the fresh release arm64 binary staged through the common macOS path, and its
manifest, SBOM, and provenance were verified both before and after sealing and unpacking the
13,668,041-byte archive. The executable inside that exact `.app` completed the nineteen-command
two-panel automation batch through Metal with the reviewed deterministic SVG hashes. The exact
archive and binary digests, native presentation telemetry, and machine-verified command, artifact,
project, and frame evidence are in
`benchmark-results/PACKAGE_QUALIFICATION.md`. This is not a signing, notarization, clean-install,
Windows, or Linux pass; those exact-artifact gates remain.

### 9.5 Broad manual product qualification

- Exercise every major workflow in the release build with representative Illumina, ONT, PacBio,
  RNA, variant, quantitative, annotation, deep-coverage, and malformed datasets.
- Review navigation feel, visual clarity, inspectors, track controls, multi-track/multi-panel
  behavior, sessions, recovery, automation, exports, and accessibility as one coherent product.
- Record each finding, reduce it to an automated fixture where practical, fix it, and repeat the
  affected workflow.
- Run the long-duration soak only after the major features are present.

**Ready to start:** `benchmark-data/demo-tracks/rgv-demo-project.json` is the canonical first
launch. It opens the checked-in alignment/reference truth with 19 auxiliary tracks spanning the
implemented format families. `rgv-alignment-review-project.json` is the second launch: four panels
and a six-item Review queue expose paired, deep, RNA, structural-pair, long/specialist, and
pathological-CIGAR truth. The deterministic generator verifies both projects, their source
resolution, the alignment and reference, both TDF channels, non-empty production-engine queries
for every auxiliary track, and the named alignment semantics. `validation/MANUAL_QUALIFICATION.md`
provides the short launch sequence, user-led data matrix, complete workflow checklist, safe finding
record, and retest rule. It explicitly
treats roughly 500 ms as acceptable unless the interaction feels wrong and prioritizes incorrect
meaning, crashes, stale evidence, data loss, credential leakage, and unbounded work over
speculative tuning. The exact project also passed a focused native desktop settle, inventory-export,
SVG-render, and exit smoke before handoff.

The companion alignment project also restored all four loci, settled, exported its track identity
and visible-workspace SVG, and exited successfully through native Metal before handoff.

### Public beta gate

- Phases 0, 1, and 2 are Done.
- Phase 3 is Done if RNA support is advertised.
- The Phase 8 major-feature boundary is implemented and frozen.
- Common quantitative controls, standalone genome loading, search, ROIs, portable sessions,
  deterministic automation, and accessibility smoke tests pass.
- No open correctness, crash, stale-view, data-loss, credential-leak, or unbounded-work defect
  remains.
- The exact release artifacts pass their declared backend, real-data, platform, performance, and
  packaging matrices.

## 18. Crate ownership for future work

| Area | Responsibility |
| --- | --- |
| `rgv-core` | Stable genomic models, coordinate types, track/panel identity, settings, session schemas, command and export models |
| `rgv-hts` | Local/remote source access, indexes, decoding, normalization, reference resolution, typed track readers, I/O metrics |
| `rgv-engine` | Bounded scheduling, cancellation, generation routing, source caches, aggregation, filtering, panel projections |
| `rgv-layout` | Deterministic packing, grouping, sorting, density modes, shared scales, track and panel geometry |
| `rgv-render` | Backend-neutral semantic display lists, clipping, picking identity, text policy, raster/vector render behavior |
| `rgv-desktop` | Window lifecycle, menus, dialogs, input gestures, inspectors, accessibility, project orchestration |
| benchmark crates | Reproducible latency, memory, I/O, scale, regression thresholds, and IGV comparison protocols |

Do not move genomic interpretation into desktop widgets or raw file parsing into the renderer.
Every asynchronous result crossing a crate boundary carries source, track, panel, interval, and
generation identity sufficient to reject stale or misrouted work.

## 19. Verification matrix

Every implementation slice uses the smallest applicable layers:

- unit tests for coordinate conversion, parsers, filters, grouping, sorting, aggregation, settings,
  schemas, and deterministic budgets;
- property tests for interval algebra, identity-preserving reorder, aggregation, and migration;
- malformed-input and fuzz tests for session, locus, CIGAR, auxiliary tags, VCF, annotation, and
  remote response boundaries;
- integration tests for engine routing, cancellation, cache behavior, failures, and source parity;
- deterministic display-list and reviewed raster tests for visible semantics;
- optimized benchmarks for performance-sensitive paths;
- targeted release-build checks when a concrete visual or interaction defect cannot be established
  confidently through deterministic tests; and
- clean-machine tests for packages, projects, credentials, and migrations.

The standard automated close gate is:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p rgv-desktop
cargo run --release -p rgv-validation-runner -- validation/synthetic-corpus.json
```

Feature slices that alter a measured hot path run the relevant regression benchmark; ordinary
feature slices do not stop for speculative optimization. Visible changes require a deterministic
render test where practical. The broad manual and performance release pass occurs after the major
feature phases.

## 20. Phase 9 performance and resource targets

These are qualification targets, not feature-development tasks. They are measured and tuned in
Phase 9 on named hardware and a pinned source/locus manifest. Before then, performance work is
limited to preventing a serious regression or fixing behavior that makes the application unusable.
An existing result that remains usable, such as the current 563.96 ms four-track/two-panel complete
frame, does not block feature delivery merely because it has not reached a final target.

| Metric | Beta target |
| --- | ---: |
| Cached pan/zoom median | at most 16 ms |
| Cached pan/zoom p95 | at most 33 ms |
| Ordinary local first useful frame | at most 100 ms |
| Controlled warm remote first useful frame | at most 500 ms |
| UI-thread file/network operations | zero |
| Obsolete result replacing active state | zero |
| Unexplained empty retained frame | zero |
| Pending-work visual frame | 16.7 ms target, 33 ms p95 ceiling |
| Memory, records, primitives, labels | explicit deterministic workspace caps |
| Inspector/clipboard previews | explicit deterministic caps |
| Large structured exports | bounded or streamed |

Each benchmark report records build ID, safe source checksum, loci, panel/track count, viewport,
settings, OS/hardware, warm/cold state, retained records, primitives, cache state, native RSS, and
I/O counters where relevant.

## 21. Immediate implementation queue

Work in this order. Each numbered item must be independently testable and reviewable.

1. **Done:** rerun the complete automated gate after the multi-panel benchmark refactor.
2. **Done:** complete the shared alignment group, sort, independent direction, and persisted density
   models with backward-compatible session defaults and SAM-tag validation.
3. **Done:** implement centre-base, strand, first-of-pair-strand, read-group, haplotype, custom-tag,
   pair, clip, name, length, quality, and position ordering, independent reversal, and deterministic
   repacking in `rgv-layout` without changing source or record identity.
4. **Done:** add the desktop controls, bulk-compatible appearance propagation, session persistence,
   per-panel centre-base behavior, cache identity, and deterministic tests.
5. **Done:** complete copyable coverage details and bounded consensus/read/reference actions. The
   read inspector copies details, CIGAR, reference-forward sequence, all loaded tags or one tag,
   and the visible reference; the pileup inspector copies the independently loaded reference beside
   its declared quality-weighted consensus. Sequence and tag payloads have explicit limits and
   truncation disclosure.
6. **Done:** build the long-read supplementary chain diagram and selected-to-top linked evidence
   behavior. The inspector now has clickable read-coordinate and genomic-order lanes, exact details,
   previous/next and active/all-panel navigation, exact loaded-name selection, and an ephemeral
   selected-to-top override ahead of configured sorting. The override participates in row and
   layout-cache identity but neither it nor the private read name is persisted or diagnosed. On 22
   August 2026, formatting, all 251 workspace tests, strict all-feature Clippy, the release desktop
   build, and synthetic validation passed.
7. **Done:** implement the typed VCF/genotype model and truth corpus for Phase 2. Indexed VCF/BCF
   queries now retain bounded typed headers, REF/ALT, FILTER, INFO/FORMAT, genotype allele indices
   and phasing, symbolic structural classes, declared endpoints, and BND partner orientation. The
   desktop has stable type rendering, semantic selection, a copyable header-described inspector,
   virtualized sample-searchable genotype rows, and linked-panel BND navigation. An indexed
   generated BCF fixture covers small variants, symbolic SVs, multi-allelic records, missing values,
   filters, typed INFO/FORMAT, phased and unphased genotypes, and interchromosomal BNDs. On 22 August
   2026, formatting, all 254 workspace tests, strict all-feature Clippy, the release desktop build,
   and synthetic validation passed. At that checkpoint Phase 2 still required filters, cohort
   grouping, evidence links, and bounded exports; item 8 below subsequently closed those tasks.
8. **Done:** implement the persisted Phase 2 cohort filters, sample subset, genotype grouping and
   colours, evidence links, and bounded exports. Filters run before the record cap and cover type,
   QUAL, FILTER, AF, genotype, INFO, FORMAT, and GT with explicit missing rules. Sample lookup is
   linear and bounded; selected evidence exports are background-written and joined on shutdown. On
   22 August 2026, formatting, all 260 workspace tests, strict all-feature Clippy, the release
   desktop build, and synthetic validation passed.
9. **Done:** complete the Phase 3 RNA and Sashimi implementation. Exact sampling-independent
   junction evidence, flank and count filters, strand and annotation provenance, persisted
   single/multi-sample layouts and scales, and bounded BED/PNG/SVG exports are implemented. On 22
   August 2026, the automated close gate passed with 266 workspace tests.
10. **Done:** complete Phase 4: the common quantitative model and overlays, arithmetic composition,
    BEDPE/interact, SEG/CBS, cancer mutation MAF, genePred/refFlat, and narrowPeak. The final format
    slice added bounded plain/gzip parsing, coding/UTR and summit rendering, exact packed-row
    selection, feature inspection, file/CLI/session integration, and fuzz coverage. On 22 August
    2026, the automated close gate passed with 320 workspace tests.
11. **Done:** complete Phase 5. Standalone FASTA/genome bundles, explicit aliases, default
    annotations, bounded annotation and variant-ID search, ambiguity handling, previous/next
    feature navigation, independent reference orientation and six-frame translation, editable BED
    ROIs, cross-panel overlays, and locus/gene review queues with status, notes, assignment,
    filtering, navigation, session persistence, and versioned interchange are implemented. On 22
    August 2026, the automated close gate passed with 337 workspace tests.
12. **Done:** complete Phase 6: sample metadata, bulk actions, portable projects, atomic background
    saves, bounded opt-in autosave, recovery selection, schema fixtures, and explicit IGV-import
    reporting. On 22 August 2026, the automated close gate passed with 351 workspace tests.
13. **Done:** complete Phase 7. Deterministic batch automation, structured/vector exports, safe
    opt-in local control, accessibility, observable bounded remote BAM/CRAM/reference access, and
    reproducible self-describing package production are implemented. On 22 August 2026, the
    automated close gate passed with 390 Rust tests plus four packaging tests.
14. **Done:** bisulfite, SMRT kinetics, SBX, Ultima, bigGenePred, bigNarrowPeak, broadPeak,
    nucleotide PSL, GWAS, and TDF are implemented with generated semantic truth, malformed-input
    behavior, persistence, desktop integration, and applicable automated gates. The feature-support
    audit found no unfinished item inside the agreed Phase 8 boundary.
15. **Done:** complete the final desktop feature-access and persistence audit. It added
    **File → Open URL**, distinct persisted custom indexes for each remote alignment and the
    reference, correct signed-URL inference/redaction, and the missing advertised `.bed.gz`,
    `.gff3.gz`, and `.gtf.gz` desktop paths. The audit is recorded in
    `docs/DESKTOP_FEATURE_ACCESS.md`; custom index URLs are bounded to HTTP/S remote sources and
    have credentials removed from persisted projects. Formatting, all 505 active tests, strict
    all-feature Clippy, the release desktop build, and the pinned semantic corpus passed.
16. **Ready:** launch the broad manual product pass from the verified mixed-format and
    alignment-review projects and the release build instructions in
    `validation/MANUAL_QUALIFICATION.md`. Fix concrete findings
    before longer soak, platform/signing, or matched-IGV qualification. Retain existing benchmark
    evidence and do not tune roughly 500 ms loads without a demonstrated usability problem or
    regression.

At the end of every item:

1. update its status here;
2. record exact commands and results;
3. add automated truth, regression, and bounded-work coverage appropriate to the feature;
4. update the user guide, compatibility notes, and changelog for user-visible behavior; and
5. run targeted manual or performance work before Phase 9 only for a concrete defect, a serious
   regression, or behavior that blocks continued feature development.

## 22. Release boundaries and deliberate deferrals

### Public beta requires

- trustworthy short-read and long-read review;
- the Phase 8 sequencing-specific BAM modes and common remaining IGV data tracks;
- stable multi-track and multi-locus comparison;
- complete variant/genotype review;
- RNA support if advertised;
- common quantitative controls;
- standalone genomes, feature search, ROIs, portable sessions, deterministic snapshots, basic
  automation, keyboard operation, and qualified local/remote BAM/CRAM;
- signed packages for every advertised platform; and
- reproducible performance evidence.

### Version 1 adds or hardens

- the complete RNA and quantitative comparison suite;
- sample metadata and bulk cohort workflows;
- richer interaction/copy-number/mutation formats;
- full project recovery and external-control surface;
- complete accessibility and platform matrices; and
- matched published RGV-versus-IGV reports.

### Deferred unless demand changes

- BLAT or external search-service integration;
- Hi-C matrices and circular genome views;
- specialist GWAS analytics beyond the association-summary Manhattan track, and specialist cancer
  formats beyond the listed high-value set;
- a public plugin API;
- hosted genome/track catalogues beyond local genome usability;
- cloud collaboration or shared hosted workspaces; and
- clinical interpretation or decision support.

## 23. IGV behavior references used for this audit

- [Alignment display, coverage, grouping, sorting, and consensus](https://igv.org/doc/desktop/UserGuide/tracks/alignments/viewing_alignments_basics/)
- [Paired-end alignment workflows](https://igv.org/doc/desktop/UserGuide/tracks/alignments/paired_end_alignments/)
- [Chimeric and supplementary-read workflows](https://igv.org/doc/desktop/UserGuide/tracks/alignments/chimeric_reads/)
- [Bisulfite sequencing modes](https://igv.org/doc/desktop/UserGuide/tracks/alignments/bisulfite_sequencing/)
- [PacBio SMRT kinetics](https://igv.org/doc/desktop/UserGuide/tracks/alignments/smrt/)
- [SBX alignment options](https://igv.org/doc/desktop/UserGuide/tracks/alignments/sbx/)
- [Ultima indel coloring](https://igv.org/doc/desktop/UserGuide/tracks/alignments/ultima/ultima/)
- [RNA-seq and Sashimi workflows](https://igv.org/doc/desktop/UserGuide/tracks/alignments/rna_seq/)
- [Quantitative scales, aggregation, and overlays](https://igv.org/doc/desktop/UserGuide/tracks/quantitative_data/)
- [Track attributes and multi-track actions](https://igv.org/doc/desktop/UserGuide/tracks/track_attributes/)
- [Regions of interest](https://igv.org/doc/desktop/UserGuide/regions/)
- [Reference sequence and translation](https://igv.org/doc/desktop/UserGuide/tracks/sequence/)
- [Sessions and autosave](https://igv.org/doc/desktop/UserGuide/sessions/)
- [Batch scripts](https://igv.org/doc/desktop/UserGuide/tools/batch/)
- [External control](https://igv.org/doc/desktop/UserGuide/advanced/external_control/)
- [Supported data-track formats](https://igv.org/doc/desktop/FileFormats/DataTracks/)
