# Dense alignment layout benchmark

This runner exercises stable row reconciliation independently of the desktop. It always runs the
deterministic 1k/10k/25k/50k/100k synthetic clique matrix and can additionally query the public RNA BAM
at the two-base ACTB regression locus with a deliberately Too-dense canonical horizon.

```bash
cargo run --release -p rgv-dense-layout-bench -- \
  --iterations 5 \
  --actb-bam benchmark-data/large-public-alignments/ENCFF686MTA.bam \
  --enforce-gates \
  --output benchmark-results/dense-layout-latest.json
```

The report separates total reconciliation, row allocation, history indexing/commit, canonical
metric comparison, compatibility probes, and row reuse/append counts. `--enforce-gates` applies
the budgets from `plans/DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md`:

- 50,000-read synthetic allocation p95 at or below 250 ms;
- adjacent size scaling at or below 2.5x;
- zero row compatibility probes while seeding an empty-horizon clique; and
- ACTB reconciliation p95 at or below 1 second when the BAM is supplied.

Record the machine, build revision, and cache state alongside any report used as release evidence.
