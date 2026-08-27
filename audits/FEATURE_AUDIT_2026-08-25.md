# RGV versus IGV Desktop feature audit

**Audit date:** 25 August 2026

**RGV baseline:** the current workspace, including uncommitted changes present during the audit

**Comparator:** the current official IGV Desktop documentation (not IGV-Web or igv.js)

## Executive finding

RGV is already a substantial native genome viewer. It has strong implementation coverage for the
central alignment-review workflow, common indexed and text tracks, multi-track/multi-locus work,
quantitative composition, reproducible projects, bounded automation, and privacy-aware remote I/O.
It is credible today as an alternative for a deliberately scoped set of IGV workflows.

It is **not yet accurate to claim complete IGV feature parity**. There are three separate reasons:

1. Several important IGV workflows are absent or materially narrower: whole-genome view, hosted
   genomes/tracks and UCSC track hubs, gene lists, on-track VCF genotype matrices, some variant
   display/sort modes, general cloud authentication, interactive SVG export, and a number of file
   formats and utility services.
2. The broad representative-data, platform, accessibility, soak, and matched IGV comparison gates
   are explicitly not complete. Implementation breadth is not the same as qualified behavior.
3. Compatibility is sometimes intentionally RGV-native rather than IGV-compatible. Examples are
   `.rgvgenome` rather than IGV genome JSON, RGV JSON projects with partial IGV XML import, typed
   JSON automation rather than IGV batch syntax, and incomplete interpretation of source track
   lines.

The most defensible current positioning is:

> RGV is a native, bounded-work Rust alternative for common alignment, variant inspection,
> annotation, quantitative, RNA, and review workflows. Full IGV parity is a roadmap, not a current
> release property.

This audit deliberately does not assign a single parity percentage. Counting a BAM pileup, a
hosted genome catalogue, and a legacy file extension as equal-sized “features” would give a precise
but misleading number.

## Audit method and confidence

The RGV side was checked against source models, readers, desktop wiring, persistence, tests, and
the existing user/qualification documents. The IGV side was checked against the current official
[IGV Desktop documentation index](https://igv.org/doc/desktop/), including its pages for
[navigation](https://igv.org/doc/desktop/UserGuide/navigation/),
[alignment viewing](https://igv.org/doc/desktop/UserGuide/tracks/alignments/viewing_alignments_basics/),
[formats](https://igv.org/doc/desktop/FileFormats/DataTracks/),
[reference genomes](https://igv.org/doc/desktop/UserGuide/reference_genome/), and
[automation](https://igv.org/doc/desktop/UserGuide/tools/batch/).

The following command was run on the current RGV workspace:

```text
cargo test --workspace --all-targets
```

Result: **620 passed, 0 failed, 1 ignored**. The ignored test requires the public IGV data server.
This audit did not repeat the release build, strict Clippy pass, packaged GPU smoke, or manual
qualification. The repository records those older targeted results separately.

Status language used below:

- **Equivalent** — the important user outcome and semantics are implemented, although the UI may
  differ.
- **Alternative** — RGV reaches a similar outcome through a deliberately different workflow.
- **Partial** — useful support exists, but an important part of the IGV workflow is absent.
- **Missing** — no current end-to-end RGV workflow was found.
- **RGV advantage** — a material capability beyond the documented IGV workflow.

“Implemented” is a static/automated conclusion. Unless stated otherwise, broad representative-data
and cross-platform qualification is still pending.

## Workflow comparison

| Area | IGV Desktop capability | Current RGV capability | Audit |
| --- | --- | --- | --- |
| Basic navigation | Coordinate/feature search, ideogram/ruler, pan/zoom, whole chromosome and base resolution | Coordinate and bounded feature/variant-ID search, history, contig overview, cursor zoom, pan, base resolution | **Equivalent** for single-contig review |
| Whole-genome view | `All` view lays chromosomes side by side and supports genome-wide tracks | No cross-chromosome whole-genome canvas was found | **Missing** |
| Multi-locus | Side-by-side loci, panel add/remove/reorder, mate and ROI entry points | Stable independently navigable panels, panel persistence, linked mate/supplementary/endpoint navigation | **Equivalent** |
| Gene/region lists | Persistent GMT/GRP/BED gene lists and multi-locus viewing | ROIs and a richer status/notes/assignment review queue; BED ROI and TSV review import/export | **Alternative**; no IGV-style gene-list library |
| Reference genomes | Hosted catalogue, GenArk/useOneFile hubs, FASTA/bgzip FASTA, 2bit, genome JSON, GenBank | Indexed local/HTTP FASTA plus `.rgvgenome` bundles with aliases, cytobands, and default annotations | **Partial** |
| Reference display | Bases, reverse strand, translation table and three-frame translation on the displayed strand | Pinned reference, reverse orientation, configurable genetic code and six-frame translation | **Equivalent / RGV advantage** |
| Local data loading | Multi-file load, drag/drop, indexed and unindexed track families | Native dialogs, CLI, projects and drag/drop; supported formats are extension-typed | **Equivalent** within RGV's format set |
| Remote data loading | General URL loading for tracks plus indexed range sources | HTTP/S for BAM/CRAM/FASTA, VCF/BCF, BigWig/TDF/BigBed and typed BigBed derivatives; common text tracks are local-only | **Partial** |
| Hosted/cloud data | Hosted tracks, track servers/hubs, OAuth2, Google Cloud and authenticated S3 workflows | Bearer-token HTTP support and signed-URL-safe handling; no catalogue, hub, OAuth UI, `s3://`, or provider integration | **Partial** |
| Alignment fundamentals | SAM/BAM/CRAM, coverage/read/junction subtracks, visibility windows, details and clipboard actions | Indexed BAM/CRAM, reference/coverage/junction/read layers, bounded inspectors and copy actions | **Equivalent** for BAM/CRAM; SAM absent |
| Coverage | Dynamic pileup, allele fraction, quality weighting, strand counts, optional associated precomputed TDF | Full-query allele-aware coverage, quality weighting, strand/indel detail, consensus and explicit sampling provenance | **Equivalent / RGV advantage** dynamically; no automatic BAM-TDF association |
| Read layout | Expanded/squished/collapsed, downsampling, base/mismatch/quality/clip/indel display | Density modes, deterministic sampling, all CIGAR operations, quality-aware mismatches, explicit truncation | **Equivalent / RGV advantage** |
| Alignment group/sort/filter | Rich group, sort, colour and filter controls including tags and pair attributes | Rich persisted group/sort/colour/filter model including tags, clips, haplotype, pair state and direction | **Equivalent** |
| Paired and chimeric reads | Pair view, insert/orientation evidence, split mate view, SA-linked supplementary diagram | Pair packing/connectors/classification, mate panels, read-order/alignment-order chain diagram and segment navigation | **Equivalent** |
| Specialist BAM semantics | Bisulfite, MM/ML modifications, SMRT kinetics, SBX and Ultima modes | All five families have explicit models, rendering, legends/inspection and automated truth fixtures | **Implemented; qualification pending** |
| RNA review | Split reads, junction track, filters, strand semantics, multi-track Sashimi window and PNG/SVG | Dynamic junction/Sashimi subtrack, raw/CPM scale, strand/filter/annotation handling and BED/PNG/SVG export | **Alternative / near-equivalent** |
| VCF calls | Variant markers, allele-frequency/fraction colouring, expanded/squished/collapsed modes | Typed VCF/BCF markers, type/cohort-genotype colouring, SV/BND orientation, filters and partner navigation | **Partial** |
| VCF genotypes | Genotype matrix directly below calls; show/hide, row density, grouping and sorting by genotype/name/DP/GQ/attributes | Genotypes are decoded, filtered, virtualized and grouped in the inspector, with subset/pedigree controls; no on-track genotype matrix was found | **Material gap** |
| Variant-to-read review | VCF inspection beside separately loaded evidence | Inspector links a selected variant to current alignment pileups and structural endpoint panels without calling observational evidence | **RGV advantage** |
| Annotation tracks | BED/BigBed/GFF/GTF/genePred/PSL/peaks, three density modes, strand grouping, navigation/export | Common formats, typed exon/CDS/peak/PSL semantics, three density modes, search/navigation and inspection | **Near-equivalent**; no annotation group-by-strand or general in-view BED export found |
| Quantitative tracks | WIG/BigWig/bedGraph/TDF; bar/line/points/heatmap, ranges, windowing, overlays, transparency, arithmetic and group autoscale | Same common sources and display modes, automatic/fixed/shared scales, signed-log, missing-data policy, overlays, opacity, arithmetic and scale groups | **Equivalent / RGV advantage** |
| Copy number/mutation/GWAS | SEG/CBS, CN/LOH/GISTIC/Birdsuite specialists, mutation MAF, GWAS | SEG/CBS heatmap and sorting, mutation MAF matrix, typed Manhattan GWAS | **Partial** because specialist families are absent |
| Interactions | BEDPE and other arc-style tracks; specialist Hi-C/circular/RNA structure views also exist in the wider product | BEDPE/interact arcs or blocks with directed endpoints and linked panels | **Equivalent** for BEDPE/interact; specialist views missing |
| Sample attributes | Adjacent colour-coded attribute panel; select/sort/group tracks and VCF samples | Separate virtualized metadata table, exact sample associations, cohort selection, bulk compatible actions and pedigree ordering | **Alternative / partial** |
| General track control | Multi-select, reorder, rename, remove, colour, height, visibility and type-specific menus | Stable-ID selection, reorder, rename/bulk rename, remove, height, visibility/collapse and type-specific settings | **Equivalent** |
| Sessions/recovery | XML save/open/reload/new; exit and timed autosaves; optional autoload | Versioned portable JSON projects, relative relocation, atomic save, opt-in timed recovery and unclean-exit chooser | **Alternative / RGV advantage** for safety; IGV XML import is partial |
| Images and exports | Interactive whole-window/panel PNG and SVG plus batch snapshots | Interactive PNG; deterministic PNG and semantic SVG in automation; many bounded structured evidence exports | **Partial** interactive parity / **RGV advantage** for structured exports |
| Batch automation | Text command files and a broad command vocabulary | Versioned typed JSON batches with settlement, stable IDs, deterministic reports and failure semantics | **Alternative / RGV advantage** for reproducibility |
| External control | Optional unauthenticated-compatible command port using batch vocabulary | Disabled-by-default, authenticated loopback service with filesystem confinement; optional MCP adapter | **RGV advantage** in safety and structured observation |
| Utilities/services | igvtools transform/index/count/sort, motif finder and BLAT | No bundled equivalents | **Missing / deliberately deferred** |
| Accessibility | Standard desktop controls; platform behavior is part of mature IGV use | AccessKit semantics, keyboard workflows, UI scale/high contrast/reduced motion and automated node tests | **Implemented; hands-on screen-reader/platform qualification pending** |

### The most important correction to the existing roadmap

The current roadmap labels “VCF and genotype review” complete. Static inspection supports the VCF
decoder, filters, inspector, sample subset, grouping, evidence link, and exports. It does **not**
support the stronger statement of IGV workflow parity: RGV's variant layout draws variant markers,
while genotype rows are presented in the inspector rather than as the scrollable per-sample matrix
below the variant calls that IGV documents. IGV also documents collapsed/squished/expanded VCF
modes, genotype visibility, allele-frequency/fraction marker colouring, and sorting sample rows by
genotype, name, depth, and quality. Those controls are not represented in RGV's current
`VariantTrackSettings`.

This does not make RGV's current variant implementation weak; its typed filtering and
variant-to-alignment evidence are substantial. It means the correct status is **partial workflow
parity**, not complete parity.

## Data-format comparison

The official IGV page currently lists 31 top-level data-format families (counting CBS and SEG as
separate documented headings and excluding the Track Lines/Type Lines metadata sections). RGV has
full implementations for 20 of those headings, partial coverage of the combined GFF/GTF heading,
and no implementation for 10. This count is only an inventory, not a parity score.

### Implemented in RGV

| Family | RGV notes |
| --- | --- |
| BAM; CRAM | Local and HTTP/S indexed access; BAI/CSI and CRAI; CRAM reference support |
| BED/BED12; narrowPeak; broadPeak; PSL | Bounded local plain/gzip indexes; nucleotide PSL only |
| BigBed; bigGenePred; bigNarrowPeak | Local/HTTP indexed access; explicit typed custom schemas |
| bedGraph; WIG; BigWig; TDF | Common quantitative model; BigWig/TDF local/HTTP indexed access; multi-channel TDF |
| BEDPE | Typed interaction track; RGV also supports UCSC `interact` |
| genePred | Base, extended and refFlat-style models; RGV also exposes refFlat directly |
| GWAS | IGV/PLINK-compatible association headers and Manhattan view |
| VCF | Indexed VCF; RGV additionally supports indexed BCF |
| SEG; CBS | One copy-number decoder and multi-sample heatmap |
| Mutation Annotation Format MAF | Typed cancer mutation records and sample matrix |

### Partial format or metadata compatibility

| Family | Gap |
| --- | --- |
| GFF/GTF | GFF3 and GTF are implemented; the source model advertises `Gff3`, not general GFF2 compatibility |
| Track/type lines | Most text-track metadata directives are ignored; SEG honors `coords`, but RGV UI/project settings are not a general parser for IGV/UCSC `graphType`, colours, ranges, scale, windowing, URL and type-line semantics |
| FASTA reference | Plain indexed FASTA works locally and over HTTP/S; block-gzipped FASTA and automatic index creation are not advertised |
| IGV session XML | Resource/locus and selected common track state are imported with an explicit imported/approximated/ignored report; this is not full round-trip compatibility |

### Documented IGV families not implemented in RGV

| Family | Current disposition |
| --- | --- |
| SAM | Missing; RGV intentionally starts at indexed BAM/CRAM |
| Merged `.bam.list` | Missing |
| IGV multi-sample numeric `.igv` | Missing |
| CN and LOH | Missing specialist numeric/copy-number families |
| GISTIC and Birdsuite | Missing specialist cancer/copy-number families |
| Multiple Alignment Format MAF | Missing; mutation MAF is a distinct supported format |
| RNA secondary-structure BP/DB/CT/DP/arc imports | Missing |
| SHAPE/MAP chemical-reactivity import | Missing |

### Reference formats and catalogues outside that 31-heading data-track count

| IGV capability | RGV status |
| --- | --- |
| Hosted genome catalogue | Missing |
| UCSC GenArk/useOneFile track hubs | Missing |
| IGV genome JSON | RGV has its own `.rgvgenome` alternative, not format compatibility |
| 2bit reference | Missing |
| GenBank reference | Missing |
| Block-gzipped indexed FASTA | Not advertised/supported by source classification |

## RGV capabilities that should remain product differentiators

Parity work should not erase the architectural strengths already present:

- generation-based cancellation and stable panel/track identities prevent stale results from
  replacing a newer view;
- explicit workspace-wide limits cover decoded data, queries, GPU primitives, inspectors and
  exports, with omissions reported rather than hidden;
- deterministic deep-locus sampling retains nearby read identity while coverage uses all eligible
  evidence;
- typed BCF, structural breakend orientation, variant-to-alignment observations and bounded
  structured exports go beyond the basic documented IGV VCF workflow;
- portable relative-path projects, atomic saves, credential stripping and opt-in recovery provide
  unusually clear reproducibility and privacy behavior;
- typed JSON automation waits for actual settlement and emits deterministic command-by-command
  reports; and
- authenticated, root-confined local control plus the MCP adapter is safer and more observable
  than a general listener enabled without the same confinement model.

## Recommended priorities

### P0 — required before a broad parity claim

1. **Complete the qualification already defined in the repository.** Run the representative
   Illumina, ONT, PacBio, RNA, amplicon, bisulfite, SBX, Ultima, single-cell, high-depth, SV-heavy,
   cohort, CRAM and remote matrix; then the hands-on, accessibility, failure, soak and platform
   passes.
2. **Qualify the current workspace revision.** The audit ran against a dirty working tree and a
   larger 620-test suite than the 505-test result recorded on 23 August. Freeze the intended
   revision and repeat format, Clippy, tests, release build, synthetic validation and packaged
   smoke.
3. **Correct product/status language.** Change “VCF and genotype review: Complete” to partial until
   the missing on-track cohort workflow is implemented or explicitly declared outside scope.
4. **Run the matched IGV protocol.** The repository has a rejecting report compiler and procedure,
   but no completed comparison. Do not publish speed or memory superiority until raw matched
   samples exist.

### P1 — highest-value feature gaps

1. Add an on-track VCF genotype matrix with show/hide, density modes, sample scrolling, grouping and
   genotype/name/DP/GQ/attribute sorting; add allele-frequency/fraction marker rendering.
2. Add a whole-genome view, especially for SEG/CBS, mutation, GWAS and other genome-wide summaries.
3. Decide whether RGV wants catalogue compatibility (hosted genomes, IGV genome JSON and/or UCSC
   hubs) or a first-party Rust-native catalogue. This is a major usability difference for new
   users.
4. Broaden remote loading to common text tracks and add a deliberate OAuth/cloud-provider story if
   shared cloud datasets are in target scope.
5. Add interactive SVG export and general feature-in-view BED export, or document automation as the
   required export path.
6. Improve source metadata compatibility: parse the useful subset of track/type lines instead of
   silently replacing those semantics with defaults.

### P2 — scope-dependent compatibility

- SAM, `.bam.list`, bgzip FASTA, 2bit, IGV numeric, CN and LOH;
- persistent gene-list import and multi-locus management;
- annotation group-by-strand and the remaining IGV track presentation details;
- broader IGV XML import/export compatibility; and
- GISTIC, Birdsuite, multiple-alignment MAF, RNA structures, chemical-reactivity import, BLAT,
  motif search, igvtools, Hi-C and circular views only where user demand justifies their cost.

## Qualification state at audit time

| Gate | Evidence found | Audit conclusion |
| --- | --- | --- |
| Current automated unit/integration tests | 620 pass, 0 fail, 1 ignored on the current workspace | Strong |
| Synthetic format/semantic fixtures | Broad checked-in coverage and prior mixed-track/native smoke reports | Strong but synthetic/targeted |
| Representative real-data matrix | Repository says not run except a narrow ONT engineering baseline | Open |
| Broad hands-on review | Fixture suite is ready; user-led pass remains | Open |
| Windows/Linux installed packages | Packaging definitions exist; actual platform qualification not complete | Open |
| macOS package | Prior unsigned targeted smoke; signing/notarization/clean install remain | Partial |
| Screen readers/accessibility | Semantic and keyboard tests exist; platform screen-reader pass remains | Partial |
| Recovery/soak/failure lifecycle | Automated and controlled targeted tests; packaged exercise and one-hour soak remain | Partial |
| Matched IGV performance/resources | Protocol/compiler exist; data collection not run | Open |

## Primary evidence

RGV repository evidence:

- [`README.md`](../README.md)
- [`docs/FORMAT_SUPPORT.md`](../docs/FORMAT_SUPPORT.md)
- [`docs/USER_GUIDE.md`](../docs/USER_GUIDE.md)
- [`docs/DESKTOP_FEATURE_ACCESS.md`](../docs/DESKTOP_FEATURE_ACCESS.md)
- [`plans/RGV_IGV_PARITY_ROADMAP.md`](../plans/RGV_IGV_PARITY_ROADMAP.md)
- [`validation/MANUAL_QUALIFICATION.md`](../validation/MANUAL_QUALIFICATION.md)
- [`benchmark-results/PHASE9_QUALIFICATION_STATUS.md`](../benchmark-results/PHASE9_QUALIFICATION_STATUS.md)
- source definitions in `crates/rgv-core`, format readers in `crates/rgv-hts`, layouts in
  `crates/rgv-layout`, and desktop wiring/tests in `apps/rgv-desktop`

Official IGV Desktop evidence:

- [Documentation index and feature navigation](https://igv.org/doc/desktop/)
- [Navigating and multi-locus view](https://igv.org/doc/desktop/UserGuide/navigation/)
- [Loading local, URL and hosted tracks](https://igv.org/doc/desktop/UserGuide/loading_data/)
- [Reference genomes, hosted genomes and UCSC hubs](https://igv.org/doc/desktop/UserGuide/reference_genome/)
- [Data-track formats](https://igv.org/doc/desktop/FileFormats/DataTracks/)
- [Alignment basics and coverage](https://igv.org/doc/desktop/UserGuide/tracks/alignments/viewing_alignments_basics/)
- [Paired-end alignments](https://igv.org/doc/desktop/UserGuide/tracks/alignments/paired_end_alignments/)
- [Chimeric and supplementary reads](https://igv.org/doc/desktop/UserGuide/tracks/alignments/chimeric_reads/)
- [RNA-seq and Sashimi](https://igv.org/doc/desktop/UserGuide/tracks/alignments/rna_seq/)
- [VCF calls and genotype rows](https://igv.org/doc/desktop/UserGuide/tracks/vcf/)
- [Quantitative tracks and overlays](https://igv.org/doc/desktop/UserGuide/tracks/quantitative_data/)
- [Track arithmetic](https://igv.org/doc/desktop/UserGuide/tools/combine_tracks/)
- [Sample attributes](https://igv.org/doc/desktop/UserGuide/sample_attributes/)
- [Regions and gene lists](https://igv.org/doc/desktop/UserGuide/regions/)
- [Sessions and autosave](https://igv.org/doc/desktop/UserGuide/sessions/)
- [PNG/SVG image export](https://igv.org/doc/desktop/UserGuide/saving_images/)
- [Batch scripts and external control](https://igv.org/doc/desktop/UserGuide/tools/batch/)
- [OAuth and cloud access](https://igv.org/doc/desktop/UserGuide/advanced/oauth/)
- [BLAT](https://igv.org/doc/desktop/UserGuide/tools/blat/) and
  [motif finder](https://igv.org/doc/desktop/UserGuide/tools/motif_finder/)
