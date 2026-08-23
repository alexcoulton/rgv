# ADR 0006: MCP stdio adapter over local control

Status: accepted for the initial MCP release.

## Context

RGV needs to expose its real visible window to an agent for feature and regression testing. The
existing opt-in local control service already owns authentication, path confinement, serialized UI
dispatch, query settlement, capture, and structured reports. Embedding a second command executor
or an MCP protocol runtime in the GPU/UI process would duplicate those boundaries and couple
model-facing input directly to rendering state.

## Decision

- Ship `rgv-mcp` as a separate stdio process using `rmcp` 3.1.4.
- Translate focused typed MCP tools into the shared versioned `rgv-core` automation commands.
- Attach only to an explicitly enabled HTTP control service at an IP-literal loopback address and
  explicit port.
- Read the bearer value from the same protected file as the desktop. Never accept or publish the
  value through MCP configuration, arguments, results, or logs.
- Keep stdout exclusive to MCP traffic and send tracing to stderr.
- Serialize requests locally, while retaining desktop-side serialization and generation-based
  settlement.
- Keep workspace and selection extraction in the desktop executor, where it can reuse production
  inspector/export state. The adapter performs only schema translation, safe error mapping, and
  bounded artifact handling.
- Generate temporary capture names in a dedicated root-confined directory. Durable exports and
  projects remain user-named root-relative files and are never automatically removed.
- Expose no MCP transport other than stdio and no lifecycle tool that exits or launches the viewer
  in the initial release.
- Scope opaque alignment handles and immutable evidence-selection IDs to an unpredictable desktop
  process instance. Query changes invalidate handles; viewport changes never rewrite retained
  evidence.
- Publish valid-by-construction setting variants rather than generic key/scope/value combinations.
- Correlate observations and captures with the desktop PID, application instance, workspace
  revision, evidence revision, panel intervals, and render generations.

## Consequences

MCP failures cannot directly corrupt renderer state, the adapter can be tested against a fake
control service, and non-MCP clients retain the same automation boundary. A host may start while
RGV is unavailable and reconnect through later calls. Operations that have reached the desktop
cannot be transactionally rolled back when an MCP caller cancels, so subsequent mutation should
begin with fresh workspace observation.

The user must start and own the viewer explicitly. Root-confined project replacement is supported,
but managed viewer launch, endpoint discovery, multiple windows in one process, subscriptions,
cooperative cancellation, tasks, and remote MCP transport need separate lifecycle and threat-model
decisions.
