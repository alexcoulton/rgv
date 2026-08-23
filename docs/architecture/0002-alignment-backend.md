# ADR 0002: Initial alignment backend

- Status: accepted
- Date: 2026-08-21

## Decision

RGV uses `rust-htslib` as its first BAM and CRAM backend behind an RGV-owned data-source boundary. A bounded, shared HTSlib decompression pool is used rather than allocating a decompression pool per track.

`noodles` remains in the alignment benchmark as an independent differential oracle and as a candidate remote backend.

## Evidence

The repository benchmark shows the best tested local performance from multithreaded `rust-htslib`, full digest parity across the benchmark corpus, and substantially different remote request/byte tradeoffs between HTSlib and the custom `noodles` implementation.

See [`../../benchmark-results/RESULTS.md`](../../benchmark-results/RESULTS.md).
