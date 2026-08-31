# Unified alignment-scene interaction and capacity remediation plan

Status: proposed; implementation has not started.

Written on 31 August 2026 after native ACTB testing invalidated the combined Phase 7–9 visual
checkpoint in `UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md`. This plan is the temporary
execution authority for alignment interaction, publication, and resource control. Phase 10
remote/CRAM qualification remains blocked until this plan is implemented, automatically
qualified, and manually accepted by the user.

The preceding recovery work remains useful evidence, but its accepted R6 behaviour has regressed
in the expanded desktop path. Earlier claims that current camera capture is constant-cost or that
ordinary GPU pressure remains nonterminal are therefore reopened rather than treated as proven.

## Outcome

RGV should browse a settled dense alignment scene as a camera-driven world, not as a succession of
exact-viewport reconstructions.

After ACTB has settled, a small contained pan must:

- retain the complete exact alignment evidence and stable row assignment;
- move already prepared read and derived-lane geometry continuously;
- update only bounded camera, clipping, visible-page, text, and interaction state;
- perform no BAM acquisition, row-atlas rebuild, full-row-stack scan, scene replacement, or
  unchanged geometry upload; and
- remain responsive while optional preparation immediately yields to foreground input.

Changing locus, layout contract, reference authority, or leaving the prepared content envelope
may require new work. That work must be latest-wins and privately staged. It may never blank,
partially mutate, falsely truncate, or permanently disable the last coherent presentation.

Resource pressure must be handled by one end-to-end publication transaction and one aggregate
resource authority. A temporary capacity shortfall may wait for relevant progress; an
intrinsically impossible request may become truthfully `BudgetLimited`. Neither is a source
failure, and neither may make the rest of the application unusable.

## Why this plan exists

### Native symptoms

The current RNA/ACTB build exhibited three related but distinct failures:

1. browsing was significantly more laggy than the previously accepted R6 build;
2. slight horizontal pans repeatedly appeared to calculate the complete approximately 50,000-row
   stack again; and
3. after limited browsing, atomic GPU presentation failed with:

   ```text
   alignment GPU replacement could not reclaim sufficient capacity:
   Insufficient {
       required_bytes: 115079511,
       released_bytes: 0,
       remaining_bytes: 115079511,
       pinned_bytes: 910996762,
   }
   ```

RGV then converted the capacity condition into a permanent Complete-read failure and retained no
owner capable of retrying or settling the target.

### Established performance evidence

The current interaction route does not behave like a camera-only renderer:

- every distinct horizontal camera interval can miss the exact-interval row-activity cache;
- the row-activity calculation walks all row extents, overlap structures, and connectors;
- the ACTB fixture contains approximately 693,350 alignments across 57,861 layout rows;
- read camera capture re-enters general preparation, including CPU label projection,
  reconciliation, sorting, vector construction, buffer writes, and pin bookkeeping;
- native Sashimi camera capture can rescan visible junction primitives to resolve presentation
  metrics;
- contextual lanes are CPU-reprojected, cloned, recomposed, rehashed, and re-admitted during raw
  camera input even when their immutable geometry is already resident; and
- progressive vertical preparation and fixed page-count GPU prefetch can repeatedly request
  redraws and compete with foreground interaction.

The current latest-wins mailbox can replace work that has not begun. It cannot interrupt an
already running complete-row activity calculation, so rapid input may repeatedly start expensive
work whose result is obsolete before completion.

A live sample also reported a physical footprint of approximately 2.3 GiB, a 2.8 GiB peak, and
approximately 2 GiB in swapped malloc regions. Paging is an amplifier, not the root cause: even
unlimited physical RAM would not make repeated scene-sized camera work architecturally correct.

The previous headless ACTB benchmark did not exercise the complete desktop GPU, contextual-lane,
input, and composition path. It therefore qualified semantic scene publication without
qualifying native interaction performance.

### Established capacity evidence

The displayed numbers mean that the attempted replacement needed approximately 110 MiB more
capacity and that the two alignment GPU renderers released none. They do not mean that the
approximately 869 MiB ledger-wide pin total was the only occupied memory or that every pinned byte
directly blocked the candidate.

The current allocation ledger is shared by alignment source evidence, summaries, row atlases,
CPU semantic pages, read GPU pages, summary GPU pages, and multiple alignment owners. Foreground
GPU reclaim, however, asks only the read and summary GPU renderers to evict. It does not coordinate
reclaim with inactive source or semantic-page owners of the same ledger.

Publication also crosses an incomplete ownership boundary:

1. a new worker scene can become the desktop source/panel candidate before GPU admission;
2. the old `PresentedAlignmentEpoch` must retain its semantic and GPU state to prevent blanking;
3. GPU replacement retirement credit covers old read and summary GPU allocations, but not all old
   semantic/source allocations that successful end-to-end commit would release;
4. admission can therefore count old semantic ownership as continuing steady state; and
5. success is required to release that ownership even when retaining it is what prevents success.

This is an admission stalemate, not proof that complete alignment data is infeasible.

Finally, the original typed steady-budget or transition-overlap cause is flattened into generic
GPU pressure. An `Insufficient` outcome is then converted to terminal `Failed`, its retry authority
is cleared, and the UI presents the result as unavailable reads. This explains why the application
does not recover.

### Budget-setting mismatch

The user-visible cache figure is not currently a process-wide memory ceiling. The same configured
amount is used for multiple base, transition, transient, contextual-GPU, and generic-renderer
allowances. Driver allocations, allocator retention, and some UI/runtime memory also remain
outside the alignment ledger.

Adding RAM or increasing the setting may allow one failing candidate to pass and reduce swapping,
but it only moves the threshold and may increase the amount of memory subject to paging. A larger
hard-coded allowance is not a remedy.

## Architectural decision

Preserve the parts of the unified scene that remain correct:

- stable source-position handles and complete exact record identity;
- immutable evidence roots and coverage certificates;
- exact summary evidence;
- immutable canonical row assignment;
- semantic world-coordinate geometry;
- one CPU/GPU semantic model;
- stable picking and inspection handles; and
- current/candidate publication rather than in-place mutation.

Rebuild the boundaries that have failed:

- exact camera coordinates must not identify content or invalidate the row atlas;
- a camera frame must not call the general content-preparation path;
- exact row activity and derived-lane metrics need immutable indexes rather than full-scene scans;
- CPU semantic state and GPU state must enter and leave through one publication transaction;
- every owner of a shared budget must participate in one aggregate reclaim/admission policy; and
- speculative vertical residency and prefetch must be scheduled below foreground input.

Do not reintroduce a legacy renderer, retained navigation texture, screen-space read cache,
sampling, hidden read cap, timeout-based commit, repeated blind retry, or hard-coded memory
increase. GPU acceleration remains optional: CPU and GPU paths consume the same immutable scene,
and correctness may not depend on a vendor-specific compute path.

This is a substantial control-plane and presentation-path refactor. It is not a wholesale rewrite
of HTS decoding, complete evidence, row assignment, stable identity, or semantic geometry.

## Product contract

### Complete data

1. Every alignment satisfying the active source/filter contract is represented exactly. No
   sampling, coverage-dependent truncation, or silent maximum may be used to improve interaction.
2. Complete is a semantic guarantee, not a requirement that every derivative occupy GPU memory
   simultaneously.
3. Canonical evidence, summaries, semantic pages, and GPU pages may have different residency, but
   every displayed derivative must identify the exact evidence epoch that produced it.
4. If the minimal exact requested presentation is intrinsically impossible under the configured
   policy, RGV reports a truthful, non-destructive `BudgetLimited` result with actionable required
   capacity. It does not publish a partial result as Complete.
5. Whole-stack GPU residency may warm opportunistically and should provide instant arbitrary
   vertical movement when it fits. It may not own navigation liveness or monopolise foreground
   reserve.

### Continuous navigation

1. A settled, contained pan changes the live camera, not the content epoch.
2. Prepared overlapping pages remain visible throughout pointer-down movement and release.
3. Moving beyond prepared coverage retains reusable overlap and prepares only newly required
   edges or contract products.
4. A candidate cannot clear, recolour, repack, or partially replace the committed epoch.
5. A disjoint contig jump may require a neutral pending surface, but the application and controls
   remain responsive and the exact latest target retains an identifiable owner.

### Interaction and derived products

1. Read bodies, coverage, substitutions, Sashimi, modifications, row metrics, scrollbar state,
   picking, hover, selection, and inspection identify one coherent committed alignment epoch.
2. Hover and selection modify a bounded overlay or interaction buffer, not immutable content.
3. Contextual lanes may have independent candidate lifetimes unless an explicit coherence group
   requires atomic replacement with alignment products.
4. Text and collision layout may be camera-dependent, but work must be bounded by visible or
   prepared labels rather than the complete scene.

### Visual assessment

Automated tests may validate pixels, operations, ownership, and timing. They may not claim that
dragging, flicker, visual continuity, scrollbar feel, or perceived responsiveness is acceptable.
Only the user records the native visual gate.

## Non-negotiable invariants

### Camera/content separation

For movement inside a committed content envelope:

- source acquisition count is zero;
- canonical decode and row-atlas construction count are zero;
- complete-row activity scans are zero;
- semantic-page construction and content replacement count are zero;
- immutable geometry upload bytes are zero;
- allocation registration, retirement, and pin-set reconstruction count are zero;
- live camera work is bounded by intersecting pages and visible semantic overlays; and
- cost does not scale with the total off-screen vertical row count.

Camera state is not part of `ContentEpochId`, `RowAtlasId`, immutable page keys, or source-query
identity. It may be part of a lightweight `CameraFrameId` and exact indexed display metrics.

### Publication atomicity

1. There is at most one committed `PresentedEpoch` and one private latest `CandidateEpoch` per
   panel/alignment coherence owner.
2. Candidate CPU and GPU allocations are reserved before any current authority is changed.
3. A move-only `EpochRetirement` identifies every exact old allocation guaranteed to disappear on
   commit.
4. Admission uses the combined semantic-plus-GPU retirement claim once.
5. Commit swaps semantic state, GPU state, row metrics, scrollbar authority, picking, and
   interaction authority atomically.
6. Failure or supersession releases candidate-only leases and leaves the old epoch byte-for-byte
   and interaction-for-interaction unchanged.

### Resource liveness

1. One aggregate broker accounts for all application-owned alignment CPU, alignment GPU,
   contextual GPU, generic GPU, candidate, transition, and transient capacity in its declared
   scope.
2. Driver/runtime memory that cannot be exactly charged is reported separately and incorporated
   into measured defaults, not hidden inside a misleading cache label.
3. Foreground visible publication has an explicit reserve that optional work cannot consume.
4. Reclaim coordinates every owner of the relevant budget in a deterministic order.
5. `WaitingForCapacity` is nonterminal and wakes only after relevant progress.
6. `BudgetLimited` means the protected minimum plus the candidate cannot fit under the current
   policy after all legal reclaim. It remains interactive and may retry after target, product,
   device, or budget change.
7. Source failure, capability unavailability, temporary capacity pressure, and intrinsic capacity
   limitation are distinct product states.

### Scheduling

1. Raw pointer events update one latest live camera state. Heavy work is not synchronously
   dispatched once per input delta.
2. Foreground camera publication preempts or pauses optional vertical preparation, label
   enrichment, and GPU prefetch.
3. Work already running observes supersession at bounded checkpoints; uncancellable complete-scene
   scans are prohibited from the camera path.
4. Background work uses a measured wall-clock quantum and foreground watermark rather than a fixed
   number of arbitrarily sized pages.
5. No self-requested redraw loop may starve event handling.

## Target architecture

```text
source / filter / layout contract
                |
                v
       immutable ContentEpoch
       evidence + row atlas + indexes
                |
                v
    immutable semantic page directory
                |
                +------------------------------+
                |                              |
                v                              v
       PresentedEpoch                    CandidateEpoch
 semantic leases + GPU leases      private leases + reservation
 row/scroll/pick authority          combined retirement proposal
                |                              |
                +---------- atomic commit <----+
                |
                v
          LiveCameraFrame
 camera/clip/row transform + bounded overlays
                |
                v
              GPU/CPU
```

The content epoch changes only when source coverage, evidence, filter, row-layout, relevant
reference, or semantic product authority changes. The camera frame changes whenever the user
moves. A camera frame references a committed content/presentation epoch; it does not rebuild or
repin that epoch.

### 1. Stable content envelope and page identity

Use source-coordinate page keys derived from content authority, genomic tile, row block, semantic
family, and LOD. Exact viewport endpoints are excluded.

The committed epoch advertises a truthful content envelope:

- genomic coverage and prepared margin;
- row-atlas lineage and total row extent;
- resident/readable semantic page directory;
- derived-product readiness; and
- the camera/LOD classes it can present without content replacement.

Contained movement performs page intersection against this immutable directory. Boundary movement
retargets only edge requirements and reuses every overlapping page. Chunk/tile dimensions and
margin sizes are selected from measurement and exposed through the existing browsing-prefetch
policy where useful; this plan does not prescribe arbitrary constants.

### 2. Exact indexed row activity

Build `RowActivityIndex` with the immutable row atlas. It must answer which stable rows and density
segments overlap a genomic interval without walking every row and every connector.

Candidate implementations include an interval index producing a row bitmap, genomic-bin row
bitmaps with exact boundary correction, or a measured hybrid. The public contract is fixed even
if the internal structure changes:

- exact results equivalent to a clean record/row scan;
- query cost proportional to index traversal and result representation, not blindly to every row;
- immutable identity tied to the row atlas rather than the camera;
- deterministic results for split, spliced, supplementary, paired, and long-read spans; and
- reusable multi-resolution aggregates for the vertical scrollbar density preview.

If an interval genuinely contains nearly every row, processing the result bitmap is unavoidable;
rescanning every row's alignment layers and connectors is not. Row activity must never trigger
row assignment or semantic page reconstruction.

### 3. Genuine camera frames

Split renderer APIs into two non-overlapping operations:

- `prepare_content(...)`: allocate, upload, admit, pin, and construct immutable draw/page
  authority for a candidate epoch; and
- `capture_camera(...)`: reference a committed authority and write only precharged camera, clip,
  visible-range, and bounded interaction data.

`capture_camera` may not call `prepare_content` internally. Debug assertions and operation
counters enforce that boundary.

Read labels use immutable page-local anchors. Camera-dependent collision work is limited to
visible/prepared anchors and cached independently of read-body publication. If measurements show
benefit, GPU culling or indirect drawing may accelerate visible selection, with an equivalent CPU
indexed fallback. GPU compute is not required for semantic correctness.

Summary families use immutable per-page or range-query aggregates:

- coverage and allele/substitution pages retain exact evidence-derived values;
- Sashimi range scaling and eligibility query a precomputed junction index rather than rescanning
  every primitive on every camera delta; and
- modification summaries follow the same content/camera split.

Contextual rectangles remain in source coordinates in persistent buffers. A contained camera move
updates transforms and clips without CPU-reprojecting, cloning, recomposing, rehashing, or
re-admitting every lane. Text is a separately bounded semantic overlay.

### 4. End-to-end epoch transaction

Introduce explicit types conceptually equivalent to:

```rust
struct PresentedEpoch {
    content: Arc<ContentEpoch>,
    semantic: SemanticPresentationLease,
    gpu: AlignmentGpuLease,
    interaction: InteractionAuthority,
    resources: PresentedResourceLease,
}

struct CandidateEpoch {
    target: NavigationTarget,
    content: Arc<ContentEpoch>,
    semantic: CandidateSemanticLease,
    gpu: CandidateGpuLease,
    reservation: ForegroundReservation,
}

struct EpochRetirement {
    old_epoch: EpochId,
    exact_allocation_claim: MoveOnlyRetirementClaim,
}
```

A worker result remains private candidate state until all mandatory semantic and GPU products are
ready and aggregate admission succeeds. It must not become source/panel `current` prematurely.

The resource broker validates that the retirement claim owns every live lease it credits. On
commit, one token-checked operation swaps the candidate into `PresentedEpoch`, updates navigation
and interaction authority, then retires the old epoch. On cancellation or admission failure, the
candidate drops without touching the presented epoch.

### 5. Aggregate resource broker

Replace renderer-local foreground reclaim with a broker that knows all owners and allocation
classes within the configured scope.

Suggested reclaim order:

1. cancelled candidate reservations and abandoned transient work;
2. speculative whole-stack GPU residency and distant prefetch pins;
3. inactive GPU pages and contextual/generic slabs;
4. inactive semantic and optional vertical presentation pages;
5. inactive summary derivatives;
6. inactive source/evidence caches that remain reloadable; and
7. bounded navigation history.

Current committed evidence required for correctness, mandatory visible pages, and exact rollback
authority are protected. Reclaim reports protected, pinned, evictable, credited-retiring, and
out-of-scope bytes by owner, epoch, and allocation class.

Admission distinguishes:

- steady-state capacity;
- old/new transition overlap;
- transient scratch;
- foreground reserve; and
- separately reported driver/runtime overhead.

The settings UI must describe the actual aggregate scope. Defaults and optional advanced controls
are chosen after native measurement. They persist between runs, and changing them invokes one
ordered broker transaction rather than multiplying the value across independent pools.

### 6. Input-aware residency and prefetch

Visible and near-visible work has priority over complete-stack warming. Optional residency enters
only when:

- no foreground gesture is active;
- no newer camera or navigation target is pending;
- the foreground reserve and low-watermark remain satisfied; and
- its work quantum fits the measured UI/GPU scheduling budget.

The prefetch owner keeps one latest horizontal margin target and one latest vertical target. It
reuses pages that still contribute after reversal. Whole-stack residency holds a byte-bounded,
revocable lease; it is released first on foreground pressure.

For a supported dense fixture such as ACTB, qualification should attempt complete-stack warm
residency under the measured default and report whether it fits. If it does not fit, every row
remains exact and reachable through demand paging, the current presentation remains coherent
while the requested page prepares, and the UI reports warming or `BudgetLimited` truthfully rather
than showing white or declaring reads unavailable.

## Implementation workstreams and gates

No native visual checkpoint is requested until all implementation workstreams and automated gates
below are complete. UI automation is not used as a substitute for the final user assessment.

### Phase 0 — Preserve, reproduce, and establish comparative baselines

1. Preserve the current uncommitted branch state in an explicit checkpoint before destructive
   implementation begins.
2. Identify the exact previously accepted R6 revision and build it independently for comparison.
3. Record deterministic input/state traces for:
   - settled small ACTB pans;
   - rapid ACTB pan/reversal;
   - ACTB vertical warm-up and deep scrolling;
   - zoom in/out/return;
   - ACTB to BAP1 and back;
   - repeated locus/contig changes; and
   - the observed approximately 110 MiB capacity failure.
4. Instrument the current build without changing scheduling:
   - raw input events and presented camera frames;
   - source requests, decode work, row-atlas builds, and row-activity index/cache work;
   - semantic page construction and progressive publications;
   - read, summary, context, and generic-renderer CPU preparation;
   - immutable upload, camera-write, overlay-write, and pin operations;
   - prefetch quanta and self-requested redraws;
   - candidate/retirement/admission state;
   - ledger bytes by owner/class/epoch and OS resident/compressed/swap indicators; and
   - input callback, input-to-present, CPU-frame, and GPU queue durations.
5. Run the same trace with read labels, Sashimi, context, coverage, and background prefetch
   independently enabled to attribute—not guess at—the regression.

Gate:

- the current and prior builds have reproducible operation-count and timing reports;
- the capacity failure identifies steady versus transition origin and exact owner/class demand;
- the apparent complete-stack recalculation is attributed to named operations;
- instrumentation is diagnostic-gated and does not materially perturb the result; and
- no workaround or larger budget has been introduced.

### Phase 1 — Establish the new contracts and compile-time boundaries

1. Add the stable content-envelope/page-key contract and exclude exact cameras from content
   identity.
2. Define separate `prepare_content` and `capture_camera` interfaces for reads, summaries, and
   contextual lanes.
3. Define `PresentedEpoch`, private `CandidateEpoch`, combined `EpochRetirement`, typed capacity
   causes, and broker request/result contracts.
4. Define `RowActivityIndex` and range-aggregate contracts with clean-scan reference oracles.
5. Define an input scheduler contract with one latest camera, explicit foreground state, and
   cancellable optional work.
6. Add dependency/source checks preventing camera capture from calling content preparation or
   source acquisition.

Gate:

- contracts compile headlessly without depending on desktop UI state;
- property tests establish epoch/retirement linear ownership;
- a camera-only test fails if it registers, pins, retires, uploads, or constructs immutable
  content; and
- there is no compatibility contract that permits the old exact-viewport path as fallback.

### Phase 2 — Repair aggregate admission and publication liveness

1. Implement the aggregate resource broker and owner registration.
2. Move alignment CPU, alignment GPU, contextual GPU, and generic GPU application-owned capacity
   into the declared accounting scope or an explicit non-borrowing reservation where integration
   must be staged.
3. Add combined semantic-plus-GPU retirement credit.
4. Keep worker/source/panel results private until aggregate admission succeeds.
5. Commit one end-to-end epoch atomically.
6. Replace terminal capacity settlement with token-owned `WaitingForCapacity` and truthful
   `BudgetLimited` states.
7. Retry once per relevant broker progress revision; reject stale tokens and avoid polling/spin.
8. Make diagnostics retain the original steady/transition/transient cause and allocation
   breakdown.

Gate:

- old semantic plus old GPU plus candidate overlap may exceed base while the candidate steady
  state fits after combined retirement, and the transaction succeeds once;
- rejected/cancelled candidates release every candidate-only lease and preserve exact old
  presentation and hits;
- cross-owner reclaim can satisfy a request that GPU-only reclaim cannot;
- a protected minimum that genuinely cannot fit becomes interactive `BudgetLimited`, never source
  failure;
- repeated ACTB-style navigation retains at most one current plus one candidate and reaches a
  stable high-water band; and
- no pressure message can strand an ownerless navigation target.

### Phase 3 — Separate camera movement from content work

1. Route raw gestures into one latest `LiveCamera` state rather than synchronous heavy dispatch per
   pointer delta.
2. Classify movement against the committed content envelope before scheduling source or content
   work.
3. For contained movement, capture a camera frame directly from the committed epoch.
4. For boundary movement, reuse overlap and request only missing edge pages/products under a
   latest-wins candidate.
5. Build and integrate the exact `RowActivityIndex` and multi-resolution scrollbar aggregates.
6. Remove exact camera intervals from row-activity cache identity and delete full-row scans from
   the input/render path.
7. Add cancellation checkpoints to every remaining candidate index/page build.

Gate:

- a settled contained ACTB pan produces exactly zero source, atlas-build, full-row-scan,
  semantic-build, replacement, immutable-upload, and pin-reconstruction operations;
- its foreground work is bounded by intersecting pages/visible overlays rather than 57,861 rows;
- clean indexed row activity and scrollbar results equal a full independent scan;
- a one-pixel pan cannot start complete vertical residency again; and
- crossing one prepared boundary constructs only the missing edge while retaining overlap.

### Phase 4 — Make every renderer genuinely camera-driven

1. Split read renderer general preparation from camera capture; preserve immutable draw/page and
   pin authority in the presented epoch.
2. Move label anchors into immutable pages and bound collision work to visible/prepared anchors.
3. Add exact indexed/range aggregates for Sashimi and other camera-dependent summary metrics.
4. Move contextual lanes to persistent source-coordinate buffers with uniform/scissor camera
   updates.
5. Separate contextual text from immutable shapes and bound its layout.
6. Ensure hover and selection update only bounded overlays.
7. Preserve CPU projector equivalence and device-loss reconstruction without a second semantic
   architecture.

Gate:

- repeat camera capture uploads zero immutable read, summary, or context geometry;
- read-body publication performs no label HashMap/sort over off-screen content;
- Sashimi capture performs no primitive-wide rescan;
- context camera movement performs no all-lane CPU rectangle projection, display-list cloning,
  generic recomposition, or content admission;
- hover/selection changes no content authority; and
- CPU/GPU framebuffer and picking oracles agree across representative LODs and products.

### Phase 5 — Make preparation input-aware and whole-stack residency revocable

1. Replace fixed pages-per-redraw prefetch with measured time-budgeted background work.
2. Suspend optional CPU/GPU preparation immediately while a gesture or foreground candidate is
   active.
3. Reserve foreground capacity before admitting optional work.
4. Make horizontal and vertical prefetch retargetable and overlap-reusing.
5. Replace permanent all-page speculative pins with a byte-bounded revocable residency lease.
6. Measure compact GPU page layouts and indirect/GPU culling only where they reduce actual cost;
   retain the platform-neutral CPU indexed path.
7. Ensure background redraw requests cannot starve the event loop.

Gate:

- foreground input preempts optional work within a bounded checkpoint;
- an input burst produces bounded queued state and the exact final camera;
- reversing direction retains useful overlap and abandons obsolete edge work;
- optional residency cannot cause foreground admission failure;
- warm ACTB vertical movement uses resident pages continuously when the configured policy can fit
  the complete stack; and
- otherwise exact demand paging remains live, coherent, and free of white replacement or false
  unavailability.

### Phase 6 — Complete desktop integration and delete superseded paths

1. Route row metrics, scrollbar, reads, summaries, picking, selection, inspection, export,
   automation, and diagnostics through the committed epoch and live camera frame.
2. Remove the old general-prepare camera routes from production.
3. Remove GPU-only foreground reclaim and every capacity-to-source-failure conversion.
4. Remove premature worker-scene promotion and split semantic/GPU retirement.
5. Remove raw-input contextual CPU reprojection and generic recomposition.
6. Remove fixed-page prefetch/redraw loops and permanent speculative full-scene leases.
7. Add a machine-readable/source-level inventory that fails if a deleted authority or fallback
   returns.

Gate:

- the production local-BAM path has one content authority, one presented epoch, one camera path,
  and one resource broker;
- no old path is reachable through settings, pressure, unsupported product, or device recovery;
- feature inventory proves coverage, substitutions, Sashimi, modifications, context, interaction,
  and export retain their exact semantic source; and
- all affected crate/workspace tests, formatting, and source inventories pass.

### Phase 7 — Automated qualification and native user acceptance

Run deterministic and native-instrumented workloads across:

- ACTB extreme-depth RNA and BAP1 moderate-depth RNA;
- representative WGS and exome low/high-depth loci;
- one and multiple alignment tracks/panels;
- coverage, substitutions, Sashimi, modifications, reference, genes, and auxiliary lanes;
- cold load, settled contained pan, reversal, zoom return, deep vertical scroll, locus switch, and
  contig switch;
- low, measured-default, and high persisted aggregate budgets; and
- pressure, cancellation, cache clear, budget change, and device-resource recreation.

Establish numerical performance thresholds only after Phase 0 records comparable current and
accepted-R6 baselines. Record median, tail, and worst-case input-to-present latency together with
operation counts; a fast frame that secretly reconstructs the scene does not pass.

Automated gate:

- every invariant and workstream gate above passes;
- repeated workloads converge to bounded queue, allocation, and high-water state;
- the current regression is measurably removed relative to the current build and does not regress
  the accepted R6 baseline on the same hardware/workload;
- zero-work contained-pan counters remain zero in a release build with diagnostics enabled; and
- the exact latest target always commits, remains truthfully pressure-owned, or returns an
  interactive typed terminal capability/capacity result.

Only then launch the RNA/GRCh38/GENCODE session and ask the user to assess:

1. small held ACTB pans after settlement, confirming that no complete-stack recalculation appears;
2. rapid pan and reversal, confirming continuous resident reads and no white/texture replacement;
3. pan release, checking for flicker;
4. zoom in/out/return, checking reuse and exact substitutions;
5. rapid and slow ACTB vertical scrolling, including stopping while held;
6. ACTB/BAP1 switching, scrollbar extent/density, hover, and selection;
7. Sashimi, coverage, modifications, genes, reference, and auxiliary-lane coherence; and
8. repeated mixed navigation under the measured default budget, confirming that no internal
   capacity error, permanent `Updating view...`, wait-cursor lock, or unusable state occurs.

The user alone records acceptance. A successful build, screenshot, automated cursor movement, or
agent interpretation cannot substitute for it.

## Deletion and replacement map

| Current responsibility or defect | Required replacement |
| --- | --- |
| Exact camera interval as row-activity cache key | Immutable atlas-bound `RowActivityIndex` with exact interval queries |
| Complete-row activity scan during camera updates | Indexed row bitmap/density aggregation outside row assignment |
| Heavy input dispatch for every pointer delta | One latest `LiveCamera`, sampled/presented by the render loop |
| Read `capture_camera_frame` re-entering general preparation | Strict precharged camera capture over immutable content authority |
| Per-camera CPU label projection across page content | Immutable anchors plus bounded visible-label layout |
| Per-camera Sashimi primitive scan | Exact junction range aggregates/index |
| Context CPU reprojection and generic recomposition on pan | Persistent source-coordinate GPU geometry and camera uniforms |
| Worker scene promoted before GPU admission | Private end-to-end `CandidateEpoch` |
| GPU-only retirement credit | Combined semantic/source/GPU `EpochRetirement` |
| Renderer-only foreground reclaim on a shared ledger | Aggregate resource broker and ordered cross-owner reclaim |
| Generic `Insufficient` converted to terminal failure | Typed `WaitingForCapacity` or interactive `BudgetLimited` |
| Fixed pages-per-frame prefetch | Measured time-budgeted, input-aware optional scheduler |
| Permanent speculative all-page GPU pins | Byte-bounded, foreground-revocable residency lease |
| Cache setting multiplied across independent pools | Persisted aggregate policy with truthful scope and diagnostics |

## Required diagnostics

The implementation is not qualified unless diagnostics can answer, without a full hot-path
inventory:

- Was this camera move contained, boundary-extending, or content-changing?
- Which content and presentation epoch supplied visible pixels and hits?
- Did source I/O, row assignment, activity indexing, semantic construction, or immutable upload
  occur, and why?
- How many rows, overlap entries, connectors, labels, junctions, pages, and contextual primitives
  were visited?
- Which work was foreground, optional, cancelled, reused, or superseded?
- What bytes are current, candidate, retiring, protected, pinned, evictable, transient, and
  outside the broker, grouped by owner and class?
- Did pressure originate from steady state, transition overlap, transient scratch, or foreground
  reserve?
- Who owns a waiting target, what exact progress revision can wake it, and what result follows?
- How do tracked allocations reconcile with process resident, compressed, swapped, allocator, and
  driver-reported memory?

Normal camera publication reads only incrementally maintained counters and bounded owner state.
Full allocation inventories are explicit diagnostics, never a per-frame liveness mechanism.

## Validation strategy

### Correctness and equivalence

- indexed row activity versus exhaustive clean scan;
- exact summary and Sashimi range aggregates versus primitive scan;
- stable rows and handles across contained pan and envelope extension;
- CPU versus GPU geometry, clipping, picking, hover, and selection;
- complete record/summary fingerprints before and after eviction/reload; and
- clean independent target load versus final result after cancellation storms.

### Transaction and resource properties

- move-only candidate and retirement ownership;
- combined old semantic/GPU retirement admission;
- rejection and cancellation rollback;
- deterministic cross-owner eviction order;
- protected-minimum and intrinsic-capacity classification;
- one retry per relevant progress revision;
- latest-target supersession while waiting for capacity;
- aggregate settings persistence and ordered budget changes; and
- stable high-water behaviour under repeated dense navigation.

### Interaction operation-count properties

- settled contained pan has zero source/content/replacement work;
- vertical camera movement does not repack rows;
- hover/selection does not replace content;
- derived/context cameras do not upload unchanged geometry;
- optional preparation yields to foreground input; and
- quiescent UI performs no scene-sized or allocation-inventory work.

### Native measurements

- input callback and input-to-present latency distributions;
- CPU scene, renderer preparation, and GPU queue/present durations;
- operation and allocation counts per presented frame;
- cold/warm source and page reuse;
- foreground versus optional work time;
- tracked high-water, process footprint, page faults, compression, and swap; and
- whole-stack residency fit and warm-up behaviour by fixture and policy.

## Stop conditions

Stop and return to architectural review rather than patching the symptom if:

- contained camera movement still performs work proportional to total retained rows, reads,
  junctions, labels, contextual primitives, or allocations;
- exact viewport coordinates remain part of immutable content identity;
- `capture_camera` can allocate immutable content or call general preparation;
- old semantic ownership cannot participate safely in end-to-end retirement;
- one shared budget remains controlled by owner-local reclaim only;
- liveness depends on increasing a cache value, a timeout, blind retry, or forced commit;
- whole-stack GPU residency is required for semantic completeness or foreground navigation;
- a pressure/cancellation path clears a coherent committed epoch;
- complete reads or derived evidence are sampled, capped, or silently omitted;
- two alignment presentation authorities or a legacy fallback remain reachable;
- the same deterministic trace grows memory, pins, queues, or candidate state without bound; or
- automated tests are used to claim the user's visual acceptance.

## Relationship to existing plans

- `UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md` remains the master product and feature plan,
  but its combined Phase 7–9 visual checkpoint is failed and its Phase 10 frontier is blocked.
- `UNIFIED_ALIGNMENT_SCENE_RECOVERY_PLAN.md` remains the historical R0–R6 recovery record. The
  current regression demonstrates that later integration violated some of its invariants; this
  document does not rewrite that history.
- `DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md` covers canonical row-construction algorithms. This
  plan addresses repeated row-activity/presentation work after the row atlas already exists; it
  does not reopen canonical row correctness without measurement proving a separate defect.
- `ALIGNMENT_BROWSING_WORKING_SET_PLAN.md` and `COMPLETE_READ_PRESENTATION_PLAN.md` remain useful
  design evidence, but this plan's aggregate broker, complete epoch transaction, and strict
  camera/content boundary supersede any incompatible cache or presentation advice.

After user acceptance, update the master plan's progress ledger with the measured evidence,
record this remediation as complete, and resume Phase 10. Do not mark the older Phase 7–9 claims
accepted merely because implementation or automated gates pass.

## Definition of done

This remediation is complete only when:

- small settled ACTB pans never recalculate the complete row stack or reconstruct content;
- camera work is bounded by intersecting pages and visible overlays rather than total scene size;
- complete exact reads and all derived products retain one coherent semantic authority;
- CPU and GPU publication enter through one end-to-end epoch transaction;
- one aggregate broker enforces truthful settings and coordinates all relevant reclaim owners;
- temporary capacity pressure remains live and intrinsic limitation remains interactive;
- optional prefetch/full-stack residency cannot starve foreground work;
- repeated dense navigation reaches stable memory, pin, queue, and candidate high-water state;
- no white replacement, texture substitute, release flicker, missing-read return, stuck update,
  wait-cursor lock, hover/selection regression, or internal capacity overlay remains;
- all automated correctness, operation-count, transaction, resource, and native performance gates
  pass; and
- the user manually accepts the final native ACTB/BAP1 interaction checkpoint.

