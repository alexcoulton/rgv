# ADR 0007: One immutable alignment-scene authority

Status: accepted, 29 August 2026

## Context

RGV's stable alignment path decodes complete evidence correctly, but its presentation authority is
split across exact viewport queries, a mutable resident row scene, screen-space display lists,
query-local picking identities, GPU rectangle slabs, and navigation textures. Attempts to retain
some of those authorities while adding world-coordinate pages produced blank panels, release
flicker, missing reads, and broken interaction.

The product contract requires complete rather than sampled read evidence, continuous camera-like
navigation over prepared data, correct interaction, and a bounded user-controlled cache.

## Decision

Create the headless `rgv-alignment-scene` crate and make one immutable scene graph the eventual
authority for alignment evidence, rows, semantic geometry, picking, selection, inspection, derived
lanes, automation, and export.

The implementation follows these boundaries:

- `rgv-hts` captures physical source positions and retains proven decode/biological algorithms.
- `rgv-alignment-scene` owns factorised contracts, source-revision handles, completeness proofs,
  immutable evidence/row/presentation roots, semantic pages, and resource metadata.
- `rgv-engine` owns each mutable source actor, scheduling, the canonical scene store, and atomic
  publication.
- `rgv-render` projects the same semantic scene through CPU and GPU implementations.
- `rgv-desktop` owns cameras and UI orchestration, not alignment evidence or geometry identity.

Canonical source chunks own records. Genomic tiles and row blocks only reference stable handles.
GPU allocations are disposable derivatives. Published roots have no interior mutation. Current and
candidate roots coexist only through charged leases and publish atomically.

The replacement is developed and qualified headlessly. Desktop alignment consumers cut over as a
single semantic authority. The old renderer, picker, query-local selection, whole-panel texture,
and CompleteHorizon path are then deleted; no production fallback can restore them.

## Consequences

- The current `AlignmentQuery` remains an oracle during development, not the new cache unit.
- BAM virtual offsets and CRAM container/slice/record positions must be proven before scene
  ingestion is considered complete.
- A CPU reference projector is required before the GPU implementation is qualified.
- Resource limits charge live allocation capacity, not cache-key estimates.
- Unsupported alignment modes block cutover instead of invoking the legacy path.
- Visual behavior is manually assessed by the user at the first native MVP and at the exclusive
  renderer gate; automated GUI control is not used as visual approval.

The detailed phases, gates, stop conditions, and deletion map are maintained in
`plans/UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md`.
