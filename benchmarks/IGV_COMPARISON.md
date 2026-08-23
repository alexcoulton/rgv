# RGV versus IGV benchmark protocol

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
