# RGV Forward Implementation Plan

Status date: 22 August 2026

## 1. Purpose

This is the forward execution plan for turning the current RGV working tree into a credible public
beta and, after that, an IGV-class version 1 desktop genome viewer.

It starts from the code that exists now. It does not repeat the full design history in the older
plans. For delivery order and current status, this document supersedes:

- `RGV_NEXT_IMPLEMENTATION_PLAN.md`;
- `RGV_BETA_EXECUTION_PLAN.md`;
- `PRODUCT_PARITY_IMPLEMENTATION_PLAN.md`;
- `REMAINING_IMPLEMENTATION_PLAN.md`; and
- `GUI_REDESIGN_PLAN.md`.

Those documents remain useful as detailed feature catalogues and design records.

## 2. Product outcome

RGV should be a native Rust desktop viewer for short-read, long-read, RNA, variant, annotation, and
quantitative genomic data. It should support the important IGV Desktop review workflows while
being measurably faster and more predictable at deep, complex, and remote loci.

The beta must make these workflows dependable:

1. Review short- and long-read alignments, including bases, indels, clipping, pairs,
   supplementary segments, splice junctions, and modified bases.
2. Compare multiple alignment samples at one or more loci.
3. Review variants and sample genotypes beside the supporting evidence.
4. Navigate by coordinate, feature, mate, supplementary segment, breakend, and saved review item.
5. Save and restore a portable, deterministic workspace without losing analytical state.
6. Export reproducible visual and structured evidence.
7. Remain responsive and bounded under deep coverage, multiple tracks, multiple panels, remote
   sources, and rapid navigation.

Workflow parity is the goal. RGV does not need to imitate every IGV pixel or interaction when a
clearer and faster native design is available.

## 3. Status definitions

- **Done**: implemented, documented, and protected by the declared automated and manual evidence.
- **Candidate**: implemented and automated checks pass, but representative real-data or platform
  acceptance remains.
- **In progress**: useful code exists, but ownership, behavior, persistence, tests, or qualification
  are incomplete.
- **Not started**: no complete user workflow exists.

A feature is not Done because it rendered correctly once. It must cover model, engine, layout, UI,
persistence, failure behavior, tests, documentation, and performance where applicable.

## 4. Current engineering baseline

### 4.1 Verified on the current working tree

On 22 August 2026:

```text
cargo fmt --all -- --check                                      passed
cargo test --workspace --all-targets                            244 passed, 0 failed
cargo clippy --workspace --all-targets --all-features -- -D warnings
                                                                  passed
cargo build --release -p rgv-desktop                            passed
cargo run --release -p rgv-validation-runner -- validation/synthetic-corpus.json
                                                                  passed
```

The automated engineering gate is green for the multi-panel/session-v3 slice. Release-build manual
interaction acceptance and native multi-panel RSS evidence remain open.

### 4.2 Strong existing capabilities

- Native `winit`, `wgpu`, and `egui` desktop application with a white IGV-like track workspace.
- Indexed local BAM/CRAM, observable remote BAM, and local FASTA reference access.
- Deterministic deep-locus sampling, bounded queues and caches, stale-generation rejection,
  retained-frame navigation, and stable read packing.
- Allele-aware coverage, readable reference and read bases, semantic CIGAR events, indels,
  clipping, splice junctions, and bounded inspectors.
- Paired-read classification and layout, supplementary-chain inspection, modified-base decoding,
  and sampling-independent aggregate evidence.
- Multiple live alignment tracks with stable identity, independent engines and settings, shared
  workspace budgets, failure isolation, reordering, selection, and session representation.
- Basic BED, GFF3, GTF, VCF/BCF, BigWig, and BigBed support.
- PNG export, command search, diagnostics, JSON sessions, and partial IGV session import.
- Smooth horizontal and vertical overview navigation using the Ableton-inspired pan/zoom model.

### 4.3 Current multi-locus candidate

The working tree contains the first actual visible multi-locus implementation:

- an ordered `PanelWorkspace` with stable `PanelId` values and an active panel;
- panel-owned viewport, history, selection, query generation, rows, display list, auxiliary
  snapshots, layout state, and vertical navigation;
- per-panel alignment projections and panel-aware auxiliary-track results;
- side-by-side panel columns, per-panel genome context, active-panel activation, add/close/reorder,
  and whitespace-separated multi-locus navigation;
- composite rendering and panel-aware diagnostics; and
- session schema version 3 with ordered panels and active-panel persistence plus v1/v2 migration.

Initial and in-app sessions now restore every saved panel and track projection. Corresponding
downstream tracks share vertical geometry, inactive results wake composite rendering, GPU selection
identity cannot leak across panels, and mapped mates or supplementary segments can open in a new
panel while retaining read identity. The direct 2-track × 2-panel route, reviewed raster, v1/v2/v3
migrations, 4-track × 2-panel fixture, complete automated gate, and native RSS scaling evidence are
green. This slice is now a **Candidate engineering baseline**; release-desktop manual interaction
acceptance with short-read and ONT data remains before Phase 0 can close.

## 5. Architectural invariants

These rules apply to every remaining phase:

1. `PanelId`, `TrackId`, source identity, and query generation are stable identities, never vector
   indices.
2. Reordering or resizing a panel or track cannot change its semantic identity, selection, cached
   rows, or query destination.
3. Every track projects independently into every visible panel while sharing immutable source data
   and reusable decoded ranges where safe.
4. A slow, failed, hidden, or closed track cannot blank or block another track or panel.
5. Aggregate evidence is calculated from all eligible records, not retained display rows.
6. Record, memory, query, GPU primitive, label, preview, and export budgets are workspace-wide and
   deterministic.
7. Obsolete work can never replace a newer generation, including after panel or track deletion.
8. Display-only changes relayout retained normalized data instead of decoding again.
9. Reference, coverage, junction, modification, reads, annotations, and overlays remain separate
   semantic layers.
10. Missing or incomplete evidence is reported explicitly and is never silently inferred.
11. Sessions, diagnostics, logs, URLs, and exports never leak credentials or unintended sequence.
12. Unknown future session schemas are rejected without overwriting the source file.

## 6. Critical path

```text
finish and qualify multi-locus/session v3
  -> link mates, supplementary segments, and breakends to panels
  -> complete VCF and genotype review
  -> complete RNA and quantitative comparison
  -> genome/search/ROI/review workflows
  -> portable sessions, automation, export, and accessibility
  -> backend, platform, performance, and packaging qualification
  -> public beta
```

Representative real-data alignment acceptance runs throughout this path. Any correctness, stale
frame, crash, data-loss, or unbounded-work defect interrupts feature development.

## 7. Phase 0 — finish the multi-locus workspace

### User outcome

A user can open several coordinates side by side, navigate each independently, compare every
loaded track across them, save the arrangement, reopen it, and obtain the same settled view.

### 0.1 Complete initial session restoration

- Construct every saved panel during initial `--session` launch, not only when applying a session
  after the application has started.
- Restore panel order, stable IDs, requested loci, active panel, history-compatible viewport state,
  and the active locus editor value.
- Create a projection for every alignment-track/panel pair before results can arrive.
- Resolve every restored locus independently against each alignment dictionary and report a
  missing contig only in the affected projection.
- Preserve the compatibility `locus` field as the active panel locus during v1/v2 migration and
  schema-v3 save.

### 0.2 Make panel interaction deterministic

- Clicking or scrolling within a panel activates it before navigation, selection, or inspection.
- Restrict picking and hover inspection to the correct panel rectangle.
- Prevent renderer object IDs from highlighting an unrelated object with the same local ID in a
  different panel; use panel-aware IDs or an equivalent semantic selection path.
- Closing the active panel must update the active panel, locus editor, selection surface, and
  diagnostics in one transaction.
- Reordering panels must preserve IDs, rows, retained results, scroll position, and selection.
- Resizing the window or changing panel count must recalculate every panel width and request every
  affected projection, not only the active one.
- Settings that change query semantics must invalidate and refresh all visible panels for the
  target track. Display-only settings must relayout all retained projections without I/O.
- Keep horizontal panel navigation independent of vertical read-stack position.

### 0.3 Define and enforce geometry

- Give each panel a deterministic column rectangle with explicit minimum width.
- Keep the genome context, ruler, pinned reference/coverage lanes, read stack, and active outline
  aligned to that rectangle.
- Use a common vertical track geometry across panels so corresponding sample tracks remain
  visually comparable even when retained row counts differ.
- Keep the track-label gutter workspace-wide and show which panel owns the active scrollbar and
  inspector.
- Clip all primitives and text to their panel; no panel may draw into its neighbour or the gutter.
- Define behavior when the window is too narrow: enforce the panel limit, introduce bounded
  horizontal panel navigation, or refuse an additional panel with an explicit message.

### 0.4 Complete session schema version 3

- Add focused v3 round-trip tests for panel order, IDs, loci, active panel, and multiple tracks.
- Test v1-to-v3 and v2-to-v3 migration explicitly.
- Reject zero panels, duplicate panel IDs, an absent active ID, excessive panels, invalid loci, and
  a compatibility locus that disagrees with the active panel.
- Reject unknown future versions before mutating viewer state.
- Retain credential redaction for every alignment, reference, and auxiliary source.
- Update session fixtures, `docs/SESSION_COMPATIBILITY.md`, the user guide, and the changelog.

### 0.5 Add linked-evidence navigation

- Add “open mate in new panel” for paired reads.
- Add previous/next and open-in-new-panel actions for supplementary/`SA` segments.
- Preserve source track, selected template or chain, and origin panel in the new panel.
- Make absent, malformed, unloaded, or truncated linked evidence explicit.
- Establish one reusable panel-navigation command that later supports VCF breakends,
  BEDPE/interact endpoints, and review-list entries.

### 0.6 Tests

- Unit-test add, activate, reorder, close, and stable-ID allocation in `PanelWorkspace`.
- Unit-test multi-locus parsing, maximum panel count, and invalid partial input.
- Test independent generation routing for at least two panels and two tracks.
- Test late result delivery after panel close and after ID-preserving reorder.
- Test panel-aware layout-cache and picking identity.
- Test query-affecting versus display-only setting propagation.
- Test session startup restoration separately from in-app session application.
- Add a deterministic two-panel render fixture and reviewed screenshot/hash.
- Add an integration fixture with two panels, four alignment tracks, and one failed projection.
- Add rapid independent navigation and resize tests with pending work.

### Phase 0 gate

- The full formatting, test, strict Clippy, release build, and synthetic validation gate is green.
- A v1, v2, and v3 session all open into the documented equivalent workspace.
- A two-panel/four-track session restores in the same order and state.
- Rapid navigation never swaps, blanks, or cross-highlights panels.
- Mate and supplementary navigation open the correct locus without replacing the origin panel.
- Release-build manual tests pass with representative short-read and long-read BAMs.
- Native RSS, record counts, primitives, labels, and query work remain inside declared workspace
  budgets.

## 8. Phase 1 — qualify alignment review on real data

### User outcome

The existing alignment features are trusted on representative Illumina, ONT, PacBio, RNA, deep
coverage, and malformed-input cases.

### Work

- Pin safe manifests containing checksums, technology, expected loci, truth notes, and permitted
  use for representative datasets.
- Verify reference bases, read bases, mismatch modes, base-quality shading, all CIGAR operations,
  and both strands.
- Verify small and large insertions/deletions, clips, skips, padding, and bounded previews.
- Verify paired-read classifications, overlapping-mate policy, template-length inference, and all
  structural-pair patterns.
- Verify ONT/PacBio modified-base codes, probability behavior, missing `ML`, malformed tags,
  haplotypes, and aggregate counts.
- Verify primary/supplementary chain behavior, loaded-segment highlighting, and linked panels.
- Verify RNA junction endpoints, depth, strand provenance, filters, and sampling independence.
- Exercise missing/corrupt indexes, truncated data, wrong or partial reference, reopening, and close
  during active work.
- Run a one-hour navigation soak while observing RSS, stale frames, retained cache behavior, and
  GPU/device errors.

### Phase 1 gate

- Every scenario has a recorded pass/fail result and reproducible locus.
- No unexplained genomic correctness discrepancy remains.
- Every fixed defect has a minimal regression fixture or test.
- Candidate alignment features are promoted to Done only after their declared technology matrix
  passes.

## 9. Phase 2 — complete VCF and genotype review

### User outcome

A reviewer can inspect a variant, understand its annotations and sample genotypes, filter it, and
compare it directly with supporting reads across one or more panels.

### Work

- Preserve typed REF/ALT alleles, IDs, QUAL, FILTER, INFO, FORMAT, genotype, phasing, and symbolic
  allele semantics.
- Support SNVs, MNVs, small and large indels, multi-allelic records, symbolic SVs, and breakends.
- Add a structured, copyable variant inspector with header-derived field descriptions.
- Add virtualized sample/genotype rows; do not render thousands of samples eagerly.
- Add sample search, sample subset selection, genotype grouping, and genotype-aware colouring.
- Add persisted filters for quality, FILTER state, type, interval, allele frequency, genotype, and
  selected INFO/FORMAT fields with explicit missing-value behavior.
- Add breakend and symbolic-end navigation through the common panel command.
- Link selected variants to pileup, read, pair, and split-read evidence without inventing a variant
  caller.
- Keep parsing, filtering, sample materialization, glyphs, labels, and inspector output bounded.

### Phase 2 gate

- A generated truth set covers every supported variant form, phased/unphased and missing genotypes,
  malformed fields, multi-sample scale, and interchromosomal BNDs.
- Filter results match declared truth exactly.
- Breakend navigation opens the correct partner panel and retains the original variant selection.
- A large cohort file remains responsive and does not materialize every genotype into GUI widgets.

## 10. Phase 3 — RNA, quantitative, and interaction workflows

### 3.1 RNA and Sashimi

- Add collapsed and expanded junction modes.
- Add single- and multi-sample Sashimi rendering using selected alignment and annotation tracks.
- Preserve exact junction endpoints, strand provenance, filters, counts, and scale semantics.
- Add shared/fixed scaling and explicit normalization provenance.
- Export bounded junction BED plus deterministic Sashimi PNG/SVG.

Gate: generated RNA truth produces exact, sampling-independent endpoints and counts in single- and
multi-sample views.

### 3.2 Quantitative tracks

- Complete BigWig controls: automatic/fixed range, baseline, linear/log scale, positive/negative
  colours, missing values, and raw/minimum/mean/maximum windowing.
- Add bar, line, point, and heatmap styles where source semantics support them.
- Add grouped autoscaling, overlay/separate modes, transparency, and bounded arithmetic transforms.
- Apply common scale settings consistently across compatible tracks and panels.

### 3.3 Additional formats

Implement after the common scale and panel abstractions are reusable:

1. bedGraph and WIG;
2. BEDPE/interact with endpoint-panel navigation;
3. SEG and copy-number matrices;
4. MAF mutation tracks;
5. genePred/refFlat and narrowPeak;
6. PSL only if demonstrated workflows require it.

Gate: zoomed-out aggregation is deterministic, scale provenance is visible, remote work is bounded,
and linked endpoints use the common panel mechanism.

## 11. Phase 4 — genome, feature search, ROIs, and review queues

### Work

- Load an indexed FASTA and genome metadata without requiring an alignment file.
- Support canonical contig aliases such as `1` and `chr1` without silently resolving ambiguity.
- Load cytobands and default annotations from explicit genome configuration.
- Add feature-name and transcript-ID indexing with autocomplete and deterministic disambiguation.
- Add next/previous feature navigation and a rich annotation inspector.
- Keep the reference as an independent pinned track with sequence copy, strand flip, and optional
  three-frame translation.
- Create, edit, name, import, export, and remove regions of interest.
- Draw ROI boundaries across every visible track and panel.
- Add gene/locus lists, next/previous review actions, notes/status, and local review state.
- Avoid persisting private read names or sequence by default.

### Phase 4 gate

A custom genome, aliases, annotation, ROIs, and review list restore on a clean installation. Feature
navigation is deterministic, ambiguous names are presented for selection, and review state does not
leak unintended genomic evidence.

## 12. Phase 5 — sessions, automation, export, and accessibility

### 5.1 Portable projects and sessions

- Preserve every analysis-changing track, panel, genome, ROI, review, and visualization setting.
- Add explicit migration tests for every released schema boundary.
- Separate global defaults from workspace and per-track overrides.
- Support relative paths rooted at an explicit project directory.
- Improve common IGV XML imports while reporting every ignored semantic.
- Validate before save and never overwrite an unsupported future schema automatically.

### 5.2 Deterministic automation

- Implement a versioned batch format or local command API.
- Support: new workspace, load genome, add/remove track, goto, add/close panel, set option, set
  filter, select evidence, wait until settled, snapshot, export, save session, and exit.
- Define “settled” from generations, engine queues, uploads, and layout state rather than a delay.
- Return structured errors and keep credentials out of commands, logs, and result metadata.

### 5.3 Export

- Keep deterministic PNG export and add faithful SVG for supported layers.
- Add bounded or streamed exports for pileup, selected evidence, modification frequency, junction
  BED, variants, and ROIs.
- Record source-safe provenance: build, locus, track settings, filters, and truncation state without
  embedding credentials or private sequence unintentionally.

### 5.4 Accessibility

- Complete keyboard navigation for tracks, panels, overview bars, inspectors, menus, dialogs, and
  review queues.
- Add accessible names and state announcements for genomic objects and asynchronous errors.
- Validate focus order, text scaling, high contrast, colour independence, reduced motion, and
  screen-reader behavior supported by the desktop stack.

### Phase 5 gate

Automation reconstructs a portable multi-track, multi-panel review and exports matching evidence on
a clean installation. All core review actions work without a mouse.

## 13. Phase 6 — remote backends, performance, and public beta

### 6.1 Backend qualification

- Qualify local BAM/CRAM and remote BAM/CRAM with BAI, CSI, CRAI, redirects, interrupted requests,
  custom indexes, authentication redaction, reopen recovery, and reference lookup.
- Make remote CRAM range and reference behavior observable enough to enforce byte/request budgets.
- Verify local and remote semantic equivalence for every mutually supported format.
- Test latency, loss, changed ETags, partial objects, and bounded disk-cache behavior.

### 6.2 Matched IGV comparison

- Pin exact RGV and IGV builds, hardware, OS, datasets, loci, viewport size, settings, warm/cold
  state, and automation procedure.
- Measure startup, first useful frame, cached pan/zoom, deep-locus interaction, memory, remote bytes,
  remote requests, and export time.
- Retain raw samples, definitions, commands, checksums, and native RSS evidence.
- Publish only specific reproducible claims; “faster than IGV” is not a valid unqualified result.

### 6.3 Platform and release qualification

- Test declared macOS, Windows, and Linux versions; integrated and discrete GPUs; Retina and
  fractional scaling; mouse, trackpad, and keyboard input.
- Test device loss, display changes, sleep/wake, clean shutdown under load, and one-hour soak.
- Produce signed/notarized packages, checksums, licences, SBOM/provenance, and documented cache/data
  locations.
- Test clean install, upgrade, session migration, uninstall, and failure recovery on each platform.

### Public beta gate

- Phases 0–2 are complete.
- Phase 3.1 is complete if RNA support is advertised; common quantitative controls are complete.
- Standalone genome loading, feature search, ROIs, portable sessions, automation, and accessibility
  smoke tests pass.
- No open correctness, crash, stale-frame, data-loss, credential-leak, or unbounded-work defect
  remains.
- Exact beta artifacts pass the declared backend, platform, real-data, performance, and packaging
  matrices.

## 14. Performance and resource budgets

Budgets apply to optimized builds on named hardware. Multi-track and multi-panel tests add scaling
workloads; they do not replace single-view regression tests.

| Metric | Beta target |
| --- | --- |
| Cached pan/zoom median | no more than 16 ms |
| Cached pan/zoom p95 | no more than 33 ms |
| Local first useful frame at an ordinary locus | no more than 100 ms |
| Controlled warm remote first useful frame | no more than 500 ms |
| UI-thread file or network I/O | zero operations |
| Obsolete result replacing an active view | zero occurrences |
| Unexplained empty frame during retained navigation | zero occurrences |
| Pending-work interaction frame | 16.7 ms target; 33 ms p95 ceiling |
| Memory, decoded records, primitives, and labels | explicit workspace-wide caps |
| Inspector previews and clipboard output | explicit deterministic caps |
| Large structured exports | bounded or streamed |

Every benchmark record includes the build identifier, safe source manifest/checksum, loci, panel and
track count, viewport, settings, hardware, OS, warm/cold state, retained-record count, primitive
count, cache state, and native RSS.

## 15. Required gate for every implementation slice

Use the applicable focused tests first, then close a slice with:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p rgv-desktop
cargo run --release -p rgv-validation-runner -- validation/synthetic-corpus.json
```

Performance-sensitive slices also require the relevant optimized benchmark and native RSS evidence.
Visible behavior requires a deterministic render test where practical and a release-build manual
check on representative data. Update the user guide, compatibility notes, changelog, fixtures, and
this plan in the same slice.

## 16. Immediate implementation queue

Work in this order and keep each item independently reviewable:

1. **Done:** connect the selection-clearing path and remove the dead-code warning.
2. **Done:** make initial session launch restore all schema-v3 panels and track/panel projections.
3. **Done:** add core v3 round-trip, v1/v2 migration, invalid-panel, and fixture tests.
4. **Done:** add `PanelWorkspace` stable-ID mutation and bounded multi-locus parser tests.
5. **Done:** fix active-panel refresh after close, all-panel resize/query propagation, cursor
   containment, and per-panel gesture throttling.
6. **Done:** prevent inactive panels from reusing the active panel's GPU hover/selection identity.
7. **Done:** align downstream tracks across columns using the tallest natural primary-alignment
   frame while preserving every panel's full scrollable read stack; protect it with a geometry test.
8. **Done:** add a direct two-track/two-panel routing gate, a reviewed deterministic multi-panel
   raster hash, and the schema-v3 two-panel/four-track fixture.
9. **Done:** add mate and supplementary-segment open-in-panel actions with deferred read identity
   selection in the destination query.
10. **Done:** update schema/session/user documentation, changelog, and validation fixtures.
11. **Done:** run the complete automated gate with 244 passing tests and no Clippy warnings.
12. Manually qualify a two-panel/four-track release build with short-read and ONT BAMs.
13. **Done:** record native 1-track × 2-panel and 4-track × 2-panel latency, primitives, retained
   records, cache behavior, and RSS. The 4 × 2 cached median/p95 are 4.14/6.83 ms with 80/80 cache
   hits and 50.23 MiB peak sampled RSS; its 563.96 ms first complete workspace frame remains an
   explicit cold-start optimization item.

Do not begin broad VCF, Sashimi, or new-format work until item 12 and the Phase 0 gate are complete.

## 17. Progress tracker

| Workstream | Current status | Next promotion condition |
| --- | --- | --- |
| Core alignment interpretation | Candidate | Representative technology acceptance matrix |
| Paired, split, and modified reads | Candidate | Real-data acceptance of panel-linked navigation |
| Multiple alignment tracks | Candidate engineering baseline | Release four-track manual gate |
| Multi-locus panels | Candidate engineering baseline | Release short-read/ONT manual interaction gate |
| VCF/genotypes | Partial | Phase 2 truth, scale, filtering, and BND gate |
| RNA/Sashimi | Partial | Exact multi-sample junction workflow and export |
| Quantitative/interaction tracks | Partial | Common scales, aggregation, overlays, endpoint navigation |
| Genome/search/ROIs/review | Partial | Clean-install portable workflow gate |
| Sessions/automation/export | Partial | Deterministic reconstructed review and evidence |
| Accessibility | Partial | Keyboard and declared platform accessibility matrix |
| Remote CRAM | Partial | Observable bounded WAN/reference qualification |
| Packaging/public beta | Partial | Exact artifacts pass the complete beta matrix |

At the end of every slice:

1. update this table and the first incomplete item in Section 16;
2. record exact commands and results;
3. link fixtures, render evidence, benchmarks, and manual acceptance notes;
4. distinguish automated evidence from real-data and platform acceptance; and
5. leave no undocumented analysis-changing behavior.

## 18. Deliberate deferrals

These are not beta blockers unless user demand changes the scope:

- BLAT or external search-service integration;
- Hi-C matrices and circular genome views;
- specialist GISTIC and broad GWAS formats;
- a public plugin API;
- cloud collaboration or hosted workspaces;
- clinical interpretation or decision support; and
- a hosted genome catalogue beyond what is required for local genome usability.

The immediate next action is Section 16, item 1, followed by all-panel restoration during initial
session launch and the schema-v3 tests in items 2 and 3.
