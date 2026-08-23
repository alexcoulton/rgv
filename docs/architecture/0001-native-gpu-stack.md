# ADR 0001: Native GPU and UI stack

- Status: accepted
- Date: 2026-08-21

## Decision

RGV uses `winit` for native windows and input, `wgpu` for the genomic canvas, and `egui` for application chrome. RGV renders genomic primitives through instanced custom pipelines rather than representing individual reads as UI widgets.

The initial compatible versions are `winit` 0.30.13, `wgpu` 29, and `egui` 0.35. `egui` 0.36 requires Rust 1.95 and is therefore incompatible with the workspace's Rust 1.93 minimum.

## Consequences

- Rendering work can be batched and bounded independently from the UI tree.
- The application owns the integration between the event loop, UI, and renderer.
- API upgrades across these crates must be performed together and tested on every desktop platform.
