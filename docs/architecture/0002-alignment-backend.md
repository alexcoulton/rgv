# ADR 0002: Initial alignment backend

- Status: accepted
- Date: 2026-08-21

## Decision

RGV uses `rust-htslib` as its first BAM and CRAM backend behind an RGV-owned data-source boundary. A bounded, shared HTSlib decompression pool is used rather than allocating a decompression pool per track.

`noodles` remains in the alignment benchmark as an independent differential oracle and as a candidate remote backend.

**Amended 21 November 2026.** The decision above describes the first backend, not the code as it
stands. Today `rust-htslib` decodes local BAM only. Local CRAM, remote BAM and remote CRAM are
decoded by `noodles` (ADR 0004), and since this date `HTSlib` no longer opens any URL: a remote
object whose URL does not end in `.bam`/`.cram` is identified by its magic bytes and read by the
same `noodles` range path. `rust-htslib` thread pools cannot be shared between readers, so the
"shared pool" is a process-wide allowance of decompression threads that open sources lease from
(HTS-12). See "Backend convergence" below for where this is heading.

## Evidence

The repository benchmark shows the best tested local performance from multithreaded `rust-htslib`, full digest parity across the benchmark corpus, and substantially different remote request/byte tradeoffs between HTSlib and the custom `noodles` implementation.

See [`../../benchmark-results/RESULTS.md`](../../benchmark-results/RESULTS.md).

## Backend convergence (HTS-15, 21 November 2026)

### What is shared now

Both decoders implement one record view (`BackendRecord` in
`crates/rgv-hts/src/backend_record.rs`: flags, placement, MAPQ, CIGAR, bases, qualities, auxiliary
lookup, mate fields). Record normalisation, eligibility and filter evaluation, pileup/depth/allele
derivation, count-once mate-overlap handling, the one-shot regional fold and the positioned-stream
admission rules are generic over that view and exist once. Modification, junction and pair
summaries already worked on normalised records. `local.rs` and `remote_alignment.rs` keep only
their I/O front ends. A differential suite (`remote_alignment/tests/differential.rs`) decodes the
same BAM bytes through both backends, and the same records as CRAM through both CRAM decoders,
and requires identical normalised records, statistics, pileups, junctions, pairs, modification
summaries, block identities and source positions.

Known remaining differences, all pinned by that suite or noted here:

- `rust-htslib` 1.0.1 decodes an `H` (hex) auxiliary field as a string, so the `HTSlib` path cannot
  tell `H` from `Z`; `noodles` reports the stored type. Not fixable without `unsafe`.
- `noodles` has no value type for the non-standard `d` (double) auxiliary field. Not exercised.
- `HTSlib`'s CRAM decoder regenerates `MD`/`NM` that its encoder dropped (the private `cF` field
  records which); the `noodles` CRAM path hides `cF` but does not regenerate them. Read from the
  `HTSlib` source, not yet covered by a fixture.
- A cancelled one-shot query returns partial statistics from the `HTSlib` front end and an empty
  cancelled query from the `noodles` front ends. Deliberately left as it was.

### Measurement

`cargo run --release -p rgv-alignment-bench -- compare-backends benchmark-data/mixed.bam
--iterations 15` (`benchmarks/alignment-io/src/backend_compare.rs`). Local file, warm page cache,
both libraries inflating with libdeflate. "Layout" touches flags, placement, name and CIGAR;
"full" additionally materialises every base, quality and auxiliary value, which is what
`QueryDetail::Full` normalisation reads. A timing is accepted only when both backends produce the
same digest. `-1t` is the plain BGZF reader; `-4t` is `HTSlib` with four decode threads; `-mt4` is
`noodles`' `bgzf::io::MultithreadedReader` with four workers.

Machine: Apple M4 (10 cores), 16 GB, macOS 15.5, rustc 1.93.1, `lto = "thin"`. Other builds were
running on the machine during both runs (load average 5 to 15), so the table gives the fastest of
15 decode loops, best of two runs, in milliseconds; medians were within about 10% of these except
where a multithreaded sample was disturbed. Decode loop only: open, header and index load are
excluded (they add 0.1 to 0.4 ms for either library on this file).

| Workload (synthetic `mixed.bam`, 19.7 MB) | Detail | htslib-1t | noodles-1t | htslib-4t | noodles-mt4 |
|---|---|---:|---:|---:|---:|
| deep-hotspot: 151,259 reads of 150 bp, one locus | layout | 29.5 | 31.2 | 7.4 | 9.9 |
| | full | 44.6 | 44.4 | 21.4 | 19.7 |
| broad-chr1: 270,000 reads of 150 bp over 2 Mb | layout | 54.5 | 52.7 | 15.8 | 21.4 |
| | full | 81.9 | 74.8 | 39.5 | 32.7 |
| long-modified-split: 2,000 reads, mostly 11.5 kb with `MM`/`ML`/`SA` | layout | 18.0 | 18.8 | 5.5 | 6.1 |
| | full | 26.6 | 28.0 | 9.9 | 10.3 |
| sequential-all: 282,017 records, no index | layout | 74.3 | 69.8 | 20.8 | 20.7 |
| | full | 110.9 | 101.4 | 49.0 | 38.5 |

Reading: single-threaded, the two decoders are within 9% of each other on every workload
(`noodles` between 0.91x and 1.06x the `HTSlib` time). With four inflate threads `HTSlib` is ahead on
layout-only regional queries (`noodles` 1.1x to 1.35x its time) and `noodles` is level or ahead at
full detail (0.79x to 1.04x). For short reads full-detail decode runs at about 3.3 to 3.6 million
records (about 0.5 Gbase) per second on one thread for either library; for the long-read workload,
about 70,000 to 75,000 records (0.6 to 0.67 Gbase) per second.

Limits of this measurement: one synthetic corpus written by `HTSlib`; no real Illumina, ONT or
PacBio file; no CRAM; a shared machine. It measures decode and field access, not RGV's
normalisation, packing or pileup (those are now the same code for both backends, so they do not
separate the two). It does not measure the production remote path, where the `noodles` BGZF reader
is single-threaded.

### Recommendation

Converge on `noodles` for alignments, in two steps, and do not remove `HTSlib` in the step that
introduces the replacement.

1. Add a local-file `noodles` BAM front end (BGZF `MultithreadedReader` over the file, BAI/CSI
   through the existing index code, the existing positioned-stream and resume machinery made
   generic over the reader) behind `ProductionAlignmentSource`, selectable per open. Run the
   differential suite and `compare-backends` on real short-read and long-read BAMs. Decide the
   default on those numbers.
2. If local BAM through `noodles` holds within about 10% of `HTSlib` at four threads on real data,
   make it the default and keep `HTSlib` as a test oracle (a dev-dependency of the differential
   suite and the fixture writers) for at least one release.

What it gains: one decoder to fix and qualify instead of two; request, byte and cancellation
accounting for every alignment read; the same resume behaviour everywhere (a `noodles` stream
recognises a held chunk before reading it, an `HTSlib` stream only after reading one record of it);
no hand-off of URLs to libcurl. Build-wise, the alignment path would stop needing `hts-sys`
(HTSlib compiled from C by the `cc` crate) and, through its `curl` feature, `curl-sys` and
`openssl-sys`. Those are what stand between RGV and a plain `cargo build` on Windows MSVC, static
musl binaries and old-glibc AppImages. A C compiler is still needed for as long as `noodles-bgzf`
is built with its `libdeflate` feature (HTS-5), which these numbers were measured with; a build
with no C at all, WASM included, means choosing a pure-Rust inflater and measuring again.

What it costs and what does not come for free:

- The C dependency does not disappear with the alignment backend. `rust-htslib` is also used for
  local FASTA (`faidx`), local VCF/BCF (`bcf`), local tabix text tracks (`tbx`), index building and
  every BAM/CRAM fixture writer. Each needs a `noodles` (or RGV-owned) replacement before the crate
  can leave the dependency graph; until then only the `curl` feature can go.
- Threads: `HTSlib` leases decode threads from one process-wide allowance. `noodles`'
  `MultithreadedReader` starts its own workers per reader and needs `R: Send + 'static`; a shared
  pool would have to be built on its block-level API. The remote path stays single-threaded
  unless the range reader is made to feed such a pool.
- CRAM: `noodles` is already the production CRAM decoder, without `MD`/`NM` regeneration.
  `HTSlib` is the reference CRAM implementation and should stay the oracle for it.
- `H` and `d` auxiliary types and the cancellation shape listed above have to be settled as
  behaviour, not as backend accidents.

Converging on `HTSlib` instead is not recommended: it would put remote reads back behind libcurl
(no request accounting, budgets, authentication policy or cancellation, which is why ADR 0004
exists), keeps every build problem above, and its region iterator cannot seek past held blocks
without reading into them.

Naming debt outside this crate: `LocalBamCanonicalBackend` in
`crates/rgv-engine/src/local_bam_source.rs` drives `ProductionAlignmentSource`, which is local or
remote, BAM or CRAM; the name is stale.
