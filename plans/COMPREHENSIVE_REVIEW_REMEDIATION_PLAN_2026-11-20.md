# Comprehensive review remediation plan — 2026-11-20

Source: `audits/COMPREHENSIVE_REVIEW_2026-11-20.md`. Finding IDs below refer to that file.

## Principles

1. Fix defects before restructuring; restructure before adding features.
2. Each wave must leave `cargo check --workspace --all-targets` clean and the touched crates'
   tests passing. No wave may add clippy warnings.
3. A finding marked [U] is reproduced with a test (or disproved and struck) before it is "fixed".
4. Every behavioural fix lands with a regression test in the same crate.
5. File ownership per workstream is disjoint within a wave, so workstreams can run in parallel in
   one working tree. Public signatures that cross crates change only in the waves that say so.
6. Preserve the items listed under "Preserve" in the audit.

Status key: `[ ]` open, `[x]` done and verified, `[~]` partly done (note says what remains),
`[-]` struck (not reproducible / not a defect).

## Wave 1 — discrete defects (no cross-crate API change)

Small, local, independently testable. Six parallel workstreams by file ownership.

### W1-A `crates/rgv-core`, `crates/rgv-layout`
- [ ] CORE-1 tolerant IGV locus parse (All, bare contig, gene, multi-locus → default + notice;
      last-colon split); URL schemes not treated as local paths
- [ ] CORE-2 filter interactions to contig/viewport before the cap
- [ ] CORE-3 interaction arcs via `RectShape::JunctionArcUp`, clipped
- [ ] CORE-4 stable feature row packing (sort, pixel padding, overflow indicator)
- [ ] CORE-5 label named features with no `feature_type`
- [ ] CORE-6 ruler ticks on one-based round numbers
- [ ] CORE-7 legacy group migration repairs non-contiguous groups; remove dead checks
- [ ] CORE-8 `ReferenceDictionary` custom deserialize, index rebuild, duplicate rejection, no
      linear fallback
- [ ] CORE-9 writer version + "written by a newer RGV" error
- [ ] CORE-10 redact only credential-bearing query parameters
- [ ] CORE-11 collision-free object ids
- [ ] CORE-12 variants use clipped geometry
- [ ] CORE-13 edge codons and reverse-frame phase
- [ ] CORE-14 validated deserialize for `GenomicInterval`/`Viewport`; track cap; `:` in aliases
- [ ] CORE-15 raw quantitative truncation covers the viewport
- [ ] CORE-16 (part) canonical `Locus` parser in core; sorted landmark search; >2^32 tests
- [ ] APP-8 / OPS-9 (core side) symlink-resolving containment in `automation.rs`
- [ ] APP-11 (core side) atomic settings write
- [ ] APP-2 (core side) navigation history carries contig name

### W1-B `crates/rgv-hts`
- [ ] HTS-1 empty MM groups, `MN`/hard-clip guard
- [ ] HTS-3 streaming remote query honouring `max_records`/filters/`keep_going`; lazy CRAM
      intersect
- [ ] HTS-4 skip sequence decode when alleles are not collected; single CIGAR view per record
- [ ] HTS-6 retry body-read failures, jittered backoff, `Retry-After`, shared client
- [ ] HTS-7 strong ETags only in `If-Range`
- [ ] HTS-9 tolerant BED/GFF parsing (whitespace, thick 0/0, `##FASTA`, percent-decoding,
      skip-and-count bad lines)
- [ ] HTS-10 remote config (auth, cancellation, budgets) for bbi/TDF/text tracks
- [ ] HTS-11 transient slice errors do not escalate to whole-contig fetch
- [ ] HTS-13 index overview and `.fai` duplicate check without quadratic passes
- [ ] HTS-14 2bit allocation bounds; fuzz target writes an index
- [ ] SCENE-6 pileup mate-overlap masking respects left-mate CIGAR/BQ and drains masks
- [ ] HTS-5 `libdeflate` enabled for the app's noodles-bgzf

### W1-C `crates/rgv-alignment-scene`
- [ ] SCENE-1 MM scratch bounded by call count
- [ ] SCENE-2 primitive/depth/modification reservations clipped to the tile
- [ ] SCENE-5 prefer the current/largest containing atlas
- [ ] SCENE-8 eviction skips pinned entries
- [ ] SCENE-11 junctions starting left of the prepared interval
- [ ] SCENE-12 keyed cache lookups; single overlap index build
- [ ] SCENE-13 pair-consistent sampling; stale HashSet charge
- [ ] SCENE-14 seeded property harness (extension vs clean build, seam sums, reservation ≥ actual)

### W1-D `crates/rgv-engine`
- [ ] ENG-1 foreground presentation pressure/error latch
- [ ] ENG-3 reclaim adopts newest request id, emits `Superseded`, errors carry the id
- [ ] ENG-4 per-owner scene-build failure
- [ ] ENG-5 per-owner speculation slots
- [ ] ENG-6 explicit worker priority order
- [ ] ENG-8 `catch_unwind` at thread roots; decoder respawn
- [ ] ENG-9 wake instead of polling
- [ ] ENG-10 cancellable search, persistent query pool
- [ ] ENG-11 `CoalescingReceiver` `Drop`
- [ ] ENG-13 bounded reference slices
- [ ] ENG-14 single `satisfies` predicate (reproduce first)

### W1-E `crates/rgv-render`, `apps/rgv-desktop/src/app/viewer_runtime.rs`
- [ ] REND-1 categorical palettes per colour mode; golden-ratio hashing for open categories
- [ ] REND-2 device-lost / uncaptured-error handling
- [ ] REND-3 adapter fallback, downlevel limits, `WGPU_BACKEND`
- [ ] REND-4 explicit surface format policy
- [ ] REND-5 one-physical-pixel minimum for all shapes (shader + CPU projector)
- [ ] REND-6 junction arcs parameterised over the unclamped span
- [ ] REND-8 exact-size immutable page buffers
- [ ] REND-10 one shared base palette
- [ ] REND-12 scissor clamp at render time
- [ ] REND-14 surface rectangle overflow to the user
- [ ] REND-9 (part) export scale parameter

### W1-F `apps/rgv-desktop` (except `viewer_runtime.rs`), `apps/rgv-mcp`
- [ ] UI-1 Escape never exits
- [ ] APP-1 save/discard/cancel gate for exit, open, new, recover
- [ ] UI-2 pinch, horizontal pan, scale-correct pixel deltas
- [ ] UI-3 history and recent loci committed at gesture end
- [ ] APP-2 name-based history (desktop side)
- [ ] UI-6 locus parsing via the core parser; variant IDs; whitespace triples; case-insensitive
- [ ] UI-7 single locus retargets the active panel only
- [ ] UI-8 hover uses the unobstructed hit test
- [ ] UI-9 stable auxiliary selection identity
- [ ] UI-10 dialogs coexist
- [ ] UI-4 per-frame clones gated/borrowed; UI-5 virtualised lists
- [ ] UI-11, UI-12 small items
- [ ] APP-3 landmark/cytoband I/O off the event loop
- [ ] APP-4 autosave on dirty only, instance-scoped prefix
- [ ] APP-5 per-pid run markers
- [ ] APP-6 session open not rejected by background revisions
- [ ] APP-7 typed notification log
- [ ] APP-9 `/v1/status` Host check; APP-10 batch cancellation and wait deadlines
- [ ] APP-12 idle polling; APP-13 drop handler; APP-14 monotonic TrackId
- [ ] OPS-5 MCP error pass-through; OPS-6 schema descriptions; OPS-7 capture downscale and slim
      snapshot; OPS-12 temp PNG leak; OPS-13 schema range; OPS-9 docs

### W1-G repository and build (no Rust source)
- [ ] OPS-1 `rust-toolchain.toml`, per-OS plain build job separate from lint
- [ ] OPS-3 fuzz job `continue-on-error` per target
- [ ] OPS-10 (part) benchmark-only backends out of default members
- [ ] OPS-11 `[profile.release]`
- [ ] OPS-14 `launch.rgv.sh`, `test.rgv.sh`, deny wildcard
- [ ] DOC-1..DOC-8 documentation corrections

## Wave 2 — structural changes inside existing crates

Run after Wave 1 is green. Each item is one reviewable change.

- [ ] SCENE-3 page keys without evidence/atlas revisions (page reuse across extension)
- [ ] SCENE-4 / SCENE-7 incremental row atlas extension; stable rows under sort/group
- [ ] ARCH-3 charge-actual-after-build with per-owner soft budgets and LRU; exact ledger only in
      Complete mode; GPU pages outside the ledger
- [ ] SCENE-9 / REND-8 compact handles, primitives and 16-byte GPU instances with a palette
      uniform (unlocks REND-10 theming and PROD-17)
- [ ] SCENE-10 fused single-pass summary tiles; generic `SummaryTile<Family>`
- [ ] ENG-2 decoder stream continuation and chunk-level resume
- [ ] ENG-7 resumable/cancellable scene builds
- [ ] ENG-15 coordinator split into modules; pressure classification on error types
- [ ] REND-16 generic `PagedGpuCache`; shared WGSL; GPU readback goldens (covers REND-7)
- [ ] REND-9 GPU offscreen export at Nx; vector export from semantic primitives
- [ ] HTS-2 remote VCF/BCF over `HttpRangeReader`; drop htslib `curl`
- [ ] HTS-8 index-chunk-driven read-ahead; HTS-12 shared decompression pool
- [ ] HTS-15 / OPS-10 converge on one alignment backend
- [ ] UI-13 / APP-16 split `ui/tracks.rs`, remove glob preludes, diagnostics behind a facade
- [ ] OPS-6 derive MCP schemas from core types

## Wave 3 — architecture

- [ ] ARCH-1 decode-time summary tiles and a streamed/persisted coverage pyramid
- [ ] ARCH-2 / ARCH-7 headless `rgv-session` crate (workspace state, `Action` reducer, engines,
      scene publication); desktop, CLI snapshot, MCP and automation drive it
- [ ] ARCH-4 explicit `(PanelId, TrackId)` throughout
- [ ] ARCH-5 public track contract `query(region, bp_per_px, cancel) → world-coordinate tiles`;
      one paged world-space renderer
- [ ] ARCH-6 shared I/O and decode pool keyed by visibility; lift the 64-track cap (PROD-9)
- [ ] ARCH-8 `rgv-model` crate split from `rgv-hts`; automation/stress out of `rgv-core`
- [ ] ARCH-9 reference bases delivered engine-to-engine

## Wave 4 — product reach

Ordered by adoption impact. Each needs its own design note.

- [ ] PROD-1 / OPS-17 tagged signed releases, bioconda, Homebrew, old-glibc AppImage
- [ ] OPS-2 / PROD-2 Windows and Linux qualification; real-data CRAM/remote matrix
- [ ] OPS-4 reproducible end-to-end IGV comparison on public data
- [ ] PROD-4 / HTS-16 cloud access (S3, GCS, htsget, OAuth), remote tabix, persistent cache
- [ ] PROD-3 / PROD-12 genotype matrix, phased display, trio view
- [ ] PROD-5 whole-genome view (needs ARCH-1)
- [ ] PROD-7 / PROD-8 IGV batch and session fidelity
- [ ] PROD-10, PROD-6, PROD-11, PROD-13, PROD-14, PROD-15
- [ ] OPS-8 agent evidence tools
- [ ] UI-14 undo/redo, fuzzy palette, per-panel locus, theming (PROD-17), keybindings
- [ ] PROD-16, PROD-18..PROD-21

## Verification per wave

1. `cargo check --workspace --all-targets`
2. `cargo test -p <each touched crate>`
3. `cargo clippy --workspace --all-targets` warning count not above the 822 baseline
4. Update the status boxes above with what was verified and how.
