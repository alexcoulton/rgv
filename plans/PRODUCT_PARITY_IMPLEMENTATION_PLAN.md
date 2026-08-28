# RGV Product Parity Implementation Plan

> **Complete-read amendment (2026-08-28):** Individual-read sampling requirements in this plan are
> superseded by [Complete read presentation](COMPLETE_READ_PRESENTATION_PLAN.md). Complete
> aggregate accumulation remains required, but an over-limit individual-read view now presents no
> reads and reports Too dense.

Status date: 22 August 2026

## Implementation status

- Milestone 0 remains open for the complete real-technology and failure-recovery acceptance matrix.
- Milestone 1 has an implemented engineering candidate: bounded allele-aware pileups now retain
  depth, A/C/G/T/N, insertion/deletion, forward/reverse allele support, quality summaries, and
  full-versus-rendered evidence provenance in both local and remote BAM paths. Exact base views
  provide thresholded allele colouring, persisted quality/fraction controls, a centre/selection
  marker, a copyable pileup inspector, and documented base-only consensus.
- Milestone 1 automated evidence includes exact generated-truth counts, sampling independence,
  minimum-base-quality behavior, local/remote backend equivalence, session compatibility, layout
  threshold behavior, pointer-to-bin interaction, the complete workspace suite, strict Clippy, and
  release interaction budgets. Hands-on visual acceptance remains part of milestone 0.
- Milestone 2 is in progress. CIGAR-event markers now resolve to semantic insertion, deletion,
  reference-skip, clipping, or padding selections, and the read inspector provides coordinates,
  bounded sequence/quality previews, reverse-strand orientation, and capped sequence copying.
  Read rendering now also supports persisted mismatch-only/all/hidden base modes, optional quality
  shading, and independent marker/label thresholds. Modified bases now have persisted probability,
  per-code visibility, stable colour, mono/two-colour, and legend controls. Both BAM backends build
  a bounded, sampling-independent site/bin frequency summary with modification/canonical counts,
  probability evidence, modification and alignment strands, haplotypes, and error provenance; the
  pinned lane is pickable and copyable. Bounded `SA` chains now drive loaded-segment highlighting,
  a segment/breakpoint inspector, direct and previous/next navigation, and persisted
  chimeric/supplementary/clip group and sort controls. Real ONT visual acceptance remains.
- Milestone 7 is partial. Both BAM backends now aggregate CIGAR `N` operations independently of
  visible-read sampling with deterministic bounds, local/remote parity, explicit `XS` versus
  alignment-strand fallback provenance, persisted visibility and minimum-depth filtering,
  strand-aware lane rendering, selection highlighting, and copyable inspection. Junction BED/SVG
  export, collapsed/expanded modes, Sashimi, and multi-sample comparison remain.
- Milestone 4 is in progress. Session schema version 2 now represents ordered alignment tracks with
  stable IDs and per-track settings, migrates version 1 deterministically, preserves all IGV
  alignment resources, validates workspace limits, and redacts every source. The desktop now opens,
  queries, renders, persists, reorders, and closes ordered live controllers with independent
  generations and shared cache/record budgets. Semantic inspection is routed by stable track ID,
  loci are translated through each source dictionary, every track has the complete settings
  surface, any track can be promoted to first position, and stable-ID multi-selection provides
  compatible bulk appearance/visibility controls without replacing sample filters. Release-desktop
  four-track qualification remains. Combined workspace/per-track diagnostics and a schema-tested session with
  three healthy controllers plus one intentional source failure now provide the acceptance fixture;
  automated deep-locus failure isolation and one/two/four-track native-RSS scaling pass.
- Milestones 2–11 remain active and should proceed in the order below.

## 1. Purpose

This document defines the remaining product work required for RGV to become a credible native
desktop alternative to IGV Desktop for short-read, long-read, variant-review, and RNA workflows.
It is intentionally separate from [REMAINING_IMPLEMENTATION_PLAN.md](REMAINING_IMPLEMENTATION_PLAN.md),
which remains the authority for runtime qualification, comparative performance evidence,
packaging, signing, and release gates.

The two plans answer different questions:

- This plan asks whether users can perform the analysis workflows they depend on.
- The qualification plan asks whether those workflows are correct, fast, robust, and releasable.

Supporting a file format or preserving a SAM field is not sufficient for product parity. A feature
is complete only when its information can be interpreted, navigated, configured, inspected, saved,
and validated through the desktop UI.

## 2. Product objective

RGV should provide the common IGV Desktop workflows with lower interaction latency, bounded memory,
predictable remote I/O, and a UI designed for current short-read and long-read data.

The product priorities are:

1. Trustworthy alignment and pileup interpretation.
2. First-class paired-read, chimeric-read, and long-read analysis.
3. Multiple samples and multiple synchronized loci.
4. Complete variant and genotype review.
5. RNA junction analysis.
6. Useful quantitative, annotation, and structural-variant tracks.
7. Reproducible sessions and automated review.
8. Performance and robustness that remain stable on deep and remote data.

RGV does not need to reproduce every legacy IGV format or menu before beta. It must first cover the
workflows that determine whether a BAM, CRAM, VCF, or RNA result can be trusted.

## 3. Reference behavior

Parity decisions should be checked against the current IGV Desktop documentation rather than memory
or screenshots alone. The most relevant references are:

- [Alignment and coverage behavior](https://igv.org/doc/desktop/UserGuide/tracks/alignments/viewing_alignments_basics/)
- [Paired-end alignment workflows](https://igv.org/doc/desktop/UserGuide/tracks/alignments/paired_end_alignments/)
- [Chimeric-read workflows](https://igv.org/doc/desktop/UserGuide/tracks/alignments/chimeric_reads/)
- [Base-modification display](https://igv.org/doc/desktop/UserGuide/tracks/alignments/base_modifications/)
- [RNA-seq and splice junctions](https://igv.org/doc/desktop/UserGuide/tracks/alignments/rna_seq/)
- [VCF and genotype display](https://igv.org/doc/desktop/UserGuide/tracks/vcf/)
- [Multi-locus navigation](https://igv.org/doc/desktop/UserGuide/navigation/)
- [Regions and gene lists](https://igv.org/doc/desktop/UserGuide/regions/)
- [Quantitative tracks](https://igv.org/doc/desktop/UserGuide/tracks/quantitative_data/)
- [Reference genomes](https://igv.org/doc/desktop/UserGuide/reference_genome/)
- [Supported data-track formats](https://igv.org/doc/desktop/FileFormats/DataTracks/)
- [Batch scripting](https://igv.org/doc/desktop/UserGuide/tools/batch/)

These references establish behavior to evaluate; they do not require RGV to copy IGV's internal
architecture or visual design.

## 4. Current verified baseline

The repository already contains a substantial native implementation:

- `winit`, `wgpu`, and `egui` desktop application architecture.
- Indexed local BAM and CRAM queries.
- Local FASTA references, including partial and differently ordered reference dictionaries.
- BAI, CSI, and CRAI discovery through supported backends.
- A packed alignment model preserving CIGAR, sequence, qualities, mates, flags, and arbitrary tags.
- Every CIGAR operation, secondary and supplementary records, and missing sequence/quality states.
- Stable read-row assignment, deterministic deep-locus sampling, and full eligible-read coverage.
- Reference bases, mismatch blocks, indel and clipping markers, mate connectors, and `MM`/`ML`
  decoding.
- A pinned reference and coverage header above an independently scrollable read stack.
- Strand, mapping-quality, read-group, haplotype, and arbitrary-tag colour/group controls.
- Read picking and a SAM/tag inspector.
- BED/BED12, GFF3, GTF, indexed VCF/BCF, BigWig, and BigBed tracks.
- Local and HTTP/S data access, bounded caches, stale-query rejection, and directional prefetch.
- Track controls, sessions, partial IGV XML import, PNG export, command search, and diagnostics.

The current UI has been exercised with generated short-read fixtures and a real Oxford Nanopore BAM.
This is an engineering baseline, not yet an IGV-parity claim.

## 5. Definition of feature completion

Every feature in this plan must satisfy all applicable conditions:

- The normalized model retains the required source information without lossy shortcuts.
- Cached and uncached queries produce equivalent semantics.
- Layout remains stable across pan, zoom, detail-level, and query-boundary changes.
- The desktop offers discoverable controls and useful defaults.
- Picking or inspection explains non-obvious symbols and values.
- Session persistence restores all analysis-relevant state.
- A deterministic fixture covers the feature's normal and edge cases.
- Unit tests protect parsing and layout semantics.
- A screenshot or interaction test protects the visible behavior.
- Deep data cannot cause unbounded allocation or UI-thread I/O.
- Local and remote results agree when both backends support the format.
- The user guide documents interpretation and limitations.

## 6. Priority model

| Priority | Meaning | Rule |
| --- | --- | --- |
| P0 | Trust and correctness | Blocks further breadth work and beta qualification. |
| P1 | Core IGV workflow parity | Required for a useful alignment-and-variant beta. |
| P2 | Broad analysis coverage | Required for an IGV-class v1 unless explicitly deferred. |
| P3 | Specialist or ecosystem breadth | Implement from demonstrated demand after the core is stable. |

## 7. Milestone summary

| Milestone | Priority | Outcome |
| --- | --- | --- |
| 0. Close the local acceptance checkpoint | P0 | Current BAM behavior is trusted on representative technologies. |
| 1. Allele-aware pileup and reference evidence | P0 | Coverage supports variant interpretation rather than depth alone. |
| 2. Complete read-event and long-read inspection | P0/P1 | Indels, clips, modifications, and supplementary chains are explainable. |
| 3. Paired-read and structural-evidence workflows | P1 | Discordant pairs and mates can be interpreted and navigated. |
| 4. Multiple alignment tracks | P1 | Tumor/normal, trio, replicate, and technology comparisons work. |
| 5. Multi-locus and breakpoint panels | P1 | Mates, chimeric reads, and distant loci can be reviewed together. |
| 6. Complete VCF and genotype tracks | P1 | Variant calls, samples, genotypes, and evidence are reviewable together. |
| 7. RNA junction and Sashimi workflows | P1/P2 | Split reads become quantitative junction evidence. |
| 8. Quantitative and interaction tracks | P2 | Signal, copy number, and structural interactions are configurable. |
| 9. Genome, annotation, and review workflows | P2 | Gene search, ROIs, gene lists, and reference controls are first class. |
| 10. Sessions, automation, and export | P1/P2 | Reviews are portable, repeatable, and scriptable. |
| 11. Qualification and release handoff | P0 | Product milestones enter the existing release-quality gates. |

## 8. Milestone 0 — close the local acceptance checkpoint

### Scope

Complete the hands-on checkpoint already defined in
[REMAINING_IMPLEMENTATION_PLAN.md](REMAINING_IMPLEMENTATION_PLAN.md). Record findings as structured
evidence rather than treating a successful launch as completion.

### Required data

- A real Illumina paired-end BAM.
- The existing Oxford Nanopore demo BAM.
- A PacBio HiFi BAM and, if available, CLR.
- An RNA-seq BAM with known junctions.
- A genuinely deep BAM that activates deterministic sampling.
- The generated small/large indel and mutation fixture.

### Required scenarios

- Inspect a complete short-read pair and its mate fields.
- Inspect primary, secondary, and supplementary long-read records.
- Verify insertions, deletions, skips, clips, mismatches, and modified bases at declared loci.
- Audit the read inspector against the source SAM fields and tags.
- Exercise missing index, corrupt index, wrong reference, partial reference, and recovery by reopen.
- Close during an active deep query.
- Pan and zoom for at least fifteen minutes while watching RSS and stale-frame behavior.

### Exit criteria

- Every scenario has a pass/fail record and reproducible locus.
- No unexplained correctness, crash, stale-frame, or data-loss issue remains.
- Each fixed defect has a regression test.
- Remaining usability findings have been assigned to a milestone below.

## 9. Milestone 1 — allele-aware pileup and reference evidence

### User outcome

A user can decide whether a visible mismatch is supported by the pileup and understand coverage at a
base without manually counting reads.

### Model work

- Replace the depth-only coverage vector with a bounded pileup summary containing:
  - total eligible depth;
  - A, C, G, T, N, deletion, and optional insertion-support counts;
  - forward and reverse strand counts;
  - base-quality-weighted counts;
  - mapping-quality summaries;
  - sampled-read and full-coverage provenance.
- Keep count accumulation independent of visible-read sampling.
- Use saturating counters and explicit overflow behavior.
- Cache pileups using query interval, filters, quality settings, and source identity.

### UI work

- Stack coverage-bar colours by nucleotide when the alternate fraction exceeds a threshold.
- Add configurable allele-fraction and minimum base-quality thresholds.
- Add a coverage hover/click inspector with locus, reference base, depth, strand counts, allele
  counts, fractions, and quality-weighting state.
- Add copyable pileup details.
- Add optional center line and selected-base marker.
- Add copy-consensus-sequence for the visible interval with a documented consensus algorithm.

### Tests

- Strand-balanced and strand-biased SNVs.
- Low-quality apparent SNVs that disappear under weighting.
- Insertions and deletions contributing to pileup evidence.
- Overlapping mates with a declared overlap policy.
- Secondary, supplementary, duplicate, and QC-fail filter combinations.
- Deep sampled views whose coverage remains identical to an uncapped query.

### Exit criteria

- The generated truth fixture produces exact expected counts.
- Coverage and visible-read sampling are demonstrably independent.
- A reviewer can inspect an SNV without counting individual rows.
- Cached p95 pan/zoom remains inside the locked interaction budget.

## 10. Milestone 2 — complete read-event and long-read inspection

### User outcome

Every visible alignment symbol can be identified, inspected, and related back to the underlying read.

### Indels and clipping

- Clicking an insertion exposes length, inserted sequence, read position, qualities, and orientation.
- Large insertions use a bounded preview with copy/export rather than attempting to draw every base.
- Deletions expose length and reference coordinates.
- Soft clips expose clipped sequence and qualities.
- Hard clips and padding remain visually distinct and explained.
- Track settings control indel-label and hide-small-indel thresholds.

### Base display

- Add mismatch-only, show-all-bases, and hide-all-base modes.
- Make quality shading optional and configurable.
- Preserve readable foreground contrast in all modes.
- Represent IUPAC ambiguity codes explicitly.

### Modified bases

- Preserve canonical base, modification code, strand, skipped-base mode, and probability.
- Give modification types stable, accessible colours and a visible legend.
- Add per-type visibility and probability thresholds.
- Support mono-colour and modified-versus-unmodified two-colour modes.
- Add an aggregate modification-frequency lane by site, strand, and optionally haplotype.
- Explain missing `ML`, malformed `MM`, and unsupported codes without silently inventing probability.

### Supplementary and chimeric long reads

- Parse `SA` relationships into a bounded alignment-chain model.
- Highlight all loaded segments belonging to a selected read.
- Add a chain inspector listing contig, strand, interval, MAPQ, CIGAR, and breakpoint for each segment.
- Add next/previous-segment navigation.
- Add group/sort by chimeric, supplementary, left clip, and right clip.

### Tests

- Small and 100/1,000-base insertions.
- Small and 500/2,000-base deletions.
- Long clips and mixed CIGARs on both strands.
- Multiple `MM` groups, multiple modification codes, missing `ML`, and reverse reads.
- Intra- and inter-chromosomal `SA` chains.

### Exit criteria

- No event marker is unexplained or unpickable.
- Large event inspection remains bounded in time and memory.
- The ONT fixture distinguishes modification types instead of drawing every call identically.
- A selected chimeric read can be followed across every declared segment.

## 11. Milestone 3 — paired-read and structural-evidence workflows

Status: **Candidate.** Stable template identity, explicit pair/mate classification, bounded
all-eligible template-length inference, validated session overrides, pair packing with one pick
target, pair-aware styling, and copyable inspection are implemented. The remaining gate is
real paired-data visual acceptance and mate navigation through milestone 5's panel API. A bounded,
provenance-reporting count-once pileup policy and an explicit persisted count-both alternative now
cover overlapping mates; generated truth covers all five declared structural patterns.

### User outcome

Short-read users can identify discordant pairs and navigate mate evidence without leaving RGV.

### Deliverables

- A per-track “view as pairs” mode with connected mates and joint inspection.
- Pair highlighting that survives pan, zoom, and query refinement.
- Inferred or user-specified expected template-length distribution.
- Colour modes for template length, pair orientation, unexpected pair, and mate chromosome.
- Group modes for first-of-pair strand, orientation, mate chromosome, and read order.
- Sorting by template length, orientation, mate chromosome, and relevant pair status.
- Clear representation of missing, unmapped, out-of-view, and inter-chromosomal mates.
- A mate-region action that opens the distant location in a synchronized panel once milestone 5 is
  available.

### Correctness constraints

- Do not assume every aligner assigns identical semantics to `TLEN`.
- Infer thresholds only from eligible proper pairs and report the sample size.
- Preserve explicit user thresholds in sessions.
- Avoid double-counting overlapping mates in pileup according to a documented policy.

### Exit criteria

- Deletion-like, insertion-like, inversion-like, tandem-duplication, and inter-chromosomal fixtures
  produce their expected pair patterns.
- Pair layout remains stable under deterministic downsampling.
- Both ends and the connector are one logical pick target.

## 12. Milestone 4 — multiple alignment tracks

Status: **In progress.** The session model and ordered runtime-controller collection are implemented.
Focused tests protect migration, ordering, stable IDs, independent filter parsing, limits,
redaction, and workspace budget division. Secondary controllers query and render with independent
state and controls. Read, CIGAR-event, pileup, junction, modification, and supplementary-chain
inspection is track-aware. The full settings editor resolves the selected track's filter draft,
pair inference, modification legend, appearance, cap, and validation state; ordering can promote or
demote the first track without changing stable identity. Compatible bulk settings, combined
diagnostics, and the failure-isolation session fixture are implemented; release-desktop four-track
acceptance remains.
The headless four-track interaction/RSS gate passes under shared budgets; release-desktop manual
acceptance remains.

### User outcome

Users can compare tumor and normal, parents and child, replicates, or sequencing technologies at the
same locus.

### Architecture work

- Replace the single alignment source fields in the desktop with an ordered collection of alignment
  track controllers.
- Give each controller its own source, reference association, filters, rows, caches, settings,
  selection state, statistics, and asynchronous query generation.
- Share immutable reference, range-cache, and dictionary resources where safe.
- Bound total rather than per-track memory and primitive budgets.
- Ensure one slow or failed track cannot delay results from others.

### UI work

- Open, close, reorder, hide, collapse, and rename multiple BAM/CRAM tracks.
- Apply colour/group/filter operations to one, selected, or all alignment tracks.
- Offer shared or independent coverage scales.
- Preserve a clear mapping from reference, coverage, and reads to the owning alignment track.
- Keep pinned reference display singular while coverage remains associated with each sample.

### Session work

- Introduce a versioned session migration from one alignment source to an ordered source list.
- Preserve per-track filters and appearance without storing credentials.

### Exit criteria

- At least four simultaneous BAM/CRAM tracks remain interactive within the agreed total budget.
- Tumor/normal and trio fixtures restore identically from a saved session.
- Failure or cancellation in one track does not blank or reorder another.

## 13. Milestone 5 — multi-locus and breakpoint panels

Status: **In progress.** The stable `PanelId`, panel-generation, and panel/track projection types
are implemented. The current single panel owns its viewport, history, contig, generation, and
selection; engines route and coalesce results by panel, prefetch history is panel-local, layouts are
keyed by panel and track, and diagnostics expose panel identity. Ordered visible panels, panel
persistence, and linked-evidence actions remain.

### User outcome

Distant but related genomic regions can be reviewed side by side with synchronized tracks.

### Architecture work

- Introduce an explicit `ViewPanel` model containing viewport, history, selection, and query
  generation.
- Let a workspace own one or more ordered panels and one ordered track collection.
- Key caches and picking by panel as well as track and source.
- Share decoded data when panels overlap without coupling their navigation state.

### UI work

- Accept space-separated loci and feature names in the locus field.
- Add, reorder, resize, and close panels.
- Navigate one active panel while retaining the others.
- Open mate, supplementary segment, VCF breakend, or interaction endpoint in a new panel.
- Highlight linked evidence across panels.
- Return any panel to a normal single-locus view.

### Exit criteria

- Intra- and inter-chromosomal breakpoints can be opened with one action.
- Panel ordering and loci survive a session round trip.
- Four panels with four alignment tracks remain responsive on reference hardware.

## 14. Milestone 6 — complete VCF and genotype tracks

### User outcome

A user can interpret a variant call and its sample genotypes without opening a separate VCF tool.

### Model work

- Retain variant `FILTER`, structured `INFO`, alleles, symbolic alleles, breakends, and quality.
- Retain sample names, ploidy, phased/unphased genotype alleles, and selected `FORMAT` values.
- Decode common fields such as `GT`, `DP`, `GQ`, `AD`, `PL`, `PS`, and caller-specific values on
  demand rather than expanding every field eagerly.
- Preserve header metadata needed to explain fields and types.

### UI work

- Variant detail inspector with copyable normalized and original values.
- Collapsed, expanded, and squished display modes.
- Sample/genotype rows with clear no-call, reference, heterozygous, alternate, and phased states.
- Sorting by genotype, sample name, depth, and quality.
- Grouping and filtering by loaded sample attributes.
- Structural-variant glyphs and breakend navigation into multi-locus panels.
- Optional linkage from a selected genotype to the matching alignment sample.

### Tests

- Multi-allelic, symbolic, phased, polyploid, missing, filtered, and breakend records.
- Thousands of samples with lazy `FORMAT` decoding and bounded row virtualization.
- Sample names that differ from track labels and optional sample-mapping metadata.

### Exit criteria

- A multi-sample truth VCF renders exact expected genotype states.
- Selecting a variant exposes all required interpretation fields.
- Large sample counts do not allocate one permanent UI widget per cell.

## 15. Milestone 7 — RNA junction and Sashimi workflows

Status: **Partial.** The sampling-independent junction model, strand provenance, bounded lane,
filter, and inspector are implemented and pass generated local/remote truth and interaction gates.
Export, presentation modes, Sashimi, and multi-sample workflows remain.

### User outcome

RNA-seq users can evaluate splice-junction support and alternative isoforms quantitatively.

### Deliverables

- Aggregate `N` operations into a splice-junction model independent of individual read rendering.
- Infer strand from `XS` where available and from a documented fallback otherwise.
- Render junction arcs with depth, strand, filtering, collapsed/expanded modes, and inspection.
- Export visible junctions to a documented BED representation.
- Provide junction minimum-depth and autoscale controls.
- Add a Sashimi view combining coverage, junction arcs, counts, and a selected gene annotation.
- Export Sashimi output as PNG and SVG.

### Exit criteria

- Known RNA fixtures produce exact junction endpoints, counts, and strands.
- Junction counts remain based on all eligible reads when visible alignments are sampled.
- Sashimi rendering supports more than one alignment track for sample comparison.

## 16. Milestone 8 — quantitative and interaction tracks

### Quantitative controls

- Fixed and automatic data ranges.
- Linear and logarithmic scales.
- Minimum, mean, maximum, and raw windowing functions where supported.
- Bar, line, point, and heatmap styles.
- Positive and negative colours.
- Group autoscaling across selected tracks.
- Overlay and separate quantitative tracks with adjustable transparency.
- Arithmetic combinations with explicit missing-value behavior.

### Structural and interaction tracks

- BEDPE interactions and structural variants.
- Arc and block display modes.
- Endpoint inspection and multi-locus navigation.
- `bigInteract` or another indexed interaction representation after BEDPE semantics are stable.

### Additional format order

Implement formats according to workflow value:

1. bedGraph and WIG.
2. BEDPE/interact.
3. SEG and copy-number matrices.
4. MAF mutation tracks.
5. genePred/refFlat.
6. narrowPeak.
7. PSL.

Defer GISTIC, GWAS, TDF, and other specialist or legacy formats until user demand justifies them.

### Exit criteria

- Track settings restore through sessions.
- Zoomed-out aggregation is deterministic and documented.
- Remote indexed formats remain range-based and bounded.

## 17. Milestone 9 — genome, annotation, and review workflows

### Genome management

- Load a reference assembly from FASTA/FAI without requiring a BAM.
- Add `.2bit` only after FASTA behavior is complete.
- Support genome JSON with sequence, aliases, cytobands, and default annotations.
- Add chromosome aliases such as `1` and `chr1` without changing stored canonical names.
- Provide a curated hosted-genome catalogue with versioned metadata and explicit network behavior.
- Evaluate GenArk/UCSC track hubs after custom genome JSON is stable.

### Reference track

- Treat reference sequence as a true independent workspace track.
- Flip displayed strand.
- Copy visible or selected sequence.
- Optional three-frame amino-acid translation with selectable translation tables.

### Feature navigation

- Index annotation names, transcript IDs, and selected attributes.
- Add locus-field autocomplete and gene/transcript search.
- Add next/previous feature navigation.
- Provide a rich feature inspector and copyable attributes.

### Regions and review queues

- Create, edit, name, delete, import, and export regions of interest.
- Render ROI boundaries consistently across tracks and panels.
- Add a region navigator and next/previous review actions.
- Support gene and locus lists as saved multi-locus review sets.
- Persist review state without embedding private sequence or credentials.

### Exit criteria

- A custom genome, annotation track, ROIs, and gene list survive a session round trip.
- Searching a feature selects the correct canonical contig and interval.
- Partial references clearly indicate unavailable contigs without blocking other data.

## 18. Milestone 10 — sessions, automation, and export

### Sessions

- Introduce schema migrations for multiple alignments and multiple panels.
- Preserve all analysis-relevant track and panel state.
- Support relative paths for portable local projects.
- Improve IGV XML import for common renderer, grouping, filter, and panel state while continuing to
  ignore unsupported elements safely.
- Validate before save and never overwrite an unknown future schema.

### Automation

- Add a documented batch-command file or stable local command API.
- Minimum commands: new workspace, load genome, load track, goto locus, add panel, set track option,
  set filter, wait for settled view, snapshot, save session, and exit.
- Make “settled view” explicit so automation does not depend on arbitrary sleeps.
- Keep credentials out of scripts, logs, sessions, and exported metadata.

### Export

- Retain deterministic PNG export.
- Add SVG for tracks whose primitives can be represented faithfully.
- Add per-track data export where semantics are clear: pileup table, junction BED, ROI BED, and
  selected read or variant details.
- Support batch snapshots larger than the visible window only with explicit memory bounds.

### Accessibility and interaction

- Complete keyboard access for track menus, inspectors, panels, and review queues.
- Add accessible names for genomic symbols and state changes.
- Test high-contrast modes, text scaling, reduced motion, and screen-reader navigation where the
  platform stack permits.

### Exit criteria

- A deterministic script can rebuild a multi-sample, multi-panel review and export its evidence.
- The saved session restores an equivalent workspace on a clean installation.
- Accessibility smoke tests pass on each supported platform.

## 19. Milestone 11 — qualification and release handoff

Once milestones 0–10 reach their declared scope, execute the existing qualification plan rather than
creating a second release process.

Required gates include:

- Remote CRAM/reference qualification.
- Real-world technology corpus.
- Native macOS, Windows, and Linux runtime matrix.
- GPU device-loss and display-scaling recovery.
- Fuzz, corruption, and reopen testing.
- One-hour navigation soak.
- Reproducible RGV-versus-IGV measurements.
- Signed, notarized, clean-machine-tested packages.

No “faster than IGV” claim should be published until the matched benchmark described in
[benchmarks/IGV_COMPARISON.md](../benchmarks/IGV_COMPARISON.md) passes on the exact release candidate.

## 20. Cross-cutting architecture requirements

### Identity

Use stable identifiers for source, track, panel, record, read/template, sample, and session object.
Never use vector position as persistent identity.

### Bounded work

- Every query, cache, display list, label set, preview, genotype matrix, and export has a declared
  bound.
- Results for obsolete panel generations are discarded.
- UI controls never trigger synchronous storage or network I/O.
- Deep data use deterministic summarization, sampling, or virtualization rather than silent loss.

### Layering

- Reference, coverage, junctions, reads, and event overlays have explicit layers.
- Pinned and scrollable regions are represented structurally rather than inferred from colour or
  coordinate conventions.
- Picking uses semantic objects, not raw primitive indices.

### Configuration

- Global defaults and per-track overrides are distinct.
- Every analysis-changing option participates in cache keys and session persistence.
- Display-only changes should not force data re-decoding.

### Errors

- Loading errors replace placeholder state with a clear failed-source state.
- Partial availability is preferred when safe: for example, a partial FASTA can supply `chr6`
  without blocking BAM data on other contigs.
- Missing information is shown as unavailable, never guessed.

## 21. Fixture and validation matrix

Each fixture must have checksums, declared loci, expected counts, and permission for its intended use.

| Fixture | Required semantics |
| --- | --- |
| Generated variant truth set | SNVs, small/large insertions and deletions, labels, base contrast |
| Illumina paired-end | Proper pairs, overlaps, duplicates, discordant size/orientation, unmapped mates |
| Oxford Nanopore | Long CIGARs, `SA`, `MM`/`ML`, multiple modification types, large events |
| PacBio HiFi/CLR | Long reads, supplementary records, kinetic tags where available |
| RNA-seq | Known stranded junctions, overlapping isoforms, exon coverage |
| Tumor/normal | Shared and sample-specific SNVs, indels, SV evidence, copy-number signal |
| Trio | Phased genotypes, haplotypes, Mendelian and missing calls |
| Deep amplicon | Sampling threshold, exact full coverage, bounded memory |
| Bisulfite | CG, CHG, and CHH interpretation if included in v1 scope |
| Corruption set | Truncated data/index/reference, mismatched lengths, malformed tags |

Private real-world data may be used for local acceptance, but repository fixtures and normal CI
artifacts must remain redistributable and contain no sensitive read identifiers or sequence.

## 22. Performance budgets

Feature work must preserve the existing intent of the performance plan:

- No storage or network I/O on the UI thread.
- Cached interaction p95 below 16.7 ms on named reference hardware.
- First useful local frame below 250 ms for the agreed warm indexed locus.
- No stale result may replace the active panel generation.
- No unbounded queue or cache.
- Multi-track and multi-panel budgets apply to the total workspace.
- Large previews and exports require explicit caps and visible truncation.
- Coverage, junction, and modification aggregates must be calculable without requiring every read to
  become a retained render object.

Budgets that become unrealistic after real measurement must be revised explicitly with evidence,
not weakened silently in implementation.

## 23. Deliberately deferred features

The following are useful but do not block the first alignment-and-variant beta:

- Integrated BLAT or sequence alignment service.
- Integrated indexing, sorting, and coverage-generation utilities.
- Circular genome/SV view.
- Hi-C matrix rendering.
- GISTIC, GWAS, and specialised expression formats.
- A general plugin API.
- Collaborative annotations or cloud-hosted projects.
- Automatic variant classification or clinical interpretation.

Security-sensitive external services, especially sequence submission, require a separate privacy
design before implementation.

## 24. Recommended execution order

Work should proceed in this order unless a P0 correctness defect intervenes:

1. Finish milestone 0 and record its evidence.
2. Implement milestone 1, because allele-aware coverage is foundational to variant trust.
3. Implement milestone 2, completing event and long-read semantics already visible in the UI.
4. Implement milestone 3 for paired-end and structural evidence.
5. Refactor to milestone 4 multiple alignment tracks before adding more sample-oriented formats.
6. Add milestone 5 multi-locus panels and connect mate, supplementary, and breakend navigation.
7. Complete milestone 6 VCF/genotype review.
8. Implement milestone 7 RNA workflows.
9. Add milestone 8 quantitative, copy-number, and interaction breadth.
10. Add milestone 9 genome and review workflows.
11. Complete milestone 10 sessions, automation, export, and accessibility.
12. Execute milestone 11 qualification on the exact beta candidate.

After each milestone:

1. Run focused unit and integration tests.
2. Run formatting, strict Clippy, and the complete workspace suite.
3. Build the release desktop binary.
4. Repeat the declared manual fixture scenarios.
5. Update the user guide and changelog.
6. Record performance deltas when the hot path changed.

## 25. Beta scope

The first public beta should require:

- Milestones 0–6 complete.
- RNA milestone complete if RNA-seq is advertised as supported rather than merely renderable.
- At least the core quantitative controls from milestone 8.
- FASTA genome loading, gene search, and ROIs from milestone 9.
- Session migration and deterministic automation from milestone 10.
- All qualification gates in milestone 11.

Features not meeting these criteria must be described as experimental, not silently presented as
IGV-equivalent.

## 26. V1 scope

V1 should require:

- All beta feedback affecting correctness, crashes, accessibility, or data loss resolved.
- Milestones 0–11 complete at their committed scope.
- Supported formats and platforms frozen and documented.
- Matched IGV performance results published with raw methodology.
- Signed artifacts, checksums, notices, and session compatibility policy published.
- Deferred features explicitly listed so users can make an informed migration decision.
