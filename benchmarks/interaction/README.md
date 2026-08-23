# RGV interaction benchmark

This benchmark measures the headless end-to-end path used by an interactive alignment track:

1. Open the indexed source through the production bounded worker actor.
2. Measure the uncached query through construction of the first useful display list.
3. Repeat the same viewport through the production decoded cache.
4. Relayout the retained queries at a different vertical density without source I/O.
5. Promote a selected read to the top and relayout without source I/O.
6. Compute cached p50/p95 latency, cache-hit count, layout-only interaction time, and sampled RSS
   where supported.

Use `--track-count 1` through `64` to measure synchronized alignment-controller scaling. The
`--max-alignments` and `--decoded-cache-mib` values remain workspace-wide totals and are divided
across tracks; reports include both each per-track share and aggregate retained-record/primitive
counts. `--require-rss` makes missing OS memory samples a hard failure.

Run the deep benchmark locus with:

```bash
cargo run --release -p rgv-interaction-bench -- \
  benchmark-data/mixed.bam \
  --locus chr1:900001-920000 \
  --iterations 10 \
  --maximum-median-ms 16.7 \
  --maximum-p95-ms 16.7 \
  --maximum-first-useful-ms 250 \
  --output benchmark-results/interaction-latest.json
```

Run on stable, named hardware before treating the result as a performance gate.

The fixed post-feature scaling matrix uses the same deep short-read locus with 1, 4, and 8 tracks
across two panels. The real long-read baseline uses the pinned local ONT demo BAM and
`chr6:100001-120000`. The archived reports contain raw samples for ordinary cached interaction,
vertical-density relayout, selected-to-top relayout, resident memory, records, and primitives.
Native-window startup, GPU presentation/upload, and end-to-end export timing are deliberately
captured from the packaged desktop rather than inferred from this headless runner.
