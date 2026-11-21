# RGV comprehensive architecture and code review — 2026-11-20

Scope: the working tree of `feature/unified-alignment-scene` (79 uncommitted files at review time),
about 215k lines of Rust across seven crates and two apps. Ten parallel reviewers each read one
area in depth. All findings come from reading code unless stated. Build state at review time:
`cargo check --workspace --all-targets` clean; `cargo clippy --workspace --all-targets` 822
warnings (CI uses `-D warnings`); `cargo test -p rgv-layout` 54 passed.

Markers: **[V]** = independently confirmed in source by the lead reviewer after the subagent
report. **[U]** = the reporting reviewer marked it unverified or partly inferred. Line numbers are
approximate; function names are exact. Severity is the reviewer's.

The remediation plan is `plans/COMPREHENSIVE_REVIEW_REMEDIATION_PLAN_2026-11-20.md`.

## 1. Verdict

RGV has a strong data core and a weak control plane. What stops it beating IGV today is trust and
reach (no green CI, no release, no cloud access, no evidence for the speed claim) more than
rendering.

Preserve:

- immutable scene roots, stable read handles, completeness certificates;
- world-coordinate GPU pages panned by a camera uniform (warm pan/zoom is a uniform update);
- exact coverage/allele/modification summaries independent of row downsampling;
- generation tokens with coalescing latest-wins mailboxes; one owner thread per source;
- the `CanonicalAlignmentSourceBackend` boundary and the split decode thread with real cancellation;
- the CPU reference projector and oracle query; the byte-accountable `HttpRangeReader`;
- MCP as a separate process; opt-in, loopback-only, bearer-authenticated control service;
- `unsafe_code = forbid`, near-zero `unwrap` in production code, atomic session saves.

## 2. Root causes behind the recurring recovery plans

About ten successive plans fix the same three symptoms: stuck "Updating view", blank or flickering
panels, and cache or budget thrash.

- **ARCH-1 (critical) No streaming multi-resolution product for alignments.** Read-level evidence
  is complete-or-refuse, so memory scales with depth × span. Between 10 and 1000 bp/px coverage is
  derived only from fully decoded retained evidence. At ≥1000 bp/px
  (`rgv-core/src/lod.rs:33`) the view shows `AlignmentIndexOverview`, 256 index-density bins per
  contig (`rgv-hts/src/index_overview.rs:18`), not coverage. Whole-genome view is impossible.
- **ARCH-2 (critical) Scene control plane lives on the desktop UI thread.**
  `apps/rgv-desktop/src/workspace/alignment_management.rs` (6.6k lines) owns GPU publication,
  cross-owner reclaim and retry gating, contrary to ADR 0007. `Viewer` (`lib.rs:1387-1563`) has
  about 170 fields and owns window and GPU, so nothing at session level runs headless.
- **ARCH-3 (high) The allocation ledger is a cross-layer protocol.** One `AllocationLedger` spans
  scene, engine, render and desktop (`rgv-alignment-scene/src/resource.rs`, 3.7k lines; about ten
  pressure/retry event variants at `alignment_scene_engine.rs:181-286`). Any allocation can fail
  anywhere and each layer carries its own pressure state machine.
- **ARCH-4 (high) Implicit active panel and primary alignment.** 650 `panels.active()` uses, 254
  `primary_alignment` uses, 37 `panels.activate(id)` swaps so shared code can run
  (e.g. `queries.rs:467`). Split views are fragile.
- **ARCH-5 (high) `TrackSource` is too narrow and private.** `pub(crate)`, no resolution or pixel
  width, no cancellation, closed result enum (`track_source.rs:52`). Layout is in pixels on the UI
  thread, so every pan re-lays out and re-hashes (`display/mod.rs:1512`). Four GPU renderers and
  three hit-test models.
- **ARCH-6 (medium) Thread-per-source, no global scheduler.** Two threads per alignment source,
  blocking reqwest, `MAXIMUM_ALIGNMENT_TRACKS = 64` (`rgv-core/src/session.rs:18`).
- **ARCH-7 (medium) Automation is a parallel command executor.**
  `automation/commands.rs` is 5.4k lines with no `Action` type shared with the UI.
- **ARCH-8 (medium) Layering.** Record types live in `rgv-hts`, so `rgv-layout`,
  `rgv-alignment-scene` and `rgv-render` transitively link htslib and reqwest (no WASM, renderer
  needs a C toolchain). `rgv-core` carries automation (3.1k) and stress tooling (1.6k) plus
  `tempfile`. Public surfaces are wide (core 334 names, hts 281, scene 278, render 105).
  `LocalBamCanonicalBackend` actually wraps all four of local/remote BAM/CRAM.
- **ARCH-9 (medium) Reference bases relay through the UI thread** (reference thread → UI →
  alignment thread, `queries.rs:731`). Cold read-level path has four thread hops and four record
  copies.

## 3. rgv-core and rgv-layout

- **CORE-1 (high)** `rgv-core/src/session/igv.rs:333` `parse_igv_locus` accepts only
  `name:start-end`. IGV's `locus="All"`, bare `chr1`, gene names and multi-locus strings fail the
  whole import. `split_once(':')` splits HLA names at the wrong colon. `source_from_string` treats
  `s3://`, `gs://`, `ftp://`, `file://` as local paths.
- **CORE-2 (high)** `rgv-layout/src/track_layout.rs:745-754` `layout_interactions`:
  `.take(MAX_RENDERED_INTERACTIONS)` runs before contig/viewport filters; records after the first
  2,048 never render.
- **CORE-3 (medium-high)** `track_layout.rs:951-979` `add_interaction_arc`: arcs are 24 filled
  axis-aligned rectangles (stair-steps), endpoints unclipped.
- **CORE-4 (medium-high)** `track_layout.rs:202-221` `layout_features_with_label_widths`: rows
  assigned from a fresh `row_ends` per call over viewport-intersecting features only, so features
  jump rows while panning; assumes start-sorted input; no pixel padding; overflow silently dropped.
- **CORE-5 (medium)** `track_layout.rs:228-235`: labels require `feature_type` in
  {gene, transcript, mrna, primary_transcript}; BED records have `None`, so named BED features are
  never labelled.
- **CORE-6 (medium)** `rgv-layout/src/genome_context.rs:41-48` `layout_coordinate_ruler`: ticks on
  zero-based multiples labelled `position + 1`, so the ruler reads 87,001 / 87,501.
- **CORE-7 (medium)** `session.rs` `from_legacy` (~1198) vs `validate_workspace_presentation`
  (~2470): migration can build non-contiguous groups that validation rejects. Dead legacy-field
  checks in `validate()`. **[U]** whether old writers emitted such files.
- **CORE-8 (medium)** `coordinates.rs` `ReferenceDictionary`: `id_by_name` linear-scans on every
  hash miss; name index empty after deserialization; `push` shadows duplicate names; serialized
  `Contig.id` can disagree with position.
- **CORE-9 (medium)** `session.rs:1970` `from_json`: no writer/minor version; newer enum variants
  fail with a raw serde error; older builds silently drop newer fields on re-save.
- **CORE-10 (medium)** `track.rs:813` `redact_url_credentials` strips the entire query and
  fragment, removing non-secret required parameters.
- **CORE-11 (medium)** Object ids `1_000_000 + track_id*100_000 + record_index`
  (`track_layout.rs:306`, `display/auxiliary_lanes.rs:137`) collide for tracks with ≥100k records
  (GWAS allows 250k). **[U]** effect on picking.
- **CORE-12 (low-medium)** `track_layout.rs:356-413` `layout_variants`: unclipped coordinates cast
  to `f32`; no row packing for overlapping variants.
- **CORE-13 (low-medium)** `reference_layout.rs:72-76`: codons straddling the visible edge are
  skipped; reverse frames reuse forward phase.
- **CORE-14 (low)** `GenomicInterval` and `Viewport` derive `Deserialize`, bypassing `new()`
  (reversed interval underflows `len()`, `pixel_width: 0`). `session.tracks` uncapped. Alias
  validation forbids `:`.
- **CORE-15 (low)** `track_layout.rs:1933`: raw quantitative mode truncates to the first 16,384
  records.
- **CORE-16 (opportunity)** No canonical locus parser in core (≥3 copies elsewhere); no
  dictionary-scoped contig identity; no multi-region view model; `LandmarkSearchIndex::search`
  scans and sorts every entry per query; no tests above 2^32.

## 4. rgv-hts

- **HTS-1 (high) [V]** `modified_bases.rs:73-75`: an MM group with no positions (`C+m?;`) returns
  `MalformedGroup`, discarding every modification on the read. Hard clips and `MN` are never
  checked.
- **HTS-2 (high)** `tracks/variants.rs:30,214`: remote VCF/BCF goes through htslib/libcurl,
  bypassing `HttpRangeReader` (no auth, cancellation, byte accounting, redirect policy). htslib
  writes the remote index into the working directory. First ID search streams the whole remote
  file. **[U]** stale same-basename index reuse.
- **HTS-3 (high)** `remote_alignment.rs:955-993, 1209-1243`: one-shot remote `query` clones every
  intersecting record into a `Vec` before `max_records`, filters or `keep_going`. CRAM converts
  every slice record to `RecordBuf` before the intersect test (`:1169-1176`).
- **HTS-4 (medium)** `local.rs:1576`: full sequence decoded per record even when
  `collect_alleles` is false; `record.cigar()` rebuilt four times per record
  (`:1302, 1355, 1585, 1680`).
- **HTS-5 (medium)** `Cargo.toml:43`: `libdeflate` on `noodles-bgzf` is requested only by
  `benchmarks/alignment-io`; the app inflates remote BAM without it. Remote path is
  single-threaded. **[U]** inferred from feature resolution.
- **HTS-6 (medium)** `remote.rs:436` vs `:456-502`: body-read failures are never retried; backoff
  25/50 ms with no `Retry-After` or jitter; cancellation checked only around `send()`; each reader
  builds its own `reqwest::blocking::Client` (`:243`).
- **HTS-7 (medium)** `remote.rs:172,470`: weak ETags sent in `If-Range` (RFC 9110 forbids), so a
  compliant server returns 200 and RGV reports "object changed".
- **HTS-8 (medium)** `local.rs:146-149`, `remote.rs:399-418`: read-ahead fixed 8×256 KiB, not
  index-chunk driven; `load_block` re-reads each block from a temp file on every switch.
- **HTS-9 (medium)** `tracks.rs:779,792,848`, `tracks/text_feature.rs:36,115,413`: one bad line
  aborts the whole track; BED split on tab only; `thickStart`/`thickEnd` 0/0 rejected; GFF3
  `##FASTA` fails; GFF3 attributes not percent-decoded.
- **HTS-10 (medium)** `tracks/bbi.rs:18,67`, `tdf.rs:67`, `tracks/text_feature.rs:361`: non-
  alignment remote tracks open with `HttpRangeConfig::default()` (no auth, cancellation, budgets).
- **HTS-11 (medium/low)** `remote_alignment.rs:1180-1185, 861`: any slice decode error retries
  against the whole contig; multi-reference slices always fetch whole contigs.
- **HTS-12 (low)** `local.rs:943`: each open creates its own 4-thread HTSlib pool (ADR 0002 says
  shared).
- **HTS-13 (low)** `index_overview.rs:44,144-168`: BAI parsed again by noodles, 256 queries per
  contig, `.nth()` O(n²) in contigs, index SHA-256 hashed. `reference.rs:399`: `.fai` duplicate
  check O(n²).
- **HTS-14 (low)** `fuzz/fuzz_targets/alignment_file.rs` writes only `input.bam` with no index,
  so open fails before decode. No CRAI/FAI/2bit/VCF targets. `two_bit.rs:18,78` allows two 256 MB
  allocations from a hostile header.
- **HTS-15 (structural)** Two production decoders (`local.rs` htslib, `remote_alignment.rs`
  noodles) each with their own normalise/pileup/eligibility code. ADR 0004 and the comment at
  `remote_alignment.rs:112` are stale (local CRAM uses single-threaded noodles).
- **HTS-16 (gaps)** No htsget, `s3://`, `gs://`, DRS, OAuth, presigned refresh; no remote tabix;
  remote text tracks downloaded whole (256 MiB cap); no remote bgzip FASTA; no CRAM reference by
  `M5`/`UR`; no `.hic`, cooler, MAF, PAF, GFA, GenBank, bigInteract, plain SAM; 2bit soft-masking
  dropped; bigWig zoom mean only; bigBed zoom unused; no mate/supplementary fetch API; no
  persistent remote cache.

## 5. rgv-alignment-scene

- **SCENE-1 (high)** `semantic_page.rs:2743-2745, 2762-2770`: modified-base scratch reserves
  `sequence_len × MM_tag_bytes` slots at ~140 B (about 7 GB for a 10 kb HiFi read with a 5 kb MM
  tag).
- **SCENE-2 (high)** `semantic_page.rs:619-639, 660-670`: `maximum_primitives` counts the whole
  read's CIGAR ops (every aligned base at Base LOD), unclipped to the tile, reserved twice and
  allocated via `with_capacity`. Same pattern in `summary.rs:76-105, 258-271` and
  `modification_summary.rs:84-102` (always 32 series × 2 × 2 × bins).
- **SCENE-3 (high)** `semantic_page.rs:76-86`, `presentation_store.rs:183-193`,
  `presentation_scene.rs:717-719`, `store.rs:472-473`: `AlignmentPageKey` embeds
  `evidence_revision` and `row_atlas_revision`, and `EvidenceStore::prepare` mints a new revision
  every call, so any edge acquisition misses the cache for every read page.
- **SCENE-4 (high/medium)** `row_atlas.rs:1011-1012, 283-331`: every `extend` rebuilds the full
  `RowActivityIndex` and rehashes every record (O(N log N) per extension); layers grow without
  compaction (`:958-965`); connectors linearly scanned twice per page (`:1245-1276`); superseded
  atlases stay cached (`store.rs:1142`).
- **SCENE-5 (medium)** `store.rs:1087-1096`: `prepare_row_atlas` picks the smallest containing
  atlas, flipping back to an older revision after extension.
- **SCENE-6 (medium, accuracy)** `rgv-hts/src/pileup.rs:236-252`: count-once masks the right mate
  over `[mate_start, left.end)` regardless of the left mate's CIGAR or base quality; pending masks
  never drain when the right mate is filtered out, and >100k stale entries fail the tile with
  `MateOverlapCapacityExceeded`.
- **SCENE-7 (medium)** `row_oracle.rs:282-283, 402-414`: every candidate clones read name and
  CIGAR, unledgered. `row_atlas.rs:895-908`: any non-default sort/group/pair view forces a full
  oracle rebuild and a new row space on every extension.
- **SCENE-8 (medium)** `store.rs:1486-1531`: eviction pops entries without checking pins,
  stripping evidence backing the visible locus while freeing nothing.
- **SCENE-9 (medium, memory)** `identity.rs:7-10`: `AlignmentHandle` ~56 B (repeats the 32 B
  source revision per record); `RowAtlasRecord` ~80 B; `AlignmentSemanticPrimitive` ~180-200 B.
- **SCENE-10 (medium)** `summary.rs:124-149`, `allele_summary.rs:132-171`,
  `semantic_page.rs:1753, 2174-2178`: each tile walks each overlapping read's full CIGAR from the
  read start; `store.rs:997-1046` runs six `select` passes per tile.
- **SCENE-11 (low/medium)** `junction_summary.rs:142`: junctions owned by the tile containing
  their start; arcs starting left of the prepared interval are omitted.
- **SCENE-12 (low)** `store.rs:349-357, 667`: caches are `Vec`s scanned linearly;
  `evidence.rs:229-234` builds a `HashSet` of all handles per root build; overlap index built
  twice (`evidence.rs:29-35, 66`).
- **SCENE-13 (low)** `sampling.rs`, `row_oracle.rs:297-324`: per-window sampling splits pairs;
  `evidence.rs:725-734` charges for a removed HashSet.
- **SCENE-14 (tests)** ~185 example tests; no property or randomised oracle tests.
- **SCENE-15 (structural)** Crate holds stores, eviction policy, CPU projectors and GPU
  replacement-bundle ledger logic, contrary to the ADR's "headless data model". Six near-identical
  summary tile types.

## 6. rgv-engine

- **ENG-1 (high)** `alignment_scene_engine.rs:1163-1267`, `alignment_scene_coordinator.rs:2871-2912,
  2818-2828`: foreground row-block pressure or error is not latched, so the worker spins
  prepare → `ResourcePressure` → reclaim at 100% CPU, waking the UI each time.
- **ENG-2 (high)** `alignment_scene_coordinator.rs:3628-3656, 3785-3796`, `source_actor.rs:1020-1034,
  608-618`, `local_bam_source.rs:116-142, 582-584`: `source_intent_covers` requires exact interval
  equality, so every camera sample restarts the decoder stream.
- **ENG-3 (medium/high)** `alignment_scene_engine.rs:1295-1314, 1021-1031, 1848-1854`:
  `schedule_reclaim_target` keeps an old non-zero `request_id`; eviction error paths emit `Error`
  with no request id; the desktop waits for an exact id (`alignment_management.rs:1166-1175`).
  **[U]** whether overlapping cycles occur.
- **ENG-4 (medium)** `alignment_scene_coordinator.rs:1534-1542`, `engine:1365-1375`: one lane's
  scene-build failure cancels every lane.
- **ENG-5 (medium)** `source_actor.rs:772, 856-865`, `coordinator:1567-1580, 3695`: one
  speculation slot for all owners.
- **ENG-6 (medium)** `alignment_scene_engine.rs:978-1162`: reclaim has absolute priority; visible
  row presentation waits behind other panels' decode. `has_pending_foreground_presentation_work`
  (`coordinator:1725`) has no callers.
- **ENG-7 (medium)** `coordinator:3135-3248, 2228`, `engine:1623`: scene builds are monolithic and
  uncancellable on the worker.
- **ENG-8 (medium)** No `catch_unwind` in the crate (`engine:367-370, 697-761`,
  `local_bam_source.rs:101-104`, `track_engine.rs:895-911`); a decode-thread death is permanent.
- **ENG-9 (low/medium)** `engine:36-38, 931-940`, `local_bam_source.rs:158-162`: 1 ms polling
  while the decoder is busy, 16 ms when idle.
- **ENG-10 (low/medium)** `track_engine.rs:510-611, 579-590, 856-865, 881-911`: single FIFO actor;
  searches uncancellable; up to 8 OS threads spawned per query; no result cache.
- **ENG-11 (low)** `mailbox.rs:256-304`: `CoalescingReceiver` has no `Drop`; sends succeed forever
  after a worker exits.
- **ENG-12 (low)** `engine:771-784`, `track_engine.rs:466-476`, `reference_engine.rs:189-200`:
  `Drop` joins on the caller.
- **ENG-13 (low)** Reference slices unbounded (`reference_engine.rs:325-338`); decoder read-ahead
  uncharged (`local_bam_source.rs:55-57`).
- **ENG-14 [U]** `prepared_covers` (`coordinator:3261-3294`) and `frame` (`2489-2495`) omit the
  `payload().satisfies` check applied at `3013`. Reference `QueryCompleted` coalesced by panel only
  (`reference_engine.rs:402-407`).
- **ENG-15 (structural)** Coordinator file is 7.5k lines (3.3k tests); eight hand-copied
  "satisfies" predicates; pressure classification by exhaustive matchers
  (`coordinator:3943-4060`); three channel styles.

## 7. rgv-render

- **REND-1 (high) [V]** `alignment_scene.rs:3936` `hash_color`: hue is `value % 360`, but callers
  (`:3862-3875`) pass small ranks, so pair orientation, pair status, template length and
  specialist modes render as near-identical red.
- **REND-2 (high)** `apps/rgv-desktop/src/app/viewer_runtime.rs:60-95`: no device-lost callback,
  uncaptured-error handler or error scopes anywhere.
- **REND-3 (high)** `viewer_runtime.rs:66-83`: `force_fallback_adapter: false`,
  `Limits::default()`, `HighPerformance`, no retry; fails on HPC login nodes and remote desktops.
- **REND-4 (medium)** `viewer_runtime.rs:91`: surface format from `get_default_config` passed to
  every pipeline with raw constant colours. **[U]** per-backend format ordering.
- **REND-5 (medium)** `alignment_scene.wgsl` `project_instance`, `cpu_projector.rs:736`: only
  shapes 2/3/4 get a minimum width; mismatches shimmer below 1 px/base and rows drop out below
  1 px height.
- **REND-6 (medium)** `alignment_summary.wgsl` `junction_vertex`, `alignment_summary.rs:7063, 712`:
  junction arcs are parameterised over the viewport-clamped width, so off-screen ends land at the
  viewport edge.
- **REND-7 (medium) [U]** `alignment_scene.wgsl:44-70`: double-single arithmetic may be folded by
  fast-math; no GPU readback test.
- **REND-8 (medium)** `alignment_scene.rs:4015` `gpu_buffer_capacity` rounds immutable page
  buffers to a power of two; `AlignmentGpuInstance` (`:38`) is 40 bytes where 16 would do.
- **REND-9 (medium)** `lib.rs:811-907`, `semantic_export.rs`: PNG export is a 1x CPU rasteriser
  with different coverage rules; no scale parameter; no PDF; SVG from the screen-space list.
- **REND-10 (medium)** Base colours disagree across `alignment_scene.rs:3926`, the summary shader
  and `semantic_export.rs:262`; no colour-blind palette; no dark mode
  (`viewer_runtime.rs:104`); colours baked into page buffers.
- **REND-11 (low/medium)** `context_scene.rs:927-957`: single-f32 projection jitters after deep
  zoom over stale layout; no minimum width.
- **REND-12 (low/medium) [U]** `alignment_scene.rs:3053`, `alignment_summary.rs:5124`,
  `context_scene.rs:671`: scissors clamped at capture time, replayed at render time.
- **REND-13 (low)** `alignment_scene.rs:2323-2620`: ~10 allocations per camera frame; three walks
  of every visible primitive under hover; one draw per page; O(n²) admission check (`:2148`).
- **REND-14 (low)** `lib.rs:576-598`: `RectangleRenderer` silently drops rectangles past 16 MB.
- **REND-15 (low)** Sashimi stroke uses vertical distance; depth bars at alpha 0.90 overlap; no
  summary LOD pyramid.
- **REND-16 (structural)** `alignment_scene.rs` and `alignment_summary.rs` implement the same ~30
  cache-lifecycle methods (about 3k duplicated lines); projection rules hand-mirrored in WGSL, two
  oracles, two CPU projectors and the PNG rasteriser.

## 8. rgv-desktop — UI, display, inspection

- **UI-1 (critical) [V]** `runtime.rs:104-123`: Escape exits the app when no context menu, exit
  prompt or panel is open, ignoring `egui_response.consumed` and key repeat.
- **UI-2 (high)** `runtime.rs:314-336`, `ui/interaction.rs:152-173`: no `PinchGesture`; `zoom()`
  reads only `delta.y`; horizontal swipe does nothing; `PixelDelta` used as points
  (`gestures.rs:9-13`).
- **UI-3 (high)** `interaction.rs:167-171, 401-405, 415-419, 435`, `apply_requests.rs:178-181`,
  `navigation_controller.rs:268-278`: every wheel tick, key repeat and slider change calls
  `navigation_history.visit()`; `settings.remember_locus()` on every drag-pan move.
- **UI-4 (high)** `ui/frame/prepare.rs`: unconditional per-frame clones (`assembly` `:922-928`,
  text primitives `:878-883`, ROI/review/metadata `:1029-1030, 1046`, search results `:932`),
  `diagnostics_snapshot()` (`:45`), six inspector snapshots (`:52-62`) re-running
  `decode_modified_bases` and `format_cigar`, `show_system_dialogs` clones
  (`system_dialogs.rs:46-53`).
- **UI-5 (high)** `ui/workspace_dialogs.rs:403, 518`: Regions and Review queue lists are not
  virtualised (caps are 100,000).
- **UI-6 (medium)** `workspace/navigation.rs:10`, `navigation_controller.rs:32`,
  `apply_requests.rs:10`: `split_once(':')` breaks colon-containing contigs; variant IDs like
  `chr1:12345:A:G` never reach ID search; whitespace-separated loci go to annotation search;
  contig names case-sensitive.
- **UI-7 (medium)** `navigation_controller.rs:179-189`: entering one locus closes every other
  panel.
- **UI-8 (medium)** `interaction.rs:41, 52, 116-129`: hover hit-testing ignores floating windows.
- **UI-9 (medium)** `workspace/queries.rs:2904-2924`, `inspection/selection.rs:253-256`: auxiliary
  selections are `{track_id, record_index}` and are cleared on every lane refresh.
- **UI-10 (medium)** `ui/frame/commit.rs:454-472`: one dialog at a time by fixed precedence; a
  track gear click is dropped silently when another dialog is open.
- **UI-11 (low/medium)** `selection.rs:1275`: hover only in the active panel; Up/Down/PageUp/
  PageDown always scroll the primary alignment (`interaction.rs:195-197, 379-382`).
- **UI-12 (low)** BED import splits on tabs only (`workspace_lists.rs:121`); inspector shows
  `{:?}` enum names (`inspector_controller.rs:214-225`); ideogram senses clicks but does nothing
  (`navigation.rs:385-391`).
- **UI-13 (structural)** `ui/tracks.rs` 5.5k lines with a ~1,300-line paint function; spinner
  keyed on `data_status.starts_with("Loading")`; sublanes matched by label `"Coverage"`.
- **UI-14 (gaps)** No undo/redo; command palette has 19 static commands with no arrow-key
  selection; gene-search popup has no keyboard navigation; no per-panel locus fields; one window;
  light theme forced with 120+ literal `Color32`; no keybinding customisation or i18n; Ctrl/Cmd+F
  is "next feature"; contig picker unfiltered; no per-track progress or inline retry.

## 9. rgv-desktop — app, workspace, automation

- **APP-1 (high) [V]** `runtime.rs:82-87`, `app/tasks.rs:335`,
  `workspace/session_controller.rs:39-62`: the GUI never checks unsaved changes;
  `document_state.is_dirty()` has one non-test caller (`automation/commands.rs:1164`).
- **APP-2 (high)** `workspace/navigation_controller.rs:238-250`, `workspace/navigation.rs:151-161`,
  `rgv-core/src/navigation.rs:29-32`: history stores `ContigId` only; without a dictionary every
  contig is `ContigId(0)`; with dictionaries, history resolves against `assembly.dictionary` while
  navigation resolves against the track's.
- **APP-3 (medium)** `workspace/landmarks.rs:303-345, 935-944`, `sources.rs:331`: SHA-256, reads
  and a ≤64 MiB gzip decode run on the event loop; cytoband parse failures are only logged.
- **APP-4 (medium)** `app/tasks.rs:193-244`: autosave ignores dirty state and rotates out distinct
  snapshots; untitled projects share one prefix (`launch.rs:596-615`).
- **APP-5 (medium)** `launch.rs:680-722`: one shared run marker; pid written but never checked.
- **APP-6 (medium)** `app/document_state.rs:79-81`, `app/tasks.rs:471-493`: session opens are
  rejected when unrelated background changes bump the document revision.
- **APP-7 (medium)** 238 writes to a single last-writer-wins `data_status` string.
- **APP-8 (medium)** `rgv-core/src/automation.rs:531-547, 1932-1944`: control-root containment is
  lexical (symlink escape); `OpenWorkspace` sources and `LoadTrack` URLs unrestricted.
- **APP-9 (low)** `control_service.rs:319-357`: `/v1/status` unauthenticated with no Host/Origin
  check.
- **APP-10 (low)** `control_service.rs:504-535`: timed-out batches keep running;
  `AutomationWait::File` has no deadline (`automation/commands.rs:501`).
- **APP-11 (low)** `rgv-core/src/settings.rs:92-99`, `session_controller.rs:28-37`: settings
  writes are non-atomic and on the UI thread; a truncated file silently resets preferences
  (`launch.rs:210-217`).
- **APP-12 (low)** `runtime.rs:398-426`: 50 ms / 16 ms idle polling despite UI wakers.
- **APP-13 (low)** `session_controller.rs:39-62`: drop handler has its own extension table and
  parses a dropped `.json` up to three times.
- **APP-14 (low) [U]** `session_controller.rs:108-120`: TrackId reuse (max+1) while async events
  for the removed track are in flight.
- **APP-15 (low)** 17 synchronous `rfd::FileDialog` calls stall polling.
- **APP-16 (structural)** `use super::super::*` and a 150-line glob prelude defeat module
  boundaries; ~6k lines of diagnostics referenced from ~130 call sites, always compiled.

## 10. Engineering health, MCP, distribution

- **OPS-1 (high)** CI has never been green (23/23 runs failed or cancelled). Floating stable
  toolchain with pedantic clippy `-D warnings`, no `rust-toolchain.toml`.
- **OPS-2 (high)** Windows has never compiled past `rgv-core` in CI.
- **OPS-3 (high)** Weekly fuzz job dead for 8 weeks; `alignment_file` aborts on a LeakSanitizer
  report on its first input.
- **OPS-4 (high)** `benchmarks/IGV_COMPARISON.md` compares RGV's headless cached path with IGV's
  goto-to-snapshot wall clock on a synthetic 20 MB BAM;
  `benchmark-results/PHASE9_QUALIFICATION_STATUS.md:25` records the matched comparison as
  "Not run".
- **OPS-5 (medium)** `apps/rgv-mcp/src/server.rs:1257-1276, 1231, 1202`: MCP errors replace
  diagnostics with "Command failed".
- **OPS-6 (medium)** `server.rs:1315-2797`: 4 doc comments across ~1,480 lines of argument types;
  `rgv_execute_batch.commands` is `Vec<Value>`; ~1,500 lines of hand-mirrored `Tool*` enums.
- **OPS-7 (medium)** `server.rs:722-939`: captures inline base64 PNG up to 64 MiB with no
  downscale, plus the workspace twice; telemetry dominates the snapshot.
- **OPS-8 (medium)** No direct evidence tools (pileup at position, variants in interval, features
  overlapping, goto gene).
- **OPS-9 (medium)** `docs/MCP.md:211` says read names are never echoed, but
  `rgv_export kind=alignments` inlines `read_name`.
- **OPS-10 (medium)** Two HTS stacks and two TLS stacks ship; benchmark-only noodles 0.110 and
  `rsomics-bamio` share the workspace lockfile (612 packages).
- **OPS-11 (medium)** No `[profile.release]`.
- **OPS-12 (low/medium)** `server.rs:869-887`: `capture_window` leaks its temp PNG on two early
  returns.
- **OPS-13 (low)** MCP demands `schema_version == 6` exactly (`server.rs:58,130`);
  `docs/MCP.md:157` describes schema 4; default port 60151 is IGV's batch port.
- **OPS-14 (low)** Tracked 19.7 MB BAM and ~25 large images; empty `test.rgv.sh`; `launch.rgv.sh`
  hard-codes a home path; `deny.toml` allows multiple versions and deny fails on a wildcard
  dependency and unmaintained bincode.
- **OPS-15 (low)** 8 MCP tests for 3.2k lines.
- **OPS-16 (process)** 38 plan documents (21.5k lines), 9 remediation/recovery; 79 dirty files;
  last commit 2026-09-05.
- **OPS-17 (distribution)** No tagged or signed release; AppImage built on ubuntu-24.04; no
  bioconda/Homebrew; no auto-update.

Metrics: 0 first-party `unsafe`; 272 `#[allow]`; 1 TODO; 1,311 tests; release binaries 38.9 MB
(desktop), 13.5 MB (mcp).

## 11. Product gaps and documentation discrepancies

Documentation:

- **DOC-1** `audits/FEATURE_AUDIT_2026-08-25.md` is stale (hosted catalogue, IGV genome JSON, 2bit
  and remote text tracks now exist).
- **DOC-2** `docs/FORMAT_SUPPORT.md` contradicts itself on hosted genomes and on remote text
  annotations.
- **DOC-3** `plans/RGV_IGV_PARITY_ROADMAP.md` §5 marks "VCF and genotype review: Complete";
  `VariantTrackSettings` (`rgv-core/src/track.rs:556-585`) has no genotype-row fields.
- **DOC-4** Interactive SVG export is reachable only from the Sashimi inspector and automation
  (`ui/frame/menus.rs:114` has PNG only).
- **DOC-5** Landmark gene bundles and the richer filter/group predicates are undocumented for
  users.
- **DOC-6** Dark-mode branches are dead (`ui/inspectors.rs:294`; `lib.rs:250` forces light).
- **DOC-7** `validation/alignment-scene-feature-matrix.json` carries no pass/fail status.
- **DOC-8** ADR 0004, `docs/MCP.md` schema version and `docs/DESKTOP_FEATURE_ACCESS.md` are stale.

Gaps (P0 blocks adoption, P1 expected, P2 nice to have):

| ID | Gap | Priority |
|---|---|---|
| PROD-1 | Signed installers, tagged release, auto-update | P0 |
| PROD-2 | Hands-on real-data qualification for CRAM and remote alignments | P0 |
| PROD-3 | On-track multi-sample genotype matrix, AF colouring, VCF density modes | P0 |
| PROD-4 | `s3://`, `gs://`, OAuth, presigned refresh, htsget, DRS | P0 |
| PROD-5 | Whole-genome view | P1 |
| PROD-6 | UCSC track hubs / GenArk | P1 |
| PROD-7 | IGV batch-script and port-60151 command compatibility | P1 |
| PROD-8 | IGV session XML fidelity (colours, scales, render options) | P1 |
| PROD-9 | More than 64 alignment tracks; merged `.bam.list` | P1 |
| PROD-10 | SAM, unindexed BAM, bgzip FASTA, GenBank, index-on-load | P1 |
| PROD-11 | Interactive SVG/PDF figure export | P1 |
| PROD-12 | Phased / haplotype-resolved variant display, trio view | P1 |
| PROD-13 | SV breakpoint split view and SV table | P1 |
| PROD-14 | Plugin / scripting API | P1 |
| PROD-15 | True headless render | P1 |
| PROD-16 | Same-row supplementary linking, quick consensus | P2 |
| PROD-17 | Dark theme, colour-blind palettes | P2 |
| PROD-18 | CNV/LOH, `.igv` numeric, MAF | P2 |
| PROD-19 | Motif finder, BLAT, in-silico PCR | P2 |
| PROD-20 | Hi-C, circular view, synteny/PAF/GFA | P2 |
| PROD-21 | Session sharing by URL, web embed | P2 |

Leapfrog opportunities: clinical variant-review queue from VCF with verdict hotkeys and audit
export; agent-driven evidence summarisation over MCP; trio and tumour-normal presets; cohort mode
with a GPU genotype/coverage matrix; seamless whole-genome-to-base zoom from a coverage pyramid;
declarative publication figure mode; IGV drop-in compatibility layer; first-class long-read SV
view; ultra-deep amplicon/UMI mode; shareable review bundles.
