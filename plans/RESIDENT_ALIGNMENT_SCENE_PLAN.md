# Resident alignment scene and instant navigation plan

Status: in progress

Plan date: 2026-08-29

Implementation checkpoint: 2026-08-29

- immutable Arc-backed alignment storage, stable record IDs, and zero-copy contained selections
  are implemented;
- canonical exact coverage/junction summary tiles are resident and observable, with indexed
  splice, pair, and modification paths avoiding complete selected-read scans where possible;
- persistent row scenes support physical reuse, learned storage mappings, and incremental default
  policy edge extension;
- decoded resident windows and row-layout horizons are now separate contracts: a wide speculative
  buffer supplies immutable reads and exact indexes, while row scenes are built only for the
  canonical horizon and explicit horizontal scale class needed by the presentation;
- exact camera views remain lightweight shared cache projections and no longer displace a wider
  compatible decoded resident window during contained navigation;
- same-scale scene extension ignores inferred template-length bounds unless grouping or sorting
  actually depends on them, and the canonical-preserving left-edge path rejects any extension that
  would require recolouring incumbent rows;
- scene-backed rendering now resolves query-local records through the scene's physical-row
  projection in both read and modification materialisation rather than mixing index spaces;
- settled browsing buffers are prepared into resident row scenes on a bounded background scheduler,
  so a subsequent contained zoom can reuse row placement rather than rebuilding it in the gesture;
- visible-row rendering uses a persistent inverted row index instead of scanning every resident
  alignment;
- foreground row preparation and secondary rendering now share the configured Complete-read
  capacity contract, while a resident scene reports its actual row capacity rather than turning a
  ten-million-read admission ceiling into a fictitious ten-million-row canvas;
- exact-camera GPU slabs and render-list reuse are implemented as the migration step toward fully
  genomic GPU slabs;
- the Total cache setting is live and coordinated from measured decoded/layout/row/GPU residency,
  with adaptive per-track borrowing and Settings/Diagnostics telemetry;
- phase-specific query status, buffer margins, clamp reasons, row-scene paths, and GPU-slab reuse
  are exposed to automation;
- the native ACTB clean-fine and broad-to-fine qualifications now agree on 692,656 complete reads,
  a 703,611-record canonical horizon, scale class, row-policy and identity-to-row fingerprints,
  all row/render counts, and a byte-identical PNG;
- the native same-class ACTB edge extension now produces the same canonical identity-to-row
  fingerprint and byte-identical PNG as a clean broad build, with row-scene projection measured at
  approximately 33-46 ms instead of the previous approximately 1.8 s row rebuild; and
- occluded native automation now submits the same authoritative frame to a private offscreen
  target, so navigation commits cannot deadlock merely because the qualification window is hidden;
  interactive occlusion behaviour remains unchanged; and
- native Metal qualification, fully transform-only novel cameras, precise deduplicated cache
  accounting, advanced-policy incremental row extension, and the complete remote/workload matrix
  remain open. These headless results are implementation evidence, not final qualification.

Supersedes the end-to-end latency assumptions in:

- [Alignment browsing working set](ALIGNMENT_BROWSING_WORKING_SET_PLAN.md)

Depends on:

- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Dense-alignment layout performance](DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md)
- [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)

## Objective

Make a pan or zoom wholly contained by an admitted alignment working set behave like moving a
camera over an already-built scene. A contained navigation must not reread the BAM/CRAM, repack a
viewport-sized alignment batch, rescan every selected read to rebuild summaries, reconstruct all
read rows, or re-upload unchanged read geometry.

The alignment data path should become:

```text
indexed BAM/CRAM
      ↓ missing-slab acquisition and decoding
immutable resident read chunks
      ├── overlap and identity indexes
      ├── exact multiresolution summary tiles
      ├── persistent scale-class row scenes
      └── persistent GPU instance slabs
                         ↓
             viewport range + scale + scroll
```

The viewport is a lightweight lens over reusable source, evidence, layout, and GPU state. It is
not itself the owner of a newly constructed read population.

## User-visible outcome

When the target is covered by a ready resident scene:

- wheel zoom and small pans update on the next interactive frame;
- reads already on either side appear without a loading state;
- exact coverage and splice-junction information comes from reusable tiles;
- stable row identity, selection, picking, and inspection survive the movement;
- Diagnostics reports a resident-scene hit and zero alignment source queries; and
- changing the cache or browsing-buffer settings has an observable, truthful effect.

When the target is not covered, RGV must distinguish the work it is doing:

- **Fetching reads** for BAM/CRAM I/O and decoding;
- **Extending browsing buffer** for speculative missing-slab acquisition;
- **Building summaries** for exact derived evidence;
- **Preparing read layout** for a missing scale class; and
- **Ready** when the target is fully interactive and authoritative.

The generic loading presentation must not imply that cached reads are being reread from disk when
only a derived layer is settling.

## Current measured baseline

The ACTB RNA-seq qualification locus is
`chr7:5,460,671-5,702,331` in `ENCFF686MTA.bam`.

The existing working-set implementation proves that decoded containment is correct, but not that
navigation is immediate:

| Path | Foreground work | Layout work | Approximate visible cost |
| --- | ---: | ---: | ---: |
| Cold indexed source query | 1,458–1,616 ms | 284–293 ms | 1.7–2.0 s |
| Resident containment | 230 ms | 281 ms | about 0.5 s |
| Resident extension at the broad view | 824 ms | 293 ms | about 1.1 s |
| Exact broad resident return | 0.015 ms | 330 ms | about 0.33 s |

For one contained ACTB target, resident derivation itself split into approximately:

- overlap lookup: 2.5 ms;
- packed record projection: 77 ms;
- exact summary reconstruction: 146 ms; and
- complete read-row layout: 281 ms.

The default 512 MiB total cache also allocates only 320 MiB to decoded alignments through a fixed
5/8 split. The broad ACTB resident occupies approximately 316.5 MiB, so a spatial buffer request
can be almost entirely clamped even when the Complete read limit is ten million. This plan must
remove both the reconstruction cost and the opaque resource behaviour.

## Diagnosis

The current architecture binds four lifetimes to the exact viewport interval:

1. `AlignmentQuery` owns a packed `AlignmentBatch` for that target.
2. Target statistics, pileup, junction, pair, and modification summaries are rebuilt from the
   target population.
3. Layout is rebuilt for a target or canonical layout horizon.
4. Render items and GPU uploads are regenerated from the new layout.

The working-set layer can avoid source I/O, but its contained path still selects resident physical
indices, copies those records into another packed batch, reconstructs exact summaries, lays the
reads out again, and creates new render data. Raising the Complete read limit or the browsing
buffer cannot eliminate those costs.

Caching every exact viewport is not the solution. It would have poor hit rates during continuous
wheel zoom, duplicate most of the same records and summaries, and make memory scale with gesture
history. The reusable cache units must instead be canonical chunks, genomic tiles, layout scale
classes, and GPU slabs.

## Product and correctness contract

### Complete evidence

A resident scene may authoritatively serve a target only if:

- its genomic coverage completely contains the target;
- its source fingerprint, reference identity, filters, secondary/supplementary policy, auxiliary
  detail, and Complete read-limit contract match;
- every intersecting record can be enumerated without sampling;
- every displayed summary is exact for its declared genomic granularity;
- generation and cancellation checks prevent stale target state from committing; and
- reads, coverage, junctions, picking, inspection, selection, export, and automation all refer to
  the same evidence revision.

If only part of a target is resident, RGV may draw the old committed frame or a clearly marked,
non-authoritative preview. It must not present the resident subset as a complete target.

### Stable identity

One alignment must retain the same stable identity across:

- chunk extension and deduplication;
- viewport selections;
- layout scale classes;
- GPU instance slabs;
- picking and inspection;
- exact and coarser summary tiles; and
- session selection restoration where the source fingerprint still matches.

### Bounded resources

- The user-selected Total cache budget is the process cache ceiling, excluding explicitly reported
  unavoidable foreground and driver overhead.
- Resident reads, overlap indexes, summary tiles, layout scenes, and GPU slabs all report their
  measured weights to one adaptive cache coordinator.
- The coordinator may borrow unused capacity between tiers rather than enforcing a fixed 5/8
  decoded-alignment partition.
- Current authoritative evidence is pinned until its replacement commits.
- Speculative margins and derived scale classes are evicted before current evidence.
- Repeated pan/zoom/return cycles reach a stable memory high-water mark.

## Target data model

Exact names may change during implementation, but the lifetime boundaries must remain.

```rust
struct AlignmentResidentScene {
    contract: AlignmentSceneContract,
    interval: GenomicInterval,
    chunks: Vec<Arc<AlignmentReadChunk>>,
    overlap_index: AlignmentSceneOverlapIndex,
    summary_pyramid: AlignmentSummaryPyramid,
    layout_scenes: LayoutScaleClassCache,
    gpu_slabs: GpuAlignmentSlabCache,
    revision: u64,
}

struct AlignmentReadChunk {
    interval: GenomicInterval,
    records: AlignmentBatch,
    stable_ids: Box<[AlignmentStableId]>,
    maximum_end_index: AlignmentOverlapIndex,
    estimated_bytes: usize,
}

struct AlignmentViewportSelection {
    scene: Arc<AlignmentResidentScene>,
    interval: GenomicInterval,
    spans: SmallVec<[ChunkSelectionSpan; 8]>,
    evidence_revision: u64,
}
```

`AlignmentViewportSelection` must refer to chunk-local physical ranges or compact index spans. It
must not copy sequence, qualities, CIGAR operations, names, or auxiliary fields. A visible or
selected record is addressed by stable ID and `(chunk, physical_index)`.

The resident scene must support transactional extension. Missing left and right slabs are decoded
into new chunks, deduplicated by stable alignment identity, indexed, summarized, laid out, and
admitted before the scene revision changes. Failure or cancellation leaves the prior revision
intact.

## Workstream 0: instrumentation and reproducible baseline

Before changing ownership, add end-to-end navigation spans for:

1. input event to viewport generation;
2. alignment source wait and decoded bytes;
3. resident overlap lookup;
4. packed record copying;
5. each summary family;
6. row assignment and visible-row materialization;
7. CPU render-list construction;
8. GPU allocation/upload;
9. first submitted frame; and
10. first presented authoritative frame.

Expose these spans through Diagnostics and deterministic automation. Record cache outcome, source
query count, resident revision, summary-tile hits, layout-scene hits, GPU-slab reuse, requested and
effective buffer margins, cache allocation by tier, and the exact reason for any clamp.

Create a native ACTB gesture benchmark that performs real wheel-like zoom sequences rather than
only isolated engine calls. Preserve reports for 512 MiB, 2 GiB, and 4 GiB total budgets with 0×,
1×, and 4× requested browsing margins.

Exit gate:

- the current approximately 2 s, 1.1 s, 0.5 s, and 0.33 s paths can be identified from one report;
- UI loading text maps to the measured active phase; and
- no optimization phase starts without a before/after gesture trace.

## Workstream 1: immutable chunks and zero-copy viewport selections

### Read access

Complete the migration from public packed vectors to `AlignmentRecordView` and chunk-aware read
accessors. All consumers needed by display, picking, inspection, summaries, export, and automation
must accept views or stable handles.

### Chunk store

- Store decoded records in immutable, coordinate-sorted chunks.
- Choose chunk boundaries that support missing-slab extension without duplicating a whole scene.
- Maintain block-maximum overlap indexes so long and spliced reads beginning before a target are
  still selected.
- Deduplicate boundary records using full stable alignment identity, not byte equality alone.
- Preserve compact filtered-record provenance needed for exact target counters.

### Viewport selection

- Return chunk spans or compact physical-index lists from containment lookup.
- Teach query consumers to operate on `AlignmentViewportSelection`.
- Keep materialization only at explicit compatibility boundaries during migration.
- Remove target-sized `AlignmentBatch::push_view` copying from the normal resident hit path.

Exit gate:

- resident containment produces the same ordered stable IDs and exact filtered counters as an
  independent indexed source query;
- no sequence, quality, CIGAR, read-name, or auxiliary arenas are copied for the target;
- ACTB target projection falls below 10 ms p95; and
- 100 contained zoom cycles do not increase resident bytes.

## Workstream 2: exact tiled summary pyramid

Build canonical genomic tiles independently of viewport history.

### Summary families

- reference and allele coverage;
- mapping/base-quality-weighted coverage;
- insertions, deletions, skips, and mismatch counts;
- splice-junction counts and provenance;
- modification counts and probabilities;
- filtered and eligible record counters; and
- pair/template summaries where the selected display requires them.

### Tiling rules

- Key tiles by source fingerprint, scene contract, contig, tile coordinate, resolution, and
  reference identity where applicable.
- Assign junctions and pairs to deterministic ownership coordinates so adjacent tiles cannot
  double count them.
- Split coverage contributions across every touched tile while counting the record itself only
  under its declared ownership rule.
- Combine complete interior tiles directly.
- Resolve arbitrary partial edge tiles from finer cached tiles or the resident read index.
- Store coarse levels derived from exact child tiles; never derive an exact fine view from a
  lossy parent.
- Extend and invalidate tiles transactionally with scene revisions.

Exit gate:

- tiled results are property-tested against fresh full scans across arbitrary intervals, tile
  boundaries, long CIGAR operations, splices, duplicates, filtered reads, and contig edges;
- contained navigation performs no full selected-read summary scan when required tiles are ready;
- ACTB summary assembly falls below 10 ms p95 for ready targets; and
- summary memory remains bounded and independently observable.

## Workstream 3: persistent scale-class row scenes

Row assignment must become a reusable property of a resident scene rather than a disposable
viewport product.

### Scale classes

Layout collision rules can depend on bases per pixel, minimum rendered width, labels, pair mode,
density, grouping, and sort settings. Quantize those inputs into explicit layout scale classes.
Reuse one class while its collision contract remains valid; build another class only when crossing
a real presentation boundary.

### Incremental layout

- Assign stable row identities within each scale class.
- Keep an interval index from genomic spans to rows and instances.
- On pan or contained zoom within a class, select and clip existing rows without repacking them.
- On scene extension, lay out only new edge chunks and reconcile a bounded overlap horizon with the
  existing boundary.
- Preserve selected-to-top and paired-read policies without changing stable alignment identity.
- Evict inactive scale classes under the adaptive cache budget.

### Visible materialization

Vertical scrolling should select visible row ranges from the persistent scene. It must not trigger
row assignment. Horizontal pan and zoom should update genomic clipping and transforms only.

Exit gate:

- a contained pan/zoom within a ready scale class performs no complete row repack;
- row stability, grouping, sorting, paired layout, selection, and picking match current behaviour;
- visible-row selection and clipping fall below 10 ms p95 at ACTB; and
- incremental extension produces the same canonical row result as a clean scene build.

## Workstream 4: persistent GPU alignment slabs

Represent resident read geometry in reusable GPU allocations.

- Allocate instance slabs per immutable read/layout chunk.
- Retain genomic coordinates, row identity, style inputs, and stable picking IDs on the GPU.
- Update viewport origin, scale, clipping, and vertical row range through uniforms or compact
  indirect-draw metadata.
- Draw only intersecting genomic chunks and visible row ranges.
- Upload only new or invalidated slabs when the resident scene extends or a presentation contract
  changes.
- Keep selection and hover overlays separate so they do not invalidate base read geometry.
- Account for GPU slabs in the same adaptive cache coordinator.

Exit gate:

- ordinary contained pan/zoom uploads no unchanged read geometry;
- no GPU buffer reallocation occurs during a 100-step contained navigation loop;
- picking IDs remain stable across camera changes; and
- GPU preparation plus submission remains below the interactive frame budget.

## Workstream 5: adaptive, live cache budgeting

Replace fixed tier partitioning with one live coordinator.

### User controls

Application Settings → Performance must expose:

- **Total cache budget**, applied live where safe;
- **Read browsing buffer**, as the desired margin per side;
- current effective resident margin;
- total and per-tier resident memory;
- active prefetch progress; and
- any clamp reason, with the amount requested and admitted.

Settings remain persisted across ordinary application runs. Automation and CLI launch options must
support reproducible overrides without replacing the normal persisted profile.

### Allocation policy

- Pin current authoritative source, summary, layout, and GPU state.
- Allow decoded reads to borrow capacity unused by remote blocks, summaries, layouts, or GPU slabs.
- Reclaim speculative margins first, followed by inactive scale classes, old summary levels, and
  least-recent exact targets.
- Apply increases immediately to subsequent extension.
- On decreases, cancel incompatible speculation and trim unpinned state incrementally without
  invalidating the committed viewport.
- Divide capacity fairly across tracks and panels using observed active demand rather than an
  irrevocable equal split.

Exit gate:

- changing 512 MiB to 2 GiB changes the observable resident allowance without restart;
- a 4× request shows whether 4× was achieved and why not if it was clamped;
- total measured cache weight remains at or below the selected budget after settlement; and
- multi-track pressure cannot evict current authoritative evidence or starve foreground work.

## Workstream 6: navigation integration and truthful presentation

Classify every target before scheduling work:

1. **Ready resident scene**: commit the new camera and reusable evidence in the next frame.
2. **Resident reads, derived layer missing**: show resident reads immediately, label only the
   missing derived layer as preparing, and finish it under the current generation.
3. **Partially resident target**: retain the committed frame or show a clearly bounded
   non-authoritative preview while missing slabs are fetched.
4. **No compatible resident scene**: perform a foreground source query with explicit Fetching
   reads status.
5. **Too dense/resource refused**: preserve exact aggregate evidence and report the controlling
   limit without attempting a wasteful read-scene build.

Continuous wheel events should coalesce to the latest camera target. They must not repeatedly
cancel and restart the same resident selection, summary assembly, or layout-class build. Background
buffer extension must yield immediately to missing foreground work but should resume from admitted
chunks rather than restart from scratch.

Exit gate:

- a ready contained target never displays a generic loading state;
- loading phase text and Diagnostics agree;
- stale generations cannot replace current reads, summaries, layout, or GPU state; and
- the drag-release and retained-frame navigation guarantees continue to pass.

## Test strategy

### Correctness

- Compare stable record identities against independent BAM/CRAM queries for random contained and
  overlapping intervals.
- Property-test summary tiles across arbitrary boundaries and resolutions.
- Verify CIGAR spans, splices, long reads, pairs, modifications, secondary/supplementary reads,
  filters, missing qualities, and placed-unmapped records.
- Compare incremental row scenes with clean canonical builds.
- Verify picking, inspection, export, selection, and automation evidence revisions.
- Exercise reference-present, reference-absent, local BAM, local CRAM, remote BAM, and remote CRAM.

### Interaction matrix

- broad → narrow → exact broad return;
- narrow → small zoom-out within a ready buffer;
- repeated zoom-out steps until the buffer boundary;
- bidirectional small pans;
- sustained directional pan with edge extension;
- continuous wheel input while prefetch is active;
- change buffer and total budget during active navigation;
- multiple alignment tracks and multiple panels; and
- chromosome/overview transitions where individual reads are intentionally unavailable.

### Workloads

- dense ACTB RNA-seq;
- the downloaded exome BAM;
- a high-depth WGS locus and an ordinary-depth WGS locus;
- long-read ONT data;
- adversarial mixed fixtures; and
- a remote indexed alignment with controlled request counters.

## Performance gates

On the Apple M4 ACTB qualification system, with the target fully covered by a ready resident scene:

| Metric | Required gate |
| --- | ---: |
| Alignment source queries during contained gesture | 0 |
| Packed variable-width bytes copied | 0 |
| Full selected-read summary scans | 0 |
| Full row-layout rebuilds within a scale class | 0 |
| Unchanged GPU alignment bytes uploaded | 0 |
| Input to first updated frame, p50 | ≤ 16 ms |
| Input to first updated frame, p95 | ≤ 50 ms |
| Authoritative ready target, p95 | ≤ 50 ms |
| Exact previously visited camera return, p95 | ≤ 16 ms |
| Memory growth after 100 settled navigation cycles | 0 beyond bounded cache churn |

If an exact derived layer is absent but resident reads are ready, the read scene must still update
within 50 ms and the missing exact layer must settle within 100 ms p95 without blocking input.

Remote qualification additionally requires zero network requests for contained ready targets and
strict adherence to the speculative request/byte allowance during extension.

## Delivery sequence

Implement and review this work as independently verifiable changes:

1. navigation timing spans and phase-specific status;
2. immutable resident chunks and stable handles;
3. zero-copy viewport selection migration;
4. exact summary tile pyramid;
5. persistent scale-class row scenes;
6. persistent GPU instance slabs;
7. live adaptive cache coordinator and settings UI;
8. contained-navigation fast path;
9. local, remote, multi-track, and resource-pressure qualification; and
10. removal of superseded target-batch, full-summary, and full-layout rebuild paths.

Each change must preserve a production fallback until its replacement passes equivalence tests.
Fallback use must be observable; silent fallback is not considered qualification.

## Definition of done

This plan is complete only when:

- the ACTB interaction no longer shows a visible reload for a contained ready pan or zoom;
- a trace proves that the gesture performed no alignment source query, target batch copy, complete
  summary scan, complete row rebuild, or unchanged GPU upload;
- exact coverage, junction, read, inspection, selection, and export evidence remains equivalent to
  an independent indexed query;
- Total cache budget and Read browsing buffer persist, apply as documented, and display their
  effective result;
- local and remote resource ceilings remain bounded under sustained navigation;
- all affected tests, native Metal automation, and the workload matrix pass; and
- the qualification report contains before/after timings, cache residency, memory high-water
  marks, source request counts, and screenshots of the final loading and settings states.
