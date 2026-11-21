# ADR 0004: remote backends and byte accountability

Status: accepted for beta; amended 20 November 2026 (local CRAM backend, see Decision).

## Context

RGV needs indexed HTTP/S reads without blocking the UI thread. The alignment benchmark initially
showed that HTSlib gave the best end-to-end BAM latency, while the custom noodles experiment
transferred fewer bytes. The production pure-Rust reader subsequently met the deep-locus latency
budget while making request and byte accounting directly observable. BigWig and BigBed also need a
seekable reader that the pure-Rust `bigtools` library can consume.

## Decision

- Use `noodles` over RGV's `HttpRangeReader` for remote BAM and CRAM. BAM supports BAI/CSI and CRAM
  supports CRAI, including explicit URLs and both conventional adjacent-name forms. CRAM reference
  decoding uses an explicitly supplied local or remote indexed FASTA; its per-query decoded
  reference footprint is capped and released after the query. Both formats share normalized
  alignment, pileup, junction, pair, filtering, sampling, and generation-routing code.
- Keep local BAM behind `rust-htslib`, retaining its mature local codec behavior while making
  remote traffic fully observable in the pure-Rust path.
- **Amended 20 November 2026.** The original decision also kept local CRAM behind `rust-htslib`.
  That is no longer what the code does: `ProductionAlignmentSource::open` in
  `crates/rgv-hts/src/remote_alignment.rs` routes a local `.cram` path to
  `LocalCramAlignmentSource`, which uses the `noodles` indexed CRAM reader and the same positioned
  container/slice decoder as remote CRAM. Only local BAM (and any other local non-CRAM path) goes
  through `rust-htslib`. The doc comment on `ProductionAlignmentSource` still says "Local files
  and CRAM use `HTSlib`" and is stale. RGV therefore ships two production alignment decoders;
  converging on one is tracked as HTS-15 / OPS-10 in
  `audits/COMPREHENSIVE_REVIEW_2026-11-20.md`.
- Keep all callers behind RGV source traits so backend selection does not affect layout or
  rendering.
- Use RGV's `HttpRangeReader` for BigWig and BigBed. It performs bounded chunk coalescing, maintains
  a disk-backed LRU, validates ETag or Last-Modified with `If-Range`, retries transient failures,
  rejects insecure or unbounded redirects, cooperatively cancels obsolete generation reads, and
  reports requests (including redirect hops), response bytes, cache activity, eviction, retry, and
  cancellation counts.
- Accept bearer or caller-selected header authentication only at runtime. Authentication values,
  URL user-info, and signed queries are excluded from `Debug`, UI labels, saved settings, and
  sessions.

## Consequences

The production remote BAM/CRAM paths have deterministic in-process request, transfer, cache, index,
and reference budgets under injected latency and loss. A CRAM decoder can require a complete
reference contig; RGV therefore permits at most 512 MiB of reference bases per CRAM query and clears
the decoder repository after every result or failure. Local/remote normalization parity remains
covered by differential fixtures. Broad WAN, real-data, and matched-IGV qualification remains the
Phase 8 work described in `benchmarks/IGV_COMPARISON.md`.
