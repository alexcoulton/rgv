# Multi-track interaction scaling

Measured 22 August 2026 on an Apple M4 (`arm64`), macOS 15.5 (24F74), using optimized builds and
`benchmark-data/mixed.bam` at `chr1:900001-920000`. Each run used ten cached iterations. The
workspace-wide retained-record ceiling was fixed at 50,000 and the workspace decoded-cache budget
at 320 MiB; both were divided among the live alignment controllers.

| Tracks | Per-track record share | First useful frame | Cached median | Cached p95 | Median RSS | Retained records (workspace) | Primitives (workspace) | Cache hits |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 50,000 | 124.76 ms | 7.89 ms | 9.63 ms | 66.38 MiB | 42,855 | 81,532 | 10/10 |
| 2 | 25,000 | 104.50 ms | 6.69 ms | 7.59 ms | 71.25 MiB | 50,000 | 101,144 | 20/20 |
| 4 | 12,500 | 176.58 ms | 6.86 ms | 7.86 ms | 98.89 MiB | 50,000 | 104,400 | 40/40 |

Commands:

```sh
cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --track-count 1 --iterations 10 \
  --max-alignments 50000 --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-1-track-latest.json

cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --track-count 2 --iterations 10 \
  --max-alignments 50000 --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-2-track-latest.json

cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --track-count 4 --iterations 10 \
  --max-alignments 50000 --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-4-track-latest.json
```

The raw JSON reports contain all samples, source checksum, locus, counts, and timing components.
The evidence demonstrates that record retention remains a workspace-wide hard bound and that
cached layout/interaction work remains bounded as tracks are added. It is an RGV scaling gate, not
an RGV-versus-IGV comparison and not a substitute for release-desktop manual interaction testing.

## Post-feature multi-panel projection scaling

The benchmark now models the runtime ownership directly: each alignment source has one engine and
one independent projection per panel. The following native runs used a 1,280-pixel workspace split
into `chr1:900001-920000` and `chr2:1-200000`, the same 50,000-record workspace ceiling, a 320 MiB
decoded-cache budget, and ten cached iterations.

| Tracks × panels | Projection record share | Cold complete frame | Cached median / p95 | Density relayout median | Selected-to-top median | Peak RSS | Retained records | Primitives | Cache hits |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 × 2 | 25,000 | 114.14 ms | 3.09 / 3.23 ms | 2.99 ms | 3.51 ms | 48.03 MiB | 25,211 | 51,170 | 20/20 |
| 4 × 2 | 6,250 | 443.73 ms | 4.46 / 10.82 ms | 3.36 ms | 3.87 ms | 48.34 MiB | 25,844 | 57,836 | 80/80 |
| 8 × 2 | 3,125 | 811.66 ms | 3.97 / 4.44 ms | 3.66 ms | 4.63 ms | 61.53 MiB | 26,688 | 66,712 | 160/160 |

Commands:

```sh
cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --panel-locus chr2:1-200000 \
  --track-count 1 --iterations 10 --max-alignments 50000 \
  --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-1-track-2-panel-latest.json

cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --panel-locus chr2:1-200000 \
  --track-count 4 --iterations 10 --max-alignments 50000 \
  --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-4-track-2-panel-latest.json

cargo run --release -p rgv-interaction-bench -- benchmark-data/mixed.bam \
  --locus chr1:900001-920000 --panel-locus chr2:1-200000 \
  --track-count 8 --iterations 10 --max-alignments 50000 \
  --decoded-cache-mib 320 --require-rss \
  --output benchmark-results/interaction-8-track-2-panel-latest.json
```

All cached workloads remain inside the 16 ms median and 33 ms p95 interaction budgets, every one of
the 260 repeated projections hit the decoded cache, and retained records remain below the workspace
ceiling. Density and selected-to-top samples operate entirely on retained normalized data. The cold
result measures every panel/track projection settling, not merely the first panel. These values are
recorded without speculative cold-path tuning.

## Real ONT baseline

The user's 67 MiB `wf-human-variation-demo/demo.bam` was measured at
`chr6:100001-120000`. The release runner saw and retained 1,332 reads, aggregated 1,714 modified-base
calls, and emitted 3,847 primitives. Cold query plus layout was 22.14 ms; cached median/p95 were
0.31/0.35 ms; density and selected-to-top medians were 0.25/0.33 ms; peak sampled RSS was
16.34 MiB. Raw samples and the safe relative source name are in
`interaction-ont-deep-latest.json`.
