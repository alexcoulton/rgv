# Phase 9 qualification status

Status date: 23 August 2026

Phase 8's models and major workflows are implemented. The final desktop feature-access audit and
its **505-test close gate passed**, correcting remote URL, per-source custom-index persistence,
compressed annotation, signed-URL inference, and credential-safe label wiring. The user's broad
hands-on product pass is now the next action. Two ready-to-open projects supply 19 mechanically
verified auxiliary track examples plus multi-panel paired, RNA, long/specialist, and long-CIGAR
alignment truth. Earlier qualification evidence
remains useful baseline evidence, but it does not replace that manual pass.

| Gate | Existing evidence | Status | What remains after Phase 8 |
| --- | --- | --- | --- |
| Automated correctness | Formatting, 505 active Rust tests, strict all-feature Clippy, release build, synthetic corpus, verified mixed-format generator, and focused native project smoke; live official IGV TDF compatibility probe | Passed after the desktop-access audit | Repeat on platform CI and final release artifacts |
| Controlled local/remote backend | BAI/CSI/CRAI/reference/range/failure matrix and simulated WAN report | Prior targeted pass | Repeat affected semantics; run the private technology corpus and controlled external WAN/proxy test |
| Cached interaction and retained-data relayout | 1x1, 1x2, 4x2, 8x2 and real ONT reports; fixed cached workloads below their budgets; final-feature packaged Metal batch has 7.19 ms median and 10.24 ms p95 presented frames | Targeted packaged pass | Measure ordinary cached pan/zoom in the packaged desktop; longer resource sampling |
| Real ONT engineering baseline | User demo BAM at chr6, 1,332 reads and modified-base evidence | Prior targeted pass | Broader ONT loci and user visual acceptance |
| Exact macOS payload | Final-feature unsigned arm64 app and sealed archive verified before and after unpacking; internal executable completed a 19-command Metal batch | Targeted packaged pass | Sign/notarize, clean-install and run hardware/screen-reader matrix |
| Windows and Linux packages | Layouts, metadata, manifests and host-independent tests | Not qualified | Actual platform builds, signing where applicable, clean installation, GPU/input/scaling tests |
| Broad real-data corpus | Strict runner/schema requires fourteen category-plus-semantic rows; synthetic truth observes SMRT, SBX, and Ultima evidence | Not run | Pinned Illumina, ONT, PacBio, RNA, amplicon, bisulfite, SBX, Ultima, single-cell, high-depth, SV-heavy and reference-dependent CRAM manifest |
| Broad hands-on product review | Verified 19-track mixed-format project and four-panel alignment-review project, short launch sequence, checklist, and finding template prepared | Ready to begin | User opens both checked-in projects, then continues with representative data |
| Accessibility | AccessKit and deterministic keyboard/semantic-node tests | Targeted only | Platform screen readers, full keyboard review, contrast/scaling hands-on pass |
| Recovery and soak | Automated atomic-save, relocation and recovery tests | Targeted only | Packaged unclean-exit exercise, shutdown under work and one-hour mixed-use soak |
| Matched IGV comparison | Protocol, sample schema and rejecting report compiler exist | Not run | Pin final builds and collect matched startup, interaction, memory, remote and export samples |
| Release signatures/provenance | SBOM, licenses, internal manifests and release workflows exist | Not run | Protected signing, notarization, final checksums and attestations on tagged artifacts |

## Findings already retained

- Disk-backed remote-cache eviction deletes the evicted file and updates its observable counter.
- A null header from a severely truncated indexed BAM becomes `InvalidAlignmentHeader` rather than
  a `rust-htslib` panic.
- Remote reports separate range-only and total time and retain retry, eviction and cancellation
  counters.
- Fixed baselines include vertical-density and selected-to-top relayout plus 8x2 and real ONT
  workloads.
- Removing an auxiliary track now deletes it from the bounded actor as well as every panel and
  invalidates stale queries, selections, searches, arithmetic operands, and final-channel TDF
  source ownership. Back-to-back actor remove/query ordering and shared-source release are covered.
- Removing or replacing an alignment track now clears its stable identity from every locus,
  promotes retained primary state consistently, rebalances surviving queries, and retires the
  cancelled BAM/CRAM actor off the event thread rather than waiting during pointer interaction.
- Hidden auxiliary tracks are excluded from viewport actor queries. A hidden quantitative source
  remains in the query only while a visible arithmetic track depends on it, and visibility changes
  invalidate unread auxiliary and alignment results across every panel.
- The post-freeze unsigned macOS arm64 archive was verified after unpacking, and its internal
  executable completed all nineteen automation commands through Metal. Reviewed SVG hashes stayed
  unchanged; native presentation recorded 7.19 ms median and 10.24 ms p95 with no dropped samples,
  truncated frames, or GPU buffer reallocations.
- A strict qualification binary now verifies exact batch/report command correspondence, all nine
  generated artifacts, reviewed hashes, saved project identities/settings, recomputed frame
  summaries, and declared native/GPU limits. The accepted machine-readable record pins every input
  and output digest without retaining absolute paths; tampering and command-identity regressions
  prove the rejecting path.
- The private-corpus completeness gate now rejects category labels without applicable normalized
  evidence. Its fourteen required rows cover PacBio kinetics, SBX, Ultima, and reference-dependent
  CRAM in addition to the earlier technology matrix; the checked-in synthetic corpus now requires
  all eight observed valid SMRT-kinetic records alongside its existing SBX and Ultima truth.
- The qualification workflow covers strict all-feature gates, packaging tests and the fixed
  short-read scaling matrix.
- The mixed-format handoff project opened all 19 configured auxiliary sources without failure in the release
  desktop, expanded both named TDF channels, settled, exported its full track list and a non-empty
  SVG, and exited with a successful automation report.
- The companion alignment handoff project is generated from the six pinned semantic loci and is
  rejected unless its four panels, Review queue, proper pairs, 10,000 junctions, long/specialist
  evidence, and 70,000-operation CIGAR reopen and query correctly. Its release desktop smoke
  restored all four panels, settled in 489 ms, exported the track inventory and non-empty SVG, and
  exited with no dropped frame samples or truncated GPU frames.

## Next action

Begin the user's ordinary-use and representative-data pass in
`validation/MANUAL_QUALIFICATION.md`, starting with the checked-in mixed-format and alignment-review
projects. Record
concrete findings and fix them before later release qualification. Do not optimize the roughly
400--800 ms multi-track cold-complete baseline unless hands-on testing identifies a real usability
problem or a measured regression.
