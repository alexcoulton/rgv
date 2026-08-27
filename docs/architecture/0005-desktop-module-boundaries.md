# ADR 0005: Desktop module and state boundaries

Date: 2026-08-23

## Status

Accepted

## Context

The desktop application accumulated launch, runtime, workspace, query, display, inspection,
automation, UI, and GPU-submission responsibilities in `main.rs`. Primary alignment state was also
mirrored between the application, panel, and alignment controller, requiring synchronization.

## Decision

- Preserve the workspace dependency direction: core → HTS → engine/layout → render → desktop.
- Keep `main.rs` as startup/error reporting only; the desktop library is the composition root.
- Keep persisted alignment configuration canonical in `AlignmentTrackController.config` and keep
  panel query/layout data canonical in `AlignmentPanelProjection`.
- Require explicit `PanelWorkspace::active()`/`active_mut()` access instead of implicit dereference.
- Keep source-path and URL classification in `rgv-core`; desktop code maps classified formats to
  typed engine specifications and must not add another extension table.
- Put genomic layout algorithms below desktop. Desktop display modules schedule requests, construct
  cache keys, compose panels, and translate hit-test results.
- UI widget families edit focused drafts or return typed actions. Application dispatchers own
  workspace mutation, engine/file work, invalidation, and selection changes.
- Keep the remaining egui shell composition as one borrow-scoped frame transaction. It is larger
  than normal modules because its local drafts must be committed atomically after `Context::run`;
  dialogs, controls, diagnostics, action dispatch, and GPU submission remain separate modules.

## Consequences

New formats extend the core classifier and one typed engine mapping. New track behavior extends its
controller, projection, layout family, and focused UI action path without adding primary-track
special cases. Session JSON and existing crate APIs remain compatible because the refactor changes
internal ownership and file boundaries rather than the persisted schema.

Module movement and state changes must pass formatting, Clippy, workspace tests, manual-fixture
qualification, and the interaction benchmark described in `plans/MODULARISATION_PLAN.md`.
