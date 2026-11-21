# Alignment I/O benchmark

This benchmark compares the candidate BAM decoding and indexed-query paths for RGV:

- `noodles` 0.115 with `noodles-bgzf`'s `libdeflate` feature;
- `rust-htslib` 1.0.1, using reusable records and `libdeflate`;
- `rsomics-bamio` 0.8.4, using its borrowed raw-record scanner for sequential BAM and its
  format-independent indexed reader for regional queries.

It is a headless application benchmark rather than a parser microbenchmark. Every measured pass
opens the input, reads the header, executes the query, decodes the fields required by the workload,
and computes a backend-independent digest.

## Corpus

`generate` creates a deterministic, coordinate-sorted BAM containing 282,011 records and a BAI:

| Records | Shape exercised |
|---:|---|
| 120,000 | Distributed 150 bp reads on `chr1` |
| 150,000 | Ultra-deep 150 bp hotspot |
| 10,000 | Spliced `75M1000N75M` reads |
| 1,500 | 11,550 bp reads with clipping, insertion, deletion, `MM`, `ML`, `HP`, and SBX quality truth |
| 500 | Supplementary reads with `SA` |
| 10 | Paired structural-variant orientation, size, and inter-contig cases |
| 1 | 70,000-operation BAM long CIGAR using the standard `CG:B,I` representation |

The complete scan covers 59,896,500 sequence bases, 381,510 expanded CIGAR operations, and
569,522 selected tag occurrences. The generated BAM is approximately 19.3 MB. It is not intended
to represent a biological sample; it is a controlled stress corpus for record shapes a viewer must
handle.

Long-read qualities include deterministic 25-base left and 30-base right simplex tails. Three
forward records exercise fully discordant, partly discordant, and concordant evidence for the
50-base insertion, while the remaining long-read insertions retain high-quality evidence.

`generate-variants` creates the separate compact indexed `variants.bam` plus `variants.fa` visual
truth set. In addition to named SNV and small/large indel reads, its Ultima read group has
`PL:ULTIMA` and `mc:3`; `chrVariants:13991-15120` contains exact directional-`tp` insertion Q40,
maximum-homopolymer deletion Q0, and `t0` single-deletion Q35 records. The generator test reopens
that BAM/FASTA through RGV's production normalized query path and asserts all three evidence
sources.

## Correctness gate

Timings are accepted only when all backends agree on this digest:

- record count;
- read-name FNV-1a hash and flag sum;
- sequence bases;
- expanded CIGAR operation count and reference-consuming bases;
- occurrences of `RG`, `NM`, `MM`, `ML`, `HP`, and `SA`.

This catches region-boundary differences, skipped records, unexpanded long CIGARs, and missing
auxiliary fields. The gate is run before measurement and after every measured iteration.

## Workloads

| Workload | Purpose |
|---|---|
| `sequential-all` | Full detailed scan of all record shapes |
| `broad-chr1` | Large indexed query, core fields only |
| `deep-hotspot` | Detailed indexed query returning 151,259 reads |
| `spliced` | Detailed RNA-style CIGAR traversal |
| `long-modified-split` | Long sequences and modification/supplementary tags |
| `long-cigar` | Expansion and traversal of a 70,000-operation CIGAR |
| `remote-deep-hotspot` | Cold HTTP access with an in-process Range server |

Single-threaded paths are measured for all libraries. HTSlib is also measured with four decode
threads. `rsomics-bamio`'s four-worker BGZF reader is measured for the sequential workload; its
indexed abstraction does not expose the same worker configuration.

The remote server injects the configured latency into each request. The `noodles` remote adapter is
a deliberately simple seekable HTTP reader with one 256 KiB cache window. HTSlib uses its native
libcurl path. `rsomics-bamio` currently accepts filesystem paths and is therefore recorded as
unsupported rather than assigned a misleading timing.

`remote_bytes` in the JSON is the sum of response body ranges selected by the server. In HTSlib's
case the initial no-Range probe may be abandoned after the response headers, so this is an upper
bound on bytes actually received by the client.

## Reproduce

```sh
cargo run --release -p rgv-alignment-bench -- generate
cargo run --release -p rgv-alignment-bench -- \
  run --iterations 5 --remote-latency-ms 2 \
  --output benchmark-results/latest.json
```

The remote workload binds a loopback TCP port. In a restricted execution environment, permission
to use loopback networking may be required.

Raw samples and digests are retained in `benchmark-results/latest.json`. Medians are reported
because first-pass scheduling and filesystem noise can create outliers even after the correctness
warm-up.

## Backend comparison (HTS-15)

```sh
cargo run --release -p rgv-alignment-bench -- \
  compare-backends benchmark-data/mixed.bam --iterations 15 \
  --output benchmark-results/backend-compare.json
```

`compare-backends` measures only the two production decoders, `rust-htslib` and `noodles`, on a
local BAM: one inflate thread each, `HTSlib` with four decode threads, and `noodles` with its
`bgzf::io::MultithreadedReader` and four workers. Each runs a layout-level traversal (flags,
placement, name, CIGAR) and a full-detail one that materialises every base, quality and auxiliary
value, over a short-read deep locus, a short-read 2 Mb region, the long-read region and the whole
file. It reports the decode loop separately from open/header/index time, with the median and the
fastest sample, and rejects a timing whose digest differs between backends. The results and what
they mean for the backend decision are in `docs/architecture/0002-alignment-backend.md`.

The `rust-htslib-http` workload of `run` is the only code in the workspace that lets `HTSlib` open
a URL, which is why this crate asks for the `curl` feature itself.

## Scope limitations

- The corpus is synthetic and generated through HTSlib. Digest parity reduces format-bias risk but
  does not replace testing on real Illumina, ONT, PacBio, RNA, bisulfite, and single-cell BAMs.
- This benchmark covers BAM, not CRAM reference resolution.
- It measures I/O and record traversal, not read packing, coverage aggregation, or GPU rendering.
- The remote test controls request latency but does not impose a bandwidth limit or packet loss.
- The one-record long-CIGAR workload is primarily a correctness and fixed-overhead check; its
  throughput figure should not be extrapolated.
