# Directionality rendering audit and remediation plan

Status: implemented and qualified

Date: 25 August 2026

## 1. Purpose

This plan addresses genomic direction and orientation information that is present in RGV's data
models but is absent, incomplete, or represented only by colour in the renderer.

The immediate comparison used the latest IGV and RGV captures at
`chr1:2284422-2285804`. IGV visibly communicates direction on ordinary alignments, the reference
sequence row, and RefSeq Select transcripts. At the same locus, RGV renders ordinary alignments as
rectangles and does not provide equivalent reference or transcript glyphs.

The objective is semantic parity, not pixel-for-pixel imitation. Direction must remain legible
across zoom levels, exports, colour modes, and supported track formats without inventing a single
direction for aggregate or unstranded data.

## 2. Audit summary

| Rendering family | Direction available in the model | Current presentation | Required result |
| --- | --- | --- | --- |
| BAM/CRAM alignments | SAM reverse flag on every alignment | Terminal direction is gated on paired view and the paired flag | Every rendered alignment shows its own strand direction independently of pairing |
| Reference sequence | Forward/reverse-complement display state | Text says `forward` or `reverse strand` | Reference gutter also has an immediate left/right direction indicator |
| Genes and generic features | `FeatureRecord.strand` | Forward, reverse, and unstranded use different colours | Stranded features also use directional geometry |
| BEDPE/interact records | Endpoint strand and `directional` source/target semantics | Arcs and blocks have no arrowheads; genomic sorting obscures source/target order | Directed interactions point to the target; undirected interactions remain neutral |
| Sashimi junctions | Positive, negative, unknown, and mixed strand depths | Direction is encoded only by red, blue, purple, or grey | Pure and mixed direction remain understandable without relying on colour |
| VCF breakends | Local and partner join-before/join-after orientation | Generic variant glyph ignores breakend orientation | Breakend glyph and partner connection communicate both join orientations |

## 3. Required invariants

1. Pair status and genomic strand are independent. Pair view may change packing and connectors, but
   must not determine whether a read has a directional terminal.
2. Directional geometry is derived from biological metadata, never inferred from display colour or
   left-to-right sorting.
3. A clipped viewport boundary must not be drawn as though it were a biological read terminal.
4. Direction remains recognizable through the Bases, Reads, and Regional levels of detail without
   an abrupt shape change or disappearance at a threshold.
5. Directional elements have bounded screen-space dimensions. Zooming out must not turn a read into
   an oversized triangle; zooming in must not make the terminal disproportionately small.
6. Unstranded, unknown, or genuinely bidirectional data must not receive a misleading single arrow.
7. GPU display, CPU raster export, and SVG export must use the same direction semantics.
8. Colour may reinforce direction but cannot be its only representation.
9. Added glyphs must not change picking, selection identity, layout bounds, or label collision
   behaviour.

## 4. Shared rendering architecture

Direction is currently encoded for alignment rectangles using high object-ID bits. That mechanism
is too alignment-specific to reuse safely for feature, interaction, and breakend glyphs.

Introduce semantic shape metadata on render primitives while keeping `object_id` exclusively for
identity and picking. The shape vocabulary should initially cover:

- rectangular body;
- left- and right-facing read terminals;
- left- and right-facing chevrons;
- directional interaction target caps;
- breakend join-before and join-after markers.

Update the GPU instance representation and shader, CPU raster renderer, SVG renderer, clipping and
translation paths together. Centralize screen-space geometry and size limits so each renderer does
not independently reproduce the earlier over-triangular Regional appearance.

Repeated chevrons must be spaced in logical screen pixels, clipped to their owning connector, and
subject to a deterministic per-track primitive budget. At widths too small for repetition, use one
centred chevron when it can be rendered honestly; otherwise retain neutral geometry and expose the
strand in inspection metadata.

## 5. Workstream 1 — alignment direction

### Problem

`crates/rgv-layout/src/alignment_layout/reads.rs` currently adds a terminal only when both
`view_as_pairs` is enabled and the record has the SAM paired flag. The Regional span path has the
same conceptual restriction. This skips ordinary single-end reads, paired reads outside pair view,
long reads, spliced RNA alignments, and secondary or supplementary records even though all have an
`is_reverse` value.

### Implementation

- Derive terminal direction solely from the individual alignment's SAM reverse flag.
- Apply it in paired and unpaired presentation modes and at every alignment level of detail.
- Keep pair view responsible only for mate packing, pair connectors, and pair-level interaction.
- Place the terminal at the true aligned end. When the true terminal is outside the viewport, clip
  the body without adding a false arrow at the viewport edge.
- For spliced alignments, add the terminal only to the true terminal aligned block. Never put an
  arrow on an internal exon boundary or reference-skip connector.
- Preserve insertion, deletion, substitution, soft-clip, supplementary-chain, selection, and
  colouring overlays.
- Use a bounded terminal width and preserve a substantial rectangular body at Regional zoom.
- Make Bases-to-Reads and Reads-to-Regional transitions continuous; direction must not disappear
  when crossing a detail threshold.

### Coverage

This one implementation must cover primary, secondary, supplementary, single-end, paired,
long-read, clipped, and spliced alignments. Separate renderer-specific exceptions should not be
added for those categories.

## 6. Workstream 2 — reference orientation

RGV already persists `reference_reverse_complement` and renders the requested orientation. The
desktop gutter also labels the row as `Bases · forward` or `Bases · reverse strand`, but provides no
immediate visual direction equivalent to IGV's Sequence arrow.

- Add a non-pickable right-facing indicator for forward display and a left-facing indicator for
  reverse-complement display.
- Update the icon, accessible name, and tooltip immediately when orientation is toggled.
- Retain the textual label so the icon is not the sole source of meaning.
- Keep the indicator in the gutter; it must not distort reference base cells or translation frames.
- Do not imply that a reference is loaded when the track has no configured reference sequence.
- Retain the existing `+1/+2/+3` and `-1/-2/-3` translation-frame labels, which already communicate
  frame direction explicitly.

## 7. Workstream 3 — genes and stranded feature formats

All feature sources normalize direction into `FeatureRecord.strand`, but
`crates/rgv-layout/src/track_layout.rs` currently communicates it only through forward, reverse,
and unstranded colours.

### Transcript and blocked-feature tracks

For GFF3, GTF, genePred, refFlat, bigGenePred, BED12, blocked BigBed, and PSL:

- place repeated left- or right-facing chevrons on intron or block connector lines;
- space chevrons in screen pixels so their density remains stable while zooming;
- use no chevrons for unstranded records;
- use one centred chevron at wider Regional scales instead of dropping direction abruptly;
- add a compact terminal point to sufficiently wide simple stranded features where there is no
  connector line.

### Peaks and simple interval features

For BED, BigBed, narrowPeak, broadPeak, and bigNarrowPeak:

- show a terminal notch or centred chevron only when a `+` or `-` strand is present;
- preserve summit markers, score opacity, and existing label placement;
- leave the common unstranded case geometrically neutral.

Confirm that PSL target/query strand parsing has been normalized correctly before assigning a
genomic chevron. Do not display query orientation as though it were target genomic direction.

## 8. Workstream 4 — directional interactions

`InteractionRecord` distinguishes directed records from undirected ones and retains the strand of
each endpoint. The current renderer sorts endpoints by genomic coordinate to construct geometry,
which loses the visual source/target relationship.

- Preserve source and target identity before sorting coordinates for arc geometry.
- For `directional = true`, put an arrowhead at the target endpoint. Its screen direction follows
  the actual source-to-target geometry, including a target located to the left of its source.
- Apply the same semantics to arc and block presentations.
- For undirected BEDPE records, do not invent a source-to-target arrow. If endpoint strands are
  populated, show their orientation independently; otherwise keep both endpoints neutral.
- For cross-contig interactions, use a directional arrow only for directed records and a neutral
  join such as `↔` for undirected records.
- Retain direction when either endpoint is clipped and make the destination available through
  inspection and navigation.
- Keep selection and hit testing attached to the complete interaction rather than the new arrowhead
  primitive.

## 9. Workstream 5 — Sashimi junction direction

Junction evidence retains positive, negative, unknown, and mixed strand depths. RGV currently
encodes these as red, blue, grey, and purple arcs. That is not colour-independent, and purple hides
the fact that evidence exists in both directions.

- Add a small chevron to the horizontal crown of a pure positive- or negative-strand junction.
- For mixed positive and negative evidence, draw two offset chevrons or split the directional
  portion of the arc. Never choose one direction for a mixed junction.
- Leave unknown-strand junctions neutral.
- Preserve junction counts, annotation-state markers, tagged-versus-fallback provenance, and arc
  selection.
- Bound and deduplicate chevrons in crowded Sashimi views so the current junction primitive limit
  remains effective.

The exact positive/right and negative/left convention must be documented beside the existing
strand colour legend.

## 10. Workstream 6 — structural-variant orientation

VCF breakend parsing already preserves `local_join_after_position` and
`partner_join_after_position`, but the variant renderer ignores these fields.

- Render the local breakend as an orientation-specific bracket or notch using its join side. A
  breakend join is not a conventional strand arrow and should have its own glyph.
- When a same-contig partner is visible, connect the two breakpoints and render the partner-facing
  orientation.
- For offscreen or cross-contig partners, use an edge marker and target label/navigation while
  retaining the partner join orientation in the inspector.
- Extend inspector and export text to state both join sides rather than only partner contig and
  position.
- Support multiple declared breakend partners deterministically.

Symbolic inversions, duplications, and other SV records do not always contain enough information to
derive breakpoint orientation. Extend the parser/model for standard breakpoint-orientation or
`STRANDS` fields where present. Until that metadata exists, keep those glyphs neutral rather than
inventing an orientation from the variant type.

## 11. Explicit exclusions

The following data contains strand-associated measurements but does not describe one unambiguous
directional object:

- aggregate coverage bins;
- aggregate modified-base lanes;
- bisulfite summaries;
- SMRT kinetics summaries;
- Ultima flow-direction annotations.

Do not add a single genomic arrow to these aggregates. Coverage and modification bins can combine
both strands, while flow direction and molecular kinetics have specialist meanings that differ
from genomic left/right direction. Their inspectors and legends should continue to expose the
underlying values. Individual alignments beneath these summaries will inherit the alignment
terminal fix.

Quantitative, GWAS, copy-number, and MAF mutation tracks currently contain no renderer-level strand
semantics and require no direction glyph.

## 12. Delivery order

1. Add shared semantic shape metadata and export support.
2. Fix all alignment paths and reference orientation.
3. Add transcript, blocked-feature, simple-feature, and peak direction geometry.
4. Preserve and render interaction source/target semantics.
5. Add pure, mixed, and unknown Sashimi direction states.
6. Add breakend orientation and extend the SV model where metadata is available.
7. Update legends, accessibility descriptions, inspectors, user documentation, and the manual
   validation matrix.

Alignment and reference work are the highest-priority parity correction. Feature and interaction
direction are the next release gate. Sashimi and breakend work may land independently after the
shared primitive representation is stable.

## 13. Automated validation

### Unit and layout tests

- Forward, reverse, and unstranded records for every affected renderer.
- Alignment arrows with pair view enabled and disabled.
- Primary, secondary, supplementary, single-end, paired, long, spliced, and clipped alignments.
- True terminal offscreen versus visible.
- Forward and reverse-complement reference states.
- Simple and blocked features in each supported format family.
- Directed and undirected interactions, reversed genomic order, cross-contig targets, and clipped
  endpoints.
- Positive, negative, mixed, and unknown junction evidence.
- All four local/partner breakend join-side combinations.
- Shape continuity immediately above and below every level-of-detail threshold.
- Stable primitive counts and deterministic budget behaviour under dense records.

### Rendering parity

Assert equivalent direction in:

- the GPU window;
- CPU PNG output;
- SVG output;
- selection and inspection metadata;
- screenshots at 1× and 2× scale factors.

Tests should assert semantic shape metadata and bounded geometry rather than copying IGV pixels.

## 14. Manual and MCP qualification

Use RGV's MCP capture and workspace descriptions so every screenshot records its exact locus,
bases-per-pixel, detail level, active tracks, and relevant settings.

| Scenario | Fixture or locus | Qualification target |
| --- | --- | --- |
| Ordinary and spliced reads | `chr1:2284422-2285804` from the current comparison | Every alignment has the correct terminal independent of paired view |
| Alignment variants | `benchmark-data/manual-suite/projects/mf-01-alignment-core.rgv.json` | Paired, long, supplementary, clipped, and structural examples |
| RNA and transcripts | `benchmark-data/manual-suite/projects/mf-03-rna-sashimi.rgv.json` | Read terminals, transcript chevrons, and pure/mixed junction direction |
| Mixed track formats | `benchmark-data/manual-suite/projects/mf-10-mixed-tracks.rgv.json` | BED, GTF, genePred, PSL, BigBed, interactions, and variants |
| Reference orientation | Reference/CRAM manual scenario | Forward and reverse-complement gutter indicators |

At each alignment locus capture Bases, Reads, and Regional views while preserving the centre. Repeat
with paired view off and on. Compare biological meaning and visibility with IGV, not exact colour,
spacing, or pixel shape.

## 15. Completion criteria

This remediation is complete when:

1. ordinary alignments from the comparison source show the same strand direction communicated by
   IGV at a populated locus;
2. no alignment category loses direction merely because pair view or detail level changes;
3. reference and stranded feature tracks communicate orientation without requiring colour;
4. directed and undirected interactions are visually distinct and semantically correct;
5. pure, mixed, and unknown junction evidence cannot be confused;
6. parsed VCF breakend orientation reaches the display and inspector;
7. aggregate or unknown data remains honestly neutral;
8. GPU, PNG, and SVG results pass the same semantic tests; and
9. MCP qualification covers the manual scenarios above with no exaggerated triangles, abrupt
   direction loss, picking regression, or material performance regression.

## 16. Manual-qualification issue discovered

The MF-10 qualification pass exposed an independent vertical-scroll gutter overlap defect. It is
documented in `VERTICAL_SCROLL_GUTTER_OVERLAP_BUG.md`. Directionality implementation is not the
suspected cause, but final manual qualification must distinguish failures caused by that scroll
transform bug from directionality geometry regressions.

## 17. Implementation and qualification evidence

### Requirement audit

| Requirement | Implemented evidence |
| --- | --- |
| Shared semantics | `RectPrimitive.shape` keeps direction separate from `object_id`; GPU, CPU PNG, SVG, clipping, translation, and crop/export paths share `RectShape` semantics. |
| Alignments | Forward/reverse terminals are derived from each SAM record, independent of pair view, in Bases, Reads, and Regional layouts. Tests cover primary, secondary, supplementary, single-end, paired, long, spliced, clipped, and offscreen-terminal cases plus both LOD boundaries. |
| Reference | The gutter retains text and adds `>`/`<` orientation cues, accessible names, and tooltips for forward and reverse-complement display. |
| Features | Transcript and blocked-feature connectors use budgeted screen-spaced chevrons; simple components and peaks use compact centred cues; unstranded records remain neutral. PSL target-strand normalization remains covered by parser tests. |
| Interactions | Directed arc and block modes point to the semantic target in either genomic order; clipped targets receive outward edge cues; undirected endpoint strands remain independent; cross-contig labels distinguish `->` from `<->`; picking stays on the interaction identity. |
| Sashimi | Positive and negative junctions use right/left chevrons, mixed evidence renders both, unknown evidence remains neutral, and dense views retain the existing hard budgets. |
| Structural variants | All four BND join-side combinations reach distinct bracket geometry. Visible partners connect to a second bracket; offscreen and cross-contig partners receive bounded edge brackets and labels; multiple partners are ordered and capped deterministically. `STRANDS` parsing, inspector text, copied details, and variant TSV export retain both join sides and optional support. |
| Honest exclusions | Coverage, aggregate modification, bisulfite, SMRT, Ultima, quantitative, GWAS, copy-number, and mutation aggregates receive no invented single direction. |
| Identity and parity | Shape metadata does not alter record identity. Picking, selected/hovered GPU IDs, CPU raster pixels, SVG polygons/brackets, label collision, clipping, and standard/Retina scissor behavior have automated coverage. |

### Automated qualification

The final source state passed:

- `cargo fmt --all -- --check`;
- `git diff --check`;
- `cargo check --all-targets`;
- `cargo test --workspace` (all runnable tests passed; one pre-existing ignored test remained ignored);
- `cargo build --release -p rgv-desktop -p rgv-mcp`.

The most direct new regression tests include:

- `every_alignment_class_keeps_its_biological_terminal`;
- `alignment_direction_is_continuous_across_bases_reads_and_regional_thresholds`;
- `clipped_alignment_edges_are_not_presented_as_biological_terminals`;
- `component_features_use_compact_chevrons_instead_of_full_terminal_wedges`;
- `directed_interactions_point_to_the_semantic_target_in_either_genomic_order`;
- `directed_interactions_keep_an_outward_target_marker_when_the_target_is_clipped`;
- `mixed_junctions_render_both_directions_and_unknown_junctions_remain_neutral`;
- `every_breakend_join_side_combination_reaches_both_display_endpoints`;
- `multiple_offscreen_and_cross_contig_breakend_partners_get_bounded_edge_markers`;
- `reference_orientation_indicator_has_accessible_names_and_tooltips`;
- SVG, CPU PNG, shape-identity, clipping, and 1×/2× scissor parity tests.

### MCP and real-data qualification

All captures below were taken from settled workspaces with the release binaries and the authenticated
RGV MCP adapter:

| Scenario | Evidence |
| --- | --- |
| User-provided ONT BAM, Reads, pair view off/on | `chr6:94001-95200`, 1.6 bp/px; `/tmp/rgv-directionality-final-ont-reads-pairs-off.png` and `...-pairs-on.png` |
| User-provided ONT BAM, Regional | `chr6:90001-110000`, 26.667 bp/px; `/tmp/rgv-directionality-final-ont-regional.png` |
| User-provided ONT BAM, Bases | `chr6:94931-95020`, 0.12 bp/px; `/tmp/rgv-directionality-final-ont-bases.png` |
| Matched 1× and 2× ONT rendering | `chr6:94001-95200`; logical MCP view `1280×800` and native Retina window `2560×1600` in `/tmp/rgv-directionality-final-ont-reads-1x.png` and `...-2x.png` |
| MF-01 alignment matrix | Regional, Reads, and Bases with pair view off/on in `/tmp/rgv-directionality-final-mf01-*.png` |
| MF-03 RNA/Sashimi/transcripts | `/tmp/rgv-directionality-final-mf03.png`, `...-bases2.png`, and `...-regional.png` |
| Forward/reverse reference | MF-03 forward captures plus `/tmp/rgv-directionality-final-mf11-reverse-reference.png` |
| MF-10 formats, interactions, and variants | `/tmp/rgv-directionality-final-mf10-features.png`, `...-vcf-interactions.png`, and the final BND/interaction captures below |
| Offscreen BND partner | `/tmp/rgv-directionality-final-bnd-selected.png`, SHA-256 `86baecdc89f023e461d67d0bd9ffb7d732d5a3c0e2cca16f81d284d7c3144ef9`; both local and edge brackets, full join-side label, and inspector are visible |
| Clipped directed interaction target | `/tmp/rgv-directionality-final-interaction-clipped-target.png`, SHA-256 `879d6e911507926f98f5647171fcb7abcf39f7076c5ac7a2be99916924bb9405`; the target remains a right-facing edge cue |

The original screenshot locus `chr1:2284422-2285804` was also queried directly against the
user-specified `/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.bam`.
The settled query returned zero alignments, so it cannot qualify read geometry for that BAM. The
real-data matrix therefore uses the populated chr6 loci above, while MF-01 and MF-03 cover the
paired, supplementary, long, clipped, and spliced categories that are not all present together in
that ONT window.

The independent vertical-scroll gutter-overlap defect remains documented in
`VERTICAL_SCROLL_GUTTER_OVERLAP_BUG.md`; it is not represented as fixed by this directionality work.
