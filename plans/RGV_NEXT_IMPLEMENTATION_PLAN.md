# RGV Next Implementation Plan

Status date: 22 August 2026

## 1. Purpose and authority

This document is the new execution roadmap for taking RGV from its current multi-track engineering
candidate to a credible public beta and then an IGV-class version 1. It consolidates the remaining
work described in:

- [RGV_BETA_EXECUTION_PLAN.md](RGV_BETA_EXECUTION_PLAN.md);
- [PRODUCT_PARITY_IMPLEMENTATION_PLAN.md](PRODUCT_PARITY_IMPLEMENTATION_PLAN.md);
- [REMAINING_IMPLEMENTATION_PLAN.md](REMAINING_IMPLEMENTATION_PLAN.md);
- [GUI_REDESIGN_PLAN.md](GUI_REDESIGN_PLAN.md); and
- [IMPLEMENTATION_PLAN.md](IMPLEMENTATION_PLAN.md).

For implementation order and current status, this document takes precedence. The older documents
remain useful as detailed feature catalogues, design history, and qualification procedures.

The plan deliberately separates three states:

- **Done**: implemented and protected by automated evidence.
- **Candidate**: implemented, but still awaiting declared real-data or platform acceptance.
- **In progress**: code exists in the working tree, but the slice is incomplete or the gate is red.

Nothing becomes Done merely because it renders once on one BAM.

## 2. Product target

RGV is a native Rust desktop genome viewer for short reads, long reads, RNA alignments, variants,
and indexed genomic annotation and signal tracks. It should cover the important IGV Desktop review
workflows while being materially faster and more predictable at deep and remote loci.

The product priorities are:

1. Correct and explainable evidence.
2. Smooth, stable navigation without layout jumps or stale frames.
3. First-class short-read, long-read, modified-base, paired-read, and split-read workflows.
4. Multiple samples and synchronized loci.
5. Bounded CPU, memory, GPU primitives, labels, previews, and remote requests.
6. Portable, deterministic sessions and exports.
7. Measured performance claims based on matched RGV-versus-IGV evidence.

Workflow parity is the target; pixel-for-pixel imitation is not. The current white, IGV-like track
workspace and the Ableton-style navigation mechanism are valid RGV-specific choices.

## 3. Current status snapshot

### 3.1 Verified candidate baseline

The last fully verified candidate included:

- native `winit`/`wgpu`/`egui` desktop rendering;
- local indexed BAM and CRAM, observable remote BAM, and local FASTA reference access;
- deterministic deep-locus sampling, generation cancellation, retained-frame navigation, and
  stable read packing;
- pinned reference, allele-aware coverage, junction, modified-base frequency, and scrollable read
  lanes;
- semantic CIGAR events, readable reference/read bases, indels, clipping, and bounded inspection;
- paired-read classification, pair layout, overlap handling, group/sort/colour controls, and
  generated structural-pair fixtures;
- bounded modified-base decoding and aggregate evidence;
- bounded supplementary/`SA` chain inspection and navigation;
- sampling-independent splice-junction aggregation and inspection;
- basic BED, GFF3, GTF, VCF/BCF, BigWig, and BigBed tracks; and
- version-1 sessions, partial IGV session import, PNG export, command search, and diagnostics.

The latest green gate contains 231 workspace tests, strict Clippy, an optimized desktop
build, and passing synthetic semantic validation. Candidate status still requires representative
Illumina, ONT, PacBio, RNA, deep-coverage, corruption, and cross-platform manual acceptance.

### 3.2 Current working-tree state

The first part of the multiple-alignment-track refactor is implemented in `rgv-core` and the
desktop, while temporary primary-track compatibility fields remain during the UI migration:

- session schema version 2 introduces an ordered `SessionAlignmentTrack` list;
- version-1-to-version-2 migration and multi-track round-trip tests pass;
- alignment settings have moved from document-global state to per-track session state; and
- the desktop instantiates every entry as a live ordered controller with an independent engine,
  generation, lifecycle, filters, retained query, rows, statistics, and bounded rendering.

This slice remains **In progress**, not Candidate. At the time of this update:

- all 231 workspace tests, strict all-feature Clippy, the optimized desktop build, and synthetic
  semantic validation pass;
- secondary tracks support add, query, render, rename, visibility, collapse, height, reorder, remove,
  and session restore; and
- read, CIGAR-event, pileup, junction, modification, and supplementary-chain inspection is routed
  by stable track ID across primary and secondary alignment tracks.

The immediate priority is release-desktop manual acceptance of the automated four-track gate, then
expanding the now-green single-panel `PanelId` foundation into multiple visible loci.

### 3.3 Remaining product gaps

| Area | Status | Main gap |
| --- | --- | --- |
| Alignment interpretation | Candidate | Real-data visual and semantic acceptance matrix |
| Paired/chimeric/modified reads | Candidate | Real ONT/PacBio/paired-data acceptance and panel navigation |
| Multiple alignment tracks | In progress | Release-desktop manual four-track acceptance |
| Multi-locus panels | In progress | Stable panel/query/projection identity and single-panel ownership are green; ordered visible panels, persistence, and linked-evidence navigation remain |
| VCF and genotypes | Partial | Structured fields, virtualized samples, filters, BND navigation |
| RNA | Partial | Sashimi, modes, export, multi-sample comparison |
| Quantitative/interaction tracks | Partial | Scales, styles, aggregation, overlays, BEDPE/interact/CNV |
| Genome/navigation/review | Partial | Standalone genome, aliases, feature search, ROIs, review queues |
| Sessions/automation/export | Partial | Multi-track/panel portability, migrations, command API, SVG/data export |
| Accessibility | Partial | Complete keyboard, names, scaling, contrast, reduced-motion validation |
| Remote CRAM | Partial | Observable bounded request/reference behavior and WAN qualification |
| Packaging/release | Partial | Platform matrix, signing/notarization, clean-machine and provenance gates |

## 4. Architectural destination

The single-viewer structure should evolve toward the following ownership model:

```text
Workspace
├── GenomeContext
│   ├── reference dictionary and aliases
│   ├── reference sequence provider
│   └── shared immutable metadata
├── ordered ViewPanels
│   ├── stable PanelId
│   ├── viewport, history, selection, and query generation
│   └── per-panel settled/loading/error state
├── ordered Tracks
│   ├── stable TrackId and source identity
│   ├── per-track settings, filters, visibility, height, and selection
│   ├── per-panel query/layout state
│   └── independent loading/ready/error lifecycle
└── WorkspaceResources
    ├── shared range and decoded-data caches
    ├── global record/memory/primitive/label budgets
    ├── scheduler and cancellation
    └── diagnostics, session, automation, and export services
```

The following invariants apply throughout the refactor:

- `TrackId`, `PanelId`, source identity, and query generation are never inferred from vector index.
- Reordering a track or panel cannot change semantic identity, selection, cache identity, or rows.
- A failed or slow track cannot blank, reorder, or block another track.
- Aggregate evidence is computed from all eligible records, not retained display rows.
- Workspace budgets are global; opening four tracks must not silently multiply every cap by four.
- Display-only settings do not trigger decoding when an existing normalized result can be relaid out.
- Sessions contain no credentials, signed-query secrets, private sequence, or unintended read data.
- Unknown future session versions are rejected without overwriting them.

## 5. Dependency order

```text
restore green baseline
  -> session v2 and multiple alignment tracks
  -> multi-locus panel model
  -> complete VCF/genotype review
  -> multi-sample RNA and quantitative workflows
  -> genome/search/ROI workflows
  -> sessions, automation, export, accessibility
  -> remote/platform/performance qualification
  -> public beta
```

Real-data acceptance of the existing alignment features runs continuously in parallel with this
critical path. Correctness defects found during that acceptance interrupt feature work.

## 6. Phase 0 — recover and lock the baseline

### Outcome

The in-progress session refactor compiles, its intended migration behavior is explicit, and all
previously green gates pass again.

### Work

1. Resolve stable-ID set semantics deliberately: either derive value ordering for `TrackId` or use
   a hash set without changing serialized identity.
2. Finish and review session-v2 validation, redaction, version probing, and version-1 migration.
3. Decide whether an incomplete desktop migration should temporarily reject multi-alignment
   sessions or preserve every uninstantiated track. It must never silently discard tracks on save.
4. Update all desktop session reads/writes to the version-2 API.
5. Update `docs/SESSION_COMPATIBILITY.md`, the user guide, changelog, and fixtures.
6. Run formatting, focused tests, the complete workspace suite, strict Clippy, release build, and
   synthetic-corpus validation.

### Gate

- Session v1 loads as the documented v2 equivalent.
- V2 round-trips track order, IDs, sources, filters, visibility, collapse state, height, and caps.
- Duplicate IDs, invalid limits, non-finite settings, and future versions fail safely.
- JSON output redacts credentials in every track source.
- The complete repository gate is green before Phase 1 expands runtime ownership.

## 7. Phase 1 — multiple alignment tracks

### User outcome

A user can compare tumor/normal, trio, replicate, short-read/long-read, or multi-sample RNA BAMs in
one workspace. Every track navigates the shared locus but retains independent interpretation state.

### 1.1 Core and engine model

- Replace the desktop's single alignment controller with an ordered collection keyed by `TrackId`.
- Introduce an explicit per-track controller containing source, filters, visual settings, query
  generation, retained frame, rows, statistics, selection, and lifecycle state.
- Keep source services and immutable reference resources shareable without sharing mutable view
  state accidentally.
- Route engine results by both track ID and generation; discard only the obsolete destination.
- Make close cancellation deterministic and ensure late results cannot resurrect a closed track.
- Introduce global decoded-record, resident-memory, GPU-primitive, label, and preview budgets.
- Allocate budgets fairly to visible tracks and report truncation per track and for the workspace.

### 1.2 Desktop behavior

- Open additional BAM/CRAM files without replacing the existing alignment track.
- Close, reorder, rename, hide, collapse, resize, and select tracks.
- Preserve independent filters, colour/group/sort, density, pair mode, subtrack visibility, scales,
  modification controls, junction controls, and inspector state.
- Add single- and multi-select plus compatible bulk settings.
- Offer shared or independent coverage scale with visible provenance.
- Keep one track's loading/error surface local to that track.
- Make keyboard track selection, reordering, toggling, and menu access complete.

### 1.3 Verification

- Unit-test ordered controller mutation, close-with-in-flight-query, independent generation routing,
  and global budget allocation.
- Add a four-track integration fixture containing different filters, heights, modes, and one failed
  source.
- Round-trip the fixture through session v2 and verify byte-stable semantic state.
- Exercise four real alignment tracks for rapid navigation, reordering, hiding, and close/reopen.
- Benchmark one, two, and four tracks at the same deep locus and record latency, records, primitives,
  cache behavior, and native RSS.

### Gate

Four simultaneous alignment tracks restore in the same order and state, remain responsive under
rapid navigation, stay within workspace budgets, and tolerate one failed or closed track without
disturbing the other three.

## 8. Phase 2 — multi-locus panels and linked evidence

### User outcome

A user can inspect a read and its mate, a supplementary segment, a VCF breakend, or an interaction
endpoint beside the current locus without losing context.

### 2.1 Panel model

- Add stable `PanelId`, ordered panels, active panel, per-panel viewport/history/selection, and
  independent query generation.
- Make every track project into every visible panel while retaining one source/settings identity.
- Key cache, layout, picking, diagnostics, export, and automation operations by track and panel.
- Share decoded overlapping ranges without coupling panel navigation.
- Define panel-local versus workspace-global selection explicitly.

### 2.2 UI and navigation

- Accept multiple coordinates and supported feature names in the locus field.
- Add, close, reorder, resize, and activate panels.
- Provide new-panel actions for mate, supplementary segment, splice context, VCF breakend, and
  interaction endpoint.
- Highlight linked evidence across panels while preserving the original selection.
- Make panel navigation history and close behavior predictable.
- Persist panel order, sizes, loci, active panel, and linked-selection state.

### 2.3 Verification and gate

- Test source/track/panel identity across reorder and session restore.
- Test overlapping and non-overlapping panel queries, late results, and shared-cache accounting.
- Test intra- and inter-chromosomal pair, `SA`, and BND fixtures.
- Benchmark four tracks across four panels, including one deep and one remote locus.

Gate: one action opens linked evidence in a new panel, the original view never moves unexpectedly,
and a four-track/four-panel workspace restores deterministically within its global budgets.

## 9. Phase 3 — complete variant and genotype review

### User outcome

A user can inspect a call, understand its alleles and filters, compare sample genotypes with the
matching alignment tracks, and navigate structural breakends.

### Work

- Preserve original and normalized alleles, `QUAL`, `FILTER`, structured `INFO`, symbolic alleles,
  breakends, and relevant header metadata.
- Preserve sample names, ploidy, phasing, and lazy access to `GT`, `DP`, `GQ`, `AD`, `PL`, `PS`, and
  caller-specific `FORMAT` fields.
- Virtualize cohort sample rows rather than expanding all FORMAT values eagerly.
- Add collapsed, squished, and expanded variant modes plus copyable semantic inspection.
- Render reference, heterozygous, alternate, no-call, phased, polyploid, and filtered states clearly.
- Add sample/genotype filter, sort, grouping, and imported sample attributes.
- Map VCF samples to alignment tracks explicitly; do not infer correspondence from filenames.
- Open BND/SV partner loci through the common panel API.

### Gate

A generated multi-allelic, phased, polyploid, symbolic, and breakend fixture produces exact expected
states; thousands of samples remain bounded and scroll smoothly; linked evidence survives sessions.

## 10. Phase 4 — RNA, quantitative, and interaction workflows

### 4.1 RNA and Sashimi

- Add collapsed/expanded junction display and multi-track comparison.
- Add Sashimi rendering using selected alignment tracks and one annotation track.
- Preserve exact endpoints, strand provenance, counts, filters, and scale semantics.
- Add bounded junction BED and Sashimi PNG/SVG export.

Gate: generated RNA truth produces exact endpoints, counts, and strands in single- and multi-sample
views, independent of displayed-read sampling.

### 4.2 Quantitative tracks

- Add automatic/fixed ranges, baseline, linear/log scale, positive/negative colours, and explicit
  missing-value behavior.
- Add bar, line, point, and heatmap rendering.
- Support raw/minimum/mean/maximum windowing where the source semantics permit it.
- Add group autoscaling, overlay/separate modes, transparency, and bounded arithmetic combinations.
- Preserve every analysis-changing option in sessions and exports.

### 4.3 Additional high-value formats

Implement in this order after common scale and panel behavior is reusable:

1. bedGraph and WIG;
2. BEDPE/interact;
3. SEG and copy-number matrices;
4. MAF mutation tracks;
5. genePred/refFlat and narrowPeak;
6. PSL if supported by demonstrated demand.

Gate: zoomed-out aggregation is deterministic, endpoint navigation uses panels, remote work remains
bounded, and common settings behave consistently across compatible track types.

## 11. Phase 5 — genome, search, annotation, and review queues

### Work

- Load indexed FASTA independently of any alignment source.
- Add explicit contig aliases such as `1` and `chr1` while retaining canonical names.
- Support genome metadata for sequence, aliases, cytobands, and default annotations.
- Keep the reference an independent pinned track with sequence copy, strand flip, and optional
  three-frame translation.
- Index annotation names, transcript IDs, and selected attributes for locus autocomplete and search.
- Add next/previous feature navigation and a rich feature inspector.
- Create, edit, import, export, name, and delete regions of interest.
- Render ROI boundaries across all tracks and panels.
- Add gene/locus lists, next/previous review actions, and local review state.
- Ensure review persistence never embeds private sequence or read identifiers by default.

### Gate

A custom genome, aliases, annotation, ROIs, and a review list restore on a clean installation; gene
navigation resolves deterministically and never chooses an ambiguous transcript silently.

## 12. Phase 6 — sessions, automation, export, and accessibility

### 6.1 Portable sessions

- Add explicit migrations for every released schema version.
- Preserve all track, panel, genome, review, and analysis-changing state.
- Separate global defaults from per-track overrides.
- Support relative paths for portable project directories.
- Improve common IGV XML import while reporting ignored semantics.
- Validate before save and refuse to overwrite unknown future schemas.

### 6.2 Deterministic automation

- Implement either a versioned batch file or a stable local command API.
- Minimum operations: new workspace, load genome, load track, goto, add panel, set option, set
  filter, wait until settled, snapshot, export evidence, save session, and exit.
- Define **settled** from query generations, pending engine work, uploads, and layout state—not a
  fixed delay.
- Keep credentials out of commands, logs, sessions, diagnostics, and export metadata.

### 6.3 Export and accessibility

- Retain deterministic PNG and add faithful SVG for supported layers.
- Add bounded pileup, modification-frequency, junction BED, ROI BED, and selected evidence exports.
- Complete keyboard navigation for tracks, panels, inspectors, menus, dialogs, and review queues.
- Add accessible names and announcements for genomic symbols and state changes.
- Validate text scaling, high contrast, reduced motion, focus order, and screen readers where the
  platform stack permits them.

### Gate

Automation reconstructs a multi-track, multi-panel review and its evidence from a portable session
on a clean installation; all core review actions are keyboard accessible.

## 13. Phase 7 — qualification and public beta

Run these gates against the exact beta artifact, not a debug build or a nearby commit:

- real-data corpus: Illumina paired-end, ONT, PacBio HiFi/CLR, RNA-seq, amplicon, deep coverage,
  indels, modifications, chimeric reads, and structural evidence;
- backend matrix: local BAM/CRAM, remote BAM, remote CRAM/reference, BAI/CSI/CRAI, redirects,
  authentication redaction, cache behavior, interrupted requests, and reopen recovery;
- failure matrix: missing/corrupt index, truncated data, wrong/partial reference, malformed tags,
  device loss, close during query, and one-hour navigation soak;
- platform matrix: supported macOS, Windows, and Linux versions; integrated/discrete GPU; Retina and
  fractional scaling; mouse, trackpad, keyboard, and accessibility input;
- release matrix: signed/notarized packages, checksums, licences, SBOM/provenance, clean-machine
  install/update/uninstall, and documented data/cache locations; and
- matched RGV-versus-IGV measurements using the same data, locus, viewport, warm/cold state,
  hardware, and retained raw results.

No public “faster than IGV” claim is made until the comparison is reproducible and identifies the
specific metric, data, hardware, and software versions.

### Beta exit criteria

- Phases 0–3 are complete.
- Phase 4.1 is complete if RNA support is advertised; common quantitative controls are complete.
- Standalone FASTA, feature search, and ROIs are usable.
- Session migration, deterministic automation, and accessibility smoke tests pass.
- No open correctness, crash, stale-frame, data-loss, secret-leak, or unbounded-work defect remains.
- Signed candidate artifacts pass clean-machine installation and the full declared support matrix.

## 14. Cross-cutting performance budgets

Budgets must be measured in optimized builds on named reference hardware. Existing interaction
benchmarks remain regression gates; multi-track/panel work adds scaling gates rather than replacing
them.

| Metric | Beta target |
| --- | --- |
| Cached pan/zoom median | <= 16 ms |
| Cached pan/zoom p95 | <= 33 ms |
| Local first useful frame at ordinary locus | <= 100 ms |
| Remote first useful frame on controlled warm connection | <= 500 ms |
| UI-thread file/network I/O | 0 operations |
| Obsolete result replacing active view | 0 occurrences |
| Unexplained empty frame during retained-data navigation | 0 occurrences |
| Interaction frame while work is pending | <= 16.7 ms target, <= 33 ms p95 |
| Workspace memory | Explicit cap with visible per-track/workspace provenance |
| GPU primitives and labels | Explicit deterministic cap |
| Preview, clipboard, and data export | Explicit deterministic cap or streamed output |

Every benchmark record should include build identifier, source checksum or safe manifest ID, locus,
viewport, settings, hardware, OS, warm/cold state, retained-record count, primitive count, and RSS.

## 15. Verification required for every phase

A phase can close only when it contains all applicable evidence:

- deterministic fixture truth or a checksummed private-data manifest;
- focused model, engine, layout, rendering, and interaction tests;
- local/remote semantic equivalence where both paths support the format;
- session round-trip and backward-compatibility tests;
- screenshot or deterministic render evidence for visible behavior;
- failure, cancellation, stale-generation, and bounded-work tests;
- formatting, complete workspace tests, strict Clippy, release build, and synthetic validation;
- hot-path timings and native RSS for performance-sensitive work;
- release-build manual acceptance on representative real data; and
- updated user guide, compatibility notes, changelog, and this status plan.

## 16. First implementation sequence

The next slices should be small, reviewable, and independently green:

1. **Done:** fix the session-v2 core compile failure and complete focused migration/validation
   tests.
2. **Done:** adapt desktop session load/save to v2 without losing additional alignment-track
   entries.
3. **Done:** introduce stable per-track runtime identity and an ordered controller collection.
4. **Done:** route query results, errors, retained frames, and cancellation by track ID and
   generation.
5. **Done:** add open/close/reorder/rename/hide/collapse, primary-inclusive ordering, a complete
   per-track settings editor, and track-aware semantic inspection with independent state.
6. **Done:** enforce shared decoded-cache and hard retained-record budgets, expose combined plus
   per-track diagnostics, and record one/two/four-track native RSS scaling evidence.
7. **Done:** add stable-ID alignment multi-selection and compatible bulk visibility, collapse, and
   appearance settings while preserving sample-specific filters and caps.
8. **Partial:** add and schema-test the four-track failure-isolation session fixture, prove three
   healthy deep-locus queries survive the fourth controller's failure, and record four-track
   headless interaction/RSS evidence; release-desktop manual acceptance remains.
9. **Done:** introduce stable `PanelId`, `PanelGeneration`, and panel/track projection identity;
   move camera, history, selection, and generation into a single-panel controller; route engine
   results and diagnostics by panel; key layouts by panel and track; and retain the latest unread
   engine result independently for each panel without changing visible behavior.
10. Expand that abstraction to ordered visible panels and route mate/`SA` navigation through it.

Do not begin broad VCF, Sashimi, or new-format work until the four-track and panel foundations are
green. Otherwise every later workflow will need a second ownership and persistence rewrite.

### Latest automated gate

Verified on 22 August 2026:

```text
cargo fmt --all -- --check
cargo test --workspace --all-targets                         # 231 passed
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --release -p rgv-desktop
cargo run --release -p rgv-validation-runner -- validation/synthetic-corpus.json
```

All commands passed. This is automated engineering evidence, not a substitute for the four-track
real-data interaction and RSS gate in item 8.

## 17. Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| Multi-track refactor destabilizes the strong single-track viewer | Land compatibility-preserving slices; keep focused single-track regression gates |
| Per-track caps multiply memory and GPU work | Enforce global budgets before four-track acceptance |
| Track/panel vector indices leak into identity | Require stable IDs in messages, caches, picking, sessions, and exports |
| Layout changes revive zoom flicker or row reshuffling | Retain useful frames and stable row identities across query/detail transitions |
| Aggregate lanes accidentally use sampled reads | Keep aggregate query products separate and test exact truth under display caps |
| Remote CRAM remains opaque | Complete the explicit backend/proxy measurement decision before claiming remote parity |
| Sessions silently lose new state | Round-trip every analysis-changing setting and reject unsupported future schemas |
| Performance claims become anecdotal | Pin workloads and retain matched raw RGV/IGV results |
| Format breadth delays core usability | Require common track abstractions and demonstrated workflow demand |

## 18. Deliberate deferrals

The following are not beta blockers unless user research changes the scope:

- BLAT and external search-service integration;
- Hi-C matrix and circular genome views;
- GISTIC, GWAS, and broad specialist-format parity;
- a public plugin API;
- cloud collaboration and shared projects;
- clinical interpretation or decision support; and
- hosted genome catalogues beyond what is required for local genome usability.

They should be reconsidered only after the core viewer is qualified, versioned, and supported.

## 19. Progress tracking

At the end of each implementation slice:

1. update the status table in Section 3;
2. record the exact validation commands and results;
3. link benchmark or fixture evidence committed to the repository;
4. record remaining manual acceptance rather than promoting Candidate to Done prematurely; and
5. update the first incomplete item in Section 16.

The immediate next action is Section 16, item 8's release-desktop manual acceptance and item 10's
ordered visible-panel model. The automated panel identity and routing foundation in item 9 is green.
