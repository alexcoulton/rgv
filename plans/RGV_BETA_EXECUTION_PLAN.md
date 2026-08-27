# RGV Beta Execution Plan

Status date: 23 August 2026

## Purpose

This is the short, current execution plan. `RGV_IGV_PARITY_ROADMAP.md` remains the detailed
technical authority and historical record. Older implementation and qualification documents are
design records, not competing task queues.

The order is deliberately simple:

1. implement and expose every agreed major feature;
2. give the completed application to the user for broad manual testing;
3. fix concrete findings; and
4. qualify and package the resulting release candidate.

Performance measurements do not interrupt feature work unless they expose a serious regression or
make the application impractical to use. A roughly 500 ms load is acceptable at this stage.

## Current position

The code audit recorded Phases 0–8 of the detailed roadmap as implemented. The project is not at
Phase 0. The complete manual fixture suite is now mechanically ready, so the project is at the
start of the user's broad manual pass.

Before handoff, one implementation check remained: prove that every major capability is reachable
through the native desktop UI and not merely present in a library, command-line flag, automation
command, saved project, or test fixture. Any gap found by this audit is implementation work and is
fixed before manual testing begins.

The access and fixture audits corrected real gaps, including:

- remote BAM/CRAM, FASTA, VCF/BCF, BigWig, TDF, BigBed, bigGenePred, and bigNarrowPeak now have a
  desktop **Open URL** workflow with custom alignment/reference indexes and optional CRAM reference
  input;
- `.bed.gz`, `.gff3.gz`, and `.gtf.gz` files now work through **Add track** and drag-and-drop as
  documented, and signed URLs retain correct format inference without exposing their query in
  track names or status text;
- the focused Ultima fixture covers valid forward/reverse `tp`, `mc`, and `t0` evidence plus
  missing, wrong-length, and wrong-type controls;
- cohort VCF and BCF contain identical 128-sample truth, including a readable malformed-genotype
  boundary; and
- ROI/review imports use the exact versioned desktop syntax rather than an ad-hoc table.

## Stage 1 — finish the major-feature boundary

Status: **Complete.**

For each row below, verify the complete path: open/create, render, interact, inspect, configure,
save, reopen, fail explicitly, and run within declared bounds where applicable.

| Major workflow | Implementation status | Pre-handoff check |
| --- | --- | --- |
| Short-read and paired-read BAM/CRAM review | Implemented | Verify all controls and mate actions are reachable |
| Long-read, supplementary-chain, clipping, indel, and modified-base review | Implemented | Verify inspector, chain navigation, and display controls are reachable |
| Bisulfite, PacBio kinetics, SBX, and Ultima BAM semantics | Implemented | Verify explicit mode selection, legends, missing evidence, and persistence |
| Multiple alignment tracks and multiple loci | Implemented | Verify add/remove/reorder/select and linked navigation from ordinary UI |
| Coverage, consensus, read/reference copy, grouping, sorting, and density | Implemented | Verify track settings and inspectors expose every action |
| VCF/BCF, genotypes, cohorts, pedigrees, filters, and evidence links | Implemented | Verify settings, virtualized sample inspection, navigation, and export |
| RNA junction and Sashimi review | Implemented | Verify single/multi-sample controls, inspection, and export |
| Quantitative, arithmetic, overlay, copy-number, mutation, and interaction tracks | Implemented | Verify add, settings, selection, endpoint navigation, and export |
| Annotation, transcript, peak, PSL, GWAS, BigBed, and TDF tracks | Implemented | Verify every advertised filename and URL path reaches the correct typed track |
| Standalone genomes, aliases, search, translation, ROIs, and review queues | Implemented | Verify all management and navigation entry points |
| Sample metadata, bulk actions, projects, recovery, and IGV import | Implemented | Verify import/edit/save/reopen/recovery entry points |
| Automation, structured/vector export, local control, and accessibility | Implemented | Verify ordinary UI access where appropriate and documented opt-in CLI access otherwise |
| Local and remote source opening | Implemented | Verify file and URL dialogs, distinct custom indexes, errors, and project restore |

Stage 1 closed on 23 August 2026 when:

- no agreed major feature is available solely through internal code or an undocumented path;
- all discovered access/wiring gaps have focused automated regressions;
- formatting, all workspace tests, strict all-feature Clippy, the release desktop build, and the
  pinned synthetic validation corpus pass; and
- a release build plus simple launch instructions are ready for the user.

The close gate passed formatting, all 505 active workspace tests, strict all-feature Clippy, the
optimized desktop build, and the pinned synthetic semantic corpus. The detailed access inventory
is in `docs/DESKTOP_FEATURE_ACCESS.md`. The ready-to-open mixed-format handoff project adds 19
auxiliary track examples to the checked-in alignment/reference truth; its generator mechanically
reopens and queries every source. A companion four-panel alignment project and six-item Review
queue expose the paired, deep, RNA, structural-pair, long/supplementary/modified/SMRT/SBX, and
70,000-operation-CIGAR truth. The release desktop restored and settled both projects through Metal,
exported their track inventories and visible-workspace SVGs, and exited successfully; the
mixed-format project also expanded both TDF channels.

The subsequent complete fixture suite adds 17 focused projects, 15 scenario cards, three export
batches, 116 declared artifacts, 35 reused dependencies, 34 production semantic checks, and 59
feature/format catalog rows with no unresolved planned row. Disposable double generation proves a
stable declared tree hash, mutation/stale/missing rejection, relocation, source reopening, and the
50 MiB budget. The loopback lab passes range, conditional, authentication, redirect, retry,
validator, corruption, truncation, outage, slow-response, recovery, and clean-shutdown cases. Two
native Metal automation batches produced every declared alignment/variant/track export plus PNG and
SVG evidence. This is fixture readiness, not a recorded human pass.

Do not add broad performance tuning, platform matrices, signing, long soaks, or matched IGV
benchmarking to this stage.

## Stage 2 — user-led broad manual testing

Status: **Ready to begin.**

The user tests the release desktop as an ordinary genome viewer using representative local data.
The pass covers visual meaning and readability, navigation feel, short and long reads, variants,
RNA, quantitative and annotation tracks, multi-track/multi-locus work, sessions, recovery, and
exports.

`validation/manual-suite/README.md` is the focused fixture index and
`validation/MANUAL_QUALIFICATION.md` is the broader checklist. They are guidance rather than a
demand for the user to reproduce every automated test. Findings should describe the fixture/data,
locus, action, expected result, and observed result, with screenshots where helpful.

The initial priority order is:

1. incorrect biological or coordinate meaning;
2. crashes, stale or disappearing evidence, data loss, or credential leakage;
3. broken or inaccessible workflows;
4. confusing, unstable, or visually poor interaction;
5. performance that actually feels disruptive; and
6. cosmetic refinements.

A measured operation near 500 ms is not a defect by itself. It becomes work if the manual pass
shows that the delay is disruptive, blocks interaction, or has regressed materially.

## Stage 3 — fix manual findings

For each confirmed finding:

1. reproduce it with the smallest safe input;
2. add an automated regression where practical;
3. fix the underlying model, I/O, layout, rendering, or UI behavior;
4. run the affected focused gate; and
5. ask the user to repeat the original scenario.

Unrelated qualification work remains paused while correctness or major usability findings are
open.

## Stage 4 — release qualification

Only after the broad manual pass and its important findings are closed:

- run the private real-technology corpus and controlled external-WAN checks;
- run the one-hour mixed-use soak and recovery exercises;
- qualify macOS, Windows, and Linux packages on declared hardware/input/scaling combinations;
- perform screen-reader and full keyboard acceptance;
- sign, notarize, install, upgrade, and verify exact release artifacts; and
- run the matched RGV-versus-IGV protocol before publishing narrow performance claims.

Existing benchmark and packaged-automation evidence is retained. It should not be repeatedly
rebuilt or tuned unless the application changes in a relevant way or manual testing identifies a
real problem.

## Immediate queue

1. **Done:** complete the desktop feature-access and persistence audit.
2. **Done:** fix every access or wiring gap found by that audit with focused tests.
3. **Done:** pass the complete Stage 1 correctness gate with 505 active tests.
4. **Done:** provide the verified mixed-format and alignment-review projects, release-build launch
   instructions, and focused alignment loci.
5. **Done:** generate and mechanically qualify the complete MF-01–MF-15 manual fixture suite,
   including focused biological truth, invalid inputs, portable projects, automation batches, and
   the controlled loopback lab.
6. **Next:** record and fix concrete Stage 2 manual findings before beginning release
   qualification.
