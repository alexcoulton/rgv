# Sashimi arc interaction plan

Status: implemented — native sign-off pending

Plan date: 2026-08-28

Depends on:

- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)

## Decision

RGV will make the geometry of every rendered Sashimi arc a first-class interaction target.
Hovering an arc will highlight the exact visible component under the pointer, and clicking it will
persistently select its biological splice junction. Hit testing will use screen-space distance to
the rendered curve; support depth will never determine which visually distinct curve receives the
interaction.

Hover and selection will be separate states:

- hover is transient, always available on an interactive committed canvas, and never silently
  replaces a click selection;
- click selection is persistent and continues to drive the Properties panel, retained evidence,
  automation, and export semantics;
- the existing inspector activation preference decides whether a hover also previews Properties,
  but it does not decide whether the arc itself has hover feedback; and
- a drag that begins over an arc remains a navigation drag unless it completes within the existing
  click-motion threshold.

The layout will emit semantic arc targets only for components it actually renders. Panel
composition will transform, clip, and scroll those targets in lockstep with their drawing
primitives, assign frame-local interaction IDs, and publish a committed per-panel hit index. The
picker will not search the raw junction query.

The semantic selection remains the biological junction identified by track, start, and end. A
presentation-only strand component records which positive, negative, or unknown line received the
hover or click, so the exact line can remain highlighted while the inspector describes the complete
junction and its strand-support breakdown.

This design deliberately avoids a GPU colour-picking framebuffer. The number of rendered Sashimi
components is already bounded, and deterministic CPU geometry testing gives immediate results
without an asynchronous GPU readback or a second source of curve geometry.

## Observed failure

The current Sashimi renderer and picker use different models of the same data.

The renderer in `crates/rgv-layout/src/alignment_layout/coverage.rs` emits separate upward and
downward parabolic strokes with different heights, rows, colours, and endpoints. The picker in
`apps/rgv-desktop/src/inspection/selection.rs` uses the pointer's vertical coordinate only to decide
whether it is inside the Sashimi lane. It then converts the horizontal coordinate to one genomic
position and delegates to `junction_at_position` in
`apps/rgv-desktop/src/display/hit_test.rs`.

`junction_at_position` selects all junctions spanning that genomic position and resolves them by:

1. greatest support depth; then
2. narrowest genomic span.

It does not measure the pointer against a curve. Consequently, a high-support junction owns most of
its horizontal interval even when the pointer is visibly on another line. The same rule is used by
hover inspection, so the dominant line also suppresses feedback for its neighbours. The picker can
additionally consider junctions that were omitted by `MAX_RENDERED_JUNCTIONS`, because it searches
the query rather than the rendered set.

The repeatable native fixture is:

- source: `benchmark-data/large-public-alignments/ENCFF686MTA.bam`;
- locus: `chr1:36364083-36380134` as entered through the current one-based UI;
- collapsed Sashimi presentation;
- approximately twelve visible junctions, including several shallow overlapping arcs; and
- the highest-support arc currently captures selections intended for visibly separate lines.

## Product contract

### Select what is drawn

An arc is interactive if and only if that exact component is present in the committed visible
display.

- A component removed by the minimum-depth filter has no target.
- A component omitted by the hard rendered-junction cap has no target.
- A component outside the viewport or clipped out by its pane has no target.
- A hidden or collapsed Sashimi lane has no target.
- A stale layout, provisional navigation overlay, or retained preview texture has no target.
- Every visible positive, negative, and unknown component has its own target even when multiple
  components belong to one junction.

The interaction geometry must be derived at the point where the drawing primitive is emitted. Code
must not independently recalculate its row, height, scale, or curve from the query in the desktop
picker.

### Hover

When the pointer enters the acquisition area of an arc:

- the exact nearest component highlights within the next presented frame;
- the pointer changes to a clickable cursor;
- the highlight preserves the component's strand colour;
- the stroke becomes visibly stronger or wider, so feedback does not depend on colour alone;
- the current click selection remains intact; and
- if inspector activation is set to hover, Properties temporarily describes the hovered junction
  and returns to the persistent selection when the pointer leaves.

Leaving the Sashimi lane, entering obscuring UI, starting a drag, changing panel, invalidating the
layout, or beginning a navigation preview clears the transient hover immediately.

Hover must not start a query, rebuild alignment rows, rebuild the Sashimi layout, mutate selection
history, or change retained evidence.

### Click selection

A click completed over an acquired arc selects:

- the owning alignment track;
- the junction's zero-based half-open start and end; and
- the focused strand component for presentation.

The Properties panel continues to report the whole biological junction, including total depth,
strand support, flanks, annotation status, provenance, and current Sashimi units. It additionally
identifies the focused component and its support when a component-specific click supplied one.

Selection remains endpoint based for sessions, retained evidence, automation, and external
contracts. The focused component is transient presentation state unless a later product decision
explicitly promotes it into the public selection schema.

Click selection must not depend on whether labels are shown. Clicking an emitted count label or
direction marker may resolve to the same junction target, but those affordances must never make the
underlying line harder to acquire.

### Ambiguity and ordering

Candidate resolution is geometric:

1. smallest screen-space distance to the visible stroke;
2. the currently hovered target while it remains within a small exit radius, providing hysteresis;
3. topmost draw order when distances are equal within a narrow epsilon; then
4. a stable semantic key consisting of track, start, end, and component.

Support depth is not a hit-test tie-breaker.

At the exact mathematical crossing of two curves, one pixel can be inherently ambiguous. Moving
along either branch must immediately resolve to that branch. Distinct evidence with completely
coincident geometry is not visually distinguishable and is outside the initial direct-manipulation
contract; current junction aggregation normally collapses identical endpoints into one biological
junction with separate strand components.

### Acquisition radius

Hit testing operates in logical screen points so behaviour is stable across display scale factors.
The target is the visible stroke plus a bounded invisible acquisition margin:

```text
acquisition half-width = max(rendered stroke half-width + 3 points, 4 points)
exit half-width        = acquisition half-width + 2 points
```

The exact constants should be adjusted once against the native fixture, but they must remain
independent of genomic scale and junction depth. Thin, low-support lines therefore remain as easy
to acquire as thick, high-support lines without visually inflating every arc.

The distance test must include bounded endpoint distance rather than extending the parabola beyond
the displayed endpoints.

### Navigation and evidence truth

Sashimi interaction follows the transactional navigation contract.

- Only a `Settled` committed panel publishes arc targets.
- `Preview`, `CheckingWithoutPreview`, `ReadyToCommit`, and `Committing` expose none.
- A commit publishes the new display and its hit index atomically.
- Pointer movement during a preview cannot inspect the source texture or provisional geometry.
- Stale targets carry a content revision and are rejected if that revision is not the panel's
  committed revision.
- Native screenshots may contain a hover highlight, but semantic exports do not bake transient
  hover state into evidence.

### Multiple tracks and panels

Every target is qualified by panel and alignment-track identity. Targets from secondary alignment
tracks must be selectable even though their read object IDs are currently cleared during workspace
composition. Equal junction coordinates in different tracks or panels are distinct targets and
must open the correct inspector evidence.

Only the active, unobstructed panel can acquire a target. Pane headers, scrollbars, settings windows,
dialogs, and overlays continue to block canvas interaction.

## Geometry authority

### Typed semantic target

Add an internal layout type resembling:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JunctionArcComponent {
    Positive,
    Negative,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JunctionArcTarget {
    pub junction_index: usize,
    pub start: u64,
    pub end: u64,
    pub component: JunctionArcComponent,
    pub primitive: RectPrimitive,
    pub stroke_width: f32,
    pub draw_order: u32,
}
```

The actual type names may follow existing layout vocabulary. The required invariant is that
`primitive` is the same geometry appended to the display list, not a reconstruction.

`AlignmentLayout` gains a bounded vector of these targets. `add_junctions` appends a target at the
same time it appends each non-zero component arc. It does not create targets for baseline guides,
annotation markers, suppressed labels, or hidden candidates.

The maximum target count is bounded by three components for each of
`MAX_RENDERED_JUNCTIONS`. Memory accounting for the layout cache must include the target vector.

### Shared curve mathematics

Move the CPU curve calculation into a shared `rgv-layout` helper used by:

- hit testing;
- CPU PNG rasterization;
- renderer tests; and
- any future semantic export validation.

The WGSL shader keeps its equivalent expression, with parity tests covering representative upward
and downward arcs. The helper returns the curve position, local slope, stroke distance, and bounded
endpoint distance for a logical point. There must be one declared source for stroke width via
`junction_arc_stroke_width`.

The current parabolic form is retained:

```text
fraction = clamp(local_x / width, 0, 1)
parabola = 4 * (fraction - 0.5)^2
```

Upward and downward arcs mirror around their baseline. The hit metric uses the normalised distance
already represented by the renderer rather than raw vertical distance.

### Composed panel hit index

The desktop converts layout targets into a committed panel-local index resembling:

```rust
struct ComposedJunctionArcTarget {
    panel_id: PanelId,
    track_id: TrackId,
    key: JunctionArcKey,
    primitive: RectPrimitive,
    stroke_width: f32,
    draw_order: u32,
    interaction_id: u32,
    content_revision: u64,
}
```

Composition applies exactly the same operations as the display primitive:

- annotation-pane vertical scrolling;
- track and subtrack placement;
- panel horizontal translation;
- pane and panel clipping; and
- visibility and navigation-state gating.

A helper should transform a display primitive and its semantic target together so later changes
cannot update one without the other. A target whose arc primitive is removed by clipping is removed
from the hit index as well.

The active panel stores the committed index beside its display list. A scroll-only refresh updates
the transformed index without relaying out the evidence. Layout, filter, track, pane, panel,
navigation, surface-contract, and content-revision invalidations clear it through the same paths
that invalidate the display.

### Bounded hit-test cost

The straightforward implementation scans the current Sashimi lane's bounded target vector, rejects
expanded bounding boxes first, and evaluates curve distance only for survivors. It must allocate
nothing on pointer movement.

The hard upper bound is three times `MAX_RENDERED_JUNCTIONS`, and only the active track lane is
searched. Do not introduce a spatial tree before profiling shows it is necessary. If the native
maximum-density benchmark exceeds the performance gate, add a deterministic lane-local interval or
screen-bin index without changing selection semantics.

## Interaction state

Add explicit transient state rather than overloading read hover or persistent junction selection:

```rust
struct JunctionArcKey {
    track_id: TrackId,
    start: u64,
    end: u64,
    component: JunctionArcComponent,
}

struct JunctionArcHover {
    key: JunctionArcKey,
    interaction_id: u32,
    distance_points: f32,
    content_revision: u64,
}
```

The panel owns `hovered_junction_arc: Option<JunctionArcHover>` and a presentation-only
`selected_junction_component`. The existing `selected_junction` endpoint identity remains the
semantic authority during the first implementation.

Cursor movement always updates the hover target when the committed canvas is unobstructed. It marks
only the renderer interaction state dirty; it does not mark the alignment layout cache dirty.

Inspector resolution becomes:

- hover target when inspector activation is `Hover` and the hover revision is current;
- otherwise the persistent selected junction; and
- otherwise the existing empty-state guidance.

Click selection copies the hover target's semantic identity rather than rerunning a separate hit
test. If the pointer moved since the last hover evaluation, the release handler performs one current
geometry test before committing the selection.

## Visual feedback and renderer integration

### Styling

The base strand palette remains unchanged. Interaction styling is composited in the native
rectangle shader so hover does not rebuild or replace layout primitives.

- **Hover:** increase opacity, mix only slightly toward the interaction accent, and expand the
  effective stroke by approximately 1.5 logical points.
- **Selected component:** retain the strand colour, use a stronger accent boundary, and expand the
  stroke by approximately 2 logical points.
- **Selected junction without a component:** apply the selected style to every rendered component
  sharing the endpoint identity.
- **Hover over selected:** selected styling remains primary, with a small additional brightness
  change rather than a second unrelated colour.

The exact palette should use existing theme colours and must remain legible on the white genomic
canvas. Selection must no longer replace all junction components with black.

### Interaction IDs

Assign non-zero interaction IDs to composed Sashimi components for the current frame. IDs are
rendering handles only; semantic selection uses the typed key. They may be regenerated after display
composition as long as hover and selected handles are updated atomically with the display.

The renderer must distinguish read IDs from Sashimi interaction IDs by primitive shape and separate
uniform fields. A numeric collision must not highlight a read and an arc together. Up to three
selected component IDs may be supplied for endpoint-only selection; hover supplies one component
ID.

The arc fragment path widens its distance threshold for matching hover and selection IDs before
discarding pixels. Rectangles, read bodies, detail marks, chevrons, breakend brackets, exports, and
the neutral navigation-preview pass retain their existing behaviour.

### Cursor and redraw

An acquired target requests `PointingHand`; leaving it restores the normal genomic navigation
cursor. Pointer motion requests a redraw only when the acquired semantic target changes or its
highlight is cleared. Moving along one arc must not upload a new layout or repeatedly dirty the
display list.

## Implementation phases

### Phase 0 — Reproducer and failing contracts

1. Preserve the large RNA-BAM locus above as the native qualification fixture.
2. Add a deterministic synthetic layout with a deep broad arc and several shallow arcs that share
   horizontal spans but diverge vertically.
3. Add a failing contract proving that pointer Y selects the nearest visible curve rather than the
   deepest spanning junction.
4. Add a failing contract proving that a capped-out junction cannot be selected.
5. Record current pointer-move prepare/layout counters so hover work can be performance checked.

Exit criterion: tests reproduce the dominance bug without relying on screenshots.

### Phase 1 — Semantic arc geometry

1. Add the component and target types to `rgv-layout`.
2. Emit one target with each rendered component arc.
3. Extend `AlignmentLayout` and cache-weight accounting.
4. Factor shared curve distance and endpoint calculations.
5. Prove target order, identity, geometry, and cap/filter parity with layout tests.

Exit criterion: every and only rendered arc component has matching semantic geometry.

### Phase 2 — Panel composition and geometry picker

1. Carry targets through primary and secondary alignment-track composition.
2. Apply scrolling, translation, clipping, panel identity, track identity, interaction IDs, and
   content revision in lockstep with display primitives.
3. Store the committed hit index on the panel.
4. Replace `junction_at_position` in pointer interaction with nearest-curve selection.
5. Add hysteresis and stable tie resolution.
6. Retain the old span helper only for non-pointer semantic lookups if another caller needs it;
   otherwise remove it and its prominence-based test.

Exit criterion: all visible components are individually addressable in collapsed and expanded
layouts, and hidden components cannot be addressed.

### Phase 3 — Hover and click semantics

1. Add explicit per-panel arc-hover and component-focus state.
2. Update hover on unobstructed pointer movement independently of inspector activation mode.
3. Make hover-mode Properties consume transient state without overwriting persistent selection.
4. Make click release select the acquired semantic key while preserving drag-to-pan.
5. Clear hover through panel, navigation, source, track, lane, filter, and revision invalidations.
6. Extend the junction inspector with focused-component support when available.

Exit criterion: hover is transient and immediate, click is persistent, and the two cannot corrupt
one another.

### Phase 4 — Native visual treatment

1. Assign safe frame-local Sashimi interaction IDs.
2. Extend renderer uniforms and WGSL arc styling for hover and selection.
3. Preserve original strand colours and remove the current all-black selected-arc treatment.
4. Apply the pointing-hand cursor only while a valid target is acquired.
5. Ensure neutral/provisional navigation passes and semantic exports omit interaction styling.
6. Add renderer tests for shape/ID isolation, stroke expansion, and selected/hover precedence.

Exit criterion: visual feedback is obvious without colour alone and introduces no layout churn.

### Phase 5 — Integration and qualification

1. Run focused layout, hit-test, interaction, inspection, renderer, export, and navigation tests.
2. Run `cargo fmt --all -- --check` and `git diff --check`.
3. Run `cargo test --workspace --all-targets` with the required loopback permissions.
4. Build the optimized desktop binary.
5. Qualify the native large RNA-BAM fixture in collapsed and expanded modes, at multiple zooms,
   with Properties configured for both click and hover activation.
6. Document the interaction in the user guide and update this plan with the implementation result.

Exit criterion: automated gates pass and every visually distinct fixture arc can be highlighted and
selected by following its visible path.

## Test matrix

### Layout and target parity

- one target per non-zero positive, negative, and unknown component;
- no target for zero-depth components;
- no target below the minimum-depth filter;
- no target for a junction omitted by `MAX_RENDERED_JUNCTIONS`;
- target identity survives collapsed and expanded row assignment;
- fixed, automatic, shared, raw, and CPM scaling produce matching draw and target geometry;
- labels on/off do not change line acquisition;
- cache weight includes target storage; and
- target ordering is deterministic across identical input.

### Curve distance

- points on upward and downward curves return zero or subpixel distance;
- points inside and outside the acquisition radius classify correctly;
- endpoint distance is bounded and does not extend the curve;
- single-pixel-width and maximum-width arcs have equal minimum acquisition comfort;
- zero, tiny, and very wide rectangles are safe and finite;
- logical coordinates behave identically at 1x and 2x physical scale; and
- CPU raster coverage and hit geometry agree at representative pixels.

### Candidate resolution

- a shallow curve wins over a deep curve when it is closer to the pointer;
- pointer Y distinguishes arcs sharing the same X span;
- support depth never resolves equal-distance candidates;
- draw order and semantic key resolve deterministic ties;
- hover hysteresis prevents flicker near a close approach;
- leaving the exit radius transfers acquisition promptly;
- upward and downward components sharing endpoints remain individually hoverable; and
- exact intersections resolve to either branch immediately away from the crossing.

### Desktop interaction

- hover feedback works in click-inspector mode without changing selection;
- hover-inspector mode previews and restores persistent selection on exit;
- click selects the acquired track, junction, and focused component;
- pointer exit, overlay entry, drag start, track hide, lane collapse, panel switch, and query
  invalidation clear hover;
- dragging an arc pans instead of selecting when the motion threshold is exceeded;
- secondary alignment tracks resolve their own queries and inspectors;
- equal coordinates in two panels do not cross-select;
- annotation-pane scrolling keeps geometry and targets aligned; and
- pointer movement within one target does not rebuild layout or allocate.

### Renderer and presentation

- only the hovered component receives hover styling;
- component selection preserves its strand colour;
- endpoint-only selection styles every component of that junction;
- a read object ID numerically equal to an arc ID remains unaffected;
- hover plus selection follows declared precedence;
- native window and native-window capture render the same highlight;
- PNG/SVG Sashimi evidence exports omit transient hover styling; and
- navigation preview and neutral fallback expose no interactive highlight.

### Regression

- junction Properties still report complete evidence and provenance;
- automation junction selection remains endpoint based and backwards compatible;
- annotation status and direction markers are unchanged;
- count-label collision suppression is unchanged;
- Sashimi render and label caps remain hard;
- read, coverage, modification, feature, variant, quantitative, and interaction picking are
  unaffected; and
- full workspace tests remain green.

## Performance and resource gates

- Pointer movement performs no heap allocation in the steady state.
- Moving inside one acquired target does not rebuild the display or upload unchanged primitives.
- A changed hover target is visible by the next presented frame.
- Nearest-curve hit testing remains below 0.5 ms at the hard rendered-junction cap on the supported
  development machine; record both median and p95.
- Target storage remains bounded by three components per rendered junction and is included in cache
  admission.
- Renderer interaction styling adds no extra full-canvas render target and no GPU readback.

## Diagnostics

Extend developer diagnostics with bounded counters rather than pointer-coordinate logs:

- rendered Sashimi component targets;
- pointer candidates after bounding-box rejection;
- detailed curve-distance evaluations;
- target acquisitions, transfers, and clears;
- hysteresis retentions;
- stale-revision rejections;
- hover-driven redraws;
- accidental hover-driven layout rebuilds, expected to remain zero; and
- hit-test median/p95/max duration when diagnostics are enabled.

Do not log read names, remote credentials, raw query URLs, or unbounded pointer traces.

## Likely code areas

- `crates/rgv-layout/src/alignment_layout.rs`
- `crates/rgv-layout/src/alignment_layout/coverage.rs`
- `crates/rgv-layout/src/display.rs`
- `crates/rgv-layout/src/alignment_layout_tests.rs`
- `crates/rgv-render/src/lib.rs`
- `crates/rgv-render/src/rectangle.wgsl`
- `apps/rgv-desktop/src/display/alignment.rs`
- `apps/rgv-desktop/src/display/hit_test.rs`
- `apps/rgv-desktop/src/display/mod.rs`
- `apps/rgv-desktop/src/inspection/selection.rs`
- `apps/rgv-desktop/src/inspection/inspector_controller.rs`
- `apps/rgv-desktop/src/ui/interaction.rs`
- `apps/rgv-desktop/src/workspace/panels.rs`
- `apps/rgv-desktop/src/tests/display.rs`
- `apps/rgv-desktop/src/tests/inspection.rs`
- `apps/rgv-desktop/src/tests/workspace.rs`
- `docs/USER_GUIDE.md`

## Risks and mitigations

### Hit geometry drifts from rendering

Mitigation: emit the target beside the primitive and share CPU curve math. Transform and clip the
pair through one composition helper. Test representative points against CPU raster coverage.

### Hover causes expensive relayout

Mitigation: keep interaction state out of `AlignmentLayoutOptions` and layout-cache keys. Style
existing primitives through frame-local renderer IDs and update only renderer uniforms when the
semantic target changes.

### Thin overlapping arcs flicker

Mitigation: use nearest normalised curve distance, a larger exit radius than acquisition radius,
and stable current-target retention. Do not use support depth.

### Expanded rows or scrolling misalign targets

Mitigation: carry the emitted primitive through the exact display composition transform and add
scroll/clip integration tests. Do not independently reconstruct Y positions in the picker.

### Interaction IDs collide with read IDs

Mitigation: gate shader matching by primitive shape and provide separate read and Sashimi uniforms.
Keep semantic identity out of numeric renderer handles.

### Hover-mode Properties destroys click selection

Mitigation: make transient inspector input explicit and restore the persistent selection when hover
clears. Never implement hover by calling the ordinary click-selection reducer.

### Wider highlighting obscures neighbouring evidence

Mitigation: widen only the acquired component, keep the expansion bounded, preserve strand colour,
and render the original curve ordering. Validate the dense native locus before finalising constants.

## Non-goals

- Changing junction discovery, flank filtering, aggregation, depth calculation, CPM normalisation,
  scale modes, arc-height encoding, row packing, or render caps.
- Making unrendered or filtered junctions selectable.
- Replacing direct CPU interaction with a general GPU picking buffer.
- Adding a general tooltip framework or redesigning the Properties panel.
- Persisting a strand component in the public session or automation schema during the first
  implementation.
- Solving direct selection of two mathematically coincident but semantically different curves that
  have no visible distinguishing geometry.
- Changing read-stack selection, hover, or transactional navigation semantics.

## Implementation result

Implemented on 2026-08-28.

- `rgv-layout` now emits a typed `JunctionArcTarget` beside every rendered positive, negative, and
  unknown component. The target holds the exact emitted primitive, semantic endpoint identity,
  component, stroke width, and draw order. Filtered, zero-depth, and capped-out evidence produces
  no target, and layout-cache admission includes target storage.
- The shared curve sampler in `rgv-layout` is used by desktop picking and CPU rasterization. It
  exposes curve Y, local slope, normalised stroke distance, and bounded endpoint distance while the
  WGSL shader retains the parity-tested equivalent expression.
- Desktop composition assigns frame-local arc tokens, carries them through the same panel, pane,
  scrolling, clipping, and translation operations as the display primitives, and reconstructs a
  committed panel-local hit index from the final rendered geometry. Non-interactive navigation
  states publish no targets and content revisions reject stale targets.
- Pointer picking now uses nearest screen-space curve distance, a four-point minimum acquisition
  radius, deterministic draw-order/semantic tie resolution, and a two-point hysteresis exit
  margin. The old support-depth span picker and prominence-based pointer test were removed.
- Hover and click selection are separate panel states. Hover updates only after the semantic target
  changes, uses a pointing-hand cursor, clears through navigation/panel/focus/layout invalidations,
  and never mutates the persistent junction selection. Click retains endpoint identity plus a
  presentation-only focused component, which the junction inspector reports with its support.
- Hover and selection are composited as widened arc-only renderer shapes. Strand colour remains
  visible; selected arcs are no longer replaced with black. Endpoint-only selections style every
  rendered component, while component selections and hover style only the exact line.
- Transient hover overlays are retained in native-window capture but removed from semantic PNG and
  SVG exports. The user guide documents acquisition, hover/click separation, drag behaviour, and
  rendered-only interaction truth.
- Developer diagnostics expose bounded target, candidate, curve-evaluation, acquisition, transfer,
  clear, hysteresis, stale-revision, redraw, layout-rebuild, and timing counters. Timing retains at
  most 128 samples and is collected only while Diagnostics is open.
- Automated coverage now includes layout/target parity and caps, shared curve geometry and bounded
  endpoints, nearest-curve overlap resolution, strand-component acquisition, deterministic ties,
  hysteresis transfer, track/revision rejection, minimum acquisition comfort, overlay precedence,
  component inspector text, renderer stroke expansion, and transient-export exclusion.

Qualification completed:

- `cargo fmt --all -- --check`;
- `git diff --check`;
- `cargo check -p rgv-desktop --all-targets`;
- `cargo test --workspace --all-targets`; and
- `cargo build --release -p rgv-desktop`.

The release-only hard-cap benchmark scans 12,288 coincident component targets (three times
`MAX_RENDERED_JUNCTIONS`) without a spatial index. On the supported Apple M4 development machine it
recorded 0.115 ms median, 0.158 ms p95, and 0.274 ms maximum across 201 samples, passing the 0.5 ms
p95 gate. Run it with:

```text
cargo test --release -p rgv-desktop --lib \
  maximum_density_junction_hit_test_stays_below_half_a_millisecond_at_p95 -- --ignored --nocapture
```

Remaining sign-off: native qualification of the large RNA-BAM fixture at
`chr1:36364083-36380134`, including collapsed/expanded presentation, click/hover Properties modes,
multiple zooms, and the Diagnostics hit-test timing gate.

## Completion definition

This plan is complete only when all of the following are true:

1. Every rendered Sashimi component has semantic geometry emitted from the same layout operation as
   its drawing primitive.
2. The desktop hit index contains every and only visible committed targets, qualified by panel,
   track, component, and content revision.
3. Pointer selection is based on nearest screen-space curve distance and never on support depth.
4. Hover provides immediate component-specific feedback without mutating persistent selection or
   rebuilding layout.
5. Clicking a component selects the correct biological junction and drives the correct Properties
   evidence.
6. Collapsed, expanded, scrolled, multi-track, multi-panel, filtered, capped, and navigation-preview
   states obey the product contract.
7. Strand colour remains visible under hover and selection, and feedback does not depend on colour
   alone.
8. Automated layout, geometry, interaction, renderer, export, navigation, and regression tests pass.
9. Formatting, diff integrity, full workspace tests, and the optimized desktop build pass.
10. Native qualification at `chr1:36364083-36380134` confirms that every visually distinct arc can
    be followed, highlighted, and selected without the prominent junction stealing the pointer.
