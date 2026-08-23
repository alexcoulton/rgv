# ADR 0003: Coordinates and viewport cancellation

- Status: accepted
- Date: 2026-08-21

## Decision

All internal intervals are validated, zero-based, and half-open. Backend-specific coordinate systems are converted only at I/O boundaries using checked conversions.

Every viewport mutation advances a generation clock. Work and results carry the generation for which they were created. Workers reject stale jobs before processing and the engine rejects results that became stale during processing.

## Consequences

- Coordinate conventions are consistent across formats and renderers.
- Rapid navigation cannot allow an old result to replace a newer viewport.
- Long operations must be chunked so cancellation is observed at bounded intervals.
