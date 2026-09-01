# Alignment cache liveness and vertical-navigation plan

Status: proposed; implementation has not started

Plan date: 2026-09-01

Primary evidence:

- `rgv-manual-diagnostic-trace-9e8a3e20-b2d6-4e14-b3eb-5bb068f7dccd.json`;
- retained trace interval: 61.437–236.813 seconds;
- local RNA-seq ACTB and BAP1 browsing on the native macOS release build; and
- source audit of alignment cache, eviction, scene-worker, and vertical-navigation paths.

Related plans:

- [Native interaction stress automation](NATIVE_INTERACTION_STRESS_AUTOMATION_PLAN.md)
- [Unified alignment-scene implementation](UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md)
- [Unified alignment-scene interaction and capacity remediation](UNIFIED_ALIGNMENT_SCENE_INTERACTION_AND_CAPACITY_REMEDIATION_PLAN.md)
- [Alignment browsing working set](ALIGNMENT_BROWSING_WORKING_SET_PLAN.md)
- [Complete read presentation](COMPLETE_READ_PRESENTATION_PLAN.md)

This plan is the execution authority for the cache, eviction, scheduling, vertical-residency, and
scrollbar-liveness defects exposed by the latest ACTB trace. The separate stress-automation plan
owns gesture replay, trace control, watchdogs, native GUI drivers, stress generation, and artifact
collection. Those facilities qualify this work; they are not the cache fix.

## 1. Outcome

RGV must be able to browse a dense complete alignment stack, visit other loci, return to the dense
locus, and then scroll, pan, and zoom without prior browsing history degrading the active view or
permanently disabling navigation.

The repaired architecture must:

- give the active locus and visible row blocks first claim on the configured cache budget;
- keep previously visited loci only as a bounded, lower-priority return cache;
- maintain adaptive foreground headroom instead of reclaiming one failed allocation at a time;
- make eviction efficient, bounded, cancellable, and unable to monopolise source work;
- track vertical residency by exact block rather than globally forgetting it;
- separate row-camera movement from genomic/source navigation;
- execute at most one current missing-row request with one replaceable latest target;
- preserve complete exact evidence and stable row identity; and
- settle every current request, yield with progress, or return a truthful typed result rather than
  remaining indefinitely on `Updating view…`.

## 2. Failure established by the trace

### 2.1 Cross-locus cache retention

Immediately before returning from chr3/BAP1 to chr7/ACTB, the shared alignment ledger was already
using approximately 4.228 GB of a 4.295 GB budget:

| Owner | Approximate charge |
| --- | ---: |
| Semantic alignment scenes | 2.563 GB |
| Evidence | 1.264 GB |
| Row atlases | 270 MB |
| Total pinned allocation | 845 MB |

ACTB then committed 693,697 matching alignments across 57,861 rows into that almost-full ledger. At
trace end, 4.2945 GB was charged while approximately 1.104 GB was pinned. Most of the charge was
therefore potentially relinquishable cache ownership rather than an unavoidable complete-data
minimum.

The semantic and evidence stores already define disjoint-cache discard operations. Production has
no caller for either operation. Existing reclamation begins after admission pressure, asks for the
exact minimum bytes required by the failed allocation, and provides no low-water foreground
headroom.

This explains why fresh ACTB browsing can be fast while ACTB after extensive BAP1 browsing degrades.
A larger cache would postpone the failure, but continued browsing could fill the larger cache in the
same way.

### 2.2 Vertical transaction amplification

During the retained ACTB interaction interval, the trace recorded approximately:

| Event | Count |
| --- | ---: |
| Vertical scroll intents | 2,594 |
| Atomic preparation starts | 1,898 |
| Content replacement decisions | 310 |
| Content replacement commits | 50 |
| Uncovered candidate retentions | 628 |
| Same-target retry coalesces | 491 |

The final drag alone produced 438 scroll updates, 68 replacement decisions, and only seven commits.
Each changed row-camera position can currently invalidate navigation and request another alignment
camera transaction. Latest-wins handling removes requests that have not begun, but it does not
interrupt already-started semantic, source, or eviction work between checkpoints.

The UI event and render loop remained alive at roughly 7–9 ms per frame. Scroll targets continued
to be accepted. The visible lag was alignment-content authority falling behind the desired row
camera, not a generally blocked winit/egui loop.

### 2.3 Final source-worker stall

After ACTB expanded from approximately 3.56 kb to 303.87 kb, a foreground source request was
submitted at about 193.17 seconds. For the remaining 43.6 seconds of retained trace it stayed
`Running`, while row-atlas, semantic, and GPU products stayed `Pending`. It emitted no work-start,
checkpoint, yield, acquisition, or completion event.

The existing trace does not identify whether that interval was spent:

- waiting in a worker queue;
- executing synchronous cache eviction;
- entering or performing indexed BAM acquisition;
- decoding or filtering evidence;
- building the row atlas or exact summaries; or
- building semantic pages before the first existing checkpoint.

The stress-automation plan adds the missing phase spans. This plan must use that evidence before
choosing a source-stall remedy; it must not guess and bury the result behind a timeout.

### 2.4 Confirmed implementation defects and risks

- Inactive eviction executes synchronously on the alignment-scene worker that also pumps foreground
  source and presentation work.
- Semantic eviction repeatedly scans cached read and summary pages to select each oldest page,
  giving worst-case quadratic behavior.
- Evidence eviction sorts collections and repeatedly removes their first vector element, also
  giving worst-case quadratic movement.
- Reclaim targets the exact admission threshold. One successful replacement can refill the cache
  and force the next block into another reclaim cycle.
- One evicted read page clears every lane's vertical-residency bookkeeping.
- A missing materialized-row range can be treated as though fast-scroll coverage were proven.
- A scrollbar drag that loses exact row authority consumes movement without changing the thumb,
  producing an apparent locked control.

The trace strongly implicates cache saturation, reactive reclaim, synchronous maintenance, and
scroll transaction amplification as a combined failure. It does not yet prove which phase occupied
the final 43.6-second interval.

## 3. Product and architecture invariants

### 3.1 Complete data remains mandatory

1. Every alignment admitted by the active source and filter contract remains represented exactly.
2. No sampling, hidden coverage cap, read omission, or approximate stack may be introduced for
   interaction performance.
3. Complete canonical evidence and exact row identity do not require every derived semantic or GPU
   page to remain resident simultaneously.
4. Evicted derivative pages are reproducible from the same immutable evidence and row authority.
5. If the smallest exact requested presentation cannot fit, RGV reports a truthful,
   non-destructive `BudgetLimited` result rather than hanging, truncating, or converting capacity
   pressure into a source failure.

### 3.2 Foreground and cache authority

1. The current committed epoch, its visible row blocks, and the immediately requested block have
   highest priority.
2. Nearby genomic and vertical buffers are lower priority but may remain resident for continuous
   browsing.
3. Previously visited disjoint loci form a strictly bounded return cache. They may not compete as
   equals with the active locus near foreground pressure.
4. Speculation and return-cache residency yield before foreground work.
5. Cache policy is measured in charged bytes and ownership, not only record or page counts.
6. The configured total cache budget remains the boundary; the fix is not a hard-coded memory
   increase.

### 3.3 Scheduling and vertical interaction

1. Row-camera movement is not a genomic source change and cannot rebuild unchanged evidence or row
   authority.
2. Cached visible rows reproject immediately while the pointer is held.
3. At most one missing foreground row block executes for a lane, with one replaceable latest desired
   block waiting behind it.
4. Obsolete work observes cancellation at bounded checkpoints and cannot publish.
5. Maintenance work cannot monopolise the foreground source worker.
6. The scrollbar thumb follows accepted user intent independently of derivative-page completion.
7. Pending row authority preserves an explicit desired scroll fraction or ends the gesture
   explicitly; pointer motion cannot be silently swallowed.
8. Top and bottom boundary arithmetic produce a non-empty visible range whenever rows exist.

### 3.4 Presentation coherence

1. The last coherent committed epoch remains visible until an exact successor commits.
2. A candidate cannot blank or partially replace the current read or derived lanes.
3. Coverage, substitutions, Sashimi, labels, picking, hover, selection, inspection, and scrollbar
   metrics identify the same committed evidence and row authority.
4. Reclaiming cache ownership may remove lookup references but cannot invalidate leases held by the
   committed epoch.
5. A temporary pressure or pending state cannot make unrelated panels or application controls
   unusable.

## 4. Target cache architecture

### 4.1 Explicit residency classes

Classify every alignment cache owner into one of these priorities:

1. **Committed authority:** the current coherent epoch and exact data needed to preserve it.
2. **Active visible:** visible semantic/GPU blocks and the immediately requested foreground block.
3. **Active buffer:** nearby genomic and vertical blocks retained for ordinary browsing.
4. **Recent-locus return cache:** a bounded byte-weighted set of demoted prior working sets.
5. **Disposable speculation:** extensions and superseded candidates that yield first.

Priority is explicit cache metadata and diagnostic state. It must not be reconstructed from Arc
counts after pressure has already occurred.

Every allocation records:

- source, panel, track, interval, and row-block identity;
- evidence, atlas, semantic, and GPU authority;
- residency class and last-use revision;
- charged and pinned bytes;
- committed/candidate generation; and
- the owner capable of relinquishing it.

### 4.2 Disjoint navigation and return-cache policy

When navigation moves to a disjoint contig or non-overlapping working-set island:

- demote the old active locus to return-cache priority;
- reserve foreground headroom for the new locus before its first allocation;
- immediately relinquish disjoint semantic lookup ownership exceeding the return-cache quota;
- preserve only a bounded byte-weighted set of recent loci;
- retain leases required by the transition's committed epoch until atomic commit; and
- record bytes retained, demoted, discarded, pinned, and released.

Use the existing disjoint-cache operations as primitives, not as an unconditional clear-on-jump
hack. A useful revisit should remain fast when it fits. It must never starve the active locus.

Return-cache quota must be computed within the aggregate cache budget and reduced before active
headroom. Multiple panels and tracks participate in the same policy, with current visible owners
protected independently.

### 4.3 Adaptive high/low watermarks

Replace exact-threshold reactive reclaim with hysteresis:

- start reclaim before the ledger has no usable headroom;
- reclaim once to a low-water target capable of supporting continued foreground progress;
- derive required headroom from measured recent foreground block and replacement-bundle charges;
- bound the result by the user's configured total budget and current committed minimum;
- include all source, summary, atlas, semantic, and GPU owners in the calculation;
- recalculate when tracks, panels, cache budget, or measured bundle size changes; and
- publish the calculation and its inputs in diagnostics.

Do not hide unexplained ACTB-specific constants. Initial calibration must be recorded and qualified
across sparse, medium, and dense loci and across several budgets. The policy may use bounded
percentiles or a bounded recent maximum of observed foreground costs, provided its response is
deterministic and testable.

### 4.4 Aggregate admission and reclaim

Admission and reclaim must be one shared-ledger transaction:

1. Calculate the exact committed minimum and candidate requirement.
2. Apply retirement credit only to allocations proved to retire on atomic commit.
3. Relinquish disposable speculation and excess return-cache ownership first.
4. Evict inactive active-buffer pages outside the latest demand next.
5. Preserve committed authority and current visible blocks.
6. Retry admission only after measured ledger progress reaches the low-water gate.
7. Classify an intrinsically impossible candidate as `BudgetLimited` without destroying the
   current epoch.

A changed navigation target must explicitly cancel or retarget an obsolete reclaim transaction on
the worker. Clearing only the UI-side bookkeeping is insufficient.

### 4.5 Efficient and preemptible eviction

Replace repeated scans and front-vector removal with indexed ownership:

- use an ordered LRU queue/index or equivalent bounded candidate selection;
- batch or range-drain evidence entries instead of repeated `Vec::remove(0)` shifts;
- keep protected committed allocations outside the disposable candidate queue;
- process maintenance in bounded byte, entry, and time quanta;
- check for a newer foreground target between quanta;
- cancel or retarget stale queued reclaim; and
- report entries inspected, entries removed, bytes released, elapsed time, and remaining charge.

Evaluate two acceptable execution models with phase evidence:

1. a separate maintenance worker with safe immutable ownership handoff; or
2. the existing scene worker with strict bounded quanta and foreground preemption.

Do not move mutable coordinator state across threads merely to conceal a synchronous algorithm. The
chosen model must have explicit ownership, cancellation, publication, and shutdown semantics.

### 4.6 Exact per-block vertical residency

Track semantic and GPU vertical residency as exact row-page/block sets. Evicting one read page
removes only that block from residency. It cannot reset every lane or cause unrelated cached blocks
to be rebuilt.

The directory records:

- exact row interval;
- evidence and row-atlas authority;
- semantic and GPU page identity;
- residency class and last use;
- charge and pin ownership;
- committed/candidate generation; and
- preparation state for the current/latest request.

Whole-stack warming remains optional and lowest priority. It cannot own navigation liveness or fill
headroom reserved for the current visible block.

## 5. Vertical-navigation architecture

### 5.1 Separate row camera from source navigation

A vertical scroll changes the row camera only. It does not invalidate genomic evidence, filters,
summaries, or the row atlas.

The hot path is:

```text
input row-camera target
        ↓
update thumb and row camera immediately
        ↓
reproject already resident blocks
        ↓
look up missing visible/near blocks
        ↓
replace one latest-wins row-block request
        ↓
publish exact blocks without replacing unrelated scene authority
```

Only a genomic interval, source, filter, row-layout policy, reference, or presentation-contract
change opens broader navigation work.

### 5.2 Latest-wins block scheduling

For each panel/track lane:

- one current visible-block request may execute;
- one latest desired block may replace any queued predecessor;
- the executing request checks target currency between bounded page/build checkpoints;
- a superseded request releases private candidate ownership and cannot publish;
- cached intersections remain visible while the missing portion is prepared;
- nearby prefetch runs only when no foreground block waits; and
- release does not rebuild geometry already shown during the held drag.

Pointer sample count must not determine semantic transaction count. The scheduler responds to the
latest row-block demand, not to every intermediate pixel.

### 5.3 Scrollbar camera semantics

- Pointer-down captures exact row-coordinate authority and a normalized desired fraction.
- Every accepted movement updates the thumb and desired camera immediately.
- If total-row authority becomes pending, preserve the desired fraction and reproject it when exact
  authority arrives, or visibly terminate the old gesture.
- Do not retain pointer capture while silently ignoring movement.
- Pending thumb geometry is visibly provisional and cannot imply an exact extent.
- A new locus cannot inherit an obsolete previous-locus maximum or captured drag.
- Top, bottom, one-row, and empty-stack states have explicit tested behavior.

### 5.4 Typed materialization coverage

Replace boolean fast-scroll coverage with:

- `Covered(exact row range)`;
- `PartiallyCovered(exact intersections)`;
- `Unknown`; or
- `Absent`.

A missing materialized range is `Unknown`, never `Covered`. Separate successful camera/chrome
reprojection from proof that read pages cover the requested rows. Rendering, hit testing, status,
and diagnostics consume the same typed result.

### 5.5 Derived lanes and interaction

Coverage, substitutions, Sashimi, labels, hover, selection, read inspection, context menus, and
mate navigation must retain their existing committed authority while vertical read blocks change.
Vertical block publication cannot reset unrelated derived lanes or interaction indexes.

Multi-panel work keeps separate row cameras and block demand per panel/track. Cache admission is
aggregate, but one panel's scrollbar state cannot be copied into another panel or made pending by an
unrelated lane.

## 6. Source-worker liveness

The cache work and final 43.6-second stall overlap but are not assumed to have one cause.

After the stress-automation plan adds worker phase spans:

1. reproduce the broad ACTB stall;
2. identify the exact blocking phase and target owner;
3. determine whether it is queued behind reclaim, executing eviction, blocked in backend setup,
   decoding, filtering, atlas construction, or semantic preparation;
4. implement bounded progress/cancellation at the measured phase; and
5. verify that the current target settles or returns a truthful result.

Potential remedies include queue preemption, backend work slicing, more frequent cancellation
checkpoints, filtered-evidence reuse, incremental atlas construction, semantic page chunking, or
removal of an eviction/source scheduling cycle. None is selected until phase evidence supports it.

Every accepted foreground request must have a current owner and observable last-progress point. A
worker cannot remain indefinitely `Running` without either progress, yield, completion,
cancellation, or a typed terminal result.

## 7. Implementation phases and gates

### Phase 0 — Evidence and qualification dependency

Use the separate stress-automation plan to provide:

- worker phase spans needed to localise the source stall;
- the BAP1→ACTB trace-derived replay scenario;
- fresh ACTB, fresh BAP1, and carry-over baseline reports; and
- semantic/liveness assertions and artifacts.

Gate: the cache failure signature is repeatable or statistically classifiable, and the final stall
can be attributed when it occurs. This phase adds no cache policy.

### Phase 1 — Residency authority

Deliverables:

- explicit residency classes and owner metadata;
- exact per-owner charge/pin reporting;
- active-locus, buffer, return-cache, and speculation inventories;
- deterministic priority transitions; and
- multi-track/panel accounting tests.

Gate: every charged alignment allocation has a visible priority and relinquishing owner; current
authority is distinguishable from reusable cache ownership.

### Phase 2 — Disjoint navigation and adaptive headroom

Deliverables:

- bounded recent-locus return cache;
- proactive disjoint-cache demotion and discard;
- adaptive high/low watermarks;
- aggregate foreground reserve; and
- fresh-versus-carry-over qualification.

Gate: BAP1→ACTB cannot enter ACTB with an effectively unusable cache, while a bounded ordinary
revisit still benefits from retained data.

### Phase 3 — Eviction and scheduling

Deliverables:

- indexed/batched eviction structures;
- bounded maintenance quanta;
- foreground preemption or a safely separated maintenance worker;
- stale reclaim cancellation/retargeting;
- exact reclaim progress and complexity counters; and
- per-block residency subtraction.

Gate: production eviction has no worst-case quadratic removal path, cannot monopolise foreground
work, and converges to low water without repeated one-allocation reclaim cycles.

### Phase 4 — Vertical camera and residency cutover

Deliverables:

- row-camera-only input path;
- exact resident-block directory;
- one-current-plus-one-latest block scheduler;
- typed materialization coverage;
- responsive pending-authority scrollbar behavior;
- exact top/bottom range handling; and
- independent multi-panel scrollbars.

Gate: the ACTB rapid-drag scenario no longer amplifies pointer samples into full alignment
navigation transactions, and cached block movement remains immediately visible.

### Phase 5 — Measured source-stall repair

Deliverables depend on Phase 0 evidence. Implement the narrowest architectural correction at the
identified blocking phase, with bounded progress and cancellation.

Gate: broad ACTB work makes observable progress and settles or returns a truthful typed result
under repeated carry-over runs.

### Phase 6 — Regression and visual acceptance

Deliverables:

- release qualification across fresh and carry-over loci, budgets, panels, and tracks;
- regression checks for complete data, coverage, substitutions, Sashimi, picking, selection,
  context menus, and mate navigation;
- long stress results from the automation plan; and
- user visual assessment of held pan, vertical movement, locus switching, and pending behavior.

Gate: automated criteria pass and the user confirms intended native behavior. Only then update the
unified-scene roadmap to mark this remediation complete.

## 8. Qualification contract

The automation plan owns how scenarios are driven and artifacts are captured. This plan owns the
cache and navigation assertions.

Required scenarios:

- fresh ACTB;
- fresh BAP1;
- BAP1 → ACTB carry-over;
- ACTB → BAP1 → ACTB after deep vertical browsing;
- the recorded eight-drag ACTB sequence followed by broad zoom;
- repeated contained horizontal pans;
- rapid zoom-in/zoom-out before settlement;
- filter/highlight/sort/group changes at ACTB;
- multi-panel mate navigation with independent scrollbars;
- multiple alignment tracks sharing the ledger;
- several cache budgets and browsing-buffer settings; and
- sparse, medium, and dense loci.

Required assertions:

- the final desired target settles or returns a truthful non-destructive typed result;
- no current owner remains `Running` without phase progress;
- cache policy maintains foreground headroom within the configured budget;
- return-cache material cannot force the active locus into repeated exact-threshold reclaim;
- only current/latest missing row-block work remains publishable;
- scroll intent and thumb position remain responsive while exact authority exists;
- a pending successor cannot leave a captured scrollbar locked;
- complete matching-record and row authority remain exact;
- no critical failure, orphan reclaim, stuck publication, or permanent `Updating view…` occurs;
- eviction removes only the affected residency blocks; and
- fresh and carry-over performance remain within an agreed evidence-based variance on the same
  build and hardware.

Performance thresholds must be based on the mechanically established fresh baseline and structural
operation bounds. Do not invent a product latency promise merely to make a test turn green.

## 9. Likely implementation areas

The exact module boundaries may change during refactoring, but the work is expected to touch:

- `crates/rgv-alignment-scene/src/presentation_store.rs`;
- `crates/rgv-alignment-scene/src/store.rs`;
- `crates/rgv-engine/src/alignment_scene_engine.rs`;
- `crates/rgv-engine/src/alignment_scene_coordinator.rs`;
- `apps/rgv-desktop/src/workspace/alignment_management.rs`;
- `apps/rgv-desktop/src/workspace/queries.rs`;
- `apps/rgv-desktop/src/display/mod.rs`;
- `apps/rgv-desktop/src/ui/interaction.rs`; and
- their unit, integration, benchmark, and source-inventory tests.

Cache ownership and scheduler types should live below desktop UI code. The desktop may express
current demand and display typed state, but it cannot become the authoritative cache manager.

## 10. Definition of done

This plan is complete only when all of the following are true:

1. Every charged alignment allocation has an explicit residency class and relinquishing owner.
2. Cross-locus browsing uses a bounded return-cache and active-working-set policy.
3. Disjoint old-locus ownership is demoted before it can starve a new active locus.
4. Reclaim creates adaptive headroom instead of freeing one failed allocation at a time.
5. Aggregate admission includes evidence, summaries, atlases, semantic pages, and GPU pages.
6. Production eviction no longer uses the identified worst-case quadratic removal paths.
7. Maintenance is bounded and foreground-preemptible.
8. Stale reclaim work can be cancelled or retargeted on the worker.
9. Per-block residency remains exact after eviction and locus changes.
10. Vertical scrolling changes row camera and block demand, not unchanged source/atlas authority.
11. Only current/latest row-block work survives rapid scrolling, with bounded cancellation latency.
12. Scrollbar extent and intent remain exact and responsive through pending transitions.
13. The measured broad-ACTB worker stall has an identified and repaired cause.
14. The trace-derived carry-over scenario passes repeatedly under the 4 GiB configuration.
15. Complete ACTB record and row authority remain exact; no reads are sampled or omitted.
16. Existing alignment and multi-panel features pass regression qualification.
17. The user completes and accepts the final native visual review.

## 11. Explicit non-solutions

Do not resolve this work by:

- increasing the default cache or requiring more RAM;
- clearing every cache unconditionally on every navigation;
- sampling or silently omitting reads;
- lowering the displayed-read limit;
- rebuilding the complete row stack for every scroll sample;
- retaining a stale screen texture as a substitute for live read presentation;
- adding timeout-based commits, repeated blind retry, or fixed sleeps;
- hiding pending or failed state behind an interactive-looking but inert scrollbar;
- moving unsafe mutable state to another thread solely to hide synchronous eviction;
- resetting every lane's residency after one page is evicted; or
- accepting a fresh-locus benchmark as proof that carry-over browsing is fixed.
