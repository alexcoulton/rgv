# Complete read-presentation plan

Status: planned

Plan date: 2026-08-28

Depends on:

- [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md)
- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)

## Decision

RGV must never present a sampled set of individual reads as though it were the read population.

The existing 50,000-read boundary will become a user-visible **Complete read limit**. For the exact
viewport and active filters:

- if the eligible read count is at or below the effective limit, RGV presents every eligible read;
- if it is above the limit, RGV presents no individual reads and asks the user to zoom in or raise
  the limit; and
- complete aggregate evidence, especially coverage, remains available when it was computed from
  every eligible read.

The default remains 50,000 and the existing supported range remains 1,000–250,000. This change does
not attempt to prove that 50,000 is optimal. Users can vary the limit and measure RAM and
responsiveness later; that profiling is not an implementation or release prerequisite.

This decision supersedes the viewport-priority and deterministic sampling policies described in
earlier plans. Deterministic ordering remains part of stable layout, but never selects which reads
the user is allowed to see.

## Why this is the simpler policy

The old 50,000 cap was a round safety default, not the result of a representative memory sweep. A
sweep across short reads, spliced RNA, long reads, auxiliary tags, modifications, multiple tracks,
and different GPUs could improve a future default, but it would not answer the more important
truthfulness question: a sampled read stack is incomplete regardless of where the cap is placed.

This plan therefore separates two decisions:

1. Implement the correctness rule now: **complete or refuse**.
2. Tune the default later using real datasets and hardware, without changing that rule.

## Scope

This implementation includes:

- local and remote BAM/CRAM read queries;
- exact foreground queries, background prefetch, and canonical layout horizons;
- the existing read-limit setting, session compatibility, and clear setting language;
- native read-pane Complete, Checking, Too-dense, Failed, and Unavailable states;
- consistent rendering, picking, inspection, selection, export, automation, and MCP behaviour; and
- boundary and regression tests proving that no sampled batch reaches a product surface.

This implementation deliberately does **not** include:

- a mandatory RAM/CPU/GPU sweep to choose a new default;
- automatic hardware-dependent or adaptive limit selection;
- a new general-purpose memory admission framework;
- automatic limit increases at dense loci;
- a redesign of workspace resource arbitration; or
- the now-approved [transactional navigation-preview follow-up](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md).

Existing hard validation needed to avoid invalid or pathological allocations remains in force. A
hard failure must be reported as an error or unavailable view; it must never fall back to sampling.

## Product contract

### Complete

A read presentation is Complete only when every alignment that intersects the exact foreground
interval and passes the active source, flag, MAPQ, tag, and user filters is in the committed batch.

For a Complete view:

- `complete_read_records == eligible_read_records`;
- the same committed query revision backs layout, rendering, picking, inspection, selection,
  export, automation, and MCP;
- stable row assignment may use only complete foreground and horizon data; and
- the UI does not use ambiguous language such as “loaded” when records may be missing.

Explicit semantic filters are allowed because they define the requested population. Resource-based
sampling is not.

### Too dense

If the eligible foreground population exceeds the effective Complete read limit:

- the individual-read batch is empty;
- no read geometry, picking targets, selection results, or individual-read exports are exposed;
- the state reports the exact eligible count and effective limit once the source scan completes;
- the read pane asks the user to zoom in or raise the limit; and
- complete aggregate evidence may still be shown.

A candidate inline message is:

> 104,197 eligible reads overlap this interval; the Complete read limit is 50,000. Zoom in or raise
> the limit to inspect individual reads.

This is an inline state, not a modal warning during ordinary zoom.

### Checking and navigation

Use a neutral labelled loading state while the new interval is being checked. Do not rescale the
previous interval's semantic read geometry into the new viewport and do not make stale reads
interactive.

The follow-up [transactional navigation-preview plan](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)
retains a non-interactive raster of the previous committed genomic canvas to preserve orientation.
It remains visually and semantically separate from current evidence, so it does not weaken this
plan's first correct complete-or-refuse implementation.

### Complete aggregates

Coverage, junction, pair, and modification summaries may remain visible after individual reads are
refused only when they were accumulated from every eligible source record for the declared
interval. Their provenance must be independent of individual-read retention.

If an aggregate scan fails or is cancelled, RGV reports that aggregate as unavailable. It does not
present a partial aggregate as complete.

## Complete read limit

### Setting behaviour

Rename **Read safety cap** to **Complete read limit** and explain its semantics in the properties
panel:

- at or below the limit, all eligible foreground reads are shown;
- above the limit, no individual reads are shown;
- raising it can increase memory use, query/layout time, primitive counts, and GPU work; and
- 50,000 is a conservative default, not a universal recommendation.

Preserve the existing `maximum_alignments` session field as a compatibility alias. Its meaning
changes from “maximum retained sample” to “maximum records in an admitted complete read batch.”

The setting is stored on the alignment-track configuration and remains constrained by the existing
document/workspace maximum. If workspace policy lowers the effective query limit, diagnostics and
the Too-dense message must report the effective value actually used rather than only the requested
track value.

Changing the setting invalidates the relevant query result and retries admission. It must not
reveal a previously retained sample.

### Later user profiling

No benchmark sweep gates this implementation. Later, the interaction benchmark may accept the
same limit and report process RSS, query/cache estimates, layout time, primitive count, and
navigation responsiveness. Users can then try representative values and datasets.

If that evidence supports changing the default or range, do so in a separate decision with the
dataset, hardware, command, and raw results recorded. The complete-or-refuse contract remains the
same at every configured value.

## Technical design

### 1. Represent the state explicitly

Replace “batch plus `truncated`” as the product contract with an explicit individual-read state:

```rust
enum IndividualReadState {
    Complete {
        eligible_records: usize,
        batch: AlignmentBatch,
    },
    TooDense {
        eligible_records: usize,
        effective_limit: usize,
    },
}
```

Exact names may differ. The required invariant is that a settled result cannot contain both a
partial batch and a Too-dense/truncated state.

Aggregate results and their completeness provenance remain separate from the individual-read
state.

### 2. Stream as complete or refuse

Local and remote source workers use the same state machine:

1. Scan the exact requested interval and apply semantic filters.
2. Accumulate complete aggregate evidence and count every eligible record.
3. Provisionally retain individual reads while the count is at or below the effective limit.
4. On record `limit + 1`, release the provisional individual batch.
5. Continue the source scan far enough to finish the exact count and complete aggregates.
6. Return either the full batch or a typed Too-dense result.

After refusal, the worker should skip normalisation that is required only for individual read
display, while still doing the work required for promised aggregates. The provisional batch never
escapes the worker.

Remove record-ranking, reservoir, viewport-priority, and other sampling paths from both local and
remote queries.

### 3. Separate foreground from prefetch

The foreground query covers the exact viewport, not a viewport-plus-margin interval. Background
prefetch uses separate deterministic adjacent intervals and cannot consume the foreground read
limit or change its result.

Prefetch entries are cached only as:

- a complete batch;
- complete aggregates plus Too-dense individual reads; or
- a typed failure/cancellation with appropriate reuse rules.

No cache entry contains an undisclosed sample, and eviction removes whole entries rather than
trimming batches.

### 4. Use complete identity-bearing context without making the preferred horizon a gate

> Amended by the dense-alignment performance work. The exact Complete foreground is the
> presentation authority. A preferred wider horizon is optional layout context and cannot suppress
> that foreground merely because the wider interval exceeds its independent admission budget.

Stable layout must not use sampled identities. For a Complete foreground view:

- try the preferred deterministic canonical horizon;
- if it is Too dense, try the existing smaller fixed deterministic horizon level that still covers
  the viewport;
- commit identities only from a Complete horizon or Complete exact foreground;
- if no complete wider horizon covers the viewport, seed or extend the best-known stable
  architecture from the Complete exact foreground and report `horizon_truncated = true`; and
- never relabel or refuse a Complete exact foreground because optional layout context was Too
  dense.

Compact mode also needs only a complete foreground batch. A rejected stable-horizon extension
leaves incumbents unchanged while the Complete exact foreground may add candidate assignments
transactionally.

### 5. Commit one truth state across the desktop

Complete and Too-dense results pass the normal generation/revision checks and commit atomically.

For Too dense:

- render only the inline refusal state in the read pane;
- render complete aggregates in their own lane when available;
- install no read hit regions;
- clear or disable current read selection/inspection for the new interval; and
- refuse individual-read export and automation/MCP selection with a typed explanation.

Automation and diagnostics expose at least:

- `read_presentation_state`: `checking`, `complete`, `too_dense`, `failed`, or `unavailable`;
- foreground interval;
- exact eligible count when known;
- complete records retained (`eligible` for Complete, zero for Too dense);
- requested and effective Complete read limits;
- refusal scope (`foreground` or `layout_horizon`); and
- aggregate completeness state.

Legacy `truncated` may remain temporarily for wire compatibility, but it cannot mean “a sample is
being shown.”

## Implementation phases

### Phase 1: Setting and typed backend contract

- Rename the setting and update help/session documentation without changing the default or range.
- Add explicit Complete and Too-dense states and effective-limit/count fields.
- Implement the complete-or-refuse accumulator for local and remote BAM/CRAM.
- Remove all individual-read sampling paths.
- Keep aggregate accumulation independent of read retention.
- Add zero, exact-limit, and limit-plus-one boundary tests.

Exit criterion: the backend can return all individual reads or none, never a sample.

### Phase 2: Exact foreground, prefetch, and stable horizons

- Query the exact viewport for foreground evidence.
- Move prefetch into independent background requests/cache entries.
- Admit only complete canonical horizons and deterministic complete fallbacks.
- Refuse Stable mode when no complete horizon covers the viewport.
- Verify that a rejected horizon does not mutate stable row history.

Exit criterion: prefetch and horizon density cannot silently remove foreground reads or seed rows
from incomplete data.

### Phase 3: Desktop and machine-visible states

- Add neutral Checking and inline Too-dense read-pane states.
- Preserve complete aggregate display independently.
- Remove read rendering and interaction targets for refused views.
- Gate inspection, selection, individual-read export, automation, and MCP consistently.
- Report requested/effective limits and exact counts in diagnostics.

Exit criterion: every surface agrees on Complete versus Too dense, and no partial reads are
interactive or exportable.

### Phase 4: Qualification and documentation

- Update the user guide, architecture plans, and session compatibility notes.
- Run local/remote BAM and CRAM boundary/parity tests.
- Run workspace, engine, layout, desktop, automation, and export tests.
- Run formatting, lint/check, release build, and large-RNA qualification.
- Capture native Complete, Checking, and Too-dense screenshots.
- Verify manually that raising the limit retries the query and changes admission, not sample size.

Exit criterion: the complete-or-refuse policy is documented, tested, and visually unambiguous.

## Test matrix

### Query correctness

- Zero, one, limit-minus-one, exact-limit, and limit-plus-one eligible records.
- Filters reducing an over-limit interval to a Complete result.
- Reads beginning outside but overlapping the exact viewport.
- Secondary, supplementary, duplicate, QC-failed, MAPQ, and tag-filter policies.
- Local BAM/CRAM and remote BAM/CRAM parity.
- Cancellation before and after the limit is crossed.
- Complete coverage/counts with an empty Too-dense individual batch.
- No retained provisional individual records after refusal.

### Architecture and cache

- Admissible foreground with Too-dense prefetch, and the inverse.
- Whole Complete and Too-dense cache entries; no partial cache hit.
- Complete preferred horizon and deterministic smaller-horizon fallback.
- No complete horizon covering an otherwise Complete viewport.
- Rejected extension preserving all incumbent rows.
- Stable shared-read rows across admitted zoom levels.

### Desktop and integration

- Neutral, non-interactive Checking state after navigation starts.
- Complete result replacing Checking atomically.
- Too-dense result containing no read primitives or picking targets.
- Complete aggregates remaining available and correctly labelled.
- Zooming in producing Complete only after a real successful query.
- Raising the limit retrying admission.
- Individual-read selection/export/automation/MCP refusing Too dense.
- Multiple tracks showing independent Complete and Too-dense states without leakage.

### Resource smoke tests

- Default 50,000: exact-limit Complete and limit-plus-one Too dense.
- One higher user-selected limit proving the setting controls admission rather than sampling.
- Cold query and cached-navigation regression checks.
- Rapid zoom cancellation/supersession.

Broader RAM profiling at several limits is optional later work, not part of this test gate.

## Completion definition

This work is complete when:

- no local or remote path can return sampled individual reads for presentation;
- every settled read pane is Complete or explicitly unavailable/Too dense;
- Complete means every eligible foreground read is consistently represented across layout,
  rendering, interaction, export, automation, and MCP;
- aggregate evidence is labelled complete only when it used every eligible record;
- foreground admission is independent of prefetch;
- every identity-bearing row seed is Complete, while preferred wider horizons may be explicitly
  truncated without suppressing a Complete foreground;
- Too-dense views retain no partial read batch, geometry, or interaction target;
- Checking never reprojects stale reads as current evidence;
- the 50,000 default is exposed as a configurable Complete read limit and is not described as a
  universal optimum;
- setting/session compatibility is documented; and
- automated qualification, release build, large-RNA check, and native visual review pass.
