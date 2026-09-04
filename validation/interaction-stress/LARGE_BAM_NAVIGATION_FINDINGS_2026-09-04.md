# ENCFF686MTA large-BAM navigation findings — 2026-09-04

This is a local characterization run, not a portable benchmark threshold. It records what the
interaction-stress framework observed while driving the 9.3 GiB `ENCFF686MTA.bam` and its GRCh38
reference on an Apple M4 using Metal. RGV was built in release mode with a 10,000,000 alignment
limit and a nominal 4 GiB alignment-scene cache budget.

## Workload

`large-bam-navigation-throughput.json` drives 72 seeded native actions over three phases:

1. three simultaneous BAP1/ACTB/GAPDH-scale panels followed by 24 generated actions;
2. two deliberately overlapping three-panel, cross-chromosome `goto` batches followed by 32
   actions, including focus changes, secondary clicks, and a resize; and
3. a base-level ACTB view followed by 16 actions, a settled barrier, an invariant assertion, and a
   native-window checkpoint.

The generated blocks allow up to eight actions without a settling wait so foreground navigation,
background source work, publication, cache reclaim, and native input overlap. The companion
`large-bam-navigation-pressure.json` keeps reversal-shaped pans, zooms, and scrollbar drags to test
native path fidelity as well as application throughput.

## Passing application-throughput run

- Run: `dcff7ec1-8230-46c2-a34e-0a0e7a385340`
- Result: passed in 72.019 seconds; all 72 generated actions were failure-free.
- Coverage: 18 pans, 18 zooms, 18 vertical scrolls, 8 scrollbar drags, 6 secondary clicks, 3 focus
  cycles, and 1 resize across four observed panel identifiers and the window.
- Native delivery: 291 requested and posted driver events; 463 application input records; zero
  rejected or recorder-coalesced events; zero emergency releases.
- Liveness: 656 health samples, 15,487 UI heartbeats, 5,532 frames, 3,638 worker-progress changes,
  and 500 native-input changes during the sampled interval. Generation lag reached one and no
  terminal health failure occurred.
- Diagnostics: 25,105 retained trace events with zero dropped or overwritten events. No critical
  failure or invariant-named event was recorded.

Frame timings from the 5,515 completed traced frames were:

| Timing | Mean | p50 | p95 | p99 | Maximum |
| --- | ---: | ---: | ---: | ---: | ---: |
| Total frame | 7.642 ms | 8.242 ms | 8.845 ms | 26.453 ms | 41.622 ms |
| CPU excluding surface acquisition | 1.687 ms | 1.277 ms | 3.954 ms | 7.198 ms | 26.401 ms |

There were 79 frames above 16.7 ms, 28 above 33 ms, and none above 100 ms. The slowest total
frames spent roughly 39–40 ms acquiring the Metal surface; the slowest CPU frame combined about
15 ms of background polling with about 10 ms of display rebuilding. Slow worker phases were
asynchronous: one source pump took 5.475 seconds, presentation pumps reached 2.477 seconds, and
camera updates reached 2.447 seconds while UI health continued to advance.

Cache pressure was real but recovered. Five completed reclaims released 1,657,640,055 bytes in
total; the largest released 420,978,095 bytes and the slowest took 41.7 ms. A sampled state briefly
reported 4,350,554,697 charged bytes against a 4,288,626,400-byte current budget (61,928,297 bytes,
or about 59 MiB, above it), with zero pinned overage. The ledger explicitly permits charged memory
to remain temporarily above a requested ceiling while an unpinned lease survives removal of its
cache key; a reclaim followed and final charged memory was below budget. This is pressure worth
tracking, but this run did not prove a budget invariant defect.

The final checkpoint showed an intact single-panel chr7 overview with the target settled, no
navigation warning, and committed and desired revisions equal. The screenshot is supporting
diagnostic evidence, not a visual correctness oracle.

## Native reversal reproducer

- Run: `adfec813-9c0f-47e2-a791-91b328e7c197`
- Result: failed after 25 successful generated actions with `native_event_shape_mismatch` at
  `generated-00031-00001`.
- Expected path: drag from x=538 to 593 to 647, reverse to 551, then return to 647, with the two
  reversals separated by about 60 ms.
- Driver evidence: all seven events were posted successfully; the reversal waypoint was attempted
  at 188.480 ms and the final waypoint at 250.443 ms.
- Application evidence: RGV received x=593 and x=647, but not x=551; it then received x=647 twice
  before a valid release. The runner therefore observed zero of the two required reversals.

The corresponding diagnostic trace had no long UI frame (maximum about 42 ms), so the evidence
does not support a BAM-navigation stall. It instead demonstrates intermittent WindowServer/AppKit
motion coalescing under load despite the driver's non-coalesced event flag. The pressure scenario
is retained as a strict reproducer; the throughput scenario excludes reversal-dependent shapes so
application performance can be assessed independently from that operating-system delivery limit.

## Framework defect found and fixed

The first large-BAM attempt also revealed a controller defect: a long-running `/v1/batches`
request held the control service's only HTTP accept thread, so concurrent `/v1/stress/health`
polls timed out even though RGV remained healthy. Batch handling now runs on a separate bounded
worker while the accept thread continues serving health and status. The regression test
`stress_health_remains_available_while_a_batch_waits_for_the_ui` holds a batch response open and
proves that authenticated health remains available concurrently.

## Reproduce

Launch RGV with the command in `docs/INTERACTION_STRESS.md`, then run:

```sh
target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/large-bam-navigation-throughput.json \
  --attach validation/interaction-stress/attach.example.json

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/large-bam-navigation-pressure.json \
  --attach validation/interaction-stress/attach.example.json
```

Diagnostic tracing is enabled by default. Each command prints its UUID run directory; inspect
`stress-run.json`, `generation-coverage.json`, `health.jsonl`, `app-input-events.jsonl`, driver
requests/responses, and `diagnostics/trace.json` together before assigning a failure to RGV.
