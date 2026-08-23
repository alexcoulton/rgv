# RGV Implementation Plan

RGV will be a native Rust desktop genome viewer designed to outperform IGV Desktop in time to first useful frame, navigation responsiveness, deep-coverage behaviour, memory use, and remote-data efficiency.

The first implementation target is a focused vertical slice: open an indexed local BAM plus FASTA, navigate to a locus, and render coverage and alignments smoothly on the GPU. The repository currently contains the alignment I/O benchmark but no application scaffold, so this slice comes before broader format support.

The benchmark evidence supports `rust-htslib` as the initial alignment backend, with `noodles` retained for differential testing and potentially remote I/O later. See [`benchmark-results/RESULTS.md`](benchmark-results/RESULTS.md) and [`benchmarks/alignment-io/README.md`](benchmarks/alignment-io/README.md).

## 1. Product target

The first serious release should support:

- Native macOS, Linux, and Windows applications.
- Local BAM with BAI or CSI.
- Local CRAM with CRAI and a reference.
- Illumina, ONT, PacBio, RNA-seq, amplicon, single-cell, and SV-oriented alignments.
- Coverage, packed reads, CIGAR operations, mismatches, bases, and base qualities.
- Secondary, supplementary, paired, split, and spliced alignments.
- Arbitrary SAM auxiliary tags, including `MM`/`ML`.
- Filtering, colouring, grouping, and sorting.
- Remote HTTP/S access before beta.
- Reference, annotation, variant, and quantitative tracks before v1.

“Faster than IGV” should mean measurable improvements in:

- Time to first useful frame.
- Responsiveness while panning and zooming.
- Deep-coverage behaviour.
- Memory use.
- Remote bytes transferred.
- Large-session responsiveness.

## 2. Core architecture

```text
Window events
     │
     ▼
Viewport + generation number
     │
     ▼
Request scheduler ───────► tile caches
     │                         │
     ▼                         │
HTS workers                    │
     │                         │
     ▼                         │
Alignment batches             │
     │                         │
     ▼                         │
LOD aggregation + layout ◄────┘
     │
     ▼
Immutable display tiles
     │
     ▼
wgpu renderer
```

Every viewport change receives a monotonically increasing generation number. Work from an older generation may finish, but it must never replace newer data on screen.

### Initial workspace

Start with five production crates rather than creating a crate for every concept:

```text
apps/
  rgv-desktop/       Native executable and event loop

crates/
  rgv-core/          Coordinates, viewport, track contracts, IDs, errors
  rgv-hts/           BAM/CRAM/reference readers and normalization
  rgv-engine/        Scheduling, cancellation, caches, and LOD selection
  rgv-layout/        Coverage, read packing, and display-list generation
  rgv-render/        wgpu pipelines, GPU buffers, text, and picking

benchmarks/
  alignment-io/      Existing backend benchmark
  interaction/       End-to-end navigation benchmark
```

UI-specific code can remain in `rgv-desktop` until it becomes large enough to justify a separate crate.

### Technology choices

- Windowing and input: `winit`.
- Rendering: `wgpu`.
- Application controls: `egui`, `egui-winit`, and `egui-wgpu`.
- BAM/CRAM: `rust-htslib`.
- CPU parallel work: `rayon` or explicitly sized worker pools.
- Async orchestration: bounded channels; introduce Tokio only where remote I/O requires it.
- Logging and instrumentation: `tracing`.
- Serialization: `serde`.
- Text in the genomic canvas: `glyphon` or `cosmic-text`.
- Benchmarks: `criterion` plus a standalone interaction runner.

## 3. Threading and ownership

The UI/render thread must never read a file, decode BAM records, or calculate coverage.

- The main thread owns `winit`, `wgpu`, and `egui`.
- HTS workers each own their readers; readers are not shared through mutexes.
- A bounded, shared HTSlib decompression pool is used across tracks.
- CPU workers perform normalization, coverage, packing, and filtering.
- Results are immutable batches sent to the renderer.
- Queues are bounded so obsolete navigation cannot produce an unlimited backlog.
- Large queries are processed in chunks, checking cancellation between chunks.
- A frame upload budget prevents a large result from freezing the interface.

## 4. Internal genomic model

Use validated, zero-based, half-open intervals everywhere internally:

```rust
struct GenomicInterval {
    contig: ContigId,
    start: u64,
    end: u64,
}
```

Conversions to SAM’s one-based coordinates or HTSlib’s signed coordinates happen only at backend boundaries and must be checked.

The normalized alignment representation must preserve information rather than assuming Illumina-style reads:

- Reference start and end.
- Mapping quality and all SAM flags.
- Mate coordinates and template length.
- Read name.
- Complete CIGAR, distinguishing `M`, `=`, `X`, `I`, `D`, `N`, `S`, `H`, and `P`.
- Sequence and quality, when present.
- Secondary and supplementary state.
- Arbitrary auxiliary tags.
- Long-CIGAR representation.
- Modified-base tags.
- Missing sequence, quality, mate, or tags as valid states.

For efficiency, use a columnar `AlignmentBatch`:

- Fixed-width fields in contiguous vectors.
- Packed arenas for read names, CIGAR, sequence, quality, and tags.
- Lazy tag decoding.
- Lightweight records at distant zoom levels.
- Full sequence and tag materialization only when the detailed view or inspector requires it.

## 5. Level-of-detail system

The engine should select data and graphics from bases per pixel and a render-primitive budget, not hard-coded biological zoom levels.

| View | Display |
| --- | --- |
| Chromosome/overview | Binned coverage or density |
| Regional | Coverage, simplified alignment spans, junctions, and discordance |
| Read | Packed reads, CIGAR geometry, strand, and mapping-quality styling |
| Base | Bases, qualities, mismatches, insertions, and modified bases |

Deep coverage requires deterministic sampling:

- Coverage is calculated from all records where feasible.
- Only a bounded number of individual reads are rendered.
- Sampling uses fixed 50-base genomic windows with deterministic per-window ranking, so reads in an
  existing window are not displaced merely because neighboring regions enter the viewport.
- The UI clearly reports when alignments are sampled.
- Users can raise the limit, with an explicit memory/performance warning.

## 6. Caching strategy

Use separate caches because styling changes should not reread the BAM:

1. Remote/compressed-block cache.
2. Decoded alignment-batch cache.
3. Coverage and summary cache.
4. Layout/display-list cache.
5. GPU buffer cache.

Cache keys include:

- Source fingerprint.
- Contig and interval.
- LOD.
- Filter hash.
- Layout parameters.
- Style/theme version where relevant.

The default total cache budget should initially be around 512 MiB and configurable. Eviction must be observable through metrics. Adjacent tiles should only be prefetched when navigation direction is stable.

## 7. Rendering design

The genomic canvas should use custom GPU rendering, not thousands of UI widgets.

Use instanced pipelines for:

- Alignment rectangles.
- CIGAR segments.
- Coverage bars.
- Insertions and deletion markers.
- Splice junction lines or arcs.
- Base and mismatch glyphs.
- Selections and hover overlays.

Important constraints:

- No draw call per read.
- CPU culling before GPU upload.
- Scissoring per track.
- Reusable, amortized GPU buffers.
- High-DPI coordinates handled consistently.
- Picking based on compact object IDs or CPU interval indexes.
- Off-screen rendering for PNG export and screenshot tests.

## 8. Implementation milestones

### Phase 0 — Foundation

Deliver:

- Workspace and crate scaffold.
- Architecture decision records.
- macOS, Linux, and Windows CI.
- Formatting, Clippy, and test gates.
- Dependency auditing and license reporting.
- Structured logging and performance instrumentation.
- A native window with GPU initialization and error handling.

Exit criterion: the blank application builds and starts on all three platforms.

### Phase 1 — Synthetic GPU viewer

Deliver:

- Genomic viewport and coordinate transforms.
- Cursor-centred zooming.
- Mouse/trackpad panning.
- Track clipping and vertical scrolling.
- Synthetic coverage and read tracks.
- Instanced rendering and text.
- Frame-time and primitive-count overlay.

Exit criterion: sustained 60 FPS navigation over a synthetic high-density track on reference hardware without growing memory use.

### Phase 2 — Local BAM vertical slice

Deliver:

- Open BAM, BAI/CSI, FASTA, and FAI.
- Reference dictionary validation.
- HTS worker pool and shared decompression pool.
- Viewport generations and stale-result rejection.
- Alignment normalization.
- Coverage generation.
- Packed read rendering.
- Basic CIGAR styling, strand, and mapping quality.
- Missing-index and malformed-file errors.

Exit criterion: the existing synthetic corpus opens, all target loci render, and rapid navigation never displays stale results.

### Phase 3 — Universal BAM semantics

Deliver:

- Every CIGAR operation.
- Spliced RNA alignments.
- Secondary and supplementary alignments.
- Pair and mate relationships.
- Long reads and long CIGARs.
- Missing sequence and quality.
- Arbitrary tag inspection.
- `MM`/`ML` modified-base parsing.
- Hard/soft clipping and padding.
- Deterministic deep-coverage sampling.
- Filtering by flag, mapping quality, read group, and tags.
- Colour/group by strand, `RG`, `HP`, tag, or mapping quality.
- Read detail inspector exposing the original SAM fields and tags.

Exit criterion: the full fixture matrix passes visual, structural, and differential tests with no data type silently discarded.

### Phase 4 — Performance hardening

Deliver:

- Multi-level tile cache.
- Columnar alignment batches.
- Allocation and copy reduction.
- Prefetch tuned to navigation velocity.
- Incremental display-list uploads.
- Dedicated performance benchmark machine or stable runner.
- Automated comparisons against IGV Desktop.

Exit criterion: the agreed latency, frame-time, and memory budgets pass consistently.

### Phase 5 — CRAM and essential non-alignment tracks

Deliver:

- CRAM/CRAI and reference resolution.
- FASTA reference track.
- BED and BED-like feature tracks.
- GFF3/GTF gene models.
- VCF/BCF variants.
- BigWig quantitative tracks.
- BigBed annotations.
- Track reordering, resizing, visibility, and configuration.

Exit criterion: a typical analysis session can combine alignments, variants, genes, and quantitative data.

### Phase 6 — Remote data

Deliver:

- HTTP/S range access.
- Remote indexes and references.
- Disk-backed range cache.
- Request coalescing.
- ETag and last-modified validation.
- Authentication abstraction.
- Latency, loss, and bandwidth simulation tests.
- Credential redaction from logs and sessions.

The benchmark showed that HTSlib was fast remotely but fetched considerably more data than the custom `noodles` path. This phase should include a decision spike between:

- HTSlib/libcurl.
- A custom HTSlib I/O adapter.
- A `noodles` remote backend behind the same `AlignmentSource` interface.

Exit criterion: remote behaviour meets both latency and transferred-byte budgets under simulated WAN conditions.

### Phase 7 — Desktop product features

Deliver:

- Native file dialogs and drag-and-drop.
- Recent files and loci.
- JSON session files.
- Partial IGV session import.
- Image export.
- Keyboard shortcuts and command search.
- Accessible labels and navigation.
- Settings and cache management.
- Signed installers and update strategy.
- Opt-in diagnostics only.

### Phase 8 — Beta and v1

Deliver:

- Large real-world validation corpus.
- Reproducible benchmark report against IGV.
- Crash and corruption testing.
- User documentation.
- Stable session schema.
- Compatibility and migration policy.
- Release packaging for all supported platforms.

## 9. Performance budgets

These should initially be provisional, then locked to named reference hardware:

- No synchronous storage or network I/O on the UI thread.
- Cached interaction frame time: p95 below 16.7 ms.
- Typical main-thread event handling: below 2 ms.
- First useful local frame: below 250 ms for an indexed warm-cache locus.
- Detailed local view settled: below 500 ms.
- Cached pan should require no new record decoding.
- Old requests should stop producing useful work within one processing chunk.
- No unbounded queue or cache.
- Default cache budget: approximately 512 MiB.
- Deep loci must remain interactive regardless of read depth.
- No stale generation may overwrite the active viewport.

The comparison suite should run identical scripted loci in RGV and IGV and record:

- Startup time.
- Time to first useful frame.
- Pan/zoom p50 and p95 latency.
- CPU and resident memory.
- Deep-locus settle time.
- Remote requests, bytes, and wall time.

## 10. Verification strategy

- Unit tests: coordinates, clipping, CIGAR interpretation, packing, LOD, and cache keys.
- Property tests: interval conversion, CIGAR consumption, and packing invariants.
- Fuzzing: BAM-derived normalized records, CIGAR, tags, `MM`/`ML`, filters, and malformed input.
- Differential tests: compare `rust-htslib` and `noodles` digests over the same regions.
- Golden tests: display-list output independent of GPU backend.
- Screenshot tests: a small stable set with tolerance for platform text differences.
- Integration tests: rapid navigation, reopening, missing index, corrupt record, empty region, and reference mismatch.
- Performance tests: I/O, coverage, packing, display-list construction, GPU upload, and complete navigation.
- Fixture matrix: Illumina paired-end, ONT, PacBio HiFi/CLR, RNA-seq, amplicon, bisulfite, single-cell, high-depth, and SV-heavy BAMs.

## 11. First ten implementation PRs

1. Add architecture records and scaffold the five production crates.
2. Implement genomic coordinates, contig dictionaries, and viewport transforms.
3. Create the `winit`/`wgpu`/`egui` application shell.
4. Add the instanced synthetic coverage and alignment renderer.
5. Implement the generation-based scheduler and bounded channels.
6. Add local `rust-htslib` open, header, and indexed-query support.
7. Implement normalized columnar alignment batches.
8. Add tiled coverage calculation and LOD selection.
9. Add read packing and complete basic CIGAR rendering.
10. Add FASTA bases, mismatches, selection, and the read inspector.

At PR 10, RGV should have its first genuinely usable prototype. This is the right point for an architectural and performance review before expanding into additional formats.

## 12. Expected schedule

For one experienced full-time Rust engineer:

- Navigable GPU prototype: 3–5 weeks.
- Useful local BAM vertical slice: 6–10 weeks.
- Credible alignment-focused alpha: 3–4 months.
- Cross-platform beta with remote data and essential tracks: 6–9 months.
- Broad IGV-class v1: roughly 12–18 months.

## 13. Current execution status

Status as of 21 August 2026:

- Phases 0–3 and 5 have their planned production features implemented and pass the local locked
  workspace suite. Cross-platform runtime exit criteria remain subject to CI and native smoke runs.
- Phase 4 now has a bounded cache hierarchy, production-engine interaction gate, directional
  prefetch, incremental GPU uploads, and one authoritative columnar alignment representation with
  packed name, CIGAR, sequence, quality, and auxiliary-tag arenas. Full records are materialized
  only for inspection and selected tags are decoded on demand. Alignment and auxiliary-track
  actors use bounded output mailboxes that preserve control/error events while atomically
  coalescing unread viewport results to the newest generation; shutdown explicitly unblocks full
  result queues before joining workers.
- Phase 6 has HTTP/S sources, a bounded validator-aware range cache, coalescing, retry simulation,
  authentication types, and credential redaction. Remote BAM uses the pure-Rust range backend,
  accepts explicit BAI/CSI URLs and bearer credentials, exposes live I/O metrics, and passes an
  end-to-end deep-locus request/byte/wall-time gate. Remote CRAM still uses HTSlib and needs the
  equivalent observable qualification path.
- Phase 7 application features and packaging definitions are present. Signed/notarized artifacts
  have not yet been qualified with protected platform credentials or clean-machine installs.
- Phase 8 remains active. Completion requires the pinned real-world technology matrix, bounded live
  fuzz campaigns, native frame/memory/crash qualification, matched raw IGV telemetry on the named
  benchmark runner, and signed release artifacts for macOS, Linux, and Windows.

The next internal implementation target is observable remote CRAM/reference qualification and
native frame, memory, and crash resilience. External release qualification follows once the real
corpus, IGV installation, benchmark runner, and signing credentials are available.
