# Native interaction stress testing on macOS

RGV’s interaction-stress tool records real use, converts it to inspectable scenarios, replays
those scenarios through the normal AppKit/winit path, and generates new state-aware scenarios.
Semantic setup and observation use the existing authenticated local-control service; native input
is posted only by the small Swift helper. Keyboard events remain PID-targeted. Pointer and scroll
events use guarded HID-session posting because the tested macOS 15 host did not reliably deliver
mouse-button transitions posted directly to a PID.

Before every globally routed pointer or scroll event, the helper verifies that RGV is the
frontmost application, the exact previously selected WindowServer window is the topmost normal
window at the target point, its bounds have not changed, and the point is inside the validated
content area. A failed check aborts before posting. The helper also preserves requested
inter-event gaps when an earlier macOS posting call is slow, instead of collapsing the rest of a
gesture into one instant, and restores the pre-run pointer position when replay finishes.

This framework proves requested → posted → received → applied event evidence, semantic outcomes,
interaction termination, and liveness. Screenshots are diagnostic artifacts only. It does not
claim physical-device fidelity, visual correctness, headless coverage, cross-platform support, or
XCUITest coverage.

## Build and permission setup

Build both executables from the repository root:

```sh
cargo build --release -p rgv-desktop -p rgv-interaction-stress
swift build -c release --package-path tools/interaction-stress/macos-driver
```

Create a protected bearer secret. The desktop rejects weak or broadly readable secrets.

```sh
umask 077
openssl rand -hex 32 > /private/tmp/rgv-interaction-stress.secret
```

Grant Accessibility and Input Monitoring permission in **System Settings → Privacy & Security**
to the terminal (or other parent application) from which the Swift helper is launched. The helper
performs a permission preflight and fails as `permission_failure`; it never opens the consent UI or
silently bypasses its routing checks.

Launch one visible RGV window with local control enabled. Port `0` is supported by RGV, but a fixed
loopback port is easiest to put in an attach document:

```sh
target/release/rgv-desktop \
  --control-port 60151 \
  --control-secret-file /private/tmp/rgv-interaction-stress.secret \
  --control-root "$PWD"
```

Copy `validation/interaction-stress/attach.example.json` to an untracked local file if its paths,
port, or fixture declaration need changing. `control_root` is the only tree in which control
commands and stress artifacts may read or write. `expected_application_instance_id` can be added
to pin an attach document to one launch. A scenario with a `requirements.fixture` value runs only
when the attach document declares the exact same fixture; converted recordings additionally pin
redacted SHA-256 source identities when available.

Verify identity, health, and the one-window assumption before doing any mutation:

```sh
target/release/rgv-interaction-stress probe \
  --attach validation/interaction-stress/attach.example.json
```

Attach rejects a mismatched control-schema version or a build that does not advertise the
workspace, interaction-recording, and stress-health capabilities. Each compiled driver request
also carries the selected application-instance, run, and step identities; the helper validates
those fields before it posts anything.

## Record, convert, and replay

Start the recorder, use the visible RGV window normally, then press Ctrl-C in the recorder
terminal. The terminal interrupt stops recording through local control and does not add an RGV UI
event to the journal.

```sh
target/release/rgv-interaction-stress record \
  --attach validation/interaction-stress/attach.example.json \
  --output validation/interaction-stress/recordings/manual.jsonl

target/release/rgv-interaction-stress convert \
  validation/interaction-stress/recordings/manual.jsonl \
  --output validation/interaction-stress/scenarios/manual.json

target/release/rgv-interaction-stress validate \
  validation/interaction-stress/scenarios/manual.json

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/manual.json \
  --attach validation/interaction-stress/attach.example.json
```

Recording is opt-in. By default, ordinary text-producing key presses are marked unsupported and
not converted; `record --record-text` is an explicit local override. A clean stop atomically
renames `*.partial.jsonl`. Queue pressure, writer failure, application exit, or a held interaction
leaves an explicitly incomplete partial journal. `convert --allow-incomplete` is forensic only and
does not make the source complete.

Conversion preserves supported press/drag/release, wheel phase, focus, resize, timing, and target
evidence. It emits target-relative coordinates where RGV exposed a stable panel/track region.
Fallback window coordinates retain the required logical window size and are marked fragile.
Primary and secondary drags are distinct. Phase-less mouse-wheel events are grouped into bounded
gestures without inventing a begin/end phase. Accessibility identifiers are accepted by the
scenario schema but replay is refused unless that identifier is explicitly exposed by the build.

Use `replay --dry-run` to resolve current targets and compile driver requests without posting
native events. A normal replay requires exclusive use of the RGV window; a second interaction
lifecycle observed during a gesture is classified as `extraneous_native_input`.

Diagnostic tracing used by replay is collected in the background. It does not open RGV’s
Diagnostics overlay over the target canvas.

## Generate and mutate

Generation reads the current workspace before each action, chooses only actions with available
targets, and durably appends every expanded action before delivery. The saved expansion—not the
seed alone—is the replay authority.

```sh
target/release/rgv-interaction-stress generate \
  --attach validation/interaction-stress/attach.example.json \
  --seed 184467 --actions 40 --profile smoke

target/release/rgv-interaction-stress generate \
  --attach validation/interaction-stress/attach.example.json \
  --seed 9021001 --actions 500 --profile aggressive \
  --maximum-no-wait-actions 8

target/release/rgv-interaction-stress mutate \
  validation/interaction-stress/scenarios/round-trip-smoke.json \
  --output /tmp/round-trip-mutated.json --seed 41 --intensity 6
```

The `smoke`, `recording-mutation`, `aggressive`, and `soak` profiles vary action weights and
cadence. Every profile retains hard event, run-duration, no-wait, and held-input bounds. Ctrl-C is
observed by the runner during control waits, barriers, and native delivery. During native delivery
it kills the scheduled helper, launches an independent PID-validated emergency release for mouse
buttons and keys, finalizes available evidence, and marks the run `cancelled`.

Each generated run writes `scenario.expanded.json` before delivery and reports exercised action
classes, targets, transitions, successful actions, and maximum observed no-wait depth in
`generation-coverage.json`.

## Checked-in scenarios and large-BAM pressure

Run the generic scenario before a data-specific consumer:

```sh
target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/round-trip-smoke.json \
  --attach validation/interaction-stress/attach.example.json

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/generated-overlap-smoke.json \
  --attach validation/interaction-stress/attach.example.json
```

For the first carry-over consumer, launch the local 9.3 GiB RNA alignment with a high enough
complete-read limit and keep `fixture: "ENCFF686MTA-rna-grch38"` in the attach document:

```sh
target/release/rgv-desktop \
  --bam benchmark-data/large-public-alignments/ENCFF686MTA.bam \
  --reference benchmark-data/large-public-alignments/reference/GRCh38_V29_UCSC_names_ERCC_phiX.fa \
  --locus chr3:52400000-52411500 \
  --genome-name GRCh38-ENCODE-RNA \
  --max-alignments 10000000 \
  --control-port 60151 \
  --control-secret-file /private/tmp/rgv-interaction-stress.secret \
  --control-root "$PWD"

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/bap1-to-actb-consumer.json \
  --attach validation/interaction-stress/attach.example.json

target/release/rgv-interaction-stress replay \
  validation/interaction-stress/scenarios/large-bam-navigation-throughput.json \
  --attach validation/interaction-stress/attach.example.json
```

The throughput scenario opens three loci, overlaps two rounds of cross-chromosome navigation,
drives 72 seeded pans, zooms, vertical scrolls, scrollbar drags, secondary clicks, focus changes,
and a resize, then settles at a base-level locus. Replay starts a bounded diagnostic trace by
default and writes it beneath the run's `diagnostics/` directory.

`large-bam-navigation-pressure.json` adds strict direction-reversal gestures. Use it to pressure
the native delivery path itself. A `native_event_shape_mismatch` from that scenario means the
recorded native path lost a reversal; inspect the driver response and `app-input-events.jsonl`
before attributing the failure to RGV's navigation pipeline. The measured 2026-09-04 run and
analysis are in
[`validation/interaction-stress/LARGE_BAM_NAVIGATION_FINDINGS_2026-09-04.md`](../validation/interaction-stress/LARGE_BAM_NAVIGATION_FINDINGS_2026-09-04.md).

These consumers use the generic schema, native driver, watchdog, and artifacts. Cache policy and
the meaning of a cache-specific failure remain owned by the cache-liveness plans.

## Failures, artifacts, and reduction

Every run creates `stress-run.json` in a UUID-named directory before its first mutation. The same
directory retains input and expanded scenarios, automation batches/reports, driver
requests/responses/stderr, RGV input JSONL, health samples, controller events, a bounded diagnostic
trace when available, and failure-time native-window/process-sample evidence. Manifest entries
carry SHA-256 checksums and completeness flags. A crash or interrupted writer cannot become a
passing manifest.

Failures use stable classes such as `process_exit`, `ui_stall`, `pipeline_stall`,
`progress_churn`, `native_event_missing`, `native_event_shape_mismatch`,
`extraneous_native_input`, `interaction_lock`, `semantic_mismatch`, `critical_invariant`,
`target_geometry_changed`, `recording_incomplete`, `driver_failure`, `permission_failure`, and
`cancelled`.

Without `--attach`, the reducer conservatively removes irrelevant actions and motion samples while
retaining setup and a declared typed signature:

```sh
target/release/rgv-interaction-stress reduce scenario.expanded.json \
  --output reduction/minimized.json \
  --failure-class pipeline-stall \
  --step-id generated-00017-00000
```

Offline output is a candidate, not proof of reproduction. For a real failure, pass the attach
document and reduce the saved `scenario.expanded.json` with fresh repeated native trials:

```sh
target/release/rgv-interaction-stress reduce scenario.expanded.json \
  --output reduction/minimized.json \
  --failure-class pipeline-stall \
  --step-id generated-00017-00000 \
  --attach validation/interaction-stress/attach.example.json \
  --artifacts validation/interaction-stress/runs/reduction-trials \
  --attempts 5 --minimum-reproductions 3
```

The source must meet the requested threshold before reduction begins. Every candidate is run
against a fresh attachment and retained only when the failure class and any supplied step,
target, and owner fields match. The original run remains authoritative if no smaller candidate is
stable.
