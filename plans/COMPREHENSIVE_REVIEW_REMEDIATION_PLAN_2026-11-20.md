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

Status at 2026-11-21: Wave 1 and its cross-crate adoption are implemented. Verification is
unit and integration tests only; nothing has been checked in the running application, and the CI
workflow changes have not run on GitHub.


Small, local, independently testable. Six parallel workstreams by file ownership.

### W1-A `crates/rgv-core`, `crates/rgv-layout`
- [x] CORE-1 tolerant IGV locus parse (All, bare contig, gene, multi-locus → default + notice;
      last-colon split); URL schemes not treated as local paths
- [x] CORE-2 filter interactions to contig/viewport before the cap
- [x] CORE-3 interaction arcs via `RectShape::JunctionArcUp`, clipped
- [x] CORE-4 stable feature row packing (sort, pixel padding, overflow indicator)
- [x] CORE-5 label named features with no `feature_type`
- [x] CORE-6 ruler ticks on one-based round numbers
- [x] CORE-7 legacy group migration repairs non-contiguous groups; remove dead checks
- [x] CORE-8 `ReferenceDictionary` custom deserialize, index rebuild, duplicate rejection, no
      linear fallback
- [x] CORE-9 writer version + "written by a newer RGV" error
- [x] CORE-10 redact only credential-bearing query parameters
- [x] CORE-11 collision-free object ids
- [~] CORE-12 variants use clipped geometry
      — clipping done; row packing for overlapping variants remains
- [x] CORE-13 edge codons and reverse-frame phase
- [x] CORE-14 validated deserialize for `GenomicInterval`/`Viewport`; track cap; `:` in aliases
      — session track cap chosen as 4096
- [x] CORE-15 raw quantitative truncation covers the viewport
- [x] CORE-16 (part) canonical `Locus` parser in core; sorted landmark search; >2^32 tests
      — dictionary-scoped contig identity and multi-region view model move to Wave 3
- [x] APP-8 / OPS-9 (core side) symlink-resolving containment in `automation.rs`
      — control-opened sessions and URLs also restricted; `--control-allow-private-urls`
- [x] APP-11 (core side) atomic settings write
      — desktop uses `read_or_recover`; writes on a coalescing worker
- [x] APP-2 (core side) navigation history carries contig name

### W1-B `crates/rgv-hts`
- [x] HTS-1 empty MM groups, `MN`/hard-clip guard
      — hard-clipped reads without `MN` report modifications unavailable
- [x] HTS-3 streaming remote query honouring `max_records`/filters/`keep_going`; lazy CRAM
      intersect
- [x] HTS-4 skip sequence decode when alleles are not collected; single CIGAR view per record
- [x] HTS-6 retry body-read failures, jittered backoff, `Retry-After`, shared client
- [x] HTS-7 strong ETags only in `If-Range`
- [x] HTS-9 tolerant BED/GFF parsing (whitespace, thick 0/0, `##FASTA`, percent-decoding,
      skip-and-count bad lines)
      — malformed-line count surfaced per track
- [~] HTS-10 remote config (auth, cancellation, budgets) for bbi/TDF/text tracks
      — bbi/TDF/text-feature wired with auth, cancellation, budget; remote VCF/BCF still via htslib (HTS-2)
- [~] HTS-11 transient slice errors do not escalate to whole-contig fetch
      — multi-reference slices fetch per-sequence spans; a non-span decode error still retries whole sequences once
- [~] HTS-13 index overview and `.fai` duplicate check without quadratic passes
      — quadratic passes removed; BAI re-parse and index SHA-256 kept (the digest is the remote index identity)
- [~] HTS-14 2bit allocation bounds; fuzz target writes an index
      — 2bit bounds and alignment fuzz target done; no CRAI/FAI/2bit/VCF fuzz targets; fuzzer not run
- [x] SCENE-6 pileup mate-overlap masking respects left-mate CIGAR/BQ and drains masks
      — scene summaries match the direct hts query
- [x] HTS-5 `libdeflate` enabled for the app's noodles-bgzf

### W1-C `crates/rgv-alignment-scene`
- [x] SCENE-1 MM scratch bounded by call count
- [x] SCENE-2 primitive/depth/modification reservations clipped to the tile
- [x] SCENE-5 prefer the current/largest containing atlas
- [x] SCENE-8 eviction skips pinned entries
- [x] SCENE-11 junctions starting left of the prepared interval
      — GPU arc drawn from the true start
- [~] SCENE-12 keyed cache lookups; single overlap index build
      — summary caches and chunk admission keyed; `cached_chunk_fragment` and `record_index` still linear
- [x] SCENE-13 pair-consistent sampling; stale HashSet charge
- [x] SCENE-14 seeded property harness (extension vs clean build, seam sums, reservation ≥ actual)
      — 20 seeds; no defects found; mutation-checked

### W1-D `crates/rgv-engine`
- [x] ENG-1 foreground presentation pressure/error latch
- [x] ENG-3 reclaim adopts newest request id, emits `Superseded`, errors carry the id
      — no fault injection for a real eviction error
- [x] ENG-4 per-owner scene-build failure
- [x] ENG-5 per-owner speculation slots
- [x] ENG-6 explicit worker priority order
      — reclaim no longer has absolute priority; needs on-screen soak
- [x] ENG-8 `catch_unwind` at thread roots; decoder respawn
      — decoder survives in place rather than respawning
- [x] ENG-9 wake instead of polling
- [x] ENG-10 cancellable search, persistent query pool
- [x] ENG-11 `CoalescingReceiver` `Drop`
- [x] ENG-13 bounded reference slices
      — 128 Mb ceiling
- [x] ENG-14 single `satisfies` predicate (reproduce first)
      — payload predicate unified; reference coalescing half struck as not a defect

### W1-E `crates/rgv-render`, `apps/rgv-desktop/src/app/viewer_runtime.rs`
- [x] REND-1 categorical palettes per colour mode; golden-ratio hashing for open categories
- [~] REND-2 device-lost / uncaptured-error handling
      — handlers, rebuild path and retry in place; never exercised against a real lost device
- [x] REND-3 adapter fallback, downlevel limits, `WGPU_BACKEND`
- [x] REND-4 explicit surface format policy
      — colours darker where the backend listed sRGB first
- [x] REND-5 one-physical-pixel minimum for all shapes (shader + CPU projector)
      — minimum is one logical point
- [x] REND-6 junction arcs parameterised over the unclamped span
- [x] REND-8 exact-size immutable page buffers
      — exact-size buffers; 16-byte instance is Wave 2
- [~] REND-10 one shared base palette
      — one shared base table; palette uniform, colour-blind and dark themes are Wave 2
- [x] REND-12 scissor clamp at render time
- [x] REND-14 surface rectangle overflow to the user
- [~] REND-9 (part) export scale parameter
      — interactive PNG at window scale; automation export still 1x

### W1-F `apps/rgv-desktop` (except `viewer_runtime.rs`), `apps/rgv-mcp`
- [x] UI-1 Escape never exits
- [x] APP-1 save/discard/cancel gate for exit, open, new, recover
      — any navigation counts as dirty, so closing after browsing prompts
- [x] UI-2 pinch, horizontal pan, scale-correct pixel deltas
- [x] UI-3 history and recent loci committed at gesture end
- [x] APP-2 name-based history (desktop side)
- [x] UI-6 locus parsing via the core parser; variant IDs; whitespace triples; case-insensitive
- [x] UI-7 single locus retargets the active panel only
- [x] UI-8 hover uses the unobstructed hit test
- [x] UI-9 stable auxiliary selection identity
- [x] UI-10 dialogs coexist
      — a newly requested dialog replaces the open one
- [~] UI-4 per-frame clones gated/borrowed; UI-5 virtualised lists
      — UI-4: clones gated/borrowed, read inspector cached; other inspector snapshots rebuild per frame. UI-5 done
- [~] UI-11, UI-12 small items
      — UI-12 done; keyboard scroll targets the hovered stack; hover in non-active panels remains
- [x] APP-3 landmark/cytoband I/O off the event loop
- [x] APP-4 autosave on dirty only, instance-scoped prefix
- [x] APP-5 per-pid run markers
- [x] APP-6 session open not rejected by background revisions
- [~] APP-7 typed notification log
      — 83 error writes routed to the log; two sites remain on keyword classification
- [x] APP-9 `/v1/status` Host check; APP-10 batch cancellation and wait deadlines
- [~] APP-12 idle polling; APP-13 drop handler; APP-14 monotonic TrackId
      — APP-12, 13, 14 done; APP-15 synchronous file dialogs remain
- [x] OPS-5 MCP error pass-through; OPS-6 schema descriptions; OPS-7 capture downscale and slim
      snapshot; OPS-12 temp PNG leak; OPS-13 schema range; OPS-9 docs
      — read names pseudonymised by default in inlined exports

### W1-G repository and build (no Rust source)
- [~] OPS-1 `rust-toolchain.toml`, per-OS plain build job separate from lint
      — done locally; workflows have not run on GitHub
- [~] OPS-3 fuzz job `continue-on-error` per target
      — done locally; not run on GitHub
- [x] OPS-10 (part) benchmark-only backends out of default members
- [x] OPS-11 `[profile.release]`
      — fat-LTO dist build not run to completion
- [~] OPS-14 `launch.rgv.sh`, `test.rgv.sh`, deny wildcard
      — scripts and deny fixed; large tracked data files left in place
- [~] DOC-1..DOC-8 documentation corrections
      — all but DOC-7 (feature matrix pass/fail would need real results)

## Wave 2 — structural changes inside existing crates

Run after Wave 1 is green. Each item is one reviewable change.

- [x] SCENE-3 page keys without evidence/atlas revisions (page reuse across extension)
- [~] SCENE-4 / SCENE-7 incremental row atlas extension; stable rows under sort/group
      — view-as-pairs and leftward extension of ordered layouts still rebuild
- [~] ARCH-3 charge-actual-after-build with per-owner soft budgets and LRU; exact ledger only in
      Complete mode; GPU pages outside the ledger
      — `AlignmentMemoryPolicy::Bounded` is the default; `Complete` keeps the strict path.
        Read pages draw what fits; summary pages still strict; no settings UI for the policy
- [~] SCENE-9 / REND-8 compact handles, primitives and 16-byte GPU instances with a palette
      uniform (unlocks REND-10 theming and PROD-17)
      — read instance 40 → 16 B, depth 32 → 12 B, primitive 184 → 152 B; palette uniform with
        Light, Dark and Okabe–Ito variants; `AlignmentHandle` unchanged (design noted in
        `identity.rs`); rgv-layout track colours not themed
- [~] SCENE-10 fused single-pass summary tiles; generic `SummaryTile<Family>`
      — one selection per tile and tile-clipped walks; six tile types remain
- [x] ENG-2 decoder stream continuation and chunk-level resume
      — done, including chunk-level resume; CRAM/remote resume covered only by rgv-hts tests
- [x] ENG-7 resumable/cancellable scene builds
      — desktop keeps the incumbent scene on screen during a staged build
- [x] ENG-15 coordinator split into modules; pressure classification on error types
- [~] REND-16 generic `PagedGpuCache`; shared WGSL; GPU readback goldens (covers REND-7)
      — page table/eviction/reclaim shared; plan/stage/publish left separate (8–49% alike).
        REND-7 confirmed on Metal and fixed with integer cancellation
- [~] REND-9 GPU offscreen export at Nx; vector export from semantic primitives
      — GPU PNG export done with tiling; vector SVG/PDF not started
- [x] HTS-2 remote VCF/BCF over `HttpRangeReader`; drop htslib `curl`
      — done
- [x] HTS-8 index-chunk-driven read-ahead; HTS-12 shared decompression pool
      — HTS-12 as a process-wide thread allowance (rust-htslib pools cannot be shared);
        CRAM read-ahead unchanged
- [~] HTS-15 / OPS-10 converge on one alignment backend
      — shared `BackendRecord` logic, three-way differential suite, opt-in local noodles BAM
        reader; default stays htslib until the ADR 0002 qualification criteria are met on
        real data. htslib `curl` dropped; the desktop no longer links curl or OpenSSL
- [x] UI-13 / APP-16 split `ui/tracks.rs`, remove glob preludes, diagnostics behind a facade
      — boundary violations found are listed for Wave 3; per-frame paint step still ~1,100 lines
- [x] OPS-6 derive MCP schemas from core types
      — 25 mirrors removed, batch commands typed, every field described; `rgv_query_pileup`
        and `rgv_query_variants` added (OPS-8 part); feature query and goto-gene need
        desktop commands

## Wave 3 — architecture

Status at 2026-11-22: the coverage pyramid is wired into the desktop and on by default. The
track tile pipeline is wired only as an experimental, default-off setting covering plain
annotation tracks. Nothing in this wave has been checked in the running application.

- [x] ARCH-1 decode-time summary tiles and a streamed/persisted coverage pyramid
      — on by default at ≥1000 bp/px and wherever read evidence does not cover the view;
        ultra-deep tiles are flagged when mate overlap is not collapsed; no hover inspector
        and absent from non-GPU exports
- [~] ARCH-2 / ARCH-7 headless `rgv-session` crate (workspace state, `Action` reducer, engines,
      scene publication); desktop, CLI snapshot, MCP and automation drive it
      — `Action` model and reducer in the desktop for navigation, panels, track arrangement
        and region/review edits, shared by UI and automation, with undo/redo; track load,
        display settings, session open/save and preferences not converted; no separate crate
- [~] ARCH-4 explicit `(PanelId, TrackId)` throughout
      — all 37 activate-swaps removed; ~55 functions take a panel; `inspection/selection.rs`
        and wheel/junction hover still use the focused panel
- [~] ARCH-5 public track contract `query(region, bp_per_px, cancel) → world-coordinate tiles`;
      one paged world-space renderer
      — lower crates complete. Desktop: experimental `track_pipeline = Tiles` (default
        Legacy) routes plain BED/bigBed/GFF/GTF feature tracks only; quantitative, VCF,
        interactions and the genotype matrix (PROD-3) are not wired; routed tracks lack full
        inspector fields, exports and evidence stepping (see `docs/USER_GUIDE.md`)
- [~] ARCH-6 shared I/O and decode pool keyed by visibility; lift the 64-track cap (PROD-9)
      — process-wide decode allowance with foreground priority; cap left at 64 because each
        track holds two file handles and the default macOS limit is 256
- [x] ARCH-8 `rgv-model` crate split from `rgv-hts`; automation/stress out of `rgv-core`
      — `AlignmentError` and the `BackendRecord` logic stay in rgv-hts; rgv-core keeps
        tempfile, quick-xml and sha2 (all still used)
- [~] ARCH-9 reference bases delivered engine-to-engine
      — direct sink connected; the UI relay is kept alongside it

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
