# ADR 0008: Data model and automation protocol crates

Status: accepted, 21 November 2026

## Context

Review item ARCH-8 found two layering faults.

- The record types every consumer needs lived in the I/O crate `rgv-hts`. `rgv-layout`,
  `rgv-alignment-scene` and `rgv-render` therefore depended on `rgv-hts` and linked htslib (a C
  toolchain), `reqwest`, `noodles` and `bigtools`, although they only read plain data. That blocks
  a WASM target and makes the renderer need a C compiler.
- `rgv-core`, the domain crate (coordinates, session, settings), also carried the automation
  protocol and the interaction stress documents.

## Decision

Two crates were split out. Both are move-and-re-export refactors with no behaviour change.

```text
rgv-core                      domain: coordinates, session, settings, landmarks
├── rgv-model                 record/batch/summary/query value types, pure functions over them
│   ├── rgv-hts               decoders, indexes, HTTP; re-exports every rgv-model item
│   ├── rgv-layout
│   ├── rgv-alignment-scene
│   └── rgv-render            (also rgv-layout, rgv-alignment-scene)
├── rgv-automation            automation protocol, interaction stress documents
│   └── rgv-control-client, rgv-desktop, rgv-mcp, tools/interaction-stress, validation/runner
└── rgv-engine                rgv-hts + rgv-alignment-scene
```

### `rgv-model`

Depends on `rgv-core` and `thiserror` only. It holds:

- alignment records, packed batches, CIGAR, auxiliary fields, flags, header metadata and the
  overlap index (`alignment`), physical source positions and source stamps;
- the pure evidence algorithms over those types: pileup and count-once mate overlap, junctions,
  pairs, exact depth, modified bases and their summaries, bisulfite, SBX, SMRT kinetics, Ultima,
  supplementary chains, and the alignment filter;
- the query value types `QueryOptions`, `QueryDetail`, `QueryStats`, `IndividualReadStatus` and
  `AlignmentQuery`, and `ReferenceSlice`;
- the feature, variant, quantitative, GWAS, copy-number, mutation and interaction record, query
  and provenance types with their bounds.

`rgv-hts` re-exports every one of these from its original public path, so `use rgv_hts::X`
continues to compile. Its modules are public and a few decoder-facing hooks that were
`pub(crate)` in `rgv-hts` are now `#[doc(hidden)] pub`, because the decoders that drive them stay
in `rgv-hts`.

What stays in `rgv-hts`, and why:

- `AlignmentError` wraps `rust_htslib::errors::Error` and `RemoteError`. Everything whose
  signature returns it stays: the `AlignmentSource` trait, `derive_alignment_query*`,
  `AlignmentSummaryIndex`, and the `BackendRecord`-generic normalisation and pileup driver in
  `backend_record.rs` (which also names `noodles` and htslib record types).
- `AlignmentIndexOverview` is built from `noodles` index types.
- Source stamp derivation (`local_source_stamp`, `remote_source_stamp`) reads the filesystem and
  hashes with `sha2`; only the `AlignmentSourceStamp` and `AlignmentSourceFormat` value types
  moved.
- All parsers, readers, indexes and HTTP code.

`rgv-alignment-scene` keeps a dev-dependency on `rgv-hts` for one parity test that compares scene
allele summaries with `rgv_hts::derive_alignment_query`.

### `rgv-automation`

Depends on `rgv-core`, `serde`, `serde_json`, `thiserror`, `url` and optionally `schemars`. It
holds `automation.rs` and `interaction_stress.rs` unchanged. Its `schema` feature enables
`rgv-core/schema`. `rgv-core` does not depend on it and no longer re-exports those names;
consumers import them from `rgv_automation`.

Two items stay in `rgv-core` because the session code and the protocol both need them:

- `MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT` (session validation uses it); `rgv-automation`
  re-exports it;
- `validate_alignment_predicate_expression`, now `#[doc(hidden)] pub` so the protocol can call it.

`rgv-core` keeps `tempfile` (atomic session save), `quick-xml` (IGV session import) and `sha2`
(landmark catalogue digests); none of them was used only by the moved code.

## Enforcement

`crates/rgv-alignment-scene/tests/dependency_boundary.rs` resolves normal dependency graphs from
the workspace manifests and `Cargo.lock` and fails if `rgv-model`, `rgv-layout`,
`rgv-alignment-scene` or `rgv-render` reaches `rgv-hts`, htslib, `reqwest`, `noodles` or
`bigtools`, if `rgv-model` declares anything beyond `rgv-core`, `serde` and `thiserror`, or if
`rgv-core` or `rgv-model` reaches `rgv-automation`. `crates/rgv-model/src` is a production root of
the alignment-scene source inventory.

## Consequences

- The renderer, layout and scene crates build without a C toolchain or a network stack.
- A later change can give `rgv-model` its own error type so the source trait and resident-query
  derivation can follow the value types out of `rgv-hts`.
