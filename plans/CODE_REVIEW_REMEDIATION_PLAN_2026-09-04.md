# RGV code-review remediation plan — 2026-09-04

Status: planned; none of the fixes in this document are implemented or qualified yet.

Baseline: `0ef9d56` (`feat: add native interaction recording and stress tooling`). This checkpoint
preserves the pending implementation reviewed on 2026-09-04. The review covered the working tree,
including its uncommitted changes, rather than only the preceding commit's diff.

## Outcome and scope

Resolve the ten review findings without weakening complete-read presentation, generation-based
query cancellation, bounded allocation, session compatibility, or authenticated local control.
The central architectural changes are:

- export alignment evidence from the same committed scene authority used for presentation;
- separate biological filtering from the budget for retained display data;
- give workspace mutation, loading, and saving explicit identities and commit boundaries; and
- retire background workers without joining them on the UI thread.

Transport and import fixes are also required. This plan does not expand format support, replace
the renderer, redesign the control protocol, or implement unrelated roadmap features.

Related design context:

- [Desktop module boundaries](../docs/architecture/0005-desktop-module-boundaries.md)
- [Unified alignment scene](../docs/architecture/0007-unified-alignment-scene.md)
- [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)
- [Native interaction stress testing](NATIVE_INTERACTION_STRESS_AUTOMATION_PLAN.md)
- [Validation guide](../docs/VALIDATION.md)

## Findings and evidence

Identifiers retain the order of the review. P1 means urgent correctness or credential exposure;
P2 means a functional defect; P3 means a lower-frequency robustness defect.

| ID | Priority | Finding | Evidence from review | Primary implementation area |
| --- | --- | --- | --- | --- |
| R01 | P1 | PNG/SVG exports omit alignment bodies and summaries | A settled window showed 25 alignments and coverage; successful semantic PNG/SVG exports omitted both | Desktop export commands, UI export action, scene projectors |
| R02 | P1 | Genotype display limits change variant filter results | 1,001 heterozygous records across 256 samples returned 977 records; the missing 24 returned in a narrower query | HTS variant query and genotype decoding |
| R03 | P1 | Native UI edits bypass unsaved-change detection | Source inspection: persisted edits do not consistently advance the revision used by workspace replacement | Desktop UI actions and workspace mutation |
| R04 | P1 | Custom authentication crosses redirect origins | A loopback redirect forwarded a configured `X-Api-Key` to another origin | HTTP range reader |
| R05 | P2 | Failed session application partially replaces live state | Source inspection: reference/review/assembly fields change before unsupported cytobands cause an error | Session controller |
| R06 | P2 | Engine replacement synchronously waits for workers | A 400 ms server delay blocked reference-engine destruction for approximately 405 ms | Reference/track engines and desktop retirement |
| R07 | P2 | Async save completion marks a newer revision as saved | Source inspection: completion uses the current revision and unconditionally replaces the current path | Session save tasks |
| R08 | P2 | Older session loads can replace newer requests | Source inspection: every completed load is applied without a request-generation check | Session load tasks |
| R09 | P2 | Read-ahead evicts the requested range block | A valid one-block cache with four-block read-ahead failed its first read | HTTP range cache |
| R10 | P3 | IGV notice truncation panics on Unicode | A long Unicode XML element name triggered `String::truncate` at a non-character boundary | Core IGV import notices |

The baseline `cargo test --workspace --lib --offline` run passed 1,240 tests, with one ignored.
Five temporary probes reproduced R02, R04, R06, R09, and R10. Those probes asserted the existing
broken behavior; permanent regression tests must assert the corrected behavior. R01 was reproduced
with the existing release executable and traced through the current source. The other four findings
still need executable reproductions before their fixes are accepted.

Temporary review files are not durable test fixtures. Recreate the small deterministic cases below
in the appropriate crate or validation runner; no acceptance gate should depend on `/tmp` artifacts
from the review session or on an external data server.

## Implementation sequence

Each work package should carry its reproducer, implementation, and relevant validation together.
The table defines independently mergeable packages and their dependencies.

| Package | Findings | Dependencies | Completion artifact |
| --- | --- | --- | --- |
| A: committed-scene exports | R01 | Baseline reproducer | Shared semantic export path and fixture comparison |
| B: exact variant filtering | R02 | Baseline reproducer | Filter evaluation independent of display budgets |
| C: authenticated range transport | R04, R09 | None | Origin-safe redirects and cache-capacity regressions |
| D: workspace and file-task identity | R03, R07, R08 | None | Shared document revision/epoch and ordered completion rules |
| E: asynchronous engine retirement | R06 | None | Nonblocking teardown and bounded retirement lifecycle |
| F: transactional session replacement | R05 | D and E | Prepare/validate/commit session application |
| G: Unicode import robustness | R10 | None | Character-boundary truncation regression |

Prioritize the P1 work in A–D. R09 can accompany C, and G is a small independent correction.
Finish E before F so atomic replacement does not introduce a synchronous teardown stall. Avoid
collecting all packages into one large refactor or reporting a finding fixed solely because an
unrelated broad test suite remains green.

## A. Export the committed alignment scene (R01)

Relevant code:

- [Automation snapshots](../apps/rgv-desktop/src/automation/commands.rs): snapshot dispatch and
  `full_active_panel_export_display`.
- [Desktop PNG export](../apps/rgv-desktop/src/ui/frame.rs): `export_png_requested` handling.
- [Export helpers](../apps/rgv-desktop/src/automation/export.rs):
  `semantic_export_display_list` and `display_list_svg_export`.
- [Display composition](../apps/rgv-desktop/src/display/mod.rs): native read labels and generic
  display composition.
- [CPU read projector](../crates/rgv-alignment-scene/src/cpu_projector.rs) and
  [CPU summary projector](../crates/rgv-alignment-scene/src/cpu_summary_projector.rs).

Implementation:

1. Capture an immutable export input containing the committed alignment lanes, contextual lanes,
   cameras, clips, and required style/reference authority for the requested scope. Reject pending,
   stale, or unavailable mandatory evidence explicitly. Pin the authority until export completes.
2. Project read geometry and coverage/allele/junction/modification summaries from that captured
   scene. Reuse scene projectors and shared style interpretation; do not reread alignments or
   rebuild a separate biological interpretation in the desktop exporter.
3. Route manual PNG export and semantic workspace/panel PNG/SVG snapshots through one export
   composition service. Keep transient hover state excluded. Preserve each scope's documented
   cropping and dimensions, including logical versus physical pixel behavior.
4. Build full-panel exports with an export camera rather than temporarily changing the live
   workspace layout or scroll state. Acquire any additional required pages through a bounded
   export job; refuse or explicitly report truncation if the complete scope cannot be supplied.
5. Keep export projection, rasterization, and file encoding off the UI thread where substantial
   work is involved. Account for pinned scene and export scratch memory; fail cleanly under pressure.
6. Preserve `NativeWindow` as the composed-window capture path. It remains useful for comparison,
   but does not substitute for fixing SVG or semantic exports.

Regression and acceptance:

- Reproduce with `benchmark-data/variants.bam`, `benchmark-data/variants.fa`, and
  `chrVariants:9951-10150`. Capture the settled native window, semantic PNG, and semantic SVG.
- Verify that semantic outputs contain read bodies and coverage at the expected genomic positions,
  not merely a nonzero file size, some rectangles, or floating labels.
- Cover insertions, deletions, splices, mismatches, supported specialist colors, derived summaries,
  multiple panels, fractional pan, vertical clipping, and full-panel scope using existing fixtures.
- Check structural geometry/style equivalence before rasterization, then inspect native-versus-export
  captures. Account for rasterizer differences rather than requiring identical antialiasing.
- Navigate or change selection while an export is running: the output must retain the captured
  committed authority, and completion must not move or replace live workspace state.

## B. Separate variant filtering from retained genotype limits (R02)

Relevant code: [variant source](../crates/rgv-hts/src/tracks/variants.rs), especially
`query_with_settings`, `decode_genotypes`, and `variant_record_matches`.

Implementation:

1. Evaluate configured genotype and FORMAT predicates against all selected samples needed to
   decide the record's eligibility, before applying display-retention budgets.
2. Use bounded evaluation scratch and short-circuit where predicate semantics permit it. Do not
   solve the defect by retaining the complete cohort payload or increasing the 250,000-cell cap.
3. Retain bounded genotype display data only after eligibility is known. Distinguish actual source
   missing values from payload omitted for display. Preserve existing ANY-within-clause and
   AND-between-clause semantics.
4. Audit header-sample, per-record sample, field-count, and field-value caps for the same problem.
   If a required predicate cannot be evaluated exactly, surface an explicit incomplete/error
   result; never count a record as biologically excluded because its predicate data was omitted.
5. Report filtering, record truncation, genotype display omissions, and evaluation unavailability
   separately through provenance and consumers, including exports and inspectors.

Regression and acceptance:

- Generate indexed BCF with 1,001 variants, 256 samples, and `0/1` at every sample/variant.
  A heterozygous query over all records must return 1,001 when the record limit permits it;
  display omissions may be reported, but `records_filtered` must be zero.
- Query positions `[977, 1001)` independently. Those same 24 records must also be present in the
  broad query; changing viewport width must not change their eligibility.
- Exercise budget boundaries, matching samples late in a cohort, explicit sample subsets,
  genotype/FORMAT clauses, missing GT/FORMAT values, and retained-data caps.
- Assert bounded retained memory and explicit failure when exact filter evaluation is unsupported.

## C. Fix range transport boundaries (R04, R09)

Relevant code: [HTTP range reader](../crates/rgv-hts/src/remote.rs), including
`RemoteAuthentication`, redirect policy, configuration validation, `fetch_blocks`, and `load_block`.

### R04: credential forwarding

1. Treat origin as scheme, host, and effective port. Capture the original authentication origin
   for the lifetime of the reader, including its opening probe and subsequent range requests.
2. Default to rejecting cross-origin redirects when authentication is configured. If compatibility
   requires following a redirected public object, implement an explicit policy that strips the
   configured credential and does not reattach it later in the chain.
3. Preserve authenticated same-origin redirects, the redirect count bound, HTTPS downgrade
   rejection, retries, cancellation, and redacted error messages.

Regression and acceptance:

- Use two loopback origins. Origin A redirects to B; B must never receive the custom `X-Api-Key`.
  Test opening probes and later range fetches, multi-hop chains, and redirects back to A.
- Include bearer and custom-header authentication, default/explicit ports, and same-origin success.
  Exercise scheme-change/downgrade decisions without requiring internet access.
- Errors and diagnostics must not contain credential values or sensitive URL query parameters.

### R09: read-ahead exceeding cache capacity

1. Limit a coalesced fetch by the number of blocks the cache can retain, or retain the requested
   block independently before admitting read-ahead. Never evict the requested block before serving it.
2. Define how minimum read-ahead behaves when the cache is smaller: cap speculative read-ahead
   to capacity while preserving a valid demand read, and document that effective behavior.
3. Include disk blocks, the active in-memory block, and coalesced response scratch in the intended
   resource accounting. Preserve exact byte content, EOF handling, and cancellation.

Regression and acceptance:

- Serve a 32-byte object using 8-byte chunks, an 8-byte cache, and maximum coalescing of four
  chunks. Reading into a 32-byte buffer must make progress and eventually return all bytes.
- Cover minimum coalescing greater than cache capacity, unaligned seeks, partial final blocks,
  revisiting evicted data, and mixed existing/new blocks. Assert cache bounds and meaningful stats.

## D. Give workspace edits and file jobs explicit identity (R03, R07, R08)

Relevant code: [UI actions](../apps/rgv-desktop/src/ui/frame.rs),
[navigation](../apps/rgv-desktop/src/workspace/navigation_controller.rs),
[file tasks](../apps/rgv-desktop/src/app/tasks.rs), and
[automation commands](../apps/rgv-desktop/src/automation/commands.rs).

### Shared ownership model

Introduce a process-local workspace epoch and a persisted-document revision, plus the revision of
the snapshot last saved for that epoch. Keep navigation query generations and evidence/selection
revisions distinct. Preserve the externally visible meaning of `workspace_revision`; adding a
document revision is preferable to silently repurposing an existing optimistic-concurrency field.

Use a shared workspace mutation boundary for UI and automation actions. A successful mutation of
serialized state advances the document revision once; failed actions and no-ops do not. Classify
selection/hover state separately when it is not persisted. Audit track names/settings, presentation,
loci/panels/scroll state, genome/reference configuration, regions, review items, and metadata.

### R03: dirty detection

- Derive dirty state from the active epoch's document revision and saved revision. Route all
  persisted mutations through the shared boundary, including native controls and keyboard paths.
- Keep `Reject`, `Discard`, and `SaveThenOpen` policies explicit. A dirty workspace must not bypass
  the requested policy because an edit came from the native UI instead of automation.
- Define initial states for native open, new workspace, IGV import, and autosave recovery. Imports
  and recovered edits need an explicit save destination/state; they must not accidentally inherit
  a previous workspace's saved revision.

### R07: save completion

- Capture workspace epoch, document revision, destination, and save request identity when the
  immutable session snapshot is created.
- On success, record only that snapshot's saved revision. If the document advanced during the
  write, it stays dirty. Never use the completion-time revision as the saved revision.
- A completion from an obsolete workspace may report its file result but cannot change the
  active workspace's path or dirty state. Order competing Save As completions by request identity.
- Serialize writes targeting the same normalized destination across manual, automation, and
  save-before-open paths. An older in-flight snapshot must not overwrite a newer saved document.

### R08: load completion

- Assign each load a monotonically increasing request identity and its originating workspace
  context. Only the current request may reach the commit stage.
- A newer request, successful workspace replacement, or explicit cancellation invalidates older
  pending loads. Apply the same rule across native open, automation, import, and recovery paths.
- Check unsaved-change policy again at commit if the active document changed while loading.
  A failed newer request must not cause an older superseded load to apply later.
- Keep file reading/parsing and `SaveThenOpen` file work off the UI thread; retain a visible pending
  operation and an explicit error rather than blocking input during I/O.

Regression and acceptance:

- Save, then rename a track or navigate through native controls; automation replacement using
  `Reject` must return `UnsavedChanges`. Cover representative persisted edits and no-op controls.
- Hold a save worker at a deterministic barrier, edit the document, complete the worker, and
  verify the written snapshot and still-dirty live state. Repeat after opening another workspace.
- Hold load A, finish load B, then release A; B must remain active. Repeat with failed B, import,
  recovery, and mixed UI/automation requests.
- Test edits occurring during load, out-of-order Save As completions, save failures, and
  concurrent writes to the same destination.
- Use injected completions or synchronization barriers rather than sleep-based race tests.

## E. Retire reference and track engines asynchronously (R06)

Relevant code: [reference engine](../crates/rgv-engine/src/reference_engine.rs),
[track engine](../crates/rgv-engine/src/track_engine.rs), existing
[alignment retirement](../crates/rgv-engine/src/alignment_scene_engine.rs), and desktop engine
replacement in the session/alignment controllers.

Implementation:

1. Add an explicit cancellation/shutdown signal and a retirement API for reference and auxiliary
   engines. Close output/control ownership, abandon queued obsolete work, and return immediately
   to the UI. Join workers through a retirement owner that can be polled asynchronously.
2. Use the same lifecycle policy for replacement, removal, session changes, and error cleanup.
   Audit implicit drops during partial construction and ordinary assignment, not just named removal.
3. Check cancellation during opening/query work and between bounded operations. Blocking HTTP
   operations still need finite transport deadlines; retirement must not claim instantaneous
   cancellation of an already blocked system or library call.
4. Bound the retirement backlog and retained resources. Under pressure, defer/reject a new open
   with visible status while continuing UI processing; do not accumulate unbounded detached workers
   or silently leak their cached data and file descriptors.
5. Define final application shutdown separately from ordinary UI retirement and validate that
   outstanding work/resources are released. Keep late events from touching replacement state.

Regression and acceptance:

- Hold a local HTTP response open behind a barrier. Retiring/replacing its reference engine must
  return before the response is released; then release it and verify eventual worker cleanup.
- Repeat for an auxiliary track and for a busy input queue. Include output backpressure and errors.
- Repeatedly replace sources while recording UI heartbeat/input progress and retirement count.
  Verify continued UI progress and eventual bounded resource usage without a hardware-specific
  405 ms timing threshold.

## F. Apply session replacement as a transaction (R05)

Relevant code: [session controller](../apps/rgv-desktop/src/workspace/session_controller.rs),
[session validation](../crates/rgv-core/src/session.rs), and the file-task identity from package D.

Implementation:

1. Split loading into parse, prepare/validate, and commit. Preparation produces owned replacement
   state without changing live filters, assembly metadata, reference configuration, panels,
   review items, metadata selections, or destination paths.
2. Validate desktop-specific capability restrictions before commit, including unsupported remote
   cytobands and filter/presentation/controller construction failures. Core JSON validation alone
   is insufficient for application-level acceptance.
3. Recheck the current load identity and dirty policy, then install the prepared workspace in one
   UI transaction. Publish panel identity and geometry together, advance the workspace epoch,
   and retire the previous engines through package E.
4. Distinguish a failed replacement transaction from a successfully opened workspace containing
   missing/unavailable sources. Preserve the existing partial-project recovery policy and typed
   per-source failures; remote availability is not a requirement to atomically install valid state.
5. Route native session open, automation open/new, IGV import, and recovery through the same
   application transaction rather than maintaining separate mutation sequences.

Regression and acceptance:

- Create a valid session B that changes reference, assembly, and review items but specifies remote
  cytobands. Attempt to apply it over workspace A. The operation must fail with A's persisted
  document, panels, engine identities, path, revisions, and usable evidence unchanged.
- Inject failures at other preparation boundaries and verify cleanup of prepared resources.
- A successful replacement must expose a consistent new epoch, panel geometry, and configuration
  immediately; source readiness may settle asynchronously with explicit per-source status.

## G. Truncate IGV notices at UTF-8 boundaries (R10)

Relevant code: [core session import](../crates/rgv-core/src/session.rs), `insert_igv_notice`.

Implementation and acceptance:

- Replace raw `truncate(512)` with a byte-bounded character-boundary operation. Reuse an existing
  appropriate helper if available while preserving notice-count and notice-byte caps.
- Import `<Session locus="chr1:1-100"><NAME/></Session>` with `NAME` equal to 300 repetitions of
  `é`; the importer must return a result without panic and retain a valid bounded notice.
- Test ASCII, multibyte boundaries, exactly-at-limit text, and repeated notices. Keep output valid
  UTF-8 and preserve deduplication and omitted-notice reporting.

## Qualification and completion

Per package, add failing regressions for the corrected outcome, implement the fix, and run focused
crate tests. For the integrated implementation, run the repository's required formatting, lint,
workspace, and fixture checks; include the integration tests excluded by the baseline `--lib` run.
Typical mechanical commands, from the repository root, are:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Use the existing [validation procedures](../docs/VALIDATION.md) for fixture qualification and
relevant interaction benchmarks. Native/macOS validation should use a freshly built executable,
isolated settings, temporary outputs, and local fixtures. Keep its results separate from headless
unit-test claims. Rendering must be checked on supported backends, and session/file semantics on
supported platforms; mark unavailable platform gates pending rather than claiming they passed.

Acceptance requires all of the following:

- [ ] R01: native and semantic exports contain the same committed alignment evidence for each scope.
- [ ] R02: viewport width and display caps cannot change exact variant-filter eligibility.
- [ ] R03: every persisted UI/automation edit participates in dirty-state tracking.
- [ ] R04: no configured credential reaches an unauthorized redirect origin.
- [ ] R05: failed replacement cannot partially mutate the active workspace.
- [ ] R06: ordinary engine replacement never joins a blocked worker on the UI thread.
- [ ] R07: save results apply only to the snapshot and workspace that produced them.
- [ ] R08: superseded loads cannot replace a newer workspace or override intervening edit policy.
- [ ] R09: every valid range/cache configuration can serve its requested bytes within its bounds.
- [ ] R10: bounded IGV notices remain valid UTF-8 without panics.
- [ ] Regression, fixture, export, and lifecycle evidence is recorded against the fixing commits.
- [ ] Existing session/control compatibility and resource limits remain intact; any intentional
      observable policy changes are documented in the relevant user/API guides.

Update this document with commit references and actual gate results as work lands. A green baseline
suite, a successful export response, or a passing stress run alone does not close these findings.
