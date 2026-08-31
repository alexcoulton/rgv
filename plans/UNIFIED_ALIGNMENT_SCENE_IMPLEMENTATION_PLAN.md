# Unified alignment-scene implementation and legacy-removal plan

Status: recovery R0–R6 complete on `feature/unified-alignment-scene`. R0–R5 pass their automated
gates and the user accepted the R6 native checkpoint on 31 August 2026. Broader Phase 7 feature
expansion is unblocked; its remaining product-family gates and Phase 8 deletion are still open.

Current execution authority: `UNIFIED_ALIGNMENT_SCENE_RECOVERY_PLAN.md`. Its automated R0–R5
frontier now proves the bounded control plane, owned navigation liveness, and single scene-backed
local-BAM authority. Its user-reviewed R6 record accepts the minimum native navigation and
interaction behaviour. Phase 7 feature expansion may proceed, while Phase 8 deletion remains
gated on migration and qualification of the remaining consumers.

The branch is no longer headless-only. Commit `1a923fa` implemented the headless scene
architecture and also cut the local-BAM read lane over to immutable scene frames, persistent GPU
pages, stable-handle picking, and scene-backed inspection. Commit `91d36f5` then repaired
integration contracts exposed by the first ACTB/BAP1 manual qualification: camera-exact row
activity, stable row-coordinate lineage, exact/pending/absent vertical authority, live scrollbar
geometry, terminal Complete-read refusal settlement, and command-line Complete-read-limit
propagation.

That desktop slice entered before the complete Phase 6 gate had passed. This was a sequencing
deviation, not a change of architecture. It does not waive any gate below: local-BAM read pixels
are only the first Phase 7 slice, the all-consumer cutover is incomplete, and Phase 8 destructive
deletion cannot complete until the remaining consumers have migrated and qualification has
passed.

The native test that initiated recovery invalidated the previous execution frontier. Under rapid
pan, zoom, and contig changes, that build could remain permanently on `Updating view...` while its
BAM scene-source actor was idle. Sampling also showed repeated full allocation-ledger inventories
on UI background polls while the process held approximately 9 GiB. Recovery replaced those
control-plane and mixed-authority paths; the automated R0–R5 evidence and accepted R6 checkpoint
are recorded in the recovery plan.

This plan supersedes the alignment-presentation direction in the earlier resident-window and
transactional-preview plans. Those documents remain useful evidence, but they are not the delivery
authority for the replacement described here.

## Progress ledger

The ledger distinguishes four different claims that were previously easy to conflate:

- **implemented**: the required code and focused tests exist;
- **integrated**: the desktop production path consumes it;
- **qualified**: the complete phase gate has passed across its required workload/mode matrix; and
- **accepted**: the user has manually verified the native visual behaviour where required.

Passing crate tests is implementation evidence; it is not by itself a qualified or manually
accepted phase.

| Phase | Current status | Evidence and remaining gate work |
| --- | --- | --- |
| 0 — authority, baselines, deletion map | Partial | The accepted ADR, feature/mode inventory, source inventory, telemetry, and BAM/CRAM source-position feasibility exist. A reproducible, user-reviewed ACTB/BAP1/WGS/exome/long-read fixture and screenshot bundle plus a complete baseline measurement report remain open. |
| 1 — contracts, identities, scene crate | Implemented in the scene model/local-BAM path; end-to-end source-backend gate open | Typed factorised contracts, immutable chunks, coverage certificates, source revisions, stable source-position handles, true-duplicate preservation, payload sidecars, and zero-copy selections exist. Supporting CRAM/remote identity evidence is currently at the HTS layer; it is not yet an end-to-end unified-scene backend qualification. |
| 2 — scene store and resource ledger | Implemented headlessly; gate evidence present | Immutable current/candidate publication, allocation-level charging, leases, pins, reservations, independent summary roots, pressure policy, and high-water tests exist. Production-wide pressure/soak evidence remains part of later qualification. |
| 3 — source actor and row atlas | Implemented for the local-BAM backend | Latest-wins/fair scheduling, resumable canonical decode, cancellation salvage, immutable row-atlas publication, stable-handle indexes, and clean-vs-incremental fingerprints exist. Other production source backends remain outside this scope. |
| 4 — semantic pages and CPU reference path | Broadly implemented; qualification open | Read primitives, specialist modes, exact summary pages, CPU projection, stable picking, inspection, and export APIs exist. One captured panel epoch now projects typed Coverage, Sashimi, and Modifications results with explicit Pending/Unavailable/Ready states; shared Raw and CPM Sashimi scales are resolved atomically from the complete coherence group. The feature-matrix test currently proves inventory integrity, not exhaustive independent-oracle parity for every product mode. |
| 5 — persistent GPU pages | Read and coverage/allele renderers integrated; shared reclaim and camera-only capture implemented; remaining product families open | Read world-coordinate pages, admission/pinning, prefetch, camera uniforms, overlays, unchanged-page reuse, device-resource discard, and CPU/GPU geometry tests exist. Summary-page identity is based on compatible tile content and family dependencies rather than whole-root publication revisions, so widening a prepared horizon does not inherently rekey unchanged coverage tiles. A sibling persistent renderer draws exact coverage and generic reference-aware substitutions. Shared-ledger reclaim now composes one exact goal across both renderers, protects both foreground candidates, retires speculative full-scene pins, batches deterministic LRU eviction, and suppresses rewarming until authority or budget genuinely changes. The ledger has a tested single-flight replacement-bundle primitive which separately proves steady-state base fit and bounded old/new overlap. Read and summary renderers each precharge a two-slot camera ring and expose a content-authority-checked camera-only capture path with no retained allocation, page-table replacement, or unchanged-geometry upload; transient scratch remains ledger-accounted. The desktop atomically captures and privately stages the supported read-plus-summary candidate with renderer-owned retirement tokens. Full manual visual acceptance, bisulfite GPU interpretation, junction/modification GPU families, exhaustive framebuffer equivalence, and a same-scene desktop software presenter remain open. |
| 6 — camera navigation | **Recovery R0–R6 complete and accepted** | The control plane is constant-cost/event-driven, navigation has one owned transaction, camera-only capture and bounded prefetch/reclaim are implemented, deterministic rapid-navigation/resource traces pass, and the user accepted the minimum native continuity, flicker, scroll, and interaction checkpoint on 31 August 2026. |
| 7 — desktop semantic cutover | **Unified local-BAM recovery cutover implemented; broader Phase 7 unblocked** | The supported local-BAM path consumes the committed scene epoch for rows, pixels, coverage/substitutions, picking, stable-handle selection, and status. Legacy query/row/display code is disconnected or compile-disabled for that path and remains only at the unsupported-source boundary pending later Phase 8 deletion. Remaining product-family integration and qualification may now proceed. |
| 8 — exclusive renderer and deletion | **Blocked behind remaining Phase 7 migration** | Legacy authority must be removed deliberately after every required scene-backed replacement is integrated and qualified, not retained as a fallback or deleted prematurely. |
| 9 — contextual lanes | Not started | Genes, reference, annotations, and other contextual lanes still use the generic display/retained panel presentation path. |
| 10 — remote qualification and soak | Not started for the unified scene | HTS-layer remote and CRAM infrastructure is useful foundation work, but the production scene backend intentionally supports local BAM only. Local CRAM, remote BAM/CRAM, pressure soak, and memory reconciliation remain open. |

### Post-MVP stabilization incorporated into this plan

The work in `91d36f5` advances the existing architecture rather than introducing another one:

1. `AlignmentRowAtlas` now derives a camera-exact activity profile and stable coordinate-space
   lineage, separating atlas-wide retained rows from rows active at the current locus.
2. Coordinator frames carry that exact row authority to the desktop; stale or disjoint authority
   is rejected.
3. Desktop vertical state distinguishes exact, pending, and absent authority and preserves a latent
   camera only when its lineage remains compatible.
4. The live panel allocation owns the scrollbar rail and hit geometry; scene data controls only
   the semantic extent, thumb, and density overview.
5. Complete-read refusal is a non-discardable terminal control result, and a later compatible frame
   can settle it. This closes the permanent `Updating view...` failure mode.
6. The command-line Complete-read limit now reaches the created alignment track rather than
   silently reverting to the old default.

These changes directly advance Phases 3, 6, and 7 and strengthen Phase 2/3 publication semantics.
They do not complete derived-lane migration, all-consumer cutover, legacy deletion, contextual-lane
migration, or remote qualification.

### Camera-publication incident correction awaiting native acceptance

The current native build is **not visually accepted**. The preceding build repeatedly produced
the following behaviour during zoom in/out at ACTB:
`replacement GPU transition bundle <id> is already active`, and a pending zoom could display a
full-height vertical thumb even though the locus contained many scrollable rows. The changing
bundle IDs show repeated short-lived ownership collisions, not one leaked bundle and not evidence
that the Complete-read requirement inherently exceeds memory.

The collision occurred because a camera-only desktop update could enter the same single-flight GPU
replacement transaction that source/presentation reconciliation was already using. Expected
temporary ledger occupancy was then flattened into a terminal engine/desktop error, so a
recoverable scheduling edge became `Complete reads unavailable`. At the same time, desired-camera
panel state, previously committed GPU pixels, row activity, scrollbar extent, and status could be
published from different attempts. In particular, pending row authority was interpreted as a
zero-extent exact result, producing the misleading full-height thumb.

The correction is an authority-boundary change, not another renderer fallback:

1. **Two navigation authorities.** `DesiredNavigationTarget` changes immediately with input and
   owns cancellable acquisition intent. `CommittedPresentationTarget` changes only through one
   successful presentation commit. Tokens and target revisions reject every stale completion or
   retry; a retry for an older target can never overwrite the latest camera.
2. **One committed presentation object.** `PresentedAlignmentEpoch` owns the camera-exact panel
   scene frames and the matching read-plus-summary GPU bundle. Render, hit testing, selection,
   status, row metrics, and scrollbar projection read that object rather than independently
   assignable staging fields. A failed or deferred candidate leaves the preceding committed epoch
   intact; no read or summary half may publish alone.
3. **Camera-exact row and scrollbar authority.** Every publishable lane carries row activity for
   the candidate camera. Scroll extent, density overview, thumb, and hit geometry are projected
   synchronously from that same candidate before its atomic commit. Full pixel-target identity and
   row-semantic identity are distinct: vertical camera, clip, density, or surface-only movement
   keeps the same genomic row profile exact and the scrollbar interactive while row blocks catch
   up. A genomic interval or row-lineage change revokes it. While such a desired target lacks exact
   row authority, the previous exact thumb may remain visible only as explicitly provisional and
   noninteractive; `Pending` must never be converted to `maximum = 0` or a full-height exact thumb.
4. **Precharged camera capture.** Read and summary renderers each own a two-slot camera-frame ring
   admitted with the content replacement. Movement over unchanged resident content captures both
   new camera frames from immutable content authorities, then publishes both or neither. This path
   opens no replacement bundle, admits no retained allocation, rebuilds no page table, and uploads
   no unchanged geometry. CPU packing/encoder scratch and any other transient memory remain
   explicitly charged and bounded.
5. **Bounded interaction envelope.** Immediate camera capture is valid only while the requested
   horizontal interval, row blocks, LOD, device generation, scene revisions, and complete prepared
   page manifests remain covered by the committed content authority. Preparation extends that
   envelope predictively within the user budget. Leaving it schedules a content replacement; it
   does not pretend missing pages exist, partially commit one renderer, or clear a truthfully
   covered lane. Numerical envelope sizes remain measurement-led.
6. **Typed pressure for every owner-local operation.** Allocation pressure from `UpdateCamera`,
   `ReplaceContract`, `InstallReferenceAuthority`, `SetPreparationPolicy`,
   `PrewarmPresentation`, `PumpSource`, and `PumpPresentation` is a nonterminal typed event with
   owner, request token, target, operation, intent, pressure, and retryable/infeasible disposition.
   Focused engine tests cover this contract and active-bundle contention. Intrinsically oversized
   work may become a truthful terminal capacity result; ordinary occupancy must retain the
   committed epoch and remain pending.
7. **Progress-authoritative retry.** Desktop integration now schedules at most one
   token/target/attempt-matched retry and rechecks the exact typed blocker after ledger progress;
   unrelated allocation or pin churn cannot wake it. Settlement racing error delivery is detected
   immediately, stale retries are rejected, and same-target hover/selection refresh cannot bypass
   the waiting authority. Optional source read-ahead and explicit prewarm may yield.
8. **Typed application-budget collision.** `ReduceBudget` and Clear Cache no longer flatten an
   active replacement into source failure. Each source retains the latest applicable budget edit,
   waits for its exact blocker, and reruns its local eviction pass. Clear Cache remains one ordered
   `0 -> configured budget` transaction, so mailbox coalescing cannot omit the zero pass.
9. **Operation-aware vertical limit.** An intrinsically oversized speculative
   `PumpPresentation` block preserves the current visible-complete frame and publishes truthful
   `BudgetLimited` residency. It limits instant deep scrolling; it does not make complete reads
   unavailable. Ordinary occupancy pressure remains retryable. Optional `PumpSource` semantics are
   unchanged.

A later ACTB checkpoint exposed one narrower violation of item 3: dragging rapidly down the read
stack changed the cursor to the application's blue wait indicator and then locked the scrollbar.
Temporary tracing proved that the cached and desired target revisions were already equal and the
committed/desired genomic row targets were compatible, but the cached authority had nevertheless
become `Pending` with no atlas revision. This was not event-loop starvation or a RAM limit. The
desktop capture path had coupled the genomic row profile to exact worker-camera and visible-row
page coverage; the display rebuild then discarded the retained profile a second time.

The integrated correction now uses one shared row-authority predicate based on genomic interval,
evidence/atlas lineage, and row-layout contract only. Visible-row page coverage is explicitly not
part of that identity. A contract-compatible progressive snapshot remains admitted even when the
new row block is still pending, panel capture records the truthful covered-row intersection, and
display reconstruction may read the compatible committed row profile without claiming stale GPU
pixels. Genomic movement or row-layout lineage change still revokes authority. Focused tests prove
both sides: a vertical camera change with missing row pages retains exact row authority, while a
genomic interval or layout-contract change is rejected.

Implementation and desktop-integration evidence now exists for both renderer camera-capture
paths, typed all-operation engine pressure, `PresentedAlignmentEpoch`, atomic read-plus-summary
capture, camera-exact scrollbar publication, exact GPU/source/budget retry, and the optional
vertical-residency distinction. Recovery subsequently added the owned navigation journal,
constant-size event-driven ledger control, exact contextual-product states, prepared-page spatial
indexes, incremental row-first prefetch, precharged camera envelopes, and online bounded GPU
reclaim indexes. On 31 August 2026 the independent affected suites passed: engine 120/120,
alignment scene 153/153, renderer 78/78 plus its integration test, and desktop 435 passed with one
intentional ignore; workspace all-target compilation, formatting, and diff validation also passed.
These results complete automated recovery R0–R5. The user subsequently accepted the native R6
checkpoint on 31 August 2026 after assessing the corrected RNA/GRCh38/GENCODE build.

### Current execution frontier

1. Qualify the integrated coverage/generic-substitution GPU slice under partial evidence, pressure,
   collapsed-Reads, multi-track failure, selection, and manual visual tests; then connect the same
   exact, same-epoch model to persistent bisulfite, junction/Sashimi, and modification GPU pages,
   including selection and picking.
2. Finish whole-candidate GPU admission for genuine content changes: read and derived renderers
   must privately plan and stage every missing page, page-table replacement, precharged camera
   ring, and frame allocation under one shared replacement bundle. Opaque renderer-owned
   retirement tokens must prove the exact old allocations credited to steady-state fit; remove
   the transitional multi-phase retry loops. Camera-only movement inside the bounded interaction
   envelope must never enter this transaction.
3. Complete Phase 7C by moving every remaining product consumer to scene handles and one panel
   scene epoch: export, automation, MCP, keyboard/mate navigation, properties, and diagnostics.
4. Pass the Phase 7 atomic gate before deleting legacy authorities under Phase 8.
5. Continue with contextual-lane migration and remote/CRAM qualification only after those gates.

## Decision

Build a new headless `rgv-alignment-scene` subsystem and make it the sole semantic authority for
alignment evidence, row layout, drawing, picking, selection, inspection, and export.

Preserve the proven HTS, complete-evidence, filter, summary, and row-assignment algorithms, but do
not wrap the current exact-viewport `AlignmentQuery`/`DisplayList` pipeline in another cache. The
new source store uses canonical immutable chunks, factorised contracts, source-position record
identities, immutable scene snapshots, and allocation-level resource leases. GPU pages are
disposable derivatives of those snapshots.

Develop and qualify the replacement outside the desktop presentation path. At desktop cutover,
switch an alignment track and all of its derived lanes as one authority. There will be no normal
application mode in which new read pixels are combined with legacy read picking, legacy read
rectangles, a retained read texture, or a white masking layer.

The old implementation is preserved in Git on `main` at `54ae599`. The failed hybrid experiment
and its evidence are preserved on `wip/read-browsing-scene-experiment` at `87f1a26`. This branch may
therefore delete superseded alignment-presentation code once its reference fixtures have been
captured and the corresponding replacement has passed its gate.

## Why this is a selective rebuild

The failed experiment did not invalidate the whole alignment stack.

### Preserve

- local and remote BAM/CRAM readers, indexes, reference validation, and range-cache protections;
- normalisation, filters, CIGAR interpretation, technology-specific semantics, and packed record
  accessors;
- complete-or-refuse individual-read admission;
- exact coverage, allele, junction, modification, pair, and filter-provenance algorithms;
- immutable packed-column storage and zero-copy record views where their ownership is made
  explicit;
- canonical row packing, grouping, sorting, reconciliation, and edge-extension algorithms;
- camera, panel-layout, clipping, and stable vertical-scroll mathematics;
- WGPU device/surface infrastructure, font infrastructure, and non-alignment renderers;
- generation clocks as final publication and cancellation predicates; and
- settings for Complete read limit, total cache budget, and desired browsing buffer.

### Replace

- exact viewport queries as the resident or presentation cache unit;
- the monolithic `QueryOptions::fingerprint` as a universal compatibility contract;
- one replaceable resident window per panel;
- mutable published row scenes and query-local row projections;
- alignment `DisplayList` rectangles as the source of truth;
- query-local `usize` record selection and `object_id = index + 1` picking;
- one whole-frame screen-space GPU slab for alignment geometry;
- retained read textures and the synchronous CompleteHorizon preview;
- panel-wide navigation state deciding alignment interactivity;
- per-frame legacy/new renderer ownership decisions; and
- cache-entry accounting that can miss live allocations retained through shared `Arc`s.

### Do not copy from the failed branch

The genomic tile math and Metal shader may be consulted and selectively ported behind new tests.
Do not cherry-pick its desktop integration, active/pending lane presentation, white read-pane mask,
legacy/GPU fallback composition, or query-index-to-scene-handle bridge.

## Product contract

### Complete evidence

For every alignment track declared Complete at a target:

- every eligible source record intersecting the target is enumerable exactly once;
- byte-identical duplicate records remain distinct;
- long, paired, supplementary, and spliced reads crossing chunk or render-tile boundaries retain
  one semantic identity;
- filtered-record provenance is complete for the same certified interval;
- coverage, junctions, modifications, pair summaries, selection, inspection, and export are
  derived from the same evidence revision; and
- no random sampling, hidden record cap, incomplete margin, or GPU residency state is labelled
  complete.

If the current exact population exceeds the configured Complete read limit, individual reads are
refused explicitly while any available exact summaries remain truthful. The limit is an admission
policy, not a chunk identity and not a speculative cache target.

### Continuous navigation

Inside a prepared scene:

- held horizontal pan changes the live camera and visible page selection only;
- vertical scroll changes the row camera and visible row-page selection only;
- a zoom inside a compatible layout/LOD band changes camera uniforms and page selection only;
- no BAM/CRAM query, exact target batch, full-population scan, row repack, screen-space display-list
  build, or unchanged GPU upload occurs; and
- real scene geometry remains visible while the pointer is held.

At an unprepared boundary, RGV continues to draw the truthfully covered portion of the immutable
current scene while it prepares the missing evidence. It never stretches old read pixels or
clears the entire lane to conceal an ownership transition. Missing source evidence may be shown as
a clearly bounded loading region; no architecture can display records it has not acquired.

### Interaction

- A visible read, its hover state, its click target, inspector record, mate/supplementary
  navigation, and exported identity all resolve through one stable scene handle.
- GPU readback is not required for picking.
- Pointer capture may suppress a click during an active drag, but a camera move inside complete
  scene coverage does not require a new exact query before the scene becomes interactive again.
- Selection survives camera movement, page eviction, scene extension, and compatible payload
  enrichment.

### Bounded resources

- Total cache budget controls charged retained CPU and GPU allocations.
- Current visible evidence and mandatory visible presentation pages are pinned.
- Speculative margins, inactive row/LOD variants, and disposable GPU pages evict before current
  evidence.
- Candidate work reserves capacity before allocating.
- A bounded transition reserve permits one old and one new visible presentation set during an
  atomic zoom/policy swap.
- Decoder scratch, remote on-disk cache, HTTP bodies, and GPU-driver overhead are separately
  reported rather than pretending they are retained cache bytes.
- Repeated pan/zoom/scroll/return and cancellation storms converge to a stable allocation count and
  high-water mark.

## Architectural invariants

These are implementation constraints, not aspirations. Each must have an executable test or
diagnostic assertion before desktop cutover.

1. **One authority:** one published alignment scene revision is authoritative per panel/track.
2. **One frame capture:** rendering and interaction consume the same immutable panel scene frame.
3. **Immutable publication:** no object reachable from a published snapshot is mutated. Extension
   creates a new root that shares immutable allocations.
4. **Source revision identity:** every chunk and handle includes a source revision that changes
   when the local file/index or validated remote object changes.
5. **Source-position record identity:** physical records use a stable source coordinate—BAM virtual
   offset, CRAM container/slice/ordinal, or a proven equivalent. A content digest is a semantic
   fingerprint, not the unique identity of byte-identical duplicates.
6. **Coverage certificates:** completeness is proved by certified source intervals and contracts,
   never inferred from the minimum and maximum coordinates of resident records.
7. **Exact multiplicity:** chunk overlap and deduplication preserve every eligible source record
   exactly once.
8. **Factorised contracts:** source records, payload capability, summaries, layout, style,
   reference/annotation dependencies, admission policy, and camera are separate typed contracts.
9. **Capability ordering:** richer payload may satisfy a poorer request; a poorer payload may not
   silently satisfy a richer request.
10. **Camera independence:** exact camera origin, pane position, vertical scroll, hover, and
    selection are absent from immutable base-geometry page keys.
11. **Stable row ownership:** one read receives one row for a layout-scene revision. Genomic tiles
    never assign rows independently.
12. **Current/candidate isolation:** cancellation, failure, or refusal of a candidate leaves the
    current scene byte-for-byte unchanged.
13. **GPU derivation only:** GPU eviction cannot change evidence, identity, summaries, picking, or
    selection.
14. **Mandatory-page atomicity:** a candidate is not published until its mandatory visible pages
    have either GPU leases or a same-scene CPU presentation path.
15. **Stable overlay identity:** hover and selection are overlay state keyed by stable handles and
    never rekey or recolour immutable base pages.
16. **Exact summary independence:** summaries never depend on which read-geometry pages happen to
    be resident.
17. **Latest-work bound:** a source actor retains bounded control work, the latest foreground
    requirement per fair owner, and bounded speculation. Pointer events cannot form an unbounded
    FIFO.
18. **Single mutable source owner:** one actor owns each mutable HTS reader/index/reference context.
19. **Useful cancellation:** completed certified chunks may be salvaged; partial populations and
    partial aggregates are never published as complete.
20. **Allocation-exact accounting:** every CPU/GPU allocation has one allocation ID and one charged
    capacity regardless of the number of snapshots that reference it.
21. **No uncharged lifetime:** removing a cache key does not reduce charged live bytes while a
    snapshot or projection still retains the allocation.
22. **Foreground priority:** speculative work yields or stops at a bounded source/build checkpoint
    when current evidence needs the same actor or budget.
23. **Atomic shared semantics:** lanes or tracks sharing a sashimi scale, annotation status, or
    comparison contract publish as one coherence group.
24. **Local/remote equivalence:** the same immutable source served locally and over HTTP produces
    the same stable record identities, multiplicity, filters, capability states, and summary
    fingerprints.
25. **No production semantic fallback:** after cutover there is no runtime branch that silently
    restores legacy alignment evidence, picking, or pixels.

## Layered data model

“One scene” does not mean one giant allocation. It means one immutable authority graph with
explicit layers and lifetimes.

```text
AlignmentSourceActor
    |
    v
AlignmentSceneStore
  canonical immutable record chunks + payload sidecars
  coverage certificates + source-position identities
  overlap indexes + exact summary tiles
    |
    v
AlignmentEvidenceSnapshot
  immutable scene-store root and exact capability states
    |
    v
AlignmentRowAtlas
  immutable stable handles -> row assignments for one layout contract/class
    |
    v
AlignmentPresentationScene
  semantic genomic pages + row interval/picking index + page manifest
    |
    v
PanelSceneFrame
  live camera + one scene lease per visible track/lane + coherence groups
    |
    +--> CPU scene projector
    +--> GPU page cache and renderer
    +--> picking / selection / inspection / export
```

### Source and contract types

Illustrative types:

```rust
struct AlignmentSourceRevision {
    source: SourceFingerprint,
    revision: SourceRevisionFingerprint,
}

struct RecordContract {
    filters: AlignmentFilterFingerprint,
    include_secondary: bool,
    include_supplementary: bool,
}

struct PayloadContract {
    sequence: CapabilityRequirement,
    qualities: CapabilityRequirement,
    auxiliary_tags: AuxiliaryTagSet,
    decode_reference: Option<ReferenceRevision>,
}

struct SummaryContract {
    mate_policy: OverlappingMatePolicy,
    minimum_base_quality: u8,
    minimum_junction_flank: u32,
    allele_detail: SummaryDetail,
    modification_detail: SummaryDetail,
}

struct LayoutContract {
    row_scale_class: RowScaleClass,
    stacking: ReadStackingMode,
    grouping: AlignmentGroupMode,
    group_order: AlignmentOrderDirection,
    sorting: AlignmentSortMode,
    sort_order: AlignmentOrderDirection,
    sort_anchor: Option<u64>,
    pair_mode: PairLayoutContract,
}

struct PresentationContract {
    geometry_lod: GeometryLod,
    style: AlignmentStyleFingerprint,
    reference: Option<ReferenceRevision>,
    annotation: Option<AnnotationRevision>,
}

struct AdmissionPolicy {
    complete_read_limit: usize,
    total_cache_budget: usize,
    desired_buffer_percent: u16,
}
```

`max_records`, screen coverage bins, reference display, and colour mode must not all be hashed into
one record-cache key. Changing a coverage resolution or colour should not discard compatible
decoded reads.

### Stable record handles

```rust
struct AlignmentHandle {
    source_revision: AlignmentSourceRevision,
    source_position: AlignmentSourcePosition,
}
```

The existing two-word `AlignmentStableId` remains useful as a cross-payload semantic fingerprint,
but it does not uniquely identify true duplicate source records across arbitrary independently
decoded chunks. The source adapters must capture a physical stable position. This is an early
architecture gate, especially for CRAM; the scene store must not proceed on a query-local
“digest + occurrence” identity that changes with chunk history.

### Canonical chunks and completeness

- Choose bounded source/cache-aligned decode units, not exact viewport intervals.
- A record has one canonical owner chunk based on source position, not every genomic tile it
  overlaps.
- Separate overlap indexes reference owner records from all genomic tiles they intersect.
- Chunk edge queries may return reads beginning before the requested genomic boundary. Deduplicate
  by source-position handle, not by biological fields.
- Each complete source-acquisition transaction carries a `CompleteCoverage` certificate containing
  source revision, record contract, interval, and filtered-record provenance, plus the complete
  dependency set of canonical chunks. Completeness never remains attached to one independently
  evictable member chunk.
- Scene coverage is the union of compatible certificates.
- Core columns and optional sequence/quality/auxiliary payload are separate immutable allocations
  or transactionally enriched replacements, so changing detail does not duplicate all core data.
- CRAM decode reference identity belongs to source decoding. BAM mismatch/allele display reference
  is a separately swappable evidence/presentation dependency.

### Exact summary tiles

- Coverage, allele, junction, modification, pair, and filter-provenance summaries use exact
  canonical ownership rules.
- A summary tile owns events by a deterministic genomic/source coordinate, so summing adjacent
  tiles cannot double-count long reads, mates, or splice junctions.
- Maintain exact range maxima for continuously scaled coverage without scanning the full read
  population per camera.
- Summary tiles have independent typed contracts and may outlive a presentation LOD.
- A sampled or refused read-geometry capability cannot be used to construct an exact summary.

### Immutable row atlas

Rename/refactor the current `AlignmentRowScene` to `AlignmentRowAtlas` so “scene” has one product
meaning. Preserve its canonical packing algorithms, but remove published mutation through
`Arc<RwLock<...>>` and query-local projection registration.

- Handles, not query indices, address row assignments.
- Extension builds a private new root sharing unchanged immutable layers.
- The new atlas publishes atomically.
- Row interval indexes support `row + genomic position -> ordered handles` without projecting all
  rows into an exact target array.
- Compact and Stable stacking are distinct layout contracts.
- Vertical virtualisation chooses visible row blocks without changing assignment.

### Semantic geometry pages

Each page is keyed by content, not a viewport:

```rust
struct AlignmentPageKey {
    source_revision: AlignmentSourceRevision,
    evidence_root: EvidenceRevision,
    layout: LayoutContractFingerprint,
    presentation: PresentationContractFingerprint,
    genomic_tile: CanonicalGenomicTile,
    row_block: RowBlock,
    lod: GeometryLod,
}
```

Pages contain genomic tile-local positions, stable rows, draw order, component type, style token,
and stable handle. They never contain absolute chromosome coordinates as `f32`, pane pixels,
scroll offsets, hover colours, or query-local object IDs.

- Long/CIGAR primitives split deterministically at render-tile boundaries while retaining one
  handle.
- Base read bodies, event/detail overlays, labels, hover/selection overlays, and exact summaries
  are separate page families where their invalidation differs.
- Page and row-block sizes are canonical but selected from measurements; this plan does not invent
  performance constants before profiling.

## Authority and publication model

### Actor and worker ownership

- One `AlignmentSourceActor` owns the mutable source, index, decode-reference context, and source
  chunk-store mutation.
- It consumes non-discardable controls, latest desired foreground requirements with fairness, and
  one bounded speculative target.
- Scene-builder workers operate only on immutable chunk leases. They may build summaries, row
  atlas roots, semantic pages, and indexes without calling the mutable source.
- The enforcing resource ledger grants reservations before source or builder work begins.
- Builders publish completed immutable resources back to the scene coordinator; they never mutate
  a published scene.

### Independent freshness domains

Keep these separate:

1. live camera sequence;
2. source acquisition intent and source revision;
3. evidence-root revision;
4. layout-atlas revision;
5. presentation/page revision; and
6. panel scene-frame publication revision.

A camera move must not cancel compatible source chunks or row work. A filter/source change must
not reuse incompatible evidence. A style-only change must not reread the BAM.

### Current and candidate

```rust
struct AlignmentLaneState {
    published: Option<Arc<AlignmentPresentationScene>>,
    desired: SceneIntent,
    candidate: Option<SceneBuild>,
}

struct PanelSceneFrame {
    camera: LiveCamera,
    camera_sequence: u64,
    leases: Arc<[SceneLease]>,
    coherence_groups: Arc<[CoherenceGroupRevision]>,
    frame_revision: u64,
}
```

- Candidate work is private and cancellable.
- Compatible completed chunks/pages from a stale candidate may enter caches.
- A stale candidate cannot publish.
- Before publication, acquire every mandatory visible CPU/GPU page lease and recheck camera,
  source, evidence, layout, style, reference, annotation, and admission contracts.
- Swap one immutable panel frame at a frame boundary.
- Retain the old frame and its mandatory leases until the replacement has been submitted.
- Tracks or lanes with shared scale/annotation semantics form a coherence group and publish
  together; an unrelated slow lane cannot cover or erase a ready lane.

## Rendering architecture

### CPU reference projector

Implement a bounded CPU projector before the GPU renderer. It consumes the same semantic pages,
camera, row camera, and clip rectangles as the GPU path. Its purposes are:

- deterministic reference output for geometry and picking tests;
- a software/device-loss path using the same semantic scene;
- immediate presentation from CPU-ready pages while a derived GPU upload completes; and
- evidence that GPU acceleration is an optimisation rather than a second architecture.

It may generate transient screen primitives for visible pages, but it may not own evidence,
identity, row assignment, or an exact-viewport cache.

### GPU renderer

- Add a dedicated alignment-scene renderer; do not force genomic pages through the current
  screen-space `RectangleRenderer` cache.
- Store page-local integer/split coordinates and row IDs.
- Supply camera origin/scale, page origin, row origin/height, vertical scroll, lane clip, and
  coverage transform through uniforms or compact draw metadata.
- Draw only intersecting genomic pages and visible/nearby row blocks.
- Upload only new, invalidated, re-admitted, or device-loss pages.
- Keep hover and selection in a small overlay/selected-handle buffer.
- Labels use stable anchors and bounded current-camera collision reconciliation.
- Never silently truncate at a generic per-frame instance-buffer ceiling.
- GPU buffers are charged by allocation capacity and pinned by active leases.

### Pan

For contained constant-scale movement, every input delta updates `LiveCamera`, selects pages, and
redraws. It does not advance evidence/layout revisions. Directional extension begins before the
prepared edge and retargets at chunk boundaries. If the user outruns acquisition, only the
truthfully uncovered edge reports loading.

The desktop keeps the desired navigation target separate from the committed presentation target.
Inside the committed content authority's bounded interaction envelope, a camera move captures the
precharged read and summary camera rings and commits one `PresentedAlignmentEpoch`; it does not
open a GPU replacement bundle. Outside that envelope, source/content preparation is cancellable
candidate work while the last coherent epoch remains the publication authority.

### Vertical scroll

Vertical scroll changes the row-camera uniform and visible row-block set. Foreground blocks and a
measured bounded look-ahead are pinned. It never changes the layout key, repacks rows, crops a
cached `DisplayList`, or blanks the whole read pane while admitting a page.

The row-activity profile and scrollbar projection are camera-exact members of the same publication
attempt as the pixels. A pending desired target cannot synthesize an exact zero-row result. If its
row blocks are resident, vertical movement uses the precharged camera rings; otherwise
`PumpPresentation` remains retryable after real ledger progress until the requested blocks become
publishable or are proven intrinsically infeasible.

### Zoom and LOD

- Keep decoded evidence independent of scale.
- Define a small measured set of overlapping layout/geometry validity classes with hysteresis.
- Prewarm adjacent useful classes inside the resource budget.
- Scale-independent read bodies can remain truthfully visible while richer detail pages prepare.
- Detail overlays become authoritative only under their matching LOD/reference contract.
- Crossing a row-policy/layout-class boundary publishes an immutable candidate atomically.
- Overview/regional representations use exact summaries or exact span primitives; they do not
  sample and call the result Complete.

## Picking, selection, inspection, and export

Invert the live camera to genomic position and stable row, query the immutable row interval index,
then refine candidates against CIGAR/component geometry and deterministic draw order.

Return a typed hit:

```rust
struct AlignmentHit {
    track: TrackId,
    scene_revision: SceneRevision,
    handle: AlignmentHandle,
    component: AlignmentComponent,
}
```

- Replace `selected_record: Option<usize>` with a source-scoped stable handle.
- Inspector and mate/supplementary navigation materialise through the snapshot/chunk handle.
- Promoted read and selection restoration use stable handles or explicit semantic resolution, not
  continual query-index repair.
- Automation and MCP report scene/evidence revisions and stable handles.
- Export enumerates an exact zero-copy scene selection and exact summary tiles from the same
  evidence root.
- No product consumer may reach around the scene to read `current_query`, `layout_query`, or
  `cached_layout` after semantic cutover.

## Resource ledger and cache policy

Replace advisory tier headroom with an enforcing allocation ledger.

```rust
struct AllocationRecord {
    id: AllocationId,
    class: AllocationClass,
    charged_capacity: usize,
    pins: usize,
    last_used_revision: u64,
}

struct AllocationReservation {
    class: AllocationClass,
    maximum_bytes: usize,
}
```

- Charge vector/buffer capacity, not only logical length.
- Shared immutable allocations are registered once and referenced by lease.
- Evicting a cache lookup key does not uncharge an allocation still retained by a scene.
- The ledger is shared across decoded core chunks, payload sidecars, overlap indexes, summary
  tiles, row atlases, semantic pages, GPU pages, and retained transition frames.
- Tiers may borrow unused budget; they cannot all assume the same free bytes simultaneously.
- Current scene pins are visible in diagnostics.
- A lower user budget stops new speculation immediately and evicts unpinned state. If current
  evidence alone exceeds the new ceiling, report exact pinned overage and avoid pretending it was
  evicted.
- Disjoint jumps either fit current plus a reserved candidate, use an explicitly bounded reported
  foreground transition allowance, or retain only a noninteractive captured old frame after
  releasing old semantics. This policy must never present old pixels as current evidence.

Eviction order:

1. cancelled/uncommitted speculative reservations;
2. distant horizontal and vertical prefetch pages;
3. inactive GPU pages;
4. inactive semantic pages and layout/LOD variants;
5. inactive summary derivatives;
6. inactive source chunks; and
7. navigation history.

Current evidence and mandatory visible pages are not eviction candidates.

## Scheduling and cancellation

Replace FIFO viewport input with:

- a bounded non-discardable control queue for open/close/budget/source changes;
- latest desired foreground state per panel/track owner, with age-based fairness;
- one latest speculative requirement per source; and
- explicit priority for current missing evidence over page prewarm and margins.

At every bounded record/batch/source-chunk and scene-build checkpoint, ask whether work still
contributes to a current intent. Retarget useful overlapping work rather than restarting it.

Cancellation telemetry distinguishes:

- requested;
- observed by decoder/builder;
- transport aborted;
- completed and cached as a certified reusable unit; and
- completed but discarded because it was not independently valid.

Do not add concurrent access to one mutable HTS source merely to simulate priority. Improve remote
transport abortion only after measuring its benefit; remote correctness, request limits, and byte
limits are required before remote enablement.

## New crate and dependency boundary

Create `crates/rgv-alignment-scene` as a pure, headless ownership boundary.

- Depends on `rgv-core` and `rgv-hts`.
- Owns alignment scene contracts, handles, immutable evidence/row/presentation snapshot types,
  semantic primitives, row/pick indexes, page builders, and resource metadata.
- Does not depend on WGPU, winit, egui, desktop state, system dialogs, or source I/O.
- `rgv-engine` depends on it to own the source scene store, scheduler, and publication pipeline.
- `rgv-render` consumes its immutable pages.
- `rgv-layout` retains non-alignment layout and may contain a temporary test-only legacy adapter;
  alignment row and semantic primitive ownership moves into the new crate rather than being
  wrapped indefinitely.
- `rgv-desktop` orchestrates cameras, panel frames, settings, and UI only.

Add a dependency-policy test or documented check so the scene crate cannot acquire desktop or GPU
dependencies later.

## Headless harness

Add an alignment-scene harness by extending the existing benchmark/validation ecosystem rather
than creating a second UI or source engine.

It must support:

- clean-load versus resident-scene semantic comparison;
- deterministic pan, reversal, zoom, vertical-scroll, and cancellation traces;
- exact record-handle/multiplicity fingerprints;
- row-atlas and summary fingerprints;
- CPU/GPU primitive equivalence;
- picking matrices across device pixel ratio, clipping, row density, and paired mode;
- framebuffer hashes and tolerant image comparisons for deterministic fixtures;
- export and automation snapshots;
- allocation-ledger reconciliation and high-water marks; and
- source-query, full-scan, layout, page-build, upload, and frame telemetry.

Shadow comparison is test/benchmark-only and strictly budgeted. It is never enabled during normal
ACTB browsing and never influences UI state.

Visual UI assessment remains manual: launch the qualified build and ask the user to assess it. Do
not automate GUI interaction or screenshots for visual approval.

## Feature and mode inventory

Desktop cutover is blocked until every existing alignment feature is scene-native or explicitly
removed as a product decision.

### Evidence and topology

- local and remote BAM and CRAM, including required decode reference;
- one and many alignment tracks, primary/secondary parity, multiple panels;
- complete/refused/index-only overview states;
- secondary, supplementary, duplicate, QC, mapping-quality, and user filters;
- long reads, splice skips, true duplicates, pairs, overlapping-mate policy, and supplementary
  chains;
- source, reference, annotation, filter, and device invalidation; and
- errors, cancellation, cache pressure, track removal, and device loss.

### Rows

- Stable and Compact stacking;
- every group mode and both group orders;
- every sort mode and both sort orders, including base-anchor and tag sorting;
- pair view and pair-length bounds;
- promoted/selected read ordering;
- row density, vertical virtualisation, independent pane scroll, and stable scroll anchors; and
- zoom/layout-class transitions.

### Read geometry and style

- hidden/visible reads;
- read bodies, direction terminals, CIGAR matches/mismatches, insertions, deletions, skips,
  clipping, padding, connectors, labels, and selected-chain overlay;
- hidden/mismatch/all base display, reference presence/absence, and quality shading;
- modified/unmodified base calls, probability threshold, hidden codes, and colour modes;
- all standard colour modes;
- bisulfite contexts, SMRT kinetics, Ultima, SBX, and auxiliary-tag modes; and
- hover, click, selection, inspector, mate/supplementary navigation, and export.

### Derived alignment lanes

- exact coverage, alleles, automatic/fixed/shared transforms, and center line;
- junction collapsed/expanded modes, raw/CPM counts, scales, labels, minimum depth/flank, and
  annotation status;
- modification frequency; and
- reference bases required by alignment interpretation.

## Implementation workstreams and gates

No phase enters the desktop production presentation path before its gate passes.

### Phase 0 — freeze authority, baselines, and deletion map

1. Approve this plan and record the invariants as an architecture decision.
2. Create a machine-readable feature/mode inventory mapped to current tests and consumers.
3. Capture semantic fixtures and user-reviewed screenshots from the stable reference branch for
   ACTB, BAP1, WGS, exome, long-read, pairing, modifications, and specialist colour modes.
4. Add telemetry proving the current baseline source, projection, layout, screen-list, upload, and
   memory costs without changing behaviour.
5. Mark every retained, adapted, and deleted type/function in a source inventory.

Gate:

- every product consumer of alignment evidence is identified;
- source-position identity feasibility is proven for BAM and CRAM;
- feature matrix and independent biological oracle are agreed;
- reference fixtures are reproducible; and
- no implementation ambiguity remains about one-authority cutover.

### Phase 1 — typed contracts, source identities, and scene crate

1. Add `rgv-alignment-scene` with typed factorised contracts and capability states.
2. Capture source revisions and stable source-position record IDs.
3. Add coverage certificates and exact multiplicity/property tests.
4. Add immutable core chunks with optional payload sidecars and zero-copy selections.
5. Keep the existing `AlignmentQuery` only as a compatibility oracle, outside the new hot path.

Gate:

- arbitrary chunk-boundary tests equal a fresh indexed query for ordered source IDs, multiplicity,
  filters, and summaries;
- layout-only and full payload resolve to the same handles;
- true duplicates, long/spliced reads, contig edges, and pair boundaries pass;
- local and HTTP copies of the same source produce equivalent identities; and
- repeated viewport selections allocate no new record arenas.

### Phase 2 — immutable scene store and enforcing resource ledger

1. Replace the per-panel resident-window model in the new path with an actor-owned canonical chunk
   store.
2. Add immutable evidence roots, current/candidate leases, allocation IDs, pins, and reservations.
3. Convert exact summary algorithms into independently owned canonical tiles.
4. Implement explicit pressure, budget-decrease, and disjoint-jump policy.

Gate:

- candidate failure/cancellation cannot mutate current;
- every live allocation is charged exactly once;
- no projection can keep uncharged wide storage alive after cache eviction;
- two tracks/panels, budget reduction, disjoint jump, and pinned overage pass; and
- cancellation and navigation loops reach a stable high-water mark.

### Phase 3 — latest-wins source actor and immutable row atlas

1. Replace viewport FIFO queries in the new path with latest foreground/speculative intents and
   fairness.
2. Decode/certify bounded canonical chunks and retarget useful work.
3. Move/refactor canonical row algorithms into immutable `AlignmentRowAtlas` roots.
4. Add row interval/picking indexes keyed by stable handles.

Gate:

- a large target burst keeps bounded queue and memory;
- only current compatible intents publish;
- foreground is not trapped behind speculation beyond a bounded checkpoint;
- completed useful chunks survive cancellation;
- contained movement performs no full record selection or row repack; and
- incremental row extension matches a clean canonical atlas fingerprint.

### Phase 4 — complete semantic pages and CPU reference path

1. Implement every alignment/derived-lane semantic primitive from the feature matrix.
2. Partition by genomic tile, row block, and LOD without assigning rows locally.
3. Implement CPU projection, picking, interaction, inspection, and export from the same snapshot.
4. Run headless comparisons against independent fresh queries and preserved fixtures.

Gate:

- all feature/mode capabilities have deterministic semantic equivalence;
- rendering and picking return the same stable handle/component;
- exact summaries and exports match independent oracles;
- vertical page boundaries and genomic tile boundaries neither omit nor duplicate; and
- no scene consumer uses query-local indices.

### Phase 5 — persistent GPU pages

1. Selectively port proven world-coordinate shader/tile ideas from the preserved WIP branch.
2. Add page admission, pinning, staged upload, camera uniforms, row uniforms, clipping, and
   selection overlays.
3. Establish CPU/GPU geometry and picking equivalence.
4. Test device loss and software presentation from the same scene.
5. Precharge exactly two bounded camera-frame slots for each read and summary renderer as part of
   content admission. Capture camera/scroll/overlay changes from an immutable content authority
   without retained allocation, page-table replacement, unchanged-geometry upload, or a
   replacement bundle; retain explicit transient accounting.

Gate:

- mandatory page sets publish atomically;
- a page miss cannot clear or change semantic evidence;
- interaction never rekeys immutable base pages;
- active leases survive cache pressure;
- prepared constant-scale movement uploads no unchanged geometry; and
- repeated read-plus-summary camera capture keeps both rings bounded at two slots, performs no
  retained allocation, and publishes neither renderer when either authority check fails; and
- GPU and CPU paths agree for the exhaustive scene fixture set.

### Phase 6 — camera navigation in the headless/native harness

1. Route pan, vertical scroll, compatible zoom, reversal, and edge extension through live cameras.
2. Add predictive horizontal, vertical, and adjacent-LOD preparation within the user budget.
3. Add cancellation, retargeting, and resource-pressure traces.
4. Qualify ACTB and BAP1 before broader workloads.
5. Keep desired acquisition targets separate from the committed presentation target, and define
   the measured horizontal/vertical/LOD interaction envelope in which camera-only capture is
   truthful.
6. Treat owner-local allocation pressure as a typed scheduling edge for every engine operation.
   Retry only after ledger progress and preserve operation-aware `PumpPresentation` liveness.

Gate:

- prepared pan has zero source query, target-batch copy, full summary scan, row repack,
  screen-list build, and unchanged upload;
- vertical scroll has zero row assignment and horizontal-geometry rebuild;
- zoom returns to resident compatible scenes without source I/O;
- no held interaction exposes a full white lane or release replacement flash;
- camera-only interaction cannot open or collide with a replacement bundle, allocate retained GPU
  state, partially publish read/summary frames, or turn retryable pressure into source failure;
- pending row authority cannot produce a full-height exact thumb, and exact pixels, row activity,
  scroll extent, density overview, thumb, hit testing, and status share one committed epoch;
- the final target equals a clean independent load; and
- measured results establish realistic performance thresholds rather than relying on arbitrary
  numbers chosen in advance.

### Phase 7 — atomic desktop semantic cutover

1. Publish one immutable alignment scene set/epoch per panel, containing every visible alignment
   track and explicit coherence groups.
   The desktop presentation commit is one `PresentedAlignmentEpoch` containing the camera-exact
   panel frames and matching read-plus-summary GPU bundle; independently assignable staging fields
   are not product authorities.
2. Move all alignment product consumers to scene handles: coverage/junction/modification/read
   picking, selection, inspector, navigation, keyboard traversal, sashimi, export, automation,
   MCP, properties, and diagnostics.
3. Initially, if necessary for a short integration slice, a legacy pixel adapter may project the
   **new scene**; it cannot read legacy queries or own interaction. It is test-only and scheduled
   for deletion in the next phase.
4. Do not add a normal application backend preference or per-frame fallback.

Delivery milestones used for progress tracking do not weaken the atomic gate:

- **7A — read lane:** publish local-BAM scene frames and use scene read pages, stable-handle
  hover/picking/selection, and inspection. Substantially implemented; final Phase 6/manual
  qualification remains open.
- **7B — derived lanes:** render coverage, alleles, junctions/Sashimi, modifications, and required
  reference interpretation from the same snapshot and epoch. Typed lane capture and atomic
  headless projection exist, including shared Raw/CPM Sashimi scales. Exact coverage and generic
  reference-aware substitutions now have persistent scene-native GPU pixels in the desktop, with
  partial-authority/pressure/failure-isolation qualification in progress. Bisulfite interpretation,
  junction/Sashimi and modification GPU pixels, and derived-lane interaction remain open; no
  missing family is silently supplied by a legacy semantic fallback.
- **7C — remaining consumers:** migrate keyboard and mate navigation, export, automation, MCP,
  properties, diagnostics, multi-track, and multi-panel coherence. Remains open.

These milestones describe delivery order only. Until 7A–7C all pass together, Phase 7 remains
partial and Phase 8 cannot delete the corresponding oracle or legacy code.

#### Phase 7B semantic wiring checklist

| Product behaviour | Semantic authority | Native pixels | Remaining acceptance work |
| --- | --- | --- | --- |
| Exact coverage depth | Same-epoch depth summary pages and CPU oracle implemented | Persistent GPU pages integrated | Partial-edge, collapsed-Reads, multi-track isolation, and transition-pressure control paths have focused tests; framebuffer, cross-renderer pressure, and manual visual qualification remain |
| Generic base substitutions | Reference-revision-keyed allele pages and reference-aware CPU oracle implemented | Persistent GPU allele pages integrated on the coverage scale | Manual zoom/threshold/quality-weight/selection checks and exhaustive CPU/GPU equivalence |
| Bisulfite protected/converted calls | Strand/context-aware CPU oracle implemented with exact ±2 reference context | Deliberately unavailable in the GPU presenter | Add a semantically exact native presenter; never reinterpret these calls as generic substitutions |
| Coverage selection marker | Typed same-epoch selected-position overlay implemented | Native overlay integrated | CPU/GPU geometry parity is tested; manual interaction qualification remains |
| Junctions and Sashimi | Exact junction pages, CPU projection, Raw/CPM normalization, and atomic shared-scale groups implemented | Not yet integrated | Define non-duplicating cross-tile GPU ownership, render persistent arcs, then add picking/labels and manual qualification |
| Modification frequency | Exact modification pages and typed CPU projection implemented | Not yet integrated | Add persistent native pages/pixels, selection, picking, labels, and manual qualification |
| Derived-lane inspection/export/automation | Stable semantic components exist in the scene model | Not applicable | Route each consumer through the captured panel epoch and stable handles under 7C |

“Integrated” in this table means the production desktop invokes the new path. It does not mean the
behaviour is qualified or manually accepted; those claims require the remaining gates in the last
column.

Gate:

- no product consumer reads `current_query`, `layout_query`, `resident_query`, `cached_layout`, or
  query-local selection indices;
- primary/secondary tracks and multi-panel state use one path;
- every alignment-derived lane reports the same panel scene epoch;
- desired/pending state cannot replace or mutate committed pixels, scrollbar authority, picking,
  selection, or status, and a read/summary capture commits atomically or not at all;
- user manually verifies static interaction before live navigation is connected; and
- unsupported modes block cutover rather than silently falling back.

### Phase 8 — exclusive renderer cutover and destructive deletion

1. Make the scene renderer the only alignment renderer.
2. Remove alignment rectangles from generic screen-space slabs.
3. Remove alignment use of panel-wide retained textures and CompleteHorizon previews.
4. Remove query-local picking and selection.
5. Remove duplicated primary/secondary layout construction and exact-camera alignment layout
   caches.
6. Remove the temporary scene-to-legacy pixel adapter.
7. Keep generic `DisplayList`/`RectangleRenderer` only for non-migrated, non-overlapping lanes.

Current implementation note: for a supported local BAM, the scene renderer owns read pixels at
runtime and the old alignment event loop cannot run as a fallback. This is runtime pixel authority,
not renderer-code exclusivity or Phase 8 completion. Query-era projection fields, display-list
construction, query-local interaction, `CompleteHorizon`, retained navigation textures, and the
legacy engine remain present until their Phase 7 replacements pass.

Gate:

- no legacy alignment telemetry or code path remains reachable;
- no runtime flag can select old alignment authority;
- ACTB, BAP1, WGS, exome, RNA, long-read, multi-track, multi-panel, low-memory, and device-loss
  qualification passes; and
- the user manually verifies pan, zoom, vertical scroll, hover, selection, and specialist modes.

### Phase 9 — core contextual lanes and remaining track migration

1. Use the same lane-scene protocol for built-in genes, reference, and complete text features so
   the primary browsing view moves under one camera without a panel-wide texture.
2. Add per-lane scene leases for remaining formats in measured priority order.
3. Keep cross-track coherence groups explicit where scales/comparisons are shared.
4. Remove the whole-panel navigation texture after its last consumer migrates.

Current implementation note: this phase has not started. Alignment read regions can already be
excluded from the retained preview, but genes, reference, annotation, and other contextual regions
still require the existing presentation path.

Gate:

- a slow or unprepared lane cannot cover a ready lane;
- genes/reference/alignments move continuously in the core browsing configuration;
- every remaining fallback is lane-scoped, visible in diagnostics, and scheduled for removal; and
- the panel-wide retained-texture architecture is deleted when no longer required.

### Phase 10 — remote qualification and production soak

1. Qualify remote BAM and CRAM cancellation, validators, range blocks, request/byte budgets, and
   reference behaviour.
2. Run sustained navigation, cancellation storms, multi-source pressure, and session lifecycle
   tests.
3. Reconcile allocation-ledger totals with process/GPU measurements and document unavoidable
   overhead.

Current implementation note: this phase has not started for the unified scene production path.
Source-position and remote-HTS foundations exist, but local CRAM and all remote alignment sources
currently fail explicitly with no legacy fallback rather than claiming qualification.

Gate:

- remote and local evidence fingerprints agree;
- request and byte ceilings hold under adversarial navigation;
- repeated workflows reach stable memory high-water marks;
- no stale scene can publish after source/track/session replacement; and
- there is no known correctness, interaction, resource, or presentation regression.

## Legacy deletion map

Delete only after the indicated replacement gate, but do not preserve these as permanent
fallbacks.

| Legacy area | Replacement | Earliest deletion |
| --- | --- | --- |
| `NavigationPreviewKind::CompleteHorizon`, 4,000-record/24 MiB provisional limits, `navigation_provisional_display` | camera-native scene pages | Phase 8 |
| Alignment use of `NavigationPreviewRenderer` and retained read texture | live scene camera; same-scene CPU path | Phase 8 |
| `AlignmentPanelProjection.current_query/layout_query/resident_query` as product authorities | published evidence/presentation snapshot | Phase 7 |
| Exact-camera `alignment_rows`, `row_layout_cache`, `cached_layout` | immutable row atlas and page manifest | Phase 8 |
| Mutable `AlignmentRowScene.records_by_storage` registration | immutable atlas roots | Phase 3 after oracle migration |
| Per-panel `AlignmentResidentWindow/Store` and whole-window replacement | source-scoped canonical chunk store | Phase 8 after engine compatibility removal |
| Exact-query `WeightedLruCache<QueryCacheKey, Arc<AlignmentQuery>>` as residency owner | allocation-ledger scene store | Phase 8 |
| `resolve_foreground_query`, whole-window extension/summary rebuild hot paths | scene selections, chunk acquisition, summary tiles | Phase 8 |
| Alignment `DisplayList` construction and `extend_alignment_layers` | semantic pages + scene renderer | Phase 8 |
| `alignment_record_at_point` and `object_id = index + 1` | row interval picker + stable handle | Phase 7/8 |
| `selected_record: usize` and index repair across queries | stable source handle | Phase 7 |
| Baked selected/hover colours in alignment rectangle slab hash | overlay handle buffer | Phase 8 |
| Duplicated primary/secondary alignment layout paths | panel scene-set builder | Phase 8 |
| Exact-viewport alignment GPU slabs | genomic tile × row-block page cache | Phase 8 |
| `hold_commit_until_end` for camera-ready scene lanes | live camera + atomic scene publication | Phase 8 |
| Panel-wide retained texture | per-lane scene protocol | Phase 9 |

Retain `WeightedLruCache` for unrelated caches where entry ownership really matches allocation
ownership. Retain `DisplayList` and the generic rectangle renderer for non-alignment lanes until
their own migration; deleting them globally is outside the alignment cutover.

## Validation strategy

### Correctness/property tests

- arbitrary chunk splits and merge orders;
- long and spliced reads crossing multiple chunks/pages;
- true duplicate records;
- filter and payload upgrades;
- pair/junction/modification ownership across boundaries;
- clean query versus scene selection and summary equivalence;
- current/candidate cancellation and contract mismatch;
- stable handles across reload-compatible scenes and layout/full payload;
- source revision invalidation; and
- local versus remote immutable source equivalence.

### Interaction tests

- genomic/row camera inversion and deterministic draw order;
- hover/click/component hit across DPR, clipping, zoom, row density, and scroll;
- selection preservation across pan, zoom, page eviction, and extension;
- mate/supplementary navigation and promoted row;
- primary/secondary and multi-panel equivalence; and
- a high-row committed epoch plus a newer pending target retains a provisional noninteractive
  thumb rather than publishing `maximum = 0`; the exact newer epoch swaps pixels, row activity,
  extent, density, thumb, status, and hit authority together;
- a failed or deferred camera candidate preserves the preceding `PresentedAlignmentEpoch`, and a
  partial read/summary capture cannot become visible; and
- stale frame/scene mismatch refuses interaction rather than selecting the wrong read.

### Resource tests

- every allocation charged once by capacity;
- shared current/candidate roots do not double charge;
- no uncharged Arc after cache-key eviction;
- reservation failure leaves current untouched;
- pressure follows declared eviction order;
- camera-only read and summary captures reuse their precharged two-slot rings, report zero retained
  allocation and unchanged-geometry upload, and charge/release all transient scratch;
- active replacement occupancy yields typed nonterminal pressure for every owner-local operation;
  stale token/target retries cannot publish and retries do not repeat without ledger progress;
- speculative `PumpPresentation` resumes after progress while optional source read-ahead/prewarm
  may yield, proving deep vertical-scroll liveness without a retry loop;
- budget decrease reports pinned overage;
- device-loss GPU resources release/rebuild without semantic loss; and
- bounded loops/cancellation storms reach steady state.

### Native workloads

- ACTB extreme RNA depth;
- BAP1 moderate RNA depth;
- exome and WGS loci across low/high depth;
- long-read and specialist technology fixtures;
- local and remote BAM/CRAM;
- one/many tracks and panels;
- pan, reversal, zoom in/out/return, deep vertical scroll/return, and rapid mixed gestures; and
- low/default/high user cache budgets and browsing-buffer settings.

Automated tests establish semantics, accounting, and frame behaviour. The user performs visual
acceptance when prompted.

The immediate native visual checkpoint is deliberately specific and remains open:

- at ACTB, repeatedly zoom in/out/return, hold-pan in both directions, reverse direction, and drag
  rapidly through the deep vertical stack; there must be no active-replacement error overlay,
  terminal Complete-read failure, stuck `Updating view...`, full-lane white replacement, stale or
  full-height thumb, missing resident reads, or lost hover/selection;
- at BAP1, pan and release, zoom through the individual-read boundary, then navigate to and from
  ACTB; there must be no release flash, retained old-locus reads, inherited ACTB scroll extent, or
  missing density overview; and
- only the user records this visual gate as accepted. Automated UI driving or a successful build
  cannot substitute for that decision.

## Measurement-led acceptance

Do not choose arbitrary millisecond or record-count promises before the architecture is profiled.
The invariant gates are fixed; numerical performance gates are established from phase results.

Every qualification report must include:

- source queries and remote request/byte counts;
- chunk reuse, cancellation, and salvage;
- exact scans, row packs/projections, semantic page builds, GPU uploads, and cache misses;
- frame median/tail distributions and input-to-first-frame;
- current/candidate publication latency;
- allocation bytes by class, pins, reservations, evictions, transient scratch, and high-water mark;
- complete/refused capability states and record counts; and
- scene/evidence/picking/export fingerprints.

Prepared constant-scale pan has a non-negotiable qualitative gate: none of source I/O, target-batch
copy, full summary scan, row repack, exact screen-list build, or unchanged geometry upload may
occur. If it does, the architecture has regressed even if a fast machine hides the latency.

## Stop conditions

Stop and revisit the architecture rather than adding a fallback if:

- BAM or CRAM cannot provide a stable source-position identity;
- a published scene requires interior mutation to learn a new projection;
- any displayed consumer still needs a parallel exact `AlignmentQuery` authority;
- current and candidate cannot coexist within an explicit charged transition policy;
- a feature requires query-local selection identity;
- page keys require exact screen coordinates or interaction colours;
- tile-local layout cannot reproduce canonical global rows;
- exact summaries depend on resident render pages;
- the GPU path requires a semantic model different from the CPU path;
- unsupported alignment modes would require per-frame legacy fallback; or
- automated equivalence cannot explain a difference from a clean indexed query.

## Architectural review feedback

Three independent read-only reviews examined the clean `54ae599` branch.

Accepted feedback:

- create a dedicated headless alignment-scene ownership boundary;
- do not promote `AlignmentQuery` or the mutable current row scene into the new authority;
- split source, payload, summary, layout, style, admission, and camera contracts;
- add source-position record IDs before canonical chunking;
- use coverage certificates and deterministic cross-tile ownership;
- use immutable current/candidate roots and panel-frame publication;
- use one source owner with latest-wins/fair scheduling;
- replace advisory cache totals with allocation IDs, reservations, and leases;
- migrate all alignment-derived evidence and product consumers together;
- implement a CPU reference projector before treating the GPU as qualified;
- prohibit per-frame renderer/picker fallback; and
- retain a headless comparison harness and manual user visual acceptance.

Adjusted feedback:

- A temporary backend flag is not part of the normal application plan. New rendering is exercised
  in the headless/native harness until complete, then alignment cutover is exclusive.
- Interaction does not need to wait for a redundant exact viewport query when the camera remains
  inside a complete immutable scene. Pointer-drag click suppression is an input rule, not an
  evidence limitation.
- Tile and row-block sizes, prefetch distances, and numeric frame targets are determined through
  measurements rather than fixed by this document.

## Definition of done

The work is complete only when:

- one immutable alignment scene graph is the sole authority for every alignment-derived lane and
  product consumer;
- complete evidence and true duplicate multiplicity match independent indexed queries;
- pan, vertical scroll, and compatible zoom inside prepared coverage are camera operations rather
  than query/layout/reupload operations;
- no full-white replacement, texture substitution, release flicker, missing-read zoom return,
  hover regression, or selection regression remains;
- CPU and GPU consume the same scene and stable handles;
- current/candidate publication is atomic and stale work cannot commit;
- cache settings enforce allocation-level bounds and reach a stable high-water mark;
- all existing alignment modes pass the capability matrix;
- local and remote workloads pass correctness and resource qualification;
- all superseded alignment query/presentation/picking/preview paths in the deletion map are gone;
- the user has manually accepted the key native interactions; and
- no production preference, hidden cap, or silent fallback can restore the previous hybrid
  architecture.
