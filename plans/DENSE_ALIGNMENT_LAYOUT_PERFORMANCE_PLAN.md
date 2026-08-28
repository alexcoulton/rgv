# Dense-alignment layout performance plan

Status: implemented and qualified

Plan date: 2026-08-28

Implementation date: 2026-08-28

Depends on:

- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md)

## Objective

Make exact, Complete read views at genuinely high-depth loci responsive without sampling reads,
changing the read-count semantics, or sacrificing stable row identity across navigation.

The immediate incident is the RNA-seq ACTB locus. Reading the BAM is fast enough; stable row
reconciliation is not. The current reconciliation algorithm searches occupied rows linearly for
every newly discovered read. At a dense clique, where nearly every read overlaps every other read,
the `k`th read rejects approximately `k - 1` rows before allocating another. That makes the path
quadratic and produces multi-second or multi-minute stalls for data that the indexed BAM reader
returns in well under a second.

This plan addresses that algorithmic failure first, then removes secondary synchronous and render
costs so the same issue does not reappear at a higher read limit.

## Measured baseline

The measurements below are the regression fixture for this work, not estimates of sequencing
coverage from a sampled display:

| Measurement | Observed result |
| --- | ---: |
| Exact visible interval | `chr7:5,528,398-5,528,400` (half-open) |
| Eligible alignment records in that interval | 47,839 |
| Alignments at coordinate 5,528,400 | 47,262 |
| Displayed count-once coverage near the coordinate | approximately 36,321 |
| Canonical layout horizon | `chr7:5,472,257-5,570,560` |
| Independent raw records in the horizon | approximately 703,611 |
| Warm indexed BAM iteration, visible interval | approximately 77 ms |
| Warm indexed BAM iteration, 98,304 bp horizon | approximately 244-258 ms |
| RGV reconciliation, 72,088 eligible foreground records | approximately 47.3 s |

The latest diagnostics reported 46,319 canonical rows, 52,170 stable rows, a truncated horizon,
and a cache hit. Earlier 47,839-record and later 72,088-record settles scale close to `n^2`, which
matches the implementation in
`crates/rgv-layout/src/alignment_layout/rows.rs::reconcile_history_island`.

The foreground/horizon admission fix is a prerequisite and must remain intact: a Complete exact
foreground may render even when its wider layout-only horizon is Too dense or truncated. The
horizon must not consume the foreground limit or suppress an otherwise Complete foreground.

## Product and correctness contract

Performance changes must preserve these invariants:

1. The exact foreground is **Complete or refused**. It is never sampled to make layout cheaper.
2. The complete-read limit applies to the exact foreground query. A wider layout horizon has its
   own typed completion state and cannot refuse a Complete foreground.
3. Stable mode preserves every incumbent read's row whenever the grouping and layout policy are
   unchanged. Newly discovered reads fill compatible rows or extend the architecture; they do not
   move incumbents implicitly.
4. A truncated horizon is explicit in state and diagnostics. It yields incremental, best-known
   stable architecture, not a claim that the full canonical horizon was packed.
5. Pair representatives, template spans, group ownership, sorting, filtering, picking, selection,
   export, automation, and MCP all refer to the same committed query revision.
6. Stale or cancelled generations publish neither partial row history nor partial foreground data.
7. Coverage semantics remain unchanged. This work concerns individual-read layout and rendering,
   not the count-once aggregate policy.

This narrows and supersedes the layout-horizon refusal rule in the current Complete read
presentation plan. Complete identity-bearing data remains mandatory for any interval used to seed
history, but completion of the preferred wider horizon is no longer a condition for showing a
Complete exact foreground.

## Target architecture

The layout path should have five distinct stages:

1. **Admit the exact foreground.** Query and count only the exact viewport for individual-read
   admission. Return Complete or a typed foreground Too-dense result.
2. **Acquire optional layout context.** Reuse a complete canonical horizon when available. If the
   preferred horizon is Too dense, use a complete deterministic fallback or the Complete exact
   foreground and mark the horizon truncated.
3. **Reconcile row identity efficiently.** Retain known assignments by identity, then place only
   new representatives with an indexed allocator. Do not scan every historical row per read.
4. **Publish transactionally.** Build a candidate history/layout for the current generation, check
   cancellation, and atomically commit it with the corresponding foreground query.
5. **Materialise visible geometry.** Keep assignment metadata for every admitted read, but create
   drawing and picking primitives only for rows intersecting the vertically visible range plus a
   small overscan.

The minimum unavoidable work is linear in the number of admitted read representatives. The target
is `O(n log r)` for general interval packing, where `r` is the number of rows, with an `O(n)` dense
fast path. No supported path may degrade to `O(n * r)` at a dense clique.

## Algorithm and data structures

### 1. Keep the efficient scratch sweep

The coordinate-monotonic scratch allocator already uses:

- a min-heap of active rows ordered by end coordinate; and
- a min-heap of available row IDs to choose the visually highest reusable row.

Retain this deterministic top-first sweep for a new complete island. Benchmark it independently so
future changes cannot replace it with the general linear row scan used by non-monotonic input.
Sort representatives once, by the existing canonical stable key, when input is not already
monotonic; do not silently fall back to `assign_rows_top_first_general` for large batches.

### 2. Add a dense-clique fast path

During an ordered sweep, track the minimum end among active rows. If no active row has ended before
the next read starts, there is no reusable active row. Allocate the next row directly without
examining existing rows or their interval vectors.

This is the dominant ACTB case. Once sorted, the allocator should append one row per overlapping
representative in linear time.

### 3. Replace historical row scanning with indexed reconciliation

Introduce a per-group reconciliation index built once for the candidate island:

- an identity-to-assignment map for incumbent rows;
- row ownership and group bounds;
- active rows ordered by the end of their latest relevant interval;
- available compatible row IDs ordered top-first; and
- an append cursor for allocating a new row when no compatible incumbent row is available.

For coordinate-ordered new reads, release rows whose last conflicting interval has ended into the
available-row heap, select the smallest compatible row, or append. Where historical rows contain
multiple disjoint intervals, build a gap/event index once per reconciliation rather than invoking
`history_interval_fits` against row vectors repeatedly. Preserve top-first behaviour by ordering
compatible candidates by row ID.

The first implementation should optimize the monotonic, same-generation extension path and use a
correct indexed gap representation for interleaved history. Do not introduce a bespoke interval
tree until benchmarks show the gap/event sweep is insufficient.

### 4. Avoid repeated interval-vector insertion

The current code uses `partition_point` followed by `Vec::insert` for every new row interval. That
can add repeated memory shifts even after row selection is fixed.

Accumulate new assignments in coordinate order and merge them with each row's immutable historical
intervals once at the end of reconciliation. Replace a row's interval vector only when committing
the candidate island. This also makes cancellation transactional and simplifies collision checks.

### 5. Make canonical-comparison metrics optional work

`alignment_row_organization_metrics` currently computes a fresh scratch layout and identity map
after stable reconciliation. Keep correctness counters available in diagnostics and tests, but:

- reuse artifacts already produced by the reconciliation when possible;
- do not recompute the full canonical comparison on every interactive frame; and
- run expensive diagnostic-only comparisons after the display layout is available, or behind an
  explicit diagnostics/profiling switch.

Metrics must observe the same committed generation and must not delay presentation.

## Scheduling, cancellation, and commit

Even a near-linear 50,000-read layout should not block input handling.

- Run query normalization and row reconciliation on the existing bounded worker path, not the UI
  event/render loop.
- Carry the navigation/query generation through admission, reconciliation, metric computation, and
  primitive generation.
- Check cancellation between groups and in bounded chunks during large sweeps.
- Construct row-history changes as a candidate delta or cloned island and commit only after the
  current-generation check succeeds.
- Drop obsolete candidates promptly when zooming or panning starts a newer request.
- Coalesce duplicate requests for the same source, filters, interval, layout policy, and horizon
  generation.
- Keep the previous committed image as a non-interactive navigation preview if that feature is
  enabled; never expose stale read picking while new layout work runs.

Cancellation is a responsiveness requirement, not merely cleanup: zooming away from ACTB must not
leave a 50,000-read reconciliation consuming the worker while the current locus waits.

## Vertical virtualization

At a dense clique the necessary row count is close to the read count, but only a small fraction of
those rows can be visible in the track at once. Row assignment and primitive creation should
therefore be separate.

- Compute stable row IDs for the whole Complete foreground so scrolling extent and row identity
  are correct.
- Materialise glyph, label, mismatch, mate-link, and picking geometry only for the visible row
  range plus overscan.
- Retain lightweight identity/span/row metadata for off-screen reads.
- Rematerialise newly visible rows when vertical scrolling changes, without querying the BAM or
  repacking rows.
- Ensure a selected or promoted read can be brought into view and inspected without globally
  materialising every row.

Virtualization does not reduce the number of admitted reads and is not sampling. It reduces CPU,
allocation, picking-index size, and GPU upload for geometry the user cannot currently see.

## Memory and cache policy

- Continue enforcing the existing 64 MiB row-history budget.
- Store only identity, group, genomic span, row, revision, and compact occupancy metadata in row
  history; do not retain BAM payloads or render primitives there.
- Account for every new reconciliation index and candidate delta in diagnostics.
- Evict complete islands atomically under the existing deterministic policy. Never trim an island
  into an undisclosed partial history.
- Reuse a compatible complete/truncated horizon generation across narrow pans and zooms; invalidate
  on source, filter, grouping, sorting, pair-mode, or layout-policy changes.
- Prove with a repeated navigation loop that history and candidate allocations return to a bounded
  steady state.

## Diagnostics and benchmarks

Add separate timings and counts so regression reports identify the failing stage:

- foreground query/count and normalization;
- horizon query and whether it was Complete, fallback, truncated, or a cache hit;
- history-index build;
- incumbent identity projection;
- new-read row allocation;
- candidate-history merge and commit;
- canonical diagnostic comparison;
- visible primitive generation and GPU upload;
- representatives considered, incumbent/new assignments, compatibility probes, rows appended,
  rows reused, cancellation generation, and peak candidate bytes.

Add criterion-style layout microbenchmarks and a desktop end-to-end trace. Include a counter for
row compatibility probes: the 50,000-read dense fixture must be on the order of the record count,
not billions.

## Implementation phases

### Phase 0: Lock the regression and expose the stages

1. Add deterministic synthetic fixtures for dense cliques, sparse reads, reusable gaps, interleaved
   historical intervals, pairs, and multiple groups.
2. Add microbenchmarks at 1,000, 10,000, 25,000, 50,000, and 100,000 representatives.
3. Record an end-to-end ACTB trace from query start through committed render.
4. Add the stage timings and compatibility-probe counters described above.

Exit criterion: the current quadratic curve is reproducible without the desktop, and diagnostics
attribute the ACTB stall to reconciliation rather than BAM I/O.

### Phase 1: Remove the quadratic dense path

1. Refactor the monotonic allocator into a reusable component shared by scratch and history paths.
2. Add the dense-clique direct-append path.
3. Reconcile the truncated/empty-horizon foreground with the indexed sweep rather than scanning
   `island.row_groups` from row zero for every read.
4. Sort once rather than use the general per-row scanner for large non-monotonic batches.
5. Preserve all stable identity, pair, grouping, and top-first invariants in unit tests.

Exit criterion: doubling a dense fixture from 25,000 to 50,000 records costs no more than 2.5x at
the allocator p95, with compatibility probes bounded by a small constant multiple of records.

### Phase 2: General indexed history extension

1. Build the per-group occupancy/gap index once per candidate history island.
2. Reuse topmost compatible historical rows without moving incumbents.
3. Batch-merge new intervals at commit instead of repeatedly inserting into row vectors.
4. Cover left, right, and bridging extensions, disconnected islands, restored identities, widened
   pair spans, and group-order changes.
5. Keep candidate mutation transactional under cancellation and budget eviction.

Exit criterion: all stable-row regressions pass and no tested extension pattern performs a full
row scan per new read.

### Phase 3: Decouple interaction from layout completion

1. Confirm reconciliation and diagnostics run entirely off the UI event loop.
2. Add bounded cancellation checkpoints and stale-generation discard.
3. Publish the Complete foreground, row layout, selection index, and history delta atomically.
4. Defer diagnostic-only canonical comparisons from the critical presentation path.

Exit criterion: navigation remains responsive and a newer locus can commit while obsolete ACTB
work is cancelled; no stale generation changes row history or picking.

### Phase 4: Virtualize dense row rendering

1. Split lightweight row assignment from render/picking primitive materialisation.
2. Add visible-row range plus overscan to the alignment display contract.
3. Cache per-row lightweight geometry inputs and invalidate them by committed revision.
4. Verify vertical scrolling, selection promotion, paired reads, mismatches, labels, and sashimi
   overlays across the virtual boundary.

Exit criterion: primitive count and picking-index size scale with visible rows, not total assigned
rows, while scrolling does not repack or re-query reads.

### Phase 5: Qualify and document

1. Run layout, engine, desktop, automation, and MCP test suites plus formatting and lint checks.
2. Re-run the ACTB trace in a release build with cold and warm OS caches.
3. Exercise local BAM and remote range-backed BAM paths to prove the optimization is source-neutral.
4. Record screenshots and structured diagnostics at the exact two-base ACTB interval and during a
   zoom-away cancellation.
5. Amend the Complete read presentation plan and diagnostics documentation with the final
   foreground-versus-horizon contract and measured results.

Exit criterion: the acceptance criteria below are met and the trace contains no unexplained
synchronous stall.

## Test matrix

### Layout correctness

- Empty, one-read, and equal-start fixtures.
- Dense cliques where every representative overlaps every other.
- Sparse ordered reads that all reuse row zero.
- Partially overlapping reads with repeated row release and reuse.
- Non-monotonic inputs producing the same canonical result as sorted inputs.
- Left, right, and bridging horizon extensions.
- Existing rows with multiple disjoint occupied intervals.
- Reads disappearing and later returning with the same identity.
- Paired reads, widened template spans, supplementary/secondary filtering, and duplicate names.
- All supported sort, group, group-order, and pair display modes.
- Stable shared rows across broad/narrow/broad navigation.

### Admission and state

- Exact foreground below, at, and above the Complete read limit.
- Complete foreground plus Too-dense preferred horizon.
- Complete foreground plus complete smaller deterministic fallback.
- Complete foreground seeded only from itself with `horizon_truncated = true`.
- Foreground Too dense with complete aggregates but no individual-read surfaces.
- Cancellation before and after candidate reconciliation, including immediate zoom-away.
- Cache hit, cache miss, policy invalidation, and history-budget eviction.

### Rendering and interaction

- Virtual row ranges at top, middle, and bottom of a 50,000-row clique.
- Scroll extent and row positions unchanged as primitives enter and leave overscan.
- Picking never resolves an off-screen or stale primitive.
- Selection and inspection can reveal an off-screen selected read.
- Coverage, splice junctions, mismatches, labels, mate links, and sashimi overlays remain correct.
- Desktop, automation, and MCP report the same committed counts and generation.

## Performance gates

Use a named reference Mac and record CPU, release profile, cache state, fixture checksum, and build
revision with each result. Initial release gates are:

- 50,000-read dense-clique row allocation completes in at most 250 ms at p95 on the reference
  machine after fixture setup.
- 25,000 to 50,000 and 50,000 to 100,000 allocator scaling remains at or below 2.5x at p95.
- Compatibility probes remain `O(n)` for a clique and `O(n log r)` work is retained for the general
  ordered case; no benchmarked path performs `O(n * r)` row scans.
- Warm-cache ACTB reconciliation of the exact 47,839-record foreground completes in at most 1 s,
  excluding BAM query time and diagnostic-only deferred work.
- No reconciliation task holds the UI event loop for more than one frame, and a superseding
  navigation request becomes actionable without waiting for obsolete layout completion.
- Visible primitive and picking counts remain bounded by the virtual row range plus overscan.
- Repeated ACTB zoom/pan/cancel loops remain within the 64 MiB history budget and show no monotonic
  heap growth attributable to layout candidates.

If the reference machine cannot meet a time gate after the algorithmic work, document the measured
constant cost and revise the numeric gate before merging. The complexity, correctness,
cancellation, and bounded-memory gates are not negotiable.

## Acceptance criteria

This plan is complete when:

- the exact ACTB foreground is still reported as Complete and its legitimate read count is not
  relabelled as a counting error;
- a Too-dense or truncated wider horizon no longer suppresses that Complete foreground;
- dense reconciliation has no row-by-row linear scan and benchmark scaling rejects quadratic
  regressions;
- incumbent rows remain stable and collision-free through the full navigation test matrix;
- obsolete layout work cancels without committing history or blocking the current request;
- only vertically visible rows create expensive render and picking primitives;
- diagnostics isolate I/O, reconciliation, metrics, and rendering costs; and
- the performance gates, workspace tests, release trace, and native visual checks pass.

## Implementation and qualification result

The implementation replaces the dense historical row scan with an indexed allocator, preserves
the monotonic scratch sweep as the empty/truncated-horizon fast path, batch-merges history
intervals, and exposes per-stage reconciliation timings. Layout work now runs on the existing
bounded scheduler with generation cancellation and transactional candidate-history publication.
Only the visible vertical row window plus 32 rows of overscan creates read and picking geometry;
vertical scrolling rematerialises geometry without re-querying or repacking.

The release benchmark report is
[`benchmark-results/dense-layout-actb-warm-2026-08-28.json`](../benchmark-results/dense-layout-actb-warm-2026-08-28.json).
It was recorded on a 10-core Apple M4 MacBook Pro with 16 GiB RAM, macOS 15.5, and rustc 1.93.1,
against base revision `1f8ffdd` plus the working implementation. The public RNA-seq fixture was
`ENCFF686MTA.bam` with BAM SHA-256
`6be96343d8d90a2b09f9654d3f6e63e24846bef8ab0edb8b76c52fe3e83f1b9e` and BAI SHA-256
`e0a9fc69189acfeaecd0d1eafa60fe05bcc8a2d335bc44164f1ebae11c54da7b`.

| Release gate | Measured result |
| --- | ---: |
| 50,000-read dense allocation p95 | 11.136 ms |
| 25,000 to 50,000 allocation scaling | 2.05x |
| 50,000 to 100,000 allocation scaling | 2.04x |
| 100,000-read dense allocation p95 | 22.719 ms |
| Dense compatibility probes | 0 |
| Exact ACTB eligible records | 47,839, Complete |
| Exact ACTB indexed query | 44.193 ms |
| Preferred 98,304 bp horizon | 703,611 eligible, Too dense, explicitly truncated |
| ACTB reconciliation p95 | 49.237 ms |
| ACTB allocation p95 | 15.260 ms |

The approximately 47.3-second baseline reconciliation fell to 49.237 ms p95, roughly a 960x
improvement. Synthetic dense allocation remained close to linear through 100,000 records. The
benchmark separately records BAM query time, so the allocator result is independent of the OS file
cache; the stored end-to-end ACTB query trace is a warm-cache run.

Native MCP diagnostics on the same release build confirmed both ends of the presentation policy.
At the dense reads view RGV packed 80,308 stable rows from 236,213 Complete records while
materialising only 1,052 read items. At Regional detail it materialised no read rows and displayed
the indexed-contig alignment count with the instruction to zoom in. The native capture is
[`benchmark-results/dense-layout-actb-native.png`](../benchmark-results/dense-layout-actb-native.png).

Qualification completed with `cargo fmt --all -- --check`, `cargo check --all-targets`,
`cargo test --workspace --all-targets --no-fail-fast`, and release builds of `rgv-desktop` and
`rgv-mcp`. The workspace run included the local HTTP range fixtures; reconciliation consumes the
same source-neutral `AlignmentQuery` after local or remote admission. A full-workspace
`clippy -D warnings` remains blocked by pre-existing landmark/session lint debt outside this
performance change; formatting, compilation, and all executable tests passed.

## Out of scope

- Raising the default Complete read limit to hide performance problems.
- Sampling, prioritising, or silently dropping individual reads.
- Changing count-once coverage or splice-junction aggregation semantics.
- Redesigning BAM/CRAM indexing or remote range transport.
- Moving row allocation to the GPU before the CPU algorithm and scheduling are corrected.
- Automatically compacting stable rows in a way that moves incumbents.
