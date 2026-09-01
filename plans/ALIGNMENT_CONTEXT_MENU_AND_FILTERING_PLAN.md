# Alignment context menu, filtering, and read-presentation plan

Status: code-complete for the initial typed-filter/context-menu delivery on 2026-08-31.
Automated qualification is complete; native visual checkpoints A–D and the combined real-data
pass remain explicitly pending user assessment. No visual gate is accepted by this status.

Baseline: `feature/unified-alignment-scene` at `5a28d90` (`fix: stabilize dense alignment scene
browsing`). This plan extends the unified alignment scene; it must not restore a query-local,
exact-viewport, or legacy alignment authority.

Related authority:

- `UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md` remains the architectural authority for
  alignment evidence, rows, rendering, interaction, and atomic publication.
- `UNIFIED_ALIGNMENT_SCENE_INTERACTION_AND_CAPACITY_REMEDIATION_PLAN.md` remains the record of the
  dense-locus stability work.
- This document is the focused delivery plan for alignment context menus, interactive filtering,
  the neutral-read colour correction, base-letter restoration, and mate-panel scrollbar
  qualification.

## Implementation record

- Neutral reads now use grey presentation tokens; hover and selection remain distinct blues, with
  a separate gold non-destructive highlight overlay.
- Base glyphs are projected from committed semantic base primitives only when both projected base
  width and row height meet the eight-pixel legibility boundary.
- The native alignment canvas supports secondary-click, macOS control-click, Shift+F10/context-key
  invocation, immutable committed-scene targets, stale-target refusal, read actions, position-aware
  arrangement, colour/display controls, and quick filter/highlight actions. Coverage/derived lanes
  and the read stack resolve through the same context model.
- Acquisition retains a broad immutable record population. Versioned typed AND predicates derive
  a stable-handle view used atomically by rows, coverage, alleles, junction/Sashimi summaries,
  modification summaries, picking, inspection, diagnostics, and filtered export. Clearing a
  retained filter is covered by a zero-source-I/O regression.
- Intrinsic, flag, MAPQ, template, mate-contig, typed auxiliary, CIGAR-event, aligned-length, and
  exact reference-derived substitution predicates are supported with anywhere, fixed-position,
  and fixed-interval scopes where meaningful. Reference-derived evaluation identity includes the
  immutable reference revision.
- Track settings expose active typed predicates as removable entries and common biological filter
  constructors. The complete typed contract is also session-persistent and controllable through
  automation/MCP. Track headers expose active filter/highlight state and exact counts.
- Structured exports distinguish filtered and full eligible populations and include exact filter
  fingerprint, population, per-predicate, and source-provenance fields.
- Mate panels retain independent read-camera/scroll state; state-level regression coverage is in
  place. Native scrollbar appearance and interaction still require the user-only checkpoint.
- Manual diagnostic traces now use schema version 2 and capture context-menu causality,
  privacy-preserving typed predicate structure, requested/staged/committed filter authority,
  exact match counts, reference dependency, retained-evidence filter timing versus source I/O,
  all-panel scroll authority, and base-label projection/legibility counts. Auxiliary predicate
  values, read names, sequence, credentials, URLs, and source paths remain excluded.

## Outcome

RGV should provide direct, context-sensitive alignment controls comparable to IGV while retaining
RGV's complete-evidence and unified-scene guarantees.

The user should be able to right-click an alignment lane or a specific read and:

- sort and group reads using existing and position-aware modes;
- colour reads using the existing alignment colour modes;
- filter or highlight reads using explicit biological and technical predicates;
- inspect, select, copy, and navigate to mates or supplementary alignments;
- change relevant display settings without leaving the locus; and
- understand which filters are active and how many reads they retain.

The default unclassified read body should be grey. Hovered and selected reads should become
visibly blue. At sufficient base and row size, substitution blocks should contain their base
letters again.

Opening a mate in another panel must produce a fully independent, working vertical read scrollbar.

## Current evidence

### RGV

- The read canvas has no context-sensitive right-click menu. A small menu exists on the track
  gutter/header, but it offers track management rather than alignment operations.
- The settings panel already exposes substantial colour, grouping, sorting, pairing, stacking,
  base-display, quality, coverage, junction, modification, MAPQ, flag, and SAM-tag controls.
- The unified renderer maps `AlignmentBodyColorToken::Neutral` to a blue-grey colour even though
  the UI labels the mode `IGV grey`. The previous `main` implementation used a light neutral grey.
- Unified semantic pages retain base identity, base quality, and match/mismatch classification.
  The GPU path renders the coloured base rectangle but the desktop does not present base glyphs.
  This is a presentation regression, not lost evidence.
- The current source filter supports MAPQ, required/excluded flags, and auxiliary-tag predicates.
  It has no typed CIGAR-event or reference-derived substitution predicates.
- The current unified epoch contains frames for every panel and every panel owns independent read
  scroll state. The previously reported mate-panel scrollbar problem is therefore not proven on
  the latest build, but the exact user workflow has no focused regression or current manual
  acceptance.

### IGV source audit

The local IGV repository was inspected at commit `aa47fe9d5bcd`.

IGV's alignment menu is assembled in
`src/main/java/org/igv/alignment/AlignmentTrackMenuHelper.java`. It conditionally exposes grouping,
sorting, colouring, display, paired-read, read-specific, supplementary, and Sashimi actions based
on the clicked read, track capabilities, experiment type, and available tags.

Important findings:

- IGV groups and sorts already-loaded alignments; these operations do not reread the BAM.
- IGV supports grouping by the base or insertion at the clicked genomic position.
- IGV's base sort uses the viewport centre, whereas RGV should use the clicked coordinate when the
  action originates in a context menu.
- IGV's default alignment colour is grey (`RGB 185,185,185`).
- IGV changes from coloured base blocks to letters once the available base width and row height
  support an eight-pixel font.
- IGV does not provide a working arbitrary read-filter menu. Its disconnected filter menu ends in
  a TODO. Load-time preferences filter common flags, MAPQ, alignment score, and read groups.
- IGV's `Hide small indels` is an appearance option, not a read filter.

RGV can therefore use IGV's direct-manipulation strengths without inheriting its unfinished filter
model.

## Product contract

### Context and scope

1. A context-menu action targets the alignment track and panel under the pointer, not whichever
   track happens to be globally primary.
2. The menu captures the clicked genomic coordinate and, when present, a stable alignment handle
   and semantic component.
3. Actions apply to the clicked track by default. Applying an action to selected or all alignment
   tracks must be an explicit menu choice.
4. Right-clicking empty read-lane space opens track-level alignment controls.
5. Right-clicking a read adds read-specific actions before the track-level sections.
6. Right-clicking a base, insertion, deletion, or coverage position adds coordinate-aware actions.
7. Control-click on macOS and the keyboard context-menu command must reach the same action model.
8. Opening or operating the menu must not interfere with left-button pan, read selection, wheel
   scrolling, or vertical-scrollbar dragging.

### Read appearance

1. `AlignmentColorMode::None` renders read bodies in neutral grey.
2. Hover changes the read body to a lighter blue state; selection changes it to the stronger RGV
   selection blue.
3. Selected supplementary-chain styling remains distinguishable from ordinary selection.
4. Base, modification, indel, and specialist evidence remains legible in every interaction state.
5. Colour-only changes do not reacquire the source or rebuild row assignment.
6. At sufficient zoom and row height, visible base blocks show centered A/C/G/T/N glyphs with
   contrast chosen from the base fill.
7. Below the glyph threshold, the same evidence remains as coloured blocks with no unstable
   threshold flicker.

### Sorting and grouping

1. Sorting and grouping operate on retained stable handles and publish a new immutable row atlas.
2. They do not reread the BAM when the required record payload is already resident.
3. Position-aware actions retain an explicit genomic anchor; they do not silently follow the
   moving viewport centre.
4. Re-pack clears the explicit sort/group arrangement according to a documented reset contract.
5. Reversing order changes row ordering only, not evidence membership or summaries.
6. Multi-track actions publish all explicitly targeted tracks coherently or leave their previous
   rows intact.

### Filtering and highlighting

1. A filter is a derived selection over retained complete evidence. It does not delete or mutate
   canonical evidence.
2. Clearing a filter over already-retained capabilities restores the prior complete view without
   source I/O.
3. A true filter affects the read stack and every alignment-derived summary: coverage, alleles,
   Sashimi/junctions, and modification frequency.
4. `Highlight matching reads` uses the same predicates but does not change membership or derived
   summaries.
5. Active filters are persistently visible outside the popup as removable chips and an exact
   `shown of eligible` count.
6. Initial UI composition is logical AND. The stored typed expression must be versionable so OR
   and NOT can be added without replacing the filter model.
7. Filters must be intrinsic or anchored: anywhere in the read, at a fixed genomic position, or
   within a fixed selected interval.
8. A persistent filter must not mean `currently visible in the viewport`; panning must not silently
   change its biological meaning.
9. Reference-dependent filters are disabled with an explanation when the matching reference is
   absent. They never guess from colour, `NM`, or incomplete reference coverage.
10. Filter evaluation, row publication, summaries, pixels, picking, inspection, export, and
    diagnostics share one committed epoch.

### Complete-evidence guarantee

Filtering changes what is presented, not what RGV claims was loaded.

- `eligible` is the exact record population admitted by the acquisition/source contract.
- `matching` is the exact stable-handle selection produced by the active view-filter contract.
- `shown` is the matching population when individual-read presentation is available.
- A complete-read refusal remains truthful for the matching population and does not fall back to a
  partial read stack.
- Filter provenance reports exclusions by predicate as well as existing source-level exclusions.
- Export explicitly records whether it exports the full eligible evidence or the filtered view.

## Architectural model

### 1. Context snapshot and typed commands

Introduce an immutable context captured from the committed presentation:

```text
AlignmentContextTarget
  panel_id
  track_id
  committed_target_revision
  scene_revision
  genomic_position
  optional stable handle
  optional semantic component
```

The popup only emits typed `AlignmentContextAction` values. It must not directly mutate track,
panel, engine, row-atlas, or renderer state.

Before applying an action, the controller validates its target against the current committed epoch.
A stale read-specific target is rejected rather than rebound to a different read. Track-level
actions may be safely retargeted only when the panel, track, and genomic anchor remain valid.

Use the existing native input path to detect secondary-click intent and open the popup. Do not add
a full-canvas egui response that captures left-button drag or wheel input.

### 2. Explicit operation effects

Every action declares one effect class:

| Effect | Examples | Permitted work | Forbidden work |
| --- | --- | --- | --- |
| Appearance | neutral/strand colour, quality shading, base letters, hide small event marks | style/frame update and bounded camera text projection | source I/O, evidence replacement, row repack |
| Arrangement | sort, group, reverse, pair view, re-pack | row-atlas derivation and semantic page selection from retained evidence | BAM reread when payload is resident |
| View filter | MAPQ, flags, tags, CIGAR events, substitutions | predicate facts, stable selection, rows and summaries | canonical evidence deletion or reads-only summary divergence |
| Capability acquisition | missing sequence, auxiliary payload, or reference interval | typed source/payload/reference upgrade | silent guessing or partial publication |
| Read action | inspect, copy, select, mate/supplementary navigation | stable-handle lookup and explicit navigation | query-local record indexes |

This classification is part of diagnostics and tests. A visually fast result is not acceptable if
it performs work forbidden for its class.

### 3. Separate acquisition and view-filter contracts

The current `RecordContract` mixes source acquisition identity with user-controlled filtering.
Factor this boundary deliberately:

- an acquisition contract describes which source records and payload capabilities are retained;
- a view-filter contract describes the currently matching stable handles;
- an appearance contract describes colour and glyph presentation; and
- a row-layout contract describes sorting, grouping, pairing, density, and stacking.

Changing a view predicate must reuse the retained evidence root. If a newly requested predicate
requires payload that was never acquired, the source actor performs an explicit capability upgrade.
It must not discard the previous committed view while that upgrade is pending.

Existing MAPQ, flag, and auxiliary-tag controls should migrate to the typed view-filter model where
their required evidence is retained. Hard source exclusions that cannot become interactive product
choices remain in the acquisition contract and are named separately in provenance.

### 4. Predicate representation

Add a versioned typed predicate model. The initial vocabulary should cover:

#### Source and flag facts

- mapping quality range;
- duplicate, secondary, supplementary, failed-QC, paired, proper-pair, mapped-mate, and strand
  states;
- template-length range and mate contig; and
- typed auxiliary-tag existence/comparison.

#### CIGAR-derived facts

- any insertion, any deletion, or either indel;
- insertion/deletion at an anchored position;
- minimum and maximum event length;
- soft or hard clipping and minimum clip length;
- reference skip/splice; and
- aligned-length range.

#### Reference-derived facts

- any substitution in the aligned portion of the read;
- substitution at an anchored position;
- observed base at an anchored position;
- substitution count/range; and
- optionally a fixed selected interval as scope.

`M` CIGAR operations cannot distinguish matches from substitutions. Reference-derived predicates
therefore require sequence payload and a matching reference revision. An explicit `X` operation may
be used as positive evidence, but absence of `X` is not proof that a read has no substitutions.
`NM` is not a substitute because it conflates substitutions and indels.

### 5. Immutable predicate facts and indexes

Build reusable, ledger-charged facts keyed by evidence revision and stable handle:

- intrinsic flag and numeric facts;
- compact CIGAR-event facts and anchored indexes;
- required auxiliary values; and
- lazy reference-derived substitution facts keyed additionally by reference revision and covered
  interval.

Evaluation produces an immutable matching-handle bitmap/selection plus exact per-predicate counts.
Changing combinations should operate on these facts rather than rescanning BAM records or every
base repeatedly.

Reference-derived `anywhere in read` is Ready only when the required aligned reference spans are
certified. Until then it is Pending or Unavailable; incomplete spans must not be treated as no
substitution.

All fact indexes, bitmaps, scratch, and retained selections participate in the alignment allocation
ledger and its existing current/candidate publication rules.

### 6. Atomic filtered scene publication

A filter candidate comprises:

- the filter contract and capability state;
- its matching stable-handle selection;
- exact inclusion/exclusion provenance;
- the corresponding immutable row atlas;
- read semantic pages;
- exact coverage, allele, junction, and modification summary roots; and
- the matching GPU presentation bundle.

These commit together through `PresentedAlignmentEpoch`. While evaluation or admission is pending,
the previous committed epoch remains fully interactive. Cancellation, a newer filter action, memory
pressure, or reference failure cannot publish a half-filtered view.

### 7. Base glyph presentation

Do not put screen-font geometry into canonical evidence or immutable genomic semantic pages.

Reuse the existing base primitive, which already contains the observed base, quality, and
classification. During camera capture:

1. enumerate only visible base primitives from prepared pages and row blocks;
2. calculate projected base width and row height;
3. emit a glyph only when the configured minimum legible font size is met;
4. select a contrast-safe glyph colour from the base fill and interaction state;
5. include the glyph frame in the same committed read presentation; and
6. keep the camera-dependent list bounded by visible geometry and ledger-charged scratch.

Read-name, event-length, and base glyphs should share one alignment text-presentation boundary so
all existing semantic labels are either deliberately supported or explicitly disabled. The
desktop must not consume junction labels while silently dropping read labels from the same epoch.

### 8. Multi-panel scrollbar authority

The mate action continues to create a new panel using a stable pending read identity. The new panel
must acquire and commit its own:

- panel scene frame;
- camera-exact row activity;
- `ReadPanelScrollState`;
- content extent, density overview, thumb, and hit geometry; and
- stable selection/reveal target.

No primary-panel vertical state may be copied as semantic authority. The global atomic epoch may
contain several panels, but every scrollbar derives from the matching panel/track row profile.

## Menu information architecture

### Read-specific section

Shown only when a committed read/component was hit:

- Inspect read
- Select read / clear selection
- Select or highlight reads with this name
- Go to mate
- Open mate in new panel
- Open supplementary segment(s)
- Copy read name, details, sequence, clipped sequence, or insertion sequence when available
- Keep only / highlight reads matching a relevant clicked fact

Unavailable actions remain visible with concise explanations where that teaches a capability
requirement; irrelevant specialist actions may be omitted.

### Arrange reads

- Sort by the existing supported modes
- Sort by base at the clicked position
- Reverse sort order
- Group by the existing supported modes
- Group by base or insertion at the clicked position
- Reverse group order
- Stable or Compact stacking
- View as pairs and pair bounds
- Re-pack/reset arrangement

### Colour reads

Expose the existing colour modes first, using radio/check state from the clicked track. Add missing
IGV-parity modes only after their semantic tokens and product value are established; do not place
nonfunctional entries in the menu.

High-value follow-up candidates are duplicate status, supplementary/split status, read order, read
name for bounded datasets, library/sample metadata, and position-aware base state.

### Filter or highlight reads

Quick entries:

- Contains substitution
- Contains insertion
- Contains deletion
- Contains any indel
- Contains soft clipping
- Contains splice
- Duplicate / secondary / supplementary
- MAPQ threshold
- Base or event at the clicked position
- Highlight matching instead of filtering
- Edit filters…
- Clear filters

`Edit filters…` opens the full properties editor using the same typed contract. The popup and
properties panel must never maintain separate filter models.

### Display and track actions

- Mismatches only / all bases / hide bases
- Shade bases by quality
- Minimum displayed indel/event-label size
- Show/hide reads, coverage, junctions, and modification frequency
- Track settings…
- Export full eligible evidence…
- Export filtered view…
- Apply selected appearance/arrangement to selected tracks…

## Delivery phases

### Phase 0 — freeze contracts and capture regressions

1. Record this plan and map every proposed action to its effect class.
2. Capture the accepted `main` grey and substitution-letter behaviour as semantic/render fixtures.
3. Add a source inventory for the existing sort/group/colour/filter code paths and session fields.
4. Record a focused manual reproduction card for the mate-panel scrollbar workflow.
5. Define the versioned context target, action, predicate, scope, and filter-provenance types.

Gate:

- every action has one owner and effect class;
- the old and current colour/glyph differences are explained by deterministic fixtures;
- no design question remains about filter effects on derived summaries; and
- no implementation starts by attaching a left-click-capturing widget over the genomic canvas.

### Phase 1 — correct neutral colour and restore base letters

1. Restore a neutral grey default using the previously accepted RGV/IGV visual baseline.
2. Make hover and selection visibly blue without losing legibility of base/event evidence.
3. Extend the camera-dependent alignment label projection to include base glyphs.
4. Present read, event, and base labels from the committed read frame in the desktop.
5. Preserve coloured-block rendering below the legibility threshold.

Automated gate:

- neutral, hover, selected, and selected-chain tokens produce distinct expected colours;
- base letters appear only above the width/height threshold;
- A/C/G/T/N identity and contrast agree between CPU reference and desktop text projections;
- panning at base zoom performs no source acquisition, row repack, or unchanged geometry upload;
- missing reference produces unknown/missing substitution classification rather than false matches;
  and
- the full affected scene, renderer, engine, and desktop suites pass.

Manual checkpoint A — user only:

- confirm ordinary reads are grey;
- confirm hover and selection become blue;
- zoom through the base-letter threshold and verify stable, legible substitution letters; and
- inspect insertions, deletions, direction terminals, and selected reads for visual regressions.

### Phase 2 — context-menu foundation and existing controls

1. Add secondary-click context capture without changing primary navigation input.
2. Add the immutable target and typed command dispatcher.
3. Build read/background/component menu sections from committed capabilities.
4. Expose existing sort, group, colour, base-display, pair, stacking, and track actions.
5. Add clicked-position anchors for base sorting/grouping and insertion grouping.
6. Add keyboard and accessibility semantics.
7. Persist all changes through the existing session settings authority.

Gate:

- a read-specific action resolves the same stable handle as picking and inspection;
- a stale handle cannot act on another read;
- colour actions perform appearance-only work;
- sort/group actions perform no source query with resident payload;
- context-menu use does not steal pan, click selection, wheel, or scrollbar input;
- per-track and explicit multi-track scope are deterministic; and
- save/reopen preserves the selected modes and fixed genomic anchors.

Manual checkpoint B — user only:

- right-click empty space, a read body, a base, and an indel;
- verify menu relevance, naming, check states, speed, and placement;
- exercise sorting, grouping, colouring, re-pack, selection, and mate actions; and
- confirm ordinary left-drag navigation feels unchanged.

### Phase 3 — typed view-filter engine

1. Factor acquisition and view-filter contracts without weakening source completeness proof.
2. Implement versioned predicate and scope types.
3. Build intrinsic and CIGAR fact indexes.
4. Build lazy reference-derived substitution facts with exact coverage state.
5. Produce immutable stable-handle selections, counts, and provenance.
6. Derive rows and every alignment summary from the matching selection.
7. Publish the complete filtered candidate atomically.
8. Add cancellation, latest-wins, pressure, and capability-upgrade handling.

Gate:

- every predicate matches an independent per-record oracle;
- arbitrary AND combinations equal a clean independent selection;
- indel predicates require no reference;
- generic substitutions are never inferred without certified reference coverage;
- clearing filters over retained evidence performs zero BAM acquisition;
- rows, coverage, alleles, junctions, modifications, picking, inspection, and export share one filter
  fingerprint and epoch;
- cancellation or budget refusal leaves the previous view intact; and
- repeated filter/reset cycles reach a stable charged memory high-water mark.

### Phase 4 — filter UI, highlighting, and provenance

1. Add quick filter/highlight actions to the context menu.
2. Add one full editor in track settings using the same typed model.
3. Add removable active-filter chips and exact counts to the alignment track UI.
4. Add disabled/reference-pending explanations and filter progress state.
5. Add separate full-evidence and filtered-view export choices with provenance.
6. Add automation/MCP inspection and control of the typed filter contract.

Gate:

- the menu, properties panel, session, automation, export, and diagnostics round-trip one canonical
  filter model;
- the UI cannot hide active filtering;
- highlight changes no evidence membership or summary;
- filter changes every alignment-derived consumer together;
- unsupported reference-dependent predicates are explicit; and
- multi-track and multi-panel filters remain isolated unless the user explicitly applies them
  together.

Manual checkpoint C — user only:

- filter and highlight substitutions, insertions, deletions, soft clips, flags, MAPQ, and tags;
- compare shown/eligible counts and coverage/Sashimi changes;
- pan and zoom with intrinsic and anchored filters and confirm their meaning remains stable;
- clear filters and confirm the complete stack returns without a blank transition; and
- try the same operations independently in two tracks and two panels.

### Phase 5 — mate-panel scrollbar qualification

1. Add a deterministic state-level regression that opens a selected mate in a new panel and proves
   that panel receives exact row authority and an independent scroll extent.
2. Cover a fitting mate locus and an overflowing mate locus.
3. Cover wheel scrolling, thumb dragging, density adjustment, panel switching, source settlement,
   and closing/reopening panels at the state/authority level.
4. Add focused trace fields for a missing or provisional target-panel scrollbar without enumerating
   the scene.

Automated tests may prove state and authority, but they must not claim visual acceptance.

Manual checkpoint D — user only:

- select a paired read and open its mate in another panel;
- confirm the second panel's scrollbar rail, thumb, density overview, wheel input, and dragging;
- switch repeatedly between panels and verify their vertical cameras remain independent; and
- repeat after sorting, filtering, and changing read density.

### Phase 6 — integrated qualification

Run focused and workspace-wide tests, then ask the user to perform one combined real-data pass:

- RNA ACTB and BAP1 for dense navigation, substitutions, indels, splices, Sashimi, and filtering;
- exome/WGS for ordinary paired reads, MAPQ, variants, and mate panels;
- long-read/supplementary data for clipping, chain actions, and event filtering;
- modified-base and specialist fixtures for interaction/colour compatibility;
- one and many tracks and panels; and
- low/default/high cache budgets under rapid action changes.

Remote BAM/CRAM source qualification remains in unified-scene Phase 10. The context, predicate,
session, and presentation contracts must be backend-neutral, but this plan does not silently claim
remote qualification.

## Validation strategy

### Predicate correctness

- CIGAR operations at record and tile boundaries;
- insertions anchored between bases;
- deletions and skips spanning tile boundaries;
- explicit `X`, ambiguous `M`, missing sequence, missing qualities, and missing reference;
- reverse-strand base interpretation;
- long reads and partial reference availability;
- true duplicate records with distinct stable handles;
- flags, pairs, MAPQ, template length, and typed auxiliary tags; and
- arbitrary chunk splits and predicate application order.

### Architectural operation counters

- appearance: zero source acquisition and zero row-atlas publication;
- arrangement: zero source acquisition when required payload is resident;
- filter reset: zero source acquisition when its superset evidence is retained;
- anchored filter: no semantic change from ordinary pan;
- highlight: zero evidence, row, or summary replacement;
- camera movement: no unchanged geometry upload; and
- stale/cancelled context or filter actions: zero publication.

### Interaction and accessibility

- background/read/component hit scope;
- control-click, secondary click, keyboard invocation, focus order, and screen-reader labels;
- popup placement at window and panel edges;
- menu state during Pending/Unavailable capability states;
- no conflict with pan, click suppression, scrollbar, density drag, or inspector activation;
- multi-track explicit scope; and
- session restoration of appearance, arrangement, anchors, filters, and chips.

### Native visual acceptance

The user performs every visual checkpoint. Automated input driving, screenshots captured without
the user's assessment, passing renderer tests, or a successful launch cannot mark the visual gates
accepted.

## Stop conditions

Stop and revisit the design rather than adding a workaround if:

- a context action needs a query-local record index;
- a full-canvas menu widget changes left-drag or scroll behavior;
- colour or hover requires row repacking or BAM acquisition;
- sorting/grouping rereads resident evidence;
- clearing a view filter discards or reacquires evidence that is still retained;
- a filter can publish reads without matching summaries, picking, inspection, and export;
- a substitution predicate guesses when reference coverage is incomplete;
- viewport movement silently changes an unanchored filter's meaning;
- fact indexes, selections, glyph frames, or scratch allocations bypass the ledger;
- a stale menu action can target a newer read at the same screen position;
- the mate panel borrows primary-panel row or scrollbar authority; or
- any visual gate is marked accepted without the user's review.

## Definition of done

This work is complete when:

- reads are neutral grey by default, blue on hover/selection, and biologically legible in all
  supported modes;
- base substitutions regain stable A/C/G/T/N glyphs at an appropriate zoom and row height;
- right-clicking the alignment canvas presents relevant, responsive track and read actions;
- existing sort/group/colour controls are available directly without duplicating their state;
- typed indel, substitution, flag, quality, mate, and tag filters work with exact visible counts;
- filtering is non-destructive, clearing retained filters requires no source I/O, and derived lanes
  remain coherent;
- active filters are always visible and persist through sessions;
- context actions use stable handles and survive or reject navigation safely;
- the mate panel owns a working independent vertical scrollbar;
- dense ACTB browsing retains the stability accepted at `5a28d90`; and
- all automated gates pass and the user accepts each native visual checkpoint.
