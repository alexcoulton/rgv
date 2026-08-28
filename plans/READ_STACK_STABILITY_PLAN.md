# Persistent read-stack stability plan

> **Complete-read amendment (2026-08-28):** References below to safety sampling or cap crossings
> are superseded by [Complete read presentation](COMPLETE_READ_PRESENTATION_PLAN.md). A sampled
> identity set may not define rows or reservations. Stability applies only to complete admitted
> foreground and canonical-horizon data.

Status: implemented and qualified

Plan date: 2026-08-28

Implementation result: the checked-in deterministic reproducer and the opt-in local
`ENCFF686MTA.bam` qualification both report zero moved shared reads in Stable mode and zero moved
rows after restoring the broad view. The local qualification observed 3,655 broad records, 1,262
narrow/shared records, 944 scratch-packed moves, zero stable moves, zero restored-broad moves,
one layout island, 292,528 estimated history bytes, zero evictions, and zero narrow-view row
inflation (184 stable rows versus 184 scratch rows). The three stable reconciliations took roughly
1–3 ms each in the final release run. Run it with
`cargo run -p rgv-layout --example read_stack_qualification --release`.

The follow-on [canonical stable read-architecture implementation](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
now establishes that persistent history from a deterministic, top-first, layout-only horizon rather
than from the first opportunistic viewport query.

Final qualification on 2026-08-28:

- `cargo test --workspace --all-targets` passed, including the existing reviewed render goldens and
  the new deterministic, random-navigation, reservation, compact-mode, repack, camera, cache,
  session, automation, and desktop-integration coverage.
- `cargo check --all-targets` and `cargo fmt --all -- --check` passed.
- The release desktop build was opened with `ENCFF686MTA.bam` at
  `chr1:36323329-36347904`; the native annotation and read panes loaded the dense RNA locus.
- The indexed large-BAM qualifier exercised broad → narrow → broad reconciliation and asserted
  all row maps directly. Screenshots remain supporting visual evidence rather than the correctness
  oracle.

## Purpose

This plan makes read stacking a stable part of the user's visual context. Zooming, panning, query
prefetching, level-of-detail changes, filtering, resizing, or asynchronous data replacement must not
silently move a read that the user already saw. Navigation changes which evidence is visible; it
does not redefine the vertical arrangement of evidence shared by the old and new views.

The immediate defect was reproduced with `ENCFF686MTA.bam` while zooming from a query containing
3,655 alignments to one containing 1,262 alignments. The narrow query contained 1,262 identities
already present in the broad query. The core history-aware packer preserved all 1,262 rows when it
received the previous layout, but the desktop discarded that history because query-local inferred
template-length bounds changed from `0–526 bp` to `0–398 bp`. Packing from scratch changed 1,254 of
the 1,262 rows.

Removing that incorrect invalidation is necessary, but it is not sufficient. The target is a
general stability contract that remains true across multiple query generations, temporary absence,
return navigation, loading transitions, and other non-semantic presentation changes.

## Governing principle

> Navigation changes what is visible, not where an already-known read is stacked.

Read rows are persistent presentation state. A query supplies records and evidence; it does not own
the user's row arrangement. Query-local statistics may change rendering or status text, but they may
not invalidate row state unless they are part of an explicit, stable layout policy.

Stable stacking is the default and takes precedence over minimum stack height. The implementation
may leave gaps or use more rows than a fresh optimal packing. Users who prefer maximum density can
explicitly select compact stacking for a track. Compaction is never an unannounced consequence of
zooming or data refresh while stable stacking is active.

## Required invariants

The completed system must enforce these invariants whenever **Stable across navigation** is active.
Compact mode explicitly relaxes shared-identity, reversible-navigation, and no-compaction guarantees,
but it must remain deterministic, collision-free, atomic, and consistent across rendering and
picking.

1. **Shared-identity stability:** if an alignment identity occurs in two views connected by ordinary
   navigation and the layout policy has not changed, it has the same stable row in both views.
2. **Reversible navigation:** broad → narrow → broad navigation restores the original broad
   arrangement, including reads temporarily absent from the narrow query.
3. **Additive discovery:** newly discovered reads are placed into non-conflicting rows without moving
   existing or reserved reads.
4. **No automatic compaction:** reads disappearing because of zoom, filtering, query bounds, or
   safety sampling do not cause surviving reads to move upward.
5. **Atomic query handoff:** replacing a settled query may add and remove reads, but it may not
   transiently display a scratch-packed arrangement.
6. **Stable vertical camera:** if the user does not scroll vertically, navigation preserves the
   stable row at the same offset within the read pane wherever the surrounding shell geometry permits.
7. **Policy-controlled reflow:** only an explicit row-semantic change or explicit repack operation
   starts a new arrangement.
8. **Deterministic restoration:** returning to a retained layout context produces the same rows
   independently of query completion order, cache hits, or BAM iteration order.
9. **Collision safety:** no two reference-overlapping alignment spans occupy the same row within one
   group, including rows restored from history and rows reserved for temporarily absent reads.
10. **Rendering/picking agreement:** drawing, hit testing, selection, export, automation, and
    diagnostics consume the same row assignment and layout revision.

## Behaviour contract for stable stacking

| Event | Required row behaviour |
| --- | --- |
| Zoom in or out | Preserve every shared read's row |
| Pan across overlapping intervals | Preserve every shared read's row |
| BAM/CRAM prefetch tile changes | Preserve rows |
| Query cache hit or miss | Preserve rows |
| Reads appear after zooming out | Add without displacing known reads |
| Reads disappear after zooming in | Leave gaps; do not compact |
| Return through navigation history | Restore the retained arrangement |
| Cross Reads/Bases detail levels | Preserve rows |
| Enter Overview/Regional without individual reads | Suspend the stack and restore it on return |
| Resize the window, Properties panel, or track panes | Preserve rows and vertical camera anchor |
| Change colour, labels, base marks, or read density | Preserve rows |
| Apply or clear a filter | Preserve surviving rows and reservations for hidden reads |
| Change coverage, sashimi, or annotation presentation | Preserve rows |
| Explicitly change grouping or sorting | Start or restore the corresponding layout-policy context |
| Toggle individual-read/pair view | Start or restore the corresponding layout-policy context |
| Explicitly select **Compact/repack now** | Deliberately establish a new compact stable baseline |
| Jump to a disjoint locus | Start or restore a separate layout island; do not contaminate the old island |

## User-facing stacking strategy

Expose a per-alignment-track **Read stacking** setting with exactly two modes:

| Mode | Contract |
| --- | --- |
| **Stable across navigation** | Preserve known rows across navigation and query changes; accept gaps and a potentially taller stack |
| **Compact each view** | Repack each settled query for minimum height; accept that reads can move when query membership changes |

`Stable across navigation` is the default for new tracks and migrated sessions. It reflects RGV's
evidence-review priority: users should remain oriented unless they deliberately choose density over
continuity. `Compact each view` is useful for surveying very deep data where fitting more evidence
on screen is more important than tracking the same row through navigation.

The setting must follow these rules:

- It is stored per alignment track, persisted in the workspace/session, copied by compatible bulk
  appearance operations, and exposed through accessibility and typed automation configuration.
- Switching to compact mode is an explicit layout transition. Compact mode scratch-packs only when
  a settled query or row-semantic policy changes; it must not introduce an intermediate loading
  reflow.
- Switching to compact mode does not destroy the track's retained stable histories. Switching back
  restores the matching stable policy context where it is still retained.
- Stable mode provides a separate **Compact/repack now** action. It discards the active island's old
  reservations, packs the current query compactly, and establishes that result as the new stable
  baseline without changing modes.
- The one-shot action must be distinguishable from the persistent `Compact each view` mode in UI,
  session state, automation reports, and diagnostics.
- Do not add a hybrid automatic mode such as “compact when convenient.” A transition whose timing is
  unpredictable would recreate the orientation problem.
- A disjoint locus may begin with a fresh compact baseline in stable mode because it shares no reads
  with the active island; this does not weaken continuity within either island.

Suggested UI copy:

```text
Read stacking
  Stable across navigation  Keeps reads on the same rows; may leave gaps.
  Compact each view         Uses less vertical space; reads may move when navigating.

Compact/repack now          Make the current arrangement the new stable baseline.
```

## Scope and terminology

### Stable alignment identity

The existing dual-hash identity plus duplicate occurrence is the starting point. The identity must
remain independent of query interval, viewport, detail level, iteration order, record cap, and
presentation. Pair view uses one stable template representative rather than two record identities.

Duplicate occurrences require special care. An occurrence number derived only from the current
batch can change when identical records outside the query disappear. The implementation must either
prove that the current ordering is query-stable for identical records or strengthen the identity
with stable source-disambiguating fields. Tests must include duplicated records and supplementary
segments with repeated read names.

### Layout policy

A layout policy contains only values capable of intentionally changing row membership, order, or
span ownership. It should include:

- read-stacking strategy, while retaining stable histories separately from ephemeral compact
  projections so switching back can restore them;
- grouping mode and group order;
- sorting mode, sorting direction, and any fixed sorting anchor;
- individual-read versus pair view;
- stable pair-classification bounds only when the selected grouping or sorting actually uses pair
  classification;
- promoted-read behaviour if promotion reserves or relocates rows; and
- any future user-selected packing strategy.

It must not include:

- viewport interval or pixel width;
- query/prefetch interval;
- query-local inferred statistics that do not affect the active row semantics;
- loaded record count or row capacity;
- colour, base display, labels, annotation height, density, or scroll position;
- source cache generation; or
- Properties/window geometry.

The policy has a typed equality/fingerprint used by row history. The display-list cache may have a
broader key because painting can change without row assignment changing. Row-layout compatibility
and render-layout compatibility must not share one catch-all invalidation predicate.

### Layout island

A layout island is a connected set of queried reference intervals for one panel, alignment track,
contig, and layout policy. Overlapping or containing navigation extends the active island. A
disjoint jump opens another island. This bounds collision checking and history growth without
weakening the zoom/pan guarantee inside the active context.

## Architectural design

### 1. Separate row state from the current query

Replace the current single `CachedAlignmentRows { query, layout, ... }` authority with two layers:

1. `AlignmentRowHistory`, persistent across query replacement and owned by the panel/track layout
   context.
2. `AlignmentRowProjection`, the current query's record-index-to-stable-row projection.

The history should retain compact entries similar to:

```text
StableAlignmentIdentity -> {
    group identity,
    row within group,
    reference start and end,
    pair/template representative where applicable,
    last-seen generation,
    active or reserved state
}
```

The projection is rebuilt for each query but must reconcile against history rather than replacing
it. Display object IDs remain query-local; stable row IDs do not.

### 2. Preserve reservations for temporarily absent reads

One-step history is insufficient. Consider broad → narrow → broader navigation:

- the first broad query assigns read A to row 7;
- the narrow query does not contain A;
- another read appears while A is absent;
- returning broad must restore A to row 7 without moving the other read.

The active island therefore retains A's interval and row as a reservation. New records may reuse a
row only when they do not conflict with active or retained reservations relevant to that island.
Reservations can share a row when their reference intervals do not overlap.

### 3. Reconcile a query without moving incumbents

For each settled query:

1. Resolve stable identities and pair representatives.
2. Select or create the active layout island for the contig and policy.
3. Freeze rows for identities already known to the island.
4. Validate restored assignments for group membership and interval collision. A violation is a
   correctness error with diagnostics, not permission to silently repack the whole stack.
5. Restore returning identities to their reserved rows.
6. Sort genuinely new identities deterministically using the active layout policy.
7. Place each new identity in the lowest compatible row while treating incumbents and reservations
   as fixed constraints.
8. Extend the row set when no existing row fits.
9. Mark unseen historical entries as reserved rather than deleting them.
10. Produce the current query projection and a reconciliation report containing retained, restored,
    added, hidden, and moved counts. Navigation must report `moved = 0`.

The algorithm must be group-aware. Group offsets should be stable identifiers rather than a compact
prefix recomputed from the groups present in only the current query. A temporarily absent group must
not shift all following groups upward.

### 4. Make inferred layout inputs stable

The reproduced defect occurs because `effective_template_length_bounds` combines user settings with
bounds inferred from the current query. Those inferred bounds naturally vary with locus and query
extent.

Adopt two separate concepts:

- **observed query bounds**, which may update in status and inspection; and
- **layout-policy bounds**, which remain fixed while a pair-classification-dependent layout policy
  is active.

For ordinary position stacking, pair bounds are irrelevant and must not participate in row-history
compatibility. If the user chooses `Unexpected pair` grouping or `Pair status` sorting, obtain stable
bounds from explicit settings or freeze a well-defined inferred snapshot for that policy context.
Changing or recalibrating those bounds is an explicit policy change.

Other query-derived values must follow the same rule: observational metadata can update freely;
layout-affecting values must be explicit or frozen.

### 5. Give centre-based sorting a fixed anchor

`Base at centre` is currently described in viewport-relative terms. A viewport-relative sorting
position conflicts with stable navigation when zooming around a non-central pointer or panning.

When the user invokes this sort, capture one fixed genomic position as part of the layout policy.
Keep it stable until the user deliberately chooses another sort position or another sorting mode.
Expose the active position visually and in accessibility/automation state. Zooming must not silently
change it.

### 6. Preserve the vertical camera by stable row

Row stability alone is insufficient if the read pane's scroll offset is rescaled from total content
height after every query. Store a camera anchor containing:

- stable row or nearest surviving stable row;
- offset within that row;
- fallback scroll fraction only when no historical row is available; and
- layout-island/policy identity.

After reconciliation, project the anchor into the new content geometry. Do not preserve only a raw
fraction when row count changes. Window and pane resizing may change pixels per row, but the anchored
read should remain at the same relative position in the read pane.

### 7. Commit query, rows, camera, and picking atomically

The visible transition from retained data to a new settled query must commit together:

- current query;
- current row projection;
- stable history revision;
- vertical camera projection;
- display list and picking map; and
- inspector/selection rebinding.

Until reconciliation finishes, continue rendering the retained query clipped to the new viewport
when it overlaps. Never expose an intermediate scratch-packed generation.

### 8. Define explicit reset and compaction semantics

Create one typed row-policy transition path. It decides whether to retain, switch, or reset a layout
context and records the reason in diagnostics.

Expected explicit reflow operations are:

- switching between **Stable across navigation** and **Compact each view**;
- choosing a different grouping or sorting policy;
- choosing a new fixed sort anchor;
- toggling individual-read/pair view;
- changing pair-classification bounds while using a dependent layout policy; and
- invoking **Compact/repack reads**.

Where practical, cache separate contexts by policy fingerprint so switching back restores the
earlier arrangement. Track removal, workspace replacement, or source replacement may discard the
associated history. Colour, filter, density, annotation, resize, and query changes are not reset
operations.

Compact mode should use a query-local scratch projection and must not mutate stable history.
**Compact/repack now**, by contrast, intentionally replaces the active stable island with the
current scratch-packed projection and starts a new stable history revision. Both operations must
record distinct typed reset reasons.

### 9. Bound memory without breaking active navigation

History storage must be measured and bounded, but the active island must never be evicted while
zooming or panning within it.

- Store compact identity, interval, group, and row data rather than records, sequence, tags, or CIGAR.
- Maintain separate islands for disjoint loci and layout policies.
- Retain islands referenced by active navigation-history entries.
- Apply an LRU/byte budget only to inactive, disjoint islands.
- Expose history bytes, island count, reservations, and eviction count in diagnostics.
- If an old disjoint island was evicted, returning to it may create a new arrangement, but this must
  never occur during an overlapping zoom/pan chain. Consider a bounded user-visible notice if an
  explicit return cannot restore an evicted context.

The implementation should measure realistic memory use before setting a default budget. A few
thousand compact assignments should be inexpensive; whole records must never be retained solely for
layout.

## Current architectural cause

The current primary-track flow computes query-local inferred pair bounds in
`Viewer::effective_template_length_bounds`, stores them in `CachedAlignmentRows`, and requires exact
equality before passing the previous `AlignmentRowLayout` to `assign_alignment_rows`. When zooming
changes the query sample, the inferred interquartile bounds change. The desktop then supplies
`previous = None`, and the core packer follows `assign_rows_from_scratch`.

The same broad compatibility pattern exists in secondary alignment rendering and should be audited
with the primary path. The core `AlignmentRowLayout::compatible_with` already avoids treating pair
bounds as universally row-relevant, but it has only immediate previous-layout history and cannot by
itself implement reversible navigation or reservations.

Likely code areas include:

- `apps/rgv-desktop/src/display/mod.rs`
- `apps/rgv-desktop/src/display/alignment.rs`
- `apps/rgv-desktop/src/workspace/alignments.rs`
- `apps/rgv-desktop/src/workspace/queries.rs`
- `apps/rgv-desktop/src/ui/interaction.rs`
- `apps/rgv-desktop/src/inspection/inspector_controller.rs`
- `crates/rgv-layout/src/alignment_layout.rs`
- `crates/rgv-layout/src/alignment_layout/rows.rs`
- `crates/rgv-hts/src/pair.rs`

## Implementation phases

### Phase 0: Codify the contract and reproduce the failures

- Add a compact RNA-style BAM fixture with dense exon coverage, long reference skips, paired reads,
  duplicates, and supplementary records. Do not add the 9.3 GB qualification BAM to CI.
- Reproduce the exact broad/narrow mechanics with query-local inferred bounds that differ.
- Add a failing desktop-level test showing that ordinary position stacking discards history solely
  because inferred pair bounds changed.
- Add a reconciliation test proving that all shared identities can retain their rows for the same
  query pair.
- Add structured diagnostics for row-policy fingerprint, history revision, retained/restored/added/
  moved counts, packed height, active stacking strategy, and reset reason.
- Add session-schema and UI tests establishing `Stable across navigation` as the default and proving
  that the per-track choice round-trips through save/open.
- Record the current large-BAM loci and screenshot evidence as manual qualification inputs.

**Exit criteria**

- The `3,655 → 1,262` failure has a deterministic non-screenshot assertion.
- The test reports 1,262 shared identities and demonstrates that scratch packing moves nearly all of
  them while history-aware packing moves none.
- Every row reset has a typed, observable reason.

### Phase 1: Separate row-policy and render invalidation

- Add the persisted `ReadStackingMode::{Stable, Compact}` setting to alignment-track configuration,
  alignment settings UI, accessibility state, and typed automation.
- Introduce a typed layout-policy key independent of the display-list cache key.
- Remove unconditional pair-bound equality from ordinary row-history compatibility.
- Make pair bounds row-relevant only for pair-classification-dependent grouping or sorting.
- Audit primary and secondary alignment-track paths for equivalent catch-all invalidation.
- Ensure colour, annotation height, density, resize, query interval, and loaded count cannot discard
  row history.
- Route compact mode through deterministic scratch packing without mutating any retained stable
  history.
- Preserve the current core history-aware packing while the persistent history layer is introduced.

**Exit criteria**

- The reported large-BAM zoom no longer repacks shared reads.
- New and migrated tracks default to stable mode; compact mode deliberately reproduces compact
  query-local packing.
- Switching compact → stable restores the retained stable context rather than adopting the latest
  compact projection.
- All existing grouping, sorting, pair, picking, and layout-cache tests pass.
- Changing a genuinely row-semantic setting still produces an intentional policy transition.

### Phase 2: Add persistent history, reservations, and stable groups

- Introduce `AlignmentRowHistory`, `AlignmentRowProjection`, layout islands, and policy-scoped stable
  group identities.
- Reconcile new queries by freezing known assignments before placing new reads.
- Preserve temporarily absent reads as compact reservations.
- Restore returning reads and groups without shifting following groups.
- Retain the largest required row capacity as history metadata without using capacity changes as a
  reason to repack.
- Add **Compact/repack now**, which creates a compact projection and adopts it as a fresh stable
  history revision without changing the persistent stacking mode.

**Exit criteria**

- Broad → narrow → broad restores every original row.
- A newly discovered read cannot steal a reserved conflicting row.
- Filtering and clearing the filter preserves all surviving and returning assignments.
- One-shot repack changes the baseline exactly once; subsequent stable navigation preserves it.
- No ordinary navigation reconciliation reports a moved identity.

### Phase 3: Stabilise derived policies and vertical camera state

- Split observed pair bounds from stable layout-policy bounds.
- Freeze or explicitly configure pair bounds for dependent grouping/sorting.
- Convert centre-based base sorting into a fixed genomic sort anchor.
- Anchor vertical scroll to stable rows rather than total-height fraction.
- Preserve the anchor across query replacement, density changes, pane resizing, and adaptive
  annotation-height changes.
- Define fallback behaviour when the anchor read is filtered, absent, or belongs to an inactive
  group.

**Exit criteria**

- Pair classification statistics may change with the query without moving reads under ordinary
  stacking.
- Pair-dependent layouts remain stable until their policy bounds are explicitly changed.
- Non-central zoom and resize preserve both row identity and the user's vertical evidence position.

### Phase 4: Make query handoff atomic across every consumer

- Reconcile query, row projection, camera, display list, picking, selection, and inspection under one
  committed revision.
- Keep retained overlapping data visible while the next query settles.
- Prevent stale display-list cache entries from pairing a new query with an old record-index map.
- Ensure layout cache hits and misses produce identical stable rows.
- Apply the same model to all alignment tracks and panels.

**Exit criteria**

- No loading frame exposes a scratch layout or mismatched picking target.
- Selected reads remain selected and on the same stable row through zoom.
- Multi-panel and multi-alignment-track navigation does not share or corrupt independent histories.

### Phase 5: Qualification, observability, and documentation

- Add property tests over random overlapping query sequences, subset/superset transitions, and
  return navigation.
- Add large-BAM manual qualification using the captured loci around `STK40`.
- Capture before/after native screenshots and structured diagnostics at broad, narrow, and restored
  broad views.
- Stress long sessions across disjoint loci and measure history memory, reconciliation latency, row
  inflation, and island eviction.
- Document intentional reflow operations and the explicit compact/repack control.
- Expose the active sort anchor and stable-layout reset reason through accessibility and automation.

**Exit criteria**

- Structured evidence shows `moved = 0` for every navigation-only transition.
- Broad and restored-broad captures have identical row assignments for shared identities.
- Reconciliation remains within the interactive frame/query-settlement budget on dense loci.
- Memory remains bounded without any active-island eviction.

## Test matrix

### Core and property tests

- Broad → narrow, narrow → broad, and repeated oscillation.
- Overlapping pan in both directions across prefetch-tile boundaries.
- New reads that fit existing rows and new reads that force row extension.
- Temporarily absent reads with conflicting newcomers.
- Long RNA reference skips spanning multiple exons.
- Duplicate identities, supplementary records, and paired representatives.
- Stable groups with one group temporarily absent.
- Query record count growing and shrinking across the safety cap.
- Determinism under permuted query completion and record iteration order.
- Collision-free assignment after every reconciliation.

Property tests should generate sequences of overlapping query subsets and assert:

```text
for every adjacent or restored view pair using the same policy:
    every shared identity keeps the same stable row
    no overlapping spans share a group row
```

### Desktop integration tests

- Query completion with changed inferred pair bounds under default position stacking.
- Cache hit versus cache miss for the same interval.
- Reads ↔ Bases and Reads ↔ Regional/Overview ↔ Reads transitions.
- Window maximisation, Properties expansion, pane resizing, and annotation adaptive reflow.
- Vertical read scrolling before and after zoom.
- Filter apply/clear and selection retention.
- Explicit group, sort, pair-mode, fixed-anchor, and compact/repack transitions.
- Stable/compact mode switching, per-track independence, save/open persistence, and stable-history
  restoration after time spent in compact mode.
- Multiple panels showing the same track at related and disjoint loci.
- Primary and secondary alignment tracks.
- Rendering, hit testing, and inspector selection agreeing on stable rows.

### Large-BAM qualification

Use the local `ENCFF686MTA.bam` only as an opt-in qualification dataset:

1. Open the established `STK40` locus.
2. Set the broad view reproducing 3,655 loaded and 2,494 visible alignments.
3. Record the row identity map and vertical camera anchor.
4. Zoom to the view reproducing 1,262 loaded and approximately 513 visible alignments.
5. Assert zero moved shared identities.
6. Return to the broad view and assert restoration of the complete retained map.
7. Repeat through cache clear, resize, filter, and LOD transitions that are not policy changes.

Screenshots support human review but do not replace identity/row assertions.

## Performance and resource gates

- Reconciliation should be proportional to the current query plus relevant rows/reservations, not
  all alignments ever seen on the contig.
- Stable placement should use row interval indexes rather than scanning every reservation for every
  new read.
- The active island must remain responsive at the configured 50,000 Complete read limit.
- History memory and reconciliation time must be reported in existing diagnostics.
- Row inflation should be measured separately from natural overlap depth and scratch-packed height.
  Inflation is acceptable for continuity, but pathological growth must be observable.
- An inactive-island memory budget may never trigger an active navigation reflow.

## Risks and mitigations

### Taller or sparse stacks

Stable history can use more rows than an optimal fresh pack. This is expected. Provide explicit
compact mode, one-shot repacking, and diagnostics; do not silently trade orientation for density in
stable mode.

### Unbounded historical reservations

Retaining whole records would be unacceptable. Store compact assignments, partition them into
layout islands, protect active/history-referenced islands, and evict only inactive disjoint islands
under a measured byte budget.

### Identity ambiguity

Query-relative duplicate occurrence numbers can undermine restoration. Strengthen identity before
claiming the invariant if adversarial duplicate tests reveal instability.

### Group and pair-policy drift

Query-local inferred values can alter group membership even when the user changed nothing. Freeze
layout-affecting derived values and separate them from live observational statistics.

### Stale object IDs

Stable rows do not make query-local record indexes stable. Commit the query projection, display
list, picking map, and selection rebinding together.

### Hidden loading transitions

Rendering retained data and settled data through different paths can bypass history. All query
handoffs must use the same reconciliation and commit path, including cache hits.

## Decisions established by this plan

1. `Stable across navigation` is the default and prioritises visual orientation over automatic stack
   compactness.
2. `Compact each view` is an explicit per-track opt-in that prioritises density and permits settled
   query reflow.
3. Stable mode zooming and overlapping panning never intentionally reflow shared reads.
4. Filtering hides evidence but does not compact surviving rows in stable mode.
5. Query-local inferred metadata cannot invalidate ordinary row history.
6. Layout-affecting automatic values must be frozen into a policy context.
7. Persistent compact mode and one-shot **Compact/repack now** are distinct, explicit, and observable.
8. Compact-mode projections never destroy retained stable history; switching back restores it.
9. Active stable history is retained across temporary absence and reversible navigation.
10. Disjoint loci use separate layout islands so global BAM-scale packing is unnecessary.

## Completion definition

This work is complete only when:

- the original large RNA-seq zoom reproducer reports zero moved shared reads;
- stable mode is the persisted default, while compact mode predictably minimises each settled query;
- switching stable → compact → stable restores the prior stable context;
- **Compact/repack now** establishes a new stable baseline without enabling persistent compact mode;
- broad → narrow → broad restores the original row map;
- ordinary zoom, pan, query, cache, resize, filter, and LOD changes cannot reset active row history;
- stable rows and vertical camera anchoring survive asynchronous query handoff;
- explicit policy changes are the only ordinary source of reflow and expose a reset reason;
- collision, determinism, identity, memory, and performance tests pass at dense-locus limits; and
- native visual qualification confirms that users remain oriented while navigating the data.
