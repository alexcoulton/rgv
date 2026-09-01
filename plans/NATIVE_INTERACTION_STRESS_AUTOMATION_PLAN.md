# Native interaction stress automation plan

Status: proposed; implementation has not started

Plan date: 2026-09-01

Initial evidence and client:

- `rgv-manual-diagnostic-trace-9e8a3e20-b2d6-4e14-b3eb-5bb068f7dccd.json`;
- retained trace interval: 61.437–236.813 seconds;
- 100,000 retained events and 54,494 overwritten earlier events; and
- the BAP1→ACTB cache-liveness and vertical-scroll failure.

Related plans:

- [Alignment cache liveness and vertical navigation](ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md)
- [Unified alignment-scene implementation](UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md)
- [Manual fixture suite](MANUAL_FIXTURE_SUITE_PLAN.md)
- [RGV MCP implementation](RGV_MCP_IMPLEMENTATION_PLAN.md)

This plan owns automated gesture reproduction, native GUI stress, trace and action capture,
watchdogs, seeded scenario generation, failure reduction, and qualification artifacts. It does not
prescribe alignment cache policy, eviction algorithms, vertical-residency architecture, or the fix
for the motivating ACTB defect. The cache-liveness plan consumes this framework as its first major
client.

## 1. Outcome

RGV should be testable through repeatable interaction sequences that resemble actual browsing:
held drags, fast reversals, scrolling, zooming before settlement, switching loci, focus loss,
resizing, and overlapping operations. A failed run should produce enough evidence to explain the
failure and a compact script capable of reproducing it.

The completed framework provides:

- a compact, complete, resolution-independent journal of meaningful user actions;
- one versioned gesture-script format shared by deterministic and native drivers;
- deterministic replay through the same post-egui viewer-input decisions as the real application;
- trace start, status, freeze, save, and composed-frame capture through typed automation;
- an external heartbeat and progress watchdog capable of distinguishing UI and worker stalls;
- a packaged-app XCUITest driver for real macOS accessibility and pointer routing;
- a PID-targeted Core Graphics driver for high-rate native stress;
- seeded state-aware scenario generation and automatic failure minimisation;
- semantic, liveness, performance, and bounded visual oracles; and
- a clear human visual gate for behavior automation cannot judge reliably.

The framework is reusable across tracks, panels, properties, annotation, navigation, selection,
context menus, exports, and future native interaction work. It is not an ACTB-only test script.

## 2. Why the existing automation is insufficient

### 2.1 High-level commands bypass the failure path

The current `ChangeViewport` automation computes and assigns a genomic viewport directly. It is
valuable for setup and semantic operations, but it bypasses:

- OS-to-winit event conversion;
- egui consumption decisions;
- cursor movement and physical/logical coordinate conversion;
- press, held movement, raw motion, and release ordering;
- pointer capture and focus cancellation;
- scroll units, phases, and momentum;
- the vertical scrollbar's drag lifecycle; and
- redraw behavior while an input remains held.

Those are precisely the paths involved in the panning, white-space, flicker, scrollbar, and
interaction regressions found during manual testing. Direct camera mutation cannot qualify them.

### 2.2 The current trace cannot be replayed completely

The verbose diagnostic recorder is a bounded in-memory ring. The motivating 236-second session
retained 100,000 events but overwrote its first 54,494 events. Raw frame and pointer diagnostics are
too verbose to serve as the sole replay record.

The trace also records worker state mainly when existing events are emitted. In the final ACTB
stall, a request remained `Running` for more than 43 seconds but produced no first work checkpoint.
The trace could not identify which call or phase occupied the worker.

### 2.3 Automatic visual claims have been unreliable

Headless state assertions and screenshots have repeatedly failed to predict what the user sees
during continuous native interaction. The new framework must distinguish:

- semantic correctness;
- liveness and ownership correctness;
- performance and work-amplification evidence;
- bounded machine-checkable visual symptoms; and
- subjective visual quality requiring user assessment.

It may detect a fully blank read-region capture. It may not declare motion, flicker, labels,
chevrons, substitutions, or Sashimi aesthetically correct without user review.

## 3. Governing principles

1. **One scenario vocabulary, multiple fidelity levels.** Deterministic replay, XCUITest, and
   Core Graphics stress share scripts and assertions but clearly label what each driver proves.
2. **Drive real decisions.** Gesture tests enter through the same normalized viewer-input dispatcher
   as native events; they do not assign camera or scrollbar state directly.
3. **Semantic targets over desktop pixels.** Scripts address panels, tracks, canvases, and controls
   using stable identities and normalized coordinates.
4. **Record intent compactly.** Preserve gesture starts, paths, reversals, phases, checkpoints, and
   releases independently of verbose diagnostic retention.
5. **Progress over arbitrary waiting.** Barriers observe generations, owners, and progress. Fixed
   sleeps are permitted only as intentional parts of an input cadence, not as settlement logic.
6. **Artifacts are first-class.** Every run has a manifest linking build, data, script, trace,
   screenshots, state, timing, and failure signature.
7. **Failures must shrink.** A long stress failure is not fully actionable until the runner has
   attempted to reduce it to a smaller preserving script.
8. **Automation cannot self-certify aesthetics.** Human visual assessment remains an explicit gate.
9. **Opt-in and authenticated.** Replay and process control are available only when explicitly
   enabled at launch and use the existing authenticated local-control model.
10. **No production behavior fork.** The instrumented/replayable release application uses the same
    renderer, source, workers, and input reducers as ordinary use.

## 4. Run artifact and identity model

Every run receives a stable run ID and artifact directory containing:

```text
run-manifest.json
gesture-script.json
action-journal.jsonl
diagnostic-trace.json
initial-state.json
final-state.json
heartbeats.jsonl
captures/
  before.png
  held-*.png
  after.png
native/
  screenshot-*.png
  process-sample.txt
  controller.log
  optional-profile-export/
```

The manifest records:

- application version, commit, branch, dirty-state fingerprint, and build profile;
- operating system, hardware, GPU, memory, scale factor, and window geometry;
- source/session/reference fingerprints without copying private data;
- settings, panels, tracks, filters, and starting loci;
- gesture schema version, seed, driver, and timing mode;
- trace schema and artifact checksums;
- assertion results and failure signature;
- controller and application exit status; and
- whether user visual review is required, accepted, rejected, or not run.

Artifact writes use atomic finalisation where practical. Partial runs retain an explicit incomplete
manifest rather than masquerading as successful evidence.

## 5. Compact action journal

Add a compact user-level journal independent of the verbose frame trace.

Journal entries include:

- locus editor submission and history navigation;
- panel/track activation;
- pointer press, normalized movement path, held duration, release, and cancellation;
- vertical wheel or scrollbar fractions, cadence, interaction ID, and target identity;
- wheel delta, unit, touch phase, momentum phase, anchor, and modifiers;
- horizontal pan and overview gestures;
- zoom factor/path and anchor;
- focus loss/gain, resize, scale change, and window occlusion where observable;
- relevant settings and context-menu actions;
- explicit wait/barrier and capture requests; and
- semantic outcome or error associated with the action.

Repeated movement samples may be delta/time encoded. The following may never be discarded:

- gesture start;
- changes in direction or phase;
- held-state capture points;
- focus/capture transitions;
- final accepted target; and
- release/cancel.

Write the journal asynchronously to a temporary artifact throughout the run. The writer must not
block the UI and must preserve ordering by sequence number. If journal pressure occurs, coalesce
only redundant intermediate motion while retaining the structural events above.

The journal serves three purposes:

1. turn manual runs into candidate replay scripts;
2. preserve the complete user sequence when the verbose trace ring overwrites old events; and
3. correlate user intent with worker, scene, GPU, and frame diagnostics.

## 6. Shared normalized viewer-input dispatcher

Extract post-egui native input decisions from the runtime event handler into a typed `ViewerInput`
dispatcher. Real winit handlers and deterministic replay both call it.

Representative events include:

- cursor movement with physical position, scale, and consumed state;
- primary/secondary button down and up;
- raw mouse motion;
- wheel delta with pixel/line units, phase, momentum, anchor, and modifiers;
- focus lost/gained;
- resize and scale-factor change;
- redraw request/tick needed by a held gesture; and
- explicit cancellation caused by window or control state.

The dispatcher preserves:

- egui-consumed versus viewer-consumed decisions;
- physical-to-logical conversion;
- press/move/release ordering;
- pointer capture ownership;
- active panel and control targeting;
- the same workspace actions and invalidation path as native input; and
- manual diagnostic correlation IDs.

### 6.1 Required separation

Use two layers:

1. **Native adapter:** translates winit/AppKit-facing events and egui consumption into
   `ViewerInput`.
2. **Viewer reducer:** applies `ViewerInput` to viewer interaction state and workspace actions.

Deterministic replay enters at the second layer. It proves reducer, action, scheduling, scene, and
render behavior but does not claim to prove AppKit/winit conversion or OS coalescing. Native drivers
exercise both layers.

### 6.2 Event-loop marshalling

Replay must run on the real application event loop. Use a custom application user event or bounded
controller mailbox to deliver scheduled replay input; do not attempt to fabricate native
`WindowEvent` values and claim OS fidelity.

The event loop remains free to process worker replies, redraws, and native events between scripted
inputs. A replay controller owns scheduling, barriers, cancellation, and artifact completion, but it
cannot mutate viewer state from its own thread.

## 7. Gesture script schema

Use a versioned, inspectable JSON format. A script contains:

- schema version and deterministic seed;
- required source/session fingerprints and launch configuration;
- window size, scale, cache settings, panels, tracks, and initial locus;
- semantic targets such as `genomic_canvas(panel)`, `read_canvas(panel, track)`,
  `read_scrollbar(panel, track)`, `overview(panel)`, and `locus_editor`;
- normalized target coordinates;
- timestamped events or duration/cadence curves;
- semantic setup commands;
- no-wait overlap groups;
- settlement/progress barriers;
- state, liveness, performance, and visual-region assertions;
- capture points before, during, and after held gestures; and
- an expected failure signature for preserved pre-fix regressions.

### 7.1 Step types

Initial typed steps should include:

- `goto`;
- `activate_panel` / `activate_track`;
- `press`, `move`, `release`, and `cancel`;
- `drag_path` with normalized control points and timing curve;
- `wheel_sequence` with units and phases;
- `horizontal_pan` and `vertical_scrollbar_drag` convenience expansions;
- `zoom_sequence`;
- `resize` and `focus_change`;
- `set_option` / `configure`;
- `wait_for` a typed semantic or progress predicate;
- `capture`;
- `assert`;
- `start_trace`, `freeze_trace`, and `save_trace`; and
- `repeat` or seeded generated block with an explicit expansion in the final artifact.

Convenience steps compile to primitive viewer inputs before execution. The artifact stores the
compiled sequence so the exact delivered inputs remain reviewable.

### 7.2 Timing modes

Support:

- **deterministic cadence:** fixed script timestamps and a virtual scheduling clock;
- **native cadence:** the OS controls event timing while the controller records actual delivery;
- **maximum-rate stress:** deliver the next event whenever the event loop accepts it; and
- **recorded cadence:** replay observed inter-event timing from a manual action journal.

Real worker execution remains asynchronous in every mode. Timing mode changes input delivery, not
source or renderer implementation.

## 8. Driver A: deterministic in-app replay

This is the primary development loop.

The release application launches with explicit replay/control enabled. The controller:

1. validates build, fixture, source, settings, and window prerequisites;
2. starts tracing and the action journal;
3. compiles semantic gesture targets against the live workspace layout;
4. marshals normalized viewer inputs onto the event loop;
5. evaluates typed barriers and assertions against live semantic state;
6. captures the composed GPU frame, including frames while a pointer remains held;
7. freezes artifacts on failure; and
8. finalises the run manifest.

Use the existing semantic settlement and alignment diagnostics rather than duplicating application
state in the controller. Extend them only where an assertion cannot identify the current owner,
generation, or product state.

This driver proves:

- the viewer input reducer and gesture lifecycle;
- workspace action/cancellation behavior;
- source, scene, GPU, and display responses;
- deterministic script reproduction;
- semantic and liveness assertions; and
- composed-frame symptoms.

It does not prove:

- native AppKit event translation;
- OS pointer coalescing;
- system Accessibility routing;
- true native focus/capture behavior; or
- subjective visual smoothness.

## 9. Driver B: packaged macOS XCUITest

Add a small independent XCUITest project that launches the staged `org.rgv.viewer` bundle using
`XCUIApplication(bundleIdentifier:)` and passes fixture/control/trace launch arguments.

Required RGV accessibility work:

- give genomic canvases, read canvases, overview controls, locus editor, panels, and read
  scrollbars stable AccessKit author identifiers;
- incorporate panel and track identity in split-workspace selectors;
- expose meaningful controls and regions, not millions of individual read nodes;
- expose current value/range for scrollbar and zoom controls where semantically valid; and
- add accessibility-tree tests preventing duplicate or unstable identifiers.

The XCUITest driver resolves an element, derives element-relative coordinates, and performs click,
held drag, hover, keyboard, and scroll operations. It attaches native screenshots and the internal
run artifact to failures.

This driver proves packaged-app launch, accessibility discovery, AppKit event routing, native focus,
and ordinary OS-paced gestures. It is not the sole high-rate stress driver because macOS controls
event cadence and coalescing.

Full Xcode is a prerequisite. The current development host has Command Line Tools but not the full
XCUITest/xctrace toolchain. The XCUITest project can remain separate from the Rust app build and
launch the staged external bundle by bundle identifier.

## 10. Driver C: PID-targeted native stress

Add a small Swift helper that uses Accessibility only to discover the RGV process, window, and
target bounds, then posts Core Graphics mouse and scroll events to that PID.

It covers:

- rapid horizontal reversals;
- extremely fast vertical thumb movement;
- pixel, line, continuous, and momentum scroll phases;
- drag outside the original control;
- focus loss/cancellation mid-drag;
- overlapping zoom and pan bursts;
- long native soak runs; and
- abusive event cadence unsuitable for XCUITest.

Requirements:

- explicit target PID and bundle verification;
- stable semantic target lookup through accessibility identifiers;
- no global untargeted input;
- pre-authorised Accessibility permission;
- an interactive dedicated macOS session;
- emergency cancellation and maximum run duration; and
- actual delivery timestamps recorded in the artifact.

This driver belongs in dedicated local/nightly qualification, not generic headless CI.

## 11. Diagnostic tracing

### 11.1 Worker phase spans

Add begin/end/cancel spans with stable correlation identifiers for:

- mailbox enqueue/dequeue and displaced input;
- inactive eviction and maintenance;
- source `begin_work` entry and return;
- indexed fetch open/seek/query setup;
- decode batches and record/byte progress;
- filter and evidence-root construction;
- exact summary construction;
- row-atlas construction;
- semantic page construction;
- GPU preparation and admission;
- publication and retirement; and
- cancellation observation latency.

Record target revision, owner, work/request ID, interval, row range, source/filter fingerprint,
charged/pinned bytes, elapsed time, and relevant operation counts. Emit entry before calling a
potentially blocking backend so a missing exit identifies the blocked call.

Instrumentation must be useful across alignment fixes. It reports what occurred and where; it does
not embed cache-policy decisions or declare an operation wrong merely because it is expensive.

### 11.2 Typed trace control

Extend automation/control with commands to:

- start tracing;
- describe recorder state and artifact path;
- freeze on an invariant or liveness failure;
- save/export without stopping the app;
- stop tracing without discarding the artifact; and
- request a composed native-frame capture.

Support trace-on-launch for stress runs. Automated and manually saved traces use the same schema and
viewer diagnostics.

### 11.3 Trace retention

Keep the bounded verbose ring for low-overhead detailed history, but add:

- the compact streaming action journal;
- periodic compact state checkpoints;
- overwrite counters and retained time range in every export;
- configurable diagnostic capacity for dedicated stress runs; and
- optional chunked asynchronous trace persistence only if measurement proves it does not perturb
  interaction materially.

Never imply that an in-memory trace can be recovered after a completely blocked or terminated
process unless it was already persisted externally.

## 12. Heartbeat and progress watchdog

Expose an atomic status snapshot readable by the controller without waiting for the UI event loop.
It includes:

- UI heartbeat/frame sequence and last-frame time;
- desired and committed workspace/scene generations;
- active navigation owner and phase;
- last source, semantic, GPU, and publication progress sequence/time;
- pending resource/reclaim identity;
- current charged, pinned, and budget bytes;
- active pointer/scroll interaction;
- trace/journal writer state; and
- latest terminal or invariant failure.

Classify at least:

1. **UI stall:** heartbeat stops.
2. **Pipeline stall:** heartbeat advances, but the current worker owner makes no progress and does
   not settle, yield, or fail.
3. **Interaction lock:** native/viewer input arrives, but the intended control state cannot advance
   despite valid authority.
4. **Progress churn:** progress counters advance but the latest desired generation cannot commit
   because obsolete work repeatedly wins resources.

Progress-based detection is primary. A generous wall-clock deadline still protects the runner, but
product performance qualification uses fresh/carry-over baselines and structural work bounds rather
than arbitrary sleeps.

On failure:

- if the UI responds, freeze/export trace and capture the composed frame;
- if the control service responds but UI does not, record heartbeat, process sample, RSS, and logs;
- if the process exits, preserve its partial manifest, journal, controller log, and exit status; and
- never wait indefinitely for the application to serialize an artifact after a proven UI stall.

## 13. Oracles and assertions

### 13.1 Semantic oracles

- desired and committed generations agree after settlement;
- every current request has an owner or typed terminal state;
- expected tracks/panels/loci and exact data products are present;
- selection, history, settings, and interaction state match the scenario;
- no stale generation publishes; and
- no critical or invariant failure occurs.

Feature-specific plans provide domain assertions such as exact read count, cache headroom, row
authority, variant truth, or annotation selection. The automation framework carries and reports
these assertions without hard-coding their policy.

### 13.2 Liveness oracles

- UI heartbeat advances during active work;
- a current owner advances phase/progress, yields, settles, cancels, or fails truthfully;
- accepted input changes its intended camera/control state when authority permits;
- pointer capture terminates on release, cancellation, focus loss, or target destruction;
- no orphan navigation/reclaim/publication transaction remains; and
- `Updating view…` cannot persist without an identifiable current owner and progress.

### 13.3 Performance evidence

Record distributions rather than a single elapsed time:

- input-to-camera and input-to-visible-update latency;
- worker phase durations;
- stale cancellation latency;
- preparations, replacements, publications, and retries per gesture;
- frame work and hitch distribution;
- cache/ledger charge and process RSS; and
- fresh-versus-carry-over comparisons on the same build and hardware.

Plans consuming the framework define their structural and relative thresholds. The framework must
not silently invent universal product latency figures.

### 13.4 Bounded visual oracles

Use internal composed-frame captures at before, held, and after points. Machine checks may include:

- unexpected full-region blank/white ratio;
- disappearance of a lane expected to remain committed;
- stale pixels outside a permitted transition region;
- missing accessibility controls; and
- gross geometry or clipping violations.

Use region masks and tolerances, not exact whole-screen hashes across fonts, DPI, and GPU drivers.
XCUITest screenshots support native chrome and routing evidence. They do not replace internal GPU
captures or user visual assessment.

## 14. Trace-derived initial scenario

The first checked-in scenario reproduces the motivating alignment workload without embedding a
cache fix in the harness.

### 14.1 Setup

1. Launch the release application with the recorded 4 GiB total cache and 400% browsing-buffer
   settings.
2. Load the local RNA BAM and matching reference/annotation session by fingerprint.
3. Navigate to BAP1 `chr3:52253682-52257430` and execute the preserved browsing setup.
4. Navigate to ACTB `chr7:5527146-5530709` and wait for exact row authority.

### 14.2 Recorded ACTB interaction

Replay normalized vertical thumb paths using the recorded timing curves:

1. 0% → 46.5% over 2.334 s;
2. 46.5% → 75.6% over 1.295 s;
3. 75.4% → 100% → 19.7% over 5.153 s;
4. 19.7% → 0% over 1.499 s;
5. 0% → 27.9% → 0% over 2.269 s;
6. 0% → 100% over 2.569 s;
7. 100% → 0% over 3.487 s; and
8. 0% → 69.9% → 0% over 3.886 s.

Then perform two horizontal overview gestures expanding the viewport to approximately
`chr7:5395882-5699754`.

### 14.3 Pre-fix preservation

Before the cache remediation, preserve reports showing the observed cache pressure, replacement
amplification, no-progress signature, or other reproduced failure. The exact 43-second stall may be
scheduling-sensitive. Classify by semantic signature rather than requiring identical wall time and
retain every seed and run outcome.

### 14.4 Framework assertions

The scenario asserts only framework-level facts directly:

- delivered interaction matches the compiled gesture sequence;
- the intended control receives and terminates each gesture;
- trace, journal, heartbeat, captures, and state artifacts are complete;
- a stall or critical failure is detected and classified; and
- a completed run reaches the final requested viewport or reports its typed outcome.

Cache, residency, and vertical-navigation correctness criteria belong to the separate cache-liveness
plan and are referenced by scenario assertion IDs.

## 15. Seeded stress and failure reduction

Build a state-aware generator over valid interaction states. Initial actions include:

- locus and contig navigation;
- vertical top/middle/bottom reversals;
- horizontal pan reversals;
- zoom before settlement;
- release while work is pending;
- focus loss/cancel during a drag;
- resize and scale changes;
- filter, sort, grouping, and appearance changes;
- multi-panel and multi-track activation;
- context-menu and mate-navigation operations; and
- configurable data/settings matrices supplied by consuming plans.

The generator records its expanded script. It cannot depend on hidden randomness inside the app.

On failure, delta-debug:

- action list;
- gesture paths and control points;
- repetition counts;
- durations, pauses, and cadence;
- setup operations;
- panels/tracks; and
- parameter matrices.

Reduction preserves a typed failure signature such as current-owner no-progress, scrollbar input
lock, stale publication, critical invariant, blank committed region, or process exit. Re-run the
minimized scenario deterministically, then several times with native drivers because OS scheduling
remains statistical.

## 16. Repository structure

Suggested structure:

```text
validation/interaction-stress/
  README.md
  scenarios/
  expected/
  reports/

tools/interaction-stress/
  controller/
  macos-xcuitest/
  macos-cgevent-driver/

apps/rgv-desktop/src/automation/
  gesture_script.rs
  gesture_runner.rs
  artifact_writer.rs

crates/rgv-core/src/
  automation.rs
  viewer_input.rs
```

Exact placement may follow existing module conventions. Shared schemas belong in a non-UI crate.
Native macOS drivers remain tools and cannot become production runtime dependencies.

## 17. Implementation phases and gates

### Phase 0 — Preserve evidence and define schemas

Deliverables:

- checked-in trace analysis and failure signatures;
- run-artifact manifest schema;
- compact action-journal schema;
- gesture-script schema;
- heartbeat/progress schema; and
- the initial trace-derived scenario specification.

Gate: schemas distinguish input, semantic state, liveness, performance evidence, visual artifacts,
and human review without embedding the motivating cache solution.

### Phase 1 — Complete observability

Deliverables:

- compact asynchronous action journal;
- worker phase spans;
- typed trace-control commands;
- atomic heartbeat/progress status;
- watchdog capture and partial-run recovery; and
- overhead/correlation tests.

Gate: a manual repeat produces a complete action journal and identifies any long-running worker
phase without materially changing interaction behavior.

### Phase 2 — Deterministic viewer-input replay

Deliverables:

- typed `ViewerInput` dispatcher;
- native adapter and shared viewer reducer;
- gesture compiler and event-loop runner;
- semantic barriers/assertions;
- composed held-frame capture; and
- reproducible run artifacts.

Gate: the trace-derived sequence runs through the same viewer reducer as native input without direct
camera mutation.

### Phase 3 — Scenario conversion and reduction

Deliverables:

- journal-to-script converter;
- seeded state-aware generator;
- typed failure signatures;
- deterministic delta-debugger; and
- regression scenario catalog.

Gate: a long failing run can be converted and reduced while preserving its semantic failure class.

### Phase 4 — Packaged macOS XCUITest

Deliverables:

- stable AccessKit identifiers;
- accessibility-tree regression tests;
- independent XCUITest project;
- staged-app launch configuration;
- element-relative native gesture execution; and
- native screenshot/artifact attachment.

Gate: core scenarios run against the packaged app and prove ordinary accessibility/AppKit/winit
routing. Full Xcode availability is required for this phase.

### Phase 5 — High-rate native stress

Deliverables:

- PID-targeted Core Graphics driver;
- Accessibility target discovery;
- momentum, reversal, focus, and held-drag sequences;
- dedicated-host permission/setup documentation;
- emergency cancellation; and
- repeated native-run classification.

Gate: minimized scenarios can be stressed at controlled high cadence without global untargeted
input or an unbounded run.

### Phase 6 — Qualification integration

Deliverables:

- reusable assertion plug-in/schema for feature plans;
- release-run matrix support;
- artifact summarisation and comparison;
- CI/local/nightly execution profiles;
- documented manual visual handoff; and
- the cache-liveness scenario fully integrated as the first consuming plan.

Gate: a consuming plan can declare its scenarios and semantic assertions without adding a new GUI
driver or bespoke artifact format.

## 18. Qualification matrix

### 18.1 Mechanical tests

- schema validation and backward compatibility;
- action-journal ordering and coalescing;
- identical native/replay viewer-reducer decisions;
- press/move/reversal/release/focus-cancel state machines;
- event-loop scheduling and cancellation;
- trace/journal/status correlation;
- heartbeat classification;
- artifact atomicity and partial-run preservation;
- semantic target resolution after resize/split panels;
- driver capability labelling; and
- deterministic generation and reduction.

### 18.2 Release integration

- held canvas pan with capture before, during, and after release;
- wheel zoom burst and reversal;
- fast read-scrollbar drag then pause;
- vertical wheel scrolling;
- locus and contig switching;
- resize/focus loss while work is pending;
- multi-panel and multi-track interaction;
- context-menu selection and dismissal;
- mate navigation and history; and
- feature-specific scenarios registered by other plans.

### 18.3 Native qualification

- packaged-app XCUITest on the supported macOS configuration;
- high-rate Core Graphics stress on a dedicated interactive host;
- repeated minimized failures;
- long seeded soak runs;
- process sample/profile capture for detected stalls; and
- user visual review after automated semantic/liveness gates pass.

## 19. Human visual gate

The framework must explicitly prompt the user when a consuming plan reaches visual qualification.
Provide:

- the build and session to launch;
- the scenario and expected behavior;
- the automated results already passed;
- controls/actions the user should perform if free browsing is required;
- the artifact location; and
- a short list of perceptual questions.

Do not automate mouse control and then infer that a brief screenshot proves continuous behavior.
Human assessment remains required for flicker, motion continuity, read legibility, substitutions,
chevrons, insertion rendering, Sashimi, hover, selection, and overall browsing feel.

## 20. Definition of done

This plan is complete only when all of the following are true:

1. Manual native interaction produces a complete compact action journal.
2. A journal can be converted into an inspectable resolution-independent gesture script.
3. Deterministic replay uses the same viewer-input reducer as native events.
4. Replay can hold, move, reverse, capture, release, cancel, resize, and change focus.
5. A controller can start RGV, start tracing, replay gestures, detect stalls, save artifacts, and
   exit without manual intervention.
6. Worker phase spans make every long-running current request attributable.
7. The compact action journal survives verbose trace-ring overwrite.
8. Heartbeat and watchdog distinguish UI stall, pipeline stall, input lock, and progress churn.
9. Failures preserve partial artifacts and a typed signature.
10. Seeded stress runs are reproducible and automatically reduced.
11. Stable accessibility identifiers support split panels and tracks without exposing reads as
    millions of accessibility nodes.
12. XCUITest drives the staged packaged app through ordinary native interactions.
13. The PID-targeted driver performs bounded high-rate stress on an authorised test host.
14. Deterministic and native drivers use the same script and artifact vocabularies while reporting
    their different fidelity.
15. The trace-derived BAP1→ACTB scenario runs end to end and supplies evidence to its consuming
    cache-liveness plan.
16. Other feature plans can register scenarios and assertions without bespoke GUI automation.
17. Automated captures are treated as supporting evidence, not automatic visual acceptance.
18. The user is prompted for and completes the required final visual reviews.

## 21. Explicit non-solutions

Do not implement this framework by:

- relying on AppleScript or absolute desktop coordinates as the primary driver;
- using high-level camera assignment to claim native gesture coverage;
- fabricating winit `WindowEvent` values and claiming they traversed the OS path;
- coupling replay to private mutable viewer fields;
- exposing every read as an accessibility node;
- depending on one driver for deterministic, packaged-native, and high-rate guarantees;
- using fixed sleeps as settlement or liveness assertions;
- treating exact whole-screen pixel hashes as the sole visual oracle;
- discarding failure artifacts when the application exits or blocks;
- adding unbounded trace logging that materially changes the behavior under test;
- letting an automation controller mutate viewer state off the UI thread;
- embedding cache, renderer, or biological correctness policy inside the generic harness; or
- claiming subjective visual correctness without user assessment.
