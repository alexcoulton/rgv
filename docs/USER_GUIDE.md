# RGV user guide

RGV is a native genome viewer for local and remote indexed alignment data. It accepts BAM/BAI or
CSI, CRAM/CRAI plus a reference when required, FASTA/FAI, BED/BED12, GFF3, GTF,
genePred/refFlat, narrowPeak/broadPeak, PSL, GWAS/PLINK association summaries, indexed
bigGenePred/bigNarrowPeak, indexed VCF/BCF,
BigWig, TDF, bedGraph, WIG, generic BigBed, BEDPE/interact, and SEG/CBS copy-number matrices.
Cancer Mutation Annotation Format (`.maf`, `.maf.txt`, and `.maf.gz`) files are also supported; UCSC Multiple
Alignment Format happens to share the `.maf` suffix but is not yet supported.

For agent-driven testing of the visible desktop window, use the separate opt-in `rgv-mcp` adapter.
Its protected-secret setup, host configuration, complete tool contract, coordinate rules, privacy
bounds, and troubleshooting are documented in [Controlling RGV from an MCP agent](MCP.md).

## Opening data

**Hosted genomes.** **Genomes → Hosted genomes** lists the genomes published for IGV (human,
mouse and many others). Choosing one loads its reference sequence, chromosome ideogram, chromosome
aliases (so a BAM using `1` displays against a `chr1` reference) and default gene track. The
reference is read on demand over the network; the small resources are saved locally the first
time, so the same genome opens offline afterwards. `--genome hg38` does the same at launch, and
also accepts the path or URL of an IGV genome JSON file. Such a file can be opened with
**Genomes → Open genome configuration** or dropped onto the window. References may be FASTA with
a `.fai`, or UCSC `.2bit`. This feature has automated tests only and has not yet been checked
against the live hosting.

A small remote BED, GFF3, GTF, genePred, refFlat, narrowPeak, broadPeak or PSL file (up to
256 MiB) can be opened by URL; it is downloaded whole rather than read by range request.

**Built-in gene landmarks.** When a landmark catalogue is installed and the loaded reference is
positively matched to one of its assemblies, RGV adds a **Genes** track automatically, uses the
bundle's gene-name index for symbol search, and can take cytobands and chromosome aliases from the
same bundle. The **Genomes** menu shows the match as a `Landmarks: …` line (status, evidence and
reason), or an error line when no catalogue was found. If you remove the automatic Genes track,
**Genomes → Restore default genes** brings it back; **Genomes → Open landmark resource folder**
opens the catalogue directory.

RGV looks for a directory containing `catalog.json` in this order: `--landmark-resource-root`,
the `RGV_LANDMARK_RESOURCE_ROOT` environment variable, then
`benchmark-data/large-public-alignments/landmarks/igv` and `resources/landmarks` relative to the
working directory, then `landmarks`, `resources/landmarks` and `../Resources/landmarks` beside the
executable. The catalogue is **not tracked in Git and the packaging scripts do not currently copy
it into the staged application**, so on a fresh checkout or packaged build there is no automatic
Genes track until you build the bundles (hg38, hg19, mm10, mm39 and hs1; see
[Curated gene landmark bundles](LANDMARK_BUNDLES.md)) or point RGV at an existing catalogue.
Without one, the Genomes menu reports "No built-in landmark catalogue was found; automatic Genes
tracks are unavailable" and everything else works as usual. Hosted genomes (above) supply their
own default gene track and do not depend on the landmark catalogue.

Use **File → Open BAM/CRAM** for an alignment source, **Genomes → Open FASTA** for an indexed
reference genome, and **Tracks → Add track** for annotations, variants, or signal. Use
**File → Open URL** for remote BAM/CRAM, a remote FASTA, or an indexed remote VCF/BCF, BigWig, TDF,
BigBed, bigGenePred, or bigNarrowPeak track. The URL dialog accepts non-conventional BAI/CSI/CRAI
and FAI URLs and can open a CRAM's remote reference in the same action. Each alignment's custom
index and the reference index are retained in an RGV project, so reopening several remote samples
does not incorrectly reuse the first sample's index. A FASTA and its
adjacent `.fai` can be opened without any BAM/CRAM; its dictionary drives chromosome navigation and
its independently loaded reference track stays pinned above the scrollable data tracks. Opening or
dropping another BAM/CRAM adds it beneath the existing alignment
tracks; it no longer replaces the first. Files can also be dropped onto the window. HTTP/S URLs are
accepted in the desktop URL dialog, on the command line, automation, and in sessions. Remote BAM,
reference-dependent CRAM, and FASTA use
RGV's bounded validator-aware range cache. BAM tries both conventional BAI/CSI names and CRAM tries
both conventional CRAI names; `--remote-index-url` supplies a nonconventional BAI, CSI, or CRAI.
`--remote-reference-index-url` supplies a nonconventional FAI for a remote reference. A CRAM
reference may be local or remote. Runtime bearer authentication, bounded redirects and retries,
generation cancellation, and request/byte/cache counters apply to remote BAM/CRAM/FASTA. RGV
range requests with configured bearer or custom-header authentication reject redirects outside
their original scheme/host/effective-port origin. Same-origin redirects retain authentication.
Speculative read-ahead is capped to cache capacity, even when configured minimum read-ahead is larger.
Indexed VCF/BCF uses its conventional adjacent index through HTSlib; BigWig, TDF, and BigBed-derived tracks
use RGV's bounded range reader. Signed source query strings are preserved during I/O, while project
persistence removes them. RGV reports missing indexes, dictionary mismatches, malformed data,
changed objects, and unsupported range servers without crashing the UI thread.

To inspect the implemented track families without locating separate example files, open the
checked-in project from the repository root:

```sh
target/release/rgv-desktop --session benchmark-data/demo-tracks/rgv-demo-project.json
```

This workspace contains the synthetic variant alignment/reference truth and 19 auxiliary examples
covering compressed annotation, transcript, peak, PSL, GWAS, BCF/genotype, quantitative, TDF,
interaction, copy-number, mutation, and indexed BigBed-derived tracks. The project starts at
`chrVariants:8501-16000`; `chrVariants:9941-10080` is the closest base-level alignment view. The
generator refuses success unless every auxiliary source opens and returns data through RGV's track
engine.

For a ready-made four-panel alignment review spanning paired reads, RNA junctions, long and
supplementary reads, modified-base/SMRT/SBX evidence, and a pathological 70,000-operation CIGAR,
open:

```sh
target/release/rgv-desktop \
  --session benchmark-data/demo-tracks/rgv-alignment-review-project.json
```

Use the Review queue in that project to reach the two additional deep-coverage and structural-pair
truth loci.

The locus field uses one-based inclusive display coordinates such as `chr1:1,000,001-1,100,000`.
Enter several whitespace-separated coordinates to open ordered side-by-side panels, for example
`chr1:1,000,001-1,100,000 chr6:31,000,001-31,100,000`. RGV validates the complete expression
before replacing the current panel arrangement. A workspace supports at most 16 panels and refuses
an additional arrangement when the window is too narrow to keep every panel readable. Internally,
all calculations use zero-based half-open intervals.

Every place that accepts a locus (the locus field, `--locus`, automation `goto`, IGV import) uses
one grammar. Accepted forms are a bare contig (`chr1`, the whole contig), a position
(`chr1:12,345`, shown with a 20 bp flank), a range (`chr1:100-200`), and the tab- or
space-separated forms pasted from a table (`chr1<TAB>12345`, `chr1 100 200`), which are read as a
single locus with one-based coordinates. Contig names resolve case-insensitively (`CHR1`, `chrx`)
and across naming conventions (`1`/`chr1`, `MT`/`chrM`) when exactly one contig matches. A contig
whose own name contains colons, such as `HLA-A*01:01:01:01`, is recognised from the reference
dictionary, so `HLA-A*01:01:01:01:1-100` navigates and the locus strings RGV itself writes for
such contigs can be pasted back. Text that is not a locus, such as a gene symbol, a variant ID
like `chr1:12345:A:G`, or an HGVS expression, is sent to ID and annotation search instead; text
that is recognisably a locus but wrong (`chr1:0`, an unknown contig with a range) reports why.
Back/Forward history records contigs by name, so it returns to the right contig even when an
alignment header orders contigs differently from the reference, and in track-only workspaces
that have no dictionary.

**Genomes → Open genome configuration** loads a portable schema-versioned JSON bundle containing
the FASTA, assembly name, exact contig aliases, optional cytobands, and default annotations. Local
paths are relative to that file. See [Portable genome configuration](GENOME_CONFIGURATION.md).
Aliases can also be supplied as repeatable `--contig-alias ALIAS=CANONICAL` arguments. They are
exact and persisted in sessions; conflicts and aliases targeting absent FASTA contigs are errors.

## Navigation and inspection

The locus field also searches open text annotation tracks. Type at least two characters that are
not a coordinate locus to autocomplete feature names, gene names, aliases, IDs, and transcript IDs. Exact matches
are preferred; a unique exact match opens on Enter, while ambiguous matches remain as explicit
choices with their track and coordinates. Gene results lead with the human-readable gene symbol;
stable gene and transcript accessions remain visible as secondary identifiers and remain directly
searchable. Symbol matches rank before aliases and stable identifiers. Search indexing and prefix
scans are bounded and run in the background; the results panel reports when a very broad search may
omit matches.
Submitting a name also searches VCF/BCF record IDs. That bounded ID index is created lazily on the
background track worker, so opening a variant track does not add a whole-file scan to startup.
Use **Regions → Previous annotation feature** or **Next annotation feature** to move to the nearest
feature boundary on the active contig across all searchable text annotation tracks.

Open the reference track settings to display the reverse strand and enable any combination of the
three forward and three reverse translation frames. Translation uses absolute genomic phase and an
explicit **Standard** or **Vertebrate mitochondrial** genetic code. Ambiguous codons render as
`X`; stop codons render as `*`. These choices and the strand-aware visible-reference copy action are
persisted in sessions. The reference gutter begins with `>` for the forward display or `<` for the
reverse-complement display and retains the corresponding text label, so orientation is not conveyed
by colour alone.

### Regions of interest and review queues

Use **Regions → Add current view as region** to retain the active panel's locus. **Manage regions**
can rename and recolor a region, replace its coordinates with the current view, navigate to it, or
delete it. Regions are drawn as clipped translucent spans with boundaries across every visible
track in every panel showing the same contig. Import and export use ordinary zero-based, half-open
BED; BED `itemRgb` colors are retained. An import is limited to 64 MiB and 100,000 regions, and its
file read runs outside the UI thread.

**Add current view to review list** creates a pending locus item. A selected annotation or gene can
also be added directly from its feature inspector, preserving the feature name and exact interval.
The **Review queue** edits the label, status, notes, and optional assignment; its status filter also
controls deterministic wrapping Previous/Next navigation. Review lists are limited to 100,000
items with 16 KiB notes. Versioned RGV review TSV import/export preserves multiline notes without
placing file I/O on the UI thread.

Regions and review items survive JSON session save/reopen. RGV records only the explicitly named
locus, status, notes, and assignment: it does not automatically place selected read names, bases,
or sequence into review state.

### Sample metadata and cohort track actions

Use **Tracks → Import sample attributes** to load a tab-separated table. The first column is the
stable sample key; every remaining named column is a generic attribute. Duplicate keys or columns,
ragged rows, overlong fields, more than 100,000 samples, more than one million cells, or more than
64 MiB of input are rejected explicitly. File reading runs outside the UI thread.

Open **View → Sample metadata** for the resizable, virtualized table. Each alignment or auxiliary
track can be associated with an exact sample key in its track settings. Unresolved keys remain
visible rather than being silently reassigned. Choose an attribute and value to select every
associated matching track across track types, or adjust the checkboxes directly.

The metadata panel applies only compatible bulk operations: show/hide, expand/collapse, rename to
sample keys, height where track geometry permits it, explicit full alignment settings and filters,
quantitative autoscale/overlay/separation, same-type auxiliary appearance and colors, and confirmed
two-step removal. Alignment, coverage, junction, and modification projections share their parent
alignment track's sample association. Metadata, associations, and settings survive session save and
reopen.

When a metadata table contains `father_id`/`mother_id` (or recognized equivalent columns), variant
settings can order genotype rows by pedigree: child first, then father, mother, and other family
members. If parent columns are unavailable, RGV states that it is retaining VCF header order.

- The compact navigation toolbar keeps assembly, contig, locus, history, and zoom controls together.
  The chromosome row uses a cytoband ideogram when `--cytobands` is supplied and a fallback bar
  otherwise. Rounded chromosome ends and an indented waist identify the centromere wherever the
  cytoband source provides `acen` records. Hovering any band highlights it and reports its name,
  one-based coordinate range, and stain. A separate coverage/read-density overview rail directly
  below it retains the blue visible-span handle and horizontal pan/zoom interactions. The ideogram,
  density bins, visible-span handle, and pointer navigation use the same full-contig horizontal
  axis, so positions align exactly between the two rows. Coordinate ruler labels always use
  one-based display positions.
- The chromosome overview supports Ableton-style direct navigation: drag left/right to pan, drag
  down/up to zoom in/out around the press position, click to recenter, and double-click to show the
  complete contig. Indexed BAM sources contribute a stable, logarithmic whole-contig occupancy
  preview derived from BAI/CSI metadata, so zooming into a small query does not erase the rest of
  the navigation map. At Overview detail the alignment lanes also settle in this index-only state:
  RGV does not request or decode BAM/CRAM records until the viewport returns to Regional, Reads, or
  Bases detail. This preview estimates alignment density without scanning every record;
  sources without a spatial index summary fall back to loaded alignment coverage, where a faint
  band distinguishes the queried span from unqueried portions of the contig. Pan and zoom can be
  combined in one diagonal gesture. Horizontal movement is
  scale-aware: broad views receive extra pan gain for fast chromosome traversal, and movement
  becomes progressively finer as the gesture zooms into a smaller genomic span. During a drag the
  pointer is hidden and captured, so movement continues after it would otherwise reach a screen
  edge; it is restored on release. The visual camera retains fractional-base position at extreme
  zoom while data queries remain integer genomic intervals, keeping fine panning continuous.
  Horizontal zoom changes genomic scale only: reference, coverage, and read-row vertical anchors
  remain fixed, and read density is controlled exclusively by the right overview. Read rows are
  assigned over the bounded retained query before viewport clipping, so existing reads keep their
  vertical positions from chromosome scale through Regional, Reads, and Bases detail. Refined
  queries retain assignments for shared read identities and place newly admitted complete reads around them.
  Navigator handles capture immediately on press; at extreme zoom their enlarged hit targets stay
  relative to the current viewport, while a stationary click elsewhere on the rail recentres.
  Horizontal navigation is transactional. RGV immediately transforms the last committed genomic
  canvas while coalescing background queries, then replaces it in one frame only after the exact
  target reference, tracks, complete reads, and layout have settled. The transformed image is a
  non-interactive orientation aid: it cannot be selected, inspected, exported, or reported as
  current evidence. Uncovered zoom-out flanks are neutral rather than stretched. Where every
  visible read track has a complete, policy-matching canonical horizon that contains the target,
  RGV may overlay accurate target-oriented rows with no object IDs; coverage remains retained
  preview pixels until commit. A delayed `Updating view…` notice appears only for a sustained
  transaction, and commits never fade. Resize, pane, density, vertical-scroll, track-presentation,
  or display-scale changes invalidate an incompatible preview safely.
- Drag horizontally to pan and use the wheel or trackpad to zoom around the pointer.
- Annotation tracks occupy the upper pane and reads occupy the lower pane. They are independent
  presentation channels even when both rows refer to the same alignment source. Every row has an
  eye that controls its content without removing its restore header. Groups have a separate eye
  (parent visibility gate) and chevron (expand/minimize); changing either preserves each child's
  individual eye state. Configurable auxiliary annotations expose thin
  horizontal resize boundaries in the left label gutter; the pointer changes to a vertical-resize
  cursor at a boundary. Drag the horizontal divider across the genomic canvas to allocate more
  height to either the annotation pane or the read pane. The annotation pane initially fits its
  measured content up to the configured limit; manually moving the divider establishes an explicit
  pane allocation. Annotation heights do not change with read density. Every track header has a
  visible gear button that opens its settings; right-clicking
  the header opens the same track menu. The read-pane eye hides or restores the Reads lane without
  collapsing the complete alignment track.
- The right edge contains two scrollbar segments on the same x-axis. The upper segment is a
  conventional annotation-stack scrollbar; the lower segment is the read-stack overview and
  density control. Each alignment read panel also has its own data/density scrollbar. In split
  workspaces each genomic panel owns a separate set of these controls. Scrolling over any pane or
  read panel changes only its owning camera, and all offsets are saved independently in projects.
- The lower read overview is an Ableton-style two-axis control: drag up/down to scroll and
  left/right to compact/expand read rows. Diagonal motion combines both actions,
  density changes retain the row beneath the exact grabbed point, and expansion creates a real
  density-scaled vertical camera even when the loaded rows are sparse. The thumb and coloured
  activity bands share that camera's coordinates: moving the thumb away from an occupied band
  moves those rows out of view instead of leaving an inert preview behind. Read rows can expand to
  roughly 64 pixels for close inspection. It uses the same captured-pointer behavior and visual
  language as the chromosome overview. Clicking the lower rail jumps to that read position.
- Arrow Left/Right pans by ten percent; `+` and `-` zoom around the centre.
- Home returns to the initial locus; Alt+Left/Right traverses locus history.
- `Ctrl/Cmd+L` focuses the locus field and Enter navigates.
- The **Panels** control activates, moves left/right, or closes a panel. Clicking or zooming inside
  a panel also activates it. Each panel owns its locus history, selection, read-stack scroll,
  retained results, and query generation; a result from one panel cannot replace another panel.
  Tracks keep one source and settings identity while projecting independently into every panel.
  Corresponding tracks below the primary alignment stay vertically aligned across columns; a panel
  with fewer retained read rows reserves blank space rather than pulling later tracks upward.
- Click a read to open its SAM fields and auxiliary tags in the docked **Properties** panel.
  The panel can be resized, collapsed to the right edge, reopened from **View**, and automatically
  reopens for a new selection. **Copy details**, **Copy CIGAR**,
  **Copy read sequence**, and **Copy tags** copy the selected evidence without reconstructing it
  from the display. Every auxiliary tag also has an individual copy action. Read sequence is copied
  in the reference-facing alignment order stored by SAM/BAM; flag `0x10` means `SEQ` has already
  been reverse-complemented and `QUAL` reversed. Read, consensus, event, and reference clipboard
  sequences are capped at one million bases; the hover
  text reports the copied and complete lengths when the cap applies. Very large tag values and the
  combined tag clipboard payload are also bounded and visibly report omitted content.
- Selecting a read with an `SA` tag, or an explicitly supplementary record, also opens a bounded
  supplementary-chain inspector. Cyan outlines identify every loaded segment with the same read
  name. Its **Read order** lane places each alignment on inferred original-read coordinates,
  including strand-aware soft/hard clipping; its **Alignment order** lane orders segments by contig
  and genomic position. Blue blocks are forward, purple blocks are reverse, and the active segment
  has an orange outline. Click either diagram or an exact-detail row, or use Previous/Next, to
  navigate. **Open active segment in new panel** retains the origin locus; **Open all segments in
  panels** opens as many linked loci as the bounded panel workspace permits and reports the cap.
  Malformed entries, invalid tag types, and the 64-segment cap are reported rather than hidden.
- Click an individual visible substitution, insertion, deletion, reference skip, soft/hard clip, or
  padding marker to add semantic event details to the selected read's inspector. A substitution
  reports its exact one-based genomic locus, aligned-read position, reference and observed bases,
  base quality, and whether it came from explicit CIGAR `X` or read/reference comparison. This
  per-base hit testing is available whenever coloured base marks are rendered, including the
  intermediate level where letters remain suppressed. If loaded read/reference bases contradict
  an explicit CIGAR `X` or `=` assertion, RGV renders a distinct warning marker instead of a
  nucleotide-coloured substitution; selecting it reports both the assertion and the observed
  comparison. Insertions and soft clips include read
  positions, a bounded sequence preview, mean event quality, and a copy-sequence action. Preview
  generation is capped at 120 bases; copying is capped at one million bases and the inspector
  reports when a separate full export is required. Reverse-strand event sequence and quality order
  match the displayed read.
- `N` and `P` select the next or previous visible read without using the pointer. Select a track in
  the label gutter and press Enter to open its settings.
- **Open inspectors on** chooses the default locked click behavior or semantic hover activation.
  Hover mode updates only when the read, CIGAR event, coverage bin, or modification bin changes, so
  ordinary motion within one object does not repeatedly rebuild its inspection state. The most
  recently activated inspector remains copyable, and the preference survives sessions.
- Read controls select strand/MAPQ/read-group/haplotype or arbitrary two-character SAM-tag colour
  and grouping. Sorting supports position, the displayed base at each panel's centre, strand,
  first-of-pair strand, name, MAPQ, read group, haplotype, and a custom tag. Pair-aware choices
  include template length, orientation, unexpected status, mate chromosome, and read order.
  Chimeric, supplementary, left-clip, and right-clip grouping and aligned-length/clip sorting are
  also available. Group and sort order can be reversed independently; **Re-pack** restores natural
  position packing without discarding the selected grouping. Colour, grouping, ordering, pair
  settings, and custom-tag state survive a JSON session round trip.
- Every alignment body ends in a bounded directional cap derived from that record's SAM reverse
  flag. This remains true for single-end, paired, long, spliced, secondary, and supplementary reads,
  whether pair view is enabled or disabled and through Bases, Reads, and Regional detail. If the
  biological terminal is outside the viewport, the clipped screen edge stays rectangular.
- Alignment track settings can select an exact read/template name from the bounded records already
  loaded at the active locus. The read inspector's **Move selected to top** action then promotes
  every loaded record with that name ahead of the configured within-group sort while retaining its
  stable record identity and cyan linked-read highlighting across panels. This override and the
  typed name are deliberately ephemeral: neither is written to sessions, settings, or diagnostics,
  and selecting or clearing another read removes the override.
- Alignment appearance controls select mismatch-only, all-base, or hidden-base display. At wider
  read detail, individual bases first appear as nucleotide-coloured marks; their letters are added
  only once each base has enough screen width to remain legible. Quality shading can be disabled
  without changing the underlying base qualities. “Show indels ≥” removes
  smaller insertion/deletion markers while “Label events ≥” independently suppresses text labels;
  reference skips remain visible because hiding them would misrepresent spliced alignment geometry.
  Regional views retain CIGAR insertions as fixed-pixel marks: short events use fine pale notches,
  while label-sized events use a prominent red `I` (10 bp or longer for new tracks by default).
  Compact rows retain the high-contrast red tick. Same-row events landing in one pixel are collapsed
  deterministically and the gutter reports shown, collapsed, and omitted counts.
- Reference, coverage, Sashimi, aggregate-modification, and read lanes have independent visibility
  controls. Click the eye beside a lane in the track gutter to hide or show it. A hidden lane
  collapses to a compact labelled row with a closed eye, so it can be restored in the same place;
  the alignment track's submenu under **Tracks** is a second recovery path. Hidden lanes cannot be
  picked. Coverage can autoscale to the current visible maximum or use a positive fixed maximum.
  Each bin is the exact maximum concurrent aligned-base depth at a position inside that bin, not
  the number of reads touching a wide bin. Linear and square-root height transforms are explicit;
  the gutter reports the raw maximum, transform, fixed scale, and any incomplete event-budget
  provenance. Expanded, Squished, and Collapsed buttons set useful read-row
  densities, while the right overview remains available for continuous adjustment. Lane visibility,
  fixed scale, and the exact continuous density survive session round trips.
- Modified-base controls use stable colours per `MM` modification code, a live legend with
  per-code visibility, a `ML` probability threshold for markers on individual reads, and optional
  single-colour or modified-versus-unmodified display. Unknown skipped-base mode is never treated
  as evidence of an unmodified canonical base.
- The alignment **Colour** control includes reference-aware bisulfite modes for CG, CHG, CHH, HCG,
  GCH, and WCG contexts. Forward/first-of-pair-forward templates have grey bodies and reverse
  templates have sage bodies. Red C/G marks mean protected or nonconverted cytosines; blue T/A
  marks mean converted cytosines. Marks remain visible as bounded lines at read detail and as bases
  at nucleotide detail. **Copy legend** records the exact convention, and the read inspector reports
  protected, converted, other-context, and examined-base counts plus a conversion fraction. A
  matching FASTA is required: without one, reads remain visible and the gutter and inspector report
  that conversion evidence is unavailable rather than guessing.
- The same **Colour** control provides six **PacBio SMRT** modes. Subread/CLR IPD and pulse width
  use the `ip` and `pw` integer arrays. CCS forward-strand IPD/pulse width and reverse-strand
  IPD/pulse width use `fi`/`fp` and `ri`/`rp`; for a reverse-aligned record RGV swaps the physical
  forward/reverse tag so the requested molecule strand remains consistent. Array positions are
  reversed with reverse-aligned BAM sequence before being mapped through the CIGAR. Values 0–100
  increase red opacity, 100–600 run from red to yellow, and 600–6000 run from yellow to cyan;
  larger values remain cyan. **Copy legend** records this convention. Reads stay grey when their
  required tag or sequence is absent, and the gutter counts unavailable records. Non-integer,
  negative, over-bound, or sequence-length-mismatched arrays are reported as malformed and are not
  partially coloured. The read inspector reports the physical tag, value count, minimum, maximum,
  and mean. Rendering is capped at 50,000 kinetic marks per layout and reports any omitted marks.
- **Ultima flow indels** is an explicit persisted alignment colour mode. RGV reads the signed
  `tp:B:c` direction array and base qualities for homopolymer insertions/deletions; a single-base
  non-homopolymer deletion uses the printable Phred+33 `t0` string. **Directional tp** chooses the
  quality at the matching flow direction (with the documented symmetric-quality adjustment and a
  maximum-read-quality fallback); **Minimum in homopolymer** instead uses the lowest base quality
  in that homopolymer. A read-group `mc` value sets the maximum reportable homopolymer length, and
  the track can explicitly override it. Q0–Q42 uses a blue-through-cyan/yellow-to-red jet scale.
  For insertions, the confidence line moves from the bottom at Q0 to the top at Q42; for deletions,
  it moves from left to right. Thus position still communicates quality when colour perception is
  limited. The ordinary purple insertion or dark deletion remains visible whenever `tp`, `t0`,
  sequence, quality, reference, or supported homopolymer context is unavailable. The gutter counts
  missing/malformed records, unavailable events, and any quality marks omitted at the separate
  50,000-mark cap. **Copy legend** records the active method and `mc` source. Selecting an indel
  adds its exact quality, signed direction, evidence source, homopolymer base, and unavailable
  reason to the read inspector; a matching FASTA is required for homopolymer deletions.
- **SBX evidence** is an explicit alignment colour mode; RGV does not guess that an arbitrary BAM
  came from an SBX instrument. With the default thresholds, an insertion is light grey when every
  inserted base has quality 10 or lower, dark grey when at least one inserted, adjacent, or
  containing-homopolymer base has quality 10 or lower, and purple otherwise. The two thresholds
  are visible and editable in the track settings. Contiguous read-end bases at or below the
  simplex-tail threshold (30 by default) can be highlighted in translucent purple, hidden from the
  CIGAR/read/base/modification rendering, or left unchanged. Reverse-aligned sequence and quality
  arrays are interpreted in reference-forward order. **Copy legend** records the active thresholds
  and tail behavior; the read inspector reports left/right tail lengths, retained bases, and all
  insertion-state counts. Reads without usable sequence or qualities remain visible in neutral
  grey, while their unavailable classification and any 50,000-mark tail-display truncation are
  disclosed in the track gutter.
- Filters accept a minimum MAPQ, secondary/supplementary switches, and semicolon-separated tag
  predicates such as `HP=1;RG=tumour;MM`. New tracks exclude secondary alignments and include
  supplementary alignments by default. The gutter, diagnostics, exports, and MCP snapshots report
  exclusions; saved projects retain their explicit choices.

Each alignment track has a **Read display** setting in its properties:

- **Downsampled** is the default for new tracks. At most 100 reads whose alignments start in each
  50-base window are given rows; both numbers are adjustable. The reads kept are those with the
  smallest stable hash of the read name, so the subset does not depend on load order, does not
  reshuffle while you pan, and mates share one priority. When any read is left out, the track
  label states how many are shown, for example `Downsampled: 4,210 of 156,152 reads shown`.
- **Complete** shows every eligible read. The configurable **Complete read limit** is 50,000 by
  default. If the eligible population in the exact viewport exceeds the effective limit, RGV
  presents no individual reads, reports the exact eligible count and limit, and asks you to zoom
  in or raise the setting. Raising the limit increases memory, layout time, and GPU work.

Coverage, alleles, junctions, pair statistics, and modification summaries are computed from every
eligible read in both modes; downsampling changes only which reads are drawn as rows. Navigation
to a mate or supplementary segment can therefore select a read that is not drawn. Projects saved
before this setting existed open in Complete mode so that they reproduce what was saved.

RGV currently computes coverage from the explicitly loaded alignment source. It does not discover
or substitute a precomputed coverage file by filename convention; support for such a source will
require an explicit track association so the provenance cannot be ambiguous.

### Read context menu: arranging, filtering, and highlighting

Secondary-click (right-click) inside an alignment read lane to open the read menu. It acts on the
alignment track under the pointer and, for the "clicked position" entries, on the reference
position under the pointer.

**Arrange reads → Sort by** offers Start position, Strand, First-of-pair strand, Mapping quality,
Read name, Read group, Haplotype, Aligned length, Left clip, Right clip, Template length, Pair
orientation, Mate contig, Pair status, Read order, and Base at clicked position.
**Arrange reads → Group by** offers None, Strand, Read group, Haplotype, Chimeric, Supplementary,
Left clip, Right clip, Pair orientation, Unexpected pair, Mate contig, Read order, First-of-pair
strand, Base at clicked position, and Insertion at clicked position. Sort and group order can
each be reversed, and the same submenu switches Stable/Compact stacking, **View as pairs**, and
**Reset arrangement / re-pack**. Sorting or grouping by an arbitrary two-character SAM tag is set
in the track settings rather than in this menu.

**Filter reads** applies quick predicates. Every active condition must match (logical AND); the
menu title shows the number of active filters and the submenu lists them, with a
"*matching* of *eligible*" count when it is known. The quick predicates are:

- Contains substitution, Contains insertion, Contains deletion, Contains any indel, Contains soft
  clipping, Contains splice;
- Duplicate, Secondary, Supplementary, MAPQ at least 30;
- Insertion at clicked position, Deletion at clicked position, Substitution at clicked position.

The substitution predicates compare read bases with the reference and are refused with
"Substitution predicates require a matching reference FASTA" when none is loaded.
**Clear all filters** removes these predicates and also resets the track's minimum MAPQ,
secondary/supplementary switches, and SAM-tag predicates.

**Highlight reads** offers the same ten non-positional predicates but marks matching reads
instead of hiding the others; **Clear highlights** removes them.

Filters and highlights are stored per alignment track in the project as `alignment_view_filter`
and `alignment_highlight_filter`. The stored model is richer than the menu: a predicate may be a
mapping-quality range, a SAM flag test (duplicate, secondary, supplementary, failed QC, paired,
proper pair, mate mapped, reverse strand), a template-length range, a mate contig, a SAM-tag
comparison (exists, equal, not equal, greater, greater-or-equal, less, less-or-equal), a CIGAR
event (insertion, deletion, indel, soft clip, hard clip, reference skip) with optional length
bounds, an aligned-length range, or a substitution with an optional observed base and count
bounds. CIGAR-event and substitution predicates can be scoped anywhere in the read, to one
position, or to an interval. Predicates beyond the quick entries above have no desktop editor;
they are reachable only through automation or a hand-edited project file. OR and NOT are not
supported.

### Multiple alignment tracks

Every alignment track has a stable session identity and an independent source, engine, cancellation
generation, filters, retained query, and row layout. Its annotation and read representations have
separate ordered presentation nodes, visibility, grouping, and preferred heights. The workspace
owns the annotation/read divider rather than whichever source happens to be first. The
**Tracks** menu lists every alignment source, including hidden tracks. **Settings…** opens the same
complete appearance, grouping, pairing, modification, junction, filtering, and Complete read-limit surface
for any selected alignment track; partially entered filter text and validation errors remain local
to that track. Dragging any alignment label reorders the complete alignment list and can promote a
track to first position without reopening it or changing its stable identity. **Remove track**
closes its engine; removing the first track promotes the next retained controller.

Shift-click or Ctrl/Cmd-click alignment labels to toggle a multi-selection. Opening settings for an
active member then offers **Show all**, **Hide all**, **Expand all**, **Collapse all**, and
**Apply compatible appearance to selected tracks**. The appearance action copies display,
grouping, pairing, junction, and modification presentation. It deliberately preserves each sample's
MAPQ/base-quality filters, SAM-tag predicates, secondary/supplementary inclusion, and record cap.

The decoded-alignment cache is divided when tracks are opened, and the workspace record ceiling is
allocated fairly among loaded, queryable tracks as a hard total. Consequently, adding samples does
not multiply the declared cache or limit. The Too-dense state reports the effective allocation
actually used by the query; it never trims an already presented batch into a sample. Read,
CIGAR-event, pileup, junction, modification, and
supplementary-chain inspection first resolves the owning track frame and then reads that track's
query and dictionary. Alignment sources with a different header order therefore remain safe, and a
chromosome missing from one source is reported only in that track.

### Splice junctions

Eligible CIGAR `N` operations are aggregated independently of individual-read admission into a bounded junction
summary. Enable **Junctions** in alignment settings, set **Junction depth ≥** for a display filter,
and set **Aligned flank ≥** to requery and count only reads with the declared number of aligned
`M`/`=`/`X` bases in both adjacent exon blocks. The inspector reports the observed left/right flank
ranges and exactly how many observations the flank filter removed.

The junction lane is a Sashimi view. **Collapsed** mode overlays arcs on one baseline; **Expanded**
mode assigns overlapping arcs to deterministic rows. Arc height follows the chosen **Per-track
auto**, **Fixed**, or **Shared visible tracks** scale. When several alignment labels are selected,
shared mode compares those selected samples; otherwise it compares all visible participating
alignment tracks. Counts can be raw reads or CPM. CPM uses the whole-file mapped-read total stored
in the BAM index. If that metadata is absent, RGV explicitly labels the raw fallback instead of
presenting a regional count as library-size normalization. Arc labels are optional and bounded.

Red arcs have positive-strand support, blue negative, grey unknown, and purple mixed. A right-facing
chevron marks pure positive-strand evidence and a left-facing chevron marks pure negative-strand
evidence; mixed evidence has both offset chevrons, while unknown evidence has neither. A small green
marker means the endpoints match adjacent exons from one transcript in a visible GTF/GFF track;
orange means novel relative to a complete loaded annotation interval; grey means annotation status
cannot safely be decided. Move the pointer onto the visible line of an arc to highlight that exact
positive, negative, or unknown component. The cursor changes to a pointing hand and the stroke
widens while retaining its strand colour. Acquisition follows the nearest drawn curve in screen
space, including where shallow arcs overlap a prominent junction; support depth is not used to
steal the pointer. Filtered, capped, hidden, clipped, and provisional navigation arcs are not
interactive because no corresponding line is committed to the canvas.

Hover is transient and does not replace a clicked selection. With hover-activated Properties it
temporarily previews the junction under the pointer, then restores the clicked junction on exit;
with click-activated Properties the visual hover cue still appears. Click a highlighted component
to retain the whole biological junction and its focused strand component, then inspect or copy its
skipped interval, intron length, raw/normalized count, flank range, per-junction strand-source
counts, annotation evidence, filter provenance, completeness, and aggregation caps. Dragging from
an arc continues to pan normally once it passes the click-motion threshold.

RGV uses `XS:+` or `XS:-` when present. If `XS` is absent, it explicitly falls back to the
alignment strand and reports how many observations used that fallback. An invalid `XS` value is
retained as unknown rather than silently reinterpreted. A query retains at most 100,000 distinct
junctions and at most 4,096 splice operations from one record. A lane draws at most 4,096 visible
junctions and 512 labels, choosing the deepest deterministically when capped. The selected junction
inspector exports a bounded, provenance-bearing BED for its alignment track and deterministic
multi-sample Sashimi PNG or SVG for the selected visible tracks. At most 16 tracks and 16 MiB of
text/vector output are retained; actual layout, image encoding, and file writes run outside the UI
thread.

### Stable read stacking

**Read stacking** defaults to **Stable across navigation** for every alignment track. A read that is
shared by two overlapping views keeps the same row through zoom, pan, filter, Reads/Bases detail
changes, Overview transitions, query-cache replacement, and window or pane resizing. Reads that are
temporarily absent retain lightweight row reservations, so returning to a broader view restores the
previous architecture. The first layout is built deterministically over a fixed, tiled neighbourhood:
reads are ordered canonically and placed in the highest compatible row. That gives zoom levels the
same organised architecture instead of freezing whichever viewport happened to load first. Later
horizon extensions can leave gaps or make the read stack taller because existing reads do not move;
the right-side navigator keeps the same stable row and within-row offset at the top of the pane.

Foreground admission covers the exact requested viewport. Off-screen prefetch is a separate,
best-effort cache request and cannot consume the foreground limit or change its result. Stable row
architecture prefers a complete deterministic layout horizon. If that wider horizon is Too dense,
RGV tries the smaller fixed horizon and then falls back to the Complete exact foreground. The view
remains Complete and the diagnostics mark the best-known architecture as horizon-truncated; no
sampled horizon identities are used. Compact mode likewise needs only the complete foreground
population.

Choose **Compact each view** when minimum height is more important than visual continuity. It packs
each settled query independently and does not delete the track's retained stable history, so
switching back restores the matching stable context. In Stable mode, **Rebuild stable architecture** makes
the current compact arrangement a new stable baseline once without enabling persistent compact
mode. Grouping, sorting, pair view, and a newly chosen base-sort anchor are explicit layout-policy
changes; colour, labels, annotation height, density, and observed query statistics are not.

Disjoint loci and different row policies use separate bounded layout islands. Diagnostics and the
automation workspace snapshot report the active mode, architecture version, policy and projection
fingerprints, layout revision, reconciliation time, retained/restored/added/moved counts,
reservations, canonical and stable row counts, row inflation, displacement, top-first violations,
occupied and empty-row measures, horizon interval/generation/extensions/records/bytes/truncation,
islands, history bytes, evictions, and the typed transition reason. Navigation-only transitions
should always report `moved = 0`.

### Paired reads

Enable **View as pairs** in alignment track settings to pack primary records with the same SAM read
name on one stable row. Both loaded ends and their single connector share one pick target, so a
click opens one template-level inspection instead of selecting disconnected primitives. Secondary,
supplementary, unpaired, and unnamed records remain independent.

RGV classifies each selected record as proper, concordant, discordant by template length or
orientation, inter-chromosomal, mate-unmapped, mate-missing, read-unmapped, or unpaired. The
inspector also reports orientation, read order, whether the mate is visible, outside the viewport,
on another chromosome, unmapped, or unavailable, and a stable non-identifying template hash.

Expected template-length bounds are inferred from eligible, proper, primary first-of-template
records independently of individual-read admission. Inference uses a deterministic aggregate-model sample capped at 4,096
templates, requires at least 100 templates, reports its sample size and median, and uses a robust
three-IQR envelope. Track settings can override the inferred minimum and maximum; those explicit
bounds are validated, session-persisted, and used by pair classification, colour, grouping, and
sorting. Missing evidence remains labelled unavailable.

Pileup counts overlapping primary mates once by default: the leftmost genomic end supplies the
overlap and the right end is masked before depth, allele, indel, and quality aggregation. The
inspector reports the active policy, number of removed overlapping bases, and whether the bounded
100,000-pair tracker completed. **Count both ends** remains available and is session-persisted for
workflows that treat reads rather than templates as independent evidence. **Open mate in new
panel** centres a new panel on a mapped mate, retains the origin panel, and reselects the same read
identity when the destination query completes.

### Allele-aware coverage

At exact base resolution, coverage bars retain their grey depth silhouette and stack only
reference-discordant A, C, G, T, or N support above the configured minimum allele fraction. RGV
uses the conventional stable nucleotide palette (A green, C blue, G amber, T red). The alignment
track settings control minimum base quality, minimum allele fraction, quality-weighted colouring,
and the optional coverage centre line. Minimum base quality changes allele evidence but does not
silently change the unfiltered read depth.

Click a coverage bar to open the pileup inspector. It reports the one-based locus, reference base,
total and strand-specific depth, forward/reverse nucleotide counts, insertion and deletion support, unweighted
alternate fraction, quality-weighted support, mean base and mapping qualities, active threshold,
and whether evidence came from every eligible read. Details can be copied directly. When the
loaded pileup has one bin per genomic base, the inspector can also copy a visible base-only
consensus: the highest quality-weighted nucleotide is emitted when it has more than 50% support,
otherwise `N` is emitted; positions without observations fall back to the reference when present.
At the same resolution, **Copy visible reference** copies the loaded reference slice independently
of the consensus. The reference action is also available from the read inspector.

### Modified-base frequency

When enabled, the pinned **Modifications** lane appears beneath coverage at regional, read, and base
detail. Its bars show the dominant visible modification type at each exact site or bounded genomic
bin. Bar height is the raw modified/canonical fraction when the `MM` skipped-base mode establishes a
canonical denominator; otherwise it uses mean call probability only as a display score and the
fraction remains explicitly unavailable. In modified-versus-unmodified mode, blue is canonical
evidence and magenta is modified evidence.

Click the lane to inspect every modification series in that bin, including canonical base,
modification code and strand, optional `HP`, raw and probability-weighted fractions, alignment
strand counts, missing `ML` calls, and evidence provenance. The inspector reports malformed tags,
unavailable sequences, incomplete queries, and the bounded 32-series cap. Its details are copyable.
The aggregate is calculated from every eligible record independently of individual-read admission,
so lowering the Complete read limit does not change its counts. The per-read probability threshold does
not rewrite the aggregate; the inspector exposes both raw and probability-weighted evidence.
Insertions and deletions are reported separately and are not incorporated into this consensus.

## Tracks, sessions, and export

### Variant and genotype review

Indexed VCF and BCF tracks retain typed REF/ALT, ID, QUAL, FILTER, header-described INFO and FORMAT
fields, GT allele indices and phasing, sample names, symbolic alleles, `END`, and breakend partners.
SNVs, MNVs, insertions, deletions, inversions, duplications, copy-number records, symbolic SVs,
spanning deletions, and BNDs use stable type-specific colours rather than being inferred only from
the first allele's string length. BND markers use a bracket with its vertical bar on the declared
join-before or join-after side. A visible same-contig partner is connected and gets its own bracket;
offscreen and cross-contig partners get bounded edge brackets and retain both join sides in the
label and inspector. Symbolic SVs use parsed `STRANDS` orientation when supplied and remain
geometrically neutral when it is absent.

Click a variant marker or span to open the **Variant inspector**. It shows exact one-based locus,
alleles, all ALT-specific types, quality and filters; INFO keys expose their VCF-header descriptions.
Genotype rows can be filtered by sample name and are virtualized, so only the visible rows create UI
widgets. Phased and unphased separators are preserved. A BND's **Open partner** action retains the
origin panel and opens the partner coordinate in another panel when that contig belongs to the
active genome. **Copy details** emits a bounded textual record, INFO summary, breakend join sides,
and symbolic breakpoint orientations. The variant-and-genotype TSV exports those join sides and
their optional `STRANDS` support explicitly.

Open a variant track's **Track settings** to configure its persisted cohort view. RGV can restrict
decoding to a searched sample subset, group inspector rows by genotype, and colour markers by the
selected cohort's genotype rather than by variant type. Record filters cover variant type, QUAL,
PASS or named FILTER values, AF ranges, genotype class, and typed INFO or FORMAT conditions,
including GT. Clauses combine with AND; alternate alleles, values, and selected samples use ANY
inside a clause. Missing QUAL, AF, INFO, and FORMAT values always use the visible include/exclude
rule rather than an implicit comparison convention.

The inspector's **Alignment evidence** section connects the selected variant to each loaded
alignment track's current pileup bin, strand depth, allele or indel observations, co-located modification
calls, retained-read context, and structural endpoint panels. These are deliberately labelled as
observations, not as an RGV variant call: overlap, pair context, or modification co-location is not
presented as allele support without allele-specific evidence. Selected variant/genotype and
alignment-evidence TSV exports are capped at 16 MiB, disclose query and field limits, sanitize TSV
cells, and write on a background worker; shutdown waits for an active export to finish.

Variant queries currently cap records at 100,000, header sample names at 100,000, retain at most
256 INFO and 32 FORMAT schemas, 256 values per field, 4,096 bytes per string value, 10,000 genotype
samples per record, and 250,000 genotype cells per panel query. Core `END`, SV, allele-count, GT,
GQ, DP, AD, PS, and PL schemas are prioritized when a malformed or unusually broad header exceeds
those limits. The inspector reports record, sample, schema, and genotype omissions explicitly
rather than presenting partial evidence as complete.

These genotype limits bound retained display data, not biological eligibility. Genotype and
FORMAT filters examine the selected cohort independently of the display budget. A required
schema, sample-name set, or field value that exceeds exact-evaluation limits produces an explicit
query error, not a silently filtered record. An omitted display cell is not a missing source value.

### Quantitative and interaction tracks

BigWig, TDF, bedGraph, and fixedStep/variableStep WIG use one quantitative display model. Track settings
provide automatic, fixed, or shared compatible ranges; a configurable baseline; linear or signed
log scaling; bar, line, point, or heatmap rendering; minimum, base-weighted mean, maximum, or raw
windowing; positive and negative colours; opacity; and explicit gap, baseline, or marker handling
for missing windows. Missing values are never silently treated as zero. Add, subtract, multiply,
and divide transformations combine two loaded quantitative sources only where both have finite
overlapping values; division by zero remains missing. Clicking a source interval opens exact value,
coordinate, aggregation, scale, arithmetic, and query-cap provenance.

Track eyes are presentation controls: hiding a loaded auxiliary or alignment node does not unload
its source or erase its retained query. This keeps restoration instant and prevents an annotation
eye from accidentally starving the corresponding read channel (or vice versa). Source removal is
the explicit lifecycle operation that cancels future queries and deletes source-owned presentation
and camera state. A quantitative arithmetic operand remains queryable while its source is loaded,
regardless of whether its own row is currently painted.

TDF versions 1 through 4 are read directly with `--tdf`, the file dialog, drag-and-drop, or project
restore. Local files and HTTP/S URLs use indexed range reads: RGV reads the header and master index,
then only the selected dataset and tiles overlapping the current locus. Both compressed and
uncompressed fixedStep, variableStep, BED, and named-BED tiles are supported. RGV chooses a bounded
raw or precomputed zoom dataset for the locus and records the exact dataset, window function,
format version, genome ID, compression state, and embedded channel in the quantitative inspector.
Non-finite values are counted as missing and a query cannot retain more than 250,000 source
intervals.

A multi-channel TDF expands into one independently configurable quantitative track per embedded
channel, in source order. The channel tracks share one indexed source and bounded decompressed-tile
cache, so the file is not reopened or downloaded once per signal. Exact channel indices are saved
in the project, and the inspector and structured export state both the selected channel and total
channel count. Distinct signals are never averaged or merged.

Quantitative tracks can be assigned a non-zero overlay group in track settings. Visible tracks in
the same group occupy one frame, preserve their individual colours and opacity, and use one shared
range. Changing linear versus signed-log scaling on one member updates the complete group so a
saved session cannot restore incompatible axes. Clear **Overlay with group** to separate a track;
an optional scale group shares an axis without overlaying geometry. Overlay member labels are
bounded to the available gutter space, with an explicit omitted-member count for large groups.
The overlay header eye is a parent visibility gate and never rewrites member eyes; showing it again
restores the previous mixture of member states. Each member row has its own eye for independently
showing or hiding that layer within the composite.

BEDPE and UCSC 18-column interact files render intra-chromosomal relationships as score-scaled arcs
or endpoint blocks. Inter-chromosomal records use labelled partner markers. The inspector preserves
endpoint names, strands, score, value, experiment, colour, and direction where the source provides
them. Directed interactions point at the semantic target even when it lies left of the source;
undirected records have no source-to-target arrow, though declared endpoint strands remain visible.
Cross-contig labels use `->` for a directed relationship and `<->` for a neutral join. Either endpoint
can be opened through the common linked-panel navigation. A minimum score
filter runs before the 100,000-record query cap; drawing and labels have smaller deterministic caps.

### Segmented copy number

SEG and CBS files load as a multi-sample heatmap rather than collapsing every sample into one
signal. RGV reads the first four columns as sample, chromosome, start, and end, preserves an
optional marker count, and reads the last column as the segment value. Standard SEG coordinates
are converted from one-based inclusive input to RGV's internal zero-based half-open intervals; a
`#track ... coords=0` directive explicitly selects zero-based input. Non-numeric or non-finite
values are ignored and counted in the inspector rather than turned into zero.

Value interpretation can be inferred—any negative source value selects log-ratio semantics—or set
explicitly to log ratio with neutral value 0 or absolute copy number with neutral value 2. Copy-number
settings provide automatic, fixed, and shared compatible heatmap ranges, amplification/deletion
colours, opacity, row height, sample names, and optional in-segment values. Samples remain in file
order or can be sorted by amplification or deletion using the exact base-weighted mean over a
persisted locus; **Use current view as sort locus** updates that region.

Large cohorts use one tall scrollable track but only rows intersecting the visible canvas produce
geometry and labels. Source loading is capped at 100,000 samples and 2,000,000 segments; a panel
query retains at most 250,000 segments and locus sorting examines at most 500,000. Selection opens
the exact sample, one-based segment, value, marker count, interpretation, row rank, sort locus, and
all truncation or ignored-row provenance. Copy-number presentation and sorting survive JSON session
round trips.

### Mutation Annotation Format

GDC/TCGA-style Mutation Annotation Format files load as one vertically scrollable row per tumour
sample. RGV locates fields by their header names rather than assuming one particular GDC revision.
It requires genomic coordinates, gene, classification, variant type, reference/tumour alleles, and
either `Tumor_Sample_Barcode` or `Tumor_Sample_UUID`. MAF coordinates are converted from one-based
inclusive input to zero-based half-open internal intervals.
Pure insertions that use the established MAF convention `Start_Position = End_Position + 1` are
retained as an explicit boundary marker; the inspector reports both original source coordinates.

Mutations are open markers coloured by variant classification, basic type (`SNP`, `INS`, `DEL`, and
related types), or a uniform colour. Repeated calls for the same sample and exact interval receive a
horizontal centre line. Gene labels appear only at a readable zoom and have a hard label cap. Track
settings persist colour mode, row height, sample names, and label visibility. Only visible sample
rows produce geometry or gutter labels.

Click a marker to inspect its exact sample, locus, classification, type, alleles, and every retained
non-empty source column in original header order. The reader retains at most 100,000 samples, one
million source mutations, 100,000 mutations per panel query, 256 columns, 4,096 bytes per value,
64 KiB of annotation values per mutation, and 512 MiB of annotation values per source. Ignored rows,
omitted columns, truncated values, query caps, and retained annotation bytes are reported explicitly.

### genePred, refFlat, narrowPeak, bigGenePred, and bigNarrowPeak

UCSC genePred and refFlat files load as transcript models. RGV accepts the standard ten-column
genePred form, the fifteen-column extended form, refFlat's additional gene-name column, and the
optional leading database `bin` column found in UCSC table exports. Coordinates remain zero-based
and half-open. Exon counts and arrays must agree; exons must be sorted, non-overlapping, non-empty,
and contained by the transcript. Coding sequence is drawn as thick exon segments while UTRs and
non-coding transcript exons remain thin. refFlat and extended genePred labels use the gene symbol
when one is present, while the inspector separately retains gene and transcript accessions. Extended
genePred also retains CDS completion status, exon frames, and score. At readable zoom levels, gene
symbols appear automatically without selection. Labels use measured screen-space width, avoid one
another, and are suppressed when the view would be crowded; a selected gene keeps its label.
Stranded transcript and blocked-feature connectors carry repeated
screen-spaced chevrons, and their true terminal block has a compact directional cap. Unstranded
records remain rectangular. The same geometry is used for GFF3, GTF, BED12, bigGenePred, blocked
BigBed, and PSL target orientation; simple stranded intervals and peaks use one compact cue.

ENCODE narrowPeak files are parsed as the specified BED6+4 format. The body uses the 0–1000 score
for visual strength, and a dark vertical mark identifies the point-source summit when supplied.
Clicking a transcript or peak selects its exact packed row and opens a copyable feature inspector.
The inspector reports coordinates, strand, score, exon and coding structure, source attributes,
signal value, p-value, q-value, and the absolute and interval-relative summit position as
applicable. A source summit of `-1`, or p/q value of `-1`, remains explicitly missing rather than
being interpreted as a genomic position or statistical value.

The binary bigGenePred and bigNarrowPeak equivalents stay indexed and are queried on demand rather
than being loaded into memory. bigGenePred requires the canonical 20-field BED12+8 schema and
retains validated relative exon blocks, coding interval, strand, score, transcript and gene names,
CDS completion states, exon frames, transcript type, and gene type. bigNarrowPeak requires the
canonical ten-field BED6+4 schema and feeds the same score, signal, p/q, summit, selection, layout,
and inspector model as text narrowPeak. A mismatched declared field count or malformed indexed
record fails the affected track with its locus instead of being reinterpreted as generic BED.

Use `--big-gene-pred` or `--big-narrow-peak` when the file has an ambiguous `.bb` or `.bigBed`
suffix. That exact schema is retained in saved projects. Files named `.bigGenePred` and
`.bigNarrowPeak` are recognized by the file picker and drag-and-drop. Feature-name and transcript
search runs in the background and examines at most 100,000 indexed records when the BigBed library
cannot use an embedded extra name index; previous/next feature navigation is bounded in the same
way. Panel queries fail explicitly above 250,000 records, and individual binary records remain
limited to 1 MiB and 100,000 blocks.

Use `--gene-pred`, `--ref-flat`, or `--narrow-peak` to identify a text source explicitly. Drag-and-drop,
the track file picker, and saved sessions recognize `.genePred`, `.refFlat`, `.narrowPeak`, and
their gzip-compressed forms (case-insensitively). Text feature lines are limited to 1 MiB, each
record to 100,000 exon blocks, each source to two million records, and each panel query to 250,000
records. A source or locus exceeding a limit fails that track explicitly instead of silently
displaying incomplete annotation.

### broadPeak and PSL

ENCODE broadPeak is read as the official BED6+3 format. It uses the same score-sensitive peak body
and exact signal/p/q inspector as narrowPeak, but it has no point summit and RGV does not fabricate
one. Score must be an integer from 0 to 1000, strand must be `+`, `-`, or `.`, signal must be finite,
and p/q values accept only a non-negative value or the specified missing value `-1`.

PSL files are target alignments rather than read alignments. RGV accepts the standard 21 columns,
an optional leading UCSC table `bin`, ordinary psLayout headers, and plain or gzip-compressed input.
The target name and zero-based half-open target interval determine the genomic locus. Gap-free
target blocks are connected across the alignment; the displayed strand combines query and target
orientation, including the special reverse-coordinate rules for `qStarts` and two-character PSL
strands. The inspector retains match, mismatch, repeat, N, query/target insertion, query interval,
raw strand, and block-array provenance. Query names participate in feature search and previous/next
navigation. Translated protein PSL is rejected explicitly because its amino-acid block sizes cannot
be rendered correctly by RGV's nucleotide annotation model.

Use `--broad-peak` and `--psl` to identify these sources explicitly. `.broadPeak`, `.psl`, and their
`.gz` forms are recognized by drag-and-drop, the track picker, and saved projects. Their lines,
source records, block counts, search indexes, and panel results use the same hard limits as other
text annotations.

All feature and gene-model track settings offer **Collapsed**, **Squished**, and **Expanded**
display. Collapsed overlays every visible item in one row, squished doubles the packed row capacity,
and expanded uses normal-height packed rows. The mode is persisted, and mouse picking uses the same
row assignment as rendering.

### GWAS Manhattan tracks

RGV accepts the same flexible association-summary headers as IGV. A header is required, columns
may appear in any order, and names are matched case-insensitively: chromosome uses `CHR`, `CHROM`,
or `chromosome`; one-based position uses `BP`, `POS`, or `position`; identifier uses `SNP`, `RS`,
`RSID`, `ID`, `marker`, or `markername`; and probability uses `P`, `PVAL`, `P-VALUE`, `PVALUE`, or
`P.VALUE`. Extra columns are retained in source order for exact inspection. `NA`, `NaN`, and `.`
p-values are counted and ignored; other malformed coordinates or probabilities fail the track with
their line number. A source p-value of zero remains exact and is shown at the configured display
ceiling rather than producing invalid geometry.

Points are plotted at `-log10(p)` in a Manhattan track. Track settings provide a stable
chromosome palette, alternating chromosome colours, or one uniform colour, plus point diameter
and vertical maximum controls. Clicking chooses the closest point in both genomic and vertical
space, including when several associations share a coordinate. The inspector reports the exact
source p-value, transformed value, every retained column, and whether the current result was
bounded. Identifier search and previous/next evidence navigation use the same typed records.

Use `--gwas` explicitly, or open/drop `.linear`, `.logistic`, `.assoc`, `.qassoc`, or `.gwas` and
their gzip-compressed forms. Text lines are limited to 1 MiB, fields to 4 KiB, headers to 256
columns, a source to five million associations and 512 MiB of retained annotations, and a panel
query to 250,000 points. A query above that limit keeps the strongest association from each stable
positional chunk, shows the retained/examined count in the gutter, and preserves exact retained
records for selection. Display settings and source identity round-trip in RGV projects.

The fixed left gutter identifies every track while genomic content pans and zooms independently.
Alignment tracks identify reference, coverage, and reads separately, including maximum visible
coverage and Complete/Checking/Too-dense read-presentation status. Hover a track label for its full name, type, and
redacted source. Right-click a label to open settings, collapse or expand it, hide it, export the
current view, or remove a track. Drag an alignment or auxiliary label to reorder it and drag its
bottom separator to resize it. Hidden tracks remain discoverable in the **Tracks** menu.

Version-4 JSON projects preserve ordered panel IDs, loci, annotation/read-stack offsets, every
per-read-panel camera and density, the active panel, sources, genome/reference state, the normalized
annotation and read presentation trees, first-class groups, independent eyes/chevrons/preferred
heights, the workspace divider, alignment and reference custom remote-index URLs, pileup filters,
scales, sample metadata, ROIs, review queues, and the workspace
alignment cap. Saving is atomic and runs outside the UI thread. A successful save replaces the
previous project only after a complete validated JSON file has been flushed; a failed or
interrupted save leaves the last good project intact.

The directory containing the project JSON is its explicit project root. Local sources inside that
directory are written as relative paths and move with the directory; sources outside it remain
absolute. Opening resolves relative sources against the new project location. **File > Unresolved
project sources** lists every missing reference, cytoband, alignment, or auxiliary source
individually without hiding healthy tracks. Version-1 through version-3 fixtures migrate
deterministically into the normalized version-4 presentation without changing source order, and
future schema versions are rejected rather than guessed.

IGV XML import resolves relative resources from the XML file's directory and imports the locus,
resource names/paths, supported track names, visibility and heights, and alignment coverage or
junction visibility. It opens an **IGV session import report** that separately lists every imported,
approximated, and ignored XML semantic category; the report never silently claims that an IGV-only
renderer or scale was reproduced. An IGV `locus="All"` (whole-genome view), a missing locus, or
an unparseable one opens the default locus; a bare contig opens that contig; a gene name opens the
default locus and is then searched for in the open annotations (including the built-in Genes
track), navigating when there is a unique match. PNG export is deterministic and independent of
the platform text renderer. **File → Export PNG…** and the Sashimi PNG export write at the
display's pixel density (2× on a Retina display), so the file is as sharp as the window;
automation snapshots stay at one pixel per point.

A project written by a newer RGV opens with a warning that settings this version does not
understand were dropped. The warning is kept in the notification log, and the first **Save** back
over that file is held and shows it again; save a second time to overwrite, or use
**Save project as…** to keep the original.

If the settings file cannot be read at startup, RGV starts with default preferences, keeps the
unreadable file beside it as `<name>.corrupt`, and says so in the notification log. Settings are
saved in the background; rapid changes are written once.

The notification log also reports, once per occurrence: a view too dense to draw completely
(marks beyond the per-frame renderer budget are left out until you zoom in or collapse tracks),
and text tracks whose malformed lines were skipped ("N malformed lines skipped, first at line
L"). If the GPU device is lost, RGV retries recreating it every second without needing input.

`Ctrl/Cmd+K` opens searchable commands, `Ctrl/Cmd+O` opens alignments, `Ctrl/Cmd+S` saves a session,
`Ctrl/Cmd+E` exports PNG, `Ctrl/Cmd+D` toggles the diagnostics window, and `Ctrl/Cmd+,` opens
settings. Performance counters stay out of the primary interface unless diagnostics are opened.
Diagnostics report workspace-wide alignment/cache/layout totals followed by the lifecycle,
retained-record count, and decoded-cache residency of every alignment track, so a failed or
unavailable sample remains distinguishable from healthy samples. They list every visible panel's
stable ID, locus, and query generation and identify the active panel.
### Accessibility and keyboard operation

RGV enables the native AccessKit integration supplied by its window stack. Standard controls,
dialogs, menus, inspectors, metadata tables, regions, and review lists participate in the platform
accessibility tree and normal Tab/Shift+Tab focus order. The chromosome ideogram exposes its
chromosome and centromere summary. The custom chromosome overview, right-side
track navigator, track labels, resize handles, genomic canvas summary, and supplementary-chain
segments provide explicit roles, names, values, selection state, and visible focus outlines. Status
changes emit accessibility value-change events so loading and asynchronous failures do not rely on
visual observation alone.

The chromosome overview accepts Left/Right to pan, Down/Up to zoom in/out, and Home to show the
whole contig when focused. The right-side overview accepts Up/Down and Page Up/Page Down to scroll,
Home/End to reach the stack boundaries, and Left/Right to change read-row density. A focused track
label accepts Enter for settings, V for visibility, C for collapse, Alt+Up/Down for ordering, and
Shift+Delete to remove the track. `]` and `[` move through loaded evidence in the selected track and
open the applicable inspector; N and P retain their visible-read behavior. The keyboard-focused
command search (`Ctrl/Cmd+K`) also provides selected-track settings/visibility/collapse,
previous/next evidence and panels, metadata, ROI, and review-queue access.

Settings provides persistent 75–200% interface scaling, increased interface contrast, and reduced
motion. Reduced motion disables egui transitions and replaces the animated loading spinner with the
ordinary status text. The genomic evidence palette continues to combine colour with shapes, labels,
strand symbols, outlines, or inspector text; colour is not the sole source of analytical meaning.

### Batch automation

Launch RGV with `--batch path/to/review.json` to run a versioned JSON command list in the native
desktop application. Use `--automation-report path/to/report.json` to select the result file; when
omitted, RGV writes `<batch-name>.report.json` beside the batch. Local input and output paths inside
the batch are resolved against the batch file's directory, not the process's working directory.

Schema version 1 defines commands for a new workspace, genome and track loading, one or several
loci, panel creation/removal, workspace/track/panel options, alignment grouping/sorting/filtering,
semantic panel/track/position selection, deterministic settling, snapshots, structured export,
project save, and exit. Source-open commands do not report success merely because work was queued:
they wait for the background reader to open or return a stable error/timeout. `wait_until_settled`
checks the completed query generation, auxiliary and reference generations, background file work,
layout, and the rendered display state; it does not sleep for an assumed loading time.

```json
{
  "schema_version": 1,
  "commands": [
    {
      "command": "load_track",
      "source": { "Local": { "path": "data/reads.bam" } },
      "kind": "Alignment",
      "name": "Tumour"
    },
    { "command": "goto", "loci": ["chr1:900001-920000"] },
    { "command": "wait_until_settled", "timeout_millis": 30000 },
    {
      "command": "snapshot",
      "path": "results/review.svg",
      "scope": "visible_workspace",
      "format": "svg"
    },
    { "command": "save", "path": "results/review-project.json" },
    { "command": "exit" }
  ]
}
```

Visible-workspace, visible active-panel, and resident full-active-panel snapshots support
deterministic PNG and semantic SVG from the committed alignment scene, including read bodies and
derived summaries. Exports retain their captured camera/evidence while navigation continues;
projection and encoding run in a budgeted worker. Full-panel export succeeds only when its
complete, unscrolled content is already resident and fits the captured canvas (at most 16,384
logical pixels high). Requests requiring off-screen pages are explicitly refused, without changing
the live layout or silently exporting partial evidence. Manual PNG uses the same visible-workspace
composition.

**Where SVG export is available.** The desktop menu offers PNG only (**File → Export PNG…**,
Ctrl/Cmd+E); there is no menu item for an SVG of the workspace. Workspace and panel SVG is
produced only by the automation `snapshot` command with `"format": "svg"`, as in the example
above — through `--batch`, the opt-in local control service, or an MCP client that sends the same
command. The one interactive SVG export is **Export Sashimi SVG…** in the selected-junction
inspector, which writes the Sashimi plot only (see [Splice junctions](#splice-junctions)).

SVG
metadata records the RGV version, genome, loci, visible stable track IDs and names without
embedding source URLs, credentials, read names, or sequence. Structured commands export visible
active-panel alignments, pileup bins, modified-base series, junctions, variants/genotypes,
quantitative windows, selected evidence, region BED, and workspace track-name TSV. Each format has
an explicit 64 MiB limit and records Complete/Too-dense read state, semantic filtering, bounded
aggregate presentation, or malformed-evidence provenance where the source model provides it. Every
file operation runs on a worker, reports its
resolved output path, and is joined on shutdown.

Commands run in order. The first failure stops mutation, marks the remaining commands as skipped,
and writes a bounded structured report with stable status and error-code values. An `exit` command
closes the application only after the report has been atomically replaced and flushed. Closing the
window early also attempts to preserve a report that identifies the interrupted command.

### Opt-in local control

RGV has no command listener by default. To control an already-running desktop window, explicitly
enable the version-1 service with `--control-port` and provide a secret file. The service binds only
to IPv4 loopback (`127.0.0.1`); it cannot be configured to listen on a LAN or public interface.
Port `0` asks the operating system for a free port, which RGV reports in its startup log without
printing the secret.

Create a random secret of at least 32 bytes in a file accessible only to your account. On Unix-like
systems RGV rejects a secret file with any group or world permission bits, so a typical setup is:

```sh
umask 077
openssl rand -hex 32 > rgv-control.secret
rgv-desktop --control-port 60151 \
  --control-secret-file rgv-control.secret \
  --control-root /path/to/review-workspace
```

**Port 60151 is also IGV Desktop's default batch-command port.** RGV has no default listening
port — you always choose one with `--control-port` — but the examples in this guide use 60151 and
the `rgv-mcp` adapter's default control URL is `http://127.0.0.1:60151`. If IGV is running with
its port enabled on the same machine, one of the two will fail to bind, or a client meant for one
will reach the other. The protocols are not compatible: RGV speaks authenticated HTTP/JSON, not
IGV's plain-text batch commands. Choose a different port (or `0` for a free one) when both
applications are in use, and configure the MCP adapter's control URL to match.

An authenticated client sends the same JSON object accepted by `--batch` to
`POST http://127.0.0.1:60151/v1/batches` with `Content-Type: application/json` and
`Authorization: Bearer <secret>`. RGV returns an `AutomationRunReport` after the live window has
actually completed or failed the commands. `GET /v1/status` reports only the protocol version and
authentication scheme; it exposes no workspace state. Requests are serialized, limited to 8 MiB,
and have a ten-minute transport deadline. Local source and output paths are resolved against
`--control-root` (or the launch working directory when omitted); paths whose real location (after
resolving symbolic links) is outside that root are rejected. The same rule applies to every source
named inside a project opened with `open_workspace`: a project inside the root cannot point RGV at
files outside it. Remote sources in control batches and in such projects must be `http` or `https`
URLs and may not name `localhost` or a loopback, link-local or private-network address literal
unless RGV was started with `--control-allow-private-urls`. Projects and tracks opened from the
GUI are not restricted. Use `--batch` for longer unattended runs.

The secret is read into memory at startup. It is not accepted as a command-line value and is never
written to logs, settings, sessions, diagnostics, reports, or responses. Batch mode and local-control
mode are mutually exclusive. Closing RGV returns a structured unavailable response to an active
client where possible and stops the listener.

## Privacy, cache, and recovery

Diagnostics are off by default and this build transmits none. Runtime authentication values have
redacted debug output. URL user-info, signed query parameters, and fragments are stripped from
labels, recent history, and saved sessions, so credentials must be supplied again through the
runtime authentication mechanism. Set `RGV_REMOTE_BEARER_TOKEN` in the launch environment to
authenticate remote BAM and index requests; the value is never written to the session or settings.

The default cache hierarchy is 512 MiB and can be changed in Settings; a new budget takes effect on
restart. **Clear cache** removes decoded alignment and layout entries. The HTTP range cache is a
bounded temporary disk cache removed with its reader.

Periodic recovery autosave is disabled until enabled in **Settings > Recovery autosave**. The
interval is bounded to 30–3,600 seconds and retention to 1–20 snapshots per project. Snapshots are
written atomically in the local `autosaves` directory beside the settings file, and old snapshots
are removed only within the same project group. RGV's PID-only run marker stores no locus, source
path, sequence, or tag. After an unclean exit, RGV offers the retained snapshots explicitly; a
chosen recovery opens as an unsaved project and therefore cannot overwrite the original without a
subsequent **Save project as…** action. Recovery snapshots contain the same intentional project
state—including source paths, sample attributes, ROI/review labels and notes—as a manual project
save, but remote credentials, read names, read sequences, and loaded tags are not persisted.

## Command-line examples

```sh
rgv-desktop --reference reference.fa --genome-name hg38 --locus chr6:1000001-1010000
rgv-desktop --genome-config genomes/hg38/genome.rgvgenome --locus 6:1000001-1010000
rgv-desktop --bam reads.bam --reference reference.fa --locus chr1:1000001-1100000
rgv-desktop --bam reads.bam --genome-name hg38 --cytobands hg38.cytoband.tsv
rgv-desktop --session analysis.rgv.json
rgv-desktop --import-igv analysis.xml
rgv-desktop --bam https://example.org/reads.bam --bigwig https://example.org/signal.bw
rgv-desktop --seg cohort.seg --locus chr8:127000001-130000000
rgv-desktop --maf somatic.maf --locus chr17:7565098-7590856
rgv-desktop --bedgraph signal.bedgraph --bedpe loops.bedpe --locus chr1:1000001-1100000
rgv-desktop --bam https://example.org/reads.bam \
  --remote-index-url https://indexes.example.org/reads.custom.bai
rgv-desktop --batch validation/automation-smoke.json \
  --automation-report benchmark-results/automation-smoke-report.json
rgv-desktop --control-port 60151 --control-secret-file rgv-control.secret \
  --control-root /path/to/review-workspace
```

Use `--settings-path` to isolate preferences during automation or testing.

The cytoband input is the standard tab-delimited UCSC form: contig, zero-based start, end, band
name, and stain (for example `chr1 0 2300000 p36.33 gneg`). Unknown contigs are ignored and invalid
or unavailable cytoband data falls back to a plain chromosome bar.
