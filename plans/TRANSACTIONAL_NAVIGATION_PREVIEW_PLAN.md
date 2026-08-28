# Transactional navigation-preview plan

Status: implemented; native visual qualification pending

Plan date: 2026-08-28

Depends on:

- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
- [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md)

Implementation result: each panel now owns an explicit committed/preview/checking/ready/committing
presentation transaction. Pan and zoom update a clipped retained GPU canvas immediately while a
50 ms idle / 150 ms maximum-cadence scheduler coalesces exact queries; pointer-driven commits remain
held until release. Current-generation reference, auxiliary-track, and complete-or-refuse alignment
results become interactive together only after the target display has been built and its GPU frame
submitted. Preview geometry has no object identity, inspectors and exports are gated, status text is
debounced, and commits do not fade.

The renderer retains one workspace-sized genomic texture under a 128 MiB hard limit, maps fractional
camera state, samples sub-rectangle texel centres without panel-edge bleed, and paints neutral
zoom-out flanks. A source is admitted only when it has at least one target pixel of overlap and its
captured content revision still exists. Resize, contract change, surface loss, missing capture, and
the hard memory fallback release or invalidate it explicitly. Eligible complete canonical horizons
may add a bounded target-oriented read overlay built from cloned row history; over-budget,
incomplete, TooDense, stale, mismatched, or partially covering horizons keep the texture fallback.
Automation and diagnostics expose transaction state, source/target loci and generations, preview
kind, age, invalidation reason, physical texture size and scale, scheduling and cancellation
counters, transaction duration/frames, stale-result rejection, neutral fallback, texture counters,
and horizon eligibility/build counters.

Automated qualification: `cargo test --workspace --all-targets`, `cargo check --all-targets`,
targeted navigation/horizon/renderer tests, formatting verification, and an optimized desktop build
all pass. Native visual sign-off on the local large RNA BAM is intentionally delegated to the user.

Completion audit amendments: unchanged clamped viewports no longer create duplicate target
generations; independent wheel bursts each receive a fresh idle-coalescing window; an inactive
split panel now settles its own current auxiliary-query failure rather than waiting forever; and a
deterministic zoom-out-during-pan test records Preview → ReadyToCommit → Committing → Settled while
proving the original source remains non-interactive until the submitted commit. A native-window
automation capture is an additional render target in that same submission rather than a replacement
for the presented surface, so capture cannot commit semantic state ahead of the visible window.
Native validation is the sole intentionally outstanding completion gate.

## Decision

RGV will separate the genomic view into two temporal states:

1. **Committed evidence** is the exact, complete-or-refuse result for the current settled viewport.
2. **Navigation preview** is a transient, non-evidentiary image of the last committed genomic canvas,
   transformed to follow the user's current pan or zoom target while its queries are outstanding.

The preview preserves spatial continuity without representing stale pixels as current reads. It has
no read identities, hit targets, inspection, selection, or export semantics. A target viewport
becomes committed only when all required current-generation results and layout state are ready. The
renderer then replaces the preview atomically, without a fade or an intervening blank frame.

Rapid navigation will be coalesced. The viewport and preview continue to respond at input rate, but
RGV will query the latest useful target rather than launch an exact query for every wheel or pointer
microstep. Releasing the gesture dispatches its final target immediately.

The first implementation will retain at most one transition texture for each visible panel. It will
not introduce a general tile cache. Where a complete canonical layout horizon already contains the
target interval, RGV may additionally construct an accurate provisional stack from that horizon,
but that geometry remains non-interactive until the exact foreground query commits.

This plan supersedes the earlier decision to defer snapshot textures in the complete-read plan. It
does not supersede or weaken the complete-or-refuse evidence contract.

## Motivation

The complete-read implementation correctly stops drawing query-local read geometry as soon as its
generation is no longer current. That prevents stale reads from being mistaken for the reads in a
new viewport. It also creates a visible discontinuity:

- zooming in usually requests a smaller population and often settles before a blank frame is
  perceptible;
- zooming out requests a larger superset, and canonical horizon work can also grow;
- while that work is outstanding, the previous read primitives are rejected and the genomic pane
  can flash white; and
- suppressing the short-lived “Checking current viewport for complete reads…” message removes text
  flicker but does not remove the blank canvas beneath it.

The white flash is therefore a temporal presentation problem rather than justification for showing
an incomplete read population. The solution must preserve visual orientation and evidence honesty
at the same time.

## Product contract

### Settled states

A panel has one of these authoritative settled outcomes for its target viewport:

- **Complete**: every eligible foreground read is committed and interactive;
- **Too dense**: no individual reads are committed, the exact eligible count and effective limit
  are reported, and complete aggregate evidence may remain visible;
- **Unavailable** or **Failed**: no individual evidence is exposed; or
- **Overview**: the index-supported overview presentation is current.

All read-oriented product surfaces use the same committed generation. Rendering, CPU and GPU
picking, properties, inspection, selection, automation, MCP, and export must never disagree about
which viewport is authoritative.

### Navigation preview

`NavigationPreview` is an explicitly transient state with:

- a source panel and committed revision;
- the source and target genomic intervals;
- the source and target content rectangles;
- the current affine horizontal transform;
- the target viewport generation;
- the preview age; and
- an invalidation reason when no safe preview can be shown.

Preview pixels are a visual navigation aid, not a population of current reads. While a preview is
active:

- all genomic object IDs in the preview are zero;
- CPU picking and read hit testing are bypassed;
- hover is cleared;
- a retained read selection may remain in application state, but its highlight and current-evidence
  properties are hidden until the target generation settles;
- individual-read inspection, selection, find, copy, and export report that the target evidence is
  still being checked;
- stable-row history is neither rewritten nor learned from the transformed pixels;
- `wait_until_settled` remains false; and
- automation and diagnostics expose that a preview is active, including its source and target
  intervals.

Ordinary evidence exports must refuse while a preview is active. A native UI screenshot may capture
what is visibly on screen, but its automation result must disclose `navigation_preview_active` so a
preview cannot silently become an evidentiary screenshot.

### Visual treatment

The preview must feel like the canvas is following the navigation gesture, not like a loading
overlay:

- it appears immediately when a valid committed texture exists;
- it is transformed directly by the user's gesture and does not run a separate inertial animation;
- the committed target replaces it in a single frame with no fade or crossfade;
- transient hover, cursor, selection, menus, properties, track headers, panel gutters, and the
  genomic ruler are not baked into it;
- live chrome and the ruler continue to describe the target viewport; and
- a compact, fixed-position “Updating view…” status appears only if the preview remains active past
  the existing short-message debounce, so fast transitions add no label flicker.

The status indication must include text or an accessible state and must not depend on colour alone.
No full-pane spinner, opacity pulse, or animated shimmer is introduced.

### Zoom-out exposure

The texture contains only the source interval. Under a zoom-out transform it occupies the correct
inner genomic span; it must not be stretched to imply that its pixels cover newly exposed flanks.
Flanking areas use the panel's neutral unavailable-preview background until the target commits.
They must not flash through the window's white clear colour.

If an exact adjacent preview cache is added later, it may fill those flanks. Such a cache is not part
of this implementation.

## State and generation model

Add a panel-level navigation transaction distinct from the existing committed alignment
projection. A representative model is:

```rust
enum NavigationPresentation {
    Settled { generation: u64 },
    Preview(NavigationPreviewState),
    CheckingWithoutPreview { target_generation: u64 },
}

struct NavigationPreviewState {
    source_generation: u64,
    target_generation: u64,
    source_interval: GenomicInterval,
    target_interval: GenomicInterval,
    source_content_revision: u64,
    layout_revision: u64,
    started_at: Instant,
}
```

The exact type names may follow the existing desktop model, but the separation is mandatory:

- advancing a target viewport starts or updates a navigation transaction;
- it does not relabel the retained source texture as target evidence;
- current-generation engine results populate staging state;
- an old generation may finish but cannot alter staging or committed state;
- query, canonical layout horizon, reference, and required auxiliary track results must pass the
  existing generation gates; and
- the desktop commits the target presentation once, after all required state for that presentation
  is internally consistent.

The renderer must never compose read primitives from one generation with coverage, ruler mapping,
or hit targets from another generation. A rejected or cancelled transaction leaves its target
uncommitted and is superseded by the newest target.

## Gesture and query coalescing

### Input-rate preview

Each horizontal pan or zoom event updates the target interval and the preview transform immediately.
This preserves direct manipulation even when the query layer is slower than the input stream.

### Query policy

Introduce a dedicated navigation-query coalescer rather than reusing the resize-only refresh timer.
Initial policy:

- dispatch after 50 ms of navigation inactivity;
- during a continuous long gesture, dispatch at most once every 150 ms so useful work can begin;
- on pointer release, gesture end, typed-locus submission, or discrete navigation command, dispatch
  the final target immediately;
- never dispatch the same target and query-policy fingerprint twice; and
- cancellation and `GenerationClock` checks remain the final authority for work already in flight.

The timing values are named constants with deterministic clock-driven tests. They may be tuned after
native profiling without changing the product contract.

Coalescing applies to expensive viewport-dependent reference, alignment, canonical horizon, and
auxiliary track queries as a transaction. It must not make the visible locus field lag behind the
gesture, and it must not defer a discrete keyboard, history, search, or direct-locus navigation.

### Long-running work

If an older query is already running when a newer target becomes eligible for dispatch:

- advance the generation immediately;
- cancel or abandon the older result using the existing scheduler mechanism;
- retain the original committed preview texture rather than repeatedly snapshotting intermediate
  previews; and
- query only the newest eligible target.

This prevents a chain of lossy screenshots and ensures that every commit is built from exact source
data rather than from preview pixels.

## Preview transform

For a source interval `S`, target interval `T`, and target content width `W`, an old texture pixel
represents genomic coordinate:

```text
g = S.start + x_old * S.length / W
```

It is drawn at:

```text
x_new = (g - T.start) * W / T.length
```

Equivalently, the full source texture quad has:

```text
scale_x     = S.length / T.length
translate_x = (S.start - T.start) * W / T.length
```

The quad is clipped to the target panel content rectangle. Calculations use the same half-open
interval and pixel-rounding rules as the current locus mapper. Tests must cover zoom anchors,
contig starts and ends, single-base intervals, fractional display scale, and repeated pan/zoom
composition. Each frame computes the transform from the original committed source interval rather
than accumulating floating-point transforms.

Only horizontal genomic navigation is transformed. Vertical scrolling, row-density changes, pane
divider movement, and adaptive panel layout are separate layout revisions and invalidate the
texture.

## Renderer design

### Offscreen genomic-content surface

Add an explicit offscreen render target for the genomic content plane of each visible panel. This
is preferable to reading or copying a platform swapchain texture because surface copy capabilities
vary and because window chrome must not be captured.

The offscreen target includes data-aligned content such as reference bases, annotation glyphs,
coverage, junctions, and reads. It excludes:

- panel and track headers;
- track-label gutters and resize affordances;
- the live genomic ruler and locus controls;
- menus, dialogs, status UI, and properties;
- pointer hover and cursor feedback; and
- selection/inspection overlays.

At a settled commit, the renderer draws this content target and composites it normally into the
window. The same target becomes the immutable source texture for a subsequent horizontal navigation
transaction. During preview, the renderer composites that texture through a clipped textured quad,
then renders current target chrome and the live ruler.

The implementation must preserve all content that currently uses both the rectangle renderer and
the text/UI path. If genomic labels cannot initially be rendered into the offscreen pass without
capturing chrome, omit those labels during preview and keep the underlying glyph/coverage/read
geometry; do not fall back to a whole-window screenshot.

### Lifetime and memory

- Allocate preview textures only for visible panels with non-empty genomic content rectangles.
- Retain no more than one committed source texture per visible panel.
- Because visible panel rectangles partition the workspace, their aggregate native-resolution
  storage should be approximately one genomic framebuffer rather than one full-window texture per
  panel.
- Recreate a texture when its physical pixel dimensions or GPU format change.
- Track allocated preview bytes and enforce a fixed defensive upper bound. If a pathological window
  size exceeds it, downsample the non-evidentiary preview or use the neutral checking background;
  never weaken query completeness to save texture memory.
- Release hidden-panel and invalidated textures promptly, and release all of them on device loss.

Record physical dimensions, scale factor, allocated bytes, capture count, composite count,
invalidation reason, and fallback count in renderer diagnostics.

### Atomic replacement

Do not clear the preview on engine completion alone. Build and validate the complete target display
state first, submit the target offscreen render, and switch the panel's presentation revision only
when the target texture and live semantic state can be exposed together. `TooDense`, `Unavailable`,
and `Failed` are also explicit atomic target presentations.

The renderer performs no fade. The last preview frame is followed directly by the first committed
target frame.

## Canonical-horizon optimization

After the core texture path is correct, add a preferred provisional geometry path when all of the
following are true:

- the retained canonical layout query is Complete;
- its interval fully contains the target interval;
- its source revision, filters, complete-read limit, stacking policy, track settings, and stable-row
  policy fingerprints match the target transaction; and
- every read-bearing track required by the target independently satisfies those conditions.

RGV may then clip and remap the complete horizon records to create an accurate target-oriented
provisional stack without waiting for a new horizon query. This improves organisation during the
transition and can fill more of a zoom-out target than the source raster.

The optimization does not commit evidence early:

- its object IDs remain zero;
- it cannot be selected, inspected, exported, or returned by automation;
- it does not mutate stable-row history;
- foreground coverage and other resolution-dependent aggregates remain preview pixels unless their
  own exact current-generation query has settled; and
- any incomplete, TooDense, mismatched, or partially covering horizon is ineligible.

If the exact provisional stack cannot be built within the frame budget, keep the texture preview
and allow the foreground query to proceed. This optimization must never delay the authoritative
commit.

## Invalidation matrix

A preview may be transformed only when the source and target share the same horizontal genomic
canvas contract. Invalidate it and use the neutral checking presentation for:

- contig or assembly changes where no meaningful affine mapping exists;
- window resize or maximize events that change the content rectangle;
- properties-panel expansion or collapse that changes canvas width;
- panel split, close, reorder, or active-panel layout changes;
- annotation/read-pane divider movement, panel collapse/expand, or adaptive-height changes;
- vertical read scrolling, row-height, density, or stacking-mode changes;
- track add/remove/reorder/visibility or presentation-setting changes;
- read filters, source files, sample, complete-read limit, or query-policy changes;
- device or surface loss; and
- any source revision whose committed texture was not captured successfully.

A pure horizontal pan or zoom within the same contig may reuse the source texture. A locus jump on
the same contig may use it only while the transform retains a useful visible overlap; otherwise the
neutral checking presentation is clearer than a tiny or entirely offscreen old image.

Initial document load has no source texture and therefore uses the current debounced neutral
checking state. This plan removes avoidable white flashes after a committed frame exists; it does
not fabricate a preview before the first result.

## Desktop and external-surface changes

### Desktop

Integrate the state machine primarily through:

- `workspace/navigation_controller.rs` for transaction start and gesture semantics;
- `workspace/queries.rs` for coalescing, dispatch, generation staging, and atomic commit;
- `display/alignment.rs` and the other track display builders for preview-safe display state;
- `app/viewer_runtime.rs` for offscreen texture ownership and frame submission;
- `ui/gestures.rs`, `ui/navigation.rs`, and `ui/status.rs` for gesture boundaries and the debounced
  status indication; and
- hit-test and inspection modules for preview gating.

The exact ownership may be refined during implementation, but query policy must not leak into the
renderer and GPU texture lifetime must not become part of the biological data model.

### Automation and MCP

Extend the workspace/panel snapshot with additive fields such as:

- `navigation_preview_active`;
- `preview_source_interval` and `preview_target_interval`;
- `preview_source_generation` and `preview_target_generation`;
- `preview_age_ms`;
- `preview_kind` (`texture`, `complete_horizon`, or `none`); and
- `preview_invalidation_reason` when applicable.

`wait_until_settled` treats preview, a pending coalesced dispatch, and an in-flight transaction as
unsettled. Read commands continue to require `query_is_current()` and a Complete committed result.
MCP inherits the same behaviour through the automation contract.

Visible-panel raster capture intended for UI testing may explicitly allow preview capture and must
return its preview metadata. Biological evidence export, SVG/data export, read lists, and properties
snapshots must refuse or wait rather than serialize preview pixels as current evidence.

## Diagnostics

Add counters and timings sufficient to evaluate both smoothness and correctness:

- navigation input updates;
- coalesced, dispatched, cancelled, and committed target generations;
- gesture-end immediate dispatches;
- time from first navigation input to final commit;
- preview time and frames per transaction;
- texture captures, composites, invalidations, fallbacks, and bytes;
- exact-horizon preview eligibility, hits, misses, and build time;
- frames that used the neutral checking background despite a prior committed view; and
- stale-result rejections.

These diagnostics are observational. They do not automatically change the Complete read limit,
texture budget, query horizon, or coalescing timings.

## Implementation phases

### Phase 0 — Baseline and contract tests

1. Add a deterministic trace for the current zoomed-in-to-zoomed-out pan sequence.
2. Record query generations, dispatch frequency, frame presentation states, and the current blank
   interval.
3. Add state tests proving that stale alignment geometry is already rejected; preserve them as a
   non-regression boundary.
4. Define the preview and atomic-commit types before renderer work begins.

Exit criterion: the white-frame failure and the truthfulness boundary are reproducible without
depending only on manual screenshots.

### Phase 1 — Navigation transaction and coalescer

1. Introduce per-panel target/committed presentation revisions.
2. Add the navigation-query coalescer with injectable time.
3. Wire gesture start/update/end and discrete navigation events.
4. Stage current-generation results and commit them only as a coherent target state.
5. Gate interaction and exports while the target is uncommitted.

Exit criterion: rapid input issues bounded queries for the latest target, and no staged result is
visible as committed evidence.

### Phase 2 — Offscreen content target and texture preview

1. Split genomic content from live chrome and transient overlays.
2. Render settled genomic content to the panel offscreen target.
3. Add the clipped affine preview compositor and neutral exposed-band background.
4. Implement texture lifetime, memory accounting, device-loss handling, and invalidation.
5. Switch Complete and refusal/error states atomically with no fade.

Exit criterion: a valid previous commit remains visually continuous through a slow zoom-out query,
with no white full-pane frame and no interactive stale reads.

### Phase 3 — Product-surface integration

1. Finalize debounced status and accessibility semantics.
2. Hide stale hover/selection/properties state during preview.
3. Extend automation snapshots, settled checks, screenshot metadata, diagnostics, and MCP behaviour.
4. Cover multi-panel and mixed-track transactions.

Exit criterion: every product surface consistently distinguishes preview pixels from committed
evidence.

### Phase 4 — Complete-horizon provisional geometry

1. Add the strict horizon-eligibility fingerprint and containment test.
2. Build non-interactive target-oriented provisional rows from a complete eligible horizon.
3. Prefer the geometry preview when it meets the frame budget; retain texture fallback.
4. Prove that stable-row history and export state are unaffected.

Exit criterion: eligible targets receive a more accurate interim stack without changing the final
query result or its evidence semantics.

### Phase 5 — Native qualification

1. Run unit, integration, automation, renderer, and release-build validation.
2. Exercise the large RNA BAM through repeated pan, zoom-in, zoom-out, and mixed zoom/pan gestures.
3. Capture frame-by-frame evidence for fast and deliberately delayed queries.
4. Test resize/maximize, properties expansion, pane collapse/expand, panel splits, and device-scale
   changes.
5. Compare diagnostics before and after to confirm lower query churn and bounded texture memory.

Exit criterion: native testing shows continuous navigation, atomic truth-preserving commits, and no
regression in panel layout or complete-read enforcement.

## Test matrix

### State and correctness

- Settled Complete → preview → settled Complete uses only source pixels during preview and only
  target objects after commit.
- Settled Complete → preview → TooDense removes the preview and atomically shows refusal plus only
  complete aggregate evidence.
- Failed, unavailable, cancelled, and superseded targets never expose partial geometry.
- No preview read has a GPU or CPU hit target.
- Inspection, selection, find, properties, automation, MCP, and evidence export refuse until the
  target is committed.
- Native UI capture during preview reports that state.

### Gesture scheduling

- A burst of navigation updates shorter than 50 ms results in one final query.
- A continuous gesture uses the bounded 150 ms cadence and then immediately dispatches the released
  target.
- Keyboard/history/search/direct-locus navigation dispatches immediately.
- Duplicate targets do not query twice.
- An old completion after a new generation is ignored.

### Transform and rendering

- Pan, centered zoom, cursor-anchored zoom, contig-edge clamping, and repeated mixed gestures map
  source pixels correctly.
- Zoom-out displays neutral uncovered flanks rather than stretched data or the white clear colour.
- The target ruler remains live and is never duplicated from the source texture.
- There is no fade, opacity pulse, full-pane spinner, or blank frame between a valid preview and its
  committed target.
- Text rendering limitations do not cause whole-window or properties content to enter the texture.

### Layout and resources

- Maximize/restore, resize, display-scale change, properties expansion, pane-divider movement,
  collapse/expand, and adaptive layout invalidate safely.
- Split and multi-panel layouts allocate only visible panel textures.
- Aggregate preview texture bytes remain bounded near one genomic framebuffer and obey the hard
  defensive limit.
- Surface/device loss falls back cleanly and later recovers.

### Horizon optimization

- Complete containing horizons with identical fingerprints are eligible.
- Incomplete, TooDense, stale, filter-mismatched, policy-mismatched, and partially containing
  horizons are rejected.
- Provisional rows have no semantic IDs and do not alter stable-row history.
- Missing or slow provisional geometry never delays the foreground commit.

## Non-goals

This plan does not include:

- sampled or progressively revealed individual reads;
- treating stale raster pixels as current data;
- a persistent map-style tile pyramid or cross-session image cache;
- automatic hardware-dependent Complete read limits;
- speculative prefetch across arbitrary loci;
- fading or morphing between different read stacks;
- retaining preview selection or hover interactions;
- solving all BAM/CRAM query latency; or
- using preview textures in biological data export.

## Completion definition

This plan is complete when:

- horizontal pan and zoom retain a truthful visual preview until the target settles whenever a valid
  prior commit exists;
- rapid gestures query coalesced latest targets and the released target immediately;
- exact current-generation content replaces the preview atomically with no fade or white full-pane
  flash;
- preview pixels cannot be picked, inspected, selected, exported, or reported as current reads;
- Complete, TooDense, Failed, Unavailable, and Overview outcomes preserve the complete-or-refuse
  contract;
- resize and adaptive-layout transitions invalidate safely;
- texture memory and scheduling behaviour are observable and bounded;
- the complete-horizon provisional path is correct where eligible and harmless where unavailable;
  and
- automated and native large-RNA-BAM qualification covers the original zoom-out-during-pan failure.
