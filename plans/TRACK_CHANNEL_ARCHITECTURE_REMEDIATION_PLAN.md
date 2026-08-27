# Track-channel architecture audit and remediation plan

Status: implemented and qualified

Audit date: 2026-08-26

Audited revision: `34075ce` (`Improve multi-track rendering and pane-scoped controls`)

Implementation date: 2026-08-26

## Purpose

This document audits the implementation that presents one alignment source through two independent
UI channels:

- the upper annotation channel, containing reference, coverage, Sashimi, modification summaries,
  gene models, and other annotations; and
- the lower read-data channel, containing one independently scrollable read panel per alignment
  track.

It covers the latest read-group visibility failure, predicts related failures that follow from the
same design, and proposes a staged replacement architecture. The objective is not merely to keep a
hidden group row on screen. The objective is to make grouping, visibility, minimization, sizing,
selection, scrolling, persistence, and automation operate on one explicit and testable presentation
model.

## Executive conclusion

The latest failure is real, but the group is not actually being made visible by the child eye. The
sidebar row is being re-laid out from the bottom of the read pane to its top. That movement makes a
still-hidden group appear to have been resurrected.

The immediate cause is the proportional read-panel allocator. A group header is represented as a
20-point “panel with no reads.” When its five member panels are minimized, the one remaining expanded
read panel receives all remaining height and pushes the group header to the bottom edge. When the
remaining read panel is hidden, it also becomes a 20-point row, so both rows are packed at the top.
There is also a direct 20-point allocation versus 22-point header mismatch.

This is one symptom of a broader architecture problem:

1. Track source state, annotation presentation state, read presentation state, group state, and
   query demand are stored together or inferred from one another.
2. Groups are not first-class objects. Group ID and minimization are copied into every member track.
3. Layout is used as the track tree. Rows that are absent from a layout cease to be selectable,
   observable, or sometimes restorable from that channel.
4. The first alignment track owns the height of the entire read pane.
5. UI actions mix direct model mutations with mutations to temporary five-element tuples and apply
   them in several later passes.
6. Pane identity is inferred from geometry (`frozen_bottom.is_some()`) rather than carried by the
   node identity.
7. The MCP workspace snapshot does not expose pane groups, per-pane heights, or per-panel read
   scroll state, so these failures cannot be asserted structurally.

A local coordinate patch would leave the state-loss, identity, selection, reorder, resize, and
persistence bugs intact. The recommended fix is an explicit pane presentation tree, a pure layout
stage shared by canvas and gutter chrome, and a typed reducer that applies all track intents once per
frame.

## Implementation outcome

The remediation has been implemented in the working tree based on `34075ce`. The implementation is
an architectural replacement rather than a coordinate-only repair:

| Area | Implemented result |
|---|---|
| Presentation identity | `TrackChannel` and pane-qualified `PresentationNodeId` values are canonical for tracks, groups, selection, gestures, drag/drop, and automation. Geometry no longer decides channel identity. |
| Presentation ownership | Session schema v4 stores `WorkspacePresentation`, ordered pane nodes, first-class groups, composite parent gates, a workspace-owned read-pane height, and pane-local preferred heights. Legacy mirrored fields are accepted only as compatibility input and are omitted from new serialization. |
| Group semantics | Group chevrons and eyes are independent parent gates. Member eye states survive group hide/show and collapse/expand. IDs use a monotonic allocator, grouped members cannot be moved implicitly, and annotation/read groups are independent. |
| Composite semantics | Overlay parents have a persisted visibility gate distinct from member eyes. Hiding or showing the parent cannot overwrite member state, and MCP exposes and controls both levels. |
| Layout | Typed read-pane specs reserve literal header rows and literal preferred content heights, preserve bottom slack, shrink deterministically only when constrained, and expose a parent read-stack overflow camera. Every read panel retains its own data/density scrollbar. |
| Tree-complete chrome | Read rows are projected from the presentation tree even when data is hidden, opening, unavailable, or absent at the locus. Annotation output is reconciled with the tree before reflow, so a renderer omission produces a restore header rather than deleting the control. Composite members represented by a parent row are not duplicated. |
| Shared geometry | Immutable per-workspace/per-channel pane-layout snapshots now drive gutter rows, clipping, scrollbars, hit testing, resize/drop targets, selection, and automation descriptions. Split workspaces own separate annotation and read snapshots. |
| Actions | GUI and MCP presentation mutations use the same typed `TrackIntent` reducer and ordered atomic batches. The resulting `TrackChangeSet` separates presentation/layout/persistence work from query demand. Presentation eyes no longer unload sources or alter query membership. |
| Read cameras | Scroll offset, content origin, latent fraction, content extent, and density are retained per `(workspace panel, read track)`, survive temporary hiding/minimization, persist through save/reopen, and are pruned only when a source is removed. |
| Selection | Pane-qualified node selection is canonical. Shift ranges follow presentation-tree order; hide, group, ungroup, and source removal reconcile anchors and hidden selections transactionally. |
| Migration diagnostics | Inconsistent legacy group flags and singleton groups are repaired deterministically and surfaced under **Project issues** instead of being silently normalized. |
| Automation | Automation schema v3 reports ordered nodes, groups, composites, desired/effective visibility, pane layouts, per-panel read state, source availability, current selection, and active gestures. MCP has focused commands for node/group/composite visibility, grouping, sizing, stack scroll, read scroll, and density. |

Four further failure modes were found while implementing the plan and were fixed before qualification:

1. Overlay parent visibility initially repeated the same lossy parent/child design as groups. It is
   now a separate persisted composite gate, with member eyes preserved.
2. Annotation headers still depended on successful renderer frame emission. A final tree-to-frame
   reconciliation now guarantees the control row even if content production fails, while recognising
   composite members already represented inside their parent row.
3. An indexed Ultima benchmark truth set still inherited BAM FLAG `0x4` from `Record::new`. The new
   correct unmapped-read filter exposed it by returning no records. The generator now explicitly
   marks those records mapped and asserts that invariant before exercising the production query.
4. Two renderer golden hashes still described the pre-directionality/pre-parity read raster. The
   current Ultima indel and mixed-technology PNGs were rendered at native size, visually inspected,
   and their reviewed hashes advanced to the already-committed rendering semantics.

Compatibility scalar fields and old automation names remain as boundary adapters for old sessions
and clients. They are projections into the normalized model, not alternate authorities for grouping,
visibility, sizing, selection, or query demand. One-level grouping remains an explicit product
constraint; nested groups are rejected rather than partially supported.

### Qualification record

- Core presentation tests include migration, atomic rollback, group contiguity/movement, independent
  channels, mixed parent/child eyes, composite gates, save/reopen state, and 24 deterministic random
  reducer sequences of 160 operations each.
- Desktop regressions cover literal sizing, bottom slack, header interaction minimums, overflow,
  missing renderer frames, composite row coalescing, selection reconciliation, per-panel camera
  lifetime, and snapshot-driven controls.
- The permanent MCP workflow is
  `validation/track_channel_mcp_qualification.py`. It exercises MF-04 and MF-12 through the real MCP
  stdio server, including six-track group/hide/show, independent channel grouping, different split-
  panel densities and nonzero scroll offsets, and exact save/reopen state comparison.
- The machine-readable result is
  `validation/manual-suite/evidence/track-channel-architecture/report.json`. Four 3024×1844 native
  captures in that directory record the hidden group, restored independent channels, split-panel
  cameras, and post-reopen state.
- The final automated acceptance commands are `cargo fmt --all -- --check`,
  `cargo check --all-targets`, and `cargo test --workspace`.

The remainder of this document preserves the pre-implementation audit and staged design record.
References to “current” behavior below describe the audited `34075ce` baseline, not the qualified
implementation summarized above.

## Evidence and exact failure sequence

The three screenshots inspected were:

- `Screenshot 2026-08-26 at 17.02.49.png`: one ungrouped read panel followed by an expanded group of
  five read panels;
- `Screenshot 2026-08-26 at 17.03.06.png`: the five-track group is minimized/hidden and its control
  is pushed to the bottom edge of the read pane; and
- `Screenshot 2026-08-26 at 17.03.18.png`: hiding the remaining read subtrack shrinks that panel, so
  the still-hidden group control moves back directly below it.

The live MCP workspace at the final state reported all six alignment sources as `visible: true` and
reported only the first track as `show_reads: false`. It could not report the read group or its
minimized state because those fields are absent from `AutomationTrackSnapshot` and
`AutomationAlignmentTrackSnapshot`. This is itself an observability defect.

The current transition is:

```text
expanded track A + expanded group B..F
                 |
                 | minimize group
                 v
track A flex-grows to all free height + 20 px group row at bottom
                 |
                 | hide A's Reads subtrack
                 v
20 px track A row + 20 px still-hidden group row at top + unused space
```

No ancestor visibility state changes during the last step. Only the layout changes.

## Required behavior and terminology

Before changing code, the product semantics must be made explicit.

### Source availability

A loaded source exists independently of whether either presentation channel currently paints it.
Disabling/unloading a source is a data operation. It must not be the accidental consequence of
minimizing a row.

### Node visibility

An eye controls whether a node's data content is painted. It must never remove the node's own header
or its restore control. A child eye changes only that child's desired visibility. It must not mutate
or re-enable an ancestor group.

### Group expansion

A chevron controls whether a group's child rows are expanded in the sidebar. This is separate from
group visibility. Minimizing a group hides its children but leaves the group header in the ordered
tree.

### Group visibility

A group eye applies a parent visibility gate without overwriting each child's saved eye state. When
the group is shown again, every child returns to its previous individual state.

### Channel independence

Grouping, selection, visibility, minimization, and sizing in `Annotation` must not mutate the
corresponding `Reads` state, and vice versa. Both nodes may refer to the same source `TrackId`, but
they are different presentation identities.

### Layout stability

Hiding or minimizing one node may remove that node's content, but must not cause an unrelated sibling
to greedily absorb the freed space. Stored preferred sizes remain stable. A pane resize may clamp
sizes deterministically; a visibility mutation may not globally renormalize them.

### State restoration

Hiding or minimizing a read panel must preserve its panel-local genomic-read scroll position,
density, latent scroll fraction, and preferred height. Only removing the source should delete that
state.

## Confirmed and predicted defects

| Priority | Defect | Current cause | Likely effect |
|---|---|---|---|
| P0 | Hidden group control moves and appears to disappear/reappear | Group headers and read content share the proportional `read_panel_bounds` allocation | The reported failure; inaccessible or misleading restore control |
| P0 | Hiding/minimizing a read panel loses its camera and density | `rebuild_active_panel_from_scroll_cache` retains `read_panel_scrolls` only for currently rendered read panels | Expanding a group resets scroll position and density |
| P0 | Group eye conflates minimization with visibility | A group has only `collapsed`; its eye dispatches `ToggleTrackGroup` | Users cannot predict whether an eye hides data, children, or both |
| P0 | Read and annotation channel state is coupled to source `visible`/`collapsed` | `SessionAlignmentTrack.visible` and `.collapsed` drive query membership and both channel layouts | Hiding one representation can remove the other representation's restore row or data |
| P1 | Reordering tracks can resize the whole read pane | `rebuild_genomic_display` reads the global read-pane height from `alignment_tracks.first().config.height` | Moving a different track into index zero changes the annotation/read split |
| P1 | Group identity is ambiguous across channels | `TrackFrameKey::Group(u32)` omits `TrackPane`; both panes independently allocate IDs beginning at one | Settings, selection, keyboard commands, and future automation can target the wrong group |
| P1 | Pane identity is inferred from a geometry field | UI code treats `frame.frozen_bottom.is_some()` as “read pane” | A geometry refactor or new frozen annotation row silently changes interaction semantics |
| P1 | Reordering can split a group | Contiguity is checked only when a group is created; ordinary track reorder does not move the group atomically or ungroup the moved member | Header anchor, rail, range selection, and membership order diverge |
| P1 | Hidden group members can remain selected | Collapsing a group removes member frames but does not reconcile `selected_read_tracks`, `selected_alignment_tracks`, or their anchors | Invisible bulk edits and broken Shift-click ranges |
| P1 | Hidden or source-collapsed read nodes disappear from the read channel | `read_panel_specs` excludes `!visible` and source-collapsed tracks | A read node cannot always be restored where it was hidden |
| P1 | Group state can be internally inconsistent | Group ID and collapsed state are mirrored on every member; rendering uses “any member is collapsed” | A partially updated or imported session silently minimizes the entire group |
| P1 | Group IDs are reused | New IDs are the lowest unused integer in the flat member fields | Stale UI/automation references can point at a different later group |
| P1 | More headers than the read pane can hold are clipped without an overflow model | The allocator consumes the finite pane and later rows receive zero height | Large workspaces lose controls at the bottom |
| P1 | A read group lacks the same context actions as an annotation group | The expand/minimize context-menu branch explicitly excludes read-pane frames | Inconsistent mouse and keyboard discoverability |
| P2 | Read-panel resize values are not literal sizes | `read_group_height` is validated and presented as a height but used as a relative weight | Dragging one boundary moves siblings and can feel like stutter or resistance |
| P2 | Group header is allocated 20 points but painted as at least 22 points | `COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT` differs from the header minimum | Two pixels are clipped at pane boundaries; hit targets can be smaller than visuals |
| P2 | Frame order is not guaranteed to be geometric order | All read group frames are pushed before ordinary annotation/read frames; several consumers iterate the vector directly | Drag/drop and “first matching frame” logic can choose an unrelated row |
| P2 | Panel-local read state is not persisted | Sessions save only legacy primary vertical scroll fields, not `read_panel_scrolls` | Reopening a multi-track workspace resets non-primary read cameras and density |
| P2 | Automation cannot inspect or configure the feature being tested | MCP snapshots omit pane node identity, group state, preferred heights, and per-read-panel scroll state | Screenshot-only regressions and false confidence in structural tests |
| P2 | One UI event can cause several full display rebuilds | Actions mutate the viewer, drafts, and deferred subtrack queues in different phases | Redundant work, order-sensitive behavior, and transient inconsistent frames |

## Architectural findings

### 1. The persisted model is flat while the UI is hierarchical

`SessionAlignmentTrack` stores:

- source visibility and source collapse;
- annotation group ID and collapse;
- read group ID and collapse;
- a general `height`;
- an annotation height;
- a read-group height; and
- every alignment subtrack/filter setting.

The group itself does not exist. Its identity, membership, and collapsed state are reconstructed by
scanning every alignment track. This forces normalization code to repair duplicated state and makes
group operations proportional to all tracks. More importantly, it gives the renderer no stable
ordered group node to lay out.

The session validator rejects zero IDs and “collapsed without an ID,” but it does not reject:

- a one-member group;
- members with conflicting collapsed values;
- non-contiguous group members;
- the same numeric group ID in both channels; or
- a group whose anchor changes meaning after reorder.

### 2. Layout currently creates the presentation tree

`rebuild_genomic_display` builds render primitives, group-derived suppression sets, read-panel specs,
track frames, pane geometry, overview data, and scroll caches in one function of roughly 1,400
lines. A row exists in the sidebar only if this render-oriented pass happens to emit a `TrackFrame`.

That direction of dependency is backwards. The ordered presentation tree should exist before
layout. Layout should assign rectangles to nodes. Rendering, hit testing, sidebars, scrollbars, and
automation should consume the same immutable layout snapshot.

### 3. Visibility has at least four meanings but two booleans

The implementation overloads `visible` and `collapsed` across source tracks, frames, subtracks,
groups, and composites. For example:

- source `visible = false` also changes query membership;
- a read frame's `collapsed` means `show_reads = false`;
- a group frame is created with `collapsed = true` even when the group is expanded, because that
  flag is also used to disable resize handles;
- group eye state is recovered from separate `TrackGroupChrome`; and
- a subtrack's `collapsed` means “show a compact restore row.”

Code that receives a boolean cannot know which meaning it carries. The current bug is therefore easy
to reintroduce even after fixing its coordinates.

### 4. Node identity does not include its channel

An alignment track deliberately has two frames with the same `TrackFrameKey::Alignment(TrackId)`.
Egui widget IDs append an inferred `read_pane_frame` boolean to work around collisions, while most
domain actions do not. Group IDs are worse: `TrackFrameKey::Group(1)` can mean annotation group 1 or
read group 1.

Every presentation action needs a complete target. Pane cannot be optional or inferred from
geometry.

### 5. The read-pane sizing model mixes a global boundary with per-track state

The height of the complete lower pane is read from the first alignment track's general `height`.
Individual read-panel sizes are stored as `read_group_height`, but the allocator treats those values
as weights and rescales all expanded panels to fill the pane.

This creates three brittle behaviors:

1. reordering changes which object owns the global divider;
2. hiding any panel changes every other panel's effective height; and
3. a drag persists one value but renders a normalized value, so the pointer and boundary can diverge.

Global read-pane height belongs to workspace presentation state. Individual preferred panel height
belongs to a read-channel node.

### 6. Scroll state lifetime is tied to renderability

Per-panel read scroll state is a good direction, but it is pruned to `live_tracks`, where “live” means
the current display cache contains rendered reads. A visibility or group-collapse operation therefore
has the same cleanup effect as source removal.

Runtime state should be retained for every loaded read node and deleted only in the source-removal
transaction. A layout can be absent without its state being dead.

### 7. UI mutation is not transactional

`TrackActionDraft` exposes mutable five-tuples `(id, name, visible, collapsed, height)` while
`dispatch_track_actions` also mutates group, annotation-size, and read-size state directly on the
viewer. Subtrack actions are deferred until much later in the frame. Settings and bulk actions can
then perform additional mutations and rebuilds.

There is no single reducer, no one change set, and no single point that decides whether a mutation
requires query work, layout work, selection reconciliation, or only repainting. This makes event
ordering part of application semantics.

### 8. Tests protect helpers rather than the user-visible state machine

Existing tests verify proportional tiling, one group header placed first, group normalization, and
individual widget actions. They do not exercise the exact state sequence that failed. In particular,
the group-header layout test never places an expanded sibling before the group, so it cannot observe
the bottom-edge jump.

The suite also lacks transition coverage for group/reorder, group/selection, group/persistence,
group/multi-panel scroll state, and parent/child visibility precedence.

## Target architecture

The target flow is:

```text
loaded source model
        |
        v
workspace presentation model
  - Annotation pane tree
  - Reads pane tree
        |
        v
pure PaneLayout snapshots
  - explicit node identity and pane
  - header/content/clip rectangles
  - scrollbar ownership
        |
        +-------------------+
        |                   |
        v                   v
canvas rendering      gutter/controls/hit testing
        |                   |
        +---------+---------+
                  v
              typed intents
                  |
                  v
             one reducer pass
                  |
                  v
     query/layout/render/persist change set
```

### First-class identities

Introduce typed identities similar to:

```rust
enum TrackChannel {
    Annotation,
    Reads,
}

struct GroupId(u64);

enum PresentationNodeId {
    Reference,
    Track { channel: TrackChannel, track_id: TrackId },
    Group { channel: TrackChannel, group_id: GroupId },
}
```

`TrackFrameKey` should either become this type or be removed. `TrackFrame` must carry `channel`
directly. `frozen_bottom` remains geometry and must not identify a pane.

### First-class pane tree

Persist each group once:

```rust
struct PresentationGroup {
    id: GroupId,
    label: String,
    channel: TrackChannel,
    members: Vec<TrackId>,
    expanded: bool,
    content_visible: bool,
}

struct PaneTrackState {
    track_id: TrackId,
    content_visible: bool,
    preferred_height: f32,
}

struct WorkspacePresentation {
    read_pane_height: f32,
    annotation_order: Vec<PresentationEntry>,
    read_order: Vec<PresentationEntry>,
}
```

One level of groups is sufficient for the current product. Nested groups should be rejected until
there is a real requirement. Annotation and read groups remain independent but use the same model.
The model should support alignment and auxiliary annotation nodes; quantitative overlays remain
data-composition objects that may themselves appear as one presentation entry.

### Visibility resolution

Compute effective visibility without rewriting child state:

```text
effective child content visibility =
    source available
    AND child desired visibility
    AND every ancestor group content visibility
    AND child is exposed by the group's expansion policy
```

The group eye toggles `content_visible`. The chevron toggles `expanded`. A child eye changes only
`PaneTrackState.content_visible`. The group header is always emitted while the group exists.

Query demand should be derived separately. Initially, keep loaded sources queried while hidden to
prioritize correctness and instant restoration. Query suspension can be added later from an explicit
“any consumer needs data” calculation; it must never be driven by a presentation boolean directly.

### Pure pane layout

Replace tuple specs with typed inputs and outputs:

```rust
struct PaneItemSpec {
    id: PresentationNodeId,
    header_height: f32,
    content_visible: bool,
    preferred_content_height: f32,
    minimum_content_height: f32,
}

struct PaneItemLayout {
    id: PresentationNodeId,
    row_rect: Rect,
    header_rect: Rect,
    content_rect: Option<Rect>,
    clip_rect: Rect,
}
```

Use one header-height constant for allocation, painting, hit testing, and accessibility. A header-only
group is not a hidden read panel.

The read layout policy should be:

1. Reserve explicit header rows.
2. Use stored preferred content heights as pixel sizes, not weights.
3. Do not grow siblings when another item becomes hidden.
4. On pane resize, consume or create bottom slack before resizing panels.
5. If the pane becomes smaller than the requested total, shrink visible content deterministically
   down to its minimum without shrinking headers.
6. If headers plus minimum content still overflow, enable a conventional read-stack overflow camera.
   This parent camera moves panel rows; each visible read panel retains its own right-hand data/density
   scrollbar. The overflow control should live in the gutter so it is not confused with genomic-read
   scrollbars.
7. A boundary drag changes one node's preferred height, or transfers height only to its immediate
   visible neighbour when there is no slack. It must not globally renormalize every panel.

The same immutable `PaneLayout` must drive canvas clipping, gutter rows, resize handles, group rails,
read scrollbar rectangles, hit testing, keyboard focus, and automation descriptions.

### Typed intents and a single reducer

Replace tuple mutation and deferred queues with intents such as:

```rust
enum TrackIntent {
    SetNodeVisibility { target: PresentationNodeId, visible: bool },
    SetGroupExpanded { target: PresentationNodeId, expanded: bool },
    SetGroupVisibility { target: PresentationNodeId, visible: bool },
    ResizeNode { target: PresentationNodeId, preferred_height: f32 },
    ResizeReadPane { height: f32 },
    GroupNodes { channel: TrackChannel, members: Vec<TrackId> },
    Ungroup { target: PresentationNodeId },
    MoveNode { target: PresentationNodeId, before: Option<PresentationNodeId> },
    SelectNode { target: PresentationNodeId, mode: SelectionMode },
}
```

The reducer applies an ordered batch once and returns:

```rust
struct TrackChangeSet {
    presentation_changed: bool,
    layout_changed: bool,
    query_demand_changed: BTreeSet<TrackId>,
    selection_changed: bool,
    persistence_changed: bool,
}
```

Only the caller performs the resulting query request, layout rebuild, GPU update, and autosave.
Resizing may rebuild layout each frame, but must never request genomic data.

### Selection and reorder policy

- Pane selection stores complete `PresentationNodeId` values.
- Shift-click ranges are computed from the presentation tree, not the current render frame vector.
- Minimizing a group clears hidden child selections and anchors. Selecting the group itself remains
  possible.
- Grouping selected nodes replaces their contiguous range with one group node.
- Dragging a group moves it atomically.
- Dragging a member out of a group must be an explicit ungroup-and-move operation; ordinary source
  reorder must not silently split the group.
- Removing a source removes its nodes from both pane trees, repairs or removes affected groups, and
  is the only ordinary operation that deletes its saved scroll state.

### Persistence and MCP observability

Introduce a new session schema version with migration from the mirrored member fields. The migration
must:

1. build one group object per `(channel, old_group_id)`;
2. preserve member order from the alignment-track vector;
3. resolve inconsistent collapsed flags deterministically and emit a project issue;
4. discard singleton groups with a project issue; and
5. move the first track's `height` into workspace `read_pane_height` while preserving each track's
   explicit annotation and read preferred heights.

Persist panel-local read state for every loaded read node:

- scroll points;
- content origin;
- latent scroll fraction; and
- panel-local density override.

Extend the automation snapshot with:

- ordered annotation and read pane nodes;
- fully qualified group IDs and member IDs;
- desired and effective visibility;
- group expansion state;
- preferred and effective rectangles/heights;
- per-panel read scroll maximum, offset, and density; and
- the target node for any active resize or scrollbar gesture.

Add focused MCP mutations for group, visibility, and sizing only after the reducer exists. MCP and GUI
must call the same intents.

## Implementation phases

### Phase 0 — Freeze semantics and reproduce failures

1. Add a state-transition regression for six read tracks: group tracks B–F, hide/minimize the group,
   hide A's Reads subtrack, and assert that the group state does not change.
2. Add a layout regression with an expanded track before a group header; assert that the group header
   remains within the read pane and does not jump because a sibling visibility flag changes.
3. Add a test proving collapse/expand preserves every member's scroll and density.
4. Add tests for the 22-point header allocation and hit rectangle at both pane boundaries.
5. Retain the three screenshots as the visual bug record and add uniquely named MCP before/after
   captures to the qualification artefacts.

Exit criterion: the current failure and the state-loss failure are red tests before production code
changes.

### Phase 1 — Contain the current failure with target-compatible types

1. Replace `(ReadPanelKey, bool, Option<f32>)` with a typed `PaneItemSpec`.
2. Separate header height from content height and represent groups as header-only nodes.
3. Stop flex-growing an unrelated panel when a sibling is hidden; preserve explicit preferred
   heights and bottom slack.
4. Use the same header constant in allocation and painting.
5. Stop pruning `read_panel_scrolls` on hide/minimize; prune them in track removal only.
6. Cancel an active resize/overview gesture safely if its source is actually removed.

This phase fixes the reported issue without creating another boolean convention that must later be
removed.

Exit criterion: the exact screenshot sequence keeps the hidden group control in place, closed-eye
state remains unchanged, and expanding the group restores all five cameras and density values.

### Phase 2 — Introduce the presentation model and session migration

1. Add `TrackChannel`, fully qualified presentation node identity, pane trees, and first-class groups.
2. Move global read-pane height out of the first alignment track.
3. Move pane-specific visibility and preferred size out of source-level visibility/collapse fields.
4. Add session schema migration and strict validation of membership, order, ID uniqueness, and group
   state.
5. Preserve backward-compatible import of existing schema versions; write only the new normalized
   schema.

Exit criterion: no runtime group property is mirrored across member tracks and track reorder cannot
change the global pane split.

### Phase 3 — Replace layout-derived chrome

1. Build one pure `PaneLayout` for each channel and workspace panel.
2. Generate annotation/read render primitives against those rectangles.
3. Make gutter chrome, scrollbars, hit testing, focus, resize handles, and drag/drop consume the same
   snapshot.
4. Split `rebuild_genomic_display` into source projection, annotation layout, read layout, and final
   composition stages.
5. Split the approximately 1,000-line gutter painter into stateless row painters and interaction
   adapters.

Exit criterion: removing a `TrackFrame` construction branch cannot remove a sidebar control; frames
are a projection of the tree and layout rather than the source of either.

### Phase 4 — Transactional actions

1. Replace `TrackControl` tuples with typed immutable view models.
2. Route GUI, keyboard, context menu, settings, bulk edit, and automation through one `TrackIntent`
   reducer.
3. Apply one `TrackChangeSet` per frame.
4. Reconcile selection and active gestures inside the same transaction.
5. Request queries only from explicit query-demand changes and perform at most one layout rebuild per
   action batch.

Exit criterion: the order of later UI reconciliation blocks cannot change the meaning of a track
action.

### Phase 5 — Complete controls, persistence, and automation

1. Give groups separate chevron and eye controls in both channels.
2. Add identical read/annotation group context actions and keyboard behavior.
3. Add read-stack overflow behavior for large workspaces.
4. Persist all per-panel read cameras/densities and presentation state.
5. Extend MCP state and focused configuration commands.
6. Update the user guide and manual fixture instructions.

Exit criterion: every visible control and every persisted state transition can be described and
asserted through MCP without relying on pixel inference.

### Phase 6 — Qualification and removal of compatibility paths

1. Run focused unit/state-machine tests on every platform target.
2. Run MF-04 with six bisulfite alignment tracks and MF-12 with four/eight tracks and split panels.
3. Exercise group/hide/show/reorder/resize/save/reopen in annotation and read channels independently.
4. Inspect native MCP captures at original resolution.
5. Remove mirrored legacy runtime fields, first-track height ownership, legacy scalar scroll mirroring
   beyond the session migration boundary, and pane inference through `frozen_bottom`.

Exit criterion: the compatibility fields are used only by old-session migration, not live layout or
interaction code.

## Required regression matrix

| Scenario | Invariants |
|---|---|
| Group B–F; hide group; hide/show A reads | Group header never leaves accessible layout; group visibility and expansion do not change |
| Hide group with mixed child eye states; show group | Every child restores its previous desired state |
| Minimize and expand a scrolled group | Each child restores offset, density, and preferred height |
| Reorder an ungrouped track around a group | Group remains contiguous and moves only according to explicit tree operations |
| Attempt to drag a grouped member out | Operation is explicitly rejected or performs an atomic ungroup-and-move |
| Shift-select across visible read nodes | Range follows read-tree order and never selects annotation nodes |
| Minimize a selected group | Hidden member selections/anchors are reconciled; no invisible bulk edit remains armed |
| Hide annotation node only | Read node and read query presentation remain unchanged |
| Hide read node only | Coverage/annotation node remains unchanged |
| Resize one read panel | Only that panel and its adjacent constraint/slack change; unrelated panels do not renormalize |
| Reorder the first alignment source | Global annotation/read divider does not move |
| More minimum headers than pane height | Explicit read-stack overflow appears; no zero-height restore control |
| Two split workspace panels | Group tree is shared as designed; read offsets/densities and scrollbars remain panel-local |
| Save/reopen | Groups, eyes, chevrons, pane size, preferred panel sizes, and every panel camera round-trip |
| Same numeric legacy group in both panes | Migration produces distinct fully qualified groups |
| Inconsistent legacy member flags | Migration resolves deterministically and reports the repair |
| MCP qualification | Workspace state exposes the same node identities, effective visibility, sizes, and scroll state as the screenshot |

Add property tests over random sequences of group, ungroup, move, hide, show, minimize, expand, resize,
and remove. After every intent batch, assert:

- every loaded source has at most one node per supported channel;
- every group has at least two unique members;
- no member occurs both at pane root and inside a group;
- node identities are unique and pane-qualified;
- headers never overlap and never have a height below the interaction minimum;
- all effective content rectangles are contained by their owning pane/panel clip;
- selection and active gestures reference existing interactive nodes; and
- hiding/minimizing never deletes persistent read camera state.

## Code hotspots to change

- `crates/rgv-core/src/session.rs`: replace mirrored group fields and overloaded heights with the new
  presentation/session schema.
- `apps/rgv-desktop/src/workspace/tracks.rs`: replace group reconstruction/normalization with pane-tree
  operations and the reducer.
- `apps/rgv-desktop/src/display/alignment.rs`: replace proportional tuple allocation with the pure
  typed pane layout.
- `apps/rgv-desktop/src/display/mod.rs`: decompose `rebuild_genomic_display` and stop opportunistically
  constructing track chrome while rendering data.
- `apps/rgv-desktop/src/ui/tracks.rs`: use explicit pane node identity, separate eye/chevron semantics,
  and paint the immutable layout snapshot.
- `apps/rgv-desktop/src/app/actions.rs`: replace mixed direct/draft mutation with typed intents and one
  change set.
- `apps/rgv-desktop/src/ui/frame.rs`: remove the multi-pass tuple reconciliation and repeated rebuilds.
- `apps/rgv-desktop/src/workspace/panels.rs`: retain/persist per-read-node camera state independent of
  current renderability.
- `crates/rgv-core/src/automation.rs` and `apps/rgv-desktop/src/automation/commands.rs`: expose pane tree,
  group, visibility, size, and scroll state.
- `apps/rgv-desktop/src/tests/display.rs`, `apps/rgv-desktop/src/ui/tracks.rs` tests, core session tests,
  and MCP qualification: add transition, invariant, persistence, and screenshot coverage.

## Definition of done

The remediation is complete only when all of the following are true:

1. The exact three-step failure cannot move, remove, or re-enable the hidden group control.
2. Eye and chevron actions are separate and consistent in both channels.
3. Parent and child visibility states are independent and restore correctly.
4. Annotation grouping never groups read nodes, and read grouping never groups annotations.
5. Hiding/minimizing never deletes a read panel's camera, density, or preferred height.
6. Track reorder cannot change the global pane boundary or corrupt a group.
7. Sidebar, canvas, scrollbars, hit testing, resize handles, and automation use one layout snapshot.
8. Sessions round-trip first-class groups and all multi-panel read camera state.
9. MCP can prove the group and per-panel state associated with every retained screenshot.
10. The MF-04 and MF-12 manual workflows pass without control disappearance, overlap, bleeding,
    stutter, cross-channel selection, or scrollbar coupling.
