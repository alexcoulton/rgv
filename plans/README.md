# RGV plans

**Start here:** the current entry point is the
[comprehensive review remediation plan — 2026-11-20](COMPREHENSIVE_REVIEW_REMEDIATION_PLAN_2026-11-20.md),
which tracks the findings in
[`audits/COMPREHENSIVE_REVIEW_2026-11-20.md`](../audits/COMPREHENSIVE_REVIEW_2026-11-20.md).
The [foundation and parity-priority plan](RGV_FOUNDATION_AND_PARITY_PRIORITY_PLAN.md) (11 November
2026) is still marked in progress; neither document states how the two relate, so read the
remediation plan first and the foundation plan for the product ordering behind it.

The [IGV-parity execution roadmap](RGV_IGV_PARITY_ROADMAP.md) and the
[beta execution plan](RGV_BETA_EXECUTION_PLAN.md) were previously named here as the delivery
authority. The roadmap's own 11 November 2026 correction says its status table was overstated and
that it is now kept as the record of what was built. Everything else preserves implementation,
remediation, and design history.

## Status index

Added 20 November 2026 (review finding OPS-16). Every plan document in this directory is listed
once. The classification is taken from each document's **own** status header and text, quoted or
paraphrased in the "Basis" column; it has not been independently re-verified against the code,
and the 2026-11-20 review found several "implemented and qualified" claims overstated. Where a
document has no status line, or its status conflicts with later documents, it is marked
**status unclear** rather than guessed.

- **Active** — work is open and this is a current task list.
- **Completed** — the document records its implementation as done. Remaining acceptance or
  qualification gates it names are listed.
- **Superseded** — the document itself, or a later plan that names it, says it is no longer the
  authority.

### Active

| Plan | Basis |
| --- | --- |
| [Comprehensive review remediation — 2026-11-20](COMPREHENSIVE_REVIEW_REMEDIATION_PLAN_2026-11-20.md) | Current entry point; open checklist per workstream |
| [Foundation and parity-priority plan](RGV_FOUNDATION_AND_PARITY_PRIORITY_PLAN.md) | "Status: in progress, started 11 November 2026"; supersedes the roadmap's Phase 9 ordering for engineering work |

### Completed (per the document's own status)

| Plan | Basis and gates the document leaves open |
| --- | --- |
| [Code-review remediation — 2026-09-04](CODE_REVIEW_REMEDIATION_PLAN_2026-09-04.md) | "Implementation landed on 2026-09-05"; explicitly not a claim that release/platform gates passed |
| [Alignment cache thrash and scroll-materialization recovery](ALIGNMENT_CACHE_THRASH_AND_SCROLL_MATERIALIZATION_RECOVERY_PLAN.md) | "Implementation complete; automated structural verification passed"; native user acceptance pending |
| [macOS interaction stress-testing framework](NATIVE_INTERACTION_STRESS_AUTOMATION_PLAN.md) | "Initial implementation complete" (2026-09-04); recording-overhead and soak trials remain operator-run |
| [Alignment context menu, filtering, and read presentation](ALIGNMENT_CONTEXT_MENU_AND_FILTERING_PLAN.md) | "Code-complete for the initial typed-filter/context-menu delivery on 2026-08-31"; native visual checkpoints A–D pending |
| [Unified alignment-scene interaction and capacity remediation](UNIFIED_ALIGNMENT_SCENE_INTERACTION_AND_CAPACITY_REMEDIATION_PLAN.md) | "Core implementation complete through Phase 6"; Phase 7 native user acceptance pending |
| [Unified alignment-scene recovery and desktop cutover](UNIFIED_ALIGNMENT_SCENE_RECOVERY_PLAN.md) | "Recovery complete"; R6 native checkpoint accepted 31 August 2026 |
| [Alignment browsing working set](ALIGNMENT_BROWSING_WORKING_SET_PLAN.md) | "Implemented and locally qualified" (2026-08-29); the resident-scene plan says it supersedes this plan's end-to-end latency assumptions |
| [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md) | "Implemented and qualified"; its sampling and truncation policy is superseded by the complete-read plan |
| [Dense-alignment layout performance](DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md) | "Implemented and qualified" (2026-08-28) |
| [Gene-symbol and chromosome-context usability](GENE_SYMBOL_AND_CHROMOSOME_CONTEXT_USABILITY_PLAN.md) | "Implemented and qualified" |
| [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md) | "Implemented"; native visual qualification pending |
| [Directionality rendering remediation](DIRECTIONALITY_RENDERING_REMEDIATION_PLAN.md) | "Implemented and qualified" |
| [GUI redesign](GUI_REDESIGN_PLAN.md) | "All four phases were implemented and audited on 21 August 2026"; also named as superseded for delivery order by the forward plan |
| [Built-in genome landmark integration](GENOME_LANDMARK_INTEGRATION_PLAN.md) | "Implemented locally"; distribution review and four-build manual GUI qualification remain |
| [Manual fixture suite](MANUAL_FIXTURE_SUITE_PLAN.md) | "Implemented and mechanically qualified"; manual scenario results not yet run |
| [Modularisation](MODULARISATION_PLAN.md) | No status header; its implementation record says "Implemented on 2026-08-23" |
| [ONT–IGV parity remediation](ONT_IGV_PARITY_REMEDIATION_PLAN.md) | "Implemented and qualified" |
| [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md) | "Implemented and qualified"; references to safety sampling are superseded by the complete-read plan |
| [Sashimi arc interaction](SASHIMI_ARC_INTERACTION_PLAN.md) | "Implemented — native sign-off pending" |
| [MCP gap remediation](RGV_MCP_GAP_REMEDIATION_PLAN.md) | No status line of its own; the MF-01 follow-up plan describes itself as following "completion of" this plan |
| [Track-channel architecture remediation](TRACK_CHANNEL_ARCHITECTURE_REMEDIATION_PLAN.md) | "Implemented and qualified" (2026-08-26) |
| [Workspace adaptive layout](WORKSPACE_ADAPTIVE_LAYOUT_PLAN.md) | "Implemented and qualified" (2026-08-27) |

### Superseded

| Plan | Basis |
| --- | --- |
| [IGV-parity execution roadmap](RGV_IGV_PARITY_ROADMAP.md) | Its 11 November 2026 correction: status table overstated; "kept as the record of what was built"; current order is in the foundation plan |
| [Forward implementation plan](RGV_FORWARD_IMPLEMENTATION_PLAN.md) | The roadmap §1 names it a design record and takes over as "the ordering and completion authority" |
| [Next implementation plan](RGV_NEXT_IMPLEMENTATION_PLAN.md) | Named as superseded by the forward plan |
| [Remaining implementation and qualification plan](REMAINING_IMPLEMENTATION_PLAN.md) | Own header: "Superseded on 22 August 2026" |
| [Product parity implementation plan](PRODUCT_PARITY_IMPLEMENTATION_PLAN.md) | Named as superseded by the forward plan and as a design record by the roadmap; its sampling requirements are superseded by the complete-read plan |
| [Alignment cache liveness and vertical navigation](ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md) | Own header: "implementation checkpoint failed native acceptance; corrective work is defined in" the cache-thrash recovery plan |

### Status unclear

| Plan | Why it cannot be classified from its own text |
| --- | --- |
| [Beta execution plan](RGV_BETA_EXECUTION_PLAN.md) | Describes itself (23 August 2026) as "the short, current execution plan" derived from the roadmap. The roadmap's ordering has since been superseded, but this document carries no note saying so |
| [Original implementation plan](IMPLEMENTATION_PLAN.md) | No superseded note. Its §13 status (21 August 2026) lists phases partly implemented with exit criteria open; later plans treat it as completed background |
| [Unified alignment scene and legacy removal](UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md) | Records recovery R0–R6 and Phases 7–9 as implemented with combined native visual acceptance open, and still states "Current execution authority: this plan", which later plans contradict |
| [Resident alignment scene and instant navigation](RESIDENT_ALIGNMENT_SCENE_PLAN.md) | "Status: in progress" as of 2026-08-29; not updated or marked superseded after the unified-scene plans |
| [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md) | "Status: planned", yet several plans marked implemented depend on it and cite it as in force; the foundation plan (decision D1) later made Downsampled the default read display |
| [MF-01 follow-up remediation](MF01_FOLLOWUP_REMEDIATION_PLAN.md) | "Status and scope" section describes the work but records no implementation status |
| [MCP implementation](RGV_MCP_IMPLEMENTATION_PLAN.md) | No status line. The `apps/rgv-mcp` crate it proposes exists, but the document does not record completion |
| [Format-agnostic inspector redesign](format-agnostic-inspector-redesign.md) | No status line or implementation record |

No plan document has been moved or deleted. The lists below are the previous index, kept for its
grouping.

## Delivery plans

- [Foundation and parity-priority plan](RGV_FOUNDATION_AND_PARITY_PRIORITY_PLAN.md) — proposed
  11 November 2026; reorders engineering work after an independent assessment
- [IGV-parity execution roadmap](RGV_IGV_PARITY_ROADMAP.md)
- [Beta execution plan](RGV_BETA_EXECUTION_PLAN.md)
- [Forward implementation plan](RGV_FORWARD_IMPLEMENTATION_PLAN.md)
- [Next implementation plan](RGV_NEXT_IMPLEMENTATION_PLAN.md)
- [Remaining implementation and qualification plan](REMAINING_IMPLEMENTATION_PLAN.md)
- [Product parity implementation plan](PRODUCT_PARITY_IMPLEMENTATION_PLAN.md)
- [Original implementation plan](IMPLEMENTATION_PLAN.md)

## Focused plans

- [Code-review remediation — 2026-09-04](CODE_REVIEW_REMEDIATION_PLAN_2026-09-04.md)
- [Alignment cache thrash and scroll-materialization recovery](ALIGNMENT_CACHE_THRASH_AND_SCROLL_MATERIALIZATION_RECOVERY_PLAN.md)
- [Alignment cache liveness and vertical navigation](ALIGNMENT_CACHE_LIVENESS_AND_VERTICAL_NAVIGATION_PLAN.md)
- [macOS interaction stress-testing framework](NATIVE_INTERACTION_STRESS_AUTOMATION_PLAN.md)
- [Alignment context menu, filtering, and read presentation](ALIGNMENT_CONTEXT_MENU_AND_FILTERING_PLAN.md)
- [Unified alignment-scene interaction and capacity remediation](UNIFIED_ALIGNMENT_SCENE_INTERACTION_AND_CAPACITY_REMEDIATION_PLAN.md)
- [Unified alignment-scene recovery and desktop cutover](UNIFIED_ALIGNMENT_SCENE_RECOVERY_PLAN.md)
- [Unified alignment scene and legacy removal](UNIFIED_ALIGNMENT_SCENE_IMPLEMENTATION_PLAN.md)
- [Resident alignment scene and instant navigation](RESIDENT_ALIGNMENT_SCENE_PLAN.md)
- [Alignment browsing working set](ALIGNMENT_BROWSING_WORKING_SET_PLAN.md)
- [Canonical stable read architecture](CANONICAL_STABLE_READ_ARCHITECTURE_PLAN.md)
- [Complete read presentation and resource admission](COMPLETE_READ_PRESENTATION_PLAN.md)
- [Dense-alignment layout performance](DENSE_ALIGNMENT_LAYOUT_PERFORMANCE_PLAN.md)
- [Gene-symbol and chromosome-context usability](GENE_SYMBOL_AND_CHROMOSOME_CONTEXT_USABILITY_PLAN.md)
- [Transactional navigation preview](TRANSACTIONAL_NAVIGATION_PREVIEW_PLAN.md)
- [Directionality rendering remediation](DIRECTIONALITY_RENDERING_REMEDIATION_PLAN.md)
- [GUI redesign](GUI_REDESIGN_PLAN.md)
- [Built-in genome landmark integration](GENOME_LANDMARK_INTEGRATION_PLAN.md)
- [Manual fixture suite](MANUAL_FIXTURE_SUITE_PLAN.md)
- [MF-01 follow-up remediation](MF01_FOLLOWUP_REMEDIATION_PLAN.md)
- [Modularisation](MODULARISATION_PLAN.md)
- [ONT–IGV parity remediation](ONT_IGV_PARITY_REMEDIATION_PLAN.md)
- [Persistent read-stack stability](READ_STACK_STABILITY_PLAN.md)
- [Sashimi arc interaction](SASHIMI_ARC_INTERACTION_PLAN.md)
- [MCP gap remediation](RGV_MCP_GAP_REMEDIATION_PLAN.md)
- [MCP implementation](RGV_MCP_IMPLEMENTATION_PLAN.md)
- [Track-channel architecture remediation](TRACK_CHANNEL_ARCHITECTURE_REMEDIATION_PLAN.md)
- [Workspace adaptive layout](WORKSPACE_ADAPTIVE_LAYOUT_PLAN.md)

The GUI comparison and qualification captures used by the redesign plan are under
[`assets/gui-redesign/`](assets/gui-redesign/).
