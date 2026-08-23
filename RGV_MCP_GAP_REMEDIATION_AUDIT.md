# RGV MCP gap-remediation audit

## Outcome

The remediation described in `RGV_MCP_GAP_REMEDIATION_PLAN.md` is implemented and qualified at
commit `5aa811efecfccfad3d19174f543855c12168adfc` with the working-tree changes under review.
MF-01 was used as the acceptance fixture; the defects found and fixed were in the automation/MCP
path and qualification setup, not in MF-01's biological truth or rendered display.

The final MCP surface can discover and navigate the MF-01 review queue, manipulate viewports,
apply and observe typed settings, control group/sort order, clear and refresh selections, retain
immutable pileup evidence, find alignments through opaque handles, navigate mates and
supplementary segments, traverse history, export evidence, and save/open the workspace.

## Remediated findings

- Retained selections now have immutable IDs, evidence revisions, exact evidence intervals, and
  byte-stable selected-evidence exports across pan and zoom. Refresh is explicit and creates a new
  identity; clear removes the inspector and structured selection.
- Selection coordinates distinguish the zero-based anchor from the represented half-open evidence
  interval.
- Review queue, history, bounded alignment discovery, opaque record selection, mate navigation,
  and supplementary-chain navigation are available without parsing fixture files.
- MCP configuration is a discriminated typed union. Workspace observations expose the effective
  alignment settings, including pairing, colour, density, base display, group/sort order, lane
  visibility, modification display, and overlapping-mate policy.
- Group, sort, filter, and typed-setting mutations report requested and effective state plus
  changed/clamped/no-op outcomes. An empty filter expression explicitly clears the filter.
- Panel observations expose detail level, render lanes, omission reasons, query/render/sample
  counts, vertical-scroll limits, generations, and settlement. Control-request status repaints no
  longer make an otherwise settled workspace observation report `settled: false`.
- Workspace save/open is root-confined and in-process. Status and all command results carry an
  unpredictable application instance ID; local status also supplies the exact PID used for focus.
- The same MCP adapter process re-observed a replacement viewer instance and rejected a handle
  issued by the previous instance with `stale_handle`.
- Sensitive exact-read-name input, read sequences, arbitrary tags, notes, assignments, source
  paths, credentials, and control secrets are excluded or defensively redacted from MCP results.

## Native MF-01 qualification

Two independent clean launches of the final release binaries passed on
`macOS-15.5-arm64-arm-64bit`, a 2560 x 1600 surface, and scale factor 2.0:

| Run | Application instance | MCP actions | Screenshots | Result |
| --- | --- | ---: | ---: | --- |
| A | `dad9bd04-60c2-45ae-be87-11f7356d0ed8` | 70 | 11 | Pass |
| B | `87decb3d-8fa0-4078-8b89-d877f57731c4` | 70 | 11 | Pass |

Every retained screenshot from both runs was inspected at original resolution. The review showed
the expected sparse, overview, deep-coverage, and read-level states; fixed reference/coverage lanes
while the read stack scrolled; the two-segment supplementary chain; exact retained pileup evidence
with depth 1; and the restored saved workspace. No clipping, stale-frame, corrupt rendering, or
MF-01 display defect was observed.

Both reports contained zero occurrences of the supplied exact read name, bearer material, secret
bytes, control-secret filename, workspace/repository absolute paths, or temporary-root paths.
Selected-evidence artifacts likewise contained no sensitive read-name or authentication material.

## Completion gates

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: pass.
- `cargo test --workspace --locked -q`: pass (one pre-existing ignored test).
- Focused core automation, desktop automation, and MCP protocol tests: pass.
- Packaging tests: 4 pass.
- `cargo build --release --locked -p rgv-desktop -p rgv-mcp`: pass.
- Two clean native MF-01 qualifications with in-process save/open: pass.
- MCP reconnect across a viewer restart with stale-handle rejection: pass.
- Original-resolution review of all 22 final screenshots: pass.
- Tool schema/result/failure/report/artifact privacy audit: pass.

Expected HTSlib diagnostics from deliberately corrupt, truncated, unindexed, and wrong-reference
test inputs appeared during the full suite; the suite itself completed successfully.
