# Alignment backend results

Benchmark completed on 20 August 2026 using an Apple M4 MacBook Pro with 10 cores and 16 GB RAM,
macOS 15.5, and Rust 1.93.1. Values are medians of five optimized runs after a checksum-verifying
warm-up. Lower time is better.

## Local BAM

| Workload | Records | `noodles` 1t | `rust-htslib` 1t | `rsomics-bamio` 1t | `rust-htslib` 4t | `rsomics` 4t |
|---|---:|---:|---:|---:|---:|---:|
| Sequential all | 282,001 | 122.848 ms | 106.912 ms | 98.450 ms | **36.613 ms** | 57.052 ms |
| Broad chr1 | 270,000 | 91.453 ms | 77.805 ms | 130.994 ms | **30.966 ms** | — |
| Deep hotspot | 151,259 | 65.445 ms | 57.530 ms | 92.154 ms | **19.522 ms** | — |
| Spliced | 10,000 | 5.735 ms | 5.506 ms | 4.963 ms | **2.481 ms** | — |
| Long/modified/split | 2,000 | 30.048 ms | 24.190 ms | 26.512 ms | **8.947 ms** | — |
| 70,000-op long CIGAR | 1 | 0.544 ms | **0.451 ms** | 0.877 ms | 0.477 ms | — |

The four-thread HTSlib path was 2.2–2.9× faster than its single-thread path across the multi-record
workloads and was the fastest backend in every multi-record workload. Among single-thread paths,
`rsomics-bamio` led the sequential and spliced scans, while HTSlib led the broad, deep, long-read,
and long-CIGAR queries.

## Simulated remote BAM

The deep-hotspot query used a cold local HTTP server with 2 ms latency applied per request.

| Backend | Median | Requests | Response body bytes selected |
|---|---:|---:|---:|
| `noodles` + 256 KiB range cache | 228.550 ms | 38 | 9,441,305 |
| HTSlib/libcurl | **60.448 ms** | 2 | up to 35,669,792 |
| `rsomics-bamio` | unsupported | — | — |

HTSlib won this latency-oriented test by coalescing access into two requests. Its server-selected
body size was much larger, so the result must not be generalized to bandwidth-constrained links.
RGV's production remote reader should combine HTSlib-level request coalescing with an explicit
bounded byte-range cache and be retested under bandwidth limits.

## Decision

Use `rust-htslib` as the initial BAM/CRAM backend, with a bounded shared HTSlib thread pool rather
than one pool per track. Keep the application-facing `AlignmentSource` interface independent of
HTSlib so a pure-Rust `noodles` backend remains possible and can serve as a differential-correctness
oracle.

Do not adopt `rsomics-bamio` as the primary indexed backend yet. Its borrowed sequential scanner is
promising, but its indexed path did not outperform HTSlib here and it lacks a URL input abstraction.
Revisit it if its borrowed record API becomes available for indexed chunks.

Raw measurements, including every iteration and digest, are in [latest.json](latest.json).
