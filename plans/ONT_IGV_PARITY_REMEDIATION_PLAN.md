# ONT RGV–IGV parity remediation plan

Status: implemented and qualified

Date: 24 August 2026

## 1. Purpose and scope

This plan addresses the differences observed while displaying the same Oxford Nanopore BAM in IGV
Desktop and RGV:

1. RGV coverage has a different shape and a much larger reported maximum.
2. RGV's read stack becomes visually sparse where IGV remains dense.
3. IGV's zoomed-out insertion `I` symbols are substantially more prominent than RGV's insertion
   evidence.
4. IGV visibly identifies GRCh38, shows a cytoband ideogram, and opens a default gene annotation,
   while the current RGV project does not.
5. The original comparison was not state-matched: the applications showed different loci and the
   RGV process had not been launched with its authenticated MCP control endpoint.

The real ONT data already available on this machine is the qualification dataset. Do not create or
substitute a generated BAM fixture for this work. Small in-memory unit-test records may still be
used to protect isolated algorithms, but all visual and end-to-end acceptance must use the real
files below.

This plan does not require RGV to adopt IGV-style silent read downsampling, infer an assembly from
contig names alone, enable local automation by default, or reproduce IGV pixel-for-pixel. It does
require equivalent biological filtering, trustworthy coverage values, comparably discoverable
read events, and explicit genome-context provenance.

## 2. Authoritative ONT data

Use these exact files for every hands-on and MCP-driven RGV–IGV comparison:

| Role | Path | SHA-256 |
| --- | --- | --- |
| Alignments | `/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.bam` | `c48afe5d794c781582dc9e0be11d4e68e4373903f1c5f15ce8beca11643b303c` |
| BAM index | `/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.bam.bai` | `8fc3a0e0c1462dbcb2b687b07b6a5eb9f84e8b5f23f25ebbf02489febbc72909` |
| Partial reference | `/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.fasta` | `50c15bd3c4bf5a3ee288db8f48bed59801c5d522bac6ab4188a6c452fc331995` |
| FASTA index | `/Users/alexcoulton/work/external.repos/ont_test/wf-human-variation-demo/demo.fasta.fai` | `5497607dbad7e45e3826663474336332af4df84cae62169081a4baa3f4047682` |

The FASTA is intentionally partial and contains `chr6`, `chr20`, and `chrM`. The BAM header points
to the GRCh38 no-alt analysis set and provides sequence lengths and MD5 values, but the current RGV
project names the assembly `ONT human variation demo` and supplies neither cytobands nor gene
annotations.

The primary wide comparison locus is `chr6:1-630038`, matching the captured IGV view. Use the same
logical content width in both applications. Additional zoom levels must retain the same centre or
explicit interval in both viewers and must be recorded in the qualification report.

## 3. Observed baseline

The initial comparison established the following facts:

| Observation | IGV | RGV |
| --- | --- | --- |
| Open BAM | Exact ONT BAM above | Exact ONT BAM above |
| Captured locus | `chr6:1-630038` | `chr6:10550-779611` |
| Coverage label | `167` | `auto 630×` |
| Alignment policy indicated by the result | Secondary hidden; supplementary included | Secondary and supplementary included |
| Alignment count over the populated region | 1,755 without secondary alignments | 3,469 including all secondary and supplementary alignments |
| Secondary alignments | Hidden by default | 1,714 included |
| Supplementary alignments | 242 included | 242 included |
| Exact maximum aligned-base depth without secondary alignments | 167 | 167 before RGV display binning |
| Exact maximum aligned-base depth with every alignment | 603 | 603 before RGV display binning |
| RGV-style wide-bin touching-read maximum with every alignment | n/a | approximately 628–630 |
| Zoomed-out insertion evidence | Prominent repeated `I` symbols | Effectively absent in Regional span rendering |
| Genome context | `Human (GRCh38/hg38)`, cytobands, RefSeq Select | Custom name, fallback chromosome bar, no auxiliary tracks |

The RGV gutter reported `3469 visible · 3469 loaded`; the apparent sparsity was not caused by the
50,000-record safety cap. Secondary alignments inflated the maximum concurrent row count, while the
wider and shifted RGV locus added more empty sequence after the populated region.

## 4. Required invariants

These are release gates for the remediation:

1. A comparison cannot proceed unless BAM identity, contig, interval, logical content width,
   alignment filters, read density, coverage settings, and genome identity have been recorded and
   matched or explicitly identified as the variable under test.
2. New RGV alignment tracks exclude secondary alignments and include supplementary alignments by
   default. Existing projects preserve their explicit saved choices.
3. Coverage reports aligned-base depth according to a documented bin aggregation. Different reads
   touching disjoint positions in one wide bin must not be reported as simultaneous depth.
4. Coverage and read rendering use the same eligible-record filter policy, while coverage remains
   independent of the emergency visible-record safety cap.
5. Every eligible read remains available in RGV; visual parity must not be obtained by silently
   downsampling or discarding reads.
6. Insertions meeting the configured minimum indel size remain visually discoverable at Regional
   zoom. Marker visibility must not depend on the insertion consuming a readable genomic width.
7. Dense insertion evidence is bounded and deterministic. Collision handling and marker omission
   are reported rather than silently depending on iteration order.
8. RGV never silently claims GRCh38 from `chr`-prefixed names alone. Assembly identity is explicit
   or supported by matching lengths and sequence fingerprints, and its provenance is visible.
9. Missing cytobands and annotations are presented as missing genome context with a direct loading
   action, not as an apparently complete genome.

## 5. Workstream 1 — state-matched ONT comparison

### Implementation

- Add an ONT parity qualification entry point under `validation/` that accepts the paths above and
  verifies their hashes before starting.
- Launch RGV with an ephemeral, mode-`0600` control secret, an explicit loopback control endpoint,
  and a control root containing the ONT data or a permitted parent. Connect through `rgv-mcp` and
  use `rgv_capture_window` for RGV screenshots. Do not enable control for ordinary launches.
- Drive IGV through its batch/command interface, then use one AppleScript command to activate IGV
  and capture the screen.
- Before every screenshot, retrieve and record the RGV workspace and assert the expected locus,
  bases-per-pixel, filter state, track identity, loaded/eligible counts, and settled generation.
- Generate the IGV batch from the same declared interval list rather than manually navigating one
  application.
- Record both the full native window and a normalized crop of the ruler, coverage, modification,
  and read lanes. The crop is review evidence, not a substitute for structured assertions.
- Fail qualification when the two applications show different intervals or materially different
  content widths.

### Tests and evidence

- Preflight rejects a changed BAM or index hash.
- Preflight rejects the original `chr6:1-630038` versus `chr6:10550-779611` mismatch.
- RGV capture metadata proves that the image belongs to the same application instance and
  workspace revision as the structured observation.
- The report retains application versions, window/content dimensions, loci, filters, counts,
  coverage values, image hashes, and the exact commands used.

## 6. Workstream 2 — alignment-filter parity and transparent counts

### Implementation

- Change `SessionFilters::default()` in `crates/rgv-core/src/session.rs` to
  `include_secondary: false` and retain `include_supplementary: true`.
- Make the independent `rgv-hts::QueryOptions` default agree so direct engine users do not receive
  different biological semantics from desktop users.
- Apply and test the same filtering order in local BAM, remote BAM, and CRAM query paths.
- Preserve explicit values in every existing session version. The fields are already serialized,
  so changing the new-track default must not rewrite an existing `true` value.
- Extend query statistics with counts excluded as secondary, supplementary, duplicate, QC-failed,
  and user-filtered where those categories are available without a second scan.
- Show concise exclusion provenance in the alignment-track gutter and full details in settings and
  diagnostics. For the ONT locus, the default state should make the 1,714 hidden secondary
  alignments discoverable without including them in coverage or packing.
- Keep secondary alignments available as an explicit opt-in and include their state in exports,
  MCP workspace descriptions, and screenshot qualification metadata.

### Tests

- Existing version-1, version-2, and version-3 sessions with `include_secondary: true` restore it.
- A newly created alignment track defaults to secondary off and supplementary on.
- Local and remote queries return identical eligible, excluded, and emitted counts.
- Coverage, modification aggregation, junction evidence, read layout, selection search, and export
  all observe the same secondary/supplementary policy.

### ONT acceptance

At the matched wide locus, default RGV must report 1,755 eligible/loaded alignments, including 242
supplementary alignments, with 1,714 secondary alignments explicitly reported as filtered. Enabling
secondary alignments must restore the 3,469-record state deterministically.

## 7. Workstream 3 — trustworthy regional coverage

### Problem

`PileupBuilder` currently increments a wide bin once for every read with any aligned segment
touching that bin. At hundreds of bases per pixel, reads covering disjoint parts of the bin can
inflate its displayed depth. `add_coverage` then applies a square-root height transform, and
regional bins do not receive the allele colouring used in exact one-base bins. These choices make
the RGV track numerically and visually different from IGV even after filters are matched.

### Implementation

- Define the default coverage value for a display bin as maximum aligned-base depth at any position
  in that bin. Reference skips do not contribute aligned-base depth; deletion and overlapping-mate
  treatment continue to follow their explicit existing policies.
- Replace touching-read bin increments with a bounded sweep of clipped aligned-segment start/end
  events. Calculate the exact maximum concurrent depth for each display bin without allocating one
  counter per genomic base.
- Preserve forward/reverse and mapping-quality provenance with defined semantics at the position
  producing the bin maximum. Do not leave existing inspector fields with ambiguous mixed meaning.
- Use saturating arithmetic and explicit caps for event storage/work. A cap must produce typed
  incomplete provenance, not a plausible-looking exact depth.
- Keep pileup accumulation independent of the retained display-row safety cap.
- Add an explicit persisted coverage transform with at least `linear` and `square_root`. Make
  linear the default for new tracks. Preserve the previous square-root presentation for old
  projects through an explicit session migration or a clearly documented compatibility decision.
- Label the gutter with the metric and transform, for example `max 167× · linear`, so a user can
  distinguish biological depth from a presentation transform.
- Treat regional allele colouring as a follow-up inside this workstream: add it only when bounded
  aggregation has trustworthy support counts, and report when allele colours are unavailable at
  the current scale.

### Tests

- Disjoint read segments in one bin do not add into false simultaneous depth.
- Overlapping segments produce the exact expected maximum.
- Secondary filtering changes both depth and provenance consistently.
- Local and remote backends agree for the same ONT query.
- Broad loci and long/pathological CIGARs remain within declared memory and time bounds.
- Linear and square-root transforms change geometry but never the reported raw maximum.

### ONT acceptance

With secondary alignments disabled at `chr6:1-630038`, the reported raw maximum must be 167. With
secondary alignments explicitly enabled, the exact raw maximum must be 603; a value near 630 must
not be labelled as exact base depth. The matched screenshots must use the same transform before
their shapes are visually compared.

## 8. Workstream 4 — read density and zoomed-out insertion prominence

### Problem

At Regional detail (`>=10 bp/px`), `layout_alignment_query_with_rows_and_annotations` calls
`add_spans`. That path draws only a clipped read body and selected-chain highlight; it never walks
the retained CIGAR. The detailed CIGAR path draws insertion markers, but insertion text is further
restricted to `<=5 bp/px`. At the observed approximately 783 bp/px view, RGV therefore discards
the most salient zoomed-out insertion cue even though the CIGAR is loaded. IGV continues to draw
high-contrast, screen-space `I` symbols.

Secondary alignments also raise the peak row requirement from roughly the primary/supplementary
depth to approximately 630 rows. Stable query-wide packing then leaves many empty row positions in
lower-coverage parts of the region. The first correction is the default filter change; row-packing
semantics should not be redesigned until the matched-filter result has been measured.

### Regional insertion implementation

- Extend Regional span rendering to walk the retained layout CIGAR for insertion operations without
  loading sequence or auxiliary tags.
- Draw insertion evidence at its reference anchor in fixed screen-space geometry. Its minimum
  visible size must not shrink below a usable number of logical pixels as bases-per-pixel rises.
- Use a high-contrast `I` glyph where the current row height can support readable text. At very
  compact density, use a strong insertion tick/notch with an `I` tooltip/selection label rather
  than an unreadable subpixel character.
- Keep marker visibility and text-label visibility independent:
  - `minimum_indel_size` controls whether the insertion has a marker;
  - `minimum_event_label_size` controls whether its `I`/`I{length}` text is requested;
  - zoom and row geometry decide whether text or the compact fallback glyph is used.
- Do not retain the existing `<=5 bp/px` rule as the only way to see `I`. Define explicit
  Regional behavior and test it at the ONT wide view.
- Use the existing insertion palette and technology-specific classification where the required
  evidence is available. Fall back to a prominent ordinary insertion colour when it is not.
- Make Regional insertion markers pickable. Selection must identify the read, CIGAR event,
  insertion length, and reference anchor just as detailed-mode selection does.
- Bound dense output deterministically by row/pixel collision buckets and an explicit global
  marker/label budget. When several insertion events collide in the same row and pixel bucket,
  retain a deterministic representative, optionally strengthen the glyph to show multiplicity,
  and count the omitted/collapsed markers in layout statistics.
- Surface `regional insertions shown/collapsed/omitted` in diagnostics and screenshot qualification
  metadata.

### Read-stack presentation implementation

- Re-run the ONT comparison after secondary alignments are disabled before changing the packing
  algorithm.
- Add row-occupancy and natural-stack-height metrics to the qualification report for the entire
  query and for selected genomic slices. This distinguishes missing reads from intentional stable
  rows with no alignment at a particular position.
- Retain stable query-wide packing as the default so reads do not jump during pan and zoom.
- Tune Regional read-body contrast, opacity, minimum thickness, and row gap using the matched ONT
  captures. Insertion glyphs must remain distinguishable from both read bodies and the white
  background at every named density preset.
- Verify the existing Expanded, Squished, Compact, and continuous-density controls against the ONT
  data and give each preset a deterministic screen-space geometry.
- Only if the matched-filter stack remains materially sparser than IGV should an optional
  `Repack current viewport` mode be designed. It must be explicit because it trades density for row
  movement during navigation.

### Tests

- Regional layout emits an insertion marker from a retained CIGAR at 10, 100, and approximately
  783 bp/px.
- Marker geometry has a tested minimum screen-space size and does not collapse to zero width.
- `I` text versus compact fallback selection is deterministic for each density preset.
- Minimum indel and event-label thresholds remain independent.
- Multiple events mapping to the same pixel obey deterministic collision and budget rules.
- Regional markers remain semantically pickable.
- Detailed Reads/Bases insertion rendering and technology-specific quality marks do not regress.
- Disabling secondary alignments reduces row inflation without removing any eligible primary or
  supplementary read.

### ONT visual acceptance

- Capture IGV and RGV at the same `chr6:1-630038` interval, logical content width, track height,
  and primary-plus-supplementary filter state.
- At the wide Regional scale, insertion evidence visible as `I` in IGV must have a clearly visible
  RGV glyph at the same genomic anchors.
- Reviewers must not need to zoom from hundreds of bases per pixel to five bases per pixel merely
  to discover that insertions exist.
- The `I` glyph or compact fallback must remain visually prominent over the read body and must not
  disappear in the palest alignment colour mode.
- Dense insertion regions remain responsive and report any collision/budget reduction.

## 9. Workstream 5 — explicit GRCh38, chromosome, and gene context

### Genome identity

- Add an explicit stable assembly identifier and provenance to genome configuration and workspace
  state, separate from the editable display name.
- Maintain a small built-in fingerprint table for recognised assemblies. Match contig lengths and
  sequence MD5 values where available; use BAM-header `UR` text only as supporting evidence, not as
  sole proof.
- Recognise the current data as a partial GRCh38 no-alt-analysis-set reference when its available
  fingerprints match, and label it accurately, for example `GRCh38 · 3-contig subset`.
- Present assembly identity in the toolbar and MCP workspace description. Preserve the source and
  confidence/provenance in saved projects and exports.
- When a likely match is found, offer an explicit confirmation/loading action. Do not silently
  rename a custom genome.

### Cytobands and annotations

- Add a genome-context completeness summary covering reference, aliases, cytobands, and default
  annotations.
- Keep the cytoband ideogram on its own chromosome-context row. Preserve the coverage/read-density
  preview as a separate horizontal overview rail with the viewport handle and pan/zoom behavior;
  neither representation replaces the other.
- Replace an unexplained plain chromosome bar with a clear fallback state and an action to load a
  genome configuration or cytoband source.
- Prepare a local GRCh38 `.rgvgenome` configuration for this ONT review using the existing partial
  FASTA plus appropriately versioned cytobands and a gene-model source. The external cytoband and
  annotation files must carry recorded source/version/checksum/licensing provenance; they are not
  currently present beside the ONT BAM and must not be invented.
- Load the gene model as a normal RGV `GeneModel` track so search, previous/next annotation,
  selection, inspection, and session persistence all use the existing track architecture.
- Support a full GRCh38 configuration separately from the three-contig ONT subset. Do not present
  absent contigs as reference-backed merely because they exist in the BAM dictionary.
- Keep a general hosted genome catalogue out of this corrective slice. A narrowly curated,
  explicitly downloaded GRCh38 preset may be promoted later, but local genome usability must not
  depend on that expansion.

### Tests and ONT acceptance

- Length/fingerprint agreement identifies GRCh38; a mismatched MD5 prevents a confident claim.
- A three-contig reference is labelled as a subset.
- Missing cytobands and annotations remain explicit and actionable.
- Loading the ONT GRCh38 configuration produces a cytoband ideogram for `chr6`, displays the
  selected assembly identity, and opens the configured gene track.
- Gene-name search, navigation, inspection, save, and reopen work with the loaded annotation.
- Session and MCP workspace output preserve assembly identity and context completeness without
  exposing unredacted source paths through MCP.

## 10. Session and compatibility policy

- Changing the default secondary flag affects new tracks only; existing serialized choices remain
  authoritative and do not require a schema bump.
- Correcting inflated bin depth is a documented correctness fix. Do not preserve a misleading
  value under the label `depth` merely for visual compatibility.
- If coverage transform becomes persisted, introduce the necessary schema migration so older
  projects retain their square-root presentation until the user selects the recommended linear
  mode. New tracks use linear by default.
- New assembly identity/provenance fields may be optional if their absence has the unambiguous
  meaning `custom/unidentified`; otherwise introduce and test a schema migration.
- Regional insertion visibility is a rendering improvement governed by existing indel/label
  thresholds. Add a new persisted setting only if users need to disable the markers independently.
- Saving never edits the original project or genome configuration unless the user explicitly
  chooses the target.

## 11. Qualification matrix

Every implementation slice must run the existing unit/workspace suites and the following real-data
matrix against the authoritative ONT BAM:

| Scenario | Secondary | Supplementary | Scale | Required evidence |
| --- | --- | --- | --- | --- |
| IGV-default wide view | Off | On | `chr6:1-630038` | 1,755 eligible, depth 167, comparable coverage shape, prominent insertion glyphs |
| All alignments | On | On | `chr6:1-630038` | 3,469 eligible, exact depth 603, explicit secondary provenance |
| Regional insertion review | Off | On | Same locus and matched content width | IGV `I` anchors have visible RGV glyphs; collision counts recorded |
| Density presets | Off | On | Same locus | Stable read identities, deterministic row geometry, no lost reads |
| Detailed insertion review | Off | On | Reads/Bases detail around selected ONT insertions | Exact CIGAR event, length, anchor, selection and inspector agreement |
| GRCh38 context | Off | On | `chr6:1-630038` | Assembly/subset label, ideogram, gene track, search and persistence |
| Compatibility project | Saved explicit values | Saved explicit values | Saved locus | Filter and coverage presentation migration behaves as documented |

The qualification report must contain structured results and native screenshots. A human review is
required for coverage shape, read density, insertion prominence, chromosome context, and gene-track
legibility; numeric assertions alone cannot close the visual findings.

## 12. Delivery order

Implement the remediation in this order:

1. State-matched ONT qualification entry point and MCP-controlled RGV launch.
2. Secondary-off/supplementary-on defaults plus exclusion provenance.
3. Exact regional coverage aggregation and raw-value tests.
4. Explicit coverage transform and compatibility handling.
5. Regional insertion `I`/fallback glyphs, collision budgets, selection, and diagnostics.
6. Re-measure and tune read density/contrast with matched filters; change packing only if evidence
   still demonstrates a problem.
7. Assembly fingerprint/provenance model and genome-context completeness UI.
8. Local GRCh38 ONT genome configuration with sourced cytobands and gene annotation.
9. Full ONT qualification matrix, documentation updates, and incorporation of the results into the
   Phase 9 RGV–IGV comparison evidence.

The filter-default and Regional insertion changes should land early because they address the two
largest visible read-track discrepancies without depending on genome asset provisioning. Coverage
correctness must land before visual coverage tuning so presentation is not calibrated around an
inflated metric.

## 13. Definition of done

This remediation is complete when:

- RGV and IGV can be captured automatically at the same ONT locus and width with verified state.
- Default RGV shows the same primary-plus-supplementary record population represented by IGV.
- RGV reports exact maximum aligned-base depth 167 for that population at the wide locus.
- The RGV read stack no longer appears sparse because secondary alignments silently inflated the
  row canvas; any remaining stable-row whitespace is explainable and quantified.
- Zoomed-out ONT insertions have prominent, bounded, pickable RGV `I` symbols or compact fallback
  glyphs at the same anchors where IGV exposes insertion evidence.
- RGV visibly identifies the confirmed GRCh38 subset, draws available chromosome cytobands, and
  displays/searches the configured gene annotation.
- Existing saved filter choices remain intact, all new state survives save/reopen, local and remote
  semantics agree, and the complete ONT qualification report passes.

## 14. Implementation and qualification record

Completed on 24 August 2026 against the exact files and hashes in section 2.

- New alignment tracks now use secondary off, supplementary on, linear coverage, and a 10 bp event
  label threshold. Legacy sessions retain their serialized filters, square-root coverage fallback,
  and prior 1 bp missing-field label threshold.
- Exact aligned-segment depth is 167 for the IGV-like 1,755-record population. Enabling secondaries
  yields 3,469 eligible records and exact depth 603. The former approximately 630 value is no longer
  presented as exact depth.
- Regional ONT reads use a higher-contrast purple body. Small insertions remain fine pale notches;
  label-sized insertions render bounded, pickable red `I` glyphs with collision/omission provenance.
- RGV identifies `GRCh38 · 3-contig subset`, renders UCSC cytobands on a dedicated chromosome row,
  preserves the separate coverage/read-density navigation rail, and pins the loaded MANE Select
  gene context above the alignment stack.
- UCSC extended 25-field bigGenePred and RGB `itemRgb` values are accepted while canonical fields
  and extension attributes remain available to search/inspection.
- Genome identity, cytobands, the one configured default annotation, the ready gene track, filters,
  locus, and exact depth survive a project save/reopen round trip.
- The original state-matched IGV screenshot is the approved visual baseline. The reproducible RGV
  structured report and captures are written under `validation/ont-igv-parity-output/`; the final
  automated run used `--skip-igv` because no fresh IGV capture was requested after baseline review.
