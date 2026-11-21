# ADR 0004: remote backends and byte accountability

Status: accepted for beta; amended 20 November 2026 (local CRAM backend) and 21 November 2026
(shared derivations, extension-less URLs, see Decision).

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
  through `rust-htslib`. RGV therefore ships two production alignment decoders; converging on one
  is tracked as HTS-15 / OPS-10 in `audits/COMPREHENSIVE_REVIEW_2026-11-20.md` and discussed, with
  measurements, in ADR 0002 ("Backend convergence").
- **Amended 21 November 2026.** The doc comment on `ProductionAlignmentSource` now states the
  routing above. "Both formats share normalized alignment, pileup, ... code" is now true of both
  *backends* as well: normalisation, eligibility, pileup and positioned-stream admission are
  generic over one record view (`crates/rgv-hts/src/backend_record.rs`), and local/remote parity
  is enforced by a differential suite rather than by keeping two copies in step. A remote URL
  whose path does not end in `.bam` or `.cram` (an object key, a download endpoint, a signed link)
  is identified by the object's leading bytes (`CRAM`, or a BGZF block inflating to `BAM\1`) over
  `HttpRangeReader` and opened by the same range path with the usual index discovery;
  `LocalAlignmentSource::open` refuses every URL, so `HTSlib` never opens one. Positioned streams
  can resume past chunks the caller already holds (`begin_positioned_stream_resuming`); for remote
  BAM the skipped byte ranges are left out of the range plan and are not requested.
- **Amended 21 November 2026 (later).** Three more readers use the range reader, each held to
  its local counterpart by tests that compare region results:
  - Local BAM can be opened through the `noodles` decoder on request
    (`AlignmentOpenOptions::local_bam_backend`, default `HTSlib`); it is the remote BAM decoder
    over a file. See ADR 0002, "Step 1 as built", for the measurements and for what has to hold
    before the default changes.
  - A remote bgzip-compressed BED, GFF, GTF or other tabix-format text annotation with a `.csi`
    or `.tbi` beside it (or an explicit index URL) is read on demand
    (`crates/rgv-hts/src/tracks/remote_tabix.rs`): the index is held in memory and only the BGZF
    blocks of a region are requested. Line selection follows `HTSlib`'s tabix iterator, including
    its refusal of a region whose chunk holds a line without coordinates (a `###` separator in
    GFF3), so remote and local agree on that too. A remote text annotation without an index is
    still downloaded whole (256 MiB cap). Building the name-search index of a remote indexed
    file streams it through the same reader, up to 64 MiB of transfer, after which the search
    index is reported as truncated.
  - A remote bgzip-compressed FASTA (`.fa.gz` with `.fai` and `.gzi`) is read through its `.gzi`
    block index; bases come back as stored, soft-masking included, as for an uncompressed FASTA.
    A `.2bit` reference reports soft-masking in lower case when
    `FastaOpenOptions::two_bit_soft_masking` is set (off by default, in which case the mask
    tables are not read).
  - `HTSlib` is never given a URL: a reference or variant location with a scheme other than
    `http`, `https` or `file` (`s3://`, `gs://`, `ftp://`) is refused by name before any open.
    Nothing in the product crates needs the `curl` feature of `rust-htslib` any more; only the
    `rust-htslib-http` comparison workload in `benchmarks/alignment-io` does, and it enables
    the feature itself. The workspace dependency (and `fuzz/Cargo.toml`) still name the
    feature at the time of writing.
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
