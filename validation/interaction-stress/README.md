# Native interaction stress validation

This corpus drives a visible local RGV process through PID-targeted macOS events. It is opt-in and
is not run in generic CI. See [`docs/INTERACTION_STRESS.md`](../../docs/INTERACTION_STRESS.md) for
setup, permissions, recording, replay, generation, artifacts, and failure handling.

For a block-by-block tmux/vim-slime walkthrough, use
[`scripts/interaction-stress-demo.sh`](../../scripts/interaction-stress-demo.sh) with the
fixture-neutral `attach.smoke.example.json` configuration.

The checked-in scenarios are:

- `round-trip-smoke.json`: generic pan, zoom reversal, focus, resize, routing, and lifecycle proof;
- `generated-overlap-smoke.json`: short state-aware generation with bounded no-wait overlap;
- `generated-aggressive-reversals.json`: long seeded reversal, scrolling, focus, and resize stress;
- `bap1-to-actb-consumer.json`: the first data-specific consumer, gated by the explicitly declared
  `ENCFF686MTA-rna-grch38` fixture;
- `large-bam-navigation-throughput.json`: 72 seeded native actions across multi-panel,
  cross-chromosome, and base-level navigation of the 9.3 GiB ENCFF686MTA BAM; and
- `large-bam-navigation-pressure.json`: the same large-BAM pressure pattern with direction-
  reversal gestures retained as strict native-delivery assertions.

The throughput scenario is the repeatable application stress test. The pressure scenario is
intentionally stricter: macOS may occasionally coalesce an intermediate dragged waypoint even
when the driver requests non-coalesced delivery, in which case the runner correctly reports
`native_event_shape_mismatch`. See
[`LARGE_BAM_NAVIGATION_FINDINGS_2026-09-04.md`](LARGE_BAM_NAVIGATION_FINDINGS_2026-09-04.md) for a
measured local run and the distinction between application and delivery findings.

Validate every document without launching RGV:

```sh
for scenario in validation/interaction-stress/scenarios/*.json; do
  cargo run -p rgv-interaction-stress -- validate "$scenario"
done
```

Run directories and raw local recordings are ignored. Reviewed minimized regressions belong in
`expected-failures/`.
