use std::ops::Range;

use rgv_alignment_scene::SceneRevision;

/// Copyable row-range coordinates used by constant-size worker telemetry. Keeping this separate
/// from [`Range`] lets every engine event carry the latest diagnostic state without retaining
/// scene authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneTelemetryRowRange {
    pub start: u32,
    pub end: u32,
}

impl From<Range<u32>> for AlignmentSceneTelemetryRowRange {
    fn from(value: Range<u32>) -> Self {
        Self {
            start: value.start,
            end: value.end,
        }
    }
}

/// Terminal state of the most recent optional vertical-presentation checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentVerticalResidencyPumpOutcome {
    /// One independently keyed semantic row block was warmed without replacing the visible scene.
    Warmed,
    /// The warmed block directory now covers every row in the stable atlas.
    Completed,
    /// The optional candidate was refused by the shared allocation policy; the visible scene was
    /// preserved and the lane stopped warming.
    BudgetLimited,
    /// Preparation failed for a reason other than ordinary allocation pressure.
    Failed,
}

/// Constant-size trace record for one semantic vertical-residency checkpoint. Target page counts
/// describe only the independently warmed row block; no cumulative snapshot is constructed.
/// `target_*_pages` is absent when preparation failed before that block was admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentVerticalResidencyPumpTelemetry {
    pub checkpoint: u64,
    pub current_scene: SceneRevision,
    pub published_scene: Option<SceneRevision>,
    pub current_prepared_rows: AlignmentSceneTelemetryRowRange,
    pub target_prepared_rows: AlignmentSceneTelemetryRowRange,
    pub total_rows: u32,
    pub current_read_pages: u64,
    pub current_summary_pages: u64,
    pub target_read_pages: Option<u64>,
    pub target_summary_pages: Option<u64>,
    pub target_read_page_cache_misses: Option<u64>,
    pub prepare_elapsed_nanoseconds: u64,
    pub pump_elapsed_nanoseconds: u64,
    pub outcome: AlignmentVerticalResidencyPumpOutcome,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneCoordinatorTelemetry {
    pub camera_updates: u64,
    pub camera_only_updates: u64,
    /// Row-camera intents coalesced behind one bounded visible-block presentation checkpoint.
    pub row_camera_updates_deferred: u64,
    pub foreground_row_block_publications: u64,
    pub return_cache_demotions: u64,
    pub return_cache_entries_demoted: u64,
    pub source_work_started: u64,
    pub source_checkpoints: u64,
    pub source_retargets: u64,
    pub source_yields: u64,
    pub source_acquisitions: u64,
    pub source_refusals: u64,
    pub evidence_candidates: u64,
    /// Non-empty typed view-filter derivations evaluated over already retained canonical
    /// evidence. These counters distinguish CPU predicate work from BAM/CRAM source acquisition.
    pub view_filter_evaluations: u64,
    pub view_filter_records_examined: u64,
    pub view_filter_records_matched: u64,
    pub view_filter_evaluation_nanoseconds: u64,
    pub last_view_filter_evaluation_nanoseconds: u64,
    pub last_view_filter_contract_fingerprint: u64,
    pub last_view_filter_evaluation_fingerprint: u64,
    pub exact_summary_candidates: u64,
    pub row_atlas_candidates: u64,
    pub presentation_candidates: u64,
    pub scene_publications: u64,
    pub foreground_submissions: u64,
    pub speculation_submissions: u64,
    pub resource_pressure_speculation_aborts: u64,
    pub vertical_residency_checkpoints: u64,
    pub vertical_residency_publications: u64,
    /// Independently keyed row blocks warmed without replacing the visible scene snapshot.
    pub vertical_residency_blocks_warmed: u64,
    pub vertical_residency_completions: u64,
    pub vertical_residency_budget_limits: u64,
    pub vertical_residency_failures: u64,
    /// Sum of independently warmed read-page key occurrences. Growth is linear in warmed blocks,
    /// never the triangular sum of successively larger cumulative manifests.
    pub vertical_residency_read_page_occurrences: u64,
    pub vertical_residency_read_page_cache_misses: u64,
    /// Summary-page counterpart to `vertical_residency_read_page_occurrences`.
    pub vertical_residency_summary_page_occurrences: u64,
    /// Cumulative wall time spent inside semantic presentation-store preparation.
    pub vertical_residency_prepare_nanoseconds: u64,
    /// Cumulative wall time for complete vertical pump checkpoints, including staging/publication.
    pub vertical_residency_pump_nanoseconds: u64,
    /// Exact state and duration of the latest attempted vertical checkpoint.
    pub last_vertical_residency_pump: Option<AlignmentVerticalResidencyPumpTelemetry>,
    pub optional_work_pauses: u64,
    pub optional_work_resumes: u64,
    /// Exact camera-row activity queries served from the immutable atlas index.
    pub row_activity_queries: u64,
    /// Queries whose camera interval exactly matched the retained activity profile.
    pub row_activity_cache_hits: u64,
    /// Queries advanced incrementally through the retained atlas-bound activity cursor.
    pub row_activity_cursor_updates: u64,
    /// Queries which had to initialize a cursor rather than advance the current one.
    pub row_activity_rebuilds: u64,
    /// Interval-index entries whose start/end predicate changed across camera updates.
    pub row_activity_intervals_examined: u64,
    /// Entries whose active membership actually changed across camera updates.
    pub row_activity_intervals_changed: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AlignmentRowActivityTelemetry {
    pub(crate) queries: u64,
    pub(crate) cache_hits: u64,
    pub(crate) cursor_updates: u64,
    pub(crate) rebuilds: u64,
    pub(crate) intervals_examined: u64,
    pub(crate) intervals_changed: u64,
}

impl AlignmentRowActivityTelemetry {
    pub(crate) fn record_cache_hit(&mut self) {
        self.queries = self.queries.saturating_add(1);
        self.cache_hits = self.cache_hits.saturating_add(1);
    }

    pub(crate) fn record_cursor_update(
        &mut self,
        stats: rgv_alignment_scene::AlignmentRowActivityUpdateStats,
    ) {
        debug_assert!(!stats.rebuilt());
        self.queries = self.queries.saturating_add(1);
        self.cursor_updates = self.cursor_updates.saturating_add(1);
        self.record_interval_work(stats);
    }

    pub(crate) fn record_rebuild(
        &mut self,
        stats: rgv_alignment_scene::AlignmentRowActivityUpdateStats,
    ) {
        debug_assert!(stats.rebuilt());
        self.queries = self.queries.saturating_add(1);
        self.rebuilds = self.rebuilds.saturating_add(1);
        self.record_interval_work(stats);
    }

    pub(crate) fn record_interval_work(
        &mut self,
        stats: rgv_alignment_scene::AlignmentRowActivityUpdateStats,
    ) {
        self.intervals_examined = self
            .intervals_examined
            .saturating_add(stats.examined_intervals() as u64);
        self.intervals_changed = self
            .intervals_changed
            .saturating_add(stats.changed_intervals() as u64);
    }
}
