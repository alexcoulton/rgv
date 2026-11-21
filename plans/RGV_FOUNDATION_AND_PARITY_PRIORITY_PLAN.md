# RGV foundation and parity-priority plan

Status: **in progress**, started 11 November 2026. See section 15 for what has landed.

Baseline: `9972c06` on `feature/unified-alignment-scene`.

## 1. Purpose

RGV's goal is a genome viewer that is much faster than IGV Desktop with feature parity. An
independent assessment on 11 November 2026 found that the project is further from that goal than
`RGV_IGV_PARITY_ROADMAP.md` reports, for three reasons:

1. **Core formats regressed.** The unified alignment scene opens local BAM only. Local CRAM and all
   remote BAM/CRAM fail at open (`apps/rgv-desktop/src/workspace/alignments.rs:2222-2235`); Phase 10
   of `UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md` is "not started". The README still
   advertises them.
2. **The speed claim is unproven, and cold load is slow.** Warm camera updates take 0.01–2 ms.
   First useful frame takes 1.0 s (exome BRCA1, 69k reads), 2.2 s (`mixed.bam`, 156k reads, against
   a 250 ms gate) and 2.8 s (ACTB, 693k reads, 741 MiB RSS). The matched IGV comparison in
   `benchmarks/IGV_COMPARISON.md` has never been run.
3. **No parity gap has closed since `audits/FEATURE_AUDIT_2026-08-25.md`.** All later work went
   into alignment scene and cache remediation.

This plan orders the work that fixes those three things first. It supersedes the ordering in the
roadmap's Phase 9 for engineering work; the roadmap remains the record of what was built.

## 2. Decisions already made

| ID | Decision | Made |
| --- | --- | --- |
| D1 | Read display has two modes per alignment track: **Downsampled** (default) and **Complete** (opt-in, today's complete-or-refuse behaviour). Coverage is exact from every eligible read in both. | 11 Nov 2026 |
| D2 | Genomes are compatible with IGV's hosted genome list and genome JSON. `.rgvgenome` stays readable but is not extended. | 11 Nov 2026 |
| D3 | IGV batch-script and port-60151 compatibility is wanted for parity but is not urgent; it is scheduled after the foundation work. | 11 Nov 2026 |

## 3. Ordering principle

1. Measure before changing, using the headless benchmarks; IGV comparison is deferred.
2. Fix what every user hits on every load (cold latency, deep loci, missing reference) before
   adding formats.
3. Restore regressed formats before adding new ones.
4. Change an architectural boundary only when a scheduled feature needs it, and land the boundary
   with that feature.

Workstreams A–C are sequential. D can run in parallel with A–C. E–G follow.

```text
A baseline ─> B display modes + cold load ─> C CRAM/remote ─┐
D genome catalogue (parallel) ──────────────────────────────┼─> F desktop structure ─> G parity queue
                              E track-source boundary ──────┘
H small everyday fixes (any time; independent)
```

## 4. Workstream A — truthful baseline

**Goal:** a known-good starting point and documentation that matches the code.

- Rerun the standard gate (fmt, `cargo test --workspace --all-targets --locked`, strict Clippy,
  release build) on the baseline commit and record the result.
- Correct the README, `docs/FORMAT_SUPPORT.md` and the roadmap's status table so that CRAM, remote
  alignments and "VCF and genotype review" are not reported as complete.
- Use the existing headless `rgv-interaction-bench` as the performance measure for workstream B.

**Deferred by the user on 11 November 2026:** the matched IGV comparison and all hands-on UI
testing. Work proceeds on headless tests and benchmarks; the user will test the UI afterwards.

**Gate:** the automated gate result is recorded; documentation matches the code.

## 5. Workstream B — read display modes and cold load

This is the largest lever on both speed and stability. The trail of seven remediation plans traces
back to one cause: holding and laying out every read in a locus (about 860 bytes per read; ACTB
produced 57,861 rows and 2.56 GB of semantic pages against a 4 GiB budget).

### B1. Downsampled mode (default)

- **Model.** Add a persisted per-track read-presentation setting: `Downsampled { window_bp,
  max_reads_per_window }` or `Complete`. Defaults follow IGV: 50 bp window, 100 reads. IGV does not
  downsample long-read tracks by default; decide RGV's long-read default from headless measurement.
- **Where sampling happens.** In the decode pass of the source backend
  (`crates/rgv-engine/src/local_bam_source.rs`), before records enter the evidence store, so that
  memory is bounded by the view and not by depth.
- **Coverage stays exact.** Summary contributions (coverage, alleles, junctions, modifications)
  must be accumulated from every eligible record in the decode pass, including records that
  sampling drops. Verify first how the summary tiles are currently fed; if they are derived from
  retained evidence chunks, that derivation moves into the decode pass.
- **Algorithm.** Streaming, per window keyed by alignment start: keep the `max_reads_per_window`
  records with the smallest hash of (read name, track seed). This is deterministic, independent of
  arrival order and stable under panning, and mates share a hash so pairs are usually kept or
  dropped together. A mate that is kept alone is drawn with an explicit "mate not shown" state.
- **Disclosure.** Mark downsampled windows under the coverage lane (IGV uses a black bar), state
  kept/total counts in the gutter and inspector, and carry the mode in exports and automation
  reports.
- **Visibility range.** Beyond a per-track span (IGV defaults: 30 kb, 300 kb for RNA, 1,000 kb for
  long reads) show coverage only. Reconcile with the existing thresholds in
  `crates/rgv-core/src/lod.rs`.
- **Sessions.** New tracks default to Downsampled. Sessions saved without the field load as
  Complete so they reproduce what was saved.

### B2. Complete mode (opt-in)

Keeps the current contract: every eligible read or an explicit refusal above the configured limit.
No behaviour change, but it no longer has to be the path every user takes.

### B3. Cold decode

- Reuse one `bam::Record` across the loop instead of allocating per record
  (`crates/rgv-hts/src/local.rs:1122`).
- Decode directly into the columnar chunk; remove the owned-record, per-block sort and copy stages
  (`local_bam_source.rs:136-193`) and the 6x scratch reservation (`:307`).
- Publish progressively: exact coverage and the rows intersecting the viewport first, the rest of
  the working set afterwards.
- Share one HTSlib decompression pool across tracks; each open currently creates its own
  (`local.rs:943`).

### B4. Worker structure and cancellation

- Split decode from presentation building so they do not share one serial thread per source
  (`crates/rgv-engine/src/alignment_scene_engine.rs:910-1140`).
- Replace the hard-wired `keep_going = || true` (`local_bam_source.rs:292`) with a real cancellation
  token checked inside a block, so a superseded navigation stops promptly.

### B5. Resource accounting

Once Downsampled mode bounds memory by the view, measure whether the global `AllocationLedger`
(`crates/rgv-alignment-scene/src/resource.rs:822-889`: one mutex, one hash-map entry per
allocation, shared by every worker and the UI) is still needed on that path. The expected outcome
is per-store byte counters with a coarse LRU for Downsampled mode, and the ledger confined to
Complete mode. Decide from the measurement; do not delete it speculatively.

### Gates

- Cold first useful frame at most 250 ms on `mixed.bam chr1:900001-920000`.
- Downsampled-mode RSS at ACTB under 200 MiB (from 741 MiB).
- Coverage, allele and junction summaries are byte-identical between Downsampled and Complete.
- The same reads are shown at a position before and after panning away and back.
- End-to-end tests drive the real engine from open to published frame; structural tests alone do
  not close a gate.

## 6. Workstream C — CRAM and remote alignments on the scene path

- Implement `CanonicalAlignmentSourceBackend` for local CRAM, then remote BAM, then remote CRAM.
  The only production implementation today is local BAM (`local_bam_source.rs:247`).
- Stable read handles need a physical position per format: BAM virtual offsets exist; CRAM uses
  container/slice/record positions (HTS-layer evidence exists but is not connected).
- Reuse `crates/rgv-hts/src/remote_alignment.rs` (currently unreachable from the desktop) rather
  than writing a second remote reader. Delete the `AlignmentQuery` oracle code that no production
  caller uses once the backends land.
- Doing this after B means each backend implements sampling and progressive publication once.

**Gate:** the existing local/remote and BAM/CRAM parity fixtures pass through the scene path; the
"no legacy fallback" failures are gone; README claims are true again.

## 7. Workstream D — IGV-compatible genomes

Verified on 11 November 2026: IGV Desktop reads a tab-delimited genome list
(`https://igv.org/genomes/genomes.tsv`: name, URL to a genome JSON, id). The hosted `hg38.json`
contains `id`, `name`, `twoBitURL`, `cytobandURL`, `aliasURL`, `chromSizesURL`, `chromosomeOrder`,
`wholeGenomeView`, `tracks` (each with `id`, `name`, `url`, `format`, `order`, `html`) and `hubs`.
It has no `fastaURL`, so a 2bit reader is required, not optional.

- **Genome JSON reader** in `rgv-core`/`rgv-hts`, accepting both `twoBitURL` and
  `fastaURL`/`indexURL` forms. Unknown properties are reported, not fatal.
- **2bit reference source**, local and over the existing `HttpRangeReader`, behind the same
  reference-sequence interface as FASTA. Also add bgzip FASTA with `.gzi`.
- **Genome list and menu.** Genomes → Select hosted genome, a toolbar dropdown of previously used
  genomes, and a configurable list URL. Fetch off the UI thread; cache the list, JSON and small
  resources on disk so a chosen genome works offline afterwards.
- **Genome resources.** Cytobands (`cytoBandIdeo.txt.gz`), the alias table (which gives automatic
  `1`/`chr1` resolution), chromosome order and sizes, and the default annotation track. The hg38
  default is `format: "refgene"` gzipped text over HTTP: add a download-and-cache path for small
  remote text tracks and map `refgene` onto the existing genePred reader.
- **Genome ids.** `hg38`-style ids resolve through the catalogue, including in IGV XML session
  import, which currently drops them.
- **Deferred within this workstream:** `hubs` (UCSC track hubs, GenArk) and GenBank.
- **Existing landmark bundles** (`docs/LANDMARK_BUNDLES.md`) are not staged into packages today.
  Decide whether they remain as the gene-symbol search index or are replaced by the catalogue's
  annotation track.

**Gate:** on a clean machine, choosing hg38 from the menu gives sequence, ideogram, genes and gene
search with no command-line flags; a BAM with `1`-style contigs displays against it; a second
launch works offline.

## 8. Workstream E — track-source boundary and scalable auxiliary tracks

Adding a track kind currently touches about 13 places across six crates through parallel closed
enums, and all auxiliary tracks share one worker thread (`crates/rgv-engine/src/track_engine.rs:526`).

- Introduce a `TrackSource` trait: `query(region, bases_per_pixel)` returns either records or
  binned summaries. Collapse the format enums into one registry (classify → open → boxed source).
- One worker per source, or a small pool keyed by source, so a slow file cannot block the others.
- **Tabix/CSI** for bgzipped BED, GFF, GTF and VCF. Remove the 2,000,000-row whole-file cap
  (`crates/rgv-hts/src/tracks.rs:158`) for indexed inputs; a full GENCODE GTF must open.
- **BigWig/BigBed zoom levels** when zoomed out; `query_bigwig` currently reads raw intervals and
  truncates at 250,000 (`crates/rgv-hts/src/tracks/bbi.rs:153-181`).
- A feature query over the record cap returns a density summary, not an error.
- Confirm whether GFF/GTF rows are assembled into transcripts; if not, add it.
- Move auxiliary layout off the UI thread, keyed by (snapshot generation, settings), with per-lane
  dirty flags in place of the single global `display_dirty`.
- Replace the per-kind `selected_*` fields with one `Selection { track, record }`.

**Gate:** a new text format is added by touching the registry, one reader and one layout mapping; a
3-million-row GTF opens in under a second and pans at frame rate.

## 9. Workstream F — desktop structure

Do these as the features above touch the affected code, not as a standalone rewrite.

- Split the roughly 170-field `Viewer` (`apps/rgv-desktop/src/lib.rs:1367-1539`) into owned
  sub-states (GPU, workspace, chrome, diagnostics) with explicit borrows in place of glob imports.
- Break up `render_shell_frame` (4,579 lines, `ui/frame.rs:279-4858`) and
  `rebuild_genomic_display_pass` (1,591 lines, `display/mod.rs:614`).
- One `Action` type dispatched by both UI and automation, so automation becomes a thin
  serialisation layer. This is the prerequisite for IGV batch compatibility in G.
- Give alignment lanes the same frame/height/hit-test/inspector interface as other lanes;
  `primary_alignment` appears 228 times in non-test code.
- Wake the event loop from workers with an `EventLoopProxy` instead of a 16 ms poll
  (`runtime.rs:390-406`).
- Put the four overlapping diagnostic systems (about 7.5k lines) behind one tracing facade and a
  feature flag; move `mark_display_dirty` out of `manual_diagnostic_instrumentation.rs`.

## 10. Workstream G — parity queue after the foundation

In priority order:

1. IGV batch-file parser and an opt-in port-60151 text listener, translating to `Action`.
2. VCF genotype rows on the track, allele-frequency colouring, display modes, unindexed small VCF.
3. Soft-clip display, linked supplementary alignments, quick consensus.
4. IGV XML session fidelity (data ranges, colours, display modes) and XML export.
5. Whole-genome view as a distinct mode over binned summaries from workstream E.
6. SVG export and alignment export from the UI (both exist in automation).
7. SAM, `.bam.list`, htsget, cloud authentication, track hubs, then BLAT, igvtools and Hi-C on
   demand.

## 11. Workstream H — small everyday fixes

Independent and small; do any time.

- Locus box accepts `chr1:12345`, bare `chr1`, and gene names inside multi-locus lists.
- Double-click to centre and zoom; drag on the ruler to zoom to a region; Ctrl-F/Ctrl-B for
  next/previous feature; End key.
- Check IGV's documented defaults for duplicate and QC-fail filtering and for "colour alignments
  by", then match them. These were asserted from memory in the assessment and not confirmed.
- Stage whatever default genome resources remain bundled into the release packages.

## 12. Verification policy

- Every gate above is an end-to-end behaviour measured headlessly on the release build; hands-on
  UI acceptance is the user's later pass.
- Performance gates run in CI against checked-in fixtures and fail the build on regression.
- A representative real-data set (Illumina WGS and exome, ONT, PacBio, RNA-seq, CRAM) is pinned
  before B begins and reused for every later gate.
- Plans record what was measured, not what was implemented.

## 13. Risks

- **B changes the product contract.** Downsampled-by-default must be obvious in the UI, or users
  will read a sampled stack as complete evidence.
- **Pair coherence under sampling** is best-effort with the hash scheme; view-as-pairs needs an
  explicit lone-mate state.
- **Hosted genomes depend on third-party servers** (igv.org, UCSC). Caching covers offline use
  after first fetch, not first run.
- **F can sprawl.** Limit each structural change to the code a scheduled feature already touches.

## 14. Not verified in the assessment

- Pass/fail of the workspace test suite on the baseline commit.
- Runtime behaviour of the desktop application; findings are from code and recorded benchmarks.
- How summary tiles are fed today (section B1), and GFF/GTF transcript assembly (section 8).
- Whether CI currently passes on Windows and Linux.

## 15. Progress log

All measurements are headless, release build, Apple Silicon, `benchmark-data/mixed.bam` at
`chr1:900001-920000` (156,152 reads; about 1,200x depth). Nothing here has had hands-on UI testing.

### 11 November 2026

**Baseline.** `cargo test --workspace --locked`: 1,283 passed, 0 failed, 1 ignored (network).
Strict Clippy (`--all-targets --all-features -D warnings`) fails on the baseline with about 850
pedantic findings across the workspace on Rust 1.93.1; none of the changes below adds to that count.
Clearing it is separate clean-up work. The two public BAMs referenced by earlier benchmark reports
are no longer present in `benchmark-data/large-public-alignments/`, so ACTB/BRCA1 were not re-run.

**Workstream B — landed**

- *Cause of slow cold load found and fixed.* Profiling showed 92% of first-useful time in
  `mark_directional_read_geometry` (`crates/rgv-alignment-scene/src/semantic_page.rs`), which
  rescanned the whole page for every read: quadratic in reads per page. It now touches only the
  primitives just emitted for that read. BAM decoding was about 20% of the remaining time, so the
  decode changes proposed in B3 (record reuse, direct columnar decode) are low value and are
  dropped from the near-term list.
- *Evidence selection.* `EvidenceRoot::select_impl` now sorts on precomputed keys and removes
  duplicate handles by adjacency instead of a hash set.
- *Read display modes (D1).* `ReadSampling` on the scene `LayoutContract`, applied when rows are
  selected (`row_oracle.rs`) and when an atlas is extended (`row_atlas.rs`). Persisted as
  `SessionFilters::read_sampling`; new tracks default to Downsampled 100 reads / 50 bp, projects
  saved without the field load as Complete. Downsampled tracks use the hard decode ceiling
  instead of the Complete read limit, so they are not refused. The track label reports
  `Downsampled: N of M reads shown`. A Read display control was added to alignment settings.

| Measure | Before | After (Complete) | After (Downsampled) |
| --- | ---: | ---: | ---: |
| Cold first useful frame | 1,998 ms | about 290 ms | about 265 ms |
| Rows | 1,210 | 1,210 | 320 |
| Scene bytes charged | 128 MiB | 128 MiB | 115 MiB |
| Camera update p95 | 0.03 ms | 0.03 ms | 0.03 ms |

**Workstream B — still open**

- Downsampling is applied at row layout, not at decode. Every eligible read is still decoded and
  held as evidence so that coverage, alleles and junctions stay exact, which is why memory fell
  only 10%. Bounding memory by the view needs summaries accumulated in the decode pass (B1,
  "Where sampling happens"); that is the next structural step.
- The 250 ms gate is narrowly missed at this 1,200x locus. The remaining time is spread across
  five separate record selections (summaries, atlas, pages) with no single hot spot.
- Visibility range (coverage-only beyond a span), long-read default, lone-mate state under
  view-as-pairs, B4 worker split and cancellation, and B5 ledger simplification are not started.
- At an extension boundary the sampled set can differ from a clean rebuild for reads that
  straddle the previous horizon edge; incumbent rows are never withdrawn.

**Workstream E — landed**

- *BigWig zoom levels.* Wide queries read the coarsest precomputed zoom level that still gives
  at least 4,096 bins across the query; dense raw queries that would hit the 250,000-record cap
  fall back to the finest zoom level instead of truncating. The inspector states when values are
  zoom-summary means (`crates/rgv-hts/src/tracks/bbi.rs`).
- *Tabix-indexed text annotations.* A bgzip-compressed BED/GFF3/GTF/genePred/peak/PSL file with
  an adjacent `.tbi` or `.csi` is read on demand per query with no whole-file load and no
  2,000,000-row cap; next/previous feature and name search work (search builds a slim index in
  one streaming pass on first use). Unindexed files behave as before, and the cap error now says
  how to index (`crates/rgv-hts/src/tracks/text_feature.rs`).

**Workstream H — landed**

- Locus box accepts a single position (`chr1:12345`, centred in a 41-base window) and a bare
  contig name (whole contig). Typed contig names resolve across `chr`/no-`chr` and `MT`/`chrM`
  without configured aliases. This applies to typed loci only; matching tracks whose contig
  naming differs from the reference still needs the genome alias table from workstream D.

**Workstream A — landed**

- README, `docs/FORMAT_SUPPORT.md`, `docs/USER_GUIDE.md` and the roadmap now state that local
  CRAM and remote alignments do not open on this branch and describe the two read-display modes.

**Workstream C — landed (automated coverage only)**

- *One backend for every alignment source.* `rgv-hts` gained a resumable positioned stream for
  the pure-Rust decoders (`crates/rgv-hts/src/remote_alignment/positioned_stream.rs`): remote BAM
  returns one BGZF block per checkpoint and CRAM one slice, with the same eligibility, admission
  and refusal rules as local BAM. `ProductionAlignmentSource` dispatches the stream, and the
  scene backend (`LocalBamCanonicalBackend`, name kept to limit churn) now owns one of those
  instead of a local BAM reader. Chunk identity comes from the records' physical positions, so
  CRAM slices use the `SourceChunkId::CramSlice` identity that already existed in the scene.
- *Desktop.* `spawn_alignment_track_controller` opens local BAM, local CRAM and HTTP/S BAM/CRAM
  through the scene; the two "no legacy fallback" refusals are gone. CRAM receives the loaded
  reference; BAM still does not.
- *Missing CRAM reference.* A reference-compressed CRAM opened without a reference used to panic
  inside the CRAM decoder (latent in the one-shot query too). It now fails with "load the
  matching reference FASTA to display it".
- *Remote BAM source is `Send`.* The remote BAM reader no longer wraps a boxed index trait
  object, so the source can be moved to its worker thread without `unsafe`.
- *Evidence.* Stream-versus-query parity tests for remote BAM (through the in-process HTTP
  fixture) and local CRAM; a scene-backend test admitting CRAM evidence; and the headless
  interaction benchmark run end to end: the CRAM fixture publishes 16 records in 8 rows, and
  `mixed.bam` served over a local HTTP range server publishes the same 156,152 records, 1,210
  rows and charged bytes as the local file (first useful frame about 620 ms against about 370 ms
  local in the same session).

- *Slice-bounded CRAM reference.* Each single-reference slice is decoded against only its own
  span of the reference (taken from the CRAI) instead of the whole chromosome, with one retry
  against the whole sequence if the index understated the span. This matters most with a remote
  or hosted reference, where the alternative was downloading a chromosome per view. A test
  checks every decoded record's CIGAR, bases and qualities against HTSlib's decoding of the same
  file and that fewer reference bases were read than the sequence holds.

**Workstream C — still open**

- A superseded remote request is not cancelled mid-transfer: the source worker is blocked in the
  HTTP read and only observes cancellation between checkpoints. The remote transfer budget is
  not connected either.
- Multi-reference CRAM slices (rare; at chromosome boundaries) still load each whole reference
  sequence they touch.
- A slow remote source stalls presentation building for its own track, because decode and
  presentation share one worker (B4).
- The legacy `AlignmentQuery` oracle path is still in the tree; it is now used only by tests and
  the parity checks above.
- No hands-on desktop testing; remote CRAM is covered only by the existing HTS-layer fixture, not
  through the scene.

**Workstream D — landed (automated coverage only)**

- *2bit reference reader* (`crates/rgv-hts/src/two_bit.rs`) behind the existing
  `FastaReference` interface, local or over HTTP range requests, both byte orders, 32- and 64-bit
  offsets. Only the header and name index are read at open; a sequence's unknown-base runs are
  read on first use and the soft-mask table is skipped without being downloaded. Bases are
  always upper case. Sequence lengths come from a `chrom.sizes` table when one is supplied
  (carried in the reference's existing companion-index slot, so sessions and launch options need
  no new setting), otherwise from each sequence record.
- *IGV genome JSON, hosted list and alias table parsers* (`crates/rgv-core/src/igv_genome.rs`).
  Unknown properties are reported, not fatal. Relative resource locations resolve against the
  description's own location.
- *Loader with an on-disk cache* (`crates/rgv-hts/src/igv_genome.rs`). It converts a description
  to the existing `GenomeConfiguration`: the reference stays remote; the description, chromosome
  sizes, cytobands (stored decompressed), alias table and text annotation tracks are downloaded
  once. `refgene`-style tracks are cached under a `.genePred` name so the existing reader is
  selected. The genome list is refreshed when the server is reachable and read from the cache
  when it is not.
- *Desktop.* Genomes → Hosted genomes lists IGV's catalogue with a filter box and loads the
  chosen genome off the UI thread. Genomes → Open genome configuration and drag-and-drop accept
  an IGV genome JSON. `--genome hg38` (or a JSON path/URL) works at launch. `.2bit` is accepted
  wherever a FASTA is.

- *IGV session import.* A session whose `genome` attribute is an id such as `hg38` now takes
  its reference, cytobands and aliases from the hosted genome (resolved off the UI thread for
  File → Open, at start-up for `--import-igv`); a failure is recorded in the import report.
- *Contig list.* The Genomes menu's contig list scrolls instead of growing with the assembly.

**Workstream D — still open**

- Not verified against the live IGV hosting: outbound network is blocked in the development
  sandbox. An opt-in test does it:
  `cargo test -p rgv-hts -- --ignored hosted_hg38_opens_from_the_igv_genome_list --nocapture`.
- `chromosomeOrder` is parsed but not applied (contigs keep the reference's own order), and the
  Genomes menu still lists every contig as a button, which is unwieldy for assemblies with
  hundreds of sequences.
- The default annotation tracks of a genome resolved for an IGV session are not added. A
  recently-used genome list, a configurable list URL in settings (today: the `RGV_GENOME_LIST_URL` environment variable), bgzip FASTA with `.gzi`,
  `hubs`, and GenBank.
- Variant and other indexed default tracks with a non-conventional index URL are skipped with a
  notice.

**Workstream H — landed (second pass)**

- Ctrl/Cmd-F and Ctrl/Cmd-B jump to the next and previous annotation feature, as in IGV.

**Workstream E — landed (second pass)**

- *Remote text annotations.* A BED, GFF3, GTF, genePred, refFlat, peak or PSL URL is downloaded
  whole (up to 256 MiB) on the track worker and read like a local file; the server is asked
  first and the previous copy is used when it cannot be reached. Authenticated servers are not
  supported on this path.

**Workstream B — decision on decode-time sampling**

Investigated and deliberately not attempted blind. Exact summaries are six tile families
(`ExactSummaryRoot`: depth, alleles, junctions, pairs, filters, modifications) derived from
retained evidence under a summary contract, and the evidence store's completeness certificates
assume every eligible read is held. Sampling at decode therefore means making summaries a
decode-time product keyed by the summary contract (re-decoding when that contract changes) and
introducing an explicitly sampled evidence authority through the store and coordinator. That is
the largest change in this plan and needs hands-on testing alongside it. Until then Downsampled
mode bounds rows, not memory: with a 48 MiB budget the 1,200x test locus still fails with
allocation pressure, exactly as Complete mode does.

### 12 November 2026 — structural pass

Headless verification only, as before. Measurements: release build, `benchmark-data/mixed.bam`
at `chr1:900001-920000`.

**B4 — decode split from presentation (landed)**

- The indexed reader now lives on its own `rgv-alignment-decode` thread per source
  (`DecoderThread` in `crates/rgv-engine/src/local_bam_source.rs`). The scene worker receives
  finished BGZF blocks / CRAM slices through a two-unit hand-over queue and never blocks on file
  or network I/O. A new `CanonicalDecodeCheckpoint::Pending` tells the actor nothing is ready;
  the worker then builds presentation, or listens for requests for 1 ms.
- Cancellation is real: each stream has a generation, `keep_going` is no longer hard-wired to
  `true`, and a superseded target yields at once (`active_is_superseded`) instead of waiting for
  the unit in flight. For remote sources the same generation check is installed as the HTTP
  reader's cancellation hook, so a transfer is abandoned mid-download.
- Each remote acquisition has a transfer allowance (20,000 requests, 2 GiB); exceeding it is an
  error on that track rather than an unbounded download.
- Synchronous drivers (tests, tools) keep blocking checkpoints; only the threaded engine opts in
  to `with_nonblocking_checkpoints`.
- Cold first useful frame: about 250 ms (from about 290 ms), which meets the 250 ms gate at this
  locus. Rows, records and charged bytes are unchanged.

**B5 — resource ledger (measured; kept)**

Sampling the cold load shows no time in `AllocationLedger` at all (0 of roughly 150 active
samples; the top frames are sorting, hashing and row assignment). The ledger is therefore not a
performance cost at this depth and is left as it is. Replacing it with per-store counters only
makes sense together with decode-time sampling, when retained bytes stop tracking depth.

**B1 — decode-time sampling (not done; design narrowed)**

The benchmark now reports retained bytes by class. At 1,200x (156,152 reads):

| Class | Complete | Downsampled |
| --- | ---: | ---: |
| Payload sidecar (bases, qualities, tags) | 63.2 MiB | 63.2 MiB |
| Core chunks | 28.6 MiB | 28.6 MiB |
| Row atlas | 17.9 MiB | 5.1 MiB |
| Semantic pages | 17.8 MiB | 17.6 MiB |
| Exact summaries | 0.7 MiB | 0.7 MiB |

So the cost that still scales with depth in Downsampled mode is evidence, and two thirds of it
is payload. There is no existing path that yields exact coverage without retained evidence (a
refused dense region publishes no summaries either), so dropping reads at decode needs new tile
builders. The narrowest sound design found: keep core columns for every read (depth, junction,
pair and filter tiles stay exact unchanged), keep payload only for reads the sampler keeps, and
carry a per-chunk pre-aggregated pileup of the dropped reads for the allele and modification
tiles, keyed by the allele contract and re-decoded when that contract changes. That would cut
retained evidence roughly in half at this depth. It was not attempted without hands-on testing:
it changes the allele tile builder, the payload sidecar and the store's payload-satisfaction
rules, and a read shown without its payload at an extension boundary would be a visible error.

**E — track sources (landed)**

- `TrackSource` trait (`crates/rgv-engine/src/track_source.rs`) implemented by all ten source
  kinds; the engine holds `Box<dyn TrackSource>` and the three ten-arm matches (query, search,
  next/previous feature) are gone.
- A viewport query runs up to eight tracks at once on scoped threads, so one slow file no longer
  delays the lanes behind it. TDF channels of one archive share a mutex-guarded reader.
- Feature layout stays on the UI thread by measurement: 0.16 ms for 500 features, 0.19 ms for
  5,000 and 1.5 ms for 50,000 (`feature_layout_cost` probe in `rgv-layout`).

**F — desktop structure (partly landed)**

- `render_shell_frame` and `rebuild_genomic_display_pass` were split into named pieces in
  sibling modules (see the notes below for what was left whole).
- Workers wake the event loop through an `EventLoopProxy` (`rgv_engine::set_ui_waker`) when they
  publish. The 16 ms poll remains only for tasks observed by completion; with engines alone the
  fallback is 50 ms.
- Not done: splitting the `Viewer` struct into owned sub-states, the single `Action` type shared
  by UI and automation, and the diagnostics facade.

**Legacy removal (landed)**

- Deleted the unused `Scheduler` and the weighted tile cache and its keys from `rgv-engine`
  (and its `lru` dependency); removed the last `AlignmentQuery` use from the desktop crate.
- The one-shot `query` in `rgv-hts` is kept deliberately: it is the independent oracle the
  parity tests, the validation runner and two benchmarks check the scene path against.

**Not started:** workstream G; the transcript assembly check in E.
