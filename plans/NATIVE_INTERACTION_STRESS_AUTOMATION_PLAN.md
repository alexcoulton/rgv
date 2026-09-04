# RGV macOS interaction stress-testing framework plan

Status: initial implementation complete; mechanical tests, checked-in scenario validation, and
local native acceptance of both the round-trip smoke scenario and a generated 12-action scenario
passed on 2026-09-04. Recording-overhead characterization and longer aggressive/soak trials remain
operator-run macOS validation because they require a visible session and pre-authorised OS input
permissions.

The implementation and operating instructions are documented in
[`docs/INTERACTION_STRESS.md`](../docs/INTERACTION_STRESS.md). The plan below remains the design and
acceptance record; its local native gates are intentionally not claimed by non-interactive CI.

This plan supersedes the earlier, broader native-interaction automation proposal. It reflects the
following decisions:

- extend RGV's existing automation and validation spine rather than create a parallel framework;
- build the generic stress infrastructure before tailoring it to the BAP1→ACTB defect;
- make recorded manual use and seeded scenario generation the two primary sources of tests;
- qualify semantic behavior, liveness, and actual macOS event routing;
- run against a visible local RGV application on macOS;
- make no cross-platform native-driver commitment; and
- exclude automatic or human visual-quality certification from this framework.

Related plans:

- [Alignment cache liveness and vertical navigation](ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md)
- [Alignment cache thrash and scroll-materialization recovery](ALIGNMENT_CACHE_THRASH_AND_SCROLL_MATERIALIZATION_RECOVERY_PLAN.md)
- [Manual fixture suite](MANUAL_FIXTURE_SUITE_PLAN.md)
- [RGV MCP implementation](RGV_MCP_IMPLEMENTATION_PLAN.md)

## 1. Outcome

RGV gains a local macOS framework that can stress the real desktop application in two ways:

1. record a sequence of native use, turn it into a stable scenario, and replay it; and
2. generate new, state-aware native interaction sequences from a reproducible seed.

Both paths use the same scenario, target, timing, assertion, failure, and artifact vocabulary. The
controller uses RGV's existing authenticated automation service for semantic setup and observation,
but gestures are delivered as macOS events to the selected RGV process. A passing native run must
show that the driver posted the events, RGV received them through its normal AppKit/winit path, the
intended interaction lifecycle ran, and the resulting semantic/liveness assertions passed.

The completed framework provides:

- opt-in, streaming capture of replay-relevant native input;
- conversion of recordings into inspectable, resolution-independent scenarios;
- guarded native pointer/scroll delivery, PID-targeted keyboard delivery, and PID/window-bound
  focus and resize driving on macOS;
- stable targeting of panels, tracks, canvases, scrollbars, and supported controls;
- recorded, accelerated, maximum-rate, and seeded-jitter timing modes;
- a state-aware scenario generator that persists every generated action before executing it;
- semantic, interaction-lifecycle, native-routing, invariant, and liveness oracles;
- external watchdogs that remain useful when the UI thread is blocked;
- complete and partial run artifacts suitable for diagnosis and replay; and
- failure-signature-preserving reduction of recorded or generated scenarios.

This is an interaction stress-testing extension to RGV automation. It is not a replacement for
unit tests, engine/property tests, format validation, fixture generation, performance benchmarks,
MCP, or manual visual review.

## 2. Scope and confidence boundary

### 2.1 In scope

- A visible RGV application launched locally on macOS.
- Development and release builds, provided they explicitly enable authenticated local control and
  interaction recording.
- Manual recording and native replay of accepted pointer, wheel, keyboard, focus, and window
  interactions.
- Generated scenarios designed to provoke races, stale work, stuck interactions, excess work,
  crashes, and invariant violations.
- Semantic setup through existing `AutomationCommand` operations when setup itself is not under
  test.
- Semantic and liveness inspection through existing automation snapshots and targeted additions.
- Capturing diagnostic traces and screenshots as failure evidence.
- The BAP1→ACTB workload as an early consumer after the generic record/replay/generate path works.

### 2.2 Out of scope

- Cross-platform native driving or a portable promise for the controller.
- Headless end-to-end interaction qualification.
- XCUITest, an Xcode test project, or XCTest result-bundle integration.
- A second application-control protocol alongside the existing authenticated loopback service.
- Internal replay that calls camera, scrollbar, workspace, or gesture state directly.
- Claiming that a Core Graphics event is identical to physical mouse or trackpad hardware input.
- Screenshot comparison, aesthetic assertions, flicker certification, or a human visual gate.
- Exhaustively exposing the RGV UI as accessibility nodes solely for test automation.
- Cache policy, rendering policy, biological truth, or the fix for any consuming defect.
- Generic fuzzing of arbitrary bytes or invalid JSON. Existing unit/fuzz targets own those inputs.

### 2.3 What a passing run proves

A successful native stress run proves, for the declared scenario and environment, that:

1. the controller targeted the expected live RGV instance and window;
2. the macOS driver posted the compiled event sequence only while the selected process and window
   passed the delivery guard;
3. RGV observed the required native event structure through its normal runtime path;
4. the intended target acquired, advanced, and released or cancelled its interaction correctly;
5. declared semantic outcomes and invariants held; and
6. UI and pipeline progress satisfied the declared liveness predicates.

It does not prove identical event coalescing on another run, physical-device fidelity, visual
quality, or behavior on another operating system.

## 3. Existing foundation: extend, do not duplicate

The repository already contains most of the semantic control plane:

| Existing capability | Current location | Required extension |
| --- | --- | --- |
| Versioned, validated command batches | `crates/rgv-core/src/automation.rs` | Reuse commands for setup/inspection; add recording and trace-control commands only. |
| UI-loop automation execution | `apps/rgv-desktop/src/automation/commands.rs` and `runner.rs` | Keep it authoritative; submit short batches between native gesture steps. |
| Authenticated loopback control | `apps/rgv-desktop/src/control_service.rs` | Add an authenticated, non-UI-dependent health read. Do not add a second server. |
| MCP control client | `apps/rgv-mcp/src/control_client.rs` | Extract the reusable transport into a shared client crate rather than copy it. |
| Semantic workspace/layout snapshots | `AutomationWorkspaceSnapshot` in `rgv-core` | Add missing stable target identities or rectangles only when a scenario needs them. |
| Native composed-window capture | `AutomationSnapshotScope::NativeWindow` | Use on failure for evidence; do not turn it into a visual oracle. |
| Run, frame, GPU, and navigation telemetry | `AutomationRunReport` | Reuse per control batch and aggregate it in one stress-run manifest. |
| Manual diagnostic trace | `manual_diagnostic_trace.rs` | Correlate it with stress runs; retain it as bounded forensic detail. |
| Short gesture/render trace | `interaction_trace.rs` | Reuse it for focused latency evidence; do not treat it as the replay journal. |
| Native event handling | `apps/rgv-desktop/src/runtime.rs` | Extract observable input handling so recording can capture pre/post decision data. |

There are three deliberately separate evidence streams:

1. **Interaction recording:** compact, streaming, replay-complete input and target evidence.
2. **Diagnostic trace:** bounded, verbose causal evidence for debugging.
3. **Stress-run telemetry:** controller, health, assertions, timing, and final classification.

They share a run ID, step IDs, interaction IDs, and monotonic sequence numbers. They must not be
collapsed into one unbounded trace or reimplemented as unrelated schemas.

## 4. User workflows

### 4.1 Record and replay

The intended local workflow is:

```text
launch or attach to an explicitly controlled RGV instance
        ↓
start interaction recording through authenticated control
        ↓
use RGV normally in the visible window
        ↓
stop recording; retain a complete or explicitly incomplete journal
        ↓
convert native events and resolved targets into a scenario
        ↓
review or edit the inspectable scenario
        ↓
replay it through the macOS driver and evaluate the run
```

Illustrative commands, to be finalised with the implementation:

```sh
rgv-interaction-stress record --attach control.json --output validation/interaction-stress/recordings/run.jsonl
rgv-interaction-stress convert validation/interaction-stress/recordings/run.jsonl --output scenario.json
rgv-interaction-stress replay scenario.json --attach control.json
```

Stopping the recorder may be initiated from the controller terminal, so the stop action does not
need to contaminate the RGV input sequence.

### 4.2 Generate and stress

The generated workflow is:

```text
attach and validate prerequisites
        ↓
query current semantic state and valid targets
        ↓
select a valid action from a seeded weighted grammar
        ↓
durably append the expanded action to a partial JSONL expansion before delivery
        ↓
post native events, observe receipt and outcome
        ↓
either continue immediately to create overlap or cross a progress barrier
        ↓
classify, preserve, and attempt to reduce any failure
```

Illustrative command:

```sh
rgv-interaction-stress generate --attach control.json --seed 184467 --actions 500 --profile aggressive
```

On clean completion, the expansion journal is compiled atomically into `scenario.expanded.json`.
The expanded scenario, not the seed alone, is the replay authority. Runtime state and OS scheduling
can influence which state-aware action is selected even when pseudorandom generation is stable.

## 5. Architecture

### 5.1 Components

The framework consists of four cooperating components:

1. **Rust stress controller**
   - parses scenarios and recordings;
   - submits existing `AutomationBatch` values for setup and observation;
   - queries semantic targets and health;
   - schedules native gestures;
   - generates scenarios, evaluates assertions, writes artifacts, and reduces failures.

2. **Small Swift macOS input driver**
   - accepts a narrow versioned stream of compiled native event instructions;
   - verifies the target PID and window;
   - posts keyboard events to that PID and pointer/scroll events through a guarded HID event tap;
   - performs explicit Accessibility operations only for window discovery, focus, and resize;
   - reports attempted post time, result, and safety state for every instruction.

3. **RGV input observer/recorder**
   - observes real `WindowEvent` and relevant `DeviceEvent` values around the existing input
     decisions in `runtime.rs`;
   - resolves semantic targets against the authoritative workspace layout;
   - records egui consumption, viewer acceptance, interaction transitions, and semantic outcome;
   - never accepts synthetic replay by directly mutating viewer state.

4. **Existing RGV automation/control service**
   - owns setup commands, workspace snapshots, captures, trace control, and semantic state;
   - exposes a minimal atomic health snapshot independently of UI command execution;
   - remains opt-in, loopback-only, authenticated, and rooted to an allowed artifact directory.

The Swift helper avoids introducing unsafe macOS FFI into Rust crates, whose workspace policy
forbids unsafe code. It builds with Swift Package Manager and Command Line Tools; full Xcode is not
required. The development host currently has Swift 6.1.2 Command Line Tools but no full Xcode,
which is another reason not to make XCUITest part of this plan.

### 5.2 Native execution sequence

For each gesture step, the controller:

1. verifies the application instance ID and PID through local control;
2. requests a fresh workspace layout and checks required state;
3. asks the macOS driver for the selected process window's current global bounds;
4. resolves the semantic target into global coordinates and records the layout revision;
5. opens an RGV observation scope carrying the run ID and step ID;
6. sends the compiled primitive event schedule to the driver;
7. receives driver acknowledgements while polling external health;
8. closes the observation scope and obtains RGV's native receipt/lifecycle summary;
9. checks routing, target, semantic, invariant, and liveness assertions; and
10. writes the step result before proceeding.

Target or window geometry is refreshed at every gesture boundary. If it changes during a gesture
when the step did not request a resize, the run aborts that gesture with a typed
`target_geometry_changed` result rather than posting to an uncertain location.

### 5.3 No in-app replay driver

The earlier proposal routed replay into a post-egui viewer reducer. That would provide a fast test
level, but it would not meet the selected goal of exercising actual macOS routing and would require
a large production input refactor before the framework found its first bug.

This plan therefore performs end-to-end replay only through the external native driver. Small pure
functions extracted while making input decisions observable receive ordinary unit tests, but they
are not presented as an alternative qualification driver.

## 6. Scenario and recording schemas

### 6.1 One orchestration layer over existing commands

Add a versioned `InteractionStressScenario` type in `rgv-core`. It is a thin orchestration document,
not a replacement for `AutomationBatch`:

- semantic setup and inspection steps contain existing `AutomationCommand` values;
- native steps contain semantic targets and primitive event schedules;
- barriers and assertions operate on existing automation snapshots plus the new health/input
  observation records; and
- generated blocks are expanded into ordinary steps before or during execution.

The controller submits only the embedded command portions to the current `/v1/batches` endpoint.
The RGV process never receives an instruction to fabricate a native event.

Representative shape:

```json
{
  "schema": "rgv.interaction_stress",
  "schema_version": 1,
  "name": "vertical-scroll-reversal",
  "requirements": {
    "fixture": "mf-01-core-alignments",
    "window_logical_size": [1280, 800]
  },
  "steps": [
    {
      "type": "control",
      "commands": [
        { "command": "goto", "loci": ["chr1:900001-920000"] },
        { "command": "wait_until_settled", "timeout_millis": 120000 }
      ]
    },
    {
      "type": "gesture",
      "id": "scroll-1",
      "target": {
        "kind": "read_scrollbar",
        "panel_id": 1,
        "track_id": 1
      },
      "timing": { "mode": "recorded" },
      "events": [
        { "at_micros": 0, "event": "move", "x": 0.5, "y": 0.1 },
        { "at_micros": 1000, "event": "primary_down" },
        { "at_micros": 180000, "event": "drag", "x": 0.5, "y": 0.9 },
        { "at_micros": 260000, "event": "drag", "x": 0.5, "y": 0.2 },
        { "at_micros": 270000, "event": "primary_up" }
      ]
    },
    {
      "type": "assert",
      "predicate": { "kind": "gesture_terminated", "step_id": "scroll-1" }
    }
  ],
  "limits": {
    "maximum_duration_millis": 300000,
    "maximum_native_events": 100000
  }
}
```

The exact Rust representation must use bounded typed enums rather than accepting arbitrary event
or assertion objects.

### 6.2 Step types

The initial schema supports:

- `control`: one short existing automation batch;
- `gesture`: target plus compiled pointer/scroll/key primitives;
- `barrier`: a typed semantic or progress predicate with an outer deadline;
- `assert`: an immediate typed predicate;
- `checkpoint`: persist state, health, recording position, and optional diagnostic capture;
- `generated`: a bounded generator specification; and
- `repeat`: a bounded repetition expanded into explicit steps in the run artifact.

No-wait behavior is expressed explicitly: a gesture may proceed directly into the next step while
source/render work remains active. A barrier is inserted only when the scenario requires a
particular state or authority.

### 6.3 Semantic targets

Initial target kinds are:

- RGV window;
- genomic canvas by panel;
- read canvas by panel and track;
- read scrollbar by panel and track;
- track gutter/header by track;
- overview and ruler by panel;
- pane content/header by stable pane identity; and
- a supported control by stable accessibility identifier.

Coordinates are target-relative fractions. Values outside `[0, 1]` are allowed only for an
explicit bounded `drag_outside` path. The recording retains original physical and logical
coordinates as evidence, but converted scenarios prefer target-relative coordinates.

When no stable semantic or accessibility target exists, conversion may emit a normalized window
coordinate with a required window size and mark the step `fragile_target`. Generated scenarios may
not choose fragile targets.

### 6.4 Timing and reproducibility

Supported delivery modes are:

- `recorded`: preserve observed inter-event cadence;
- `scaled`: multiply recorded cadence by a bounded factor;
- `maximum_rate`: post the next event as soon as the driver accepts it; and
- `seeded_jitter`: perturb a base cadence with fully recorded seeded choices.

Reproducibility means:

- a saved expanded scenario contains the exact event intent and chosen timing;
- the driver log contains actual post timestamps;
- the RGV journal contains actual receipt timestamps and accepted event structure; and
- all differences between requested, posted, received, and applied input are visible.

It does not mean macOS will coalesce or schedule events identically on every run.

## 7. Native interaction recording

### 7.1 Recorded evidence

Recording starts only through an explicit authenticated command and captures:

- run/session ID, application instance ID, process ID, build, scale, and window geometry;
- initial workspace state and source/session/fixture fingerprints;
- monotonic sequence number and receipt time;
- native event class and supported payload;
- physical and logical pointer positions;
- button, modifiers, wheel units/deltas/phases, and raw motion where delivered;
- current layout revision and resolved semantic target;
- egui-consumed and viewer-accepted decisions;
- interaction begin/advance/release/cancel transitions and correlation ID;
- focus, resize, and scale changes;
- semantic state checkpoints at gesture boundaries; and
- unsupported/non-convertible events with an explicit reason.

Sensitive source paths and credentials follow the existing automation redaction rules. A local
recording may refer to an explicit fixture ID; a checked-in scenario may not depend on a private
absolute path. Keyboard input is persisted only for recognized, non-sensitive RGV controls by
default. Text sent to an unknown or sensitive field is recorded as redacted and non-convertible
unless the user explicitly enables text capture for that recording.

### 7.2 Runtime integration

Refactor the large native-input match in `runtime.rs` only enough to make decisions observable:

- capture immutable pre-event context;
- execute the existing native handler exactly once;
- return a bounded `NativeInputObservation` describing target and outcome; and
- send that observation to the recorder when enabled.

The refactor must not introduce a second mutation path or change event ordering. Native behavior
before and after extraction is covered by focused unit/state-machine tests.

### 7.3 Streaming and completeness

The existing diagnostic ring cannot be the replay source because it overwrites old events.
Interaction recording therefore writes ordered JSON Lines through a bounded background writer to
`*.partial.jsonl` and atomically finalises it on clean stop.

Rules under pressure:

- press, release, cancellation, focus changes, phase transitions, direction reversals, target
  changes, and checkpoints are structural and may never be silently discarded;
- redundant adjacent motion may be coalesced only when target, held state, direction, and semantic
  outcome are unchanged;
- if the writer cannot preserve a structural record without blocking the UI, mark the recording
  incomplete and stop accepting further replay claims; and
- a crash or stall retains the partial file and its last durable sequence number.

### 7.4 Conversion

The converter groups events into interaction lifecycles, maps physical coordinates to semantic
targets, normalises paths, and emits a scenario plus a conversion report. It rejects or flags:

- missing press/release or focus-cancel structure;
- unexplained sequence gaps;
- unsupported event classes;
- target identity changes that cannot be represented safely;
- source/session prerequisites without a portable fixture mapping; and
- recordings already marked incomplete.

Conversion never silently repairs an invalid recording. A user may explicitly accept a
`fragile_target` or edit a generated scenario before replay.

## 8. macOS native input driver

### 8.1 Process and window targeting

The driver accepts only an explicit PID supplied by the authenticated controller. Before input it
validates:

- the PID is still alive;
- the control service reports the same application instance ID and PID;
- the process is RGV, with bundle identity checked when running a packaged build;
- exactly one selected RGV window matches the expected bounds; and
- Accessibility/event-posting permissions are available.

The driver discovers the target window and translates target-local coordinates to global desktop
coordinates immediately before each gesture. It does not use absolute coordinates stored in the
scenario as the primary target.

### 8.2 Event coverage

The first complete driver supports:

- pointer move;
- primary and secondary down/drag/up;
- held drags with reversals and out-of-target motion;
- line and pixel scroll events;
- continuous scroll, scroll phase, and momentum-phase fields supported by Core Graphics;
- modifiers and bounded keyboard sequences;
- RGV focus loss/gain through a driver-owned focus-sentinel window and explicit process
  reactivation; and
- explicit window resize through the selected Accessibility window.

Core Graphics provides PID-targeted keyboard posting, HID-session event posting, and typed
[scroll phase and continuity fields](https://developer.apple.com/documentation/coregraphics/cgeventfield).
On the supported macOS host, direct PID posting delivered pointer movement but did not reliably
deliver mouse-button transitions. Pointer and scroll events therefore use the HID event tap only
after the driver validates the exact frontmost RGV process, WindowServer window number and bounds,
topmost normal-window hit, and content-relative point. XCUITest would add a second orchestration
stack without adding a required confidence claim.

### 8.3 Safety

- Never post an event without a validated PID, application instance, window, run ID, and step ID.
- Keep keyboard events PID-targeted. Before every globally routed pointer or scroll event, require
  the exact RGV PID to be frontmost, the selected layer-zero WindowServer window to own the target
  point, unchanged window bounds, and a point inside the validated content area; abort before
  posting if any check fails.
- Require exclusive interaction with the selected RGV window during replay; abort on native input
  that cannot be matched structurally to the active driver step.
- Bound every run by event count, wall-clock duration, coordinate extent, and held-button duration.
- On controller cancellation or driver error, attempt the matching button/key releases before
  exit and record whether cleanup succeeded.
- Provide immediate terminal cancellation independent of the RGV UI thread.
- Pause if the selected window moves, resizes, disappears, or loses the required identity.
- Restore the pointer position when practical and record whether it was restored.
- Refuse to run without the required macOS permissions; never attempt to bypass them.

The driver protocol is intentionally narrow and versioned. The Rust controller remains responsible
for scenario semantics; Swift receives only validated primitive event instructions and returns
delivery acknowledgements.

## 9. State-aware scenario generation

### 9.1 Generator model

The generator selects only actions valid for the current `AutomationWorkspaceSnapshot` and current
interaction state. Its model includes:

- available panels, tracks, panes, canvases, scrollbars, and supported controls;
- active panel/track and current loci;
- settled versus pending generations;
- active pointer/scroll capture;
- track visibility and scrollability;
- current focus and window geometry; and
- configured run limits and already exercised transition classes.

Every choice derives from an explicit seed and is appended durably to a partial JSONL expansion
journal before its events are posted. Hidden randomness in either RGV or the controller is not a
reproducibility mechanism.

### 9.2 Initial action grammar

- canvas pan, hold, slow drag, fast drag, and rapid reversal;
- vertical scrollbar top/middle/bottom traversal and reversal;
- vertical wheel/trackpad-like scrolling over valid targets;
- zoom bursts and zoom-direction reversal;
- a new gesture while source or render work remains pending;
- locus changes interleaved with unresolved gestures or work;
- release outside the original target;
- focus loss during a held interaction;
- resize between or during bounded interaction sequences;
- switching active panels and tracks;
- right-click/open/dismiss on supported targets; and
- bounded repetitions and pairwise action combinations.

Semantic automation commands may establish a complex starting workspace efficiently. Generated
actions intended to test interaction routing must still use native events.

### 9.3 Profiles and coverage

Initial profiles are:

- `smoke`: short, conservative cadence and one action from each available class;
- `recording_mutation`: mutate paths, timing, repetitions, and interleavings from a recording;
- `aggressive`: reversals, maximum-rate bursts, focus/resize disruption, and no-wait overlap; and
- `soak`: bounded long duration with weighted state transitions and periodic checkpoints.

The run reports action classes, targets, transition pairs, overlap depth, event rates, pending-work
states, and failure-free iterations. Early generation is state- and coverage-aware, not code
coverage-guided. A future corpus-guided mutator is permitted only after the basic generator yields
actionable failures.

## 10. Oracles and watchdogs

### 10.1 Native-routing oracle

For every gesture the run correlates:

- controller instruction sequence;
- Swift driver post acknowledgements;
- RGV native event receipts;
- resolved target and consumption decision; and
- interaction state transitions.

Mouse-move coalescing may reduce one-to-one receipt counts, so the oracle compares required
structure rather than exact counts: down precedes held motion, required reversals are observed,
phase boundaries are preserved where supported, and every held input terminates. Missing required
structure is `native_event_missing` or `native_event_shape_mismatch`, not a semantic application
failure.

### 10.2 Semantic and interaction oracles

Initial typed predicates include:

- expected panel, track, target, locus, viewport relation, or scroll relation;
- accepted input changed the intended control state when authority permitted;
- current desired generations eventually committed or reached a typed terminal state;
- no stale generation published as current;
- pointer, scroll, menu, or focus capture ended correctly;
- no active interaction survived release, cancellation, focus loss, or target destruction;
- no critical/invariant failure occurred; and
- a step declared as intentionally overlapping work did not accidentally wait for settlement.

Feature plans may add typed predicates over existing automation snapshots. They must not put cache,
renderer, or biological policy into the generic generator.

### 10.3 Health channel

Add an authenticated health endpoint served by the existing control-service thread without waiting
for an automation batch or the UI event loop. Its bounded atomic snapshot contains at least:

- process and application instance identity;
- UI/event-loop heartbeat sequence and last monotonic update;
- native input receipt sequence;
- frame/presentation sequence;
- desired and committed workspace/scene generation counters;
- source/worker progress sequence and last update;
- active interaction ID and state code;
- latest terminal/invariant failure code; and
- recording writer state and last durable sequence.

The endpoint may return a slightly stale but internally versioned snapshot. It must never acquire a
lock owned by the UI or a source/render worker.

The source/worker progress sequence advances at bounded phase boundaries: request dequeue, source
work entry/return, fetch/decode progress, scene preparation, GPU preparation, and publication or
terminal cancellation/failure. Corresponding diagnostic begin/end events carry the current owner
and generation. This is enough to distinguish no progress from expensive progress without putting
unbounded worker detail in the health endpoint.

### 10.4 Failure classification

The external watchdog distinguishes:

- `process_exit`: the target process ended;
- `ui_stall`: control health responds but UI heartbeat stops;
- `pipeline_stall`: UI advances while current desired work makes no source/render/publication
  progress and neither settles nor fails;
- `progress_churn`: counters advance but current desired authority cannot commit;
- `native_event_missing` or `native_event_shape_mismatch`;
- `extraneous_native_input` when replay cannot distinguish the expected stream from concurrent
  interaction;
- `interaction_lock`: input arrives but the intended interaction cannot advance or terminate;
- `semantic_mismatch`;
- `critical_invariant`;
- `target_geometry_changed`;
- `recording_incomplete`; and
- `driver_failure` or `permission_failure`.

Progress predicates are primary. Every wait still has a generous outer deadline so a defective run
cannot continue indefinitely.

## 11. Run artifacts and diagnostic capture

Each run writes one directory:

```text
stress-run.json
scenario.input.json
scenario.expanded.json
scenario-expansion.partial.jsonl   # retained after interruption during online generation
controller-events.jsonl
driver-events.jsonl
app-input-events.jsonl
health.jsonl
automation/
  batch-*.json
  report-*.json
recording/
  source.jsonl                 # when the run originated from recording
diagnostics/
  trace.json                   # when available
  process-sample.txt           # on a detected stall
  failure-window.png           # evidence only
reduction/
  candidate-*.json
  minimized.json
```

`stress-run.json` is a thin aggregate manifest. It references, rather than duplicates, existing
automation reports and contains:

- run ID, scenario schema, seed, profile, and source recording ID;
- application build/commit/dirty fingerprint and controller/driver versions;
- macOS, hardware, display scale, window geometry, and requested limits;
- source/session/fixture fingerprints with existing redaction rules;
- requested, posted, received, applied, coalesced, and rejected event counts;
- assertion results and typed failure signature;
- process, controller, driver, and recorder exit states; and
- checksums and completeness state for every referenced artifact.

The manifest is created as `running` before the first mutation. It is atomically finalised as
`passed`, `failed`, `cancelled`, or `incomplete`. A crash must not make a partial run look passed.

On failure:

- stop native delivery and release held state;
- freeze and save the existing diagnostic trace if the UI/control path responds;
- request the existing native-window snapshot as diagnostic evidence, not an oracle;
- sample the process externally when the UI is stalled;
- preserve journal, driver, controller, health, and partial manifest without waiting for the UI;
  and
- begin reduction only after the original artifacts are durable.

## 12. Failure reduction

Reduction operates on the fully expanded scenario, not on a seed or unexpanded generator block. It
attempts, in order:

1. remove action ranges while preserving prerequisites;
2. remove individual actions;
3. shorten repetitions and gesture paths;
4. remove intermediate motion samples while preserving reversals and phases;
5. simplify timing, overlap, and jitter;
6. remove irrelevant panels, tracks, options, and setup commands; and
7. reduce generated parameter choices.

A candidate preserves a failure only when its typed signature matches the original class and
required target/owner identity. Because native scheduling is statistical, the controller records
attempts and successes and uses a configurable reproduction threshold. It never labels a
non-reproducing candidate as minimized merely because one attempt timed out differently.

The original failure remains authoritative even if automatic reduction finds no smaller stable
case.

## 13. Proposed repository structure

```text
crates/rgv-control-client/
  src/lib.rs                         # shared authenticated client used by MCP and stress controller
crates/rgv-core/src/
  interaction_stress.rs             # scenario, recording, predicate, failure, and manifest types
apps/rgv-desktop/src/
  input_dispatch.rs                 # observable native input decisions; no synthetic injection
  interaction_recording.rs          # compact streaming recorder
  stress_health.rs                  # lock-independent health publication
tools/interaction-stress/
  Cargo.toml
  src/                              # Rust controller, converter, generator, oracle, reducer
  macos-driver/
    Package.swift
    Sources/                        # narrow Swift Core Graphics/Accessibility driver
    Tests/
validation/interaction-stress/
  scenarios/
  recordings/
  expected-failures/
  README.md
docs/
  INTERACTION_STRESS.md
```

Exact module names may follow the modularisation work, but ownership does not change:

- portable serializable types live in `rgv-core`;
- authenticated HTTP client behavior is shared rather than copied from `rgv-mcp`;
- native event decisions and observations stay in `rgv-desktop`;
- macOS APIs stay in the development-only driver; and
- scenarios and retained failure cases live under validation.

## 14. Implementation phases and gates

### Phase 0 — Contracts and controller skeleton

Deliverables:

- `InteractionStressScenario`, recording, target, assertion, health, failure, and manifest schemas;
- explicit bounds and backward-compatibility tests for every schema;
- shared extraction of the current MCP/control client transport;
- Rust controller CLI with attach, identity validation, run directory, and short control batches;
- Swift Package skeleton with permission preflight and PID/window verification; and
- documentation of the requested/posted/received/applied evidence model.

Gate: a controller attaches to one visible RGV instance, validates identity, queries the current
control status and workspace state, and creates a truthful empty run artifact without a second
control protocol.

### Phase 1 — Complete native recording

Deliverables:

- observable native input handling around the existing runtime decisions;
- compact asynchronous JSONL recording with explicit completeness state;
- recording start/status/stop commands on the existing automation schema;
- target resolution and gesture lifecycle correlation;
- initial-state and gesture-boundary semantic checkpoints; and
- recording-to-scenario conversion with fragile/unsupported diagnostics.

Gate: an ordinary manual session containing a canvas drag, scrollbar reversal, scroll/zoom, focus
change, and resize produces a complete journal and an inspectable scenario without relying on the
bounded diagnostic ring.

### Phase 2 — Native replay and routing proof

Deliverables:

- Swift Core Graphics pointer, scroll, modifier, and key delivery;
- Accessibility window discovery, focus, and resize;
- semantic-to-global coordinate resolution refreshed per gesture;
- driver/app correlation and native-routing assertions;
- atomic authenticated health publication plus process/UI/native-input watchdogs;
- recorded, scaled, maximum-rate, and seeded-jitter timing;
- emergency release/cancellation and all safety bounds; and
- aggregate artifacts plus failure-time trace/capture/process sampling.

Gate: the Phase 1 scenario replays into a visible RGV process through macOS, RGV confirms the
required native event structure and lifecycle, and basic semantic/liveness assertions pass.

### Phase 3 — State-aware generation

Deliverables:

- live generator state model;
- initial action grammar and smoke/aggressive/soak profiles;
- durable online expansion before event delivery;
- no-wait overlap and progress barriers;
- action/target/transition coverage reporting; and
- mutation of recorded scenarios.

Gate: identical saved expanded scenarios replay identically at the intent level, different seeds
exercise meaningfully different valid action sequences, and a bounded generated run completes or
fails with a typed, actionable classification.

### Phase 4 — Watchdogs and reduction

Deliverables:

- deeper pipeline, churn, routing, and interaction classifiers using the Phase 2 health stream;
- bounded source/worker phase progress instrumentation and correlated diagnostic spans;
- controller-side deadline and cancellation handling independent of the UI;
- failure-signature-preserving reducer with repeated native trials; and
- recovery tests for blocked, crashed, cancelled, and partially written runs.

Gate: controlled test doubles for each failure class are classified correctly, their partial
artifacts remain readable, and at least one deliberately verbose failing scenario is reduced while
preserving its signature.

### Phase 5 — Scenario corpus and first real consumers

Deliverables:

- generic record/replay round-trip scenario;
- generated pan/zoom/scroll/focus/resize stress scenario;
- documented local smoke and soak commands;
- conversion of the retained BAP1→ACTB interaction into the generic schema;
- cache-liveness assertions supplied by the consuming cache plan; and
- retained minimized regressions discovered by subsequent stress runs.

Gate: the framework works without knowledge of ACTB, then runs the BAP1→ACTB workload as a normal
consumer and preserves any observed failure in the same artifact and reduction pipeline.

## 15. Verification strategy

### 15.1 Mechanical tests

- scenario/recording/manifest schema validation, size limits, and version compatibility;
- automation command embedding without a duplicate command representation;
- recording sequence ordering, structural-event preservation, coalescing, and incomplete status;
- target resolution across panel/track changes, window scale, and resize;
- coordinate conversion between RGV logical, physical window, and global macOS spaces;
- native input decision equivalence before and after runtime extraction;
- press/held/reversal/release/cancel state machines;
- generator determinism, validity, bounds, and durable expansion;
- assertion and failure classifier tables;
- watchdog behavior over synthetic health streams;
- manifest atomicity and partial-run recovery;
- reduction dependency preservation; and
- Swift driver protocol parsing, limits, validation, and emergency-release behavior.

### 15.2 Local native integration

- attach only to the intended RGV instance;
- refuse stale PID/instance/window combinations;
- record and replay canvas pan with multiple reversals;
- record and replay fast read-scrollbar travel;
- scroll and zoom with line, pixel, continuous, and phase metadata;
- release outside a target;
- lose focus during a held interaction;
- resize between and during scheduled work;
- overlap a new gesture with unresolved source/render work;
- cancel a run while a button is held;
- preserve artifacts after application exit or UI stall; and
- verify driver-post versus RGV-receipt evidence for every supported primitive.

These end-to-end tests require an interactive local macOS session and pre-authorised permissions.
They are not silently added to generic CI.

### 15.3 Framework overhead

Measure native interaction with recording disabled and enabled. The acceptance report includes:

- UI time spent constructing observations;
- recorder queue high-water mark and coalesced motion count;
- event-to-visible-update latency distribution;
- frame-time distribution; and
- output bytes per input event and per minute.

No universal product latency threshold is invented here. The gate is that recording does not cause
the interaction failure it is intended to observe, does not block the UI, and reports any evidence
loss truthfully.

## 16. Initial scenario corpus

The framework is implemented generically before the motivating defect. The first corpus contains:

1. **Record/replay round trip:** manually record and natively replay representative pan, zoom,
   vertical scroll, focus, and resize interactions on a deterministic manual fixture.
2. **Generated overlap smoke:** issue valid gestures with and without settlement to exercise input,
   source, scene, GPU, and publication overlap.
3. **Generated aggressive reversals:** stress canvas and scrollbar capture with rapid reversals,
   out-of-target release, and cadence mutation.
4. **BAP1→ACTB consumer:** convert the retained sequence after the generic schema and driver gates
   pass. Its cache-specific expectations remain in the cache-liveness plan.

The BAP1→ACTB scenario may supply useful path/timing data while the framework is built, but no core
type, target, oracle, or failure class may be named for that defect.

## 17. Risks and controls

| Risk | Control |
| --- | --- |
| macOS coalesces native movement differently between runs | Store requested, posted, and received streams; assert structural events and use repeated trials for scheduling-sensitive failures. |
| Recording perturbs the bug | Keep UI work bounded, stream asynchronously, benchmark overhead, and fail visibly on evidence pressure. |
| Semantic geometry goes stale | Resolve immediately before each gesture and abort on unexpected revision/window changes. |
| A generated event affects another application | Require PID and instance validation plus a per-event frontmost-process, exact-window-ID, unchanged-bounds, layer-zero hit, and content-bounds guard before globally routed pointer/scroll delivery. |
| Concurrent human input corrupts routing evidence | Declare replay exclusive, recognise expected event structure, and abort with `extraneous_native_input`. |
| Controller loses contact while input is held | Enforce driver-local duration bounds and emergency release independent of RGV. |
| Existing synchronous batch request blocks on a UI stall | Use short batches and a separate authenticated atomic health read served outside the UI queue. |
| Recording cannot map an ordinary control stably | Add a stable identifier only for needed controls; otherwise emit an explicit fragile or unsupported conversion. |
| Recording captures sensitive typed text | Capture text only in recognized non-sensitive controls by default; redact and reject unknown text input during conversion. |
| Seed does not reproduce online state-aware choices | Persist every expanded action before delivery; replay the expansion, not the seed alone. |
| Reducer mistakes a different timeout for the original bug | Match typed signature and target/owner fields across repeated trials. |
| Stress schema becomes a second automation API | Embed existing commands and snapshots; keep the new layer limited to native orchestration. |

## 18. Definition of done

The plan is complete when all of the following are true:

1. A controller can attach safely to one visible, explicitly controlled RGV process on macOS.
2. Manual native use streams to a complete journal that survives verbose trace overwrite.
3. A stopped or crashed recording is unambiguously complete or incomplete.
4. A complete recording converts into an inspectable target-relative scenario.
5. The scenario replays through guarded native macOS events, not direct RGV state mutation.
6. Driver post evidence correlates with native events received by RGV.
7. Each gesture proves target acquisition, accepted transitions, and release/cancellation.
8. Existing automation commands and snapshots perform setup and semantic observation without a
   parallel control protocol.
9. Semantic and liveness predicates produce typed, reviewable results.
10. The health/watchdog path distinguishes process exit, UI stall, pipeline stall, progress churn,
    routing failure, interaction lock, and semantic/invariant failure.
11. A state-aware generator produces bounded scenarios, durably stores their expansion, and
    reports action/transition coverage.
12. Recorded scenarios can be mutated by timing, path, repetition, and interleaving while retaining
    their prerequisites.
13. Failures preserve complete controller/driver/app evidence even when RGV cannot answer a final
    command.
14. The reducer attempts to produce a smaller reproducer matching the original typed signature.
15. Local smoke, aggressive, and soak workflows are documented and have explicit cancellation and
    safety bounds.
16. The generic round-trip and generated scenarios pass before BAP1→ACTB is added as a consumer.
17. BAP1→ACTB uses the same schema, driver, artifacts, and failure model without framework changes.
18. The framework makes no visual-quality, physical-device, cross-platform, or headless confidence
    claim.

## 19. Explicit non-solutions

Do not implement this framework by:

- creating a new local-control server or duplicating `AutomationCommand`;
- turning MCP into the stress runner;
- assigning viewport, scrollbar, pointer-capture, or gesture fields directly for replay;
- claiming an internal reducer test proves native routing;
- using AppleScript or unguarded global input as the primary driver;
- depending on absolute desktop pixels when a semantic target is available;
- requiring XCUITest or full Xcode for this local framework;
- treating the bounded diagnostic trace as a complete replay journal;
- silently dropping structural recording events;
- treating a random seed as sufficient without saving the expanded scenario;
- waiting indefinitely for settlement or artifact export;
- using screenshots as pass/fail evidence for semantic, liveness, or visual correctness;
- building cross-platform abstractions that the selected macOS-only scope does not need; or
- embedding a particular cache, renderer, or feature fix in the generic harness.
