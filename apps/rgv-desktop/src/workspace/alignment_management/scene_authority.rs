use crate::diagnostics::ManualDiagnosticTraceFields;
use crate::workspace::alignment_navigation::AlignmentNavigationPhase;
use crate::workspace::panels::{
    ReadPanelScrollState, ReadRowCoordinateAuthority, ViewPanelController,
};
use crate::{AlignmentGpuPrefetchState, ui};
use rgv_alignment_scene::{
    AlignmentPanelSceneFrame, AlignmentPanelSceneStore, AlignmentSceneCamera,
    AlignmentSceneSnapshot, AlignmentSemanticRetirementStats, AllocationRetirementClaimSet,
    AllocationRetirementToken, ReferenceRevision, RowAtlasRevision,
};
use rgv_core::{GenomicInterval, PanelId, TrackId, Viewport};
use rgv_engine::{
    AlignmentIndividualReadCapability, AlignmentSceneRequestToken, AlignmentVerticalResidency,
    SourceOwnerId,
};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

// Optional work begins at one cancellable page and adapts from measured wall time. This is a
// scheduler target, not a correctness timeout or residency cap.
pub(super) const ALIGNMENT_GPU_PREFETCH_TARGET_QUANTUM: Duration = Duration::from_millis(2);

pub(super) const ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM: usize = 64;

pub(super) fn retire_reference_install_receipt_for_contract_epoch(
    receipts: &mut BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
    owner: SourceOwnerId,
) -> bool {
    receipts.remove(&owner).is_some()
}

impl AlignmentGpuPrefetchState {
    pub(super) fn maximum_pages(&self) -> NonZeroUsize {
        NonZeroUsize::new(
            self.page_quantum
                .clamp(1, ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM),
        )
        .expect("the adaptive prefetch quantum is non-zero")
    }

    pub(super) fn observe_quantum(&mut self, elapsed: Duration, processed_pages: usize) {
        if processed_pages == 0 || elapsed.is_zero() {
            return;
        }
        let measured = ALIGNMENT_GPU_PREFETCH_TARGET_QUANTUM
            .as_nanos()
            .saturating_mul(processed_pages as u128)
            .checked_div(elapsed.as_nanos().max(1))
            .unwrap_or(1)
            .clamp(1, ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM as u128)
            as usize;
        let lower = self.page_quantum.saturating_div(2).max(1);
        let upper = self
            .page_quantum
            .saturating_mul(2)
            .clamp(1, ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM);
        self.page_quantum = measured.clamp(lower, upper);
    }
}

#[derive(Clone)]
pub(super) struct CapturedAlignmentLaneSource {
    pub(super) panel_id: PanelId,
    pub(super) track_id: TrackId,
    pub(super) semantic_request_token: AlignmentSceneRequestToken,
    pub(super) desired_request_token: AlignmentSceneRequestToken,
    pub(super) camera: AlignmentSceneCamera,
    pub(super) individual_reads: AlignmentIndividualReadCapability,
    pub(super) semantic_target_complete: bool,
    pub(super) reference_ready: bool,
    pub(super) vertical_residency: AlignmentVerticalResidency,
    pub(super) camera_sequence: u64,
    pub(super) snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    pub(super) scene_contracts_current: bool,
}

pub(super) struct CapturedAlignmentPanelSceneEpoch {
    pub(super) panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
    pub(super) lane_sources: Vec<CapturedAlignmentLaneSource>,
    pub(super) panel_scene_stores: HashMap<PanelId, AlignmentPanelSceneStore>,
}

/// Visual inputs owned by one read-panel scroll derivative. Presentation target revision and exact
/// materialized-page coverage are intentionally absent: both can advance with a camera-only native
/// commit while the already-built scrollbar/read-stack geometry remains valid.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ReadPanelDisplayDerivativeAuthority {
    pub(super) density_camera: bool,
    pub(super) row_origin: Option<f32>,
    pub(super) row_height: Option<f32>,
    pub(super) row_coordinate_authority: ReadRowCoordinateAuthority,
    pub(super) row_activity_atlas_revision: Option<RowAtlasRevision>,
    pub(super) total_rows: Option<usize>,
    pub(super) authoritative_overview: Option<ui::ScrollbarOverview>,
    pub(super) overview: ui::ScrollbarOverview,
    pub(super) actual_content_bottom: f32,
    pub(super) scroll: ReadPanelScrollState,
}

pub(super) fn read_panel_display_derivative_authority(
    panel: &ViewPanelController,
    read_panel_index: usize,
    track_id: TrackId,
) -> ReadPanelDisplayDerivativeAuthority {
    let read_panel = &panel
        .scroll_display_cache
        .as_ref()
        .expect("read panel index came from this cache")
        .read_panels[read_panel_index];
    ReadPanelDisplayDerivativeAuthority {
        density_camera: read_panel.density_camera,
        row_origin: read_panel.row_origin,
        row_height: read_panel.row_height,
        row_coordinate_authority: read_panel.row_coordinate_authority,
        row_activity_atlas_revision: read_panel.row_activity_atlas_revision,
        total_rows: read_panel.total_rows,
        authoritative_overview: read_panel.authoritative_overview.clone(),
        overview: read_panel.overview.clone(),
        actual_content_bottom: read_panel.actual_content_bottom,
        scroll: panel.read_panel_scroll_state(track_id),
    }
}

pub(super) struct AlignmentPanelSceneEpochCaptureFailure {
    pub(super) message: String,
    pub(super) fields: ManualDiagnosticTraceFields,
}

/// The immutable genomic interval belongs to navigation, while the horizontal pixel transform
/// belongs to the committed workspace layout. During a native panel resize those two authorities
/// can advance in adjacent frame phases, so every presentation capture explicitly projects the
/// interval through the live layout width instead of borrowing a stale stored pixel width.
pub(crate) fn alignment_scene_viewport_for_layout_width(
    viewport: Viewport,
    layout_width: f64,
) -> Option<Viewport> {
    viewport.resized(layout_width).ok()
}

pub(crate) fn retained_alignment_scene_can_present(
    scene_contracts_current: bool,
    has_snapshot: bool,
    covered_interval: Option<GenomicInterval>,
    requested_interval: GenomicInterval,
) -> bool {
    scene_contracts_current
        && has_snapshot
        && covered_interval.is_some_and(|covered| covered.overlaps(requested_interval))
}

/// Whether a worker progress frame must leave the lane's committed incumbent in place instead of
/// withdrawing it.
///
/// The frame answers the exact desired request (`exact_staging`) but carries no scene which is
/// current for the desired contracts: the worker has accepted a filter, colour, sort, or other
/// contract change and is still deriving the replacement, possibly from evidence it already
/// holds, one stage per worker turn. While the navigation transaction still names an owner for
/// that replacement, the incumbent stays presentable and non-authoritative (the retained-scene
/// bridge reports it as pending) until the replacement publishes atomically. Once the
/// transaction is terminal, or the frame itself is presentable, the frame is admitted as
/// before, so a failed or refused target still withdraws pixels which contradict it.
pub(crate) const fn alignment_scene_progress_frame_defers_to_incumbent(
    exact_staging: bool,
    candidate_contracts_current: bool,
    phase: AlignmentNavigationPhase,
) -> bool {
    exact_staging
        && !candidate_contracts_current
        && matches!(
            phase,
            AlignmentNavigationPhase::Running
                | AlignmentNavigationPhase::WaitingForSourcePressure
                | AlignmentNavigationPhase::WaitingForReference
        )
}

/// A vertical-only target which has not prepared any of its requested rows is not a publishable
/// successor to a coherent native read frame. Keeping the committed camera authoritative until
/// at least one requested row is covered avoids replacing truthful pixels with an empty frame;
/// genomic movement is deliberately excluded because old pixels must never claim a new locus.
pub(crate) fn uncovered_vertical_candidate_retains_committed_pixels(
    committed_viewport: Viewport,
    committed_reads_complete: bool,
    committed_native_reads: bool,
    candidate_viewport: Viewport,
    candidate_requested_rows: &std::ops::Range<u32>,
    candidate_covered_rows: &std::ops::Range<u32>,
) -> bool {
    committed_reads_complete
        && committed_native_reads
        && committed_viewport == candidate_viewport
        && !candidate_requested_rows.is_empty()
        && candidate_covered_rows.is_empty()
}

/// Move-only semantic half of an end-to-end alignment replacement. Both old frame captures and
/// their resident panel stores are detached together, so rollback can restore the exact prior hit
/// and display authority. The planner recursively proves snapshot, root, leaf, page, and pin
/// ownership; only allocations whose complete live reference set belongs to this token enter
/// `claim_set`, while genuinely shared ownership remains charged and protected.
#[must_use = "restore this semantic epoch after refusal or settle it with the replacement"]
pub(super) struct AlignmentSemanticEpochRetirement {
    pub(super) claim_set: AllocationRetirementClaimSet,
    pub(super) stats: AlignmentSemanticRetirementStats,
    pub(super) panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
    pub(super) panel_scene_stores: HashMap<PanelId, AlignmentPanelSceneStore>,
}

impl AllocationRetirementToken for AlignmentSemanticEpochRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}
