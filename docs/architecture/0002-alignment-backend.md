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
(HTS-12). See "Backend convergence" below for where this is heading. Step 1 of that plan is now
in the tree: a local `.bam` can be opened through `noodles` on request
(`AlignmentOpenOptions::local_bam_backend`); `rust-htslib` remains the default.

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

### Step 1 as built (21 November 2026)

`LocalBamAlignmentSource` (`crates/rgv-hts/src/remote_alignment/local_bam.rs`) is the local-file
front end of the `noodles` BAM decoder. `ProductionAlignmentSource::open` returns it for a local
file named `.bam` when `AlignmentOpenOptions::local_bam_backend` is
`AlignmentBackendPreference::Noodles`. The default is `Htslib`, and nothing in RGV sets the option
yet.

- One decoder, three byte sources. The record iteration, the one-shot fold, the positioned
  stream and the resume logic are generic over a small BGZF-stream trait (`BamBlockStream`). The
  remote range reader, a local file and an in-memory BAM implement it; there is no second copy of
  any of that logic.
- BGZF: `noodles`' `MultithreadedReader` over the file, with as many workers as the source is
  granted from the same process-wide decompression allowance the `HTSlib` sources lease from
  (plus one thread that reads raw blocks while a read is in flight). A source granted no thread
  uses the plain single-threaded reader. The index is the BAI or CSI beside the file (or
  `index_path`), parsed by `noodles`; index overview, mapped-record totals, header metadata, the
  source stamp and the changed-file guard are the same values the `HTSlib` front end reports.
- The multithreaded reader reports a block it cannot read (an I/O error, a file cut short inside
  a block) as the end of the stream, and after a seek to the end of the file keeps serving its
  previous block. The front end checks its compressed position against the file length, so both
  cases are errors or a clean end as appropriate, not silently short or repeated data
  (`a_truncated_local_bam_is_an_error_not_a_short_result`).
- A positioned stream keeps the one record it reads ahead (the first record of the next owner
  block) instead of seeking back to read it again, so a stream that is simply continued never
  restarts the reader's workers. This applies to remote BAM as well.
- Two per-record costs of the `noodles` record view were removed for BAM, because they decided
  the comparison: bases, qualities and CIGAR are now read from the record's bytes instead of
  through the boxed per-element iterators of the generic alignment-record trait, and a one-shot
  query reuses one record buffer instead of allocating one per record. Before this, full-detail
  normalisation through `noodles` took about 2.5 times as long as through `HTSlib` (deep-hotspot
  full-detail stream: 125 ms against 51 ms). Remote BAM gets the same saving.

The differential suite now asserts the same fixtures identical across `HTSlib`-local,
`noodles`-local without BGZF workers (the plain reader) and with them (the multithreaded reader),
and `noodles`-remote: the edge-case fixture over the whole option matrix (one-shot queries and
positioned streams, with whole and half admission), `mixed.bam` and `variants.bam`, the resume
scenarios, and a positioned stream interleaved with one-shot queries. The two `noodles` front ends are additionally required to be equal as whole
values. **No divergence was found** beyond the `H`-type difference already listed.

Behaviour that differs by front end, in addition to the list above:

- Cancellation. A cancelled one-shot query through `noodles` is an empty cancelled query (as for
  remote sources). A `noodles` positioned-stream checkpoint is one BGZF block of work and does not
  consult `keep_going` inside it; it never returns `Cancelled`. The `HTSlib` front end checks
  between records.
- Resume receipts. `noodles` receipts carry the chunk's start offset and are honoured before any
  byte is read; a receipt issued by the `HTSlib` front end has no start offset and is ignored by a
  `noodles` stream (the chunk is read once more). Receipts are not persisted, so this only matters
  if one process mixes the two for one file.
- Headers. The `noodles` front ends parse the SAM header with `noodles`' parser, which is stricter
  than `HTSlib`'s; a header it rejects fails the open.

#### Measurement of RGV's own path

`cargo run --release -p rgv-alignment-bench -- compare-backends benchmark-data/mixed.bam
--iterations 15`, production section: `ProductionAlignmentSource` opened once per backend on the
local file (open excluded), then the same regional request 15 times, as a one-shot `query` and as
a positioned `stream` drained to completion. This is the real path: index lookup, BGZF, record
decode, eligibility, normalisation and packing, and for the one-shot query the pileup and summary
derivation. "Layout" is `QueryOptions::default()` with the record limit raised; "full" is
`QueryDetail::Full` with allele and modification pileups. A timing is accepted only when the
query result and the stream's blocks are equal across all four configurations. `1t`/`4t` is the
number of decompression threads requested (and granted) per source.

Machine: Apple M4 (10 cores), 16 GB, macOS 15.5, rustc 1.93.1, `lto = "thin"`, warm page cache.
The machine was busy with other builds throughout (load average 6.3 to 6.7 on 10 cores), so the
table gives the fastest of 15 iterations, best of two runs, in milliseconds; the better median of
each cell was within 9% of its figure here (13% for the 5.7 ms cell). One synthetic corpus written
by `HTSlib`; no real data.

| Workload (`mixed.bam`) | Request | Detail | htslib-1t | noodles-1t | htslib-4t | noodles-4t |
|---|---|---|---:|---:|---:|---:|
| deep-hotspot: 151,258 records | query | layout | 74.4 | 75.0 | 67.8 | 72.2 |
| | query | full | 378.8 | 394.1 | 387.6 | 400.2 |
| | stream | layout | 25.3 | 26.3 | 25.7 | 15.8 |
| | stream | full | 49.0 | 42.7 | 47.4 | 42.4 |
| broad-chr1: 269,999 records | query | layout | 126.5 | 132.4 | 120.8 | 131.2 |
| | query | full | 599.0 | 634.1 | 603.5 | 632.1 |
| | stream | layout | 46.3 | 46.2 | 45.8 | 26.1 |
| | stream | full | 83.5 | 74.1 | 85.4 | 75.6 |
| long-modified-split: 2,000 records | query | layout | 19.6 | 20.9 | 5.4 | 6.4 |
| | query | full | 312.1 | 315.3 | 312.5 | 315.5 |
| | stream | layout | 19.4 | 19.3 | 5.0 | 5.7 |
| | stream | full | 18.8 | 20.6 | 11.2 | 12.2 |

Reading: on RGV's own path `noodles` takes between 0.57 and 1.18 times the `HTSlib` time. It is
within 10% everywhere except two cells: long-read layout at four threads (1.15x and 1.18x, about
1 ms) and, in its favour, short-read layout streams at four threads (0.57x to 0.61x; the `HTSlib`
region iterator gains nothing from its thread pool there). One-shot queries are 1% to 9% slower
through `noodles`; full-detail short-read streams are 11% to 13% faster. The one-shot full-detail
query is dominated by pileup derivation, which is the same code for both.

#### What has to be true before the default changes

The default stays `Htslib` until all of these hold. None of them is established by a synthetic
corpus.

1. **Identical results on real data.** The differential suite (one-shot and positioned stream,
   both detail levels) passes with no divergence on at least: an Illumina WGS BAM, an RNA-seq BAM
   with spliced reads, an ONT BAM with `MM`/`ML` and supplementary alignments, a PacBio HiFi BAM,
   a BAM indexed with CSI on a contig longer than 512 Mb, and a BAM not written by `HTSlib` (its
   records span BGZF blocks). The corpus has to be available to CI or to a documented local run;
   `benchmark-data/large-public-alignments` holds none today.
2. **Every header `HTSlib` accepts opens.** No file of that corpus fails to open through
   `noodles` because of header strictness, or the front end is changed to take reference names
   and lengths from the binary header and the metadata from the raw header text.
3. **Speed on real data.** The production section of `compare-backends` on that corpus, on an
   idle machine, shows `noodles` within 10% of `HTSlib` at four threads for the one-shot layout
   query and for positioned streams at both detail levels, for short and long reads. On the
   synthetic corpus the long-read layout case is the one outside that band.
4. **Cancellation is decided, not inherited.** The engine either does not depend on a
   `Cancelled` checkpoint and on partial statistics from a cancelled one-shot query, or the
   `noodles` front end provides them.
5. **Threads and memory under many tracks.** With 24 or more local BAM tracks open, the
   decompression threads stay within the shared allowance (already true by construction and
   tested for the lease), and the resident read-ahead (workers + 2 inflated blocks per active
   source) is measured in the desktop application, not assumed.
6. **Failure behaviour.** Truncated files, files replaced under an open source, unreadable
   indexes and index/file mismatches produce errors, never short or stale results. Truncation is
   covered by a test and replacement by the file-version guard the `HTSlib` front end also uses;
   an index that belongs to another file is not detected by either front end.
7. **`H` and `d` auxiliary types** have a specified representation that both decoders produce.
8. **A release with the option exposed.** The desktop application offers the backend as a
   setting (or environment override) for one release, with `HTSlib` still the default, and no
   report traces to it.

After the switch `HTSlib` stays as the oracle of the differential suite and as the local decoder
for any file not named `.bam`, as the recommendation above says.

### Unindexed input (PROD-10, 21 November 2026)

A plain-text SAM file, or a BAM with no index (sorted or not), is served by the same `noodles`
BAM decoder from memory: the file is read whole, its records are sorted by position (stably, so
ties keep file order), written to an in-memory BGZF BAM, and indexed as they are written
(`remote_alignment/in_memory.rs`). Queries, positioned streams, resume and the index overview
then work exactly as for an indexed file, and nothing is written to disk.
`ProductionAlignmentSource::open` does this for a file named `.sam`;
`ProductionAlignmentSource::open_in_memory` does it for any SAM or BAM file. The file may be at
most `MAX_IN_MEMORY_ALIGNMENT_BYTES` (256 MiB) on disk and its records at most that much once
encoded as BAM; beyond that the open fails with `ErrorKind::FileTooLarge` and tells the user to
sort and index. Tests hold a SAM file to the BAM `HTSlib` converts it to, and an unsorted,
unindexed BAM to its sorted, indexed original, read by `HTSlib`. A sorted BAM too large for
memory is still refused rather than indexed on load.

Naming debt outside this crate: `LocalBamCanonicalBackend` in
`crates/rgv-engine/src/local_bam_source.rs` drives `ProductionAlignmentSource`, which is local or
remote, BAM or CRAM; the name is stale.
