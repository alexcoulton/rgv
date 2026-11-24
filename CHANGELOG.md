# Changelog

All notable stable changes to RGV will be recorded here. The format follows Keep a Changelog and
versions follow Semantic Versioning.

## Unreleased

## 0.1.1 — 2026-11-24

- An app icon: a stack of reads with one variant column. It is the macOS app icon, the Windows
  executable, installer and Start menu icon (with version information in the executable), the
  Linux desktop and AppImage icon and the window icon. Sources and the script that renders them
  are in `packaging/icons`.

- Windows packages (MSI installer and portable zip). Windows builds use the MinGW target
  (`x86_64-pc-windows-gnu`) so they read files through the same HTSlib as macOS and Linux;
  `hts-sys` and `rust-htslib` are vendored with small Windows patches (see `vendor/README.md`).
  CI builds and tests Windows the same way.

## 0.1.0 — 2026-11-24

First release: unsigned packages for macOS (Apple silicon and Intel) and Linux (AppImage and
tarball). Windows packages will follow.

- Repository and build hygiene from the 2026-11-20 review: pinned the toolchain in
  `rust-toolchain.toml` (1.93.1) and made every workflow use it; split CI into separate
  build-and-test and formatting/Clippy jobs per operating system; run each fuzz target as its own
  job; added `[profile.release]` (thin LTO, one codegen unit, stripped debuginfo) and a fat-LTO
  `[profile.dist]` used by the signed-release workflow; excluded `rgv-alignment-bench` from the
  workspace default members. Documentation now states that the matched IGV comparison has not
  been run, that local CRAM uses the `noodles` decoder, that "VCF and genotype review" is partial,
  and where SVG export, landmark gene bundles, and read filter predicates are reachable.

- Added a deterministic, ready-to-open mixed-format manual-test project under
  `benchmark-data/demo-tracks`. It combines the checked-in alignment/reference truth with 19
  auxiliary track examples and a SHA-256 manifest. The generator reopens the portable project,
  alignment, reference, every auxiliary source, both named TDF channels, and requires a non-empty
  production-engine query for every track before reporting success. A focused native Metal smoke
  additionally restored all 19 configured sources, expanded the two TDF channels, settled, exported
  the complete track inventory, rendered an SVG, and exited successfully.

- Added a companion four-panel alignment-review project backed by the checked-in semantic BAM. Its
  six-item Review queue covers paired, deep, RNA, structural-pair, long/supplementary/modified/
  SMRT/SBX, and 70,000-operation-CIGAR truth; generation fails if the core panel evidence is not
  queryable. A native Metal smoke restored all four panels, settled, exported the track inventory
  and a non-empty visible-workspace SVG, and exited successfully.

- Added an ordinary **File → Open URL** workflow for remote BAM/CRAM, FASTA, indexed VCF/BCF,
  BigWig, TDF, BigBed, bigGenePred, and bigNarrowPeak data. The dialog accepts custom
  alignment/reference indexes and an
  optional CRAM reference; each alignment now owns and persists its own custom BAI/CSI/CRAI URL,
  while the project persists its reference FAI URL. Signed source URLs retain their query for I/O
  but use redacted track and status labels. Desktop inference now also recognizes the advertised
  `.bed.gz`, `.gff3.gz`, and `.gtf.gz` paths and resolves indexed formats correctly through signed
  URL query strings. Custom index fields accept only bounded HTTP/S URLs attached to remote
  sources, and project serialization strips embedded credentials, query strings, and fragments
  from them just as it does for source URLs.

- Strengthened `--require-complete-matrix` so a private release corpus cannot pass by attaching
  technology labels to generic alignments. The gate now requires fourteen roadmap categories,
  including PacBio SMRT kinetics, SBX, Ultima, and reference-dependent CRAM, plus declared and
  observed paired, long-read, modified-base, spliced, bisulfite-context, kinetic-array,
  cell-barcode/UMI, supplementary, SBX, and Ultima evidence as applicable. Amplicon and high-depth
  rows require explicit depth minima, while the CRAM row requires a pinned reference and CRAM
  source. The synthetic corpus now makes its eight valid SMRT-kinetic records required truth.

- Added a strict packaged-automation qualification verifier. It matches every result to the exact
  checked-in command and order, rejects failed or incomplete reports, verifies every generated
  file and reviewed SVG digest, checks the saved multi-panel project identities and grouping/sort
  state, recomputes native frame summaries, enforces declared frame/GPU bounds, and emits a
  machine-readable digest-and-telemetry record. Tampered output and mismatched command identity
  have explicit regressions.

- Hidden tracks no longer consume routine viewport queries. Auxiliary-track requests now carry the
  exact stable-ID membership for the current workspace: visible tracks are queried, unrelated
  hidden tracks are skipped, and a hidden quantitative source is retained only when a visible
  arithmetic track depends on it. Hiding or showing a track advances every affected panel's
  generation so unread results cannot restore stale data; alignment visibility changes now apply
  the same stale-result invalidation to their independent BAM/CRAM projections.

- Fixed alignment-track removal and replacement across multi-locus workspaces. Every panel now
  drops selections, hover state, frames, and pending read navigation owned by the deleted stable
  track ID; promoting a replacement primary track synchronizes its retained query and statistics
  into every panel before redraw. Surviving tracks are re-queried with rebalanced workspace bounds.
  Cancelled BAM/CRAM actors are joined by bounded retirement tasks rather than blocking the native
  event thread, including when a complete controller set is replaced.

- Fixed auxiliary-track removal so stable track IDs are removed from the bounded background actor,
  not only from the active panel's visible configuration. Removal now invalidates unread results,
  refreshes every panel from the surviving actor state, clears stale selections, hover targets,
  search hits, settings panels, automation state, and quantitative arithmetic operands, and releases
  a shared TDF reader only after its final embedded channel is removed. Single-track and metadata
  bulk removal use the same path; actor-ordering, multi-panel state, arithmetic dependency, and
  shared-source lifetime regressions cover it.

- Added indexed read-only TDF versions 1–4 for archived IGV quantitative data. Local and HTTP/S
  sources read only bounded header, index, dataset, and overlapping tile ranges; compressed and
  uncompressed fixedStep, variableStep, BED, and named-BED tiles are supported. Adaptive raw or
  precomputed resolution feeds the existing quantitative layouts and controls, while the
  inspector and structured export preserve format, genome, dataset, window-function, compression,
  and selected-channel provenance. Multi-channel sources expand asynchronously into independent
  source-ordered tracks which share one reader and bounded tile cache; exact channel indices persist
  without duplicate expansion after restore. CLI, file/drop inference, project restoration, resource-limit,
  malformed-input, compressed-fixture, and fuzz paths are included.

- Added first-class bounded GWAS Manhattan tracks for plain/gzip `.linear`, `.logistic`, `.assoc`,
  `.qassoc`, and `.gwas` summaries. Flexible IGV/PLINK header aliases locate chromosome,
  one-based position, identifier, and p-value columns; all additional columns and the original
  p-value spelling remain available for exact inspection. Rendering applies `-log10(p)` with a
  safe zero-p-value ceiling, stable chromosome, alternating, or uniform colours, selectable point
  sizes and vertical maximum, two-dimensional point picking, visible bounded-query provenance,
  identifier search, previous/next navigation, CLI/file/drop integration, session persistence,
  structured selected-evidence export, and malformed/gzip/fuzz coverage.

- Added bounded ENCODE broadPeak and UCSC nucleotide PSL tracks. broadPeak preserves its BED6+3
  score, strand, signal, and optional p/q statistics without inventing a point summit. PSL accepts
  the standard 21 fields and optional table bin, skips standard psLayout headers, validates count,
  span, insertion and block arrays, maps reverse query and target coordinates, retains alignment
  counts and query provenance, and rejects translated protein blocks rather than drawing them at
  nucleotide scale. Plain/gzip I/O, CLI, file/drop/session inference, name search, previous/next
  navigation, inspection, and export use the existing bounded feature path. Feature and gene
  tracks now persist true collapsed, squished, and expanded row layout modes, with picking kept in
  exact lockstep with rendered packing.

- Added typed indexed UCSC bigGenePred and ENCODE bigNarrowPeak tracks through the existing local
  and HTTP range-backed BigBed reader. Canonical field counts, coordinate bounds, integer scores,
  strands, exon arrays, CDS states, frames, peak statistics, and summits are validated; gene/CDS
  and peak/summit rendering, picking, and copyable inspection reuse the existing typed models.
  Queries, records, blocks, feature search, and adjacent-feature navigation have explicit bounds.
  `--big-gene-pred` and `--big-narrow-peak`, recognized long extensions, file dialogs,
  drag-and-drop, generated indexed binary truth, malformed-record errors, and an explicit saved
  format hint cover ambiguous `.bb` project restoration without eager loading.

- Corrected the execution order to finish the remaining major feature gaps before broad manual and
  performance qualification. Added persisted, reference-aware bisulfite colouring for CG, CHG,
  CHH, HCG, GCH, and WCG contexts on both strands. Grey/sage bodies encode first-of-pair strand;
  red and blue marks distinguish protected and converted evidence at read and base detail. The
  mode has a copyable legend, missing-reference disclosure, bounded marker rendering with omitted
  counts, per-read inspector summaries, session round trips, and exact context/strand tests.

- Added persisted PacBio SMRT kinetic colouring for subread/CLR `ip` and `pw` arrays and CCS
  forward/reverse `fi`, `ri`, `fp`, and `rp` arrays. Tag selection and array indexing account for
  alignment orientation; the documented red-to-yellow-to-cyan scale is visible at read and base
  detail. Integer type, non-negative values, sequence length, and a per-record safety bound are
  validated. Missing or malformed evidence leaves the read visible and is disclosed in the gutter
  and inspector, while a separate display cap reports omitted kinetic marks.

- Added persisted Ultima flow-indel evidence with directional `tp` or minimum-homopolymer quality
  methods, printable `t0` single-deletion qualities, read-group `PL`/`mc` metadata, an explicit
  track-level `mc` override, and the same 44-step Q0–Q42 jet convention as IGV. Confidence is
  encoded by both colour and position inside the ordinary insertion/deletion marker. Exact quality,
  signed direction, evidence source, homopolymer base, missing/malformed state, unavailable-event
  reason, and bounded-display omissions are exposed through the legend, gutter, and inspector.
  Generated indexed BAM/FASTA truth covers directional insertion Q40, maximum-homopolymer Q0, and
  `t0` deletion Q35 through the production query path.

- Added an explicit persisted SBX evidence mode with configurable discordant-base and simplex-tail
  quality thresholds. Insertion markers distinguish concordant, partly discordant, and fully
  discordant evidence; low-quality read-end tails can be highlighted, hidden, or left unchanged at
  both read and base detail. Reverse orientation, adjacent and containing-homopolymer checks,
  missing qualities, bounded tail rendering, gutter disclosure, copyable legends, read inspection,
  and generated full/partial/concordant truth are covered without inferring instrument type.

- Completed the controlled alignment-backend matrix retained for Phase 9 qualification. Local BAM now has explicit BAI/CSI,
  missing-index, corrupt-index, and truncated-source regression coverage. Remote BAM exercises both
  conventional BAI and CSI names plus custom indexes; remote CRAM exercises both CRAI names plus a
  custom index, authenticated signed FASTA/FAI, reference mismatch, and truncated data. Range tests
  cover redirects, transient failures, cancellation before and during a request, validators, short
  bodies, bounded eviction, and reopen recovery. Qualification fixed disk-cache files not being
  removed or counted on capacity eviction and converted a `rust-htslib` null-header panic for a
  severely truncated indexed BAM into an explicit `InvalidAlignmentHeader` error. The archived WAN
  report now separates range-only and total time and includes retry, eviction, and cancellation
  counters.

- Completed reproducible package staging for macOS, Linux, and Windows. Development artifacts are
  now an app ZIP, Linux tarball, and Windows portable ZIP/MSI rather than raw executables, while the
  signed DMG/MSI/AppImage workflow reuses the same verified layouts. Packages contain the MIT and
  Apache-2.0 project licenses, full deduplicated dependency license/attribution texts, a locked-graph
  CycloneDX 1.6 SBOM, credential-safe build provenance, and an internal SHA-256 manifest. Final
  platform-named SBOM/provenance files, checksums, and GitHub build attestations are published beside
  release artifacts. Four standard-library-only packaging tests cover dependency reachability,
  every platform layout, metadata integrity, and mutation detection.

- Added observable pure-Rust remote CRAM/CRAI and indexed FASTA/FAI access beside the existing
  remote BAM path. Remote BAM now discovers both conventional BAI/CSI filename forms; CRAM does the
  same for CRAI, and both accept custom signed index URLs. Bounded index downloads, exact range and
  object-validator checks, disk-backed range caching, bearer authentication, credential-safe
  errors, transient probe/data retries, redirect-hop accounting, HTTPS downgrade rejection, and
  cooperative generation cancellation are shared across alignment and reference traffic. CRAM
  reference decoding has a 512 MiB per-query cap and clears full-contig decoder state after every
  query. A generated reference-dependent CRAM fixture verifies authenticated remote/local core,
  CIGAR, pileup, junction, and pair parity, custom and conventional CRAI discovery, request/cache
  observability, signed-query preservation, and malformed CRAI rejection.

- Added the first end-to-end deterministic desktop automation slice. A bounded versioned JSON
  format covers workspace, genome/track, locus/panel, option, grouping, sorting, filtering,
  selection, settled-state, snapshot, export, project-save, and exit commands. Local paths resolve
  beside the batch; source opens wait for actual background-reader readiness; settled state checks
  completed generations rather than elapsed time; asynchronous saves and exports route results by
  command identity. Visible workspace/panel and bounded offscreen full-panel PNG/SVG snapshots,
  alignment, pileup, modified-base, junction, variant/genotype, quantitative-window,
  selected-evidence, region BED, and track-name exports run off the UI thread with source-safe
  provenance and explicit bounds. The first failure skips remaining commands, reports stable error
  codes, and an exit waits for an atomically flushed credential-safe report. A checked-in
  nineteen-command two-panel BAM smoke batch produced byte-identical visible and full-panel SVGs on
  repeated release runs.

- Added an opt-in local control service that reuses the versioned batch vocabulary and returns the
  same structured command results from the live desktop window. It is disabled by default, binds
  exclusively to IPv4 loopback, requires a bounded bearer secret loaded from a protected file,
  never logs or persists that secret, serializes bounded requests, and stops cleanly with the
  application. Local input/output paths are confined lexically to an explicitly selected control
  root. Transport, authorization rejection, secret validation, path confinement, and the live
  release executor are covered.

- Added the native AccessKit window integration and an accessibility implementation pass. Custom
  genome and track overview controls now expose named slider values, keyboard pan/zoom/scroll/density
  behavior, and visible focus rings; track labels expose selection state plus keyboard settings,
  visibility, collapse, ordering, removal, and resizing; supplementary-chain segments and the
  genomic canvas expose semantic nodes. Keyboard command search covers panels, tracks, evidence,
  metadata, ROIs, and review queues, while status changes produce accessibility events. Persistent
  interface scaling (75–200%), increased contrast, and reduced-motion preferences are backward
  compatible with existing settings. The close audit added explicit names to metadata, ROI, review,
  variant, and track-setting controls and fixed light-theme activation so contrast and reduced
  motion affect the active UI. Automated AccessKit-tree tests cover custom sliders, track selection,
  resize handles, status, and supplementary segments.

- Completed portable project saving and recovery. Project-contained sources are persisted relative
  to the project JSON while external sources remain canonical absolute paths; relative paths reopen
  after relocating the complete directory and every missing local source is listed independently.
  Manual saves and opt-in periodic recovery snapshots serialize off the UI thread and atomically
  replace only a fully flushed validated document. Autosave interval and per-project retention are
  bounded, unclean exits offer explicit recovery selection, and a recovered snapshot opens unsaved
  to prevent accidental replacement. Ordered panels now retain independent read-stack scroll
  positions. Versioned migration/future-rejection fixtures cover every schema boundary, and IGV XML
  import now resolves relative resources, restores supported track state, and displays a bounded
  imported/approximated/ignored semantic report instead of silently dropping renderer state.

- Added UCSC genePred/refFlat and ENCODE narrowPeak tracks, including transparent gzip input and
  optional UCSC table-bin columns. Transcript parsing preserves validated exon arrays, coding
  regions, gene/transcript identity, extended status/frame fields, and renders coding exons thicker
  than UTRs. narrowPeak preserves score, signal, p/q statistics, and its called summit, which is
  rendered explicitly. Exact packed-row selection opens a bounded, copyable feature inspector;
  source lines, records, blocks, and panel results have deterministic failure limits.
- Added bounded GDC/TCGA Mutation Annotation Format review. Header-name parsing preserves stable
  tumour-sample identity, one-based coordinates, gene/classification/type/allele semantics, and
  ordered non-empty annotations with explicit source, field, column, and query caps. The desktop
  virtualizes sample rows, provides classification/type/uniform colours, repeated-call markers,
  exact picking, a scrollable copyable provenance inspector, and persisted track settings. The
  `.maf`, `.maf.txt`, and `.maf.gz` sources are interpreted as cancer Mutation Annotation Format;
  UCSC Multiple Alignment Format remains unsupported.
- Added first-class multi-sample SEG/CBS copy-number review. The bounded parser preserves sample
  identity and marker counts, converts standard one-based inclusive coordinates (or honours an
  explicit `coords=0` track directive), ignores invalid numeric rows with provenance, and infers
  log-ratio versus absolute-copy semantics without rewriting values. The desktop virtualizes sample
  rows, supports automatic/fixed/shared heatmap ranges, neutral baselines, colours, opacity,
  persisted base-weighted amplification/deletion sorting at a chosen locus, exact selection, and a
  copyable provenance inspector. Sessions retain every copy-number setting.
- Added the common quantitative and interaction workflow for local BigWig, bedGraph, WIG, BEDPE,
  and UCSC interact sources. Quantitative tracks now have deterministic raw/minimum/weighted-mean/
  maximum windows, automatic/fixed/shared scales, linear/signed-log transforms, four display modes,
  explicit missing-data behavior, colours, opacity, and bounded arithmetic composition. Persisted
  overlay groups now combine compatible signals in one frame, synchronize transforms and shared
  ranges, separate explicitly, retain exact member selection, and bound their gutter labels.
  Interaction tracks provide filtered arcs or blocks, cross-contig partner markers, exact
  inspection, and linked endpoint panels. Query, rendering, labels, and composition all expose
  deterministic caps.

- Added the persisted cohort-review workflow for variant tracks. Query-time filters now cover type,
  QUAL, PASS/named FILTER values, AF, genotype class, and typed INFO/FORMAT/GT conditions with
  explicit missing-value rules. A searched sample subset reduces decoding work, genotype rows can
  be grouped and coloured by call class, and marker colours can summarize the selected cohort.
  Sample membership uses linear-time lookup and the query discloses hard 100,000-header-sample,
  10,000-sample-per-record, and 250,000-cell limits. The variant inspector links exact pileup,
  indel, modification, pair/read context, and structural endpoints without mislabelling overlap as
  support. Bounded variant/genotype and evidence TSV exports are written off the UI thread and
  joined safely at shutdown.
- Replaced the lossy VCF/BCF record path with a bounded typed variant query. Header descriptions,
  all alleles, FILTER, typed INFO/FORMAT values, genotype allele indices and phasing, symbolic SV
  classes, declared `END`, and BND partner/orientation semantics now survive the engine snapshot.
  Variant rendering uses stable type colours; click selection opens a copyable inspector with
  header-described INFO, virtualized sample-searchable genotype rows, explicit truncation
  provenance, and linked-panel BND navigation. An indexed generated BCF truth fixture covers SNV,
  MNV, insertion, deletion, multi-allelic, symbolic deletion/inversion, BND, filters, INFO, missing
  values, and phased/unphased genotypes.
- Replaced the text-only supplementary-alignment workflow with a bounded, clickable two-lane chain
  diagram. Read order uses strand-aware clipped query coordinates; alignment order uses stable
  contig/position ordering; exact segment evidence remains available, and active or all segments
  can open in linked panels subject to the workspace cap. Exact loaded read/template-name selection
  and a non-persisted selected-to-top override now promote linked records ahead of configured sort
  order, invalidate row/layout caches correctly, and preserve stable identities without saving
  private read names.
- Completed bounded alignment evidence clipboard actions. The read inspector now copies its full
  visible details, CIGAR, reference-forward read sequence, all loaded tags or one selected tag, and
  the visible reference sequence; the pileup inspector exposes the same reference sequence beside
  its declared quality-weighted consensus. Read, reference, consensus, event, tag-value, and
  combined-tag payloads have explicit limits and report truncation instead of creating unbounded UI
  or clipboard work.
- Completed alignment ordering controls with independent natural/reverse group and sort direction,
  centre-base, strand, first-of-pair-strand, read-group, haplotype, and custom-tag sorting, plus an
  explicit re-pack action. Base sorting follows each panel independently, uses displayed
  reverse-strand orientation, distinguishes deletions/skips/unavailable sequence, remains bounded,
  and participates in row and layout-cache identity. All settings persist in backward-compatible
  schema-v3 sessions, whose custom style tags are now validated as SAM tags.
- Added ordered side-by-side loci with stable `PanelId`, panel-generation, and panel/track
  projection identities. Each panel owns its camera, history, selection, read-stack scroll,
  retained query, layout, auxiliary snapshots, and generation. The locus field accepts an
  atomically validated whitespace-separated coordinate list; the panel selector activates,
  reorders, and closes panels. Alignment and auxiliary engine results route independently by panel,
  unread viewport results coalesce per panel, layout cache entries include panel and track, and
  inactive panels cannot mirror an active panel's GPU selection ID. Window resize and
  query-affecting settings refresh every panel projection.
- Introduced session schema version 3 with ordered panel loci and an explicit active-panel stable
  ID. Version-1 and version-2 sessions migrate deterministically to one panel, invalid or excessive
  panel sets fail before viewer mutation, and both initial `--session` launch and in-app session
  application construct every saved panel and alignment-track/panel projection.
- Added linked-evidence panel actions for mapped mates and supplementary-chain segments. The origin
  panel remains intact, the destination uses the selected track's dictionary, and a pending stable
  read-name selection resolves against the destination query without coupling panel generations.
- Aligned corresponding downstream tracks across panel columns by reserving the tallest natural
  primary-alignment frame while retaining each panel's complete scrollable read stack. Added a
  reviewed deterministic multi-panel raster and a direct two-track/two-panel query-routing gate.
- Introduced session schema version 2 with ordered, stable-ID alignment-track records containing
  independent source, name, visibility, collapse, height, filters, and record cap. Version-1
  sessions migrate deterministically, all alignment resources survive IGV import, and every entry
  becomes a live controller with an independent engine, generation, lifecycle, query result, rows,
  cache statistics, and bounded rendering. Opening another BAM/CRAM adds a track and rebalances
  cache allocation; secondary tracks can be renamed, hidden, collapsed, resized, reordered, and
  removed. Decoded cache bytes and retained-record caps are divided across the workspace rather
  than multiplied per track, and persisted sources remain credential-free. Semantic read and
  aggregate inspection is routed by stable track ID, loci are resolved through each BAM/CRAM's own
  reference dictionary, and a missing chromosome or exhausted record allocation remains local to
  the affected track. The complete settings surface now targets any selected alignment track with
  independent draft/error state, and drag ordering can promote or demote the first track without
  reopening its controller or changing its stable identity.
- Expanded diagnostics from primary-only counters to workspace totals and per-alignment lifecycle,
  retained-record, and decoded-cache rows. Added a schema-tested four-track acceptance session with
  three distinct healthy controllers and one intentional source failure. An automated check proves
  that all three healthy controllers still complete deep-locus queries, and the interaction
  benchmark now records one/two/four-track scaling under shared record/cache budgets with native RSS.
- Added Shift/Ctrl/Cmd alignment-track multi-selection with group highlighting and compatible bulk
  visibility, collapse, and appearance actions. Bulk appearance explicitly preserves per-sample
  MAPQ/base-quality filters, SAM-tag predicates, inclusion switches, and record caps.
- Added sampling-independent splice-junction aggregation from CIGAR `N` operations, explicit
  `XS`/alignment-strand/invalid-tag provenance, a bounded strand-aware junction lane, persisted
  visibility and minimum-depth filtering, and copyable click-or-hover inspection.
- Completed the RNA implementation slice with exact query-time minimum-flank filtering, per-event
  strand-source and flank provenance, index-derived CPM denominators, collapsed/expanded Sashimi
  layouts, raw/CPM count labels, automatic/fixed/shared multi-sample scales, transcript-aware
  annotated/novel/unknown markers, and background-written bounded junction BED and Sashimi PNG/SVG
  exports. The generated indexed BAM now contains exact `XS:+`, `XS:-`, missing, and malformed
  strand truth.
- Added session-persisted visibility for the reference, coverage, modification-frequency, and read
  lanes; automatic or fixed coverage scaling with an observed-depth label; and expanded, squished,
  and collapsed read-density presets that remain compatible with continuous vertical zoom, plus a
  persisted click-or-hover inspector activation preference.
- Added semantic CIGAR-event picking for insertions, deletions, skips, clipping, and padding, with
  event coordinates, bounded sequence/quality previews, reverse-strand orientation, and capped
  sequence copying in the read inspector.
- Added session-persisted mismatch-only, all-base, and hidden read-base modes, optional
  base-quality shading, and independent minimum indel-marker and event-label thresholds.
- Added session-persisted modified-base visibility, probability, stable per-type colour, legend,
  and mono/two-colour controls. A pinned, pickable modification-frequency lane now aggregates exact
  or binned raw and probability-weighted evidence from every eligible read, split by modification
  strand, alignment strand, and optional haplotype, with explicit missing-`ML`, malformed-tag,
  unavailable-sequence, incomplete-query, and bounded-series provenance.
- Added bounded `SA` parsing and supplementary-chain inspection with current/declared segment
  coordinates, strand, MAPQ, edit distance, CIGAR, breakpoints, malformed/truncated provenance,
  chain-wide loaded-segment highlighting, and direct/previous/next navigation. Alignment settings
  now include session-persisted chimeric/supplementary/clip grouping and aligned-length/clip sorting.
- Added a normalized paired-read model with stable template identity and explicit proper,
  concordant, discordant-length, discordant-orientation, inter-chromosomal, unmapped, missing, and
  out-of-view mate states. The session-persisted **View as pairs** mode packs both primary ends and
  one connector onto a stable row and one pick target. A deterministic 4,096-template summary
  infers robust expected-length bounds from all eligible proper templates before display sampling;
  track settings expose sample provenance and validated overrides. Pair-aware colour, group, sort,
  and copyable inspector fields cover template length, orientation, unexpected status, mate
  chromosome, first-of-pair strand, and read order. Pileup now counts overlapping primary mates
  once by default with a bounded, provenance-reporting leftmost-end policy; a persisted count-both
  option remains available.
- Replaced depth-only coverage vectors with bounded allele-aware pileups carrying A/C/G/T/N,
  insertion/deletion, strand, base-quality, mapping-quality, and full-evidence provenance. Exact
  base views now colour reference-discordant support at a configurable fraction, expose a copyable
  pileup inspector and base-only consensus, and persist pileup controls in version-1 sessions.
- Accepted partial and differently ordered FASTA dictionaries by validating shared contigs and
  resolving reference slices by contig name, preventing BAM startup from remaining on the
  synthetic placeholder for subset references.
- Froze the reference and coverage lanes above the vertically scrollable read stack, including
  matching pinned gutter labels and clipping that prevents reads from entering the header region.
- Increased base-label legibility with larger glyphs, dark base-specific reference lettering, and
  fully opaque contrast-adaptive lettering on read mismatch blocks.
- Kept alignment body colors continuous across the Reads-to-Bases detail boundary; exact matches
  retain the configured read color while only true mismatches receive strong nucleotide colors.
- Corrected mapped flags and BAM bins in generated corpora so indexed queries beginning inside an
  alignment return that alignment at extreme zoom, rather than only finding reads when the query
  includes their starting coordinate.
- Replaced rectangle-built 3×5 base and alignment-event glyphs with Retina-aware anti-aliased
  monospace text, including deterministic font rendering in PNG exports, and let a lone alignment
  track fill otherwise unused canvas height.
- Added Ableton-style two-axis overview navigation: chromosome pan/zoom gestures and vertical
  scroll/read-density gestures with pointer anchoring, live scale-coupled pan sensitivity,
  simultaneous diagonal control, hidden captured-pointer input for unbounded motion, matching
  navigator styling, fractional base-scale camera motion, and commit-on-release history.
- Extended vertical magnification from 21-pixel to approximately 64-pixel alignment rows, while
  scaling auxiliary tracks more gently above their normal height.
- Decoupled horizontal level-of-detail transitions from vertical alignment geometry, keeping the
  reference, coverage, and read stack anchored while zooming through base detail.
- Added viewport-independent alignment row maps shared across Regional, Reads, and Bases detail,
  with stable read identities that carry row assignments into refined queries.
- Fine-scale chromosome-handle presses now enter relative pointer capture immediately and cannot be
  misclassified as chromosome-wide rail clicks when the intended drag is below the GUI threshold.
- Zoom-out gestures retain and render the overlapping portion of the current query instead of
  blanking the track, while throttled refinement queries fill the expanding viewport before release.

### Added

- Opened indexed FASTA as a standalone genome source through the command line, the Genomes menu,
  drag-and-drop, and sessions. FASTA indexing and sequence fetches run in a bounded background
  actor; the dictionary drives navigation without a BAM, and base-detail sequence renders in its
  own pinned reference track and powers the bounded visible-reference copy action.
- Added portable schema-versioned genome configuration files for a FASTA, assembly name, local
  cytobands, explicit contig aliases, and default annotations. Relative paths resolve from the
  bundle, aliases reject canonical conflicts or missing targets, and the resulting genome metadata
  persists in RGV sessions.
- Added bounded background indexes for text-annotation names, aliases, gene/transcript identifiers,
  and locus-bar autocomplete. Results are deterministically ordered, unique exact matches navigate
  directly, ambiguous matches expose their track and coordinates for explicit selection, and the
  Regions menu navigates to the nearest previous or next annotation feature.
- Added lazy bounded VCF/BCF ID indexing to submitted locus-bar searches without adding a
  whole-file variant scan to normal track startup or ordinary annotation autocomplete.
- Added persisted reference-strand display and selectable +1/+2/+3/−1/−2/−3 translation lanes with
  explicit standard and vertebrate-mitochondrial genetic codes; visible-reference copy follows the
  selected strand.
- Added complete regions-of-interest and review-queue workflows. Named, colored regions can be
  created, relocated, navigated, deleted, imported/exported as BED, persisted in sessions, and
  rendered as clipped overlays across every matching panel and track. Current loci or selected
  annotation features can enter a bounded review queue with wrapping previous/next navigation,
  status filtering, editable notes and assignment, versioned TSV interchange, and no automatic
  capture of read names or sequence. Imports and exports run off the UI thread with explicit byte,
  record, string, note, rendering, and task bounds.
- Added bounded sample-attribute TSV import, a resizable virtualized metadata table, stable
  per-track sample associations, cross-type selection by attribute value, and session persistence.
  Metadata selections drive compatible visibility, collapse, rename, height, alignment setting and
  filter, quantitative autoscale/overlay/separation, same-type appearance/color, and confirmed
  removal operations. Variant genotype rows now support explicit pedigree ordering from recognized
  parent columns with a visible VCF-order fallback. Inputs are capped at 64 MiB, 100,000 samples,
  256 attributes, one million cells, and bounded field sizes.
- Native Rust desktop genome viewer with GPU alignment, coverage, reference, feature, gene,
  variant, and quantitative rendering.
- Local and remote BAM/CRAM plus essential genomic track formats.
- Version 1 JSON sessions and partial IGV XML import.
- Bounded caches, deterministic deep-coverage sampling, performance gates, corpus validation, and
  signed cross-platform release workflows.
- Packed columnar alignment storage with lazy selected-tag decoding and on-demand inspector record
  materialization.
- Bounded actor output mailboxes that retain the newest unread viewport result without dropping
  control events, plus deadlock-free shutdown when result queues are full.
- IGV-like light desktop shell with application menus, compact assembly/contig/locus navigation,
  locus history, cytoband-aware ideograms, adaptive coordinate ruler, fixed track-label gutter,
  integrated track scrolling, compact status, and opt-in diagnostics.
- Contextual track settings with naming, visibility, collapse, height, colour/group/sort/filter
  controls where applicable, drag resize/reorder, source tooltips, accessible labels, and current-view
  export.
- Pale outlined reads with bounded event labels and distinct mismatch, insertion, deletion,
  reference-skip, clipping, mate-connector, hover, and selection semantics.

### Security

- Diagnostics default off; authentication and URL credentials are redacted from logs, settings,
  and sessions.

### Fixed

- Rectangle renderer pipeline validation on strict GPU backends by exposing the shared screen
  uniform to both the vertex and fragment shader stages that read it.
- Zoom navigation now retains the last useful frame and immediately relayouts prefetched data when
  possible instead of flashing an empty canvas while a new query settles.
- Vertical alignment navigation now has a persistent right-edge scrollbar, toolbar buttons, and
  Page Up/Down and arrow-key bindings; Shift+wheel also handles the horizontal wheel deltas emitted
  by macOS.
- Alignment tracks now use an IGV-like white workspace, light application chrome, grey default
  reads and coverage, and pink/purple strand colouring. Deep retained reads are packed into a
  bounded scrollable row stack instead of being capped at the first 90 rows.
- Deep-coverage sampling now uses stable 50-base genomic windows rather than one viewport-wide
  competition, preventing surrounding regions from reshuffling reads already in view. A black bar
  below coverage marks sampled alignment views.
- Regional-detail alignments now reuse rows after earlier genomic spans end instead of assigning
  rows by record index, preventing reads from collapsing into sparse vertical stripes when zooming
  out across the Reads-to-Regional detail boundary.
