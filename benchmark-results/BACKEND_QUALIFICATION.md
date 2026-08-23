# Controlled backend qualification

Status date: 22 August 2026

Status: **controlled automated matrix passed; broad real-data and WAN qualification remains**.

This record covers deterministic fixtures and the generated RGV corpus. It does not claim that the
private Illumina, ONT, PacBio, RNA, amplicon, bisulfite, SBX, Ultima, single-cell, deep-coverage,
SV-heavy, and reference-dependent CRAM release corpus has passed. That corpus and hands-on review
remain part of the Phase 9 release gate.

## Matrix exercised

| Source | Index/reference combinations | Failure and recovery evidence |
| --- | --- | --- |
| Local BAM | Conventional BAI and explicit CSI | Missing index, corrupt BAI, corrupt BAM, and truncated BAM are rejected |
| Local CRAM | Conventional CRAI with explicit FASTA/FAI | Normalized query parity is checked; a mismatched reference is rejected by checksum |
| Remote BAM | `sample.bam.bai`, `sample.bai`, `sample.bam.csi`, `sample.csi`, custom BAI, and custom CSI | Corrupt index, signed-query preservation, bounded transfer, and local/remote pileup, pair, and junction parity |
| Remote CRAM | `sample.cram.crai`, `sample.crai`, and custom CRAI; local or authenticated remote FASTA/FAI | Truncated source and malformed/out-of-object CRAI are rejected; local/remote record, CIGAR, pileup, pair, and junction parity |
| HTTP range layer | Authentication, same-origin redirect, retry, exact range geometry, validators, coalescing, and warm cache reuse | Short body, changed validator, cancellation before and during a request, bounded LRU eviction, and successful reopen after object replacement |

The tests also verify that source/index/reference signed queries remain separate, URL credentials
are redacted from diagnostics, redirects are counted, transient probe and data failures are retried,
and every remote counter includes requests, bytes, hits, misses, evictions, retries, and
cancellations.

## Gate results

The following checks passed on the development Apple Silicon macOS host:

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets --quiet        398 tests passed
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p rgv-desktop
cargo run --release -p rgv-validation-runner -- validation/synthetic-corpus.json
cargo run --release -p rgv-remote-range-bench -- --output benchmark-results/remote-range-latest.json
```

The simulated 20 ms-latency range run used three requests including one injected transient
failure, retried once, transferred 786,433 successful bytes, completed its range-only work in
99.06 ms, and added no request for the warm reread. The deep remote BAM query used 10 requests,
transferred 14,684,514 bytes, completed in 383.86 ms, retained 40,009 of 151,259 records, and added
no request for the identical warm query. Exact machine-readable values are in
`remote-range-latest.json`.

## Findings fixed during qualification

1. Capacity eviction happened inside the LRU, but RGV used an insertion API that did not return the
   evicted entry. The counter therefore remained zero and its disk file was not removed. The range
   cache now captures the evicted block, deletes the file, and reports the eviction.
2. A severely truncated indexed BAM could make `rust-htslib` return a reader with a null header and
   then panic when RGV asked for contigs. RGV now validates the header pointer first and returns the
   typed `InvalidAlignmentHeader` error.

## Still required for release qualification

- Run the pinned private real-data manifest with `--require-complete-matrix`.
- Exercise controlled real WAN/proxy behavior, including bandwidth limitation and longer outages.
- Repeat the matrix with the exact signed release artifacts on every advertised platform.
- Complete the broad manual product and visual review before declaring backend qualification
  passed for beta.
