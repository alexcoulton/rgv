# RGV feature reference

The complete list of what RGV supports today. For a short introduction, see the
[README](../README.md); for how to use each feature, the [user guide](USER_GUIDE.md).

- A native macOS, Linux, and Windows application shell.
- An IGV-like light desktop layout with application menus, compact locus navigation, assembly and
  contig selectors, locus history, chromosome context, adaptive coordinate ruler, fixed track-label
  gutter, contextual track controls, integrated scrollbar, and compact status bar.
- Cursor-centred zooming, drag panning, and clipped vertical scrolling across stacked tracks.
- Indexed local BAM queries with a four-thread decompression pool, plus local CRAM and HTTP/S
  BAM/CRAM through the same alignment scene. CRAM needs its reference FASTA loaded. Remote and
  CRAM alignments have automated coverage but have not yet had hands-on desktop testing on this
  branch.
- Standalone indexed FASTA genomes with FASTA-backed contig navigation, an independent pinned
  reference track, and bounded off-thread sequence fetching; the same FASTA also configures CRAM.
- All CIGAR operations, secondary/supplementary reads, arbitrary auxiliary fields, and missing
  sequence/quality states in the normalized alignment model.
- Two read-display modes per alignment track. **Downsampled** (the default for new tracks) shows
  at most 100 reads per 50-base window, chosen by a stable hash of the read name so the subset
  does not change while panning; the track label states how many reads are shown. **Complete**
  presents every eligible read or asks the user to zoom in or raise the limit. Coverage, alleles,
  junctions, and modification summaries always count every eligible read in both modes.
- Transactional pan/zoom previews retain orientation while coalesced exact queries settle, then
  switch atomically without fading; preview pixels are never exposed as current evidence.
- CPU read packing and instanced GPU rendering for coverage and CIGAR geometry.
- Generation-based cancellation so stale queries never replace a newer viewport.
- Reference bases, quality-aware mismatches, mate links, `MM`/`ML` overlays, picking, and a read
  inspector.
- Persisted CG/CHG/CHH/HCG/GCH/WCG bisulfite modes with both-strand reference interpretation,
  first-of-pair body colours, protected/converted marks at read and base detail, bounded rendering,
  a copyable legend, and per-read conversion summaries.
- Persisted PacBio SMRT kinetic modes for subread/CLR `ip` and `pw` arrays and CCS
  `fi`/`ri`/`fp`/`rp` arrays. Forward/reverse molecule-strand selection follows alignment
  orientation, per-base colouring uses a documented bounded scale, and missing or malformed arrays
  leave reads visible with explicit gutter and inspector status.
- A persisted Ultima flow-indel mode for signed `tp` direction arrays, `t0` deletion qualities,
  base qualities, and read-group `mc` limits. Insertions and deletions retain their ordinary shape
  while a bounded, position-and-colour confidence mark shows Q0–Q42; the inspector exposes exact
  quality, direction, source, homopolymer base, and unavailable reasons.
- A persisted, explicit SBX evidence mode. Insertions are purple, dark grey, or light grey for
  concordant, partly discordant, or fully discordant base-quality evidence; configurable simplex
  tails can be highlighted, hidden, or left unchanged. Thresholds, fallback state, omitted marks,
  and per-read evidence summaries remain visible rather than inferring the sequencing technology.
- MAPQ/flag/tag filtering plus strand, MAPQ, `RG`, `HP`, and arbitrary SAM-tag colour/group modes.
- Local BED/BED12, GFF3, GTF, genePred/refFlat, narrowPeak/broadPeak, PSL, GWAS/PLINK
  association summaries, indexed
  bigGenePred/bigNarrowPeak, indexed VCF/BCF, BigWig, TDF, bedGraph, WIG, generic BigBed,
  BEDPE/interact, multi-sample SEG/CBS copy-number, and cancer Mutation Annotation Format tracks.
- Bounded background annotation indexing and locus-bar autocomplete for feature names, gene names,
  aliases, IDs, and transcript IDs, with exact matches preferred and ambiguous choices shown.
- HTTP/S BAM, reference-dependent CRAM, FASTA, VCF/BCF, BigWig, TDF, and BigBed access. BAM, CRAM,
  CRAI, and FASTA/FAI use the observable pure-Rust range reader with a bounded disk cache, request
  coalescing, validator checks, safe redirects, retries, cooperative cancellation, and redacted
  authentication objects.
- Track ordering, visibility, and height controls; native file dialogs and drag-and-drop.
- Versioned JSON sessions, partial IGV XML-session import, and deterministic PNG export.
- A 512 MiB bounded cache hierarchy, stable directional prefetch, incremental GPU uploads, and
  observable cache/upload metrics.
- One authoritative columnar alignment batch with packed name, CIGAR, sequence, quality, and
  auxiliary-tag arenas; tags are decoded selectively and full records only on inspection.
- Persistent recent files/loci, configurable cache preferences, keyboard command search, and
  diagnostics that remain disabled unless explicitly enabled.

The execution roadmap is in [plans/RGV_IGV_PARITY_ROADMAP.md](../plans/RGV_IGV_PARITY_ROADMAP.md);
the complete planning index is [plans/README.md](../plans/README.md).
