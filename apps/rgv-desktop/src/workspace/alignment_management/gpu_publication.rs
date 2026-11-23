use crate::workspace::alignment_management::scene_authority::AlignmentSemanticEpochRetirement;
use crate::workspace::alignments::{
    AlignmentLedgerProgressView, alignment_ledger_pressure_blocker_remains,
};
use crate::{
    AlignmentGpuPublicationBlocker, AlignmentGpuPublicationRetry, AlignmentGpuPublicationRetryGate,
    AlignmentScenePublicationAttempt, AlignmentScenePublicationAuthority,
    AlignmentSceneTargetIdentity,
};
use rgv_alignment_scene::{
    AlignmentCamera, AlignmentInteractionState, AlignmentPresentationScene, AlignmentSceneCamera,
    AlignmentSceneSnapshot, AllocationBundleCommit, AllocationBundleError, AllocationBundleLease,
    AllocationRetirementClaimSet, AllocationRetirementToken, CpuClipRect, CpuProjectionError,
    CpuSummaryCamera, LedgerError, PresentationContract,
};
use rgv_core::{PanelId, TrackId};
use rgv_engine::{AlignmentSceneBuildContract, AlignmentSceneRequestToken};
use rgv_render::{
    AlignmentGpuCameraCaptureError, AlignmentGpuFrame, AlignmentGpuPageAdmissionError,
    AlignmentGpuPrepareError, AlignmentGpuReplacementError, AlignmentGpuRetirement,
    AlignmentGpuSceneRequest, AlignmentGpuScissor, AlignmentSummaryFrameOverlays,
    AlignmentSummaryGpuCameraCaptureError, AlignmentSummaryGpuFrame,
    AlignmentSummaryGpuLaneOptions, AlignmentSummaryGpuPageAdmissionError,
    AlignmentSummaryGpuPrepareError, AlignmentSummaryGpuReplacementError,
    AlignmentSummaryGpuRetirement, AlignmentSummaryGpuSceneRequest, GpuBudgetPressure,
    GpuReclaimGoal,
};
use std::sync::Arc;

pub(super) enum AlignmentGpuCameraCaptureOutcome {
    Captured {
        reads: Option<AlignmentGpuFrame>,
        summaries: Option<AlignmentSummaryGpuFrame>,
    },
    ContentReplacementRequired,
    Deferred(AlignmentGpuPublicationBlocker),
    Terminal(String),
}

pub(super) const fn alignment_gpu_publication_ledger_blocker(
    error: LedgerError,
) -> Option<AlignmentGpuPublicationBlocker> {
    match error {
        LedgerError::ReplacementTransitionActive { .. } => {
            Some(AlignmentGpuPublicationBlocker::Ledger(error))
        }
        LedgerError::TransientBudgetExceeded {
            requested, limit, ..
        }
        | LedgerError::BudgetExceeded {
            requested, limit, ..
        } if requested <= limit => Some(AlignmentGpuPublicationBlocker::Ledger(error)),
        LedgerError::TransientBudgetExceeded { .. }
        | LedgerError::BudgetExceeded { .. }
        | LedgerError::ReservationExceeded { .. }
        | LedgerError::UnknownAllocation(_)
        | LedgerError::IdOverflow
        | LedgerError::Poisoned => None,
    }
}

pub(super) const fn alignment_read_prepare_ledger_error(
    error: &AlignmentGpuPrepareError,
) -> Option<LedgerError> {
    match error {
        AlignmentGpuPrepareError::Ledger(error)
        | AlignmentGpuPrepareError::Admission(AlignmentGpuPageAdmissionError::Ledger(error)) => {
            Some(*error)
        }
        AlignmentGpuPrepareError::LabelProjection(CpuProjectionError::Ledger(error)) => {
            Some(*error)
        }
        AlignmentGpuPrepareError::InvalidFrameGeometry
        | AlignmentGpuPrepareError::MissingMandatoryPages { .. }
        | AlignmentGpuPrepareError::TooManyOverlayPrimitives
        | AlignmentGpuPrepareError::DynamicUniformAddressSpace
        | AlignmentGpuPrepareError::FrameSlotCapacity { .. }
        | AlignmentGpuPrepareError::FrameSlotBusy { .. }
        | AlignmentGpuPrepareError::Admission(_)
        | AlignmentGpuPrepareError::LabelProjection(CpuProjectionError::InvalidCamera) => None,
    }
}

pub(super) const fn alignment_summary_prepare_ledger_error(
    error: &AlignmentSummaryGpuPrepareError,
) -> Option<LedgerError> {
    match error {
        AlignmentSummaryGpuPrepareError::Ledger(error)
        | AlignmentSummaryGpuPrepareError::Admission(
            AlignmentSummaryGpuPageAdmissionError::Ledger(error),
        ) => Some(*error),
        AlignmentSummaryGpuPrepareError::InvalidFrameGeometry
        | AlignmentSummaryGpuPrepareError::InvalidAlternateFraction
        | AlignmentSummaryGpuPrepareError::IncompatibleAlleleScale
        | AlignmentSummaryGpuPrepareError::MissingJunctionNormalization
        | AlignmentSummaryGpuPrepareError::InvalidJunctionScale
        | AlignmentSummaryGpuPrepareError::MissingMandatoryPages { .. }
        | AlignmentSummaryGpuPrepareError::TooManyOverlayPrimitives
        | AlignmentSummaryGpuPrepareError::DynamicUniformAddressSpace
        | AlignmentSummaryGpuPrepareError::FrameSlotCapacity { .. }
        | AlignmentSummaryGpuPrepareError::FrameSlotBusy { .. }
        | AlignmentSummaryGpuPrepareError::ResidentMetadataMismatch(_)
        | AlignmentSummaryGpuPrepareError::Page(_)
        | AlignmentSummaryGpuPrepareError::Admission(_) => None,
    }
}

pub(super) fn alignment_read_camera_capture_blocker(
    error: &AlignmentGpuCameraCaptureError,
) -> Option<AlignmentGpuPublicationBlocker> {
    match error {
        AlignmentGpuCameraCaptureError::Prepare(error) => {
            alignment_read_prepare_ledger_error(error)
                .and_then(alignment_gpu_publication_ledger_blocker)
        }
        // A compatible authority provisions two reusable slots. Desktop owns only the committed
        // frame when capture begins, so exhausting both is an invariant diagnostic, not ledger
        // pressure and must never enter the progress retry loop.
        AlignmentGpuCameraCaptureError::FrameSlotBusy { .. }
        | AlignmentGpuCameraCaptureError::ContentReplacementRequired { .. } => None,
    }
}

pub(super) fn alignment_summary_camera_capture_blocker(
    error: &AlignmentSummaryGpuCameraCaptureError,
) -> Option<AlignmentGpuPublicationBlocker> {
    match error {
        AlignmentSummaryGpuCameraCaptureError::Prepare(error) => {
            alignment_summary_prepare_ledger_error(error)
                .and_then(alignment_gpu_publication_ledger_blocker)
        }
        AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { .. }
        | AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired { .. }
        | AlignmentSummaryGpuCameraCaptureError::Page(_) => None,
    }
}

#[cfg(test)]
pub(crate) fn alignment_read_camera_capture_error_is_deferred(
    error: &AlignmentGpuCameraCaptureError,
) -> bool {
    alignment_read_camera_capture_blocker(error).is_some()
}

#[cfg(test)]
pub(crate) fn alignment_summary_camera_capture_error_is_deferred(
    error: &AlignmentSummaryGpuCameraCaptureError,
) -> bool {
    alignment_summary_camera_capture_blocker(error).is_some()
}

pub(crate) fn alignment_scene_staging_frame_is_current(
    desired_token: Option<AlignmentSceneRequestToken>,
    staged_token: Option<AlignmentSceneRequestToken>,
    desired_camera: Option<AlignmentSceneCamera>,
    staged_camera: AlignmentSceneCamera,
    captured_camera: AlignmentSceneCamera,
) -> bool {
    staged_token.is_some_and(|token| {
        crate::workspace::queries::alignment_scene_frame_is_current(
            desired_token,
            token,
            desired_camera,
            staged_camera,
        ) && staged_camera == captured_camera
    })
}

/// Revalidates immutable semantic content independently of the worker camera that most recently
/// published it. This is what permits held-pan camera capture from a resident horizon without
/// treating that old worker frame's row profile or status as current for the new request.
pub(crate) fn alignment_scene_snapshot_satisfies_contract(
    snapshot: &AlignmentSceneSnapshot,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    let reads = snapshot.reads();
    let evidence = reads.evidence();
    let atlas = reads.atlas();
    let summaries = snapshot.summaries();
    evidence.source_revision() == contract.source_revision
        && evidence.record_contract() == contract.record_contract
        && evidence.payload() == &contract.payload
        && atlas.source_revision() == contract.source_revision
        && atlas.record_contract() == contract.record_contract
        && atlas.layout_contract() == contract.layout
        && alignment_presentation_core_matches(reads.presentation(), contract.presentation)
        && summaries.source_revision() == contract.source_revision
        && summaries.record_contract() == contract.record_contract
        && summaries.contract() == contract.summaries
}

pub(super) fn alignment_presentation_core_matches(
    mut actual: PresentationContract,
    mut desired: PresentationContract,
) -> bool {
    actual.reference = None;
    desired.reference = None;
    actual == desired
}

#[derive(Clone)]
pub(super) struct OwnedAlignmentGpuRequest {
    pub(super) panel_id: PanelId,
    pub(super) track_id: TrackId,
    pub(super) scene: Arc<AlignmentPresentationScene>,
    pub(super) camera_sequence: u64,
    pub(super) camera: AlignmentCamera,
    pub(super) scissor: AlignmentGpuScissor,
    pub(super) interaction: AlignmentInteractionState,
    pub(super) live_clip: Option<CpuClipRect>,
}

impl OwnedAlignmentGpuRequest {
    pub(super) fn borrowed(&self, screen_size_points: [f32; 2]) -> AlignmentGpuSceneRequest<'_> {
        AlignmentGpuSceneRequest {
            scene: &self.scene,
            camera_sequence: self.camera_sequence,
            camera: self.camera,
            screen_size_points,
            scissor: self.scissor,
            interaction: &self.interaction,
        }
    }
}

#[derive(Clone)]
pub(super) struct OwnedAlignmentSummaryGpuRequest {
    pub(super) panel_id: PanelId,
    pub(super) track_id: TrackId,
    pub(super) scene: Arc<AlignmentSceneSnapshot>,
    pub(super) camera: CpuSummaryCamera,
    pub(super) scissor: AlignmentGpuScissor,
    pub(super) options: AlignmentSummaryGpuLaneOptions,
    pub(super) overlays: AlignmentSummaryFrameOverlays,
    pub(super) junction_presentation: Option<Arc<rgv_model::SashimiPresentationTable>>,
}

impl OwnedAlignmentSummaryGpuRequest {
    pub(super) fn borrowed(
        &self,
        camera_sequence: u64,
        screen_size_points: [f32; 2],
    ) -> AlignmentSummaryGpuSceneRequest<'_> {
        AlignmentSummaryGpuSceneRequest {
            scene: &self.scene,
            coherence_scope: self.panel_id.get(),
            lane_identity: self.track_id.get(),
            camera_sequence,
            camera: self.camera,
            screen_size_points,
            scissor: self.scissor,
            options: self.options.clone(),
            overlays: self.overlays,
            junction_presentation: self.junction_presentation.clone(),
        }
    }
}

/// Move-only authority for every old allocation credited to one read-plus-summary replacement.
/// The combined claim cannot outlive either child renderer token.
pub(super) struct AlignmentGpuCompositeRetirement {
    pub(super) claim_set: AllocationRetirementClaimSet,
    pub(super) semantic: Option<AlignmentSemanticEpochRetirement>,
    pub(super) reads: Option<AlignmentGpuRetirement>,
    pub(super) summaries: Option<AlignmentSummaryGpuRetirement>,
}

impl AllocationRetirementToken for AlignmentGpuCompositeRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

/// A partially resident read presentation that may be completed later ("draw what fits").
///
/// Under the bounded memory policy a visible page set larger than the GPU share is published
/// with its most central pages and the rest deferred. This records what that publication left
/// out so the event loop can try again when it could plausibly do better, and only then.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AlignmentGpuPartialTopUp {
    pub(crate) target_revision: u64,
    /// Visible pages the presented frame does not draw.
    pub(crate) deferred_pages: usize,
    /// GPU ledger occupancy (charged plus reserved) right after the publication.
    pub(crate) occupied_bytes: usize,
    /// Consecutive top-up attempts for this target that admitted nothing new.
    pub(crate) fruitless_attempts: u32,
}

/// The page allowance for the next, smaller candidate after the ledger refused one that planned
/// `planned_page_bytes` of new read pages, or `None` when no read page is left to shed and the
/// refusal therefore cannot be answered by drawing less.
pub(crate) fn alignment_gpu_degraded_read_page_allowance(
    planned_page_bytes: usize,
    pressure: GpuBudgetPressure,
) -> Option<usize> {
    (planned_page_bytes > 0)
        .then(|| planned_page_bytes.saturating_sub(pressure.required_reclaim_bytes().max(1)))
}

/// The top-up record to keep after publishing a frame with `deferred_pages` visible pages left
/// out; `None` once the presentation is complete.
pub(crate) fn alignment_gpu_partial_top_up_after_publication(
    previous: Option<AlignmentGpuPartialTopUp>,
    target_revision: u64,
    deferred_pages: usize,
    occupied_bytes: usize,
) -> Option<AlignmentGpuPartialTopUp> {
    if deferred_pages == 0 {
        return None;
    }
    // Publishing retires the previous epoch's pages, so the first attempt after any
    // publication that made progress is always worth making. One that admitted nothing new is
    // counted, and the record then waits for capacity to be released.
    let fruitless_attempts = previous
        .filter(|previous| {
            previous.target_revision == target_revision && deferred_pages >= previous.deferred_pages
        })
        .map_or(0, |previous| previous.fruitless_attempts.saturating_add(1));
    Some(AlignmentGpuPartialTopUp {
        target_revision,
        deferred_pages,
        occupied_bytes,
        fruitless_attempts,
    })
}

/// Whether another attempt to admit the deferred pages should run now: once straight after a
/// publication that made progress, and afterwards only when GPU occupancy has fallen below what
/// that publication left, which is the only way a retry can end differently. This keeps a
/// presentation that simply does not fit from re-planning on every frame.
pub(crate) fn alignment_gpu_partial_top_up_is_due(
    top_up: AlignmentGpuPartialTopUp,
    desired_target_revision: u64,
    occupied_bytes: usize,
) -> bool {
    top_up.target_revision == desired_target_revision
        && (top_up.fruitless_attempts == 0 || occupied_bytes < top_up.occupied_bytes)
}

/// What a read lane says while its picture is only partially resident: the same "updating"
/// wording used for other pending lane state, never a claim of completeness. `None` when every
/// visible page is drawn.
pub(crate) fn alignment_partial_residency_status(deferred_pages: usize) -> Option<String> {
    (deferred_pages > 0).then(|| {
        format!(
            "Updating: {deferred_pages} visible page{} waiting for GPU memory",
            if deferred_pages == 1 { "" } else { "s" }
        )
    })
}

pub(super) struct AlignmentGpuBundleFailure {
    pub(super) message: String,
    pub(super) pressure: Option<GpuBudgetPressure>,
    pub(super) blocker: Option<AlignmentGpuPublicationBlocker>,
}

impl AlignmentGpuBundleFailure {
    pub(super) fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            pressure: None,
            blocker: None,
        }
    }

    pub(super) fn bundle(error: AllocationBundleError) -> Self {
        Self {
            pressure: alignment_gpu_bundle_budget_pressure(&error),
            blocker: alignment_gpu_bundle_blocker(&error),
            message: error.to_string(),
        }
    }

    pub(super) fn read_replacement(error: AlignmentGpuReplacementError) -> Self {
        Self {
            pressure: None,
            blocker: alignment_read_replacement_blocker(&error),
            message: error.to_string(),
        }
    }

    pub(super) fn summary_replacement(error: AlignmentSummaryGpuReplacementError) -> Self {
        Self {
            pressure: None,
            blocker: alignment_summary_replacement_blocker(&error),
            message: error.to_string(),
        }
    }
}

pub(super) fn alignment_gpu_bundle_blocker(
    error: &AllocationBundleError,
) -> Option<AlignmentGpuPublicationBlocker> {
    if let Some(pressure) = alignment_gpu_bundle_budget_pressure(error) {
        return Some(AlignmentGpuPublicationBlocker::AggregateGpu(pressure));
    }
    match error {
        AllocationBundleError::ReplacementTransitionActive { bundle_id } => Some(
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
                bundle_id: *bundle_id,
            }),
        ),
        AllocationBundleError::PolicyRevisionChanged { planned, current } => {
            Some(AlignmentGpuPublicationBlocker::PolicyRevisionChanged {
                planned: *planned,
                current: *current,
            })
        }
        AllocationBundleError::Ledger(error) => alignment_gpu_publication_ledger_blocker(*error),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn alignment_gpu_bundle_error_is_deferred(error: &AllocationBundleError) -> bool {
    alignment_gpu_bundle_blocker(error).is_some()
}

pub(super) fn alignment_read_replacement_blocker(
    error: &AlignmentGpuReplacementError,
) -> Option<AlignmentGpuPublicationBlocker> {
    let ledger = match error {
        AlignmentGpuReplacementError::Ledger(error) => Some(*error),
        AlignmentGpuReplacementError::Bundle(error) => return alignment_gpu_bundle_blocker(error),
        AlignmentGpuReplacementError::Admission(AlignmentGpuPageAdmissionError::Ledger(error)) => {
            Some(*error)
        }
        AlignmentGpuReplacementError::Prepare(error) => alignment_read_prepare_ledger_error(error),
        _ => None,
    };
    ledger.and_then(alignment_gpu_publication_ledger_blocker)
}

pub(super) fn alignment_summary_replacement_blocker(
    error: &AlignmentSummaryGpuReplacementError,
) -> Option<AlignmentGpuPublicationBlocker> {
    let ledger = match error {
        AlignmentSummaryGpuReplacementError::Ledger(error) => Some(*error),
        AlignmentSummaryGpuReplacementError::Bundle(error) => {
            return alignment_gpu_bundle_blocker(error);
        }
        AlignmentSummaryGpuReplacementError::Admission(
            AlignmentSummaryGpuPageAdmissionError::Ledger(error),
        ) => Some(*error),
        AlignmentSummaryGpuReplacementError::Prepare(error) => {
            alignment_summary_prepare_ledger_error(error)
        }
        _ => None,
    };
    ledger.and_then(alignment_gpu_publication_ledger_blocker)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlignmentGpuPublicationRetryPoll {
    Superseded,
    Waiting(AlignmentGpuPublicationRetry),
    Ready,
}

/// Tests whether the exact refusal still exists in the first snapshot taken after it was
/// returned. A false result proves that ledger progress raced with error delivery, so waiting for
/// a second epoch change would deadlock the latest target.
pub(super) fn alignment_gpu_publication_blocker_remains(
    blocker: AlignmentGpuPublicationBlocker,
    snapshot: &impl AlignmentLedgerProgressView,
) -> bool {
    match blocker {
        AlignmentGpuPublicationBlocker::Ledger(pressure) => {
            alignment_ledger_pressure_blocker_remains(pressure, snapshot)
        }
        AlignmentGpuPublicationBlocker::AggregateGpu(pressure) => {
            alignment_gpu_budget_pressure_remains(pressure, snapshot)
        }
        AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. } => false,
    }
}

/// Re-evaluates one aggregate GPU refusal without losing its replacement accounting model.
/// `retirement_credit_bytes` is subtracted only for the steady-state test; transition-overlap and
/// ordinary retained pressure intentionally use physical occupied bytes.
pub(super) fn alignment_gpu_budget_pressure_remains(
    pressure: GpuBudgetPressure,
    snapshot: &impl AlignmentLedgerProgressView,
) -> bool {
    let snapshot = snapshot.alignment_ledger_progress();
    let expected_limit = match pressure.cause {
        rgv_render::GpuBudgetPressureCause::RetainedBudget => {
            pressure.limit_bytes == snapshot.budget
                || pressure.limit_bytes
                    == snapshot.budget.saturating_add(snapshot.transition_reserve)
        }
        rgv_render::GpuBudgetPressureCause::SteadyReplacement => {
            pressure.limit_bytes == snapshot.budget
        }
        rgv_render::GpuBudgetPressureCause::TransitionOverlap => {
            pressure.limit_bytes == snapshot.budget.saturating_add(snapshot.transition_reserve)
        }
    };
    if !expected_limit {
        return false;
    }
    let occupied = snapshot.charged.saturating_add(snapshot.reserved);
    let effective_occupied = match pressure.cause {
        rgv_render::GpuBudgetPressureCause::SteadyReplacement => {
            occupied.saturating_sub(pressure.retirement_credit_bytes)
        }
        rgv_render::GpuBudgetPressureCause::RetainedBudget
        | rgv_render::GpuBudgetPressureCause::TransitionOverlap => occupied,
    };
    effective_occupied.saturating_add(pressure.requested_bytes) > pressure.limit_bytes
}

pub(super) fn alignment_gpu_pressure_target_charged(
    pressure: GpuBudgetPressure,
    snapshot: &impl AlignmentLedgerProgressView,
) -> usize {
    let snapshot = snapshot.alignment_ledger_progress();
    let retirement_credit = match pressure.cause {
        rgv_render::GpuBudgetPressureCause::SteadyReplacement => pressure.retirement_credit_bytes,
        rgv_render::GpuBudgetPressureCause::RetainedBudget
        | rgv_render::GpuBudgetPressureCause::TransitionOverlap => 0,
    };
    let exact_admission_threshold = pressure
        .limit_bytes
        .saturating_sub(pressure.requested_bytes)
        .saturating_add(retirement_credit)
        .saturating_sub(snapshot.reserved);
    // Reclaim to a low-water mark, not the single byte at which this one failed bundle happens to
    // fit. The extra reserve is exactly one measured foreground bundle (the current refusal), so
    // it adapts to sparse and dense loci without an ACTB-specific byte constant. Pinned committed
    // authority remains the hard floor.
    exact_admission_threshold
        .saturating_sub(pressure.requested_bytes)
        .max(snapshot.pinned)
}

pub(super) fn alignment_gpu_publication_retry_gate(
    blocker: AlignmentGpuPublicationBlocker,
    snapshot: &impl AlignmentLedgerProgressView,
) -> AlignmentGpuPublicationRetryGate {
    let snapshot = snapshot.alignment_ledger_progress();
    if alignment_gpu_publication_blocker_remains(blocker, &snapshot) {
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(snapshot.progress_epoch())
    } else {
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
    }
}

/// Cache-owner progress is meaningful even when old presentation pins keep the ledger's charged
/// byte total unchanged: relinquishing the final outside `Arc` can enlarge the exact retirement
/// set on the next atomic attempt. This permits one correlated retry; a second reclaim cycle with
/// neither byte nor ownership progress settles `BudgetLimited` instead of spinning.
pub(super) fn alignment_capacity_reclaim_allows_retry(
    blocker: AlignmentGpuPublicationBlocker,
    snapshot: &impl AlignmentLedgerProgressView,
    relinquished_cache_entries: usize,
) -> bool {
    relinquished_cache_entries > 0 || !alignment_gpu_publication_blocker_remains(blocker, snapshot)
}

pub(super) fn alignment_gpu_publication_retry_poll(
    mut retry: AlignmentGpuPublicationRetry,
    desired_target_revision: u64,
    attempt_sequence: u64,
    snapshot: &impl AlignmentLedgerProgressView,
) -> AlignmentGpuPublicationRetryPoll {
    let snapshot = snapshot.alignment_ledger_progress();
    if retry.target_revision != desired_target_revision
        || retry.attempt_sequence != attempt_sequence
    {
        return AlignmentGpuPublicationRetryPoll::Superseded;
    }
    match retry.gate {
        AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress => {
            AlignmentGpuPublicationRetryPoll::Ready
        }
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(refused_epoch)
            if refused_epoch != snapshot.progress_epoch() =>
        {
            if alignment_gpu_publication_blocker_remains(retry.blocker, &snapshot) {
                // The ledger changed, but not in a way which removed this exact refusal. Advance
                // the observed epoch so the same unrelated churn cannot be reconsidered on every
                // poll, and keep waiting without opening another GPU transaction.
                retry.gate = AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(
                    snapshot.progress_epoch(),
                );
                AlignmentGpuPublicationRetryPoll::Waiting(retry)
            } else {
                AlignmentGpuPublicationRetryPoll::Ready
            }
        }
        AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(_) => {
            AlignmentGpuPublicationRetryPoll::Waiting(retry)
        }
    }
}

/// Starts a publication attempt only when it cannot bypass an unchanged progress gate. The
/// pending retry owns the current desired target's attempt sequence while it waits: ordinary
/// hover, selection, and same-camera refreshes therefore coalesce behind that authority instead
/// of manufacturing a newer attempt against the same obstruction. A different target is still
/// latest-wins, while observed ledger progress consumes the retry and permits one new attempt.
pub(super) fn begin_alignment_gpu_publication_attempt(
    authority: &mut AlignmentScenePublicationAuthority,
    retry: &mut Option<AlignmentGpuPublicationRetry>,
    target: &AlignmentSceneTargetIdentity,
    snapshot: &impl AlignmentLedgerProgressView,
) -> Option<AlignmentScenePublicationAttempt> {
    if let Some(pending) = *retry {
        if authority.desired_target_matches(target) {
            match alignment_gpu_publication_retry_poll(
                pending,
                authority.desired_target_revision(),
                authority.attempt_sequence(),
                snapshot,
            ) {
                AlignmentGpuPublicationRetryPoll::Waiting(updated) => {
                    *retry = Some(updated);
                    return None;
                }
                AlignmentGpuPublicationRetryPoll::Ready
                | AlignmentGpuPublicationRetryPoll::Superseded => {
                    *retry = None;
                }
            }
        } else {
            // A camera/layout change supersedes the old refusal even if its ledger epoch did not
            // advance. Its new desired revision immediately stale-gates the prior presentation.
            *retry = None;
        }
    }
    Some(authority.begin_attempt(target))
}

pub(super) fn alignment_gpu_bundle_budget_pressure(
    error: &AllocationBundleError,
) -> Option<GpuBudgetPressure> {
    match *error {
        AllocationBundleError::SteadyBudgetExceeded {
            candidate,
            occupied,
            retiring,
            limit,
        } => Some(GpuBudgetPressure::from_steady_replacement_failure(
            candidate, occupied, retiring, limit,
        )),
        AllocationBundleError::TransitionOverlapBudgetExceeded {
            candidate,
            occupied,
            limit,
        } => Some(GpuBudgetPressure::from_transition_overlap_failure(
            candidate, occupied, limit,
        )),
        _ => None,
    }
}

pub(super) const fn alignment_gpu_budget_pressure_blocker(
    pressure: GpuBudgetPressure,
) -> AlignmentGpuPublicationBlocker {
    AlignmentGpuPublicationBlocker::AggregateGpu(pressure)
}

/// Translates retained shared-ledger pressure into the exact headroom goal understood by both GPU
/// cache owners. Transient decoder/upload capacity and an active replacement gate are released by
/// their owning checkpoint, not by evicting retained pages.
pub(super) fn alignment_source_pressure_gpu_budget(
    pressure: LedgerError,
) -> Option<GpuBudgetPressure> {
    match pressure {
        LedgerError::BudgetExceeded {
            requested,
            charged,
            reserved,
            limit,
        } if !pressure.intrinsically_exceeds_limit() => Some(
            GpuBudgetPressure::from_budget_failure(requested, charged, reserved, limit),
        ),
        LedgerError::ReplacementTransitionActive { .. }
        | LedgerError::TransientBudgetExceeded { .. }
        | LedgerError::BudgetExceeded { .. }
        | LedgerError::ReservationExceeded { .. }
        | LedgerError::UnknownAllocation(_)
        | LedgerError::IdOverflow
        | LedgerError::Poisoned => None,
    }
}

pub(super) fn settle_alignment_gpu_bundle(
    commit: AllocationBundleCommit,
    composite: AlignmentGpuCompositeRetirement,
) -> std::result::Result<
    Vec<AllocationBundleLease>,
    (AllocationBundleError, AlignmentGpuCompositeRetirement),
> {
    let mut token = Some(composite);
    // `retire_and_settle` delegates to the prepared-settlement boundary: every `Err` occurs
    // before token consumption, while the successful destructive suffix is infallible.
    let result = match commit.retire_and_settle(&mut token) {
        Ok(leases) => Ok(leases),
        Err(error) => Err((
            error,
            token.expect("settlement preflight errors preserve the retirement token"),
        )),
    };
    result
}

/// The result of one shared-ledger foreground reclaim attempt. A boolean cannot distinguish an
/// intrinsically oversized candidate from capacity which exists but is still pinned/protected;
/// callers must retry only the satisfied case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AlignmentGpuReclaimOutcome {
    Satisfied {
        required_bytes: usize,
        released_bytes: usize,
        retired_read_residencies: usize,
        retired_summary_residencies: usize,
    },
    Infeasible {
        required_bytes: usize,
        baseline_occupied_bytes: usize,
    },
    Insufficient {
        required_bytes: usize,
        released_bytes: usize,
        remaining_bytes: usize,
        pinned_bytes: usize,
    },
}

impl AlignmentGpuReclaimOutcome {
    pub(super) fn for_goal(
        goal: GpuReclaimGoal,
        occupied_bytes: usize,
        pinned_bytes: usize,
        retired_read_residencies: usize,
        retired_summary_residencies: usize,
    ) -> Self {
        let required_bytes = goal.required_reclaim_bytes();
        if !goal.is_feasible() {
            return Self::Infeasible {
                required_bytes,
                baseline_occupied_bytes: goal.baseline_occupied_bytes,
            };
        }
        let released_bytes = goal.released_bytes_at(occupied_bytes);
        if goal.is_satisfied_at(occupied_bytes) {
            Self::Satisfied {
                required_bytes,
                released_bytes,
                retired_read_residencies,
                retired_summary_residencies,
            }
        } else {
            Self::Insufficient {
                required_bytes,
                released_bytes,
                remaining_bytes: required_bytes.saturating_sub(released_bytes),
                pinned_bytes,
            }
        }
    }
}

#[cfg(test)]
mod partial_residency_tests {
    use super::*;

    #[test]
    fn a_partially_resident_lane_reports_updating_and_never_claims_completeness() {
        assert_eq!(alignment_partial_residency_status(0), None);
        assert_eq!(
            alignment_partial_residency_status(1).as_deref(),
            Some("Updating: 1 visible page waiting for GPU memory")
        );
        let several = alignment_partial_residency_status(12).unwrap();
        assert_eq!(several, "Updating: 12 visible pages waiting for GPU memory");
        // The same pending wording the lane title uses for filters and highlights.
        assert!(several.starts_with("Updating"));
        assert!(!several.to_lowercase().contains("complete"));
    }

    #[test]
    fn each_refusal_sheds_at_least_its_shortfall_until_no_read_page_is_left() {
        // 1,000 bytes over a 4,000-byte limit while planning 3,000 bytes of new read pages.
        let pressure = GpuBudgetPressure::from_steady_replacement_failure(3_500, 1_500, 0, 4_000);
        assert_eq!(pressure.required_reclaim_bytes(), 1_000);
        assert_eq!(
            alignment_gpu_degraded_read_page_allowance(3_000, pressure),
            Some(2_000)
        );
        // A shortfall larger than everything planned leaves an allowance of zero: the next
        // candidate draws only what is already resident.
        assert_eq!(
            alignment_gpu_degraded_read_page_allowance(600, pressure),
            Some(0)
        );
        // A refusal that reports no shortfall still makes progress.
        let exact = GpuBudgetPressure::from_steady_replacement_failure(1_000, 3_000, 0, 4_000);
        assert_eq!(exact.required_reclaim_bytes(), 0);
        assert_eq!(
            alignment_gpu_degraded_read_page_allowance(500, exact),
            Some(499)
        );
        // With no new read page in the candidate, drawing less cannot help.
        assert_eq!(
            alignment_gpu_degraded_read_page_allowance(0, pressure),
            None
        );
    }

    #[test]
    fn top_up_runs_after_progress_and_then_waits_for_released_capacity() {
        // A complete publication keeps no record.
        assert_eq!(
            alignment_gpu_partial_top_up_after_publication(None, 7, 0, 1_000),
            None
        );
        // The first partial publication is retried at once: publishing retired the old epoch.
        let first = alignment_gpu_partial_top_up_after_publication(None, 7, 5, 1_000).unwrap();
        assert_eq!(first.fruitless_attempts, 0);
        assert!(alignment_gpu_partial_top_up_is_due(first, 7, 1_000));
        // Not for another target.
        assert!(!alignment_gpu_partial_top_up_is_due(first, 8, 0));

        // The retry admitted two more pages: progress, so one more immediate attempt.
        let progressed =
            alignment_gpu_partial_top_up_after_publication(Some(first), 7, 3, 1_400).unwrap();
        assert_eq!(progressed.fruitless_attempts, 0);
        assert!(alignment_gpu_partial_top_up_is_due(progressed, 7, 1_400));

        // The next one admitted nothing: stop until occupancy falls below what it left.
        let stalled =
            alignment_gpu_partial_top_up_after_publication(Some(progressed), 7, 3, 1_400).unwrap();
        assert_eq!(stalled.fruitless_attempts, 1);
        assert!(!alignment_gpu_partial_top_up_is_due(stalled, 7, 1_400));
        assert!(!alignment_gpu_partial_top_up_is_due(stalled, 7, 1_900));
        assert!(alignment_gpu_partial_top_up_is_due(stalled, 7, 1_399));

        // Completing the presentation clears the record; a new target starts afresh.
        assert_eq!(
            alignment_gpu_partial_top_up_after_publication(Some(stalled), 7, 0, 1_800),
            None
        );
        let retargeted =
            alignment_gpu_partial_top_up_after_publication(Some(stalled), 8, 4, 1_400).unwrap();
        assert_eq!(retargeted.fruitless_attempts, 0);
    }
}
