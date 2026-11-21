# RGV versus IGV benchmark protocol

## Status: the matched comparison has not been run

**There is no RGV-versus-IGV result.** No IGV samples (`benchmark-results/igv-raw.json`) and no
compiled report (`benchmark-results/RGV_VS_IGV.md`) exist in this repository, and
`benchmark-results/PHASE9_QUALIFICATION_STATUS.md` records "Matched IGV comparison" as "Not run".
Nothing in this repository supports a claim that RGV is faster or slower than IGV. (Status as of
the 2026-11-20 review, finding OPS-4 in `audits/COMPREHENSIVE_REVIEW_2026-11-20.md`.)

### Why the method described below is not like-for-like

The protocol in the rest of this document, as written, measures different things on each side:

| | RGV side | IGV side |
|---|---|---|
| What is timed | `rgv-interaction-bench`: the headless data-to-display-list path. After one uncached query (`first_useful_ms`), every repeated interaction is required to hit the decoded cache. | External wall clock from `goto` to a completed snapshot in the real desktop application. |
| Window and GPU | No native window, no GPU upload, no presentation. | Full application: load, layout, paint, and image encoding to disk. |
| Cache state | Warm by construction for the p50/p95 figures. | Whatever IGV does per navigation, including any re-query. |
| Data | The committed gate uses `benchmark-data/mixed.bam`, a synthetic ~20 MB BAM written by `rgv-alignment-bench generate`. | The same file, if the protocol is followed. |

A cached, headless display-list time compared with a through-the-window goto-to-snapshot time
will favour the headless side regardless of which browser is actually faster. The
`rgv-igv-comparison` compiler checks that fixture hashes, loci, viewport widths, and read caps
match, but it cannot check that the two timings cover the same work. Its `--require-rgv-faster`
flag is a release gate, not evidence.

### What a fair protocol requires

A publishable comparison needs all of the following, none of which has been done:

1. **Both applications measured through the real window.** RGV is driven through its packaged
   desktop build (batch or local-control automation) and timed from navigation command to a
   presented or exported frame, the same interval measured for IGV's `goto`-to-snapshot. The
   headless `rgv-interaction-bench` figures may be reported alongside, but not as the comparison.
2. **Real public data.** At minimum one public short-read WGS BAM/CRAM and one public ONT
   long-read BAM, each identified by accession, URL, and checksum, with the reference build
   stated. Synthetic fixtures are for regression gates only.
3. **Matched state.** Same machine, loci, viewport size, read cap/downsampling settings, and
   cold or warm cache state for both, with both cold and warm runs reported and application
   versions recorded.
4. **Raw samples committed.** Every per-navigation timing sample for both applications, the IGV
   batch file, the RGV command file, and the machine description are committed next to the
   summary so the result can be recomputed and rerun.
5. **No dropped outliers**, at least ten repetitions, p50 and p95 reported.

Until that exists, treat the remainder of this document as a description of the tooling and of
the intended procedure, not as a description of a completed measurement.

## Tooling and intended procedure

Use the same machine, reference build, alignment file, indexes, loci, viewport width, read cap, and
warm/cold cache state for both applications. Record the application versions and operating-system
power mode with every run.

For RGV, run `rgv-interaction-bench` for the data-to-display-list path and use the desktop telemetry
for frame time, cache hits, primitive count, and GPU upload bytes. The committed deep-locus gate is:

```sh
cargo run --release -p rgv-interaction-bench -- \
  benchmark-data/mixed.bam \
  --locus chr1:900001-920000 \
  --iterations 10 \
  --maximum-median-ms 16.7 \
  --maximum-p95-ms 16.7 \
  --maximum-layout-ms 16.7 \
  --maximum-first-useful-ms 250
```

The runner opens the source through the production alignment actor, records the first uncached
query plus layout as `first_useful_ms`, then requires all repeated interactions to hit the decoded
cache. It records cached p50/p95 and samples resident memory where supported. It does not include
native-window creation or GPU presentation, which must be captured from desktop telemetry.

For IGV Desktop, use a batch file that loads the same genome and BAM, navigates to the same ordered
locus list, and takes a snapshot after every navigation. Measure process launch-to-first-snapshot
and each `goto`-to-snapshot interval from an external monotonic clock. Run at least ten repetitions,
discard no outliers, and report p50 and p95.

Alongside latency, sample resident memory immediately after every snapshot and capture remote
request count, response-body bytes, and wall time at a controlled HTTP proxy for both applications.
Report both warm and cold runs. A comparison is publishable only
when the complete commands, IGV batch file, fixture fingerprints, raw timing samples, and machine
description accompany the summary.

After collecting the raw IGV samples in the versioned template, compile and enforce the report:

```sh
cargo run --release -p rgv-igv-comparison -- \
  benchmark-results/interaction-latest.json \
  benchmark-results/igv-raw.json \
  --output benchmark-results/RGV_VS_IGV.md \
  --require-rgv-faster \
  --require-complete-telemetry
```

The compiler rejects mismatched fixture hashes, loci, viewport widths, read caps, invalid values,
missing commands, and fewer than ten samples. Release qualification additionally requires RGV to
beat IGV at p50 and p95 interaction latency, sampled RSS, remote wall time, and remote response
bytes, as well as first-useful latency. `--allow-development-sample` exists only for testing the
report layout and must not be used for a published comparison.
