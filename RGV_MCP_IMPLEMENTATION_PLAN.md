# RGV MCP implementation plan

Date: 2026-08-23

## Purpose

RGV already exposes a versioned automation vocabulary and an authenticated loopback control
service. This plan adds an agent-facing Model Context Protocol (MCP) server that lets an MCP client
inspect and manipulate a live RGV desktop window, wait for real genomic queries and rendering to
settle, inspect the resulting state, and capture visual or structured evidence.

The delivered component will be an MCP **server**, tentatively named `rgv-mcp`. The agent host is
the MCP client. The server will adapt MCP tools to RGV's existing local-control service rather than
introducing a second viewer mutation path.

## Intended outcome

An agent connected to `rgv-mcp` must be able to:

- discover whether a live RGV window is reachable and protocol-compatible;
- inspect the current genome, panels, loci, tracks, selections, readiness, and visible failures;
- navigate to exact genomic intervals;
- pan and zoom an existing panel using explicit, deterministic viewport operations;
- add, remove, show, hide, resize, group, sort, and filter tracks;
- select a panel, track, genomic position, or evidence item;
- wait until the actual viewer generations have settled;
- retrieve structured evidence about the active selection;
- capture the visible workspace or a panel as an image the agent can inspect;
- export bounded genomic evidence and save the workspace inside the configured control root; and
- receive stable, structured errors without scraping status text or application logs.

All successful mutations must affect the actual live application state and therefore be visible in
the RGV window.

## Non-goals for the first release

The first release will not:

- simulate pointer movement, clicks, scroll wheels, or key presses;
- expose RGV on a LAN or public network;
- replace the existing JSON batch format or command-line automation;
- create a second genomic query or rendering implementation in the MCP process;
- let the MCP server read arbitrary files outside the configured control root;
- embed model inference or autonomous decision-making inside RGV;
- support multiple simultaneous writers to one RGV window;
- manage multiple RGV windows through one MCP process; or
- require RGV to be running as a headless application.

MCP-managed launch and shutdown of the desktop application is a later milestone. The first release
attaches to an explicitly started RGV window.

## Current baseline

The implementation should extend, not bypass, the following existing facilities:

- `rgv-core::AutomationBatch` is schema-versioned, bounded, validated, serializable, and used by
  both batch files and live control.
- `rgv-core::AutomationCommand` already covers workspace creation, genome and track loading,
  removal, absolute locus navigation, panels, options, grouping, sorting, filtering, selection,
  settlement, snapshots, structured exports, saving, and exit.
- The desktop automation executor runs commands on the real `Viewer`, stops at the first failure,
  and returns stable command results and error codes.
- `Viewer::automation_is_settled` checks pending work and generation identity across reference,
  alignment, and auxiliary-track projections. It does not treat elapsed time as completion.
- The opt-in control service binds to IPv4 loopback, requires a bearer secret loaded from a
  protected file, bounds request size and duration, serializes requests, and confines local paths
  to an explicit control root.
- Snapshot and evidence export already run off the UI thread and have bounded output formats.

The main functional gap is observation. The current unauthenticated status endpoint deliberately
reveals no workspace state, and most detailed information is available only by exporting a file.
An effective agent needs structured read operations so it can observe, act, and verify.

## Architectural principles

1. Keep one authoritative viewer mutation path: MCP tools become automation batches.
2. Keep one authoritative readiness definition: MCP waits use the existing settled-state logic.
3. Put shared command and result models in `rgv-core`, not in the MCP crate.
4. Keep live viewer-state extraction in `rgv-desktop`, where the state is owned.
5. Keep MCP transport, tool schemas, HTTP adaptation, and artifact presentation in `rgv-mcp`.
6. Prefer typed, task-oriented tools over asking a model to construct the raw batch wire format.
7. Return machine-readable state alongside concise human-readable summaries.
8. Treat screenshots and structured evidence as complementary verification channels.
9. Preserve loopback binding, secret-file handling, path confinement, redaction, and bounded output.
10. Make every behavioural increment independently testable through the existing control service
    before exposing it through MCP.

## Target architecture

```text
MCP host and agent
      |
      | MCP over stdio
      v
apps/rgv-mcp
  - typed tool schemas
  - connection and protocol checks
  - AutomationBatch construction
  - AutomationRunReport translation
  - bounded artifact reading
      |
      | authenticated HTTP on 127.0.0.1
      v
rgv-desktop local-control service
      |
      | serialized ControlRequest
      v
Viewer automation executor
      |
      +--> workspace, panels, tracks, queries, selection
      +--> deterministic settlement
      +--> snapshots and structured exports
```

### Why a separate process

`rgv-mcp` should be a separate workspace binary under `apps/rgv-mcp`.

- Local MCP hosts conventionally spawn stdio servers as child processes.
- MCP protocol handling and dependencies stay out of the rendering process.
- The adapter can be tested against a fake control service.
- RGV's HTTP control API remains usable by non-MCP automation.
- A failure or malformed MCP request cannot directly corrupt GPU or UI runtime state.
- The viewer remains disabled for external control unless launched with the existing explicit
  control flags.

The stdio server must reserve stdout exclusively for MCP protocol messages. Diagnostics and tracing
must go to stderr.

## Connection and lifecycle model

### Initial attachment mode

The user starts a viewer explicitly:

```sh
rgv-desktop \
  --control-port 60151 \
  --control-secret-file /path/to/rgv-control.secret \
  --control-root /path/to/review-workspace
```

The MCP host starts `rgv-mcp` with non-secret connection configuration:

```sh
rgv-mcp \
  --control-url http://127.0.0.1:60151 \
  --control-secret-file /path/to/rgv-control.secret
```

Equivalent narrowly named environment variables may be supported for MCP-host configuration:

- `RGV_MCP_CONTROL_URL`;
- `RGV_MCP_CONTROL_SECRET_FILE`; and
- `RGV_MCP_ARTIFACT_DIRECTORY`, if a non-default artifact subdirectory is required.

The secret value itself must not be accepted as a command-line option, environment variable,
tool argument, log field, or MCP result. Both processes read the same protected file.

At startup, `rgv-mcp` should call `/v1/status`, verify loopback addressing and the supported control
schema, and retain a clear unavailable state if RGV is not yet running. The MCP process should not
exit merely because the viewer is temporarily unavailable; a later tool call may reconnect.

### Later managed mode

A later milestone may allow `rgv-mcp` to launch a desktop process. Before doing so, add an atomic
`--control-endpoint-file` facility to RGV that publishes only bounded non-secret discovery data:

```json
{
  "schema_version": 1,
  "pid": 12345,
  "address": "127.0.0.1:54321",
  "application_version": "0.1.0"
}
```

Do not discover a dynamically allocated port by scraping human-readable logs. Managed mode must
also define process ownership, early startup failure, unexpected viewer exit, and whether closing
the MCP connection closes the owned viewer. These concerns are deliberately outside the initial
release.

## Required control-plane additions

The following additions should be implemented in the shared automation vocabulary before their MCP
tools are written.

### 1. Describe workspace

Add a read-only `DescribeWorkspace` command. It should return a bounded typed snapshot containing:

- application and automation schema versions;
- genome/assembly name and reference readiness;
- `settled` state;
- ordered panel IDs, loci, detail levels, and the active panel ID;
- ordered stable track IDs, names, kinds, visibility, collapse state, height, and lifecycle;
- active grouping, sorting, and filters where applicable;
- current semantic selection;
- active non-sensitive status or failure summaries; and
- explicit truncation metadata if any collection is bounded.

It must not return source credentials, signed query parameters, read names, sequences, arbitrary
tags, local paths, or the control secret. Source identity should be absent by default; if later
needed, expose only the existing redacted representation.

The snapshot should be represented by types in `rgv-core` rather than an undocumented
`serde_json::Value`. This provides a reusable compatibility boundary for HTTP, MCP, tests, and
future clients.

### 2. Describe selection

Add a read-only `DescribeSelection` command that returns bounded structured information for the
current semantic selection. Depending on track kind, this may include:

- panel, track, and genomic position identity;
- pileup counts and alleles;
- selected alignment evidence with the same privacy and sampling bounds as the existing export;
- variant and genotype fields intended for the inspector;
- quantitative or feature values;
- junction or modification summaries; and
- a clear `no_selection`, `not_ready`, or `unsupported_selection` outcome.

Reuse the existing inspector/export snapshot construction. Do not create a separate interpretation
of genomic records solely for MCP.

### 3. Explicit viewport operations

Absolute locus navigation already controls zoom because interval width defines the zoom level. Add
typed relative viewport operations so an agent can also test interactive navigation behaviour:

```rust
enum AutomationViewportOperation {
    PanBases { delta: i64 },
    PanFraction { fraction: f64 },
    Zoom {
        factor: f64,
        anchor_position: Option<u64>,
    },
    FitInterval { start: u64, end: u64 },
}
```

Expose this through a command containing an explicit `panel_id`. Define semantics precisely:

- a zoom factor greater than `1.0` zooms in and less than `1.0` zooms out;
- factors must be finite and bounded, initially `0.01..=100.0`;
- an absent anchor uses the current viewport centre;
- panning and zooming clamp to contig bounds;
- resulting intervals must remain non-empty;
- `PanFraction` is relative to the current interval width;
- coordinates remain zero-based internally and are presented as conventional one-based loci; and
- mutations advance the same generation clocks and request the same production queries as UI
  navigation.

Add exact interval results to the command output so the agent does not need to infer how clamping
was applied.

### 4. Status capability information

Extend the non-sensitive `/v1/status` response with:

- application version;
- control schema version;
- a stable service name; and
- supported capability names such as `workspace_state`, `selection_state`, and
  `relative_viewport`.

Do not include live workspace contents in this unauthenticated response.

## MCP tool surface

The first release should expose focused tools. Tool names below are provisional but should remain
short, stable, and consistently prefixed.

| Tool | Mutates viewer | Purpose |
|---|---:|---|
| `rgv_status` | No | Report connection, application version, schema compatibility, and capabilities. |
| `rgv_get_workspace` | No | Return the bounded structured workspace snapshot. |
| `rgv_get_selection` | No | Return structured evidence for the current semantic selection. |
| `rgv_navigate` | Yes | Set one or more exact loci and optionally wait for settlement. |
| `rgv_change_viewport` | Yes | Pan, zoom, or fit an explicit panel and return its exact resulting locus. |
| `rgv_load_track` | Yes | Load a typed local or supported remote track and return its stable ID. |
| `rgv_remove_track` | Yes | Remove one stable track ID with an explicit destructive annotation. |
| `rgv_configure` | Yes | Apply bounded typed workspace, panel, or track settings. |
| `rgv_group_alignments` | Yes | Apply a typed grouping mode to stable alignment track IDs. |
| `rgv_sort_alignments` | Yes | Apply a typed sort mode and direction. |
| `rgv_filter_alignments` | Yes | Apply the existing validated filter expression. |
| `rgv_select` | Yes | Select a panel, track, or genomic position. |
| `rgv_capture_view` | No | Capture a settled PNG and return MCP image content plus metadata. |
| `rgv_export` | No | Produce a bounded structured export and return an artifact reference. |
| `rgv_save_workspace` | Writes file | Save a project under the configured control root. |
| `rgv_execute_batch` | Depends | Advanced escape hatch for supported automation commands. |

### Tool design rules

- Every mutating tool should accept `wait_until_settled`, defaulting to `true` when the operation
  triggers genomic queries.
- Settlement timeout should be explicit and bounded by the existing automation limits.
- IDs must use positive integers and must never be inferred from display order when a stable ID is
  available.
- Locus strings should follow RGV's existing user-facing coordinate parser.
- Source inputs should use a simple tagged schema such as `{kind: "local", path: "..."}` or
  `{kind: "remote", url: "..."}`. The adapter translates this to the existing Rust enum wire
  representation.
- Enumerated modes and known configuration keys should appear as JSON Schema enums rather than
  unconstrained strings wherever practical.
- Results should contain a short text summary and structured content. Agents should not need to
  parse prose to obtain IDs, loci, readiness, or errors.
- Tool descriptions must say that changes affect the visible live RGV window.
- Mutation, read-only, idempotence, and destructive annotations should be accurate.

### Raw batch escape hatch

`rgv_execute_batch` is useful for forward compatibility and complex atomic sequences, but it must
not be the primary interface offered to an agent.

For the initial release:

- validate the batch with the shared schema before sending it;
- reject `Exit` unconditionally;
- reject `NewWorkspace` unless `allow_destructive` is explicitly true;
- preserve command ordering and first-failure semantics;
- preserve root-confined input and output paths;
- return every command result, including skipped results; and
- limit the accepted command count to a smaller MCP-specific default, initially 100, even though
  the underlying batch format permits more.

The advanced tool must not become a way to evade typed-tool validation or artifact bounds.

## Images, exports, and artifacts

### Image capture

`rgv_capture_view` should:

1. optionally wait for the workspace to settle;
2. request a PNG snapshot using the existing snapshot command;
3. write it into a dedicated MCP artifact directory under the configured control root;
4. verify that the resulting regular file is within the expected directory and size bound;
5. read it as bounded bytes;
6. return it as MCP `image/png` content; and
7. include structured metadata for scope, dimensions, genome, loci, and truncation.

Use PNG as the interoperable image result. SVG may remain available as a file artifact for
deterministic comparison, but should not be the default visual payload sent to an agent.

Artifact names should use server-generated random or monotonic identities. A tool argument must not
be able to choose an arbitrary temporary path. Automatic cleanup should only remove files created
by the MCP process inside its own dedicated artifact directory. User-requested durable exports and
saved projects must not be cleaned automatically.

### Structured exports

`rgv_export` should use the existing export implementations. Initially return:

- export kind;
- resolved root-relative artifact path;
- byte count;
- media type;
- truncation/sampling provenance where available; and
- either bounded text content for small textual exports or an MCP resource/file reference.

Do not silently copy unrestricted genomic exports into model context. Default inline text should be
bounded conservatively, with larger results left as artifacts the agent can read deliberately.

## Error model

The MCP adapter should preserve distinctions between:

- MCP input/schema errors;
- control authentication or connection errors;
- unsupported control schema versions;
- the viewer being busy or shutting down;
- invalid RGV commands;
- missing genomic resources;
- invalid viewer state;
- settlement timeouts;
- bounded artifact I/O failures; and
- internal failures.

Each failed tool call should return a stable adapter error code, a concise message, retry guidance
where appropriate, and the underlying RGV error code when one exists. It must not return bearer
credentials, unredacted remote URLs, private read data, or unrestricted filesystem paths.

A report with `succeeded: false` is a tool-level failure even when the HTTP request itself returned
200. Skipped commands must remain visible to the caller.

## Concurrency and cancellation

The viewer already serializes control batches. `rgv-mcp` should additionally use one async mutex or
equivalent request gate per connected viewer so that calls from a client receive predictable local
ordering rather than competing for the viewer queue.

For the initial release:

- one tool call maps to one HTTP request;
- cancellation stops waiting for the MCP caller but does not claim that an already accepted viewer
  mutation was rolled back;
- after cancellation, the adapter must query workspace state before reporting the next mutation as
  if no earlier work occurred; and
- MCP progress notifications may report `submitted`, `waiting_for_viewer`, `settling`, and
  `reading_artifact` when supported by the client.

Do not adopt MCP task extensions as a prerequisite for the first release. The existing bounded
synchronous request/reply path is sufficient for normal navigation and capture. Task support can be
evaluated later for long exports or managed lifecycle operations.

## Security and privacy requirements

The MCP work must preserve or strengthen the current control boundary:

1. RGV control remains opt-in and loopback-only.
2. The secret remains in a protected file and is never exposed to the agent.
3. `rgv-mcp` refuses a control URL whose resolved address is not loopback.
4. Local sources, snapshots, exports, and saves remain confined to the selected control root.
5. The adapter uses server-generated paths for temporary captures.
6. Remote credentials and signed URL components use existing redaction rules.
7. Workspace and selection snapshots exclude read names and sequences by default.
8. Outputs have explicit byte and collection limits with truncation metadata.
9. `Exit` is not exposed in the initial MCP tool surface.
10. Destructive operations are separate, accurately annotated tools.
11. stdout contains only MCP protocol traffic; logs use stderr and structured redaction.
12. No telemetry or genomic content is transmitted outside the local MCP and RGV processes.

The implementation should include adversarial tests for path traversal, symlink handling at the
artifact boundary, remote URL redaction, oversized response bodies, malformed JSON, wrong secrets,
non-loopback URLs, and unexpected content types.

## Proposed source layout

```text
apps/rgv-mcp/
  Cargo.toml
  src/
    main.rs              # CLI, tracing-to-stderr, stdio server startup
    lib.rs               # testable server construction
    config.rs            # URL and secret-file configuration
    control_client.rs    # bounded authenticated HTTP adapter
    errors.rs            # stable MCP-facing error translation
    artifacts.rs         # confined paths, bounded reads, cleanup ownership
    server.rs            # MCP capabilities and server metadata
    tools/
      mod.rs
      status.rs
      workspace.rs
      navigation.rs
      tracks.rs
      configuration.rs
      selection.rs
      capture.rs
      export.rs
      batch.rs
  tests/
    protocol.rs
    fake_control.rs
```

Shared additions should remain in the existing feature-oriented locations:

```text
crates/rgv-core/src/automation.rs
  - new commands
  - viewport operation types
  - typed workspace and selection snapshots

apps/rgv-desktop/src/automation/commands.rs
  - command execution and viewport mutation

apps/rgv-desktop/src/inspection/
  - reuseable selection snapshot construction

apps/rgv-desktop/src/control_service.rs
  - capability-rich non-sensitive status response
```

If the core automation file becomes difficult to navigate, split models, commands, reports, and
validation into an `automation/` module while preserving current public re-exports. Do not combine
that mechanical split with behavioural changes in one review unless necessary.

## Dependency approach

- Use the official Rust MCP SDK (`rmcp`) and pin a stable release in `Cargo.lock`.
- Use the SDK's stdio server transport and JSON Schema support.
- Reuse `serde`, `serde_json`, `tokio`, `reqwest`, `tracing`, and `thiserror` through workspace
  dependencies where their required features are compatible.
- Avoid adding an HTTP server to `rgv-mcp`; it is a stdio MCP server and an HTTP client.
- Keep the adapter free of GPU, egui, HTS, layout, and render dependencies.
- Prefer generated JSON Schemas from typed Rust arguments, with snapshot tests locking the public
  tool contract.

Before selecting an SDK version, confirm its minimum supported Rust version and protocol support
against the workspace's Rust 1.93 baseline. Record the selected MCP protocol and SDK versions in
the MCP crate documentation.

## Implementation phases

### Phase 0: lock the public boundary

Tasks:

1. Record representative JSON for workspace state, selection state, and viewport operations.
2. Decide maximum collection sizes and inline artifact byte limits.
3. Define stable capability and error-code names.
4. Define coordinate semantics and zoom-factor direction in tests.
5. Add an architecture decision record for the separate stdio adapter over loopback control.

Exit criteria:

- The new core types and tool inputs have reviewed examples.
- Privacy fields and output bounds are explicit.
- No unresolved coordinate or lifecycle ambiguity remains for the initial release.

### Phase 1: close control-plane observability gaps

Tasks:

1. Add typed workspace snapshot models to `rgv-core`.
2. Implement `DescribeWorkspace` in the live automation executor.
3. Add typed selection snapshot models.
4. Reuse inspector/export construction to implement `DescribeSelection`.
5. Add typed relative viewport operations and exact resulting-interval output.
6. Extend `/v1/status` with non-sensitive versions and capabilities.
7. Add serialization, validation, redaction, and live executor tests.
8. Extend the checked-in automation fixtures where appropriate.

Exit criteria:

- A plain authenticated HTTP client can observe, navigate, pan, zoom, select, and verify a live
  window without MCP.
- Existing automation batches remain compatible.
- Workspace and selection results contain no prohibited fields.
- Existing deterministic settlement behaviour is unchanged.

### Phase 2: create the minimal MCP server

Tasks:

1. Add `apps/rgv-mcp` to the workspace.
2. Implement CLI/environment configuration and protected secret-file reading.
3. Implement a bounded authenticated control client.
4. Implement protocol handshake and compatibility checks.
5. Start an MCP stdio server with stdout isolation and stderr tracing.
6. Expose `rgv_status` and `rgv_get_workspace`.
7. Add fake-control and in-process MCP protocol tests.

Exit criteria:

- MCP Inspector or an equivalent protocol harness can list and call both tools.
- The server starts while RGV is unavailable and reconnects on a later call.
- Wrong secrets, incompatible schemas, non-loopback URLs, and malformed responses are distinct
  safe errors.
- No secret or protocol-corrupting log line appears on stdout.

### Phase 3: live manipulation tools

Tasks:

1. Implement exact navigation and relative viewport tools.
2. Implement track load/remove tools.
3. Implement typed configuration, grouping, sorting, and filtering tools.
4. Implement semantic selection and selection inspection tools.
5. Add default settlement to query-triggering mutations.
6. Add structured result and error mapping.
7. Add the bounded raw batch escape hatch.

Exit criteria:

- Each tool visibly changes the live window where expected.
- Every mutation returns the resulting stable IDs or exact loci.
- An agent can perform an observe-act-settle-observe loop without using raw JSON.
- `Exit` cannot be invoked through MCP.
- Concurrent tool calls receive deterministic ordering.

### Phase 4: visual and export evidence

Tasks:

1. Implement the confined MCP artifact directory.
2. Implement PNG capture returned as MCP image content.
3. Implement structured export with bounded inline text and artifact references.
4. Implement workspace saving under the control root.
5. Add cleanup ownership and shutdown behaviour for temporary captures.
6. Test full-panel truncation and output-size failures.

Exit criteria:

- An MCP client can see the same settled view shown in the RGV window.
- Repeated deterministic SVG captures remain byte-identical where already guaranteed.
- Temporary cleanup cannot affect user-created files.
- Large evidence is not silently injected into model context.

### Phase 5: native end-to-end qualification

Build a deterministic native acceptance sequence using the existing generated corpus:

1. Start RGV with local control, an isolated settings path, a fixed test root, and a protected
   generated secret.
2. Connect through the MCP protocol, not directly to HTTP.
3. Verify initial workspace state.
4. Load the generated mixed BAM and matching reference.
5. Navigate to a known locus and wait for settlement.
6. Zoom in around a truth position and verify the exact resulting interval.
7. Pan, then verify that stale earlier generations never replace the final view.
8. Apply grouping, sorting, filtering, and display configuration.
9. Select a known position and verify structured evidence.
10. Capture visible and full-panel images.
11. Export representative alignment, pileup, modification, junction, variant, and selected
    evidence where the fixture supports them.
12. Save and reopen the resulting project through the ordinary desktop path.
13. Exercise invalid track, invalid locus, timeout, busy, and shutdown behaviour.
14. Verify every retained artifact and report is credential-safe and path-confined.

Exit criteria:

- The complete sequence passes twice against a release desktop build.
- Reviewed deterministic artifacts match on repeated runs.
- The MCP path and direct automation path produce equivalent final workspace state.
- Failure cases return stable errors and leave the viewer recoverable.
- No private genomic identifiers, source credentials, or unrestricted paths enter reports.

### Phase 6: documentation and packaging

Tasks:

1. Document how to create the protected secret and start RGV in control mode.
2. Document generic stdio MCP-host configuration.
3. Add one validated example for the first supported host.
4. Document every tool, coordinate convention, mutation effect, output bound, and error code.
5. Add troubleshooting for unavailable viewer, wrong secret, incompatible schema, busy state, and
   artifact permissions.
6. Include `rgv-mcp` in release packaging and provenance checks where appropriate.
7. Document that agent operations visibly mutate the live window.

Exit criteria:

- A user can connect the chosen first MCP host using only packaged binaries and documentation.
- The setup does not require placing the bearer secret in host-visible tool arguments.
- Package qualification exercises both binaries together.

## Test strategy

### Core tests

- Round-trip every new command and result type.
- Reject future versions, invalid IDs, non-finite viewport values, empty intervals, and unbounded
  factors.
- Lock coordinate and clamping semantics.
- Prove redaction and bounded collection behaviour.
- Prove existing version-1 batch fixtures still parse unchanged, or deliberately bump the schema
  with migration/compatibility tests if adding commands requires it.

Adding enum variants does not by itself require a schema bump if old readers are not expected to
accept new commands. The versioning decision must nevertheless be explicit: a client must discover
capabilities before sending an optional command.

### Desktop executor tests

- Workspace description matches known panel and track state.
- Failed tracks remain visible as local failures without preventing healthy state inspection.
- Pan and zoom advance generations and request all required data.
- Zoom anchors and contig-edge clamping are deterministic.
- Selection description agrees with existing inspector/export truth.
- Settlement remains false while required projections or artifact jobs are pending.

### Control-service tests

- Capability status is non-sensitive.
- New commands pass authentication and root confinement.
- Busy, disconnect, timeout, malformed body, wrong content type, and shutdown paths remain bounded.
- State and selection responses do not contain source credentials or forbidden evidence fields.

### MCP adapter tests

- Tool-list snapshots lock names, descriptions, schemas, and annotations.
- Every typed tool constructs the expected `AutomationBatch`.
- Every report status maps to the correct structured MCP result.
- HTTP 200 with a failed automation report is not treated as success.
- Image and export reads enforce regular-file, directory, and byte limits.
- Cancellation and reconnect behaviour are explicit.
- stdout contains valid MCP traffic only.

### Native tests

- Exercise a real rendered window and production query engines.
- Use generated, redistributable fixtures only in the repository.
- Keep platform-specific presentation checks separate from protocol correctness.
- Retain artifact hashes and a path-free structured qualification report.

## Quality gates

Run focused checks during each phase, followed by the full workspace gates at integration points:

```sh
cargo fmt --all -- --check
cargo test -p rgv-core --locked
cargo test -p rgv-desktop --locked
cargo test -p rgv-mcp --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo build --release -p rgv-desktop -p rgv-mcp --locked
```

Also rerun the existing automation qualification and deterministic snapshot comparison after any
change to the automation executor or display capture path.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Agent acts without knowing current state | Make structured workspace inspection the first tool and document observe-before-mutate behaviour. |
| MCP duplicates viewer logic | Translate tools into shared automation commands; keep state extraction in desktop. |
| Relative zoom is ambiguous | Lock factor direction, anchor, coordinate convention, clamping, and exact result in tests. |
| Screenshot is captured before data settles | Default capture to an explicit settled-state command in the same serialized batch. |
| Agent loses track of IDs after mutations | Return stable track/panel IDs and provide a fresh workspace snapshot. |
| Raw batch tool bypasses safety | Apply an MCP allowlist, smaller command limit, shared validation, and reject `Exit`. |
| Secret leaks through host configuration | Pass only the secret-file path; redact configuration and errors. |
| Artifact path escapes the root | Use server-generated paths, canonical/lexical checks as applicable, regular-file checks, and adversarial tests. |
| Long operation outlives MCP cancellation | Report cancellation semantics honestly and re-observe state before subsequent mutation. |
| Multiple calls race | Serialize in the adapter and retain viewer-side serialization. |
| MCP SDK changes | Pin a stable SDK, lock tool-schema tests, and isolate SDK types in the adapter crate. |
| Protocol logs corrupt stdio | Send tracing to stderr and test stdout at the process boundary. |
| State response exposes genomic identifiers | Define allowlisted snapshot fields and test absence of read names, sequences, credentials, and arbitrary tags. |

## Overall acceptance criteria

The initial RGV MCP release is complete when:

- a supported MCP host can launch `rgv-mcp` over stdio;
- `rgv-mcp` can attach securely to an explicitly controlled live RGV window;
- an agent can inspect the workspace before acting;
- exact navigation, relative pan/zoom, track manipulation, configuration, and semantic selection
  visibly update the real window;
- query-triggering tools wait for generation-based settlement by default;
- the agent can retrieve structured selected evidence and a PNG of the settled view;
- exports and saves remain bounded and confined to the control root;
- failures have stable structured codes and do not require log scraping;
- secrets, remote credentials, read names, sequences, and unrestricted paths are absent from MCP
  results and retained reports;
- direct automation and MCP control converge on equivalent viewer state;
- focused, workspace, strict Clippy, release build, automation qualification, and native MCP
  acceptance gates pass; and
- setup and tool behaviour are documented for at least one packaged MCP host.

## Deferred follow-on work

After the initial release is qualified, consider:

- MCP-managed launch through an atomic endpoint file;
- multiple named viewer instances;
- resource subscriptions or state-change notifications;
- cooperative cancellation propagated into accepted viewer work;
- MCP task support for long exports;
- reusable prompts for common visual genomic review workflows;
- a constrained accessibility-action tool for testing UI affordances separately from domain
  commands;
- automatic visual-regression comparison against reviewed captures; and
- remote Streamable HTTP MCP transport, but only with a separate authentication and threat model.

These additions must not weaken the local, opt-in, root-confined default established by the first
release.
