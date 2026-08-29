# Alignment browsing working-set plan

Status: implemented and locally qualified

Plan date: 2026-08-29

Implementation completed: 2026-08-29

Qualification evidence:

- [Alignment browsing working-set qualification](../benchmark-results/ALIGNMENT_BROWSING_WORKING_SET_QUALIFICATION.md)
- `working-set-actb-0.json` and `working-set-actb-100.json`
- `working-set-buffer-{0,50,100,200,400}.json`
- `alignment-working-set-native-report.json`

Depends on:

- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
- [Dense-alignment layout performance](DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md)
- [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)

## Objective

Make nearby pan and zoom operations feel like browsing an already-open dataset rather than issuing
an unrelated BAM/CRAM query for every viewport. When complete decoded reads already cover a target,
RGV should derive the target view from that resident working set. When the target approaches the
edge of resident data, RGV should extend the working set in the background under an explicit byte
budget.

The common interactions to optimize are:

1. zoom into a complete broader view and then zoom back out;
2. pan slightly left or right within recently decoded data;
3. start at a narrow view and make a small zoom-out after adjacent data has been prefetched; and
4. continue panning in one direction while RGV prepares the next margin ahead of the viewport.

This work must not weaken RGV's complete-or-refuse read contract, present broad-interval aggregate
counts as though they were exact for a narrower target, or make memory consumption unbounded.

## Measured motivating case

The local RNA-seq ACTB view provides the first qualification workload:

| Measurement | Observed result |
| --- | ---: |
| Foreground locus | `chr7:5,460,671-5,702,331` |
| Eligible and complete foreground records | 783,370 |
| Effective Complete read limit | 10,000,000 |
| Canonical layout-horizon records | 788,880 |
| Canonical layout-horizon bytes | 102,579,378 |
| Row-history bytes | 63,541,584 |
| Packed rows | 57,861 |
| Read items materialized for the visible pane | 10,816 |

The experiment demonstrates that a large complete population can remain usable because RGV keeps
decoded data column-oriented, row history compact, and render materialization vertically bounded.
It does **not** mean that ten million records should be cached automatically. The Complete read
limit is an evidence-admission ceiling; the browsing cache must be controlled independently by
measured bytes.

## Current baseline

RGV already contains four relevant mechanisms:

1. Each alignment actor owns a byte-weighted LRU of decoded `AlignmentQuery` values. Its key is the
   exact genomic interval plus the query-options fingerprint.
2. Stable layout uses a canonical 32,768-base horizon. Local sources include one tile of margin on
   each side; remote sources extend more conservatively. The horizon is normally layout-detail
   data, not a full evidentiary foreground query.
3. After repeated pans in one direction, the actor best-effort-prefetches one adjacent interval of
   the same span.
4. Transactional navigation retains the previous GPU canvas and may build a bounded,
   non-interactive horizon overlay while the exact target settles.

The default 512 MiB total cache budget allocates 320 MiB to decoded alignments before that amount is
divided across open alignment tracks. An entry larger than its track share is not admitted to the
LRU. The currently committed query remains alive in the desktop projection, but ordinary query
replacement discards that projection reference.

These mechanisms help exact revisits, stable rows, sustained directional pans, and visual
continuity. They do not provide general containment or overlap reuse:

- a zoom changes the exact interval and therefore the cache key;
- a cached superset is not projected onto a contained target;
- partially overlapping queries cannot be merged;
- the layout horizon often omits sequence, quality, auxiliary and aggregate detail required by an
  authoritative target; and
- dense horizons exceed the deliberately small provisional-preview limit.

The reported “forgotten surrounding reads” behaviour is therefore real even when some layout and
texture state remains available.

## Product contract

### Authoritative evidence

A viewport may commit a Complete read presentation from resident data only when all of the
following are true:

- the resident source is a complete decoded population for an interval containing the target;
- source, filters, include-secondary/include-supplementary policy, reference-dependent options,
  required auxiliary tags, and effective resource contract match;
- every resident record intersecting the target can be identified without sampling;
- exact target aggregates and statistics are recomputed or obtained from exact compatible tiles;
- the target's eligible record count is at or below its effective Complete read limit; and
- the derived read population, layout, picking, inspection, selection, export and automation state
  commit under one current generation.

A broad query's `records_eligible`, pileup bins, junction counts, modification summaries, pair
summaries or reference slice must never be relabelled as exact target results.

### Navigation presentation

The existing retained texture remains the immediate response to an input gesture. Resident-data
reuse accelerates the authoritative commit behind that preview; it does not turn preview pixels
into evidence.

If a resident target can be derived quickly, the transition remains preview to atomic final frame.
If projection or layout exceeds the interactive budget, work stays off the UI thread and the last
committed texture remains non-interactive until the current generation is ready.

### Bounded resources

- Cache and buffer admission is based on `estimated_bytes`, not read count alone.
- The active foreground may exceed the configured cache share because it is required for the
  current display. In that case, no additional resident ancestor or margin is retained.
- Resident ancestors, extensions, indexes and derived batches all count against the decoded
  alignment cache share.
- Multiple tracks divide the decoded budget using the existing workspace allocation policy.
- Memory pressure evicts speculative margins first, then least-recent resident ancestors. It never
  evicts the current committed foreground before replacement is ready.
- The working set reaches a bounded steady state during repeated navigation.

The existing **Total cache budget** remains the hard memory boundary. A separate application-level
**Read browsing buffer** setting controls the desired spatial reach inside that boundary. The
spatial control is a target, not a memory reservation: RGV may reduce the effective margin when
density, the Complete read limit, multi-track allocation or remote-transfer limits make the
requested reach unsafe.

### Application setting and defaults

Add **Read browsing buffer** under Application settings → Performance:

- express the value as margin on each side in multiples of the current viewport span;
- support `0×` through `4×` per side, with `1×` per side as the default;
- present useful quarter-step values while allowing direct numeric entry;
- explain that `1×` aims for a three-viewport-wide resident window: one viewport of cached margin,
  the visible viewport, and one viewport of cached margin;
- explain that actual reach is adaptive and constrained by Total cache budget;
- show that remote sources acquire the same desired reach conservatively and may stop early at
  their speculative transfer bound; and
- define `0×` as disabling speculative adjacent fetches while retaining exact-query caching and
  reuse of already-resident navigation ancestors.

Persist a bounded integer representation such as `read_browsing_buffer_percent` with range
`0..=400` and default `100`, rather than persisting a free-form floating-point value. Existing
settings files receive the default through normal backward-compatible deserialization and
validation.

Changing this setting takes effect immediately for scheduling. Increasing it allows subsequent
extensions to reach farther. Decreasing it cancels incompatible speculative work and makes excess
margins first in line for eviction; it does not invalidate the exact committed foreground. Setting
it to zero cancels pending speculative extensions and releases unneeded margins while preserving
ordinary exact cache entries.

## Target architecture

Each panel/track projection should distinguish three layers:

```text
decoded resident window or shards
            ↓ exact target derivation
current-generation foreground query and aggregates
            ↓ layout / virtualization
committed interactive display
```

The resident layer is source data, not a committed viewport. A representative model is:

```rust
struct AlignmentResidentWindow {
    contract: ResidentQueryContract,
    interval: GenomicInterval,
    records: Arc<AlignmentRecordStore>,
    overlap_index: AlignmentOverlapIndex,
    estimated_bytes: usize,
    last_used_revision: u64,
}

struct ResidentQueryContract {
    source: SourceFingerprint,
    query_options_fingerprint: u64,
    effective_complete_read_limit: usize,
    required_detail: QueryDetail,
}
```

Exact names may differ. The mandatory separation is between a broad complete decoded store and an
exact target result derived from it.

### Containment lookup

Before dispatching HTS work, the alignment actor should search compatible resident windows for the
smallest interval that contains the target. A hit avoids BAM/CRAM I/O and decompression. The actor
derives an exact `AlignmentQuery` for the target, publishes ordinary cache telemetry with a
distinct `resident_containment_hit` reason, and follows the existing generation/cancellation path.

The first useful retention rule is simple: when zooming into a complete broader query, keep that
query as the preferred navigation ancestor if it fits the remaining decoded budget. This directly
optimizes zoom-in/zoom-out without waiting for adaptive prefetch.

### Exact target derivation

`AlignmentBatch` is currently a packed column store whose fields are accessed directly throughout
HTS, layout, display and inspection code. Containment reuse needs a target projection without
materializing a heavyweight `AlignmentRecord` allocation graph for every read.

Implement this in two steps:

1. Add read-only record accessors/iterators that expose a zero-allocation `AlignmentRecordView` over
   packed columns and arenas. Migrate summary builders and relevant consumers away from direct
   public-vector assumptions where necessary.
2. Add an interval projection path backed by a compact physical-index list or a newly packed target
   batch. Start with the simpler measured implementation, then move to shared arenas/index views
   only if copying is a material latency or memory cost.

The resident store is start-sorted. Add a bounded overlap index so selecting target intersections
does not scan every earlier record merely because a long or spliced alignment may cross the target.
The index must handle long CIGAR spans correctly; a start-only binary search is insufficient for
RNA-seq.

Using the selected exact population, rebuild:

- target `QueryStats` and Complete/Too-dense state;
- pileup/allele coverage at the target binning resolution;
- junction, pair and modification summaries when enabled; and
- any target-specific reference association.

Reference bases may settle through the existing reference engine/cache, but atomic navigation
commit still waits for all required target data and row geometry.

### Adaptive extension

After a foreground settles, estimate resident bytes per genomic base and per eligible record.
Use the configured viewport-multiple target and remaining decoded-byte budget to choose left and
right margins:

- begin symmetrically at the configured margin when there is no navigation direction;
- bias the same total configured reach toward a stable pan direction without silently increasing
  the user's target;
- trigger extension before the viewport reaches the final quarter of the resident interval;
- align requests to canonical genomic boundaries where doing so improves reuse;
- run at most one speculative extension per alignment actor; and
- cancel or deprioritize extension immediately when a current foreground request needs the actor.

Extensions should request only missing genomic slabs and merge them transactionally with the
resident store. BAM overlap semantics can return records spanning slab boundaries, so merging must
deduplicate using the same stable alignment identity used by canonical row architecture. A failed,
cancelled or Too-dense extension leaves the existing resident window intact and marks that edge as
temporarily non-expandable.

The initial buffer policy should be conservative:

- treat the configured viewport multiple as a desired maximum, not a minimum guarantee;
- never exceed the track's remaining decoded-byte budget;
- never exceed the configured resident-record ceiling derived from the requested Complete read
  limit;
- prefer one useful margin over two margins too small to cover a likely gesture; and
- stop extending when observed bytes diverge materially from the density estimate.

### LRU integration

Do not build an unbounded second cache beside `WeightedLruCache`. Extend the decoded alignment
cache so exact query entries and resident windows share accounting and eviction telemetry.

Current foreground ownership and cache ownership must remain explicit. `Arc` references may keep a
committed query alive after an LRU eviction, but resident accounting must not claim those bytes are
free while a pinned navigation ancestor still owns them.

The long-term lookup order should be:

1. exact compatible target query;
2. compatible resident containment hit;
3. compatible overlapping resident data that can be extended within budget;
4. indexed BAM/CRAM foreground query; and
5. optional background margin extension after foreground commit.

Exact cache entries remain useful for aggregate-rich revisits. Resident data serves interval
containment and overlap reuse.

## Local and remote policy

Local and remote sources use the same correctness model but different speculation:

### Local BAM/CRAM

- With the default `1×` setting, aim for one viewport span on each side when budget permits.
- Favor decoded-record reuse because local BGZF blocks may already be in the OS page cache but
  decompression and normalization still cost CPU.
- Permit larger extensions after measured fast foreground queries.

### Remote BAM/CRAM

- Keep the foreground higher priority than all prefetch work.
- Honor the same configured spatial target, but acquire one margin at a time and prioritize the
  stable navigation direction.
- Bound speculative HTTP bytes and range-request count separately from decoded resident bytes.
- Reuse the existing remote block cache and cancellation token.
- Do not let a broad background read request delay or serialize a newer user target.
- Report speculative bytes so remote browsing never hides a large transfer behind “cache”.

No initial implementation should prefetch an entire chromosome or infer that a high Complete read
limit authorizes a comparably large network transfer.

## Invalidation matrix

| Change | Reuse decoded records | Reuse exact aggregates | Reuse row history |
| --- | --- | --- | --- |
| Contained pan/zoom, same contract | Yes | Recompute for target | Yes |
| Complete read limit changes | Revalidate/rekey | No | Usually yes |
| Read browsing buffer changes | Retain/trim to new policy | Yes | Yes |
| MAPQ/tag/flag filters change | No in first release | No | No if population changes |
| Colour-only styling changes | Yes | Yes | Yes |
| Group/sort/stack policy changes | Yes | Yes | Reconcile/rebuild |
| Pair presentation changes | Usually yes | Recompute pair summary if needed | Policy-dependent |
| Reference source changes | Records usually yes | Reference-derived data no | Usually yes |
| Alignment source/fingerprint changes | No | No | No |
| Panel closes or track is removed | Release panel/track residency | Release | Release |

Later work may retain unfiltered raw decoded records and apply new filters locally. The first
release should use the existing query-options fingerprint and reject uncertain reuse.

## Implementation phases

### Phase 0 — Baseline, telemetry and failing contracts

- Add an interaction benchmark that executes narrow → broad → narrow and short bidirectional pans
  at ACTB and representative sparse loci.
- Record HTS query count, bytes decoded, cache hit reason, projection time, summary time, row-layout
  time, time to committed target, and resident/foreground bytes.
- Add tests proving the current exact-key cache misses a contained interval and that the old broad
  projection is released on replacement.
- Capture local and remote baselines before changing admission policy.
- Benchmark `0×`, `0.5×`, `1×`, `2×` and `4×` buffer targets so the default is evidence-backed.

Deliverable: reproducible evidence that distinguishes disk/range I/O, decode, target projection,
aggregate rebuild and layout.

### Phase 1 — Protected navigation ancestor

- Retain at most one complete previous query as a navigation ancestor after a zoom-in, provided it
  fits the decoded budget.
- Consult it before HTS dispatch when a target exactly matches that ancestor.
- Account for the pinned bytes and release the ancestor on contract change, contig change, budget
  pressure or navigation outside its useful lineage.
- Expose exact-return hits and releases in diagnostics.

Deliverable: an independently shippable instant return to a recent broader exact viewport without
changing the `AlignmentBatch` consumer model.

### Phase 2 — Containment-derived exact targets

- Introduce packed record views and a correct overlap index.
- Derive exact target batches, statistics and summaries from a complete containing ancestor.
- Route derived results through the normal generation, row-layout and atomic commit path.
- Preserve inspection/export fidelity and stable record identity.
- Measure materialized-copy versus shared-view implementations and retain the simpler version if it
  meets latency and memory gates.

Deliverable: small pans and zooms contained by a resident ancestor require no BAM/CRAM query.

### Phase 3 — Adaptive resident margins

- Add the validated, persisted `0..=400%` browsing-buffer setting with a `100%` default.
- Estimate buffer reach from observed density and remaining byte budget.
- Fetch missing left/right slabs in the background after foreground commit.
- Merge and deduplicate extensions transactionally.
- Bias extension by navigation direction and suppress repeatedly failing/Too-dense edges.
- Share accounting and eviction policy with the decoded alignment LRU.

Deliverable: a session that starts zoomed in becomes ready for a slight zoom-out or pan before the
gesture occurs.

### Phase 4 — Remote discipline and scheduler integration

- Add independent speculative network-byte/range counters.
- Ensure foreground requests preempt or cancel extension work.
- Tune remote margins against representative HTTP BAM/CRAM sources.
- Verify that abandoned navigation does not leave range requests or decode work running.

Deliverable: remote browsing gains reuse without surprising transfers or head-of-line blocking.

### Phase 5 — Product integration and native qualification

- Add the Read browsing buffer control to Application settings → Performance, including numeric
  entry, explanatory copy, immediate scheduler updates and accessible effective-state feedback.
- Integrate cache-hit reasons and resident coverage into desktop diagnostics and MCP workspace
  state.
- Keep ordinary status text quiet for fast hits; show checking only after the existing debounce.
- Exercise horizontal pan, wheel zoom, history navigation, vertical read scrolling, selection,
  export, multi-panel and multi-track workflows.
- Run long warm navigation loops and capture steady-state memory.
- Document the automatic policy and the relationship to Total cache budget.
- Expose the requested value through typed automation and report requested versus effective margin
  through MCP observation.

Deliverable: a native browsing experience with quantified reuse and no evidence-contract regressions.

## Tests

### Correctness

- A complete containing resident interval yields exactly the same read identities and target
  summaries as an independent indexed source query.
- Reads beginning before the target but overlapping it through long CIGAR operations remain
  present.
- Spliced RNA alignments crossing multiple extension boundaries are deduplicated exactly once.
- A target over its effective Complete read limit is refused even when all records are resident.
- Too-dense, cancelled and failed margins are never merged as complete data.
- Filters, flags, required tags and source fingerprints prevent incompatible reuse.
- Stale generations cannot publish projected data or resident extensions.
- Picking, properties, selection, inspector and export reference the committed target revision.

### Navigation

- Zoom in, zoom out to the exact ancestor, and verify zero new HTS queries.
- Pan and zoom within a containing resident interval and verify zero new HTS queries.
- Cross one resident edge and verify only the missing side is requested.
- Reverse direction during extension and verify foreground work is not blocked.
- Navigate to another contig and verify the previous resident window becomes evictable.
- Repeat with independent panels and alignment tracks.

### Resource bounds

- The default `1×` request expands symmetrically when bytes permit and reports a smaller effective
  margin when it cannot.
- `0×` schedules no speculative adjacent alignment query but preserves ordinary exact cache reuse.
- `4×` never exceeds the decoded byte budget, resident record ceiling or remote transfer bounds.
- Settings files without the browsing-buffer field load the `1×` default, while out-of-range values
  fail validation.
- A query larger than the decoded cache share remains a valid active foreground but creates no
  speculative buffer.
- Exact queries, resident windows, indexes and target derivations remain within accounted bytes.
- A repeated 100-cycle navigation loop reaches a stable resident byte count and allocation high
  water mark.
- Lowering the cache budget evicts speculative state without invalidating the current foreground.
- Remote speculative bytes and request counts remain within configured hard bounds.

### Regression

- Complete/Too-dense boundary behaviour remains all-or-nothing.
- Canonical row identity and dense-layout performance do not regress.
- Transactional preview remains non-interactive and commits only after target data and geometry are
  ready.
- Overview LOD still avoids individual-read fetches.
- Setting, session, automation and MCP contracts remain backward compatible.

## Performance gates

The implementation is complete only when:

1. a compatible contained navigation emits no indexed alignment-source query;
2. an exact ancestor return emits no indexed alignment-source query and no new row architecture;
3. resident-hit target commit is materially faster than a warm source reread on ACTB and sparse
   fixtures, with per-stage timings reported rather than hidden in a total;
4. pointer/wheel preview response remains within the existing interaction-frame budget;
5. background extension never delays a newer foreground dispatch;
6. resident memory remains within the decoded cache allocation except for the one required active
   foreground; and
7. `0×` suppresses speculative fetches, while non-zero values never exceed their requested spatial
   target without an explicit diagnostic reason; and
8. repeated navigation shows no monotonic growth in records, row history, cache weight, GPU
   primitives or process memory.

Set numeric latency targets after Phase 0 captures stable native baselines. Do not choose a target
that can be met by returning provisional or incomplete evidence.

## Diagnostics and MCP

Expose per alignment track and panel:

- foreground cache outcome: exact, resident containment, resident extension or source query;
- resident interval(s), records and estimated bytes;
- requested and effective browsing-buffer margins on each side, including any clamp reason;
- resident contract revision and whether the current target is contained;
- resident projection and exact-summary rebuild time;
- extension requested/completed/cancelled/Too-dense counts;
- source queries and decoded bytes avoided;
- speculative local records or remote bytes fetched;
- cache resident/maximum weight and eviction reason; and
- target commit latency split into lookup, projection, summaries, layout and presentation.

Automation observation must remain read-only. It should make reuse verifiable without changing
foreground scheduling merely because diagnostics are open.

## Likely code areas

- `crates/rgv-engine/src/alignment_engine.rs`
- `crates/rgv-engine/src/cache.rs`
- `crates/rgv-hts/src/alignment.rs`
- `crates/rgv-hts/src/local.rs`
- `crates/rgv-hts/src/remote_alignment.rs`
- `crates/rgv-hts/src/pileup.rs`
- `crates/rgv-core/src/settings.rs`
- `apps/rgv-desktop/src/ui/system_dialogs.rs`
- `apps/rgv-desktop/src/workspace/alignments.rs`
- `apps/rgv-desktop/src/workspace/alignment_management.rs`
- `apps/rgv-desktop/src/workspace/queries.rs`
- `apps/rgv-desktop/src/display/navigation_preview.rs`
- `apps/rgv-desktop/src/automation/commands.rs`
- `apps/rgv-desktop/src/ui/diagnostics.rs`
- `benchmarks/interaction/`

## Risks and mitigations

### Broad data is mistaken for exact evidence

Never commit an `AlignmentQuery` whose interval-specific statistics and aggregates belong to a
different interval. Resident records are inputs to target derivation, not a target query by
themselves.

### Cache duplication consumes more memory than rereading

Use shared packed storage where measurements justify it, include projection indexes and pinned
ancestors in byte accounting, and evict speculative margins before admitting another overlapping
window.

### Long and spliced reads disappear or duplicate at boundaries

Use a genuine overlap index and stable record identity. Qualify against RNA-seq CIGAR `N`
operations, supplementary records and long-read fixtures; do not infer overlap from alignment
start alone.

### Prefetch competes with user navigation

Give speculative work lower priority, limit it to one request per actor, make it cancellable in
bounded chunks, and service a new foreground before continuing extension.

### A high Complete read limit silently becomes a memory policy

Keep the display limit and cache budget separate. The buffer consumes only measured remaining
bytes even when the user permits ten million foreground reads.

### The spatial setting appears to promise an exact amount

Label it as a desired browsing buffer and show requested versus effective margins in diagnostics.
The cache budget, record admission and remote-transfer caps always win; the UI copy must not imply
that `4×` guarantees four fully decoded viewport spans at every locus.

### Projection avoids I/O but still feels slow

Measure selection, summary rebuilding, row reconciliation and primitive materialization
independently. Add indexing or shared views only where those measurements identify the next
bottleneck.

## Non-goals

- Caching an entire chromosome's individual reads.
- Sampling reads to make a resident window fit.
- Replacing aggregate overview/index LOD with individual-read prefetch.
- Making preview pixels interactive before exact target commit.
- Treating the operating-system file cache as proof that decoded data is reusable.
- Automatically increasing the user's Complete read limit.
- Persisting decoded BAM/CRAM records across application launches in the first implementation.

## Completion definition

This work is complete when a compatible resident working set makes ordinary nearby pan and zoom
targets authoritative without another BAM/CRAM query; initial narrow views gain byte-bounded,
cancellable adjacent margins with a safe `1×` default and an adjustable `0×..=4×` application
setting; exact aggregates and complete-read semantics remain correct; local and remote speculation
cannot block foreground navigation; all resident memory is accounted and bounded; diagnostics
prove the source of every hit; automated tests pass; and native ACTB plus sparse/remote navigation
loops demonstrate faster commits with stable memory.
