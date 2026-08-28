# Canonical stable read-architecture implementation plan

> **Complete-read amendment (2026-08-28):** The sampling and truncation policy in this historical
> architecture plan is superseded by
> [Complete read presentation](COMPLETE_READ_PRESENTATION_PLAN.md), as amended by
> [dense-alignment performance](DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md). Foreground batches and
> every identity-bearing seed must be Complete. An over-limit foreground yields no individual-read
> presentation; an over-limit preferred wider horizon instead falls back to a Complete smaller
> horizon or the Complete exact foreground and is reported as truncated. Deterministic ordering
> remains only a packing mechanism.

Status: implemented and qualified

Plan date: 2026-08-28

Depends on: [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md)

Implementation result: Stable mode now builds a deterministic, top-first row architecture from a
fixed, tiled, layout-only horizon and projects the rich visible query onto that architecture. The
horizon and visible query commit atomically; extensions preserve incumbents and report a typed
transition. Compact mode uses the same improved scratch packer without changing stable history,
and the former one-shot compact action is now **Rebuild stable architecture**.

Final qualification on 2026-08-28:

- The large `ENCFF686MTA.bam` horizon at `chr1:36274176-36405248` loaded 39,211 layout-only
  identities in 27.9 ms and 5.39 MB without truncation. It produced 1,103 canonical rows, zero
  top-first violations, zero visible mismatches, and identical narrow-first and broad-first row
  order. Reconciliation took 24–33 ms and retained zero moved shared reads.
- Broad → narrow → broad navigation retained all 1,262 shared identities: an independent scratch
  pack moved 944 of them, while Stable mode and the restored broad projection moved zero.
- At the coarse `chr1:36292459-36386166` locus, an expanded prefetch query contained 83,406
  eligible records and hit the 50,000 safety cap. Viewport-priority sampling retained all 21,965
  records from the requested interval; canonical-horizon sampling remains viewport-independent.
- `cargo test --workspace --all-targets --quiet` and `cargo fmt --all -- --check` passed. The
  reviewed render golden changed by 44 pixels at deterministic equal-start ordering boundaries and
  was inspected before its hash was updated.
- The release desktop build was opened on the large RNA BAM at the motivating locus. The resulting
  wide-view organisation was accepted in native visual review; structured row and horizon metrics
  remain the correctness oracle.

## Purpose

RGV now preserves a read's row across ordinary navigation, but the preserved arrangement can be
visually poor. Stable mode currently adopts the first settled query's packing as the long-lived row
architecture. That packing minimises row count, but it does not deliberately choose the visually
highest reusable row, and later queries must insert or restore reads around frozen incumbents and
reservations. The result can be collision-free and stable while still looking vertically scrambled.

This plan replaces “freeze the first arrangement” with a stronger rule:

> Build a coherent canonical row architecture over a bounded layout horizon, then preserve it.

For a fixed set of interval-like reads, compactness and stability are compatible: sort the records
canonically, assign each record to the topmost compatible row once, and reuse that row map at every
zoom level. The genuine tradeoff arises only when navigation discovers records outside the set used
to establish the architecture. RGV should reduce that uncertainty with a deliberate layout horizon,
measure any remaining organisation debt, and never hide it behind an automatic reflow.

The repeated RNA `N...` labels visible in the motivating screenshots are a separate presentation
issue. This plan concerns vertical read order and row architecture only.

## Observed failure

The motivating native screenshots show the same RNA locus at approximately:

- `chr1:36339309-36343089`, with 2,508 loaded alignments; and
- `chr1:36337134-36345430`, with 6,087 loaded alignments.

The narrower view appears more coherent when packed independently. The wider stable view preserves
rows but contains visually irregular bands and ordering. Before persistent stability, zooming in
discarded the wider layout and freshly packed the smaller subset, making it look organised at the
cost of moving almost every shared read.

The current causes are concrete:

1. `assign_rows_from_scratch` keeps one heap ordered by row end and reuses the row whose previous
   interval ended earliest. It does not release all available rows and choose the smallest row ID.
   This is height-efficient interval partitioning, but it is not top-first visual packing.
2. The first settled, query-derived batch becomes the stable baseline even though the query extent
   depends on viewport scale, tile rounding, navigation direction, cache state, and safety limits.
3. When the known set grows, persistent reconciliation correctly freezes incumbents and
   reservations. New reads use compatible holes, but it cannot retroactively turn an arbitrary old
   baseline into the canonical offline arrangement without moving those incumbents.
4. Reads retained by prefetch or history can own rows even when their geometry is outside the visible
   viewport. This is required for continuity, but its visual cost is currently unmeasured.
5. A configured sort mode orders candidate placement. It does not currently establish or report a
   canonical stable architecture for the complete bounded neighbourhood.

## Product contract

### Stable across navigation

Stable mode must provide both continuity and a coherent initial architecture:

- The first committed layout island is packed canonically, not opportunistically.
- For default start sorting, reads are processed by a deterministic start/end/identity key and use
  the topmost compatible row.
- Every shared identity retains its absolute stable row across zoom, pan, cache, LOD, resize, query
  replacement, filtering, and return navigation while the row policy is unchanged.
- Reads discovered after baseline establishment are inserted into the topmost row compatible with
  every incumbent and reservation; incumbents never move automatically.
- Layout-horizon growth must be observable and must not masquerade as a policy reset.
- Organisation debt may increase after discovering data outside the horizon, but it is diagnostic
  state, never permission to repack automatically.

### Compact each view

Compact mode remains a settled-query-local mode. It uses the same canonical top-first packer so its
output is visually coherent and deterministic, but it is allowed to assign different rows when the
query membership changes.

### Rebuild stable architecture

The existing one-shot `Compact/repack now` action should be renamed or described as **Rebuild stable
architecture**. It explicitly adopts the current canonical top-first layout as a new stable baseline.
It remains a deliberate user action and must not become an automatic response to organisation debt.

## Required invariants

The completed implementation must enforce:

1. **Canonical baseline:** given the same bounded records and row policy, baseline row assignments
   are identical regardless of BAM iteration order, query completion order, or cache state.
2. **Top-first placement:** at canonical placement time, no lower compatible row exists for an
   assigned record within its group.
3. **Optimal default height:** for ungrouped start-order packing of full display spans, the canonical
   baseline row count equals maximum concurrent span depth, subject to the configured record cap.
4. **Shared-row stability:** ordinary navigation reports zero moved shared identities.
5. **Horizon independence:** zooming within an established layout horizon does not change the
   architecture or its policy identity.
6. **Additive extension:** records discovered beyond the horizon are added without moving or
   renumbering incumbents.
7. **Reservation safety:** temporarily absent and prefetched reads cannot have a conflicting row
   stolen.
8. **Group stability:** an absent group does not shift following groups, and new group members use
   the topmost compatible row in their retained group band.
9. **Atomic commitment:** query, row projection, camera, display list, picking, selection, and
   organisation diagnostics share one committed row revision.
10. **No silent repair:** neither high organisation debt nor horizon extension triggers an
    unannounced reflow.

## What “well organised” means

Visual organisation needs machine-checkable semantics rather than screenshot intuition alone.

For default `Start` sorting, a canonical baseline processes representative reads by:

```text
(group order, reference start, reference end, stable identity)
```

Each read receives the smallest compatible absolute row in its group. Compatibility uses the same
display ownership span as rendering and picking, including pair/template extent where pair view is
active. RNA reference skips continue to reserve their displayed connector span unless a separate
block-aware design proves that overlapping connectors remain legible.

For other explicit sort modes, the configured semantic key precedes start/end/identity. These modes
may intentionally sacrifice minimum height to express the requested order, but they must remain
deterministic, top-first relative to that order, and stable within their policy context.

The implementation must report at least these quality measures:

- canonical baseline rows;
- stable row-space extent;
- current visible occupied-row span;
- visible empty-row count and ratio;
- row inflation relative to a canonical pack of the same known records;
- canonical displacement count: known identities whose retained row differs from the current
  offline canonical row;
- top-first placement violations;
- horizon interval, generation, extension count, and truncation state; and
- moved shared identities, which remains zero for navigation.

Canonical displacement is an organisation-debt measurement, not a request to move reads.

## Feasibility boundary

There is no compactness-versus-stability conflict when the relevant record set is known. A globally
canonical interval layout can be computed once and reused at every scale.

There is an information boundary when a later query reveals reads not included in the baseline. An
online algorithm cannot guarantee all three of the following for arbitrary arrivals:

1. retain every incumbent's absolute row;
2. match the optimal offline canonical layout of the enlarged set; and
3. reserve no unused structure in advance.

RGV will therefore establish a bounded neighbourhood before committing the baseline, extend it
additively when necessary, and expose the resulting debt. Whole-contig BAM preprocessing is not a
prerequisite for this work, though a future optional layout index could provide exact global
canonicality.

## Target architecture

### 1. Canonical top-first packer

Replace earliest-ending-row reuse in the baseline path with deterministic lowest-row reuse.

For start-ordered representatives, use two heaps per group:

```text
active_by_end: min-heap of (end, row)
available_rows: min-heap of row

for read in canonical order:
    release every active row with end <= read.start into available_rows
    row = smallest available row, or allocate the next contiguous row
    assign read to row
    push (read.end, row) into active_by_end
```

Releasing every ended row before choosing the smallest ID is essential. It retains optimal interval
height while producing top-first lanes. Pair representatives, groups, capacity limits, promoted
records, and explicit sort modes need typed variants rather than a fallback to BAM order.

Use this packer for:

- a new Stable island baseline;
- Compact mode settled queries;
- one-shot stable-architecture rebuilds; and
- offline organisation-debt comparison.

### 2. Stable layout horizon

Separate three intervals that currently risk being conflated:

- **viewport interval:** what the user sees;
- **data query interval:** what the engine fetched for rendering and prefetch; and
- **layout horizon:** the fixed bounded neighbourhood used to establish a canonical row island.

When entering individual-read detail at a disjoint locus or row policy:

1. Select a tile-aligned layout horizon that contains the data query interval and provides a
   measured navigation margin on both sides.
2. Query layout-level alignment fields for that horizon using the track's exact eligibility and
   sampling policy.
3. Build the canonical baseline from the settled horizon records.
4. Atomically commit the visible query projection against that baseline.
5. Keep the horizon fixed while zooming or panning inside it.

The initial constants must be chosen from benchmarks, not guessed. Candidate policy:

- at least the current expanded query;
- multiple adjacent fixed layout tiles on both sides;
- a byte/record budget independent of window width;
- no sequence, qualities, or large auxiliary payload solely for row construction; and
- a stricter remote-I/O budget with cache-aware progressive extension.

The horizon belongs to one panel, alignment track, contig, and row-policy context. It is runtime
layout state, not persisted record data.

### 3. Additive horizon extension

When navigation approaches or crosses a horizon edge:

- request the missing adjacent layout tile without invalidating the current baseline;
- merge stable identities and reservations;
- process genuinely new representatives in canonical order;
- assign each to the lowest compatible retained row;
- extend row space only when no retained row fits; and
- commit the extension and visible projection atomically.

An extension increments a typed horizon generation and records added reads and organisation debt.
It must report `moved = 0`. Returning to a previously covered interval restores the same rows.

### 4. Stable architecture versus visible projection

`AlignmentRowHistory` remains the authority for stable identity-to-row assignments. Add explicit
horizon metadata to each island rather than treating the latest query interval as its definition.

The visible `AlignmentRowProjection` contains only current query indexes mapped onto stable rows.
Rows owned solely by offscreen horizon records or reservations remain real architecture, but display
and camera code should distinguish:

- stable row-space extent;
- rows occupied by current query records;
- rows with geometry intersecting the viewport; and
- rows occupied only by retained reservations.

This distinction supports honest gap diagnostics without weakening stability.

### 5. Canonical sampling and identity

The same source record must not enter or leave the layout set merely because viewport-dependent
safety sampling changed. Layout-horizon queries need a deterministic, horizon-qualified record
selection policy.

- Below the safety cap, retain every eligible representative in the horizon.
- At the cap, sample by stable identity and fixed layout tile rather than current viewport.
- Preserve paired representatives and supplementary identity semantics.
- Report truncation and prevent claims of global canonicality for omitted records.
- Filters may hide current records while preserving their stable horizon reservations.

### 6. Policy and algorithm versioning

Add a typed row-architecture algorithm version to the runtime policy fingerprint and diagnostics.
It prevents a legacy earliest-end baseline from being mistaken for a canonical top-first context
during development or hot state restoration. The version is not a user setting.

Changing grouping, semantic sorting, fixed sort anchor, pair mode, or dependent pair bounds still
selects a separate policy context. Colour, labels, read density, panel geometry, viewport, and data
query interval remain outside row policy.

### 7. UI and observability

Keep the two persistent stacking modes. Update explanatory copy so Stable describes both continuity
and canonical baseline behaviour.

Suggested copy:

```text
Stable across navigation
Builds an organised row architecture and keeps each read on its row.

Compact each view
Rebuilds the current view for density; reads may move while navigating.

Rebuild stable architecture
Use the current known neighbourhood as a new organised stable baseline.
```

Automation and diagnostics should expose the horizon, architecture version, quality measures,
extension reason, and rebuild reason. Do not add an automatic “tidy when messy” mode.

## Event contract

| Event | Required architecture behaviour |
| --- | --- |
| Initial Reads view | Settle bounded horizon, then commit canonical top-first baseline |
| Zoom within horizon | Preserve architecture and shared rows exactly |
| Pan within horizon | Preserve architecture and shared rows exactly |
| Query/cache replacement | Reproject onto the same architecture atomically |
| Zoom or pan beyond horizon | Add adjacent horizon data without moving incumbents |
| Return navigation | Restore retained rows and horizon generation |
| Filter apply/clear | Hide/restore through reservations; no compacting |
| Window, pane, or density change | Preserve rows and row-anchored camera |
| Semantic group/sort/pair change | Select or create the matching canonical policy context |
| Compact mode | Canonically top-first pack each settled query; stable history untouched |
| Rebuild stable architecture | Explicitly replace active baseline and report the reset |
| Record-cap truncation | Keep deterministic tile-qualified sample and report incomplete horizon |

## Implementation phases

### Phase 0: Reproduce and quantify organisation

- Add a deterministic interval fixture that demonstrates earliest-end reuse selecting a lower visual
  lane even though row 0 is free.
- Record the two motivating RNA loci, loaded/visible counts, row maps, horizon/query intervals, and
  native captures.
- Add organisation metrics to the current and proposed packers.
- Establish screenshot-independent assertions for top-first violations, canonical displacement,
  visible gaps, row height, and moved identities.
- Confirm whether the current poor broad layout originates in baseline packing, later horizon
  extension, retained reservations, or a combination, and quantify each contribution.

**Exit criteria**

- A test fails under earliest-end reuse and passes under top-first reuse.
- The large RNA locus has a structured before baseline, not only subjective screenshots.
- Metrics separate continuity (`moved`) from organisation quality.

### Phase 1: Implement the canonical top-first baseline

- Replace the scratch packer's one-heap reuse with release-all plus lowest-free-row selection.
- Preserve deterministic start/end/stable-identity ordering.
- Route Stable baseline creation, Compact mode, explicit rebuild, and qualification comparison through
  the same canonical implementation.
- Retain the legacy public render-golden ordering only where it is an intentional rendering contract;
  do not allow it to seed stable row history.
- Add algorithm-version diagnostics.

**Exit criteria**

- Default start packing has zero top-first violations.
- Baseline row count equals maximum concurrent display-span depth.
- Input permutations produce the same identity-to-row map.
- Existing stable-navigation, collision, grouping, pair, picking, and render-golden tests pass.

### Phase 2: Establish a query-independent layout horizon

- Add typed layout-horizon state and generations to each row-history island.
- Define and benchmark local and remote horizon selection budgets.
- Add a bounded layout-only query path that omits unnecessary record payload.
- Decouple horizon identity from viewport and directional data-prefetch intervals.
- Make safety sampling stable by horizon tile and identity.
- Delay only the first baseline commit required for canonicality; retain existing content during later
  extensions.

**Exit criteria**

- Starting narrow inside a horizon produces the same row map as starting at its broad extent.
- Zooming anywhere inside the horizon performs no architecture mutation.
- Cache hit/miss and query completion order do not affect the baseline.
- Initial latency and remote bytes remain inside measured budgets.

### Phase 3: Extend horizons without degrading continuity

- Fetch adjacent layout tiles as navigation approaches the horizon edge.
- Insert new identities top-first relative to fixed incumbents and reservations.
- Track organisation debt against an offline canonical layout of all currently known records.
- Preserve group bands, pair spans, row camera, selection, and picking revision atomically.
- Protect active and navigation-referenced horizons under the existing history budget.

**Exit criteria**

- Every extension reports zero moved shared identities and zero collisions.
- Repeated left/right extension and return navigation restore exact maps.
- Organisation debt is deterministic, bounded, and observable; it never triggers automatic reflow.
- Disjoint islands and multiple panels/tracks remain independent.

### Phase 4: Complete UI, automation, and explicit rebuild semantics

- Update Stable/Compact descriptions and rename the one-shot action to **Rebuild stable
  architecture**.
- Expose architecture version, horizon interval/generation, truncation, extension count, top-first
  violations, displacement, row inflation, and visible-gap metrics through typed automation.
- Distinguish `InitialCanonical`, `HorizonExtension`, `PolicyContext`, `CompactMode`, and
  `ExplicitRebuild` transition reasons.
- Document when exact offline canonicality cannot be guaranteed because the horizon was truncated or
  extended after baseline creation.

**Exit criteria**

- UI copy accurately describes guarantees and never suggests invisible auto-tidying.
- Persistent mode and one-shot rebuild remain distinct in session, automation, and diagnostics.
- Accessibility exposes the active mode, horizon state, and explicit rebuild action.

### Phase 5: Qualify the complete behaviour

- Run synthetic property tests across random overlapping intervals, horizon edges, filters, policy
  switches, query permutations, and record-cap boundaries.
- Run the existing 3,655 → 1,262 broad/narrow/return stability qualifier.
- Add the 2,508 → 6,087 RNA organisation reproducer from the motivating screenshots.
- Compare current versus canonical metrics and native captures at narrow, broad, and restored views.
- Stress 50,000-record local horizons and bounded remote horizons.
- Update the user guide and the completed stability plan with the final architecture contract.

**Exit criteria**

- All navigation-only transitions report `moved = 0`.
- Canonical baselines report zero top-first violations and deterministic maps.
- Narrow-first and broad-first entry within the same horizon produce identical rows.
- The motivating wide RNA view passes structured organisation thresholds and native visual review.
- Reconciliation, first-baseline latency, memory, and remote-byte gates pass.
- The complete workspace suite and reviewed render goldens pass.

## Test matrix

### Core layout tests

- Multiple rows become free simultaneously; the smallest row ID is reused.
- A long-lived row 0 interval followed by shorter rows does not cause permanent lane rotation.
- Equal-start/equal-end duplicates use stable identity ties.
- Input reversal and random permutation preserve the canonical map.
- Baseline height equals maximum concurrent span depth for start sorting.
- Pair representatives and template spans remain collision-free.
- Spliced full-display spans remain collision-free.
- Group bands remain ordered and stable when a group disappears and returns.
- Capacity and deterministic sampling never depend on viewport width.

### History and horizon property tests

- Narrow-first versus broad-first inside one horizon.
- Zoom oscillation with no horizon mutation.
- Alternating left/right horizon extensions.
- New reads fitting old rows and forcing row extension.
- Hidden/read-return reservations with conflicting newcomers.
- Disjoint-locus islands, LRU eviction, and navigation pins.
- Compact → Stable restores canonical stable history.
- Explicit rebuild changes the baseline once and then remains stable.

For every generated sequence:

```text
shared identities keep their rows
overlapping display spans never share a group row
canonical baseline placement has no lower compatible row
horizon extension never changes an incumbent row
the same known set and policy produce the same diagnostics
```

### Desktop integration tests

- First read display waits for and atomically adopts its canonical horizon.
- Retained content remains visible while a horizon extension settles.
- Query, row revision, display list, picking map, inspector, and camera agree.
- Cache hit versus miss and asynchronous completion permutations.
- Window maximisation, Properties expansion, pane resize, and density changes.
- Multiple alignment tracks and panels at overlapping and disjoint loci.
- Session round-trip retains the mode without persisting private records.
- Typed automation distinguishes extension from explicit rebuild.

### Large RNA-BAM qualification

Using local `ENCFF686MTA.bam`:

1. Enter the narrow motivating locus and record the chosen layout horizon.
2. Assert its baseline matches a direct canonical pack of that complete horizon.
3. Zoom to `chr1:36337134-36345430` and assert zero moved shared reads.
4. Record top-first violations, visible gaps, displacement, row height, reconciliation time, history
   memory, horizon extensions, and truncation.
5. Return narrow and assert the complete original map and camera anchor.
6. Repeat broad-first and assert the same map when both starts use the same horizon.
7. Compare native narrow/broad/restored captures for coherent vertical lanes.

The splice-label presentation issue must not be used to accept or reject row-architecture metrics.

## Performance and resource gates

- Canonical top-first packing should remain `O(n log r)`, where `r` is allocated row count.
- Stable reconciliation remains proportional to the current horizon delta plus relevant reservation
  indexes, not every read ever seen on the contig.
- The layout-only horizon must not retain sequence, qualities, or unneeded auxiliary tags.
- Default local first-baseline latency must remain within the interactive query-settlement budget at
  50,000 records.
- Remote horizon expansion must have an explicit request/byte cap and reuse the range/query cache.
- History bytes, layout-query bytes, packing time, extension time, canonical-comparison time, and
  organisation metrics must be observable.
- No active or navigation-pinned island may be evicted to satisfy the inactive history budget.

## Risks and mitigations

### Larger initial query delays first display

Measure layout-only queries separately from full read-detail queries, omit heavy payload, cap the
horizon by records/bytes, and commit atomically. Do not choose a large fixed base-pair span without
density-aware qualification.

### Remote sources make proactive horizons expensive

Use smaller cache-aligned remote horizons and progressive adjacent extension. Report when the
architecture is locally canonical rather than pretending it covers unseen data.

### Record caps undermine canonicality

Sample by stable identity inside fixed layout tiles, not viewport. Report truncation and qualify
stability for the retained deterministic set.

### Explicit sort modes conflict with minimum height

Treat the user-selected sort key as semantic priority, use top-first compatible placement within
that order, and report height inflation. Default start sorting retains the stronger optimal-height
guarantee.

### Horizon extension cannot recreate an offline optimum without movement

Preserve incumbents, measure displacement debt, and keep rebuild explicit. A future optional
whole-contig layout index may remove this boundary for repeated review workflows.

### “Topmost” conflicts with retained group ownership

Define topmost within the stable group band and retained policy context. Never reuse a row owned by
another group merely because that group is temporarily absent.

## Likely code areas

- `crates/rgv-layout/src/alignment_layout/rows.rs`
- `crates/rgv-layout/src/alignment_layout.rs`
- `crates/rgv-layout/src/alignment_layout_tests.rs`
- `crates/rgv-engine/src/alignment_engine.rs`
- `crates/rgv-hts/src/local.rs`
- `crates/rgv-hts/src/sampling.rs`
- `apps/rgv-desktop/src/workspace/alignments.rs`
- `apps/rgv-desktop/src/workspace/queries.rs`
- `apps/rgv-desktop/src/display/mod.rs`
- `apps/rgv-desktop/src/display/alignment.rs`
- `apps/rgv-desktop/src/ui/frame.rs`
- `apps/rgv-desktop/src/automation/commands.rs`
- `crates/rgv-core/src/automation.rs`
- `docs/USER_GUIDE.md`

## Completion definition

This work is complete only when:

- the stable baseline is canonical, deterministic, top-first, and minimum-height for default start
  sorting;
- Stable mode retains exact shared rows across all ordinary navigation and query transitions;
- starting narrow or broad inside the same layout horizon produces the same row architecture;
- horizon extension adds reads without moving incumbents and reports deterministic organisation debt;
- Compact mode uses the improved packer without mutating stable history;
- **Rebuild stable architecture** is the only operation that deliberately replaces the active
  baseline without a row-policy change;
- grouping, sorting, pair view, reservations, camera, display, picking, and selection remain
  revision-consistent;
- the motivating RNA screenshots are replaced by structured metrics and native captures showing a
  coherent wide-view vertical arrangement;
- local, remote, capped, multi-panel, and multi-track performance/resource gates pass; and
- the full workspace tests, format checks, and reviewed render goldens pass.
