# MF-01 viewer and MCP follow-up remediation plan

## Status and scope

This plan follows the first broad agent-driven MF-01 exploratory run after completion of
`RGV_MCP_GAP_REMEDIATION_PLAN.md`. The MCP can operate the live application and complete the main
workflow, but the run exposed correctness, presentation, and fixture-discoverability problems.

The work covers:

- alignment discovery and selection semantics in the desktop automation executor;
- bounded MCP evidence returned for large records;
- read-inspector layout and long-CIGAR presentation;
- review-item loci and agent navigation ergonomics;
- MF-01 qualification assertions and screenshot review.

It does not replace visual testing with structured metadata or broaden MCP access to read names,
sequences, arbitrary tags, or source paths.

## Observed baseline

The exploratory run established the following facts:

| Priority | Finding | Classification |
| --- | --- | --- |
| P0 | A read at `chr2:10001-20100` was selected while the viewport was `chr2:900001-1100000`, but the selection response reported `visible_in_active_view: true`. | MCP correctness |
| P0 | At `chr2:1-30000`, the rendered modification lane and export reported 19 tagged records and 38 decoded calls, while `has_modified_bases=true`, hit summaries, and the selected read reported no modification evidence. | MCP correctness |
| P1 | Supplementary-chain discovery depends on the active query extent, so the same biological record can gain or lose chain availability as the viewport changes. | MCP correctness |
| P1 | The read inspector covers most of the evidence area. A 70,000-operation CIGAR produces an oversized, poorly bounded presentation. | Viewer UX |
| P1 | The full pathological CIGAR can dominate an MCP selection response. | Agent ergonomics |
| P1 | The long-read/modification and pathological-CIGAR review loci initially show little useful read evidence because they open too broadly. | Fixture UX |
| P2 | Exact-name selection can select an off-screen record but offers no atomic way to reveal it. | MCP ergonomics |
| P2 | Concurrent dependent MCP calls can be observed out of order unless the host awaits each response, despite documentation saying calls are serialized. | MCP contract |
| P2 | Sparse evidence has low contrast and the primary track label is truncated without a convenient full-name affordance. | Viewer UX |

The app did not crash. Pan, zoom, vertical scrolling, grouping, sorting, display settings,
selection retention, export, save, and reopen all worked during the run.

## Required invariants

These are release gates for the remediation:

1. Selection visibility is calculated against the selected record's contig and half-open interval,
   never defaulted to `true`, and is correct both in the mutation response and later observations.
2. Alignment discovery and inspector evidence use the same loaded auxiliary information as the
   modification renderer/export, subject to the same explicit malformed-tag policy.
3. Supplementary-chain availability is a property of the record and its indexed relationships,
   not an accidental property of the current viewport width.
4. `rgv_find_alignments` has a documented search scope. Every returned hit is either inside that
   scope or explicitly marked as a related off-scope result.
5. Ordinary and pathological records cannot make the inspector obscure the majority of the
   genomic evidence at its default size.
6. Human-visible and MCP CIGAR representations are bounded by default while preserving access to
   an exact value through an explicit copy/export operation.
7. Every MF-01 review item opens at a scale where its named evidence is visible or where the UI and
   MCP state clearly explain the next focusing action.
8. Dependent mutations have one supported ordering mechanism with tested semantics.

## Workstream 1: selection visibility and search scope

### Implementation

- Add the selected contig to the retained automation selection snapshot. Do not infer it from the
  current viewport.
- Replace all construction-time `visible_in_active_view: true` values with the common visibility
  calculation used by `observed_retained_selection`.
- Define visibility precisely as full containment of the selected evidence interval in the active
  panel. If partial overlap is useful, expose it separately as `overlaps_active_view`; do not
  overload the boolean.
- Recompute only the visibility fields after navigation. Keep the retained evidence interval and
  evidence revision immutable.
- Define `rgv_find_alignments` as searching the current settled track query by default. Add the
  query interval and scope to its structured result so callers can tell what was searched.
- Do not return a viewport-external record as an ordinary hit. Related records discovered while
  resolving a mate or chain must be returned by the relationship tools and marked off-screen.
- Add an optional selection reveal policy to `rgv_select_alignment`:
  `none` (backward-compatible default), `center`, or `fit`. Return the exact resulting viewport and
  whether navigation occurred. This makes exact-name lookup usable without an unsafe second guess
  at the interval.

### Likely code areas

- `crates/rgv-core/src/automation.rs`
- `apps/rgv-desktop/src/automation/commands.rs`
- `apps/rgv-mcp/src/server.rs`
- `apps/rgv-desktop/src/tests/automation.rs`

### Tests

- Same-contig fully visible, partially visible, and fully off-screen selections.
- Cross-contig selection after navigation.
- Immediate `rgv_select_alignment` response versus subsequent `rgv_get_selection` equality.
- Exact-name selection with each reveal policy.
- Search-result intervals constrained to the reported search scope.

## Workstream 2: modified-base and supplementary-chain correctness

### Implementation

- Trace the alignment materialization used by automation discovery. Ensure it requests or retains
  the MM/ML auxiliary data needed by `decode_modified_bases`; an empty auxiliary payload must not
  be interpreted as biological absence when the tags were simply not loaded.
- Reuse a single modification-evidence summary for rendering, inspector fields, exports, semantic
  matching, and hit summaries. The summary should distinguish:
  `present`, `absent`, `malformed`, `unavailable/not loaded`, and `truncated`.
- Change `has_modified_bases` matching so `unavailable` is not silently treated as `false`.
  Prefer loading the bounded required fields; otherwise return a typed incomplete-query error.
- Make malformed MM/ML cases observable through bounded status/count fields without exposing raw
  tags. MF-01 contains deliberate missing, malformed, wrong-type, and probability-length cases.
- Resolve supplementary chains from a bounded record-relationship lookup independent of the
  current viewport query. Keep opaque handles scoped to the application instance and query
  generation, but do not derive chain existence solely from records co-loaded in the viewport.
- Use the same relationship resolver for semantic search, hit counts, selection, the visible chain
  inspector, and `rgv_get_related_alignments`.

### Tests

- At `chr2:1-30000`, semantic modification search finds the tagged controls and agrees with the
  modification export's tagged-record and decoded-call totals.
- Selected-record modification counts agree with the renderer/export summary for that record.
- Missing tags remain `absent`; malformed and unavailable inputs do not become false negatives.
- A supplementary control reports the same chain count at 30 kb, 200 kb, and 2 Mb view widths.
- Navigating to a supplementary segment and back preserves relationship identity and history.

## Workstream 3: bounded inspector and CIGAR presentation

### Implementation

- Give the read inspector a bounded default width and maximum height relative to the viewport.
  Put its body in a scroll area so it cannot cover most of the application.
- Prefer a docked side panel or a compact movable window that leaves the selected evidence visible.
  Preserve the user's resized dimensions for the session, but clamp them after window resize.
- Render ordinary fields in a two-column layout with selectable/wrapping values.
- Replace the inline full CIGAR with a summary containing at least operation count, query span,
  reference span, and a short prefix/suffix preview. Put the exact text behind `Copy CIGAR` and an
  explicitly expanded scrollable section.
- Collapse auxiliary tags and other large sections by default. Show clear counts and malformed or
  truncated status in the collapsed header.
- Bound the MCP selection field for CIGAR separately from the GUI copy value. Return a typed
  summary and truncation metadata; provide exact CIGAR only through a bounded explicit evidence
  export when requested.
- Keep selection IDs and evidence digests independent of presentation truncation.

### Likely code areas

- `apps/rgv-desktop/src/ui/inspectors.rs`
- `apps/rgv-desktop/src/inspection/inspector_controller.rs`
- `apps/rgv-desktop/src/automation/commands.rs`
- selected-evidence export code in the desktop automation/export modules

### Tests

- UI test or deterministic render fixture for an ordinary read and the 70,000-operation CIGAR.
- Default inspector occupies no more than an agreed fraction of the content area at supported
  minimum and common window sizes.
- Long CIGAR text neither expands the window nor clips controls.
- Exact CIGAR copy/export remains byte-correct.
- MCP response size remains small and stable for the pathological record.

## Workstream 4: review-item and visual discoverability

### Implementation

- Refine generated MF-01 review loci in
  `validation/runner/src/bin/rgv-manual-fixtures/projects.rs`:
  - open long-read/modification evidence around `chr2:1-30000`;
  - use relationship navigation to reach the distant supplementary segment;
  - tighten the pathological-CIGAR interval to a scale that visibly renders the record and permits
    selection without prior fixture-file knowledge.
- Regenerate the checked-in project rather than hand-editing only the JSON artifact.
- Extend review-item observation with a bounded evidence hint when appropriate, such as
  `recommended_detail` or `focus_interval`; do not expose private notes or read identifiers.
- When evidence is compressed at Overview/Regional detail, show a visible lane message or stronger
  glyph rather than an apparently empty Reads lane.
- Improve sparse-read and modification-mark contrast against the current light background.
- Show the complete track name on hover and in workspace/inspector context while retaining bounded
  sidebar width.

### Tests

- Freshly generated MF-01 projects contain the intended loci and pass fixture validation.
- Each review item produces visible, interpretable evidence in its first settled screenshot.
- The long-read item visibly includes reads and modification marks.
- The CIGAR item visibly includes the pathological record.
- Existing dense-coverage screenshots do not regress through the contrast changes.

## Workstream 5: ordering contract for agent operations

### Implementation

- Decide and document one contract:
  - guarantee FIFO admission for MCP tool calls received on the stdio connection; or
  - state that concurrent calls are unordered and require callers to await dependent operations.
- Regardless of the choice, keep `rgv_execute_batch` as the atomic ordered mechanism for dependent
  multi-step workflows.
- Add optional `expected_workspace_revision` preconditions to mutations that consume state-derived
  IDs or handles. Reject mismatches with a typed `workspace_revision_conflict` rather than applying
  an operation to unexpected state.
- Update `docs/MCP.md`; remove the unconditional serialization claim unless the implementation and
  protocol-level test prove it.

### Tests

- Send navigation and observation concurrently many times and assert the documented outcome.
- Verify batch command ordering.
- Verify stale revision preconditions fail without mutating the workspace.

## Workstream 6: qualification and regression evidence

Extend `validation/rgv_mcp_qualification.py` so MF-01 fails automatically on the structured parts
of the defects while retaining human screenshot inspection for appearance.

Add assertions for:

1. selection visibility before and after same-contig and cross-contig navigation;
2. search-scope metadata and absence of unmarked off-scope hits;
3. modification semantic hits agreeing with export provenance;
4. supplementary-chain counts remaining stable across viewport widths;
5. reveal-selection navigation and history behavior;
6. bounded pathological-CIGAR response fields;
7. save/open persistence after the new settings and navigation operations;
8. privacy checks for exact names, raw tags, paths, and secrets.

Capture and inspect at least these settled states:

- each review item's initial view;
- deep coverage before and after vertical scroll;
- long-read/modification view before and after selection;
- supplementary chain before and after related navigation;
- pathological CIGAR before and after selection;
- reopened workspace.

The screenshot command must continue to focus the exact PID and capture in the same AppleScript
invocation. The qualification report must record application instance, workspace revision,
selection/evidence revision, viewport, and screenshot filename for every checkpoint.

## Delivery order

1. **Correctness first:** workstreams 1 and 2, with focused unit/integration tests.
2. **Presentation:** workstream 3, verified at minimum and common window sizes.
3. **Discoverability:** workstream 4 and regenerated MF-01 artifacts.
4. **Contract hardening:** workstream 5 and MCP documentation.
5. **End-to-end qualification:** workstream 6, followed by two clean native runs.

Do not change the MF-01 expected truth merely to accommodate current output. Fixture-locus changes
may improve discoverability, but biological counts and named control semantics remain authoritative.

## Completion gates

- Focused automation, inspector, modification, supplementary, MCP schema, and protocol tests pass.
- `cargo fmt --all -- --check` passes.
- `cargo check --all-targets` passes.
- The workspace test suite passes with no new ignored tests.
- Release `rgv-desktop` and `rgv-mcp` binaries build from a clean checkout.
- Two independent MF-01 native runs complete every manual action.
- All retained screenshots are inspected at original resolution.
- No selection visibility, modification count, chain count, or search-scope contradictions remain.
- The ordinary and pathological inspectors leave enough genomic evidence visible to judge the
  selected record.
- MCP reports and artifacts pass the existing privacy and control-root confinement audit.

## Definition of done

MF-01 is complete when an agent can discover each review target, bring its evidence into a useful
view, manipulate and inspect it, navigate mates and supplementary segments, verify modifications,
handle the pathological CIGAR, traverse history, export, save, and reopen without fixture parsing,
coordinate guessing, contradictory structured state, or an inspector that prevents visual review.
