# Unified alignment-scene recovery and desktop cutover plan

Status: recovery complete. R0–R5 pass their automated gates and the user accepted the R6 native
visual checkpoint on 31 August 2026. Written after the preceding native build became permanently
stuck on `Updating view...` under rapid pan, zoom, and contig navigation.

This was the execution authority for recovering Phases 6–8 of
`UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md`. It does not replace the product contract or the
headless scene architecture. It paused further alignment feature work while the desktop control
plane was made bounded, live, and exclusively scene-backed; its accepted result now unblocks the
next master-plan frontier.

## Outcome

RGV must remain responsive under an arbitrarily untidy burst of valid navigation input. Older
work may be cancelled or reused, but the exact latest target must always end in one of three
truthful outcomes:

1. a coherent committed presentation;
2. an explicit terminal source/capability result; or
3. active, identifiable work that still owns the target.

The application must never remain indefinitely unsettled when no worker, retry, or terminal result
owns the latest target.

This recovery preserves the following requirements:

- Complete alignment evidence is not sampled, capped, or silently discarded to improve speed.
- CPU and GPU presentation consume the same immutable semantic scene.
- Only the currently necessary GPU pages need be resident; complete evidence does not mean that
  every read's geometry must occupy the GPU simultaneously.
- Prepared pan and vertical movement show resident surrounding reads continuously rather than a
  texture substitute or a white panel.
- Superseded work is cancellable and the newest intent wins.
- Hover, selection, coverage, substitutions, row metrics, and scrollbar state refer to one
  coherent committed alignment epoch.
- Visual acceptance is performed by the user. Automated UI driving is not a substitute.

## Architectural decision

Preserve the headless alignment-scene foundation:

- immutable evidence roots and coverage certificates;
- stable record handles;
- exact summary pages;
- the immutable row atlas;
- the latest-wins source actor and bounded mailboxes;
- persistent CPU/GPU semantic pages;
- the enforcing allocation ledger; and
- separate desired and committed presentation targets.

Rebuild the desktop integration boundary around those components. The current integration is a
hybrid: the scene owns native read pixels, while legacy query, row-layout, display-rebuild, and
navigation-preview state still participates in deciding whether a target can commit. Recovery is
not complete until the unified local-BAM path has one semantic authority and one navigation
liveness model.

Do not add another fallback renderer, timeout-based forced commit, retry loop, hidden read limit,
or larger hard-coded cache allowance. Such changes can hide a stranded state without making the
system live or correct.

## What the current failure proves

The stuck native process was sampled while the `Updating view...` state was visible:

- physical footprint was approximately 9.0 GiB, with a 9.3 GiB peak;
- the process remained CPU-active rather than waiting on disk;
- the main UI thread repeatedly entered `Viewer::poll_background_work`;
- most sampled active UI work was inside `AllocationLedger::snapshot`;
- each full snapshot clones and sorts every allocation record; and
- the local-BAM scene-source actor was asleep for almost the entire sample.

Two desktop retry functions currently take a full ledger inventory on every background poll, even
when no matching retry exists. This turns control-plane work into work proportional to the entire
retained data set. The operation is at least linear in the allocation count and includes a sort.
Allowing a larger cache can therefore make ordinary UI administration slower.

Separately, navigation readiness is an all-or-nothing conjunction of alignment query state,
reference state, generic track state, and desktop row-layout state. The desktop row-layout gate
still reads legacy `current_query` and `layout_query` values, and display reconstruction returns
early while that work is pending. Rapid input advances and supersedes several generations across
these authorities. The present model does not prove that the newest unsettled generation is still
owned by queued/running work or a typed pressure retry.

The sample therefore establishes a control-plane scalability defect and strong evidence of a
navigation-liveness defect. It does not establish that loading complete read evidence is
infeasible. Additional runtime telemetry is required to identify the exact stranded dependency in
each reproduction.

The 4 GiB alignment-cache setting is not a process-wide memory limit. The difference between that
setting and the sampled 9 GiB footprint must be reconciled before qualification. It may include
GPU allocations, transition overlap, duplicate legacy/new representations, allocator retention,
or other uncharged storage; the sample alone does not distinguish them.

## Required invariants

### UI and control-plane cost

1. An idle UI poll performs no work proportional to retained records, pages, or allocations.
2. Navigation input performs work proportional to the input and visible/prepared products, not
   the complete allocation inventory.
3. Full allocation inventories are produced only for explicit diagnostics or eviction planning,
   never as a liveness probe.
4. Retry checks run only while a typed retry exists and wake only after relevant progress.
5. No source decode, row-atlas construction, full row projection, or allocation sort runs on the
   UI thread.

### Navigation ownership

1. Every desired target has a unique token and generation.
2. The latest nonterminal target has exactly one current owner: queued work, running work, a typed
   pressure wait, or a privately staged candidate.
3. Every owner reports a terminal transition: committed, explicitly unavailable, cancelled as
   superseded, or failed with a typed reason.
4. Superseded generations cannot participate in readiness for the latest generation.
5. A lost completion, disconnected mailbox, or ownerless target is detected as an invariant
   failure with a useful diagnostic; it is not represented indefinitely as `Updating view...`.
6. Repeated input coalesces to bounded state while retaining the exact latest target.

### Semantic authority

1. The unified local-BAM path obtains evidence, rows, geometry, picking, selection, inspection,
   coverage, and substitutions from the scene epoch.
2. Legacy `current_query`, `layout_query`, `alignment_rows`, retained alignment textures, and
   query-local indices do not gate or supply the unified path.
3. The row atlas is the sole alignment-row authority. Vertical paging changes residency, not row
   assignment or semantic extent.
4. A committed alignment presentation is immutable. Candidate work cannot partially mutate it.
5. Read and derived-lane products that require atomic coherence publish together or retain the
   preceding coherent epoch.
6. Reference-dependent products use explicit `Pending`, `Ready`, or `Unavailable` states. A
   pending substitution interpretation need not prevent truthful read geometry or exact coverage
   from settling.
7. Generic contextual lanes do not hold the alignment presentation in a global unsettled state
   unless the product contract explicitly requires their atomic coherence.

### Presentation

1. Camera movement inside a committed scene's validity envelope changes camera state without
   rebuilding evidence, rows, page tables, or unchanged geometry.
2. Resident surrounding pages remain visible during prepared pan and vertical movement.
3. Outside the validity envelope, the prior committed epoch remains unchanged while a candidate
   prepares. It may be reprojected only where its coverage certificate makes that truthful.
4. A disjoint contig jump never presents old-contig pixels as the new target. A neutral loading
   surface may be necessary, but the application and navigation controls remain responsive and
   the latest target must settle.
5. No temporary pressure, cancellation, or missing optional page clears an otherwise truthful
   committed lane.
6. Status identifies the actual pending or terminal dependency; `Updating view...` is not an
   unbounded catch-all state.

### Resources

1. The cache budget has a documented scope and every allocation in that scope is charged once.
2. Current, candidate, transition, transient, GPU, and legacy compatibility allocations are
   separately observable.
3. Repeated navigation reaches a stable high-water band for an unchanged workload and policy.
4. Increasing the cache budget increases retained useful data without increasing the asymptotic
   cost of UI polling.
5. Complete evidence may be paged between storage, CPU memory, and GPU memory without changing
   completeness or stable identities.

## Target state model

The desktop should have one small, auditable record per panel/alignment owner:

```text
latest desired target
        |
        v
 queued -> running -> waiting for typed pressure -> candidate ready
   |          |                  |                       |
   +----------+------------------+-----------------------+
                              atomic commit
                                   |
                                   v
                       committed presentation epoch
```

Every arrow is token-checked. A newer desired target moves the older record to `Superseded` and
either retargets reusable work or cancels it at the next bounded checkpoint. `Superseded`,
`Committed`, `Unavailable`, and `Failed` are terminal for that generation.

There is no second desktop alignment-query state machine beside this record. Contextual tracks may
have independent state machines, but their state cannot silently keep an otherwise complete
alignment target unsettled.

## Recovery workstreams

### R0 — Freeze, preserve, and make the failure observable

1. Preserve the current uncommitted implementation and this native failure on an explicitly named
   recovery checkpoint before destructive deletion.
2. Pause derived-lane expansion, remote/CRAM work, and visual polish.
3. Add a structured navigation journal containing:
   - panel, track, token, and generation;
   - desired and committed cameras;
   - source, row-atlas, semantic-page, GPU, reference, and contextual-lane states;
   - the current owner and its queue/running/pressure/candidate state;
   - the exact readiness predicate preventing commit; and
   - every supersede, cancellation, terminal result, and commit transition.
4. Add a non-UI diagnostic snapshot that is constant-size per panel/track and does not enumerate
   allocations or reads.
5. Add an invariant report for `unsettled && no current owner && no terminal result`.
6. Capture the current ACTB/BAP1 and rapid-contig failure as deterministic input traces at the
   command/state-machine boundary. This is not automated visual assessment.

Gate:

- a reproduction identifies the exact stranded dependency rather than only displaying
  `Updating view...`;
- diagnostics do not materially change scheduling or memory behaviour;
- every current target's owner can be named; and
- no behavioural workaround has been introduced.

### R1 — Make the resource control plane constant-cost and event-driven

1. Split the ledger API into:
   - an O(1) `LedgerProgress`/capacity summary maintained by ledger mutations; and
   - an explicit full `LedgerInventory` for diagnostics and eviction planning.
2. Maintain required aggregates, including pinned bytes and progress revision, incrementally.
3. Return immediately from source, budget, and GPU retry paths when their retry registries are
   empty.
4. Replace event-loop retry polling with progress notifications carrying the relevant ledger
   revision and blocker class.
5. Wake at most the owners whose blockers may have changed. Unrelated allocation churn must not
   reopen a retry.
6. Move unavoidable inventory/eviction planning off the UI thread and publish only its bounded
   result.
7. Add operation counters that prove an idle or camera-only poll enumerates zero allocation
   records.
8. Reconcile process footprint against charged CPU, GPU, transient, transition, compatibility, and
   allocator-retained memory.

Gate:

- retry-free UI polling takes no ledger lock that enumerates records;
- camera-only navigation performs zero full ledger inventories;
- a populated ledger does not alter the number of control-plane operations per UI poll;
- pressure retries wake only after relevant progress and settle or remain truthfully owned; and
- memory reporting explains the difference between the configured budget and process footprint.

### R2 — Replace navigation readiness with an owned liveness transaction

1. Introduce one `AlignmentNavigationTransaction` per panel/track using the target state model
   above.
2. Make the transaction the only authority for whether alignment navigation is pending, ready,
   committed, superseded, or terminal.
3. Require source, scene, renderer, and desktop messages to carry the same target token and
   generation.
4. Make source and GPU pressure explicit owned states rather than errors or untracked deferred
   flags.
5. Ensure mailbox replacement returns or records the displaced token's `Superseded` transition.
6. Remove readiness inference from scattered booleans such as `pending_query` plus independent
   generation comparisons.
7. Decouple optional/reference/contextual product readiness according to the semantic dependency
   graph rather than a global conjunction.
8. Keep a bounded recent terminal history for diagnostics, not as live semantic authority.

Gate:

- every generated target reaches a terminal state or remains attached to identifiable active
  work;
- no superseded target can delay the latest target;
- queue replacement, cancellation, pressure, errors, and worker disconnection have tested
  terminal transitions;
- an idle worker with an unsettled current target fails a deterministic invariant test; and
- the exact final target after an input burst equals a clean load of that target.

### R3 — Remove dual alignment authority from the production path

1. Route the unified local-BAM path directly from `PresentedAlignmentEpoch` and scene handles.
2. Remove `ensure_alignment_rows()` from navigation readiness and unified display rebuilding.
3. Remove unified-path reads of legacy `current_query`, `layout_query`, `alignment_rows`,
   `cached_layout`, and query-local selection indices.
4. Route row metrics, density preview, scrollbar extent, picking, selection, and inspection through
   the committed scene/row-atlas epoch.
5. Remove alignment pixels from generic display-list and retained-navigation-texture paths.
6. Keep legacy structures only behind an explicit unsupported-source boundary while any
   non-scene backend remains. They cannot be a fallback for a supported local BAM.
7. Add source-level checks/tests that fail if forbidden legacy accessors become reachable from the
   unified production path.

Gate:

- the local-BAM unified path has exactly one evidence authority, one row authority, and one
  presentation authority;
- alignment rendering and interaction continue when generic annotation work is independently
  pending;
- vertical movement never invokes row assignment or query-local layout;
- supported local BAM cannot enter legacy alignment rendering, picking, or preview code; and
- deleting the now-unreferenced legacy alignment integration changes no unified-path behaviour.

### R4 — Re-establish coherent progressive presentation

1. Preserve a committed epoch while constructing a candidate privately.
2. Use camera-only capture inside the exact validity envelope without opening a replacement
   bundle or uploading unchanged geometry.
3. Outside the envelope, prepare evidence, semantic pages, row blocks, and GPU pages under one
   token-owned candidate.
4. Atomically commit all coherence-required alignment products. Represent optional product
   readiness explicitly within the epoch.
5. Never clear committed truthful pages because adjacent prefetch, deep vertical pages, or optional
   derived pages are missing.
6. Ensure that selection and hover overlays are camera/epoch updates, not content replacements.
7. Make status and progress originate from the navigation transaction and product states.

Gate:

- prepared pan and vertical scrolling preserve resident pixels continuously;
- releasing a gesture produces no white replacement flash;
- a candidate failure leaves the preceding coherent epoch unchanged;
- disjoint navigation remains responsive and cannot show stale pixels as current;
- read pixels, summaries, row metrics, scrollbar, picking, selection, and status agree on one
  committed epoch; and
- no `replacement GPU transition bundle ... already active` condition reaches the user.

### R5 — Prove liveness and boundedness before another visual build

Create deterministic non-visual stress traces covering:

- rapid alternating pan and zoom;
- direction reversal before prior work settles;
- repeated zoom-in/zoom-out cache reuse;
- ACTB horizontal and deep vertical navigation;
- BAP1 navigation after ACTB;
- repeated chromosome changes;
- reference-ready and reference-pending states;
- annotation-ready, pending, and failed states;
- budget pressure, budget reduction, and cache clearing;
- GPU candidate pressure and device-resource recreation; and
- cancellation at each source/scene/page checkpoint.

For every trace, assert:

- the exact latest target settles or returns a typed terminal result;
- no target is unsettled and ownerless;
- queue and retry state remain bounded;
- no active replacement bundle survives quiescence;
- no full ledger inventory occurs on a UI/camera poll;
- final semantic output equals a clean independent load;
- complete-evidence certificates and stable handles remain unchanged;
- no stale generation publishes pixels or interaction state; and
- repeated traces reach a stable resource high-water band under an unchanged policy.

Do not select arbitrary frame-time or memory thresholds in advance. First prove the architectural
operation counts and bounded-state properties, then measure native performance and establish
targets from the observed hardware/workload envelope.

Gate:

- all deterministic liveness, semantic-equivalence, and resource-invariant tests pass;
- a captured trace of the formerly stuck sequence reaches quiescence;
- profiling shows no total-scene work in the UI/camera path; and
- no known failure is being hidden by a timeout, limit, fallback, or forced commit.

### R0–R5 implementation and qualification record — 31 August 2026

The automated recovery frontier is complete. Native visual acceptance is recorded separately in
R6 because that assessment belongs to the user.

| Recovery phase | Implemented evidence | Deterministic gate evidence |
| --- | --- | --- |
| R0 — observability | Bounded per-lane `AlignmentNavigationTransaction` journal, constant-size diagnostic, explicit owner/product state, ownerless-target failure | `every_live_phase_names_exactly_one_owner`, `idle_source_cannot_leave_a_running_target_unsettled`, `journal_is_bounded_under_an_arbitrary_input_burst` |
| R1 — control plane | O(1) `LedgerProgress`, coalesced progress subscriptions, typed blocker-specific retries, retained-class/high-water counters, manifest/prefetch/reclaim operation counters | `constant_size_progress_never_enumerates_allocations`, `progress_notifications_are_coalesced_and_only_follow_capacity_changes`, `retained_class_breakdown_reconciles_without_inventory`, `pan_zoom_reversal_and_vertical_return_converge_without_new_scene_work` |
| R2 — owned liveness | One token/generation transaction per panel/track; latest-wins mailbox and source work; source/GPU/budget pressure remain owned; illegal/lost completion becomes terminal | `rapid_pan_zoom_reversal_commits_only_the_exact_latest_target`, `repeated_contig_changes_reject_every_stale_completion_and_settle_the_latest_target`, `a_large_target_burst_keeps_only_latest_bounded_slots`, `active_gpu_settlement_pressure_preserves_latest_camera_and_rejects_a_stale_retry` |
| R3 — single authority | Unified local BAM returns before query-local display construction; legacy row task and poller are compile-disabled; scene epoch supplies rows, pixels, picking, stable-handle selection, and inspection | `unified_annotation_lane_layout_exists_without_a_legacy_query`, `alignment_picking_routes_by_stable_track_frame_before_record_id`, `cpu_projection_and_picking_are_camera_only_views_of_one_scene_snapshot` |
| R4 — coherent presentation | Immutable prior epoch plus private candidate; atomic read/summary replacement; camera-only capture ring; prepared spatial indexes; reference/contextual products settle independently; bounded row-first prefetch | `compatible_read_capture_uses_the_precharged_ring_while_an_unrelated_bundle_is_active`, `compatible_summary_capture_uses_the_precharged_ring_during_an_unrelated_bundle`, `mandatory_batch_failure_never_publishes_a_partial_gpu_page_set`, `core_scene_completes_while_reference_is_pending_and_upgrade_reuses_source_roots`, `contextual_annotation_pending_ready_and_failure_never_own_core_navigation` |
| R5 — stress proof | Deterministic pan/zoom/reversal, rolling pan, LOD return, deep-row, disjoint jump, contig burst, pressure, cache-clear, device recreation, cancellation, and stable-high-water traces | The trace matrix below and the complete focused suite results |

The R1 eviction design was refined during implementation. Renderer pressure recovery no longer
constructs a full ledger or page inventory to hand to an asynchronous sorter. Each renderer
maintains an ordered `(last-used revision, page key)` reclaim index as pages are admitted, touched,
or retired. An explicit pressure pass walks only enough oldest entries to satisfy the exact failed
request, checks pins, and applies that bounded key result. This avoids both a failure-time full
scan/sort and the complexity of transferring live GPU ownership to a worker. It never runs during
idle polling or camera-only capture. Page-table leases conservatively charge the index keys, B-tree
node links, occupancy slack, and allocator metadata, so the index is not an invisible cache. The
exact-stop property is covered by
`cooperative_reclaim_batches_many_summary_pages_around_protected_and_pinned_members`; table/index
charging is asserted by the read and summary atomic-admission tests; inspected keys are exposed in
`reclaim_authority_keys_inspected` diagnostics.

R5 trace coverage:

| Required trace | Principal deterministic evidence |
| --- | --- |
| Rapid alternating pan/zoom and reversal | `pan_zoom_reversal_and_vertical_return_converge_without_new_scene_work`; 10,000-target desktop transaction burst |
| Zoom-in/zoom-out and prepared cache reuse | `bundled_constant_camera_reuses_prepared_geometry_without_uploading_it_again`; read and summary multi-quantum prefetch tests |
| ACTB-like horizontal and deep vertical movement | `adjacent_pans_keep_a_bounded_rolling_window_and_converge_to_a_clean_scene`; `worker_continues_incremental_vertical_residency_after_visible_publication`; block-aligned nearest-first residency tests |
| BAP1-like navigation after a dense locus | `foreground_jump_is_latest_wins_and_matches_a_clean_load`; clean-load scene population, row-atlas fingerprint, and exact-summary equivalence |
| Repeated chromosome changes | 4,096-target `repeated_contig_changes_reject_every_stale_completion_and_settle_the_latest_target` |
| Reference and annotation pending/ready/failed | reference-independent core/upgrade tests; independent contextual-lane transaction test |
| Budget pressure/reduction and cache clearing | source, GPU, and budget-pressure suites; `cache_clear_reissues_a_committed_camera_and_rejects_the_pre_clear_token` |
| GPU recreation and cancellation | `admitted_geometry_is_reused_across_camera_frames_and_device_discard_preserves_scene`; source checkpoint cancellation/salvage and stale-publication tests |
| Stable resources and quiescence | stable charged high-water store test; camera trace charged/retained equality; bundle refusal/settlement tests assert no surviving active replacement |

Qualification commands and results at this frontier:

- `cargo test -p rgv-alignment-scene --all-targets --quiet`: 153 passed.
- `cargo test -p rgv-engine --all-targets --quiet`: 120 passed.
- `cargo test -p rgv-render --all-targets --quiet`: 78 library tests and 1 integration test passed.
- `cargo test -p rgv-desktop --all-targets --quiet`: 435 passed, 1 intentionally ignored.
- `cargo check --workspace --all-targets`, formatting, and diff validation passed.

The desktop count includes the new repeated-contig trace. These numbers must be refreshed if code
changes after this record; they did not by themselves advance R6.

### R6 — Minimum native visual checkpoint

Only after R0–R5 pass, build and launch the RNA BAM/reference/GENCODE session used for the existing
ACTB and BAP1 checks. Ask the user to assess, without UI automation:

1. small prepared pans while holding the pointer;
2. release after pan, checking for a white flash;
3. rapid pan/zoom/reversal followed by rest;
4. zoom-in then zoom-out reuse;
5. ACTB rapid and slow vertical scrolling, including stopping while held;
6. BAP1 navigation after ACTB and scrollbar extent/density;
7. repeated ACTB/BAP1 switching;
8. a disjoint chromosome jump and immediate return;
9. hover and click selection after each navigation mode; and
10. coverage and substitution coherence at an appropriate zoom.

Expected visible behaviour:

- resident surroundings move with the camera rather than becoming white;
- no release flicker or wait-cursor lock occurs;
- rapid input may show truthful loading at an uncovered target, but the exact latest target always
  recovers and settles;
- the vertical thumb and density preview describe the current committed locus;
- old-contig or old-locus reads never appear as current data;
- hover and selection remain functional; and
- no internal pressure/bundle message is presented as a source failure.

If any item fails, stop the checkpoint. Use the failing target's navigation journal and return to
the responsible recovery phase. Do not patch the visual symptom in isolation.

Gate:

- the user explicitly accepts the minimum navigation and interaction behaviour.

Acceptance record — 31 August 2026:

- the corrected release build was launched with the ENCFF686MTA RNA BAM, its matching GRCh38
  reference, the explicit GENCODE v29 gene track, a 10-million alignment ceiling, and the persisted
  4 GiB cache policy;
- startup selected the Apple M4 Metal adapter and opened the auxiliary gene track without a source
  failure;
- after manual assessment, the user reported that the recovered viewer was looking good and asked
  for the completed work to be staged and committed; and
- R6 is therefore accepted for the minimum recovery navigation and interaction contract. This
  acceptance does not imply completion of the later specialist product families tracked in the
  master implementation plan.

### R7 — Resume the unified-scene roadmap

After native acceptance:

1. finish coverage and generic-substitution qualification;
2. migrate junction/Sashimi and modification GPU presentation and interaction;
3. migrate export, automation, MCP, keyboard/mate navigation, properties, and diagnostics;
4. delete the remaining legacy alignment integration under Phase 8;
5. run the full WGS, exome, RNA, long-read, multi-track, multi-panel, low-memory, and device-loss
   matrix; and
6. only then resume contextual-lane and remote/CRAM expansion.

## Deletion and replacement map

| Current responsibility | Recovery action |
| --- | --- |
| Full `AllocationLedger::snapshot()` used as retry progress | Replace with O(1) progress state and event-driven wake-up |
| Per-poll source/budget/GPU retry scans | Replace with typed retry registry keyed by blocker and target |
| `pending_query` plus scattered generation comparisons | Replace with the owned navigation transaction |
| `ensure_alignment_rows()` in navigation/display readiness | Remove from unified path; consume scene row-atlas authority |
| `current_query` / `layout_query` as desktop semantic inputs | Remove from unified production consumers |
| Generic alignment display-list pixels | Delete after scene renderer parity |
| Retained alignment navigation texture | Delete for the unified path |
| Query-local picking and selection | Delete after stable-handle scene cutover |
| Global alignment/reference/annotation readiness conjunction | Replace with an explicit product dependency graph |
| Catch-all `Updating view...` status | Replace with bounded transaction/product status |

Deletion occurs after replacement tests pass, but the old authority must not remain connected as a
runtime fallback. A short-lived compatibility adapter may exist only at a typed unsupported-source
boundary and must have an explicit deletion condition.

## Verification layers

1. **Unit/property:** ledger aggregates, target-state transitions, token rejection, cancellation,
   pressure wake-up, row-atlas identity, and dependency-graph truth tables.
2. **Deterministic component:** source/scene/renderer burst traces, clean-load equivalence, bounded
   queues, resource reconciliation, and quiescence.
3. **Native instrumentation:** operation counts, thread ownership, memory categories, high-water
   behaviour, and captured navigation journals.
4. **Manual visual:** only the user assesses animation, flicker, continuous pixels, scrollbar
   feel, hover, and selection.

Passing unit tests alone cannot qualify native navigation. Conversely, a visually smooth run does
not qualify the architecture unless the liveness, completeness, and resource invariants also pass.

## Stop conditions

Stop implementation and return to architectural review if any of the following occurs:

- a proposed fix requires incomplete evidence or silent record truncation;
- the UI/camera path still performs work proportional to total retained evidence;
- two alignment semantic authorities remain connected after R3;
- the latest target can become unsettled without an owner;
- correctness depends on a timeout, sleep, repeated blind retry, or increased hard-coded budget;
- one candidate can partially publish read/summary/interaction state;
- the same deterministic navigation trace has unbounded memory or queue growth; or
- the R6 visual checkpoint fails in a way not explained by the recorded transaction state.

## Definition of recovery complete

Recovery is complete only when:

- R0–R5 automated and instrumented gates pass;
- the user accepts R6;
- the unified local-BAM path has one scene-backed alignment authority;
- rapid navigation always settles the exact latest target or reports a truthful terminal result;
- prepared pan and vertical scroll retain resident pixels without white replacement;
- process memory is reconciled with the configured resource policy;
- no full ledger inventory or row reconstruction runs on the UI/camera path;
- complete evidence, exact summaries, stable selection, and derived-lane coherence are preserved;
  and
- the master implementation plan is unblocked with measured qualification evidence.
