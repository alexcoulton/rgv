# Alignment cache thrash and scroll-materialization recovery plan

Status: implementation complete; automated structural verification passed; native user acceptance
pending

Plan date: 2026-09-01

Primary evidence:

- `rgv-manual-diagnostic-trace-558a8214-6b08-4b8f-bb92-7d16da8ac221.json`;
- retained trace interval: 163.346–271.221 seconds;
- disjoint return to ACTB at 198.896 seconds;
- 4 GiB configured alignment cache and 400% read-browsing side buffer; and
- source audit of the cache-residency, aggregate-reclaim, publication, materialization, and
  vertical-scroll paths.

Related plans:

- [Alignment cache liveness and vertical navigation](ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md)
- [Unified alignment-scene interaction and capacity remediation](UNIFIED_ALIGNMENT_SCENE_INTERACTION_AND_CAPACITY_REMEDIATION_PLAN.md)
- [Alignment browsing working set](ALIGNMENT_BROWSING_WORKING_SET_PLAN.md)
- [Complete read presentation](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Native interaction stress automation](NATIVE_INTERACTION_STRESS_AUTOMATION_PLAN.md)

## Implementation record — 2026-09-01

The corrective architecture in this plan is now implemented. This is not another cache-size
increase or an exact-fit retry. It changes the ownership and scheduling rules which allowed the
previous implementation to saturate:

- reloadable alignment ownership now has five byte-accounted residency classes:
  `DisposableSpeculation`, `RecentLocusReturnCache`, `ActiveLocusCold`, `ActiveNearBuffer`, and
  `ForegroundVisible`;
- committed/candidate protection remains independent through the allocation ledger's pins, so
  lowering cache priority never destroys the last coherent presented epoch;
- evidence and semantic stores report distinct allocation bytes as well as entry counts, including
  pinned and unpinned ownership;
- every live lane contributes an explicit genomic/row demand; visible pages are promoted, nearby
  pages remain buffered, distant same-locus pages become cold, and disjoint pages become return
  cache;
- disjoint navigation proactively schedules reclaim before old cache ownership can consume the new
  locus's working space;
- the low-water target reserves a measured complete foreground derivative bundle, uses a bounded
  recent upper envelope before a new island has its own measurement, and never crosses the pinned
  floor;
- reclaim requests coalesce toward the strictest target and survive overlapping pan, zoom, and row
  movement; only a genuinely disjoint demand retargets them;
- while low-water reclaim is active, the worker checks the mailbox between bounded quanta but does
  not allocate another foreground page between those quanta. This closes the prior loop in which
  each newly freed page was immediately consumed and the target could never be reached;
- obsolete source-worker presentation ownership is detached after a failed disjoint transition,
  while the desktop's committed leases continue to keep the visible epoch safe;
- optional whole-stack warming remains disposable and is paused after pressure rather than
  competing with current rows;
- a committed native scene now publishes the normalized exact set of row blocks actually present,
  rather than leaving production materialization as `Unknown` or guessing one broad span; and
- covered vertical movement uses the existing display cache, while partial/absent coverage keeps
  the coherent committed display visible and requests only the latest row camera instead of
  invalidating the complete panel.

The diagnostic trace now records the measured reserve and low-water calculation, byte and entry
inventories for every semantic residency class, evidence/return-cache bytes, reclaim quanta and
outcome, exact materialization coverage, and whether an uncovered scroll preserved the committed
display. A follow-up trace can therefore distinguish insufficient pinned capacity from retained
return cache or an incomplete row-block publication without inference from UI symptoms.

Automated structural verification completed on the implementation worktree:

- 176 `rgv-alignment-scene` unit tests;
- 114 `rgv-engine` unit tests, including low-water reserve, bounded upper-envelope, reclaim
  coalescing, overlapping-camera persistence, disjoint demotion, and latest-row scheduling checks;
- 80 `rgv-render` unit tests and the atomic alignment-bundle integration test;
- 432 `rgv-desktop` unit tests, including exact disjoint row-block normalization and conservative
  materialization coverage; and
- `cargo check -p rgv-desktop --all-targets`.

These checks establish ownership, authority, and scheduling behavior; they do not claim native
visual acceptance. Phase 6 remains open until the user exercises fresh ACTB, deep scrolling,
ACTB → BAP1 → ACTB carry-over, pan/zoom, substitutions, Sashimi, picking, and mate-panel
behavior in the release application. If that test fails, this plan remains unaccepted and the saved
trace is the next source of truth.

This plan is the corrective execution authority for the failed native acceptance of
`ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md`. It does not discard the unified
alignment scene. It preserves immutable complete evidence, canonical row identity, canonical
semantic pages, atomic publication, and latest-wins row scheduling while completing the cache
policy and display-authority work that the preceding implementation left unfinished.

## 1. Outcome

After browsing a dense ACTB stack, visiting one or more other loci, and returning to ACTB, RGV must
not progressively degrade into repeated allocation failure, exact-threshold reclaim, full display
invalidation, or an inert/lagging vertical navigator.

The repaired system must:

- give the current visible rows and immediately useful buffer first claim on the configured
  budget;
- retain old loci only from genuinely spare residual capacity;
- reclaim to a measured foreground reserve rather than freeing one failed allocation at a time;
- distinguish visible, nearby, cold-current-locus, return-cache, and speculative derivatives;
- track charged bytes, not merely entry counts, for every residency class;
- propagate exact committed row-block materialization into the desktop fast-scroll path;
- reproject cached read blocks during vertical movement without rebuilding the complete display;
- request only missing current/latest row blocks when coverage is partial or absent;
- preserve complete read evidence, exact row identity, derived-lane coherence, picking, and
  inspection; and
- remain truthful and responsive when the complete derivative working set exceeds the configured
  cache.

This is not a request to make every derivative for every row at every visited locus permanently
resident. Complete data means that every eligible alignment remains represented by exact
authority and can be presented; it does not require all reproducible CPU/GPU pages to occupy
memory simultaneously.

## 2. Failure established by the latest trace

The relevant ACTB return and all subsequent stress activity are present despite the trace's
bounded retention. No critical failure was recorded; the observed problem is a live but
pathological capacity and presentation cycle.

### 2.1 Capacity state on ACTB return

At the ACTB return, the shared alignment ledger was already using approximately 4,082 MiB of its
4,096 MiB budget. The first reclaim after the return reported:

| Residency inventory | Entries |
| --- | ---: |
| Active evidence | 2,908 |
| Return-cache evidence | 9,724 |
| Active semantic pages | 5,948 |
| Return-cache semantic pages | 30,870 |

The old working set had therefore been classified as return-cache material, but it had not been
bounded or proactively relinquished. Classification without a byte quota did not reserve usable
capacity for ACTB.

### 2.2 Reclaim thrash

From 202.027 to 268.630 seconds, the trace records:

| Measurement | Result |
| --- | ---: |
| Completed reclaim cycles | 81 |
| Cache entries relinquished | 7,300 |
| Bytes repeatedly relinquished | 722.52 MiB |
| Total reclaim execution time | 853.67 ms |
| Average post-reclaim headroom | 1.03 MiB |
| Maximum post-reclaim headroom | 6.31 MiB |
| Evidence candidates inspected | 0 |
| Semantic candidates inspected | 24,576 |

The implementation usually reclaimed just enough for the immediate small page or replacement
bundle. The next block consumed that space and reopened the same admission/reclaim/publication
cycle. Bounded eviction kept the UI thread alive, but it did not create a stable working set.

At trace end, 4,293,216,207 bytes were charged against a 4,294,960,816-byte budget: only 1,744,609
bytes of aggregate headroom remained. Thousands of return-cache entries were still present.

### 2.3 Incomplete residency model

The preceding plan described committed authority, visible pages, active buffers, return cache, and
speculation. Production cache entries currently distinguish only:

- `ActiveBuffer`; and
- `RecentLocusReturnCache`.

This is insufficient for a dense locus. A read page thousands of rows from the vertical camera can
remain equal in priority to a visible or near-visible page merely because it overlaps the same
genomic working-set horizon. Current-locus semantic pages therefore accumulate until pressure,
while reclaim cannot distinguish immediately useful blocks from cold ones.

Pinned committed authority exists in the ledger, but it is not exposed as part of one complete
byte-weighted residency inventory. Entry counts cannot determine how much capacity is occupied by
each priority.

### 2.4 Unwired materialization authority

The typed `Covered` / `PartiallyCovered` / `Unknown` / `Absent` check is conservative, but its
production input is incomplete. `ReadPanelDisplayCache.materialized_row_range` is initialized and
reset to `None`; no production path assigns a committed materialized range.

Consequently, the requested row range is normally classified as `Unknown`, even when the unified
scene has exact covered rows. In the retained ACTB interval the trace records:

| Vertical/presentation event | Count |
| --- | ---: |
| Scroll intents | 6,099 |
| Fast-scroll refresh refusals | 1,735 |
| Scroll-cache authority mismatches | 917 |
| Atomic preparation starts | 4,201 |
| Content replacement decisions | 452 |
| Content replacement commits | 267 |

Each refused fast path marks broader display state dirty and falls back to slower reconstruction.
The materialization type fixed an unsafe truth claim, but the missing producer means the intended
fast path is effectively unavailable.

### 2.5 What did work

The trace also narrows the required remedy:

- returning to ACTB caused zero new BAM source acquisitions;
- complete evidence and exact row authority remained reusable;
- 6,099 input intents were coalesced into approximately 301 deferred row-camera requests and 299
  foreground row-block publications;
- the final authority represented 890,402 matching alignments across 57,861 rows;
- there was no critical error, idle-worker terminal failure, or loss of event-loop liveness; and
- immutable page keys remained independent of exact camera coordinates.

The source reuse, canonical-page model, exact row atlas, row-camera scheduler, and atomic epoch
should be adapted, not replaced.

## 3. Why the automated checks did not catch this

The existing tests proved local mechanisms rather than the end-to-end product invariants:

- demotion tests proved that a class flag changed, not that return-cache bytes became bounded;
- reclaim tests proved that a target could be reached, not that the target left enough capacity
  for continued foreground progress;
- headroom tests used one failed allocation, not a sequence of visible-block admissions;
- materialization tests proved the four-state classifier, but no test required a committed scene
  to populate its production input;
- scheduler tests proved current/latest coalescing, not that the desktop could present a cached
  result through its fast path; and
- unit tests did not exercise ACTB → disjoint locus → ACTB under one shared 4 GiB ledger.

The corrective work must add scenario-level structural tests. Automated tests remain valuable for
authority, ownership, and operation-count guarantees, but they do not replace the user's visual
assessment of the native application.

## 4. Product and correctness invariants

### 4.1 Complete data

1. Every alignment satisfying the active source and filter contract remains represented exactly.
2. No sampling, read omission, hidden coverage cap, or reduced Complete-read limit may be used.
3. Canonical evidence and row identity may remain resident independently of derived semantic and
   GPU pages.
4. Evicted derivatives must be deterministically reproducible from the same evidence, row atlas,
   reference, and presentation contracts.
5. An intrinsically impossible minimal visible presentation returns a truthful non-destructive
   `BudgetLimited` result; it does not hang, truncate, or become a source failure.

### 4.2 Presentation continuity

1. The last coherent committed epoch remains visible until an exact successor commits.
2. Reclaim can remove cache lookup ownership but cannot invalidate leases held by a committed
   epoch.
3. Coverage, substitutions, Sashimi, labels, picking, hover, selection, inspection, context menus,
   and scroll metrics identify the same committed evidence and row authority.
4. Partially available rows cannot clear the complete read panel or unrelated annotation panels.
5. A pending row request must not lock the scrollbar or conceal accepted user intent.

### 4.3 Resource policy

1. The configured total cache remains the hard shared-ledger boundary.
2. Policy uses distinct charged allocation ownership; an allocation referenced by several
   products is counted once.
3. Current committed and visible authority outrank cache reuse.
4. Old loci receive only residual capacity after current foreground obligations.
5. No ACTB-specific byte constant or hard-coded RAM increase is permitted.
6. Multiple panels and tracks participate in one aggregate policy without sharing row cameras.

### 4.4 Interaction

1. Vertical movement changes row demand and camera state, not unchanged source/filter/atlas
   authority.
2. Cached covered rows reproject immediately while the pointer is held.
3. At most one current missing-row request and one replaceable latest target exist per lane.
4. Optional warming and maintenance yield to visible-row work.
5. Pointer sample count cannot determine source, atlas, or complete-stack reconstruction count.

## 5. Target resource architecture

### 5.1 Orthogonal protection and residency

Do not encode committed protection and cache reuse as one ambiguous enum. Each allocation needs
two orthogonal properties:

1. **Protection:** whether a committed/candidate lease currently prevents physical reclamation.
2. **Residency priority:** why a cache owner retains a lookup reference after protection is
   accounted for.

The cache priority order, from most protected to first disposable, is:

1. `ForegroundVisible` — exact pages needed for the current visible camera or its in-flight
   successor.
2. `ActiveNearBuffer` — the configured nearby genomic and vertical buffer that supports ordinary
   pan/zoom/scroll.
3. `ActiveLocusCold` — reusable pages from the current locus outside the latest near buffer.
4. `RecentLocusReturnCache` — demoted pages belonging to a disjoint previous working set.
5. `DisposableSpeculation` — optional warming, superseded candidates, and unrequested extensions.

Eviction runs in the reverse order above. A committed lease remains safe regardless of class; the
cache can relinquish its reference and allow the bytes to retire when the final protected lease is
released.

Every cache entry records or can resolve without a full scan:

- source, panel, track, and contract identity;
- genomic tile/interval and exact row block where applicable;
- charged allocation identity and byte size;
- protection/pin owners;
- residency class and last-use revision;
- committed/candidate generation;
- rebuild cost family: source evidence, summary, atlas, semantic, or GPU; and
- its cache owner and removal operation.

### 5.2 Byte-weighted inventories

The shared ledger must provide constant-size inventories for each class:

- distinct charged bytes;
- entries and allocations;
- pinned/protected bytes;
- immediately relinquishable bytes;
- retirement-credit bytes; and
- bytes shared across owners without double counting.

Entry counts remain useful complexity telemetry but cannot drive quota decisions. The trace must
be able to state exactly how many bytes belong to active visible, near, cold, return, and
speculative ownership.

### 5.3 Demand snapshot

Each panel/track lane publishes one latest immutable demand snapshot containing:

- committed and desired evidence/row-atlas authority;
- visible genomic tiles and row blocks;
- configured near genomic and vertical buffer;
- required summary and GPU products;
- known missing pages;
- active transition retirement credit; and
- current request/generation identity.

The cache controller derives classes from all live lane demands. It does not infer priority from
`Arc` counts after admission fails. Updating a row camera reclassifies exact row blocks without
reclassifying or reacquiring unchanged evidence.

### 5.4 Dynamic foreground reserve

The foreground reserve is derived from current demand, not a fixed percentage of RAM.

It includes:

- the distinct charge of known missing visible pages;
- the exact or measured charge of one atomic visible successor bundle;
- non-retired overlap required during atomic replacement;
- reserved transition/transient capacity not already represented in the ledger charge; and
- a bounded recent upper envelope when a page's size cannot be known before construction.

The estimator records its inputs and prediction error. Once a candidate is measured, subsequent
targets use the measured charge. Sparse loci naturally reserve less; dense loci reserve more.

Define the low-water target from the configured budget minus this reserve, floored at the exact
committed minimum. A reclaim completes only when the low-water target is reached or all remaining
ownership is protected. Reaching the single-allocation admission threshold is not completion.

If the committed minimum plus the minimal visible successor intrinsically exceeds the budget,
return `BudgetLimited` with the measured requirement. Do not destroy the current epoch while
discovering this.

### 5.5 Residual return-cache quota

There is no independent fixed return-cache allowance. Its byte quota is the residual capacity
after distinct ownership for these obligations is accounted for:

```text
configured aggregate budget
  − committed minimum
  − foreground reserve
  − current visible and active-near working set
  − required non-alignment/shared reservations
  = maximum opportunistic cold/return-cache ownership
```

The calculation must use a distinct-allocation union so shared references are not subtracted
twice. `ActiveLocusCold` receives residual capacity before disjoint return-cache material. Return
cache may be large when the active view is sparse and may fall to zero at an extreme locus.

This keeps useful revisits when they genuinely fit without allowing a previous locus to starve the
current view. The user controls the total cache and browsing-buffer reach; no new tuning setting is
required for correctness.

### 5.6 Disjoint navigation transaction

When the desired source horizon becomes disjoint:

1. Publish the new demand identity and cancel obsolete speculative work.
2. Reclassify old active ownership as return-cache material while the committed epoch keeps its
   independent protection leases.
3. Compute the new foreground reserve and residual return quota.
4. Relinquish speculation and excess return-cache ownership in bounded quanta before it can
   compete with the new foreground candidate.
5. Admit and build only the latest current-locus demand.
6. Apply exact retirement credit at atomic commit.
7. Reclassify the settled visible and near blocks from the new demand snapshot.

This is neither an unconditional clear nor a wait for the first allocation failure. If useful old
data fits the residual quota it remains reusable. If it does not, cache ownership yields while its
still-committed leases remain visually safe.

### 5.7 Reclaim and eviction

Preserve bounded, preemptible eviction, but change its goal and ordering:

1. disposable speculation;
2. excess disjoint return-cache pages;
3. cold current-locus pages outside current demand;
4. excess near-buffer pages outside the effective byte-constrained buffer;
5. reloadable derived pages before expensive source evidence within the same class; and
6. source evidence only when the class quota still cannot be satisfied.

Class priority precedes rebuild-cost preference. Old return-cache evidence must eventually be
eligible; it cannot remain untouched merely because small semantic pages can satisfy each
individual failure.

Maintenance remains bounded by entry, byte, and elapsed-work quanta. The worker checks for a newer
foreground demand between quanta. Reclaim requests coalesce toward the strictest current
low-water target and are cancelled/recomputed when their genomic demand becomes obsolete.

## 6. Vertical materialization architecture

### 6.1 Exact committed coverage authority

Replace `Option<(start, end)>` as the authoritative production model with a compact exact row-block
set associated with:

- presentation target revision;
- row-atlas/coordinate-space revision;
- preparation genomic tile set;
- source/filter/presentation contract; and
- committed semantic/GPU page identity.

A single range may be exposed as a derived convenience only when the set is contiguous. Disjoint
warmed blocks cannot be represented truthfully by one optional range.

The engine already knows the lane's requested/covered rows and resident blocks. Atomic publication
must propagate this authority through the presented epoch into the desktop read-panel cache.
Normal settled native scenes must not report `Unknown`.

### 6.2 Coverage result

For the latest required visible row blocks, return one typed result:

- `Covered` — all required blocks are present under the exact committed authority;
- `PartiallyCovered` — exact intersections and missing blocks are known;
- `Absent` — none of the required blocks are present under that authority; or
- `PendingAuthority` — a successor row coordinate space is not yet exact.

`Unknown` may remain only for legacy/non-native state and must not be the normal production result.
Rendering, hit testing, selection, scrollbar status, and diagnostics consume the same authority.

### 6.3 Vertical display behavior

On vertical input:

1. Update the scrollbar thumb and desired row camera immediately.
2. Compute required blocks from the exact row atlas.
3. If `Covered`, update native camera uniforms and bounded visible interaction/text state only.
4. If `PartiallyCovered`, reproject the exact intersections and submit only the missing latest
   blocks; do not invalidate the entire panel.
5. If `Absent`, keep the last coherent committed epoch visible with a truthful small pending state
   until the latest visible block commits; do not publish a blank or falsely reprojected frame.
6. If row authority is pending, retain the latent normalized scroll intent and visibly mark it as
   provisional until exact authority returns.

An incomplete materialization result must not call the general full-display dirty path merely to
move the thumb. The current/latest row scheduler remains the only producer of missing visible
blocks.

### 6.4 Active vertical reclassification

Every row-camera update reclassifies canonical row blocks:

- visible and in-flight blocks become `ForegroundVisible`;
- configured adjacent blocks become `ActiveNearBuffer` if the byte budget permits;
- other same-locus blocks become `ActiveLocusCold`;
- optional warming pages remain `DisposableSpeculation` until demanded; and
- blocks promoted by an actual demand are touched once, not once per pointer sample.

This prevents the entire 57,861-row ACTB stack from remaining equal in priority merely because it
shares one genomic interval.

### 6.5 Optional warming

Whole-stack warming is permitted only when:

- no foreground source, row, publication, or reclaim work is pending;
- the low-water foreground reserve is satisfied;
- the resulting pages fit the effective near/cold residual quota; and
- the work observes current-demand cancellation checkpoints.

Pressure immediately pauses warming. A warmed page becomes near/visible only when the current
demand snapshot actually includes it.

## 7. Diagnostics and trace contract

Extend the manual trace with constant-size events/counters for:

- distinct bytes and entries by protection and residency class;
- calculated committed minimum, foreground reserve, high-water trigger, low-water target, and
  residual return quota;
- reserve inputs, estimator history, and prediction error;
- class transitions and bytes demoted/promoted/relinquished;
- reclaim cause, start/end charge, target, quanta, foreground preemptions, and terminal reason;
- exact required, covered, intersection, and missing row blocks;
- fast-path disposition: camera-only, partial intersection, pending, or refused invariant;
- full-display invalidations caused by vertical input;
- source/atlas/semantic/GPU work opened by row-camera movement; and
- optional warming admission, pause, cancellation, and bytes.

High-frequency pointer/frame events should be sampled or aggregated so cache-policy evidence is
not overwritten by redundant motion records. Critical state transitions and interaction
boundaries remain lossless within the configured retention window.

The trace must make these questions mechanically answerable:

1. How many bytes from prior loci remained when ACTB became active?
2. Why were those bytes retained?
3. What reserve was calculated for ACTB and from which measurements?
4. Did reclaim reach low water, and what protected it from going farther?
5. Which exact row blocks were present when fast scrolling was accepted or deferred?
6. Did any vertical input reopen BAM, filtering, or row-atlas work?

## 8. Implementation phases and gates

### Phase 0 — Reopen acceptance and add failing structural scenarios

Deliverables:

- mark the preceding native acceptance as failed;
- preserve the latest trace as the baseline evidence;
- add a deterministic shared-ledger carry-over scenario modeling ACTB → disjoint locus → ACTB;
- add a production-boundary test proving that a committed lane supplies materialized row blocks;
- add assertions that exact-threshold reclaim and normal `Unknown` materialization fail; and
- record the current operation-count baseline without treating it as visual proof.

Gate: the tests fail for the confirmed missing return quota, inadequate reserve, and missing
materialization producer rather than for an invented latency threshold.

### Phase 1 — Byte-weighted residency authority

Deliverables:

- orthogonal protection and expanded cache-residency metadata;
- exact row-block and genomic-tile owner identity;
- distinct-allocation byte inventories by class;
- class-transition operations driven by lane demand snapshots; and
- unit tests for shared allocations, pins, multi-panel ownership, and deterministic ordering.

Gate: every charged alignment allocation has an explainable protection state, cache priority,
distinct byte contribution, and relinquishing owner.

### Phase 2 — Foreground reserve and residual return quota

Deliverables:

- demand-derived foreground reserve;
- measurable fallback estimator for unknown candidate size;
- residual byte quota for active-cold and return-cache material;
- proactive disjoint-transition reclaim;
- low-water completion semantics; and
- evidence eviction once lower-priority class quotas require it.

Gate: a disjoint return cannot begin foreground ACTB work with unusable headroom while excess
return-cache ownership remains. One reclaim converges to the calculated low water rather than one
allocation's threshold.

### Phase 3 — Current-locus vertical priority

Deliverables:

- visible, near, cold, and speculative row-block reclassification;
- current/latest demand integration;
- warming admission below foreground reserve;
- exact per-block eviction subtraction; and
- row-camera tests spanning top, middle, bottom, rapid jumps, and direction reversals.

Gate: distant ACTB blocks yield before visible/near blocks, and rapid row movement cannot make all
same-locus pages equally active.

### Phase 4 — Materialization authority cutover

Deliverables:

- exact committed row-block coverage propagated from engine to presented epoch and desktop cache;
- production `Covered` / `PartiallyCovered` / `Absent` / `PendingAuthority` behavior;
- camera-only covered scrolling;
- partial intersection presentation plus missing-block scheduling;
- removal of vertical-input full-display invalidation as the ordinary fallback; and
- coherent hover, selection, picking, and multi-panel behavior across block changes.

Gate: an exactly covered vertical movement performs no general display rebuild. A partially or
fully uncovered movement remains responsive, truthful, and nonblank while only current/latest
missing blocks are prepared.

### Phase 5 — Aggregate stress qualification

Automated structural scenarios:

- fresh ACTB;
- ACTB deep scroll → BAP1 → ACTB;
- BAP1 → ACTB;
- ACTB → another chr3 locus → ACTB;
- contained pans and zoom changes after vertical browsing;
- several configured cache budgets and browsing-buffer values;
- multi-panel mate navigation with independent scrollbars; and
- multiple alignment tracks sharing one ledger.

Required automated assertions:

- exact evidence/filter counts and canonical row authority remain complete;
- old-locus bytes never exceed their calculated residual quota;
- foreground admission retains its calculated reserve or returns typed `BudgetLimited`;
- reclaim completes at low water and does not repeat for each member of the already-reserved
  visible demand;
- row-camera changes cause zero BAM acquisitions and zero row-atlas rebuilds;
- normal settled materialization is never `Unknown`;
- covered scroll uses the camera-only path;
- partial/absent coverage schedules only missing current/latest blocks;
- no whole-stack residency reset follows one page eviction;
- no orphan reclaim, permanent retry, critical failure, or stuck worker remains; and
- optional warming cannot consume foreground reserve.

Automated operation counts qualify architectural behavior. They do not claim that native motion
looks correct.

### Phase 6 — Native user acceptance

Launch the release application with the RNA BAM and diagnostic trace enabled. The user, not an
automated visual driver, assesses:

1. fresh ACTB vertical dragging at ordinary and extreme speeds;
2. stopping while holding the scrollbar;
3. repeated top/middle/bottom jumps;
4. contained horizontal pans and zoom changes;
5. ACTB → BAP1 → ACTB after deep vertical browsing;
6. coverage, substitutions, Sashimi, hover, selection, inspection, context menus, and mate panel;
7. scrollbar extent, density preview, thumb responsiveness, and per-panel independence; and
8. absence of white flashes, progressive slowdown, stale pixels, or permanent status overlays.

The saved trace must then confirm the same structural properties. If visual behavior fails, the
plan remains unaccepted even when automated tests pass.

## 9. Likely implementation areas

Expected production boundaries include:

- `crates/rgv-alignment-scene/src/resource.rs`;
- `crates/rgv-alignment-scene/src/store.rs`;
- `crates/rgv-alignment-scene/src/presentation_store.rs`;
- `crates/rgv-engine/src/alignment_scene_coordinator.rs`;
- `crates/rgv-engine/src/alignment_scene_engine.rs`;
- `crates/rgv-render/src/lib.rs` and alignment renderer residency directories;
- `apps/rgv-desktop/src/workspace/alignment_management.rs`;
- `apps/rgv-desktop/src/workspace/panels.rs`;
- `apps/rgv-desktop/src/display/mod.rs`;
- `apps/rgv-desktop/src/ui/interaction.rs`;
- `apps/rgv-desktop/src/manual_diagnostic_instrumentation.rs`;
- `apps/rgv-desktop/src/workspace/queries.rs`; and
- their unit, integration, source-inventory, and trace-schema tests.

Cache policy and row-block authority belong below desktop UI code. The desktop expresses demand,
camera intent, and presentation state; it cannot independently decide cache ownership.

## 10. Definition of done

This recovery is complete only when:

1. Protection and cache priority are separate, explicit, and byte-accounted.
2. Visible, near, cold-current, return-cache, and speculative ownership are distinguishable.
3. Old loci use only residual byte capacity after current foreground obligations.
4. Disjoint navigation proactively enforces that residual quota.
5. Reclaim targets measured low water, not the immediate failed allocation threshold.
6. Evidence becomes eligible when its lower-priority class exceeds quota.
7. Reclaim remains bounded, preemptible, cancellable, and ownership-safe.
8. Row-camera movement reclassifies exact blocks without reopening unchanged source/atlas work.
9. A committed scene propagates exact materialized row-block coverage to every desktop consumer.
10. Covered vertical movement uses camera-only presentation.
11. Partial/absent movement preserves coherence and requests only current/latest missing blocks.
12. Normal native materialization never remains `Unknown` after a settled commit.
13. Complete alignments, row identity, derived lanes, and interaction features remain exact.
14. The trace-derived 4 GiB carry-over scenario passes repeatedly without allocation-by-allocation
    reclaim.
15. No progressive slowdown, locked scrollbar, full-panel blanking, critical failure, or permanent
    `Updating view…` remains.
16. The user accepts the final native visual behavior.

## 11. Explicit non-solutions

Do not resolve this work by:

- increasing the default cache or requiring more RAM;
- clearing every cache unconditionally on any navigation;
- assigning a fixed ACTB-specific return-cache percentage;
- sampling, hiding, or truncating alignments;
- lowering the Complete-read limit;
- preserving every row derivative merely because the canonical evidence is complete;
- treating all same-locus rows as equally active;
- continuing one-allocation-at-a-time reclaim with a larger quantum;
- treating `None` materialization as covered;
- marking the complete display dirty for every uncovered scroll sample;
- retaining a stale texture or synthetic read image as a replacement for exact scene authority;
- adding blind retry loops, timeouts, sleeps, or failure-suppressing status messages; or
- accepting unit tests or a fresh ACTB run as proof that carry-over behavior is fixed.
