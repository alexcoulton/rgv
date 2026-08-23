# Early qualification evidence (superseded phase numbering)

Status date: 22 August 2026

This file records qualification evidence collected before the feature-gap re-audit. Qualification
is now Phase 9 and is paused while Phase 8 closes the remaining major features. The current status
is maintained in `benchmark-results/PHASE9_QUALIFICATION_STATUS.md`.

| Gate | Current evidence | Status | What remains |
| --- | --- | --- | --- |
| Automated correctness | Formatting, 398 Rust tests, strict all-feature Clippy, release build, synthetic corpus | Passed on current host | Repeat on final source revision and platform CI |
| Controlled local/remote backend | Full BAI/CSI/CRAI/reference/range/failure matrix and simulated WAN report | Targeted pass | Private technology corpus and controlled external WAN/proxy run |
| Cached interaction and retained-data relayout | 1x1, 1x2, 4x2, 8x2 and real ONT reports; all fixed cached workloads under 16 ms median / 33 ms p95 | Headless pass | Packaged desktop frame/GPU/export telemetry and longer resource sampling |
| Real ONT engineering baseline | User demo BAM at chr6, 1,332 reads, modified-base evidence, raw samples and RSS | Targeted pass | Broader ONT loci and user visual acceptance |
| Exact macOS payload | Fresh unsigned arm64 `.app` manifest/metadata verified; internal executable completed 19-command Metal automation | Targeted pass | Signing, notarization, clean install/upgrade/uninstall, hardware and screen-reader matrix |
| Windows package | Layout, metadata, manifest, MSI workflow, and host-independent tests | Not qualified | Actual Windows build, signing, clean install, GPU/input/scaling tests |
| Linux package | Layout, metadata, manifest, tar/AppImage workflow, and host-independent tests | Not qualified | Actual Linux build, clean install, distribution/GPU/scaling tests |
| Broad real-data corpus | Runner/schema and required category gate are implemented | Not run | Pinned Illumina, ONT, PacBio HiFi/CLR, RNA, amplicon, bisulfite, single-cell, high-depth, and SV-heavy manifest |
| Broad hands-on product review | Complete checklist and finding template prepared | Not run | User executes `validation/MANUAL_QUALIFICATION.md`; findings are fixed and retested |
| Accessibility | AccessKit and deterministic keyboard/semantic-node tests | Targeted only | Platform screen readers, full keyboard review, contrast/scaling hands-on pass |
| Recovery and soak | Automated atomic save/relocation/recovery tests | Targeted only | Packaged unclean-exit exercise, repeated shutdown under work, one-hour mixed-use soak |
| Matched IGV comparison | Protocol, raw-sample schema, and rejecting report compiler implemented | Not run | Pin IGV/RGV builds and collect at least ten matched startup/interaction/memory/remote/export samples |
| Release signatures/provenance | SBOM, licenses, internal manifests, final checksum and attestation workflows | Not run | Protected signing credentials, notarization, final checksums/attestations on tagged artifacts |

## Findings already closed in Phase 8

- Disk-backed remote cache capacity eviction now removes the evicted file and increments the
  observable counter.
- A null alignment header returned for a severely truncated indexed BAM now becomes
  `InvalidAlignmentHeader` instead of triggering a `rust-htslib` panic.
- Remote benchmark wall time is no longer ambiguous: range-only and total time are separate, and
  retry/eviction/cancellation counters are archived.
- The fixed performance reports now include vertical-density and selected-to-top relayout from
  retained data, plus the previously missing 8x2 and real ONT workloads.
- The self-hosted qualification workflow now runs strict all-feature gates, packaging tests, and
  the complete fixed short-read scaling matrix rather than only one panel.

## Superseded next decision point

The previous next action was the user's ordinary-use pass. That is no longer current: complete the
new Phase 8 feature work first. There remains no evidence-based reason to optimize the roughly
400--800 ms multi-track cold-complete results before the finished-workflow checks identify a real
problem.
