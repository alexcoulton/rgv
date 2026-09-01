use super::super::*;

// Optional work begins at one cancellable page and adapts from measured wall time. This is a
// scheduler target, not a correctness timeout or residency cap.
const ALIGNMENT_GPU_PREFETCH_TARGET_QUANTUM: Duration = Duration::from_millis(2);
const ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM: usize = 64;

fn retire_reference_install_receipt_for_contract_epoch(
    receipts: &mut BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
    owner: SourceOwnerId,
) -> bool {
    receipts.remove(&owner).is_some()
}

impl AlignmentGpuPrefetchState {
    fn maximum_pages(&self) -> NonZeroUsize {
        NonZeroUsize::new(
            self.page_quantum
                .clamp(1, ALIGNMENT_GPU_PREFETCH_MAXIMUM_PAGES_PER_QUANTUM),
        )
        .expect("the adaptive prefetch quantum is non-zero")
    }

    fn observe_quantum(&mut self, elapsed: Duration, processed_pages: usize) {
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
struct CapturedAlignmentLaneSource {
    panel_id: PanelId,
    track_id: TrackId,
    semantic_request_token: AlignmentSceneRequestToken,
    desired_request_token: AlignmentSceneRequestToken,
    camera: AlignmentSceneCamera,
    individual_reads: AlignmentIndividualReadCapability,
    semantic_target_complete: bool,
    reference_ready: bool,
    vertical_residency: AlignmentVerticalResidency,
    camera_sequence: u64,
    snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    scene_contracts_current: bool,
}

struct CapturedAlignmentPanelSceneEpoch {
    panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
    lane_sources: Vec<CapturedAlignmentLaneSource>,
    panel_scene_stores: HashMap<PanelId, AlignmentPanelSceneStore>,
}

struct AlignmentPanelSceneEpochCaptureFailure {
    message: String,
    fields: ManualDiagnosticTraceFields,
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
struct AlignmentSemanticEpochRetirement {
    claim_set: AllocationRetirementClaimSet,
    stats: AlignmentSemanticRetirementStats,
    panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
    panel_scene_stores: HashMap<PanelId, AlignmentPanelSceneStore>,
}

impl AllocationRetirementToken for AlignmentSemanticEpochRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

enum AlignmentGpuCameraCaptureOutcome {
    Captured {
        reads: Option<AlignmentGpuFrame>,
        summaries: Option<AlignmentSummaryGpuFrame>,
    },
    ContentReplacementRequired,
    Deferred(AlignmentGpuPublicationBlocker),
    Terminal(String),
}

const fn alignment_gpu_publication_ledger_blocker(
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

const fn alignment_read_prepare_ledger_error(
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

const fn alignment_summary_prepare_ledger_error(
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

fn alignment_read_camera_capture_blocker(
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

fn alignment_summary_camera_capture_blocker(
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
        super::queries::alignment_scene_frame_is_current(
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

fn alignment_presentation_core_matches(
    mut actual: PresentationContract,
    mut desired: PresentationContract,
) -> bool {
    actual.reference = None;
    desired.reference = None;
    actual == desired
}

#[derive(Clone)]
struct OwnedAlignmentGpuRequest {
    panel_id: PanelId,
    track_id: TrackId,
    scene: Arc<AlignmentPresentationScene>,
    camera_sequence: u64,
    camera: AlignmentCamera,
    scissor: AlignmentGpuScissor,
    interaction: AlignmentInteractionState,
    live_clip: Option<CpuClipRect>,
}

impl OwnedAlignmentGpuRequest {
    fn borrowed(&self, screen_size_points: [f32; 2]) -> AlignmentGpuSceneRequest<'_> {
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
struct OwnedAlignmentSummaryGpuRequest {
    panel_id: PanelId,
    track_id: TrackId,
    scene: Arc<AlignmentSceneSnapshot>,
    camera: CpuSummaryCamera,
    scissor: AlignmentGpuScissor,
    options: AlignmentSummaryGpuLaneOptions,
    overlays: AlignmentSummaryFrameOverlays,
}

impl OwnedAlignmentSummaryGpuRequest {
    fn borrowed(
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
        }
    }
}

/// Move-only authority for every old allocation credited to one read-plus-summary replacement.
/// The combined claim cannot outlive either child renderer token.
struct AlignmentGpuCompositeRetirement {
    claim_set: AllocationRetirementClaimSet,
    semantic: Option<AlignmentSemanticEpochRetirement>,
    reads: Option<AlignmentGpuRetirement>,
    summaries: Option<AlignmentSummaryGpuRetirement>,
}

impl AllocationRetirementToken for AlignmentGpuCompositeRetirement {
    fn retirement_claim_set(&self) -> &AllocationRetirementClaimSet {
        &self.claim_set
    }
}

struct AlignmentGpuBundleFailure {
    message: String,
    pressure: Option<GpuBudgetPressure>,
    blocker: Option<AlignmentGpuPublicationBlocker>,
}

impl AlignmentGpuBundleFailure {
    fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            pressure: None,
            blocker: None,
        }
    }

    fn bundle(error: AllocationBundleError) -> Self {
        Self {
            pressure: alignment_gpu_bundle_budget_pressure(&error),
            blocker: alignment_gpu_bundle_blocker(&error),
            message: error.to_string(),
        }
    }

    fn read_replacement(error: AlignmentGpuReplacementError) -> Self {
        Self {
            pressure: None,
            blocker: alignment_read_replacement_blocker(&error),
            message: error.to_string(),
        }
    }

    fn summary_replacement(error: AlignmentSummaryGpuReplacementError) -> Self {
        Self {
            pressure: None,
            blocker: alignment_summary_replacement_blocker(&error),
            message: error.to_string(),
        }
    }
}

fn alignment_gpu_bundle_blocker(
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

fn alignment_read_replacement_blocker(
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

fn alignment_summary_replacement_blocker(
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
enum AlignmentGpuPublicationRetryPoll {
    Superseded,
    Waiting(AlignmentGpuPublicationRetry),
    Ready,
}

/// Tests whether the exact refusal still exists in the first snapshot taken after it was
/// returned. A false result proves that ledger progress raced with error delivery, so waiting for
/// a second epoch change would deadlock the latest target.
fn alignment_gpu_publication_blocker_remains(
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
fn alignment_gpu_budget_pressure_remains(
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

fn alignment_gpu_pressure_target_charged(
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

fn alignment_gpu_publication_retry_gate(
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
fn alignment_capacity_reclaim_allows_retry(
    blocker: AlignmentGpuPublicationBlocker,
    snapshot: &impl AlignmentLedgerProgressView,
    relinquished_cache_entries: usize,
) -> bool {
    relinquished_cache_entries > 0 || !alignment_gpu_publication_blocker_remains(blocker, snapshot)
}

fn alignment_gpu_publication_retry_poll(
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
fn begin_alignment_gpu_publication_attempt(
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

fn alignment_gpu_bundle_budget_pressure(
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

const fn alignment_gpu_budget_pressure_blocker(
    pressure: GpuBudgetPressure,
) -> AlignmentGpuPublicationBlocker {
    AlignmentGpuPublicationBlocker::AggregateGpu(pressure)
}

/// Translates retained shared-ledger pressure into the exact headroom goal understood by both GPU
/// cache owners. Transient decoder/upload capacity and an active replacement gate are released by
/// their owning checkpoint, not by evicting retained pages.
fn alignment_source_pressure_gpu_budget(pressure: LedgerError) -> Option<GpuBudgetPressure> {
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

fn settle_alignment_gpu_bundle(
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
enum AlignmentGpuReclaimOutcome {
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
    fn for_goal(
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

impl AlignmentGpuSpeculationGate {
    fn observe_foreground(
        &mut self,
        authority: AlignmentGpuPreparedAuthority,
        budget_bytes: usize,
    ) {
        let authority_changed = self.authority.as_ref() != Some(&authority);
        let budget_increased = budget_bytes > self.budget_bytes;
        let budget_decreased = self.budget_bytes > 0 && budget_bytes < self.budget_bytes;
        self.authority = Some(authority);
        self.budget_bytes = budget_bytes;
        if budget_decreased {
            self.phase = AlignmentGpuSpeculationPhase::Suppressed;
        } else if authority_changed || budget_increased {
            self.phase = AlignmentGpuSpeculationPhase::Eligible;
        }
    }

    const fn allows_prefetch(&self) -> bool {
        matches!(
            self.phase,
            AlignmentGpuSpeculationPhase::Eligible | AlignmentGpuSpeculationPhase::Warming
        )
    }

    const fn suppress_for_foreground_pressure(&mut self) {
        self.phase = AlignmentGpuSpeculationPhase::Suppressed;
    }

    const fn finish_cycle(&mut self, missing_pages_remain: bool, retention_limited: bool) {
        if matches!(self.phase, AlignmentGpuSpeculationPhase::Suppressed) {
            return;
        }
        self.phase = if missing_pages_remain {
            AlignmentGpuSpeculationPhase::Warming
        } else if retention_limited {
            AlignmentGpuSpeculationPhase::DemandPaged
        } else {
            AlignmentGpuSpeculationPhase::Resident
        };
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod alignment_gpu_policy_tests {
    use std::{sync::Arc, time::Duration};

    use rgv_alignment_scene::{
        AlignmentPageFamily, AlignmentPageKey, AlignmentSourceRevision, CanonicalGenomicTile,
        CanonicalRowBlock, ContractFingerprint, EvidenceRevision, GeometryLod, RevisionFingerprint,
        RowAtlasRevision, SourceFingerprint,
    };
    use rgv_core::ContigId;
    use rgv_render::{GpuBudgetPressure, GpuReclaimGoal};

    use super::{
        AlignmentGpuPrefetchState, AlignmentGpuPreparedAuthority, AlignmentGpuReclaimOutcome,
        AlignmentGpuSpeculationGate, AlignmentGpuSpeculationPhase,
        alignment_gpu_bundle_budget_pressure,
    };
    use rgv_alignment_scene::AllocationBundleError;

    fn authority(tile: u64) -> AlignmentGpuPreparedAuthority {
        let source = AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        );
        let key = AlignmentPageKey::new(
            source,
            EvidenceRevision::new(RevisionFingerprint::new(5, 0)),
            RowAtlasRevision::new(RevisionFingerprint::new(6, 0)),
            ContractFingerprint::new(7),
            ContractFingerprint::new(8),
            CanonicalGenomicTile::new(
                ContigId::new(0),
                tile,
                std::num::NonZeroU64::new(100).unwrap(),
            ),
            CanonicalRowBlock::new(0, std::num::NonZeroU32::new(64).unwrap()),
            AlignmentPageFamily::ReadGeometry,
            GeometryLod::Read,
        );
        AlignmentGpuPreparedAuthority {
            read_pages: Arc::from([key]),
            summary_pages: Arc::from([]),
        }
    }

    #[test]
    fn pressure_suppression_converges_until_authority_or_budget_increase_changes() {
        let mut gate = AlignmentGpuSpeculationGate::default();
        gate.observe_foreground(authority(0), 1_000);
        assert!(gate.allows_prefetch());
        gate.suppress_for_foreground_pressure();
        for _ in 0..10 {
            gate.observe_foreground(authority(0), 1_000);
            assert!(!gate.allows_prefetch());
            assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Suppressed);
        }

        gate.observe_foreground(authority(1), 1_000);
        assert!(gate.allows_prefetch());
        gate.suppress_for_foreground_pressure();
        gate.observe_foreground(authority(1), 2_000);
        assert!(gate.allows_prefetch());
    }

    #[test]
    fn lowering_budget_suppresses_instead_of_rewarming() {
        let mut gate = AlignmentGpuSpeculationGate::default();
        gate.observe_foreground(authority(0), 2_000);
        gate.finish_cycle(true, false);
        assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Warming);
        gate.observe_foreground(authority(0), 1_000);
        assert_eq!(gate.phase, AlignmentGpuSpeculationPhase::Suppressed);
        assert!(!gate.allows_prefetch());
    }

    #[test]
    fn reclaim_outcome_distinguishes_retry_infeasible_and_pinned_shortfall() {
        let pressure = GpuBudgetPressure::from_budget_failure(200, 900, 0, 1_000);
        let goal = GpuReclaimGoal::new(pressure, 900);
        assert_eq!(
            AlignmentGpuReclaimOutcome::for_goal(goal, 800, 500, 1, 2),
            AlignmentGpuReclaimOutcome::Satisfied {
                required_bytes: 100,
                released_bytes: 100,
                retired_read_residencies: 1,
                retired_summary_residencies: 2,
            }
        );
        assert_eq!(
            AlignmentGpuReclaimOutcome::for_goal(goal, 850, 700, 1, 2),
            AlignmentGpuReclaimOutcome::Insufficient {
                required_bytes: 100,
                released_bytes: 50,
                remaining_bytes: 50,
                pinned_bytes: 700,
            }
        );

        let oversized = GpuReclaimGoal::new(
            GpuBudgetPressure::from_budget_failure(1_100, 0, 0, 1_000),
            0,
        );
        assert_eq!(
            AlignmentGpuReclaimOutcome::for_goal(oversized, 0, 0, 0, 0),
            AlignmentGpuReclaimOutcome::Infeasible {
                required_bytes: 100,
                baseline_occupied_bytes: 0,
            }
        );
    }

    #[test]
    fn adaptive_gpu_headroom_never_demands_reclaim_below_the_pinned_floor() {
        let pressure = GpuBudgetPressure::from_budget_failure(200, 900, 0, 1_000);
        let goal =
            GpuReclaimGoal::new(pressure, 900).with_additional_headroom_above_floor(200, 750);
        assert_eq!(goal.required_reclaim_bytes(), 150);
        assert!(goal.is_satisfied_at(750));

        let pinned_at_exact_fit =
            GpuReclaimGoal::new(pressure, 900).with_additional_headroom_above_floor(200, 800);
        assert_eq!(pinned_at_exact_fit.required_reclaim_bytes(), 100);
        assert!(pinned_at_exact_fit.is_satisfied_at(800));
    }

    #[test]
    fn intrinsically_oversized_steady_candidate_retains_typed_capacity_pressure() {
        for (occupied, retiring) in [(128, 64), (4_096, 3_500)] {
            let error = AllocationBundleError::SteadyBudgetExceeded {
                candidate: 1_001,
                occupied,
                retiring,
                limit: 1_000,
            };
            let pressure = alignment_gpu_bundle_budget_pressure(&error)
                .expect("an oversized exact candidate remains typed capacity pressure");
            assert_eq!(
                pressure.cause,
                rgv_render::GpuBudgetPressureCause::SteadyReplacement
            );
            assert_eq!(pressure.requested_bytes, 1_001);
            assert_eq!(pressure.retirement_credit_bytes, retiring);
            assert!(
                !GpuReclaimGoal::new(pressure, occupied.saturating_sub(retiring)).is_feasible()
            );
        }

        let overlap = alignment_gpu_bundle_budget_pressure(
            &AllocationBundleError::TransitionOverlapBudgetExceeded {
                candidate: 500,
                occupied: 900,
                limit: 1_200,
            },
        )
        .unwrap();
        assert_eq!(
            overlap.cause,
            rgv_render::GpuBudgetPressureCause::TransitionOverlap
        );
        assert_eq!(overlap.retirement_credit_bytes, 0);
    }

    #[test]
    fn optional_prefetch_quantum_adapts_gradually_from_measured_wall_time() {
        let mut state = AlignmentGpuPrefetchState::default();
        assert_eq!(state.maximum_pages().get(), 1);
        state.observe_quantum(Duration::from_micros(100), 1);
        assert_eq!(state.maximum_pages().get(), 2);
        state.observe_quantum(Duration::from_micros(100), 2);
        assert_eq!(state.maximum_pages().get(), 4);
        state.observe_quantum(Duration::from_millis(100), 4);
        assert_eq!(state.maximum_pages().get(), 2);
    }
}

impl Viewer {
    /// Immediately releases every optional GPU pin when a foreground camera/navigation target
    /// begins. Renderer pages remain reusable and exact; only the speculative whole-stack
    /// residency promise is revoked, and the same authority may warm again after input settles.
    pub(crate) fn revoke_alignment_gpu_optional_residency_for_foreground(&mut self) {
        let (reads, summaries) = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .map_or((false, false), |bundle| {
                (bundle.reads.is_some(), bundle.summaries.is_some())
            });
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.reset(reads, summaries);
        if self.alignment_gpu_speculation_gate.authority.is_some() {
            self.alignment_gpu_speculation_gate.phase = AlignmentGpuSpeculationPhase::Eligible;
        }
    }

    fn defer_alignment_gpu_publication(
        &mut self,
        attempt: &AlignmentScenePublicationAttempt,
        blocker: AlignmentGpuPublicationBlocker,
    ) {
        let snapshot = match self.alignment_scene_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.data_status =
                    format!("Could not inspect deferred alignment GPU capacity: {error}");
                return;
            }
        };
        let gate = alignment_gpu_publication_retry_gate(blocker, &snapshot);
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_publication_retry = Some(AlignmentGpuPublicationRetry {
            target_revision: attempt.target_revision,
            attempt_sequence: attempt.sequence,
            blocker,
            gate,
        });
        if matches!(
            gate,
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        ) {
            self.window.request_redraw();
        }
    }

    /// Starts one aggregate foreground-reclaim cycle after the renderer-local owners have
    /// exhausted their legal eviction candidates. Each live source engine is asked exactly once
    /// to release inactive semantic/source ownership toward the same ledger target. The existing
    /// typed retry gate prevents redraws from reopening the GPU transaction while replies arrive.
    fn defer_alignment_gpu_publication_for_cross_owner_reclaim(
        &mut self,
        attempt: &AlignmentScenePublicationAttempt,
        navigation_candidates: &[AlignmentCommittedLaneAuthority],
        pressure: GpuBudgetPressure,
    ) {
        let blocker = alignment_gpu_budget_pressure_blocker(pressure);
        self.wait_alignment_navigation_transactions_for_gpu(navigation_candidates);
        self.defer_alignment_gpu_publication(attempt, blocker);

        let snapshot = match self.alignment_scene_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_capacity_reclaim = None;
                self.data_status =
                    format!("Could not inspect aggregate alignment capacity: {error}");
                return;
            }
        };
        if !alignment_gpu_publication_blocker_remains(blocker, &snapshot) {
            // Renderer-local reclaim already crossed the exact threshold after the failure was
            // captured. The deferred gate is Ready and will issue the one permitted retry.
            return;
        }

        self.alignment_capacity_reclaim_clock = self
            .alignment_capacity_reclaim_clock
            .checked_add(1)
            .expect("alignment capacity reclaim request space was exhausted");
        let request_id = self.alignment_capacity_reclaim_clock;
        // A steady replacement may legitimately retain the old epoch until commit. Its exact
        // retirement credit therefore belongs in the physical occupancy ceiling; omitting it
        // would ask source/semantic owners to evict bytes already proved to disappear atomically.
        let target_charged = alignment_gpu_pressure_target_charged(pressure, &snapshot);
        let mut pending_tracks = HashSet::new();
        for track in &self.alignment_tracks {
            let Some(engine) = &track.scene_engine else {
                continue;
            };
            if engine
                .try_evict_inactive_for_headroom(request_id, target_charged)
                .is_ok()
            {
                pending_tracks.insert(track.config.id);
            }
        }
        self.alignment_capacity_reclaim = Some(AlignmentCapacityReclaimCycle {
            request_id,
            target_revision: attempt.target_revision,
            attempt_sequence: attempt.sequence,
            blocker,
            pressure,
            target_charged,
            pending_tracks,
            released_bytes: 0,
            relinquished_cache_entries: 0,
        });
        self.data_status = "Reclaiming inactive alignment cache for the current view…".to_owned();
        self.finish_alignment_capacity_reclaim_if_complete();
    }

    pub(crate) fn observe_alignment_capacity_reclaim(
        &mut self,
        track_id: TrackId,
        request_id: u64,
        released_bytes: usize,
        relinquished_cache_entries: usize,
    ) {
        let completed = {
            let Some(cycle) = self.alignment_capacity_reclaim.as_mut() else {
                return;
            };
            if cycle.request_id != request_id || !cycle.pending_tracks.remove(&track_id) {
                return;
            }
            cycle.released_bytes = cycle.released_bytes.saturating_add(released_bytes);
            cycle.relinquished_cache_entries = cycle
                .relinquished_cache_entries
                .saturating_add(relinquished_cache_entries);
            cycle.pending_tracks.is_empty()
        };
        if completed {
            self.finish_alignment_capacity_reclaim_if_complete();
        }
    }

    fn finish_alignment_capacity_reclaim_if_complete(&mut self) {
        let Some(cycle) = self.alignment_capacity_reclaim.as_ref() else {
            return;
        };
        if !cycle.pending_tracks.is_empty() {
            return;
        }
        let cycle = self
            .alignment_capacity_reclaim
            .take()
            .expect("the completed aggregate reclaim cycle still exists");
        if cycle.target_revision
            != self
                .alignment_scene_publication_authority
                .desired_target_revision()
            || cycle.attempt_sequence
                != self
                    .alignment_scene_publication_authority
                    .attempt_sequence()
        {
            return;
        }
        let snapshot = match self.alignment_scene_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.data_status =
                    format!("Could not inspect aggregate alignment capacity: {error}");
                return;
            }
        };
        if alignment_capacity_reclaim_allows_retry(
            cycle.blocker,
            &snapshot,
            cycle.relinquished_cache_entries,
        ) {
            if let Some(retry) = self.alignment_gpu_publication_retry.as_mut()
                && retry.target_revision == cycle.target_revision
                && retry.attempt_sequence == cycle.attempt_sequence
            {
                retry.gate = AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress;
            }
            tracing::debug!(
                request_id = cycle.request_id,
                released_bytes = cycle.released_bytes,
                relinquished_cache_entries = cycle.relinquished_cache_entries,
                target_charged = cycle.target_charged,
                blocker_cleared =
                    !alignment_gpu_publication_blocker_remains(cycle.blocker, &snapshot),
                "aggregate alignment reclaim made capacity or retirement-ownership progress"
            );
            self.window.request_redraw();
            return;
        }

        self.alignment_gpu_publication_retry = None;
        let remaining_bytes = snapshot
            .charged
            .saturating_add(snapshot.reserved)
            .saturating_add(cycle.pressure.requested_bytes)
            .saturating_sub(cycle.pressure.limit_bytes);
        let message = format!(
            "Complete alignment presentation is cache-budget limited: {} MiB additional capacity is required ({} MiB configured)",
            remaining_bytes.saturating_add((1 << 20) - 1) >> 20,
            cycle.pressure.limit_bytes >> 20,
        );
        tracing::warn!(
            request_id = cycle.request_id,
            released_bytes = cycle.released_bytes,
            relinquished_cache_entries = cycle.relinquished_cache_entries,
            remaining_bytes,
            charged_bytes = snapshot.charged,
            reserved_bytes = snapshot.reserved,
            pinned_bytes = snapshot.pinned,
            target_charged = cycle.target_charged,
            "aggregate alignment reclaim exhausted every registered owner"
        );
        self.settle_alignment_navigation_gpu_budget_limited(&message);
        self.data_status = message;
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    /// Polls a typed foreground refusal without converting the render loop into a retry clock.
    /// The exact latest target/attempt is retried once after ledger progress; unchanged epochs do
    /// nothing, and a newer attempt discards stale retry authority.
    pub(crate) fn retry_deferred_alignment_gpu_publication(&mut self) {
        let Some(retry) = self.alignment_gpu_publication_retry else {
            return;
        };
        let snapshot = match self.alignment_scene_ledger.progress() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.alignment_gpu_publication_retry = None;
                self.data_status =
                    format!("Could not inspect deferred alignment GPU capacity: {error}");
                return;
            }
        };
        match alignment_gpu_publication_retry_poll(
            retry,
            self.alignment_scene_publication_authority
                .desired_target_revision(),
            self.alignment_scene_publication_authority
                .attempt_sequence(),
            &snapshot,
        ) {
            AlignmentGpuPublicationRetryPoll::Superseded => {
                self.alignment_gpu_publication_retry = None;
                self.alignment_capacity_reclaim = None;
            }
            AlignmentGpuPublicationRetryPoll::Waiting(updated) => {
                self.alignment_gpu_publication_retry = Some(updated);
            }
            AlignmentGpuPublicationRetryPoll::Ready => {
                tracing::debug!(
                    blocker = ?retry.blocker,
                    "retrying alignment GPU publication after ledger progress"
                );
                self.alignment_gpu_publication_retry = None;
                self.alignment_capacity_reclaim = None;
                self.prepare_alignment_scene_frame_atomic();
            }
        }
    }

    /// Gives a foreground source retry priority over every optional GPU page. Published frame
    /// pins remain intact, while complete-scene residency, pending prefetch authority, idle frame
    /// buffers, sparse tables, and unpinned LRU pages yield under one shared-ledger headroom goal.
    /// The returned epoch is recorded with the exact retry so an unchanged refusal cannot spin.
    pub(crate) fn yield_alignment_gpu_speculation_for_source_pressure(
        &mut self,
        pressure: LedgerError,
    ) -> std::result::Result<LedgerProgressEpoch, LedgerError> {
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_speculation_gate
            .suppress_for_foreground_pressure();

        if let Some(pressure) = alignment_source_pressure_gpu_budget(pressure) {
            let baseline = self.alignment_scene_ledger.progress()?;
            let occupied = baseline.charged.saturating_add(baseline.reserved);
            let goal = GpuReclaimGoal::new(pressure, occupied)
                .with_additional_headroom_above_floor(pressure.requested_bytes, baseline.pinned);
            if goal.is_feasible() && !goal.is_satisfied_at(occupied) {
                let reads = self.alignment_gpu_renderer.reclaim_for_goal(
                    &self.alignment_scene_ledger,
                    goal,
                    &[],
                )?;
                if !reads.headroom_satisfied {
                    let _ = self.alignment_summary_gpu_renderer.reclaim_for_goal(
                        &self.alignment_scene_ledger,
                        goal,
                        &[],
                    )?;
                }
            }
        }

        Ok(self.alignment_scene_ledger.progress()?.progress_epoch())
    }

    /// Coordinates one composable failure-time reclaim goal across both GPU cache owners. Every
    /// full-scene residency lease is speculative and retires together, while published frames and
    /// both upcoming foreground key sets remain pinned/protected. Reads and summaries participate
    /// in the same deterministic pass because the retry is one combined candidate, not a request
    /// from either renderer independently.
    fn reclaim_alignment_gpu_caches_for_foreground(
        &mut self,
        pressure: GpuBudgetPressure,
        protected_read_pages: &[AlignmentPageKey],
        protected_summary_pages: &[AlignmentSummaryGpuPageKey],
    ) -> std::result::Result<AlignmentGpuReclaimOutcome, LedgerError> {
        let baseline = self.alignment_scene_ledger.progress()?;
        let baseline_occupied = baseline.charged.saturating_add(baseline.reserved);
        let goal = GpuReclaimGoal::new(pressure, baseline_occupied)
            .with_additional_headroom_above_floor(pressure.requested_bytes, baseline.pinned);
        if !goal.is_feasible() {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            tracing::warn!(
                pressure_cause = ?pressure.cause,
                requested_bytes = pressure.requested_bytes,
                retirement_credit_bytes = pressure.retirement_credit_bytes,
                charged_bytes_at_failure = pressure.charged_bytes_at_failure,
                reserved_bytes_at_failure = pressure.reserved_bytes_at_failure,
                allocation_limit_bytes = pressure.limit_bytes,
                required_reclaim_bytes = goal.required_reclaim_bytes(),
                baseline_occupied_bytes = goal.baseline_occupied_bytes,
                "alignment GPU candidate is intrinsically larger than reclaimable capacity"
            );
            return Ok(AlignmentGpuReclaimOutcome::for_goal(
                goal,
                baseline_occupied,
                baseline.pinned,
                0,
                0,
            ));
        }

        let retired_residency = self.alignment_gpu_residency_bundle.take();
        let retired_read_residency = usize::from(
            retired_residency
                .as_ref()
                .is_some_and(|bundle| bundle._reads.is_some()),
        );
        let retired_summary_residencies = usize::from(
            retired_residency
                .as_ref()
                .is_some_and(|bundle| bundle._summaries.is_some()),
        );
        drop(retired_residency);
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_speculation_gate
            .suppress_for_foreground_pressure();

        let reads = self.alignment_gpu_renderer.reclaim_for_goal(
            &self.alignment_scene_ledger,
            goal,
            protected_read_pages,
        )?;
        let summaries = if reads.headroom_satisfied {
            None
        } else {
            Some(self.alignment_summary_gpu_renderer.reclaim_for_goal(
                &self.alignment_scene_ledger,
                goal,
                protected_summary_pages,
            )?)
        };
        let final_snapshot = self.alignment_scene_ledger.progress()?;
        let final_occupied = final_snapshot
            .charged
            .saturating_add(final_snapshot.reserved);
        let outcome = AlignmentGpuReclaimOutcome::for_goal(
            goal,
            final_occupied,
            final_snapshot.pinned,
            retired_read_residency,
            retired_summary_residencies,
        );
        let headroom_satisfied = matches!(outcome, AlignmentGpuReclaimOutcome::Satisfied { .. });
        tracing::debug!(
            pressure_cause = ?pressure.cause,
            retired_read_residency,
            retired_summary_residencies,
            requested_bytes = pressure.requested_bytes,
            retirement_credit_bytes = pressure.retirement_credit_bytes,
            charged_bytes_at_failure = pressure.charged_bytes_at_failure,
            reserved_bytes_at_failure = pressure.reserved_bytes_at_failure,
            allocation_limit_bytes = pressure.limit_bytes,
            required_reclaim_bytes = goal.required_reclaim_bytes(),
            read_reclaimed_pages = reads.reclaimed_pages(),
            read_released_bytes = reads.released_ledger_bytes(),
            summary_reclaimed_pages = summaries
                .as_ref()
                .map_or(0, GpuReclaimReport::reclaimed_pages),
            summary_released_bytes = summaries
                .as_ref()
                .map_or(0, GpuReclaimReport::released_ledger_bytes),
            headroom_satisfied,
            "coordinated alignment GPU foreground reclaim"
        );
        Ok(outcome)
    }

    pub(crate) fn apply_live_cache_budget(&mut self, cache_budget_mib: usize) {
        let previous_budget_bytes = self
            .alignment_scene_ledger
            .progress()
            .map_or(self.alignment_scene_budget.budget_bytes, |snapshot| {
                snapshot.budget
            });
        let total_bytes = cache_budget_mib.saturating_mul(1024 * 1024).max(1);
        self.refresh_alignment_scene_budget();
        let current_budget_bytes = self
            .alignment_scene_ledger
            .progress()
            .map_or(total_bytes, |snapshot| snapshot.budget);
        if current_budget_bytes < previous_budget_bytes {
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate.budget_bytes = current_budget_bytes;
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            if let Ok(snapshot) = self.alignment_scene_ledger.progress() {
                let pressure = GpuBudgetPressure::from_budget_failure(
                    0,
                    snapshot.charged,
                    snapshot.reserved,
                    current_budget_bytes,
                );
                let goal = GpuReclaimGoal::new(
                    pressure,
                    snapshot.charged.saturating_add(snapshot.reserved),
                );
                if goal.required_reclaim_bytes() > 0 {
                    let _ = self.alignment_gpu_renderer.reclaim_for_goal(
                        &self.alignment_scene_ledger,
                        goal,
                        &[],
                    );
                    let _ = self.alignment_summary_gpu_renderer.reclaim_for_goal(
                        &self.alignment_scene_ledger,
                        goal,
                        &[],
                    );
                }
            }
        } else if current_budget_bytes > previous_budget_bytes {
            self.alignment_gpu_speculation_gate.budget_bytes = current_budget_bytes;
            self.alignment_gpu_speculation_gate.phase = AlignmentGpuSpeculationPhase::Eligible;
            let (reads, summaries) = self
                .alignment_presented_epoch
                .as_ref()
                .and_then(|epoch| epoch.gpu_bundle.as_ref())
                .map_or((false, false), |bundle| {
                    (bundle.reads.is_some(), bundle.summaries.is_some())
                });
            self.alignment_gpu_prefetch.reset(reads, summaries);
            self.window.request_redraw();
        }
    }

    pub(crate) fn aggregate_cache_budget_bytes(&self) -> usize {
        self.settings
            .cache_budget_mib
            .saturating_mul(1024 * 1024)
            .max(1)
    }

    fn externally_reserved_gpu_cache_bytes(&self) -> (usize, usize) {
        (
            self.context_scene_gpu_renderer.resident_bytes(),
            self.rectangle_renderer.resident_bytes(),
        )
    }

    pub(crate) fn alignment_ledger_occupied_bytes(&self) -> usize {
        self.alignment_scene_ledger
            .progress()
            .map_or(0, |progress| {
                progress.charged.saturating_add(progress.reserved)
            })
    }

    /// Reconciles the shared alignment ledger and the two not-yet-ledger-native GPU caches under
    /// one configured steady-state ceiling. Context and generic slabs receive explicit
    /// non-borrowing reservations: their measured resident bytes are subtracted from the ledger
    /// allowance, while their next admission allowance subtracts current ledger occupancy. This
    /// prevents the setting from being multiplied across independent pools until those renderers
    /// participate in move-only ledger transactions directly.
    pub(crate) fn refresh_alignment_scene_budget(&mut self) {
        let total_budget_bytes = self.aggregate_cache_budget_bytes();
        let (context_bytes, rectangle_bytes) = self.externally_reserved_gpu_cache_bytes();
        let external_reservation = context_bytes.saturating_add(rectangle_bytes);
        let alignment_budget_bytes = total_budget_bytes
            .saturating_sub(external_reservation)
            .max(1);
        self.alignment_scene_budget.budget_bytes = alignment_budget_bytes;
        if let Err(error) = self.alignment_scene_ledger.set_limits(
            alignment_budget_bytes,
            total_budget_bytes,
            total_budget_bytes,
        ) {
            self.data_status = format!("Could not apply alignment resource limits: {error}");
        }

        let allowance = NonZeroUsize::new(alignment_budget_bytes)
            .expect("the effective alignment budget is non-zero");
        let mut first_error = None;
        for track in &mut self.alignment_tracks {
            let Some(engine) = &track.scene_engine else {
                continue;
            };
            if track.scene_budget_bytes == allowance {
                continue;
            }
            match engine.try_reduce_budget(alignment_budget_bytes) {
                Ok(()) => {
                    track.scene_budget_bytes = allowance;
                    track.scene_budget_retry = None;
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| error.to_string());
                }
            }
        }

        self.context_lane_gpu_budget_bytes = total_budget_bytes
            .saturating_sub(self.alignment_ledger_occupied_bytes())
            .saturating_sub(context_bytes)
            .saturating_sub(self.rectangle_renderer.primary_resident_bytes());
        if let Ok(progress) = self.alignment_scene_ledger.progress() {
            self.alignment_scene_budget = AlignmentSceneBudgetSnapshot::from_progress(
                progress,
                self.alignment_scene_budget.refreshes.saturating_add(1),
            );
        }
        if let Some(error) = first_error {
            self.data_status = format!("Could not apply cache budget: {error}");
        }
    }

    pub(crate) fn request_alignment_query(&mut self) {
        let panel_id = self.panels.active().id;
        for track_index in 0..self.alignment_tracks.len() {
            self.queue_alignment_navigation_for_track(
                track_index,
                panel_id,
                false,
                "alignment refresh requested",
            );
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
        self.dispatch_alignment_query();
    }

    pub(crate) fn request_alignment_scene_camera_update(&mut self, track_id: TrackId) {
        let panel_id = self.panels.active().id;
        let Some(track_index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == track_id)
        else {
            return;
        };
        if !matches!(
            self.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Ready
        ) {
            return;
        }
        self.queue_alignment_navigation_for_track(
            track_index,
            panel_id,
            false,
            "alignment row camera changed",
        );
        self.panels.active_mut().pending_queries.alignment = true;
        self.dispatch_alignment_query();
    }

    /// Resolves the complete live presentation demand for one track from the final pane layout.
    /// A read camera exists only for actual read pixels; a derived-only track receives a genomic
    /// preparation camera with no fabricated rows or clip.
    fn alignment_scene_lane_capture_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        contig: ContigId,
        contig_length: u64,
        reference_backed_alleles: bool,
    ) -> Result<Option<AlignmentPanelLaneCapture>> {
        let Some(panel) = self.panels.get(panel_id) else {
            return Ok(None);
        };
        if panel.viewport.detail_level() == DetailLevel::Overview {
            return Ok(None);
        }
        let Some(panel_rect) = self.panel_column_rect(panel_id) else {
            return Ok(None);
        };
        if panel_rect.width() <= 0.0 {
            return Ok(None);
        }
        let Some(track) = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
        else {
            return Ok(None);
        };
        let filters = self.alignment_filters_for_track(track_id);
        let sashimi_group = alignment_scene_sashimi_group(&filters);
        let selected_pileup_position = alignment_scene_selected_position(
            panel.selected_alignment_track,
            track_id,
            panel.selected_pileup_position,
        );
        let selected_modification_position = alignment_scene_selected_position(
            panel.selected_alignment_track,
            track_id,
            panel.selected_modification_position,
        );
        let derived_lanes = alignment_scene_derived_lane_captures(
            &panel.pane_layout.annotation,
            track_id,
            f64::from(panel_rect.left()),
            f64::from(panel_rect.width()),
            &filters,
            reference_backed_alleles,
            sashimi_group,
            selected_pileup_position,
            selected_modification_position,
        )?;
        let viewport = panel
            .viewport
            .remapped_contig(contig, contig_length)
            .context("could not remap the alignment scene camera")?;
        let viewport =
            alignment_scene_viewport_for_layout_width(viewport, f64::from(panel_rect.width()))
                .context(
                    "could not reconcile the alignment scene camera with the live panel width",
                )?;
        let read_camera = alignment_scene_read_clip(
            &panel.pane_layout.reads,
            track_id,
            f64::from(panel_rect.left()),
            f64::from(panel_rect.width()),
        )
        .map(|clip| {
            let density = self.alignment_density_for_panel_track(panel_id, track_id);
            let row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            let read_height = (5.0 * density).clamp(1.5, (row_height - 1.0).max(1.5));
            let row_scroll = panel
                .read_panel_scroll_state(track_id)
                .effective_scroll_points()
                / row_height;
            AlignmentCamera::from_viewport(
                viewport,
                f64::from(row_scroll),
                f64::from(row_height),
                f64::from(read_height),
                clip,
            )
        })
        .transpose()
        .context("could not construct the alignment read camera")?;
        if read_camera.is_none() && derived_lanes.is_empty() {
            return Ok(None);
        }
        let preparation_camera = read_camera.map_or_else(
            || AlignmentSceneCamera::without_visible_rows(viewport),
            AlignmentSceneCamera::from,
        );
        AlignmentPanelLaneCapture::new(
            track.config.id,
            preparation_camera,
            read_camera,
            derived_lanes,
        )
        .map(Some)
        .map_err(Into::into)
    }

    /// Reconciles scene cameras after a desktop layout pass. Pane resizing, density changes,
    /// track expansion, and window-scale changes alter only camera/clip state; they must not wait
    /// for an unrelated genomic navigation transaction or rebuild alignment evidence.
    pub(crate) fn refresh_alignment_scene_cameras_after_layout(&mut self) {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut changed_by_panel = Vec::<(PanelId, Vec<TrackId>)>::new();
        for panel_id in panel_ids {
            let Some(panel) = self.panels.get(panel_id) else {
                continue;
            };
            let contig_name = panel.contig_name.clone();
            let changed = self
                .alignment_tracks
                .iter()
                .filter_map(|track| {
                    if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
                        return None;
                    }
                    let metadata = track.scene_metadata.as_ref()?;
                    let contig = metadata.dictionary.id_by_name(&contig_name)?;
                    let contig_length = metadata.dictionary.get(contig)?.length();
                    let reference_backed_alleles = track
                        .projection(panel_id)
                        .and_then(|projection| projection.scene_contract.as_ref())
                        .is_some_and(|contract| {
                            contract.summaries.allele_detail != AlleleSummaryDetail::None
                                && contract.presentation.reference.is_some()
                        });
                    let camera = self
                        .alignment_scene_lane_capture_for_panel_track(
                            panel_id,
                            track.config.id,
                            contig,
                            contig_length,
                            reference_backed_alleles,
                        )
                        .ok()
                        .flatten()?
                        .preparation_camera;
                    track
                        .projection(panel_id)
                        .and_then(|projection| projection.navigation.desired_camera())
                        .filter(|current| *current == camera)
                        .is_none()
                        .then_some(track.config.id)
                })
                .collect::<Vec<_>>();
            if !changed.is_empty() {
                changed_by_panel.push((panel_id, changed));
            }
        }
        if changed_by_panel.is_empty() {
            // Workspace geometry, surface scale, or physical extent can change without changing
            // coordinator demand. Still publish (or stale-gate) the native derivative for that
            // exact target instead of leaving an older layout revision implicitly current.
            self.prepare_alignment_scene_frame();
            return;
        }

        let active_id = self.panels.active().id;
        for (panel_id, track_ids) in changed_by_panel {
            for track_id in track_ids {
                let Some(track_index) = self
                    .alignment_tracks
                    .iter()
                    .position(|track| track.config.id == track_id)
                else {
                    continue;
                };
                self.queue_alignment_navigation_for_track(
                    track_index,
                    panel_id,
                    true,
                    "alignment pane geometry changed",
                );
            }
            self.panels.activate(panel_id);
            self.panels.active_mut().pending_queries.alignment = true;
            self.dispatch_alignment_query();
        }
        self.panels.activate(active_id);
    }

    /// Captures every visible alignment lane from one immutable panel-scene epoch and prepares
    /// its disposable GPU derivative. The source worker is never touched here: held pan and
    /// vertical scroll can call this on every input event using already-published pages.
    pub(crate) fn prepare_alignment_scene_frame(&mut self) {
        self.prepare_alignment_scene_frame_atomic();
    }

    fn uncovered_vertical_candidate_lane_count(
        &self,
        candidate: &CapturedAlignmentPanelSceneEpoch,
    ) -> usize {
        let Some(committed_epoch) = self.alignment_presented_epoch.as_ref() else {
            return 0;
        };
        let Some(committed_bundle) = committed_epoch.gpu_bundle.as_ref() else {
            return 0;
        };
        if !alignment_gpu_bundle_is_current(
            committed_bundle,
            &self.alignment_scene_publication_authority,
        ) {
            return 0;
        }

        candidate
            .panel_frames
            .iter()
            .flat_map(|(panel_id, candidate_frame)| {
                candidate_frame.lanes.iter().filter(move |candidate_lane| {
                    let Some(committed_lane) = committed_epoch
                        .panel_frames
                        .get(panel_id)
                        .and_then(|frame| frame.lane(candidate_lane.track_id))
                    else {
                        return false;
                    };
                    let committed_native_reads = committed_bundle.reads.is_some()
                        && committed_bundle.read_authorities.iter().any(|authority| {
                            authority.panel_id == *panel_id
                                && authority.track_id == candidate_lane.track_id
                        });
                    candidate_lane.read_camera.is_some()
                        && committed_lane.read_camera.is_some()
                        && uncovered_vertical_candidate_retains_committed_pixels(
                            committed_lane.preparation_camera.viewport(),
                            committed_lane.reads_complete(),
                            committed_native_reads,
                            candidate_lane.preparation_camera.viewport(),
                            &candidate_lane.requested_rows,
                            &candidate_lane.covered_rows,
                        )
                })
            })
            .count()
    }

    fn commit_alignment_navigation_transactions(&mut self, bundle: &AlignmentGpuFrameBundle) {
        for authority in &bundle.lane_authorities {
            if !authority.semantic_target_complete {
                continue;
            }
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == authority.track_id)
            else {
                continue;
            };
            let projection = track.projection_mut(authority.panel_id);
            if projection.navigation.request_token() != Some(authority.desired_request_token)
                || projection.navigation.desired_camera() != Some(authority.source_camera)
                || !matches!(
                    projection.navigation.phase(),
                    AlignmentNavigationPhase::CandidateReady
                        | AlignmentNavigationPhase::WaitingForGpuPressure
                )
            {
                continue;
            }
            projection.navigation.transition(
                AlignmentNavigationPhase::Committed,
                "semantic scene and native GPU derivative committed atomically",
            );
            projection.finish_checking();
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    pub(crate) fn synchronize_alignment_pending_mirrors(&mut self) {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for panel_id in panel_ids {
            let pending = self.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(panel_id).is_some_and(|projection| {
                        !projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
            });
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.pending_queries.alignment = pending;
            }
        }
    }

    fn wait_alignment_navigation_transactions_for_gpu(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
    ) {
        for candidate in candidates {
            if !candidate.semantic_target_complete {
                continue;
            }
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == candidate.track_id)
            else {
                continue;
            };
            let projection = track.projection_mut(candidate.panel_id);
            if projection.navigation.request_token() == Some(candidate.desired_request_token)
                && projection.navigation.desired_camera() == Some(candidate.source_camera)
                && projection.navigation.phase() == AlignmentNavigationPhase::CandidateReady
            {
                projection.navigation.transition(
                    AlignmentNavigationPhase::WaitingForGpuPressure,
                    "atomic GPU publication is waiting for an exact ledger blocker",
                );
            }
        }
    }

    fn resume_alignment_navigation_transactions_after_gpu_progress(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
    ) {
        for candidate in candidates {
            if !candidate.semantic_target_complete {
                continue;
            }
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == candidate.track_id)
            else {
                continue;
            };
            let projection = track.projection_mut(candidate.panel_id);
            if projection.navigation.request_token() == Some(candidate.desired_request_token)
                && projection.navigation.desired_camera() == Some(candidate.source_camera)
                && projection.navigation.phase() == AlignmentNavigationPhase::WaitingForGpuPressure
            {
                projection.navigation.transition(
                    AlignmentNavigationPhase::CandidateReady,
                    "GPU blocker cleared; rebuilding the same atomic candidate",
                );
            }
        }
    }

    fn fail_alignment_navigation_gpu_candidates(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
        message: &str,
    ) {
        for candidate in candidates {
            if !candidate.semantic_target_complete {
                continue;
            }
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == candidate.track_id)
            else {
                continue;
            };
            let projection = track.projection_mut(candidate.panel_id);
            if projection.navigation.request_token() == Some(candidate.desired_request_token)
                && projection.navigation.desired_camera() == Some(candidate.source_camera)
                && matches!(
                    projection.navigation.phase(),
                    AlignmentNavigationPhase::CandidateReady
                        | AlignmentNavigationPhase::WaitingForGpuPressure
                )
            {
                projection.settle_failed(message.to_owned());
            }
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    fn settle_alignment_navigation_gpu_budget_limited(&mut self, message: &str) {
        for track in &mut self.alignment_tracks {
            for projection in track.projections.values_mut() {
                if projection.navigation.phase() == AlignmentNavigationPhase::WaitingForGpuPressure
                {
                    projection.navigation.transition(
                        AlignmentNavigationPhase::BudgetLimited,
                        Arc::<str>::from(message),
                    );
                    projection.finish_checking();
                    projection.unavailable = None;
                }
            }
        }
        self.synchronize_alignment_pending_mirrors();
        self.resume_alignment_optional_work_for_settled_panels();
    }

    /// Ensures the scroll/display derivative catches up after the semantic/native epoch commits.
    /// Worker polling historically rebuilt this cache before GPU publication, which could leave
    /// a Pending or old-target full thumb beside newly committed exact pixels indefinitely.
    fn invalidate_stale_alignment_scroll_cache(&mut self) {
        struct CommittedReadCacheUpdate {
            panel_id: PanelId,
            track_id: TrackId,
            target_revision: u64,
            camera: Option<AlignmentCamera>,
            row_activity: Option<Arc<AlignmentRowActivityProfile>>,
            native_reads_present: bool,
            materialized_row_ranges: Option<Arc<[(usize, usize)]>>,
        }

        let expected = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| {
                let bundle = epoch.gpu_bundle.as_ref()?;
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
                    .then(|| {
                        epoch
                            .panel_frames
                            .values()
                            .flat_map(|frame| {
                                frame.lanes.iter().map(|lane| {
                                    let native_reads_present =
                                        bundle.read_authorities.iter().any(|authority| {
                                            authority.panel_id == frame.panel_id
                                                && authority.track_id == lane.track_id
                                                && authority.target_revision
                                                    == bundle.target_revision
                                        });
                                    let materialized_row_ranges = native_reads_present
                                        .then(|| {
                                            lane.snapshot.as_ref().map(|scene| {
                                                super::panels::normalize_materialized_row_ranges(
                                                    scene
                                                        .reads()
                                                        .pages()
                                                        .iter()
                                                        .map(|page| page.row_range()),
                                                )
                                            })
                                        })
                                        .flatten();
                                    CommittedReadCacheUpdate {
                                        panel_id: frame.panel_id,
                                        track_id: lane.track_id,
                                        target_revision: bundle.target_revision,
                                        camera: lane.read_camera,
                                        row_activity: lane.row_activity.as_ref().map(Arc::clone),
                                        native_reads_present,
                                        materialized_row_ranges,
                                    }
                                })
                            })
                            .collect::<Vec<_>>()
                    })
            })
            .unwrap_or_default();
        let primary_track_id = self.primary_alignment_track_id();
        let mut needs_rebuild = false;
        let record_cache_mismatches = self.manual_diagnostic_trace_is_recording();
        let mut cache_mismatches = Vec::new();
        for update in expected {
            let density = self.alignment_density_for_panel_track(update.panel_id, update.track_id);
            let density_drag_fraction = self
                .vertical_overview_drag
                .filter(|drag| {
                    drag.panel_id == update.panel_id
                        && drag.track_id == update.track_id
                        && drag.density_dragging
                })
                .map(|drag| drag.virtual_scroll_fraction);
            let Some(panel) = self.panels.get_mut(update.panel_id) else {
                needs_rebuild = true;
                continue;
            };
            let Some(read_panel_index) = panel.scroll_display_cache.as_ref().and_then(|cache| {
                cache
                    .read_panels
                    .iter()
                    .position(|read_panel| read_panel.track_id == update.track_id)
            }) else {
                // A summary-only/hidden read lane with no cached read panel is already absent.
                needs_rebuild |= update.camera.is_some();
                continue;
            };

            let activity_identity = update.row_activity.as_deref().map(|profile| {
                (
                    profile.row_coordinate_space(),
                    profile.atlas_revision(),
                    profile.active_row_end(),
                )
            });
            let cache_was_current = {
                let read_panel = &panel
                    .scroll_display_cache
                    .as_ref()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let current = if update.camera.is_none() {
                    read_panel.presentation_target_revision == update.target_revision
                        && read_panel.row_coordinate_authority == ReadRowCoordinateAuthority::Absent
                        && read_panel.row_activity_atlas_revision.is_none()
                        && read_panel.total_rows.is_none()
                } else {
                    read_row_cache_matches_committed_activity(
                        read_panel.presentation_target_revision,
                        read_panel.row_coordinate_authority,
                        read_panel.row_activity_atlas_revision,
                        read_panel.total_rows,
                        update.target_revision,
                        activity_identity,
                    )
                };
                current
            };
            needs_rebuild |= !cache_was_current;
            if record_cache_mismatches && !cache_was_current {
                let read_panel = &panel
                    .scroll_display_cache
                    .as_ref()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let cached_authority = serde_json::json!({
                    "presentation_target_revision": read_panel.presentation_target_revision,
                    "raw_row_coordinate_authority": format!("{:?}", read_panel.row_coordinate_authority),
                    "row_activity_atlas_revision": read_panel.row_activity_atlas_revision.map(|revision| format!("{revision:?}")),
                    "total_rows": read_panel.total_rows,
                    "materialized_row_ranges": read_panel.materialized_row_ranges.as_deref(),
                });
                cache_mismatches.push(serde_json::json!({
                    "panel_id": update.panel_id.get(),
                    "track_id": update.track_id.get(),
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                    "committed_row_target_matches_desired": self.alignment_scene_publication_authority.committed_row_target_matches_desired(update.panel_id, update.track_id),
                    "incoming_target_revision": update.target_revision,
                    "incoming_camera_present": update.camera.is_some(),
                    "incoming_native_reads_present": update.native_reads_present,
                    "incoming_row_activity_identity": activity_identity.map(|(coordinate_space, atlas_revision, rows)| serde_json::json!({
                        "coordinate_space": format!("{coordinate_space:?}"),
                        "atlas_revision": format!("{atlas_revision:?}"),
                        "active_rows": rows,
                    })),
                    "cached": cached_authority,
                }));
            }

            let Some(camera) = update.camera else {
                // A committed summary-only lane has no live read coordinate space. Clear current
                // scrollbar authority while retaining the panel's latent camera for restoration
                // if Reads is expanded again.
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.density_camera = false;
                read_panel.row_origin = None;
                read_panel.row_height = None;
                read_panel.row_coordinate_authority = ReadRowCoordinateAuthority::Absent;
                read_panel.row_activity_atlas_revision = None;
                read_panel.materialized_row_ranges = None;
                read_panel.total_rows = None;
                read_panel.authoritative_overview = None;
                read_panel.overview = ui::ScrollbarOverview::default();
                read_panel.actual_content_bottom = read_panel.clip_bottom;
                continue;
            };

            let Some(profile) = update.row_activity.as_deref() else {
                // Preserve the last exact extent/overview as a visible waypoint, but revoke exact
                // gesture and hit authority synchronously. The exact successor will atomically
                // replace these provisional metrics.
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.row_coordinate_authority = ReadRowCoordinateAuthority::Pending;
                read_panel.row_activity_atlas_revision = None;
                continue;
            };

            let row_height = camera.row_height() as f32;
            let row_origin = camera.clip().y as f32;
            let authoritative_overview = alignment_row_activity_overview(profile);
            let (clip_top, clip_bottom, actual_content_bottom, row_coordinate_space) = {
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let (_, actual_content_bottom) = alignment_scene_read_extent(
                    profile.active_row_end(),
                    row_origin,
                    row_height,
                    read_panel.clip_bottom,
                );
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.density_camera = update.native_reads_present;
                read_panel.row_origin = update.native_reads_present.then_some(row_origin);
                read_panel.row_height = update.native_reads_present.then_some(row_height);
                read_panel.row_coordinate_authority =
                    ReadRowCoordinateAuthority::Exact(profile.row_coordinate_space());
                read_panel.row_activity_atlas_revision = Some(profile.atlas_revision());
                read_panel.materialized_row_ranges = update.materialized_row_ranges;
                read_panel.total_rows = Some(profile.active_row_end());
                read_panel.authoritative_overview = Some(authoritative_overview.clone());
                read_panel.overview = authoritative_overview;
                read_panel.actual_content_bottom = actual_content_bottom;
                (
                    read_panel.clip_top,
                    read_panel.clip_bottom,
                    actual_content_bottom,
                    profile.row_coordinate_space(),
                )
            };

            let compatibility_primary = ReadPanelScrollState {
                scroll_points: panel.vertical_scroll_points,
                content_origin_points: panel.vertical_content_origin_points,
                latent_scroll_fraction: panel.latent_vertical_scroll_fraction,
                content_bottom_points: panel.content_bottom_points,
                density: None,
                stable_row_anchor: None,
                scene_contig: None,
            };
            let state = panel
                .read_panel_scrolls
                .entry(update.track_id)
                .or_insert_with(|| {
                    if update.track_id == primary_track_id {
                        compatibility_primary
                    } else {
                        ReadPanelScrollState::default()
                    }
                });
            let camera_padding = if update.native_reads_present {
                vertical_density_camera_padding((clip_bottom - clip_top).max(1.0), density)
            } else {
                0.0
            };
            let exact_row_camera = reconcile_read_panel_scroll_for_authority(
                state,
                ReadRowCoordinateAuthority::Exact(row_coordinate_space),
                update.native_reads_present.then_some(row_height),
                actual_content_bottom,
                clip_bottom,
                camera_padding,
                density_drag_fraction,
            );
            if let Some((settled_row_height, coordinate_space)) = exact_row_camera
                && settled_row_height > f32::EPSILON
            {
                state.stable_row_anchor = capture_stable_row_camera(
                    state.effective_scroll_points(),
                    settled_row_height,
                    coordinate_space,
                );
            }
            if update.track_id == primary_track_id {
                panel.vertical_scroll_points = state.scroll_points;
                panel.vertical_content_origin_points = state.content_origin_points;
                panel.latent_vertical_scroll_fraction = state.latent_scroll_fraction;
                panel.content_bottom_points = state.content_bottom_points;
            }
        }
        for mismatch in cache_mismatches {
            self.record_manual_diagnostic_event(
                "display_invalidation",
                "scroll_cache_authority_mismatch",
                mismatch,
            );
        }
        if needs_rebuild {
            self.mark_display_dirty("committed scroll-cache authority changed");
            self.window.request_redraw();
        }
    }

    fn alignment_filter_authorities_for_trace(
        &self,
        authorities: &[AlignmentCommittedLaneAuthority],
    ) -> Vec<serde_json::Value> {
        if !self.manual_diagnostic_trace_is_recording() {
            return Vec::new();
        }
        authorities
            .iter()
            .map(|authority| {
                let projection = self
                    .alignment_tracks
                    .iter()
                    .find(|track| track.config.id == authority.track_id)
                    .and_then(|track| track.projection(authority.panel_id));
                let presented = self
                    .presented_alignment_lane(authority.panel_id, authority.track_id)
                    .and_then(|presented| presented.lane.snapshot.as_ref())
                    .map(|snapshot| {
                        let filter = snapshot.reads().evidence().view_filter();
                        serde_json::json!({
                            "scene_revision": {
                                "high": snapshot.revision().fingerprint().high(),
                                "low": snapshot.revision().fingerprint().low(),
                            },
                            "filter_evaluation_fingerprint": filter.fingerprint().get(),
                            "eligible_records": filter.eligible_records(),
                            "matching_records": filter.matching_records(),
                            "predicate_matches": filter.predicate_matches(),
                            "row_atlas_filter_evaluation_fingerprint": snapshot.reads().atlas().view_filter().get(),
                            "summary_evidence_coherent": snapshot.summaries().evidence_revision()
                                == snapshot.reads().evidence().revision(),
                        })
                    });
                serde_json::json!({
                    "panel_id": authority.panel_id.get(),
                    "track_id": authority.track_id.get(),
                    "source_request_token": authority.source_request_token.get(),
                    "desired_request_token": authority.desired_request_token.get(),
                    "source_acquisitions_after": projection.map_or(0, |projection| projection.scene_telemetry.source_acquisitions),
                    "presented": presented,
                })
            })
            .collect()
    }

    fn prepare_alignment_scene_frame_atomic(&mut self) {
        // Capture first, then expose the desired target before fallible publication. The prior
        // committed epoch retains ownership for rollback, but render/hit/live-region consumers
        // stale-gate it immediately when the actual camera or layout target changed.
        let scale_factor = self.window.scale_factor();
        let capture_started = Instant::now();
        let candidate_epoch = self.capture_alignment_panel_scene_epoch(0);
        let capture_elapsed = capture_started.elapsed();
        let mut candidate_epoch = match candidate_epoch {
            Ok(candidate) => candidate,
            Err(mut failure) => {
                failure.fields.insert(
                    "capture_elapsed_us".to_owned(),
                    serde_json::json!(capture_elapsed.as_micros()),
                );
                failure.fields.insert(
                    "publication_disposition".to_owned(),
                    serde_json::json!(
                        "rejected before target authority advanced; committed epoch retained"
                    ),
                );
                self.data_status.clone_from(&failure.message);
                self.queue_manual_diagnostic_trace_failure(
                    "alignment_publication",
                    "panel_scene_capture_failed",
                    &failure.message,
                    failure.fields,
                );
                return;
            }
        };
        let uncovered_vertical_lanes =
            self.uncovered_vertical_candidate_lane_count(&candidate_epoch);
        if uncovered_vertical_lanes > 0 {
            let lane_diagnostics = candidate_epoch
                .panel_frames
                .iter()
                .flat_map(|(panel_id, frame)| {
                    frame.lanes.iter().map(|lane| {
                        serde_json::json!({
                            "panel_id": panel_id.get(),
                            "track_id": lane.track_id.get(),
                            "camera_revision": frame.camera_revision,
                            "preparation_interval": format!("{:?}", lane.preparation_camera.interval()),
                            "read_camera_present": lane.read_camera.is_some(),
                            "requested_rows": {
                                "start": lane.requested_rows.start,
                                "end": lane.requested_rows.end,
                            },
                            "covered_rows": {
                                "start": lane.covered_rows.start,
                                "end": lane.covered_rows.end,
                            },
                            "reads_complete": lane.reads_complete(),
                        })
                    })
                })
                .collect::<Vec<_>>();
            self.record_manual_diagnostic_event(
                "alignment_publication",
                "uncovered_vertical_candidate_retained",
                serde_json::json!({
                    "lane_count": uncovered_vertical_lanes,
                    "capture_elapsed_us": capture_elapsed.as_micros(),
                    "reason": "requested vertical rows have no prepared intersection; retaining the coherent committed epoch",
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                    "candidate_lanes": lane_diagnostics,
                    "scroll_cache_authorities": self.manual_active_scroll_cache_diagnostic_state(),
                }),
            );
            return;
        }
        let target = AlignmentSceneTargetIdentity::from_panel_frames(
            self.workspace_layout.revision,
            [self.surface_config.width, self.surface_config.height],
            scale_factor,
            &candidate_epoch.panel_frames,
        );
        let retry_was_pending = self.alignment_gpu_publication_retry.is_some();
        let attempt = if retry_was_pending {
            let snapshot = match self.alignment_scene_ledger.progress() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.alignment_gpu_publication_retry = None;
                    self.data_status =
                        format!("Could not inspect deferred alignment GPU capacity: {error}");
                    return;
                }
            };
            begin_alignment_gpu_publication_attempt(
                &mut self.alignment_scene_publication_authority,
                &mut self.alignment_gpu_publication_retry,
                &target,
                &snapshot,
            )
        } else {
            Some(
                self.alignment_scene_publication_authority
                    .begin_attempt(&target),
            )
        };
        let Some(attempt) = attempt else {
            self.record_manual_diagnostic_event(
                "alignment_publication",
                "same_target_retry_coalesced",
                serde_json::json!({
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "publication_attempt_sequence": self.alignment_scene_publication_authority.attempt_sequence(),
                    "capture_elapsed_us": capture_elapsed.as_micros(),
                    "panel_frames": candidate_epoch.panel_frames.len(),
                    "lane_sources": candidate_epoch.lane_sources.len(),
                }),
            );
            tracing::trace!(
                target_revision = self
                    .alignment_scene_publication_authority
                    .desired_target_revision(),
                publication_attempt_sequence = self
                    .alignment_scene_publication_authority
                    .attempt_sequence(),
                "coalescing same-target alignment publication behind unchanged ledger pressure"
            );
            return;
        };
        if self
            .alignment_capacity_reclaim
            .as_ref()
            .is_some_and(|cycle| {
                cycle.target_revision != attempt.target_revision
                    || cycle.attempt_sequence != attempt.sequence
            })
        {
            self.alignment_capacity_reclaim = None;
        }
        let target_revision = attempt.target_revision;
        let publication_attempt_sequence = attempt.sequence;
        self.record_manual_diagnostic_event(
            "alignment_publication",
            "atomic_prepare_started",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "target_changed": attempt.target_changed,
                "retry_was_pending": retry_was_pending,
                "capture_elapsed_us": capture_elapsed.as_micros(),
                "panel_frames": candidate_epoch.panel_frames.len(),
                "lane_sources": candidate_epoch.lane_sources.len(),
                "workspace_layout_revision": self.workspace_layout.revision,
                "surface": [self.surface_config.width, self.surface_config.height],
                "scale_factor": scale_factor,
            }),
        );
        tracing::trace!(
            target_revision,
            publication_attempt_sequence,
            target_changed = attempt.target_changed,
            "preparing atomic alignment publication"
        );
        for frame in candidate_epoch.panel_frames.values_mut() {
            frame.camera_revision = target_revision;
        }
        let lane_authorities = candidate_epoch
            .lane_sources
            .iter()
            .map(|source| AlignmentCommittedLaneAuthority {
                panel_id: source.panel_id,
                track_id: source.track_id,
                target_revision,
                source_request_token: source.semantic_request_token,
                desired_request_token: source.desired_request_token,
                source_camera: source.camera,
                individual_reads: source.individual_reads,
                semantic_target_complete: source.semantic_target_complete,
                reference_ready: source.reference_ready,
                vertical_residency: source.vertical_residency,
            })
            .collect::<Vec<_>>();
        // Retain a small identity-only copy for exact transaction settlement even when the
        // renderer publication consumes the candidate bundle vectors below.
        let navigation_candidates = lane_authorities.clone();
        if retry_was_pending {
            self.resume_alignment_navigation_transactions_after_gpu_progress(
                &navigation_candidates,
            );
        }

        let scale = scale_factor as f32;
        let screen_size_points = [
            self.surface_config.width as f32 / scale.max(f32::EPSILON),
            self.surface_config.height as f32 / scale.max(f32::EPSILON),
        ];
        let (mut owned_reads, mut owned_summaries, unsupported_interpretations) = self
            .collect_alignment_gpu_requests(
                &candidate_epoch.panel_frames,
                &lane_authorities,
                publication_attempt_sequence,
                scale,
            );
        owned_reads.sort_by_key(|request| (request.panel_id, request.track_id));
        owned_summaries.sort_by_key(|request| (request.panel_id, request.track_id));
        if unsupported_interpretations > 0 {
            self.data_status = format!(
                "{unsupported_interpretations} coverage lane{} unavailable: this allele interpretation is not yet supported by the unified GPU renderer",
                if unsupported_interpretations == 1 {
                    " is"
                } else {
                    "s are"
                }
            );
        }

        let read_requests = owned_reads
            .iter()
            .map(|request| request.borrowed(screen_size_points))
            .collect::<Vec<_>>();
        let summary_requests = owned_summaries
            .iter()
            .map(|request| request.borrowed(publication_attempt_sequence, screen_size_points))
            .collect::<Vec<_>>();
        self.record_manual_diagnostic_event(
            "alignment_publication",
            "gpu_requests_collected",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "read_requests": read_requests.len(),
                "summary_requests": summary_requests.len(),
                "unsupported_interpretations": unsupported_interpretations,
                "lane_authorities": lane_authorities.len(),
            }),
        );
        let read_authorities = owned_reads
            .iter()
            .filter_map(|request| {
                let source = lane_authorities.iter().find(|authority| {
                    authority.panel_id == request.panel_id && authority.track_id == request.track_id
                })?;
                request.live_clip.map(|clip| AlignmentNativeReadAuthority {
                    panel_id: request.panel_id,
                    track_id: request.track_id,
                    target_revision,
                    source_request_token: source.source_request_token,
                    desired_request_token: source.desired_request_token,
                    source_camera: source.source_camera,
                    scene_revision: request.scene.revision(),
                    clip,
                })
            })
            .collect::<Vec<_>>();

        if read_requests.is_empty() && summary_requests.is_empty() {
            let unchanged_authoritative_empty = !attempt.target_changed
                && lane_authorities.is_empty()
                && self
                    .alignment_presented_epoch
                    .as_ref()
                    .and_then(|epoch| epoch.gpu_bundle.as_ref())
                    .is_some_and(|bundle| {
                        bundle.reads.is_none()
                            && bundle.summaries.is_none()
                            && bundle.lane_authorities.is_empty()
                    });
            if unchanged_authoritative_empty {
                self.record_manual_diagnostic_event(
                    "alignment_publication",
                    "unchanged_empty_epoch_retained",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "reason": "the already committed target has no alignment pixel demand",
                    }),
                );
                return;
            }
            let bundle = AlignmentGpuFrameBundle {
                target_revision,
                publication_attempt_sequence,
                prepared_authority: AlignmentGpuPreparedAuthority {
                    read_pages: Arc::from([]),
                    summary_pages: Arc::from([]),
                },
                reads: None,
                summaries: None,
                lane_authorities,
                read_authorities: Vec::new(),
            };
            assert!(
                self.alignment_scene_publication_authority
                    .commit_attempt(&attempt),
                "alignment publication attempt was superseded during synchronous preparation"
            );
            self.commit_alignment_navigation_transactions(&bundle);
            self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
            self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                candidate_epoch.panel_frames,
                bundle,
            ));
            self.resolve_pending_alignment_scene_selections();
            self.invalidate_stale_alignment_scroll_cache();
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate.reset();
            self.record_manual_diagnostic_event(
                "alignment_publication",
                "empty_epoch_committed",
                serde_json::json!({
                    "target_revision": target_revision,
                    "publication_attempt_sequence": publication_attempt_sequence,
                    "panel_frames": self.alignment_presented_epoch.as_ref().map_or(0, |epoch| epoch.panel_frames.len()),
                    "reason": "no native read or summary GPU requests were captured",
                }),
            );
            return;
        }

        let current_prepared_authority = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .map(|bundle| bundle.prepared_authority.clone());
        match self
            .capture_alignment_gpu_bundle_from_resident_content(&read_requests, &summary_requests)
        {
            AlignmentGpuCameraCaptureOutcome::Captured { reads, summaries } => {
                let bundle = AlignmentGpuFrameBundle {
                    target_revision,
                    publication_attempt_sequence,
                    prepared_authority: current_prepared_authority.expect(
                        "camera-only capture requires the previously committed content authority",
                    ),
                    reads,
                    summaries,
                    lane_authorities,
                    read_authorities,
                };
                assert!(
                    self.alignment_scene_publication_authority
                        .commit_attempt(&attempt),
                    "alignment publication attempt was superseded during synchronous preparation"
                );
                self.commit_alignment_navigation_transactions(&bundle);
                self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
                self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                    candidate_epoch.panel_frames,
                    bundle,
                ));
                self.resolve_pending_alignment_scene_selections();
                self.invalidate_stale_alignment_scroll_cache();
                // Camera capture preserves the immutable prepared authority. Keep any in-flight
                // cursor and its pins instead of restarting complete-scene discovery on every
                // pan/scroll frame.
                if self.alignment_gpu_prefetch.pending() {
                    self.window.request_redraw();
                }
                let filter_authorities =
                    self.alignment_filter_authorities_for_trace(&navigation_candidates);
                self.record_manual_diagnostic_event(
                    "alignment_publication",
                    "camera_only_capture_committed",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                        "filter_authorities": filter_authorities,
                    }),
                );
                return;
            }
            AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired => {
                self.record_manual_diagnostic_event(
                    "alignment_publication",
                    "content_replacement_required",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    }),
                );
            }
            AlignmentGpuCameraCaptureOutcome::Deferred(blocker) => {
                // Camera-ring or other non-content pressure is not evidence that the source or
                // immutable scene failed. Retain the coherent prior epoch for a later retry.
                tracing::debug!(?blocker, "deferring alignment camera-only capture");
                self.record_manual_diagnostic_event(
                    "alignment_publication",
                    "camera_capture_deferred",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "blocker": format!("{blocker:?}"),
                    }),
                );
                self.wait_alignment_navigation_transactions_for_gpu(&navigation_candidates);
                self.defer_alignment_gpu_publication(&attempt, blocker);
                return;
            }
            AlignmentGpuCameraCaptureOutcome::Terminal(error) => {
                self.alignment_gpu_prefetch.clear();
                self.fail_alignment_navigation_gpu_candidates(&navigation_candidates, &error);
                self.data_status =
                    format!("Could not capture the current alignment camera presentation: {error}");
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "camera_capture_terminal",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    })),
                );
                return;
            }
        }

        let protected_read_pages = match AlignmentGpuRenderer::mandatory_page_keys(&read_requests) {
            Ok(keys) => keys,
            Err(error) => {
                self.data_status =
                    format!("Could not inventory mandatory alignment GPU pages: {error}");
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "read_page_inventory_failed",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error.to_string(),
                        "read_requests": read_requests.len(),
                    })),
                );
                return;
            }
        };
        let protected_summary_pages =
            match AlignmentSummaryGpuRenderer::mandatory_page_keys(&summary_requests) {
                Ok(keys) => keys,
                Err(error) => {
                    self.data_status =
                        format!("Could not inventory mandatory summary GPU pages: {error}");
                    let message = self.data_status.clone();
                    self.freeze_manual_diagnostic_trace(
                        "alignment_publication",
                        "summary_page_inventory_failed",
                        &message,
                        object_fields(serde_json::json!({
                            "target_revision": target_revision,
                            "publication_attempt_sequence": publication_attempt_sequence,
                            "error": error.to_string(),
                            "summary_requests": summary_requests.len(),
                        })),
                    );
                    return;
                }
            };
        self.record_manual_diagnostic_event(
            "alignment_publication",
            "mandatory_pages_inventoried",
            serde_json::json!({
                "target_revision": target_revision,
                "publication_attempt_sequence": publication_attempt_sequence,
                "read_pages": protected_read_pages.len(),
                "summary_pages": protected_summary_pages.len(),
            }),
        );
        let budget_bytes = match self.alignment_scene_ledger.progress() {
            Ok(snapshot) => snapshot.budget,
            Err(error) => {
                self.data_status = format!("Could not inspect alignment GPU budget: {error}");
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "ledger_inspection_failed",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "error": error.to_string(),
                    })),
                );
                return;
            }
        };
        // Camera capture already proved that immutable content changed. Old all-scene residency
        // and cursor pins are speculative, so release them before the new atomic plan reserves
        // capacity. The replacement itself will mint and publish the successor authority once.
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();

        let first = self.try_publish_alignment_gpu_bundle(
            target_revision,
            publication_attempt_sequence,
            &read_requests,
            &summary_requests,
            lane_authorities.clone(),
            read_authorities.clone(),
        );
        let published = match first {
            Ok(bundle) => Ok(bundle),
            Err(failure) if failure.blocker.is_some() => Err(failure),
            Err(failure) => match failure.pressure {
                Some(pressure) => {
                    self.alignment_last_gpu_budget_pressure = Some(pressure);
                    match self.reclaim_alignment_gpu_caches_for_foreground(
                        pressure,
                        &protected_read_pages,
                        &protected_summary_pages,
                    ) {
                        Ok(AlignmentGpuReclaimOutcome::Satisfied { .. }) => self
                            .try_publish_alignment_gpu_bundle(
                                target_revision,
                                publication_attempt_sequence,
                                &read_requests,
                                &summary_requests,
                                lane_authorities,
                                read_authorities,
                            ),
                        Ok(AlignmentGpuReclaimOutcome::Insufficient { .. }) => {
                            self.defer_alignment_gpu_publication_for_cross_owner_reclaim(
                                &attempt,
                                &navigation_candidates,
                                pressure,
                            );
                            return;
                        }
                        Ok(AlignmentGpuReclaimOutcome::Infeasible { required_bytes, .. }) => {
                            self.wait_alignment_navigation_transactions_for_gpu(
                                &navigation_candidates,
                            );
                            let message = format!(
                                "Complete alignment presentation is cache-budget limited: the minimum exact presentation exceeds the configured capacity by {} MiB",
                                required_bytes.saturating_add((1 << 20) - 1) >> 20,
                            );
                            self.settle_alignment_navigation_gpu_budget_limited(&message);
                            self.data_status = message;
                            self.mark_display_dirty("explicit display invalidation");
                            self.window.request_redraw();
                            return;
                        }
                        Err(error) => Err(AlignmentGpuBundleFailure::terminal(format!(
                            "could not coordinate alignment GPU cache reclaim: {error}"
                        ))),
                    }
                }
                None => Err(failure),
            },
        };

        let published = match published {
            Err(failure) if failure.pressure.is_some() => {
                let pressure = failure
                    .pressure
                    .expect("guarded aggregate GPU failure retains typed pressure");
                self.alignment_last_gpu_budget_pressure = Some(pressure);
                self.defer_alignment_gpu_publication_for_cross_owner_reclaim(
                    &attempt,
                    &navigation_candidates,
                    pressure,
                );
                return;
            }
            outcome => outcome,
        };

        match published {
            Ok(bundle) => {
                self.alignment_gpu_speculation_gate
                    .observe_foreground(bundle.prepared_authority.clone(), budget_bytes);
                // The semantic panel epoch and its native derivative become observable together.
                // A failed candidate leaves the previous (now stale-gated) pair untouched.
                assert!(
                    self.alignment_scene_publication_authority
                        .commit_attempt(&attempt),
                    "alignment publication attempt was superseded during synchronous preparation"
                );
                self.commit_alignment_navigation_transactions(&bundle);
                self.alignment_panel_scene_stores = candidate_epoch.panel_scene_stores;
                self.alignment_presented_epoch = Some(PresentedAlignmentEpoch::new(
                    candidate_epoch.panel_frames,
                    bundle,
                ));
                self.resolve_pending_alignment_scene_selections();
                self.invalidate_stale_alignment_scroll_cache();
                // Speculation must never delay publication of the frame that satisfied the
                // foreground camera. The render loop advances this authority only after that
                // frame has been submitted and presented.
                self.alignment_gpu_prefetch
                    .reset(!read_requests.is_empty(), !summary_requests.is_empty());
                if self.alignment_gpu_prefetch.pending() {
                    self.window.request_redraw();
                }
                let filter_authorities =
                    self.alignment_filter_authorities_for_trace(&navigation_candidates);
                self.record_manual_diagnostic_event(
                    "alignment_publication",
                    "content_replacement_committed",
                    serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                        "prefetch_reads": self.alignment_gpu_prefetch.reads,
                        "prefetch_summaries": self.alignment_gpu_prefetch.summaries,
                        "filter_authorities": filter_authorities,
                    }),
                );
            }
            Err(failure) => {
                self.alignment_gpu_prefetch.clear();
                if let Some(blocker) = failure.blocker {
                    tracing::debug!(
                        ?blocker,
                        "deferring alignment replacement until the active ledger transition settles"
                    );
                    self.wait_alignment_navigation_transactions_for_gpu(&navigation_candidates);
                    self.defer_alignment_gpu_publication(&attempt, blocker);
                    return;
                }
                self.alignment_gpu_speculation_gate
                    .suppress_for_foreground_pressure();
                let message = format!(
                    "Could not prepare one atomic alignment GPU presentation: {}",
                    failure.message
                );
                self.fail_alignment_navigation_gpu_candidates(&navigation_candidates, &message);
                self.data_status = message;
                let message = self.data_status.clone();
                self.freeze_manual_diagnostic_trace(
                    "alignment_publication",
                    "content_replacement_terminal",
                    &message,
                    object_fields(serde_json::json!({
                        "target_revision": target_revision,
                        "publication_attempt_sequence": publication_attempt_sequence,
                        "failure": failure.message,
                        "pressure": failure.pressure.map(|pressure| format!("{pressure:?}")),
                        "read_requests": read_requests.len(),
                        "summary_requests": summary_requests.len(),
                    })),
                );
            }
        }
    }

    fn capture_alignment_panel_scene_epoch(
        &mut self,
        camera_revision: u64,
    ) -> std::result::Result<CapturedAlignmentPanelSceneEpoch, AlignmentPanelSceneEpochCaptureFailure>
    {
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut captured = HashMap::new();
        let mut lane_sources = Vec::new();
        let mut critical_capture_failure = None;
        // The complete semantic panel authority remains private until native GPU publication
        // succeeds. A rejected/superseded candidate drops this clone and leaves the committed
        // store byte-for-byte unchanged.
        let mut panel_scene_stores = self.alignment_panel_scene_stores.clone();
        for panel_id in panel_ids {
            let Some((stored_panel_viewport, contig_name)) = self
                .panels
                .get(panel_id)
                .map(|panel| (panel.viewport, panel.contig_name.clone()))
            else {
                continue;
            };
            let Some(layout_width) = self
                .panel_column_rect(panel_id)
                .map(|rect| f64::from(rect.width()))
                .filter(|width| width.is_finite() && *width > 0.0)
            else {
                continue;
            };
            let Some(panel_viewport) =
                alignment_scene_viewport_for_layout_width(stored_panel_viewport, layout_width)
            else {
                let message = format!(
                    "Could not capture alignment scene: panel {} has an invalid live layout width",
                    panel_id.get()
                );
                critical_capture_failure.get_or_insert_with(|| {
                    (
                        message,
                        object_fields(serde_json::json!({
                            "panel_id": panel_id.get(),
                            "panel_contig": contig_name,
                            "stored_pixel_width": stored_panel_viewport.pixel_width(),
                            "layout_pixel_width": layout_width,
                            "camera_revision": camera_revision,
                            "workspace_layout_revision": self.workspace_layout.revision,
                            "surface": [self.surface_config.width, self.surface_config.height],
                        })),
                    )
                });
                continue;
            };
            let mut lane_captures = Vec::new();
            let mut panel_lane_sources = Vec::new();
            let mut capture_error = None;
            for track in &self.alignment_tracks {
                let Some(projection) = track.projection(panel_id) else {
                    continue;
                };
                if projection.index_only_overview || projection.scene_owner.is_none() {
                    continue;
                }
                let Some(metadata) = track.scene_metadata.as_ref() else {
                    continue;
                };
                let Some(contig) = metadata.dictionary.id_by_name(&contig_name) else {
                    continue;
                };
                let Some(contig_length) =
                    metadata.dictionary.get(contig).map(|entry| entry.length())
                else {
                    continue;
                };
                let reference_backed_alleles =
                    projection.scene_contract.as_ref().is_some_and(|contract| {
                        contract.summaries.allele_detail != AlleleSummaryDetail::None
                            && contract.presentation.reference.is_some()
                    });
                match self.alignment_scene_lane_capture_for_panel_track(
                    panel_id,
                    track.config.id,
                    contig,
                    contig_length,
                    reference_backed_alleles,
                ) {
                    Ok(Some(mut capture)) => {
                        let desired_token = projection.navigation.request_token();
                        let staged_token = projection.scene_frame_request_token;
                        if let (
                            Some(frame),
                            Some(semantic_request_token),
                            Some(desired_request_token),
                        ) = (projection.scene_frame.as_ref(), staged_token, desired_token)
                        {
                            let exact_staging = alignment_scene_staging_frame_is_current(
                                desired_token,
                                staged_token,
                                projection.navigation.desired_camera(),
                                frame.camera,
                                capture.preparation_camera,
                            );
                            // Semantic compatibility and visible page readiness are independent.
                            // A progressive snapshot remains the truthful source for the new
                            // camera even when its newly requested row block has not arrived yet;
                            // the panel frame records the covered row intersection explicitly.
                            let snapshot_contract_current =
                                frame.snapshot.as_ref().is_some_and(|snapshot| {
                                    projection.scene_contract.as_ref().is_some_and(|contract| {
                                        alignment_scene_snapshot_satisfies_contract(
                                            snapshot, contract,
                                        )
                                    })
                                });
                            let snapshot = snapshot_contract_current
                                .then(|| frame.snapshot.as_ref().map(Arc::clone))
                                .flatten();
                            let row_activity =
                                projection.scene_contract.as_ref().and_then(|contract| {
                                    let snapshot = frame.snapshot.as_ref()?;
                                    let profile = frame.row_activity.as_ref()?;
                                    alignment_row_activity_satisfies_contract(
                                        profile,
                                        snapshot,
                                        capture.preparation_camera.interval(),
                                        contract,
                                    )
                                    .then(|| Arc::clone(profile))
                                });
                            capture = capture.with_row_activity(row_activity);
                            // An old worker camera may lend its immutable, contract-matching
                            // snapshot and genomic row profile to the live camera. Its completion
                            // status remains Pending until the exact desired request-and-camera
                            // pair publishes; missing visible row pages remain explicit coverage,
                            // not a reason to revoke the scrollbar's independent row authority.
                            if exact_staging || snapshot.is_some() {
                                panel_lane_sources.push(CapturedAlignmentLaneSource {
                                    panel_id,
                                    track_id: track.config.id,
                                    semantic_request_token,
                                    desired_request_token,
                                    camera: capture.preparation_camera,
                                    individual_reads: if exact_staging {
                                        frame.individual_reads
                                    } else {
                                        AlignmentIndividualReadCapability::Pending
                                    },
                                    semantic_target_complete: exact_staging
                                        && (frame.visible_complete()
                                            || frame.individual_reads_refused_for_camera()),
                                    reference_ready: exact_staging
                                        && frame.reference_requirement.is_ready(),
                                    vertical_residency: if exact_staging {
                                        frame.vertical_residency
                                    } else {
                                        AlignmentVerticalResidency::ViewportBounded
                                    },
                                    camera_sequence: frame.camera_sequence,
                                    snapshot,
                                    scene_contracts_current: snapshot_contract_current
                                        && (exact_staging && frame.scene_contracts_current
                                            || !exact_staging),
                                });
                            }
                        }
                        lane_captures.push(capture);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        capture_error = Some(error.to_string());
                        break;
                    }
                }
            }
            if let Some(error) = capture_error {
                self.data_status = format!("Could not resolve alignment scene lanes: {error}");
                critical_capture_failure.get_or_insert_with(|| {
                    (
                        self.data_status.clone(),
                        object_fields(serde_json::json!({
                            "panel_id": panel_id.get(),
                            "panel_contig": contig_name,
                            "panel_viewport": {
                                "start": panel_viewport.interval().start(),
                                "end": panel_viewport.interval().end(),
                                "exact_start": panel_viewport.exact_start(),
                                "span": panel_viewport.span(),
                                "pixel_width": panel_viewport.pixel_width(),
                            },
                            "camera_revision": camera_revision,
                            "workspace_layout_revision": self.workspace_layout.revision,
                            "surface": [self.surface_config.width, self.surface_config.height],
                            "lane_capture_count": lane_captures.len(),
                        })),
                    )
                });
                continue;
            }
            let track_ids = lane_captures
                .iter()
                .map(|capture| capture.track_id)
                .collect::<Vec<_>>();
            let coherence_groups = [ALIGNMENT_SASHIMI_RAW_GROUP, ALIGNMENT_SASHIMI_CPM_GROUP]
                .into_iter()
                .filter_map(|group_id| {
                    let members = lane_captures
                        .iter()
                        .filter(|capture| {
                            capture.derived_lanes.iter().any(|lane| {
                                matches!(
                                    lane.options,
                                    AlignmentDerivedLaneOptions::Sashimi(
                                        AlignmentSashimiLaneOptions {
                                            scale: AlignmentSashimiScalePolicy::Shared(id),
                                            ..
                                        }
                                    ) if id == group_id
                                )
                            })
                        })
                        .map(|capture| capture.track_id)
                        .collect::<Vec<_>>();
                    (!members.is_empty()).then(|| {
                        AlignmentCoherenceGroupSpec::new(
                            group_id,
                            AlignmentCoherenceGroupKind::SashimiScale,
                            members,
                        )
                    })
                })
                .collect::<std::result::Result<Vec<_>, _>>();
            let coherence_groups = match coherence_groups {
                Ok(groups) => groups,
                Err(error) => {
                    self.data_status =
                        format!("Could not resolve alignment coherence groups: {error}");
                    continue;
                }
            };
            let store = panel_scene_stores.entry(panel_id).or_default();
            store.replace_visible_tracks(track_ids.iter().copied());
            if let Err(error) = store.set_coherence_groups(coherence_groups) {
                self.data_status = format!("Could not set alignment coherence groups: {error}");
                continue;
            }
            let mut admitted_panel_sources = Vec::new();
            for source in panel_lane_sources {
                let track_id = source.track_id;
                if source.snapshot.is_none() {
                    if let Err(error) = store.clear_lane(track_id) {
                        self.data_status = format!("Could not clear alignment scene: {error}");
                        continue;
                    }
                    admitted_panel_sources.push(source);
                    continue;
                }
                let publication = AlignmentPanelLanePublication::new(
                    source.camera_sequence,
                    source.snapshot.clone(),
                    source.scene_contracts_current,
                );
                match store.admit(track_id, publication) {
                    Ok(AlignmentPanelSceneAdmission::Stale) => {}
                    Ok(_) => admitted_panel_sources.push(source),
                    Err(error) => {
                        self.data_status = format!("Could not admit alignment scene: {error}");
                    }
                }
            }
            let diagnostic_lanes = self.manual_diagnostic_trace_is_recording().then(|| {
                lane_captures
                    .iter()
                    .map(|capture| {
                        let preparation = capture.preparation_camera.viewport();
                        serde_json::json!({
                            "track_id": capture.track_id.get(),
                            "preparation_viewport": {
                                "start": preparation.interval().start(),
                                "end": preparation.interval().end(),
                                "exact_start": preparation.exact_start(),
                                "span": preparation.span(),
                                "pixel_width": preparation.pixel_width(),
                            },
                            "visible_rows": {
                                "start": capture.preparation_camera.visible_rows().start,
                                "end": capture.preparation_camera.visible_rows().end,
                            },
                            "read_camera": capture.read_camera.map(|camera| {
                                let viewport = camera.viewport();
                                serde_json::json!({
                                    "start": viewport.interval().start(),
                                    "end": viewport.interval().end(),
                                    "exact_start": viewport.exact_start(),
                                    "span": viewport.span(),
                                    "pixel_width": viewport.pixel_width(),
                                    "clip": format!("{:?}", camera.clip()),
                                })
                            }),
                            "derived_lane_count": capture.derived_lanes.len(),
                        })
                    })
                    .collect::<Vec<_>>()
            });
            match store.capture(panel_id, camera_revision, panel_viewport, lane_captures) {
                Ok(frame) => {
                    // A worker candidate can be temporarily absent while the immutable scene
                    // already committed for this lane still overlaps the live camera. Preserve
                    // that truthful overlap and recapture it with the live camera; background
                    // completion remains pending and may replace it later. Without this bridge,
                    // an ordinary source checkpoint becomes an empty GPU epoch and blanks the
                    // complete lane despite reusable scene authority still being resident.
                    for lane in frame.lanes.iter() {
                        if admitted_panel_sources
                            .iter()
                            .any(|source| source.track_id == lane.track_id)
                            || !retained_alignment_scene_can_present(
                                lane.scene_contracts_current,
                                lane.snapshot.is_some(),
                                lane.covered_interval,
                                lane.preparation_camera.interval(),
                            )
                        {
                            continue;
                        }
                        let desired_request_token = self
                            .alignment_tracks
                            .iter()
                            .find(|track| track.config.id == lane.track_id)
                            .and_then(|track| track.projection(panel_id))
                            .and_then(|projection| projection.navigation.request_token());
                        let Some(desired_request_token) = desired_request_token else {
                            continue;
                        };
                        let prior_source = self
                            .alignment_presented_epoch
                            .as_ref()
                            .and_then(|epoch| {
                                let prior_lane =
                                    epoch.panel_frames.get(&panel_id)?.lane(lane.track_id)?;
                                let candidate_revision = lane.snapshot.as_ref()?.revision();
                                (prior_lane.snapshot.as_ref()?.revision() == candidate_revision)
                                    .then_some(epoch)
                            })
                            .and_then(|epoch| epoch.gpu_bundle.as_ref())
                            .and_then(|bundle| {
                                bundle.lane_authorities.iter().find(|authority| {
                                    authority.panel_id == panel_id
                                        && authority.track_id == lane.track_id
                                })
                            })
                            .copied();
                        let Some(prior_source) = prior_source else {
                            continue;
                        };
                        admitted_panel_sources.push(CapturedAlignmentLaneSource {
                            panel_id,
                            track_id: lane.track_id,
                            semantic_request_token: prior_source.source_request_token,
                            desired_request_token,
                            camera: lane.preparation_camera,
                            individual_reads: AlignmentIndividualReadCapability::Pending,
                            semantic_target_complete: false,
                            reference_ready: false,
                            vertical_residency: prior_source.vertical_residency,
                            camera_sequence: lane.source_camera_sequence,
                            snapshot: lane.snapshot.clone(),
                            scene_contracts_current: true,
                        });
                    }
                    captured.insert(panel_id, frame);
                    lane_sources.extend(admitted_panel_sources);
                }
                Err(error) => {
                    self.data_status = format!("Could not capture alignment scene: {error}");
                    critical_capture_failure.get_or_insert_with(|| {
                        (
                            self.data_status.clone(),
                            object_fields(serde_json::json!({
                                "error": format!("{error:?}"),
                                "panel_id": panel_id.get(),
                                "panel_contig": contig_name,
                                "panel_viewport": {
                                    "start": panel_viewport.interval().start(),
                                    "end": panel_viewport.interval().end(),
                                    "exact_start": panel_viewport.exact_start(),
                                    "span": panel_viewport.span(),
                                    "pixel_width": panel_viewport.pixel_width(),
                                },
                                "camera_revision": camera_revision,
                                "workspace_layout_revision": self.workspace_layout.revision,
                                "surface": [self.surface_config.width, self.surface_config.height],
                                "lanes": diagnostic_lanes.unwrap_or_default(),
                            })),
                        )
                    });
                }
            }
        }
        if let Some((message, fields)) = critical_capture_failure {
            return Err(AlignmentPanelSceneEpochCaptureFailure { message, fields });
        }
        Ok(CapturedAlignmentPanelSceneEpoch {
            panel_frames: captured,
            lane_sources,
            panel_scene_stores,
        })
    }

    fn collect_alignment_gpu_requests(
        &self,
        panel_frames: &HashMap<PanelId, AlignmentPanelSceneFrame>,
        lane_authorities: &[AlignmentCommittedLaneAuthority],
        publication_attempt_sequence: u64,
        scale: f32,
    ) -> (
        Vec<OwnedAlignmentGpuRequest>,
        Vec<OwnedAlignmentSummaryGpuRequest>,
        usize,
    ) {
        let mut reads = Vec::new();
        let mut summaries = Vec::new();
        let unsupported_interpretations = 0_usize;
        for (panel_id, frame) in panel_frames {
            for lane in frame.lanes.iter() {
                let Some(source_authority) = lane_authorities.iter().find(|authority| {
                    authority.panel_id == *panel_id
                        && authority.track_id == lane.track_id
                        && authority.target_revision == frame.camera_revision
                        && authority.source_camera == lane.preparation_camera
                }) else {
                    continue;
                };
                if lane.scene_contracts_current
                    && let (Some(snapshot), Some(camera)) = (&lane.snapshot, lane.read_camera)
                    && let Some(live_clip) = lane.covered_interval.and_then(|covered| {
                        alignment_scene_covered_clip(camera.viewport(), camera.clip(), covered)
                    })
                    && let Some(scissor) = alignment_scene_gpu_scissor(
                        camera,
                        scale,
                        self.surface_config.width,
                        self.surface_config.height,
                    )
                {
                    let interaction = self
                        .alignment_tracks
                        .iter()
                        .find(|track| track.config.id == lane.track_id)
                        .and_then(|track| track.projection(*panel_id))
                        .map_or_else(AlignmentInteractionState::default, |projection| {
                            projection.scene_interaction.clone()
                        });
                    reads.push(OwnedAlignmentGpuRequest {
                        panel_id: *panel_id,
                        track_id: lane.track_id,
                        scene: Arc::clone(snapshot.reads()),
                        camera_sequence: publication_attempt_sequence,
                        camera,
                        scissor,
                        interaction,
                        live_clip: Some(live_clip),
                    });
                }

                if !lane.scene_contracts_current
                    || !lane
                        .covered_interval
                        .is_some_and(|covered| covered.overlaps(lane.preparation_camera.interval()))
                {
                    continue;
                }
                let Some(scene) = lane.snapshot.as_ref() else {
                    continue;
                };
                debug_assert_eq!(source_authority.source_camera, lane.preparation_camera);
                for capture in lane.derived_lanes.iter() {
                    let Ok(camera) = CpuSummaryCamera::from_viewport(
                        lane.preparation_camera.viewport(),
                        capture.clip,
                    ) else {
                        continue;
                    };
                    let Some(scissor) = alignment_clip_gpu_scissor(
                        capture.clip,
                        scale,
                        self.surface_config.width,
                        self.surface_config.height,
                    ) else {
                        continue;
                    };
                    if lane
                        .covered_interval
                        .and_then(|covered| {
                            alignment_scene_covered_clip(
                                lane.preparation_camera.viewport(),
                                capture.clip,
                                covered,
                            )
                        })
                        .is_none()
                    {
                        continue;
                    }
                    let (options, selected_position) = match &capture.options {
                        AlignmentDerivedLaneOptions::Coverage(options) => {
                            // Depth is reference-independent. While the optional FASTA product
                            // is pending, publish exact depth alone; the same immutable lane is
                            // upgraded with allele/substitution pages when reference arrives.
                            let mut available = *options;
                            if scene.reads().presentation().reference.is_none() {
                                available.alleles = None;
                            }
                            (
                                AlignmentSummaryGpuLaneOptions::Coverage(available),
                                options.depth.selected_position,
                            )
                        }
                        AlignmentDerivedLaneOptions::Modifications(options) => (
                            AlignmentSummaryGpuLaneOptions::Modifications(options.clone()),
                            options.selected_position,
                        ),
                        AlignmentDerivedLaneOptions::Sashimi(options) => {
                            (AlignmentSummaryGpuLaneOptions::Sashimi(*options), None)
                        }
                    };
                    let panel = self.panels.get(*panel_id);
                    let hovered_junction = panel
                        .and_then(|panel| panel.hovered_junction_arc)
                        .filter(|hover| hover.key.track_id == lane.track_id)
                        .map(|hover| AlignmentSummaryJunctionInteraction {
                            start: hover.key.start,
                            end: hover.key.end,
                            component: summary_junction_component(hover.key.component),
                        });
                    let selected_junction = panel
                        .filter(|panel| panel.selected_alignment_track == Some(lane.track_id))
                        .and_then(|panel| {
                            panel
                                .selected_junction
                                .zip(panel.selected_junction_component)
                        })
                        .map(
                            |((start, end), component)| AlignmentSummaryJunctionInteraction {
                                start,
                                end,
                                component: summary_junction_component(component),
                            },
                        );
                    summaries.push(OwnedAlignmentSummaryGpuRequest {
                        panel_id: *panel_id,
                        track_id: lane.track_id,
                        scene: Arc::clone(scene),
                        camera,
                        scissor,
                        options,
                        overlays: AlignmentSummaryFrameOverlays {
                            selected_position,
                            hovered_junction,
                            selected_junction,
                        },
                    });
                }
            }
        }
        (reads, summaries, unsupported_interpretations)
    }

    /// Attempts a camera-only capture over the immutable content authority of the currently
    /// presented epoch. Both renderer legs must succeed before either candidate becomes visible.
    fn capture_alignment_gpu_bundle_from_resident_content(
        &mut self,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
    ) -> AlignmentGpuCameraCaptureOutcome {
        let Some(previous) = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
        else {
            return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
        };
        let read_authority = match (read_requests.is_empty(), previous.reads.as_ref()) {
            (true, None) => None,
            (false, Some(frame)) => Some(frame.content_authority().clone()),
            (true, Some(_)) | (false, None) => {
                return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
            }
        };
        let summary_authority = match (summary_requests.is_empty(), previous.summaries.as_ref()) {
            (true, None) => None,
            (false, Some(frame)) => Some(frame.content_authority().clone()),
            (true, Some(_)) | (false, None) => {
                return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
            }
        };

        let reads = match read_authority {
            Some(authority) => match self.alignment_gpu_renderer.capture_camera_frame(
                &self.device,
                &self.queue,
                &authority,
                read_requests,
                &self.alignment_scene_ledger,
            ) {
                Ok((frame, stats)) => {
                    self.alignment_gpu_last_prepare_stats = stats;
                    Some(frame)
                }
                Err(AlignmentGpuCameraCaptureError::ContentReplacementRequired { .. }) => {
                    return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
                }
                Err(AlignmentGpuCameraCaptureError::FrameSlotBusy { slots }) => {
                    return AlignmentGpuCameraCaptureOutcome::Terminal(format!(
                        "internal read-camera slot invariant failed: all {slots} provisioned slots were retained"
                    ));
                }
                Err(error) => {
                    return if let Some(blocker) = alignment_read_camera_capture_blocker(&error) {
                        AlignmentGpuCameraCaptureOutcome::Deferred(blocker)
                    } else {
                        AlignmentGpuCameraCaptureOutcome::Terminal(error.to_string())
                    };
                }
            },
            None => None,
        };
        let summaries = match summary_authority {
            Some(authority) => match self.alignment_summary_gpu_renderer.capture_camera_frame(
                &self.device,
                &self.queue,
                &authority,
                summary_requests,
                &self.alignment_scene_ledger,
            ) {
                Ok((frame, stats)) => {
                    self.alignment_summary_gpu_last_prepare_stats = stats;
                    Some(frame)
                }
                Err(AlignmentSummaryGpuCameraCaptureError::ContentReplacementRequired {
                    ..
                }) => {
                    return AlignmentGpuCameraCaptureOutcome::ContentReplacementRequired;
                }
                Err(AlignmentSummaryGpuCameraCaptureError::FrameSlotBusy { slots }) => {
                    return AlignmentGpuCameraCaptureOutcome::Terminal(format!(
                        "internal summary-camera slot invariant failed: all {slots} provisioned slots were retained"
                    ));
                }
                Err(error) => {
                    return if let Some(blocker) = alignment_summary_camera_capture_blocker(&error) {
                        AlignmentGpuCameraCaptureOutcome::Deferred(blocker)
                    } else {
                        AlignmentGpuCameraCaptureOutcome::Terminal(error.to_string())
                    };
                }
            },
            None => None,
        };
        AlignmentGpuCameraCaptureOutcome::Captured { reads, summaries }
    }

    /// Detaches the complete old semantic desktop authority before aggregate admission. Nothing
    /// outside this synchronous transaction can observe the temporary empty slots. The returned
    /// token either rejoins the exact old epoch on rollback or is consumed alongside the GPU
    /// retirement at successful bundle settlement.
    fn detach_alignment_semantic_epoch_retirement(
        &mut self,
    ) -> std::result::Result<Option<AlignmentSemanticEpochRetirement>, AllocationBundleError> {
        let Some(epoch) = self.alignment_presented_epoch.as_mut() else {
            debug_assert!(self.alignment_panel_scene_stores.is_empty());
            return Ok(None);
        };
        let panel_frames = std::mem::take(&mut epoch.panel_frames);
        let panel_scene_stores = std::mem::take(&mut self.alignment_panel_scene_stores);
        let planned = build_alignment_semantic_retirement_claim_set(
            &self.alignment_scene_ledger,
            panel_frames
                .values()
                .flat_map(|frame| frame.lanes.iter())
                .filter_map(|lane| lane.snapshot.as_ref())
                .chain(
                    panel_scene_stores
                        .values()
                        .flat_map(AlignmentPanelSceneStore::snapshots),
                ),
        );
        let (claim_set, stats) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                epoch.panel_frames = panel_frames;
                self.alignment_panel_scene_stores = panel_scene_stores;
                return Err(error);
            }
        };
        tracing::debug!(
            snapshot_occurrences = stats.snapshot_occurrences,
            unique_snapshots = stats.unique_snapshots,
            exclusively_owned_snapshots = stats.exclusively_owned_snapshots,
            exclusively_owned_read_scenes = stats.exclusively_owned_read_scenes,
            exclusively_owned_evidence_roots = stats.exclusively_owned_evidence_roots,
            exclusively_owned_row_atlases = stats.exclusively_owned_row_atlases,
            exclusively_owned_summary_roots = stats.exclusively_owned_summary_roots,
            exclusively_owned_read_pages = stats.exclusively_owned_read_pages,
            exclusively_owned_summary_pages = stats.exclusively_owned_summary_pages,
            owned_references_examined = stats.owned_references_examined,
            claimed_semantic_allocations = stats.claimed_allocations,
            "detached semantic component of alignment epoch retirement"
        );
        Ok(Some(AlignmentSemanticEpochRetirement {
            claim_set,
            stats,
            panel_frames,
            panel_scene_stores,
        }))
    }

    fn restore_alignment_semantic_epoch_retirement(
        &mut self,
        retirement: AlignmentSemanticEpochRetirement,
    ) -> std::result::Result<(), String> {
        let Some(epoch) = self.alignment_presented_epoch.as_mut() else {
            return Err("semantic rollback has no prior presented epoch".into());
        };
        if !epoch.panel_frames.is_empty() || !self.alignment_panel_scene_stores.is_empty() {
            return Err("semantic rollback would overwrite an occupied presentation slot".into());
        }
        tracing::debug!(
            claimed_semantic_allocations = retirement.stats.claimed_allocations,
            "restoring detached semantic alignment epoch"
        );
        epoch.panel_frames = retirement.panel_frames;
        self.alignment_panel_scene_stores = retirement.panel_scene_stores;
        Ok(())
    }

    fn try_publish_alignment_gpu_bundle(
        &mut self,
        target_revision: u64,
        publication_attempt_sequence: u64,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        lane_authorities: Vec<AlignmentCommittedLaneAuthority>,
        read_authorities: Vec<AlignmentNativeReadAuthority>,
    ) -> std::result::Result<AlignmentGpuFrameBundle, AlignmentGpuBundleFailure> {
        let progress = self
            .alignment_scene_ledger
            .progress()
            .map_err(|error| AlignmentGpuBundleFailure::terminal(error.to_string()))?;
        let mut next_slot = AllocationBundleSlot::new(0);
        let read_plan = if read_requests.is_empty() {
            None
        } else {
            let plan = self
                .alignment_gpu_renderer
                .plan_replacement(read_requests, &progress, next_slot)
                .map_err(AlignmentGpuBundleFailure::read_replacement)?;
            next_slot = plan.next_bundle_slot();
            Some(plan)
        };
        let summary_plan = if summary_requests.is_empty() {
            None
        } else {
            let plan = self
                .alignment_summary_gpu_renderer
                .plan_replacement(summary_requests, &progress, next_slot)
                .map_err(AlignmentGpuBundleFailure::summary_replacement)?;
            Some(plan)
        };
        let prepared_authority = AlignmentGpuPreparedAuthority {
            read_pages: read_plan.as_ref().map_or_else(
                || Arc::from([]),
                |plan| Arc::clone(plan.prepared_page_keys()),
            ),
            summary_pages: summary_plan.as_ref().map_or_else(
                || Arc::from([]),
                |plan| Arc::clone(plan.prepared_page_keys()),
            ),
        };
        let mut demands = Vec::new();
        if let Some(plan) = &read_plan {
            demands.extend_from_slice(plan.demands());
        }
        if let Some(plan) = &summary_plan {
            demands.extend_from_slice(plan.demands());
        }

        let semantic_retirement = self
            .detach_alignment_semantic_epoch_retirement()
            .map_err(AlignmentGpuBundleFailure::bundle)?;

        let mut prior_bundle = self
            .alignment_presented_epoch
            .as_mut()
            .and_then(|epoch| epoch.gpu_bundle.take());
        let mut prior_reads = prior_bundle.as_mut().and_then(|bundle| bundle.reads.take());
        let mut prior_summaries = prior_bundle
            .as_mut()
            .and_then(|bundle| bundle.summaries.take())
            .into_iter()
            .collect::<Vec<_>>();

        let read_retirement = if let Some(plan) = &read_plan {
            match self.alignment_gpu_renderer.detach_replacement_retirement(
                &self.alignment_scene_ledger,
                plan,
                &mut prior_reads,
            ) {
                Ok(retirement) => Some(retirement),
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        None,
                        None,
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let summary_retirement = if let Some(plan) = &summary_plan {
            match self
                .alignment_summary_gpu_renderer
                .detach_replacement_retirement(
                    &self.alignment_scene_ledger,
                    plan,
                    &mut prior_summaries,
                ) {
                Ok(retirement) => Some(retirement),
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        None,
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };
        let claim_set = {
            let mut claim_sets = Vec::with_capacity(3);
            if let Some(retirement) = &semantic_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            if let Some(retirement) = &read_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            if let Some(retirement) = &summary_retirement {
                claim_sets.push(retirement.retirement_claim_set());
            }
            match self
                .alignment_scene_ledger
                .combine_retirement_claim_sets(claim_sets)
            {
                Ok(claim_set) => claim_set,
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        summary_retirement,
                    )?;
                    return Err(AlignmentGpuBundleFailure::bundle(error));
                }
            }
        };
        let reservation =
            match self
                .alignment_scene_ledger
                .reserve_replacement_bundle(AllocationBundleSpec::new(
                    progress.policy_revision,
                    demands,
                    claim_set.clone(),
                )) {
                Ok(reservation) => reservation,
                Err(error) => {
                    self.restore_alignment_gpu_replacement(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        semantic_retirement,
                        read_retirement,
                        summary_retirement,
                    )?;
                    return Err(AlignmentGpuBundleFailure::bundle(error));
                }
            };
        let mut composite = Some(AlignmentGpuCompositeRetirement {
            claim_set,
            semantic: semantic_retirement,
            reads: read_retirement,
            summaries: summary_retirement,
        });

        let staged_reads = if let Some(plan) = read_plan {
            match self.alignment_gpu_renderer.stage_replacement(
                &self.device,
                &self.queue,
                read_requests,
                plan,
                &reservation,
                &self.alignment_scene_ledger,
                publication_attempt_sequence,
            ) {
                Ok(staged) => Some(staged),
                Err(error) => {
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let staged_summaries = if let Some(plan) = summary_plan {
            match self.alignment_summary_gpu_renderer.stage_replacement(
                &self.device,
                &self.queue,
                summary_requests,
                plan,
                &reservation,
                &self.alignment_scene_ledger,
                publication_attempt_sequence,
            ) {
                Ok(staged) => Some(staged),
                Err(error) => {
                    drop(staged_reads);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };
        let ready_reads = if let Some(staged) = staged_reads {
            match self
                .alignment_gpu_renderer
                .preflight_replacement_publication(staged)
            {
                Ok(ready) => Some(ready),
                Err(error) => {
                    drop(staged_summaries);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::read_replacement(error));
                }
            }
        } else {
            None
        };
        let ready_summaries = if let Some(staged) = staged_summaries {
            match self
                .alignment_summary_gpu_renderer
                .preflight_replacement_publication(staged)
            {
                Ok(ready) => Some(ready),
                Err(error) => {
                    drop(ready_reads);
                    drop(reservation);
                    self.restore_alignment_gpu_composite(
                        prior_bundle,
                        prior_reads,
                        prior_summaries,
                        composite.take(),
                    )?;
                    return Err(AlignmentGpuBundleFailure::summary_replacement(error));
                }
            }
        } else {
            None
        };

        let mut actuals = Vec::<AllocationBundleActual>::new();
        if let Some(ready) = &ready_reads {
            actuals.extend_from_slice(ready.actuals());
        }
        if let Some(ready) = &ready_summaries {
            actuals.extend_from_slice(ready.actuals());
        }
        let commit = match reservation.commit(actuals) {
            Ok(commit) => commit,
            Err(error) => {
                drop(ready_summaries);
                drop(ready_reads);
                self.restore_alignment_gpu_composite(
                    prior_bundle,
                    prior_reads,
                    prior_summaries,
                    composite.take(),
                )?;
                return Err(AlignmentGpuBundleFailure::bundle(error));
            }
        };
        let mut settled_leases = match settle_alignment_gpu_bundle(
            commit,
            composite
                .take()
                .expect("atomic candidate must own its retirement token"),
        ) {
            Ok(leases) => leases,
            Err((error, retirement)) => {
                drop(ready_summaries);
                drop(ready_reads);
                self.restore_alignment_gpu_composite(
                    prior_bundle,
                    prior_reads,
                    prior_summaries,
                    Some(retirement),
                )?;
                return Err(AlignmentGpuBundleFailure::bundle(error));
            }
        };

        // Every recoverable operation is now complete. Retirement, lease binding, and both
        // renderer cache mutations form an infallible suffix; the Viewer bundle is assigned by
        // the caller only after both publications finish.
        let publishable_reads = ready_reads.map(|ready| {
            self.alignment_gpu_renderer
                .bind_replacement_leases(ready, &mut settled_leases)
        });
        let publishable_summaries = ready_summaries.map(|ready| {
            self.alignment_summary_gpu_renderer
                .bind_replacement_leases(ready, &mut settled_leases)
        });
        assert!(
            settled_leases.is_empty(),
            "atomic alignment publication left an unclaimed bundle lease"
        );
        let reads = publishable_reads.map(|publishable| {
            let (frame, stats, _) = self.alignment_gpu_renderer.publish_replacement(publishable);
            self.alignment_gpu_last_prepare_stats = stats;
            frame
        });
        let summaries = publishable_summaries.map(|publishable| {
            let (frame, stats, _) = self
                .alignment_summary_gpu_renderer
                .publish_replacement(publishable);
            self.alignment_summary_gpu_last_prepare_stats = stats;
            frame
        });
        drop(prior_bundle);
        Ok(AlignmentGpuFrameBundle {
            target_revision,
            publication_attempt_sequence,
            prepared_authority,
            reads,
            summaries,
            lane_authorities,
            read_authorities,
        })
    }

    fn restore_alignment_gpu_composite(
        &mut self,
        prior_bundle: Option<AlignmentGpuFrameBundle>,
        prior_reads: Option<AlignmentGpuFrame>,
        prior_summaries: Vec<AlignmentSummaryGpuFrame>,
        composite: Option<AlignmentGpuCompositeRetirement>,
    ) -> std::result::Result<(), AlignmentGpuBundleFailure> {
        let (semantic_retirement, read_retirement, summary_retirement) = composite
            .map_or((None, None, None), |composite| {
                (composite.semantic, composite.reads, composite.summaries)
            });
        self.restore_alignment_gpu_replacement(
            prior_bundle,
            prior_reads,
            prior_summaries,
            semantic_retirement,
            read_retirement,
            summary_retirement,
        )
    }

    fn restore_alignment_gpu_replacement(
        &mut self,
        mut prior_bundle: Option<AlignmentGpuFrameBundle>,
        mut prior_reads: Option<AlignmentGpuFrame>,
        mut prior_summaries: Vec<AlignmentSummaryGpuFrame>,
        semantic_retirement: Option<AlignmentSemanticEpochRetirement>,
        read_retirement: Option<AlignmentGpuRetirement>,
        summary_retirement: Option<AlignmentSummaryGpuRetirement>,
    ) -> std::result::Result<(), AlignmentGpuBundleFailure> {
        let restored = (|| -> std::result::Result<(), String> {
            if let Some(retirement) = summary_retirement {
                prior_summaries = self
                    .alignment_summary_gpu_renderer
                    .restore_replacement_retirement(retirement)
                    .map_err(|error| error.to_string())?;
            }
            if let Some(retirement) = read_retirement {
                prior_reads = self
                    .alignment_gpu_renderer
                    .restore_replacement_retirement(retirement)
                    .map_err(|error| error.to_string())?;
            }
            if prior_summaries.len() > 1 {
                return Err(format!(
                    "rollback recovered {} summary frames for a single atomic bundle",
                    prior_summaries.len()
                ));
            }
            if prior_bundle.is_none() && (prior_reads.is_some() || !prior_summaries.is_empty()) {
                return Err("rollback recovered frames without prior bundle metadata".into());
            }
            if let Some(bundle) = prior_bundle.as_mut() {
                bundle.reads = prior_reads.take();
                bundle.summaries = prior_summaries.pop();
            }
            match (self.alignment_presented_epoch.as_mut(), prior_bundle.take()) {
                (Some(epoch), bundle) => epoch.gpu_bundle = bundle,
                (None, None) => {}
                (None, Some(_)) => {
                    return Err("rollback recovered bundle metadata without its panel epoch".into());
                }
            }
            if let Some(retirement) = semantic_retirement {
                self.restore_alignment_semantic_epoch_retirement(retirement)?;
            }
            Ok(())
        })();
        if let Err(error) = restored {
            self.alignment_presented_epoch = None;
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_renderer.discard_device_resources();
            self.alignment_summary_gpu_renderer
                .discard_device_resources();
            return Err(AlignmentGpuBundleFailure::terminal(format!(
                "alignment GPU rollback failed closed: {error}"
            )));
        }
        Ok(())
    }

    /// Advances speculative pages for the already-published camera authority. This path never
    /// recaptures a panel epoch, replaces a foreground frame, or increments the camera revision;
    /// repeated redraw quanta therefore accumulate rather than retiring one another.
    pub(crate) fn advance_current_alignment_gpu_prefetch(&mut self) {
        if self.alignment_optional_work_must_yield() {
            // Preserve the retargetable cursors and already useful overlap, but do not request a
            // further redraw from optional work. Input/source/GPU completion will naturally wake
            // the render loop and resume warming after foreground ownership settles.
            return;
        }
        let target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            self.alignment_gpu_prefetch.clear();
            return;
        };
        let Some(bundle) = epoch.gpu_bundle.as_ref() else {
            self.alignment_gpu_prefetch.clear();
            return;
        };
        if bundle.target_revision != target_revision {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let publication_attempt_sequence = bundle.publication_attempt_sequence;
        let published_has_reads = bundle.reads.is_some();
        let published_has_summaries = bundle.summaries.is_some();

        let scale = self.window.scale_factor() as f32;
        let screen_size_points = [
            self.surface_config.width as f32 / scale.max(f32::EPSILON),
            self.surface_config.height as f32 / scale.max(f32::EPSILON),
        ];
        let (mut owned_reads, mut owned_summaries, _) = self.collect_alignment_gpu_requests(
            &epoch.panel_frames,
            &bundle.lane_authorities,
            publication_attempt_sequence,
            scale,
        );
        owned_reads.sort_by_key(|request| (request.panel_id, request.track_id));
        owned_summaries.sort_by_key(|request| (request.panel_id, request.track_id));
        let read_requests = owned_reads
            .iter()
            .map(|request| request.borrowed(screen_size_points))
            .collect::<Vec<_>>();
        let summary_requests = owned_summaries
            .iter()
            .map(|request| request.borrowed(publication_attempt_sequence, screen_size_points))
            .collect::<Vec<_>>();
        if published_has_reads != !read_requests.is_empty()
            || published_has_summaries != !summary_requests.is_empty()
        {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let read_revisions = read_requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        let summary_revisions = summary_requests
            .iter()
            .map(|request| request.scene.revision())
            .collect::<Vec<_>>();
        let published_matches = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .is_some_and(|bundle| {
                bundle.reads.as_ref().is_none_or(|frame| {
                    frame.scene_revisions.as_ref() == read_revisions.as_slice()
                        && frame
                            .camera_sequences
                            .iter()
                            .all(|sequence| *sequence == publication_attempt_sequence)
                }) && bundle.summaries.as_ref().is_none_or(|frame| {
                    frame.scene_revisions.as_ref() == summary_revisions.as_slice()
                        && frame
                            .camera_sequences
                            .iter()
                            .all(|sequence| *sequence == publication_attempt_sequence)
                })
            });
        if !published_matches {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        // This is the exact manifest minted by the content replacement and carried by every
        // compatible camera-only publication. Rebuilding it here would make every redraw scale
        // with the complete ACTB prepared scene before doing one bounded prefetch quantum.
        let authority = bundle.prepared_authority.clone();
        if self.alignment_gpu_speculation_gate.authority.as_ref() != Some(&authority) {
            self.alignment_gpu_prefetch.clear();
            return;
        }
        let optional_pin_ceiling = match self.alignment_scene_ledger.progress() {
            Ok(progress) => progress.budget,
            Err(error) => {
                tracing::debug!(%error, "could not inspect optional GPU residency ceiling");
                self.alignment_gpu_prefetch.clear();
                return;
            }
        };
        if self.alignment_gpu_prefetch.reads && self.alignment_gpu_prefetch.read_cursor.is_none() {
            match self
                .alignment_gpu_renderer
                .begin_prepared_scene_prefetch_cursor(
                    &read_requests,
                    Arc::clone(&authority.read_pages),
                    optional_pin_ceiling,
                ) {
                Ok(cursor) => self.alignment_gpu_prefetch.read_cursor = Some(cursor),
                Err(error) => {
                    tracing::debug!(%error, "could not begin bounded read GPU prefetch");
                    self.alignment_gpu_prefetch.clear();
                    self.alignment_gpu_speculation_gate
                        .suppress_for_foreground_pressure();
                    return;
                }
            }
        }
        if self.alignment_gpu_prefetch.summaries
            && self.alignment_gpu_prefetch.summary_cursor.is_none()
        {
            match self
                .alignment_summary_gpu_renderer
                .begin_prepared_scene_prefetch_cursor(
                    &summary_requests,
                    Arc::clone(&authority.summary_pages),
                    optional_pin_ceiling,
                ) {
                Ok(cursor) => self.alignment_gpu_prefetch.summary_cursor = Some(cursor),
                Err(error) => {
                    tracing::debug!(%error, "could not begin bounded summary GPU prefetch");
                    self.alignment_gpu_prefetch.clear();
                    self.alignment_gpu_speculation_gate
                        .suppress_for_foreground_pressure();
                    return;
                }
            }
        }
        self.advance_alignment_gpu_prefetch(
            &read_requests,
            &summary_requests,
            authority,
            publication_attempt_sequence,
            optional_pin_ceiling,
        );
    }

    fn alignment_optional_work_must_yield(&self) -> bool {
        self.alignment_gpu_publication_retry.is_some()
            || self.alignment_capacity_reclaim.is_some()
            || self.panels.iter().any(|panel| {
                panel.navigation_query_schedule.gesture_active
                    || panel.navigation_query_schedule.pending.is_some()
            })
            || self.alignment_tracks.iter().any(|track| {
                track
                    .projections
                    .values()
                    .any(|projection| projection.navigation.phase().owner().is_some())
            })
    }

    fn advance_alignment_gpu_prefetch(
        &mut self,
        read_requests: &[AlignmentGpuSceneRequest<'_>],
        summary_requests: &[AlignmentSummaryGpuSceneRequest<'_>],
        authority: AlignmentGpuPreparedAuthority,
        publication_attempt_sequence: u64,
        optional_pin_ceiling: usize,
    ) {
        if !self.alignment_gpu_speculation_gate.allows_prefetch() {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_speculation_gate
                .finish_cycle(false, false);
            return;
        }

        let mut pressure = false;
        let maximum_pages = self.alignment_gpu_prefetch.maximum_pages();
        let summaries_next = self.alignment_gpu_prefetch.summaries_next;
        let advance_reads = self.alignment_gpu_prefetch.reads
            && (!self.alignment_gpu_prefetch.summaries || !summaries_next);
        let advance_summaries = self.alignment_gpu_prefetch.summaries
            && (!self.alignment_gpu_prefetch.reads || summaries_next);
        let quantum_started = Instant::now();
        let mut processed_pages = 0_usize;
        let mut attempted_quantum = false;
        let read_authority_complete = read_requests.iter().all(|request| {
            let total_rows = u32::try_from(request.scene.atlas().row_count()).unwrap_or(u32::MAX);
            let rows = request.scene.prepared_rows();
            rows.start == 0 && rows.end == total_rows
        });
        if advance_reads {
            attempted_quantum = true;
            let cursor = self
                .alignment_gpu_prefetch
                .read_cursor
                .as_mut()
                .expect("pending read prefetch has an initialized cursor");
            match self
                .alignment_gpu_renderer
                .advance_prepared_scene_prefetch_cursor(
                    &self.device,
                    &self.queue,
                    cursor,
                    &self.alignment_scene_ledger,
                    publication_attempt_sequence,
                    maximum_pages,
                ) {
                Ok(prefetch) => {
                    processed_pages =
                        processed_pages.saturating_add(prefetch.admission.requested_pages);
                    self.alignment_gpu_prefetch.reads = prefetch.missing_pages_remain;
                    if prefetch.is_complete() {
                        let cursor = self
                            .alignment_gpu_prefetch
                            .read_cursor
                            .take()
                            .expect("completed read prefetch retains its cursor");
                        if cursor.retention_limited() {
                            self.alignment_gpu_prefetch.read_retention_limited = true;
                            tracing::debug!(
                                retained_bytes = cursor.retained_bytes(),
                                optional_pin_ceiling,
                                "read GPU warm-up completed in exact demand-paged mode"
                            );
                        } else if read_authority_complete {
                            match self
                                .alignment_gpu_renderer
                                .finish_prepared_scene_prefetch_cursor(cursor)
                            {
                                Ok(lease) => {
                                    self.alignment_gpu_prefetch.read_residency = Some(lease)
                                }
                                Err(error) => {
                                    pressure = true;
                                    tracing::debug!(%error, "could not finalize read GPU residency");
                                }
                            }
                        } else {
                            // Progressive semantic preparation has not yet exposed every row, so
                            // this exact authority is demand-paged by definition even if every
                            // currently prepared page fitted beneath the optional pin ceiling.
                            self.alignment_gpu_prefetch.read_retention_limited = true;
                        }
                    }
                }
                Err(error) => {
                    pressure = true;
                    tracing::debug!(%error, "read GPU prefetch yielded to foreground capacity");
                }
            }
        }

        if advance_summaries && !pressure {
            attempted_quantum = true;
            let cursor = self
                .alignment_gpu_prefetch
                .summary_cursor
                .as_mut()
                .expect("pending summary prefetch has an initialized cursor");
            match self
                .alignment_summary_gpu_renderer
                .advance_prepared_scene_prefetch_cursor(
                    &self.device,
                    &self.queue,
                    cursor,
                    &self.alignment_scene_ledger,
                    publication_attempt_sequence,
                    maximum_pages,
                ) {
                Ok(prefetch) => {
                    processed_pages =
                        processed_pages.saturating_add(prefetch.admission.requested_pages);
                    self.alignment_gpu_prefetch.summaries = prefetch.missing_pages_remain;
                    if prefetch.is_complete() {
                        let cursor = self
                            .alignment_gpu_prefetch
                            .summary_cursor
                            .take()
                            .expect("completed summary prefetch retains its cursor");
                        if cursor.retention_limited() {
                            self.alignment_gpu_prefetch.summary_retention_limited = true;
                            tracing::debug!(
                                retained_bytes = cursor.retained_bytes(),
                                optional_pin_ceiling,
                                "summary GPU warm-up completed in exact demand-paged mode"
                            );
                        } else {
                            match self
                                .alignment_summary_gpu_renderer
                                .finish_prepared_scene_prefetch_cursor(cursor)
                            {
                                Ok(lease) => {
                                    self.alignment_gpu_prefetch.summary_residency = Some(lease)
                                }
                                Err(error) => {
                                    pressure = true;
                                    tracing::debug!(%error, "could not finalize summary GPU residency");
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    pressure = true;
                    tracing::debug!(%error, "summary GPU prefetch yielded to foreground capacity");
                }
            }
        }

        if attempted_quantum {
            self.alignment_gpu_prefetch
                .observe_quantum(quantum_started.elapsed(), processed_pages);
            self.alignment_gpu_prefetch.summaries_next = !summaries_next;
        }

        if pressure {
            self.alignment_gpu_prefetch.clear();
            self.alignment_gpu_residency_bundle = None;
            self.alignment_gpu_speculation_gate
                .suppress_for_foreground_pressure();
            return;
        }
        let reads_finished = read_requests.is_empty()
            || self.alignment_gpu_prefetch.read_residency.is_some()
            || self.alignment_gpu_prefetch.read_retention_limited;
        let summaries_finished = summary_requests.is_empty()
            || self.alignment_gpu_prefetch.summary_residency.is_some()
            || self.alignment_gpu_prefetch.summary_retention_limited;
        let retention_limited = self.alignment_gpu_prefetch.read_retention_limited
            || self.alignment_gpu_prefetch.summary_retention_limited;
        if reads_finished
            && summaries_finished
            && !retention_limited
            && self
                .alignment_gpu_residency_bundle
                .as_ref()
                .is_none_or(|resident| resident.authority != authority)
        {
            let reads = self.alignment_gpu_prefetch.read_residency.take();
            let summaries = self.alignment_gpu_prefetch.summary_residency.take();
            if let Some(request) = read_requests.first() {
                self.data_status = format!(
                    "{} complete alignments · all {} rows ready for instant browsing",
                    request.scene.evidence().record_count(),
                    request.scene.atlas().row_count()
                );
            }
            self.alignment_gpu_residency_bundle = Some(AlignmentGpuResidencyBundle {
                authority,
                _reads: reads,
                _summaries: summaries,
            });
        } else if !self.alignment_gpu_prefetch.pending() {
            // A progressively prepared read scene may not yet span every atlas row. Do not retain
            // a partial all-scene residency claim; the next content epoch will restart from its
            // own exact authority.
            self.alignment_gpu_prefetch.read_residency = None;
            self.alignment_gpu_prefetch.summary_residency = None;
            if retention_limited {
                self.data_status = "Complete alignments remain available on demand; full-stack instant residency exceeds the current cache budget".to_owned();
            }
        }
        let pending = self.alignment_gpu_prefetch.pending();
        self.alignment_gpu_speculation_gate
            .finish_cycle(pending, retention_limited);
        if pending {
            self.window.request_redraw();
        }
    }

    pub(crate) fn synchronize_alignment_scene_interactions(&mut self, panel_id: PanelId) {
        let (hovered, selected) = self.panels.get(panel_id).map_or((None, None), |panel| {
            (panel.hovered_alignment_hit, panel.selected_alignment_hit)
        });
        let track_ids = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        for track_id in track_ids {
            let highlighted = self.alignment_highlight_handles(panel_id, track_id);
            let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            else {
                continue;
            };
            let hovered = hovered
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            let selected = selected
                .filter(|hit| hit.track == track.config.id)
                .map(|hit| hit.handle);
            track.projection_mut(panel_id).scene_interaction =
                AlignmentInteractionState::new(hovered, selected, std::iter::empty())
                    .with_highlighted(highlighted.iter().copied());
        }
    }

    fn alignment_highlight_handles(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Arc<[AlignmentHandle]> {
        let Some(track_index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == track_id)
        else {
            return Arc::from([]);
        };
        let Some(metadata) = self.alignment_tracks[track_index].scene_metadata.as_ref() else {
            return Arc::from([]);
        };
        let contract = alignment_highlight_filter_contract(
            &self.alignment_tracks[track_index].config.filters,
            &metadata.dictionary,
        );
        if contract.is_empty() {
            let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
            projection.scene_highlight_key = None;
            projection.scene_highlighted = Arc::from([]);
            projection.scene_highlight_status = None;
            return Arc::clone(&projection.scene_highlighted);
        }
        let Some(snapshot) = self.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_frame.as_ref())
            .and_then(|frame| frame.snapshot.as_ref())
            .cloned()
        else {
            return Arc::from([]);
        };
        let reference_revision = self
            .panels
            .get(panel_id)
            .and_then(|panel| panel.reference_slice_revision);
        let key = (
            snapshot.reads().evidence().revision(),
            contract.fingerprint(),
            reference_revision,
        );
        if let Some(projection) = self.alignment_tracks[track_index].projection(panel_id)
            && projection.scene_highlight_key == Some(key)
        {
            return Arc::clone(&projection.scene_highlighted);
        }
        let reference = self.panels.get(panel_id).and_then(|panel| {
            panel.reference_slice.as_deref().filter(|slice| {
                panel.reference_slice_revision == reference_revision
                    && slice
                        .interval
                        .contains_interval(snapshot.prepared_interval())
            })
        });
        let evaluation = evaluate_alignment_filter(
            snapshot.reads().evidence(),
            snapshot.prepared_interval(),
            &contract,
            reference,
        );
        let (highlighted, status): (Arc<[AlignmentHandle]>, Option<String>) = match evaluation {
            Ok(evaluation) => (evaluation.into_matching_handles().into(), None),
            Err(error) => (
                Arc::from([]),
                Some(format!("Highlights unavailable: {error}")),
            ),
        };
        let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
        projection.scene_highlight_key = Some(key);
        projection.scene_highlighted = Arc::clone(&highlighted);
        projection.scene_highlight_status = status;
        highlighted
    }

    pub(crate) fn alignment_scene_snapshot_for_panel_track(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<&Arc<AlignmentSceneSnapshot>> {
        let presented = self.presented_alignment_lane(panel_id, track_id)?;
        let snapshot = presented.lane.snapshot.as_ref()?;
        presented.read_authority.map(|_| snapshot)
    }

    /// Binds cross-panel/name-based read intent to the stable handle in the newly committed scene.
    /// Query-local record indexes are never introduced on this path. Resolution is deliberately
    /// performed after the atomic epoch commit, so selection can only name evidence that render,
    /// picking, and inspection can all observe together.
    fn resolve_pending_alignment_scene_selections(&mut self) {
        let Some(epoch) = self.alignment_presented_epoch.as_ref() else {
            return;
        };
        let resolutions = self
            .panels
            .iter()
            .filter_map(|panel| {
                let pending = panel.pending_read_selection.clone()?;
                let lane = epoch.panel_frames.get(&panel.id)?.lane(pending.track_id)?;
                let snapshot = lane.snapshot.as_ref()?;
                let (handle, record) = pending.matching_scene_record(snapshot.reads())?;
                let chain = selected_supplementary_chain(&record, &panel.contig_name);
                let (retained, reveal) = pending.retained_after_resolution(Some(&record));
                Some((
                    panel.id,
                    retained.track_id,
                    snapshot.revision(),
                    handle,
                    chain,
                    retained,
                    reveal,
                ))
            })
            .collect::<Vec<_>>();
        if resolutions.is_empty() {
            return;
        }
        let mut resolved_panels = Vec::with_capacity(resolutions.len());
        for (panel_id, track_id, scene_revision, handle, chain, retained, reveal) in resolutions {
            let Some(panel) = self.panels.get_mut(panel_id) else {
                continue;
            };
            panel.pending_read_selection = Some(retained);
            panel.selected_alignment_track = Some(track_id);
            panel.selected_alignment_hit = Some(AlignmentHit {
                track: track_id,
                scene_revision,
                handle,
                component: AlignmentComponent::ReadSpan,
            });
            panel.selected_chain = chain;
            if reveal {
                panel.pending_alignment_reveal = Some((track_id, handle));
            }
            replace_panel_track_selection(
                panel,
                TrackFrameKey::Alignment(track_id),
                ui::TrackPane::Reads,
            );
            resolved_panels.push(panel_id);
        }
        resolved_panels.sort_unstable();
        resolved_panels.dedup();
        for panel_id in resolved_panels {
            self.synchronize_alignment_scene_interactions(panel_id);
        }
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    pub(crate) fn presented_alignment_lane(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<PresentedAlignmentLane<'_>> {
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        if !alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority) {
            return None;
        }
        let panel_frame = epoch.panel_frames.get(&panel_id)?;
        if panel_frame.camera_revision != bundle.target_revision {
            return None;
        }
        let lane = panel_frame.lane(track_id)?;
        let source = bundle.lane_authorities.iter().find(|authority| {
            authority.panel_id == panel_id
                && authority.track_id == track_id
                && authority.target_revision == bundle.target_revision
                && authority.source_camera == lane.preparation_camera
        })?;
        let read_authority = lane.snapshot.as_ref().and_then(|snapshot| {
            bundle.read_authorities.iter().find(|authority| {
                authority.panel_id == panel_id
                    && authority.track_id == track_id
                    && authority.target_revision == bundle.target_revision
                    && authority.source_request_token == source.source_request_token
                    && authority.desired_request_token == source.desired_request_token
                    && authority.source_camera == source.source_camera
                    && authority.scene_revision == snapshot.reads().revision()
            })
        });
        Some(PresentedAlignmentLane {
            target_revision: bundle.target_revision,
            lane,
            source,
            read_authority,
        })
    }

    /// Returns committed genomic row authority while a vertically changed pixel target is still
    /// being prepared. Unlike `presented_alignment_lane`, this accessor deliberately does not
    /// require the old GPU bundle to match the complete desired camera: row extent is independent
    /// of visible-row page readiness. The publication authority first proves that the committed
    /// and desired lanes still describe the same genomic interval.
    pub(crate) fn committed_alignment_row_activity_for_desired_target(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<(u64, Arc<AlignmentRowActivityProfile>)> {
        if !self
            .alignment_scene_publication_authority
            .committed_row_target_matches_desired(panel_id, track_id)
        {
            return None;
        }
        let epoch = self.alignment_presented_epoch.as_ref()?;
        let bundle = epoch.gpu_bundle.as_ref()?;
        if bundle.target_revision != self.alignment_scene_publication_authority.target_revision {
            return None;
        }
        let panel_frame = epoch.panel_frames.get(&panel_id)?;
        if panel_frame.camera_revision != bundle.target_revision {
            return None;
        }
        let lane = panel_frame.lane(track_id)?;
        bundle.lane_authorities.iter().find(|authority| {
            authority.panel_id == panel_id
                && authority.track_id == track_id
                && authority.target_revision == bundle.target_revision
                && authority.source_camera == lane.preparation_camera
        })?;
        let profile = lane.row_activity.as_ref()?;
        if profile.interval() != lane.preparation_camera.interval() {
            return None;
        }
        Some((bundle.target_revision, Arc::clone(profile)))
    }

    pub(crate) fn alignment_scene_record_for_hit(
        &self,
        panel_id: PanelId,
        hit: AlignmentHit,
    ) -> Option<AlignmentRecord> {
        let snapshot = self.alignment_scene_snapshot_for_panel_track(panel_id, hit.track)?;
        snapshot.inspect_retained(hit.handle).ok().flatten()
    }

    fn take_alignment_scene_owner(&mut self) -> SourceOwnerId {
        self.alignment_scene_owner_clock = self
            .alignment_scene_owner_clock
            .checked_add(1)
            .expect("alignment scene owner identity space was exhausted");
        SourceOwnerId::new(self.alignment_scene_owner_clock)
    }

    pub(crate) fn take_alignment_scene_request_token(&mut self) -> AlignmentSceneRequestToken {
        self.alignment_scene_request_clock = self
            .alignment_scene_request_clock
            .checked_add(1)
            .expect("alignment scene request identity space was exhausted");
        AlignmentSceneRequestToken::new(self.alignment_scene_request_clock)
    }

    /// Begins the sole liveness transaction for one panel/track target. Callers which already
    /// advanced the projection clock (ordinary viewport navigation) pass `advance_generation =
    /// false`. Contract changes advance explicitly at their call site; row-camera-only changes
    /// keep the same immutable source generation and receive only a new request token.
    pub(crate) fn queue_alignment_navigation_for_track(
        &mut self,
        track_index: usize,
        panel_id: PanelId,
        advance_generation: bool,
        reason: &'static str,
    ) {
        if track_index >= self.alignment_tracks.len() {
            return;
        }
        let failed = matches!(
            self.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Failed(_)
        );
        if failed {
            let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
            projection.navigation.transition(
                AlignmentNavigationPhase::Failed,
                "alignment source is already failed",
            );
            return;
        }
        if advance_generation {
            self.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .advance_scene_generation();
        }
        let token = self.take_alignment_scene_request_token();
        let track_id = self.alignment_tracks[track_index].config.id;
        let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
        let generation = projection.clock.current().get();
        projection.navigation.bind_identity(panel_id, track_id);
        projection.navigation.begin(generation, token, reason);
        projection.scene_pressure_retry = None;
    }

    /// Sends the UI's current camera to the unified source actor and changes no pixels itself.
    /// Existing immutable scene pages are captured against this camera immediately by
    /// `prepare_alignment_scene_frame`; source acquisition remains asynchronous and latest-wins.
    pub(crate) fn dispatch_alignment_query(&mut self) {
        if !self.panels.active().pending_queries.alignment {
            return;
        }
        if self.alignment_tracks.is_empty() {
            self.panels.active_mut().pending_queries.alignment = false;
            return;
        }
        let viewport = self.panels.active().viewport;
        let resolution = AlignmentSceneResolution::for_viewport(viewport);
        let panel_id = self.panels.active().id;
        if !resolution.requires_alignment_records() {
            let mut settled = 0_usize;
            let mut retired_tracks = Vec::new();
            for track in &mut self.alignment_tracks {
                match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => {}
                    AlignmentTrackLifecycle::Ready => {
                        let track_id = track.config.id;
                        let projection = track.projection_mut(panel_id);
                        // Index overview is a presentation decision, not source retirement. Keep
                        // the immutable lane cached so zooming back can reuse it immediately; it
                        // is excluded from capture while `index_only_overview` is true.
                        projection.scene_pressure_retry = None;
                        projection.settle_index_only_overview(resolution);
                        retired_tracks.push(track_id);
                        settled = settled.saturating_add(1);
                    }
                    AlignmentTrackLifecycle::Failed(_) => {
                        let projection = track.projection_mut(panel_id);
                        projection.index_only_overview = false;
                        projection.navigation.transition(
                            AlignmentNavigationPhase::Failed,
                            "alignment source failed before index overview",
                        );
                    }
                }
            }
            let primary_track_id = self.primary_alignment_track_id();
            for track_id in retired_tracks {
                let panel = self.panels.active_mut();
                panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .retire_scene_authority();
                if panel
                    .pending_alignment_reveal
                    .is_some_and(|(pending_track, _)| pending_track == track_id)
                {
                    panel.pending_alignment_reveal = None;
                }
                if track_id == primary_track_id {
                    panel.vertical_scroll_points = 0.0;
                    panel.vertical_content_origin_points = 0.0;
                    panel.latent_vertical_scroll_fraction = None;
                    panel.content_bottom_points = 0.0;
                }
            }
            self.panels.active_mut().pending_queries.alignment =
                self.alignment_tracks.iter().any(|track| {
                    track
                        .projection(panel_id)
                        .is_some_and(|projection| projection.navigation.is_queued())
                });
            if settled > 0 {
                self.data_status = format!(
                    "Index-only overview for {settled} alignment track(s); no records requested"
                );
            }
            self.prepare_alignment_scene_frame();
            return;
        }
        let contig_name = self.panels.active().contig_name.clone();
        let mut sent = 0_usize;
        for track_index in 0..self.alignment_tracks.len() {
            if matches!(
                self.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Opening
            ) {
                continue;
            }
            if matches!(
                self.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Failed(_)
            ) {
                let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
                projection.navigation.transition(
                    AlignmentNavigationPhase::Failed,
                    "alignment source is disconnected",
                );
                continue;
            }
            if !self.alignment_tracks[track_index]
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
            {
                continue;
            }
            let Some(metadata) = self.alignment_tracks[track_index].scene_metadata.clone() else {
                continue;
            };
            let Some((contig, contig_length)) = metadata
                .dictionary
                .id_by_name(&contig_name)
                .and_then(|contig| {
                    metadata
                        .dictionary
                        .get(contig)
                        .map(|entry| (contig, entry.length()))
                })
            else {
                let message = format!(
                    "{} is not available in this alignment",
                    self.panels.active().contig_name
                );
                self.alignment_tracks[track_index].mark_unavailable(panel_id, message);
                continue;
            };
            let track_id = self.alignment_tracks[track_index].config.id;
            let existing_owner = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_owner);
            let existing_contig = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contig);
            let existing_contract = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contract.as_ref())
                .cloned();
            let existing_policy = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_policy);
            let target_viewport = match viewport.remapped_contig(contig, contig_length) {
                Ok(viewport) => viewport,
                Err(_) => continue,
            };
            let old_atlas_interval = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_frame.as_ref())
                .and_then(|frame| frame.snapshot.as_ref())
                .map(|snapshot| snapshot.reads().atlas().interval());
            let contig_changed = existing_contig.is_some_and(|current| current != contig);
            let atlas_disjoint = old_atlas_interval.is_some_and(|interval| {
                interval.contig() != contig || !interval.overlaps(target_viewport.interval())
            });
            let authority_discontinuity = contig_changed || atlas_disjoint;
            let primary_track_id = self.primary_alignment_track_id();
            let scroll_reset = {
                let panel = self.panels.active_mut();
                let reset = panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .bind_scene_contig(contig, authority_discontinuity);
                if reset {
                    panel.pending_alignment_reveal = None;
                    if track_id == primary_track_id {
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                        panel.content_bottom_points = 0.0;
                    }
                }
                reset
            };
            if authority_discontinuity || scroll_reset {
                let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
                projection.scene_frame = None;
                projection.scene_frame_request_token = None;
                projection.scene_pressure_retry = None;
                if let Some(store) = self.alignment_panel_scene_stores.get_mut(&panel_id)
                    && let Err(error) = store.clear_lane(track_id)
                {
                    self.data_status = format!("Could not reset alignment scene lane: {error}");
                }
            }
            let promoted_template = self
                .promoted_read
                .filter(|(candidate, _)| *candidate == track_id)
                .map(|(_, hash)| hash);
            let contract = alignment_scene_build_contract(
                &self.alignment_tracks[track_index].config,
                &self.alignment_tracks[track_index].active_filters,
                &metadata,
                contig,
                contig_length,
                viewport,
                promoted_template,
                self.reference_revision,
            );
            let reference_backed_alleles = contract.summaries.allele_detail
                != AlleleSummaryDetail::None
                && contract.presentation.reference.is_some();
            let lane_capture = match self.alignment_scene_lane_capture_for_panel_track(
                panel_id,
                track_id,
                contig,
                contig_length,
                reference_backed_alleles,
            ) {
                Ok(Some(capture)) => capture,
                Ok(None) => {
                    // Layout is committed independently of source opening. Keep this request
                    // pending until at least one real read or derived lane exists; never invent a
                    // render clip merely to force source work.
                    continue;
                }
                Err(error) => {
                    self.data_status = format!("Could not resolve alignment scene lanes: {error}");
                    continue;
                }
            };
            let preparation_camera = lane_capture.preparation_camera;
            let policy = alignment_scene_preparation_policy(
                self.settings.read_browsing_buffer_percent,
                preparation_camera,
            );
            let complete_read_limit = self.alignment_tracks[track_index]
                .config
                .maximum_alignments
                .max(1);
            let mut queue_error = None;
            let must_insert = existing_owner.is_none() || existing_contig != Some(contig);
            let owner = if must_insert {
                self.take_alignment_scene_owner()
            } else {
                existing_owner.expect("an existing alignment scene lane has an owner")
            };
            let request_token = self.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.navigation.request_token())
                .expect("a queued alignment transaction has a request token");
            self.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .navigation
                .bind_camera(preparation_camera);
            let contract_changed = existing_contract
                .as_ref()
                .is_none_or(|existing| !alignment_scene_contract_equivalent(existing, &contract));
            let previous_view_filter = existing_contract.as_ref().map(|existing| {
                existing
                    .view_filter
                    .evaluation_fingerprint(existing.presentation.reference)
                    .get()
            });
            let previous_highlight_filter = existing_contract.as_ref().map(|existing| {
                existing
                    .highlight_filter
                    .evaluation_fingerprint(existing.presentation.reference)
                    .get()
            });
            let requested_view_filter = contract
                .view_filter
                .evaluation_fingerprint(contract.presentation.reference)
                .get();
            let requested_highlight_filter = contract
                .highlight_filter
                .evaluation_fingerprint(contract.presentation.reference)
                .get();
            let filter_authority_changed = previous_view_filter != Some(requested_view_filter)
                || previous_highlight_filter != Some(requested_highlight_filter);
            let source_acquisitions_before = self.alignment_tracks[track_index]
                .projection(panel_id)
                .map_or(0, |projection| {
                    projection.scene_telemetry.source_acquisitions
                });
            let limit_changed =
                self.alignment_tracks[track_index].scene_complete_read_limit != complete_read_limit;
            let mut limit_queued = false;
            let mut lane_removed = false;
            let mut lane_inserted = false;
            let mut contract_replaced = false;
            let mut policy_replaced = false;
            let Some(engine) = self.alignment_tracks[track_index].scene_engine.as_ref() else {
                let track = &mut self.alignment_tracks[track_index];
                track.lifecycle = AlignmentTrackLifecycle::Failed(
                    "Unified alignment scene source is unavailable; no legacy fallback was used"
                        .to_owned(),
                );
                let projection = track.projection_mut(panel_id);
                projection.navigation.transition(
                    AlignmentNavigationPhase::Failed,
                    "unified alignment scene source is unavailable",
                );
                continue;
            };
            if limit_changed {
                match engine.try_set_source_control(LocalBamSourceControl::SetCompleteReadLimit(
                    NonZeroUsize::new(complete_read_limit)
                        .expect("Complete read limit is non-zero"),
                )) {
                    Ok(()) => limit_queued = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && existing_owner.is_some() && existing_contig != Some(contig)
            {
                match engine.try_remove_lane(existing_owner.expect("checked existing owner")) {
                    Ok(()) => lane_removed = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && must_insert {
                match engine.try_insert_lane_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                    policy,
                ) {
                    Ok(()) => lane_inserted = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && contract_changed {
                match engine.try_replace_contract_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                ) {
                    Ok(()) => contract_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && existing_policy != Some(policy) {
                match engine.try_set_preparation_policy_for_request(owner, request_token, policy) {
                    Ok(()) => policy_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none()
                && let Err(error) =
                    engine.try_update_camera_for_request(owner, request_token, preparation_camera)
            {
                queue_error = Some(error);
            }
            {
                let track = &mut self.alignment_tracks[track_index];
                if limit_queued {
                    track.scene_complete_read_limit = complete_read_limit;
                }
                let projection = track.projection_mut(panel_id);
                if lane_inserted {
                    projection.scene_owner = Some(owner);
                    projection.scene_optional_work_paused = false;
                    projection.scene_contig = Some(contig);
                    projection.scene_contract = Some(contract.clone());
                    projection.scene_policy = Some(policy);
                } else if lane_removed {
                    // A bounded control queue can accept removal and reject the subsequent insert.
                    // Make the retry state explicit; never pretend the old-contig lane remains.
                    projection.scene_owner = None;
                    projection.scene_optional_work_paused = false;
                    projection.scene_contig = None;
                    projection.scene_contract = None;
                    projection.scene_policy = None;
                }
                if contract_replaced {
                    projection.scene_contract = Some(contract);
                }
                if policy_replaced {
                    projection.scene_policy = Some(policy);
                }
            }
            if contract_replaced && let Some(panel) = self.panels.get_mut(panel_id) {
                // The receipt acknowledges delivery to the previous coordinator contract epoch.
                // A replacement may preserve compatible authority, but it may also retire it.
                // Invalidate the receipt in either case so a still-pending exact requirement can
                // enqueue the cached slice again instead of waiting forever for a command which
                // already ran against the old contract.
                retire_reference_install_receipt_for_contract_epoch(
                    &mut panel.reference_installs_queued,
                    owner,
                );
            }
            match queue_error {
                None => {
                    {
                        let projection =
                            self.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.begin_checking(Instant::now());
                        projection.last_scene_resolution = Some(resolution);
                        projection.scene_pressure_retry = None;
                        projection.unavailable = None;
                        projection.index_only_overview = false;
                        projection.navigation.transition(
                            AlignmentNavigationPhase::Running,
                            "source worker accepted the exact target",
                        );
                    }
                    if filter_authority_changed && (lane_inserted || contract_replaced) {
                        let filter_state =
                            self.alignment_filter_diagnostic_state(panel_id, track_id);
                        self.record_manual_diagnostic_event(
                            "alignment_filter",
                            "contract_queued",
                            serde_json::json!({
                                "panel_id": panel_id.get(),
                                "track_id": track_id.get(),
                                "request_token": request_token.get(),
                                "lane_inserted": lane_inserted,
                                "contract_replaced": contract_replaced,
                                "previous_view_filter_evaluation_fingerprint": previous_view_filter,
                                "requested_view_filter_evaluation_fingerprint": requested_view_filter,
                                "previous_highlight_filter_evaluation_fingerprint": previous_highlight_filter,
                                "requested_highlight_filter_evaluation_fingerprint": requested_highlight_filter,
                                "source_acquisitions_before": source_acquisitions_before,
                                "expected_source_reacquisition": false,
                                "filter_state": filter_state,
                            }),
                        );
                    }
                    sent = sent.saturating_add(1);
                }
                Some(EngineQueueError::Full) => {}
                Some(EngineQueueError::Disconnected) => {
                    let track = &mut self.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    projection.navigation.transition(
                        AlignmentNavigationPhase::Failed,
                        "alignment source mailbox disconnected",
                    );
                    track.lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        self.panels.active_mut().pending_queries.alignment =
            self.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
        if sent > 0 {
            self.data_status = format!("Updating {sent} alignment scene(s)…");
        } else if self.panels.active().pending_queries.alignment {
            "Alignment queues busy; retrying the latest locus…".clone_into(&mut self.data_status);
        }
        // Camera-only movement is visible before the worker produces another event. The capture
        // keeps the latest immutable snapshot and computes its truthful intersection with the
        // live viewport.
        self.prepare_alignment_scene_frame();
    }

    pub(crate) fn alignment_track_limit_reached(&mut self) -> bool {
        if self.alignment_tracks.len() >= MAXIMUM_ALIGNMENT_TRACKS {
            self.data_status =
                format!("A workspace supports at most {MAXIMUM_ALIGNMENT_TRACKS} alignment tracks");
            return true;
        }
        false
    }

    pub(crate) fn add_alignment_track(
        &mut self,
        source: TrackSource,
        opened_name: &str,
        remote_index_url: Option<String>,
    ) {
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let next_id = configs
            .iter()
            .map(|track| track.id.get())
            .chain(self.tracks.tracks().iter().map(|track| track.id.get()))
            .max()
            .unwrap_or_default()
            .saturating_add(1);
        let mut config =
            SessionAlignmentTrack::new(TrackId::new(next_id), opened_name.to_owned(), source);
        config.remote_index_url = remote_index_url;
        config.maximum_alignments = config
            .maximum_alignments
            .min(self.workspace_alignment_limit)
            .max(1);
        configs.push(config);
        match self.replace_alignment_track_controllers(configs) {
            Ok(()) => {
                self.invalidate_navigation_contract("alignment source set changed");
                self.begin_all_panel_refresh_transactions();
                self.panels.active_mut().pending_queries.alignment = false;
                self.synthetic = false;
                self.data_status =
                    format!("Opening {opened_name} and rebalancing alignment-track caches…");
                self.rebuild_genomic_display();
                self.mark_display_dirty("explicit display invalidation");
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn open_alignment_source(&mut self, path: &Path) {
        self.open_alignment_source_with_remote_index(path, None);
    }

    pub(crate) fn open_remote_source(&mut self, request: RemoteSourceRequest) -> Result<()> {
        match request.kind {
            RemoteSourceKind::Alignment => {
                if let Some(reference_url) = request.reference_url {
                    self.open_reference_source_with_remote_index(
                        Path::new(&reference_url),
                        request.reference_index_url,
                    );
                }
                self.open_alignment_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::Reference => {
                self.open_reference_source_with_remote_index(
                    Path::new(&request.source_url),
                    request.index_url,
                );
            }
            RemoteSourceKind::IndexedTrack => {
                self.add_track_source(Path::new(&request.source_url))?;
            }
        }
        Ok(())
    }

    pub(crate) fn open_alignment_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        if self.alignment_track_limit_reached() {
            return;
        }
        let source = track_source(path);
        let opened_name = source_display_name(Some(&source), "Alignments");
        if !self.alignment_tracks.is_empty() {
            self.add_alignment_track(source, &opened_name, remote_index_url);
            return;
        }
        let source_changed = self.primary_alignment_source() != Some(&source);
        let name = if source_changed {
            opened_name
        } else {
            self.primary_alignment_name().to_owned()
        };
        let mut configs = self
            .alignment_tracks
            .iter()
            .map(|track| track.config.clone())
            .collect::<Vec<_>>();
        let config = upsert_primary_alignment_config(
            &mut configs,
            self.tracks.tracks(),
            source.clone(),
            &name,
        );
        config.remote_index_url = remote_index_url;
        config.filters = self.primary_alignment_filters().clone();
        config.maximum_alignments = self
            .primary_alignment_limit()
            .min(self.workspace_alignment_limit)
            .max(1);
        let cache_bytes = NonZeroUsize::new(self.alignment_scene_budget.budget_bytes.max(1))
            .expect("scene budget is non-zero");
        let controller = spawn_alignment_track_controller(
            configs.remove(0),
            cache_bytes,
            self.alignment_scene_ledger.clone(),
            self.reference_source.as_ref().map(source_path),
            self.remote_reference_index_url.clone(),
            &self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>(),
        );
        match controller {
            Ok(controller) => {
                let interval = self.panels.active().viewport.interval();
                self.panels.active_mut().requested_locus = Some(format!(
                    "{}:{}-{}",
                    self.panels.active().contig_name,
                    interval.start() + 1,
                    interval.end()
                ));
                self.panels.active().generation_clock.advance();
                if self.alignment_tracks.is_empty() {
                    self.alignment_tracks.push(controller);
                } else {
                    self.alignment_tracks[0] = controller;
                }
                self.invalidate_navigation_contract("alignment source changed");
                self.begin_all_panel_refresh_transactions();
                self.reconcile_track_presentation();
                self.panels.active_mut().pending_queries.alignment = false;
                self.panels.active_mut().selected_chain = None;
                self.panels.active_mut().display_list = DisplayList::default();
                self.synthetic = false;
                self.data_status = format!("Opening {}…", source.redacted_label());
                self.mark_display_dirty("explicit display invalidation");
            }
            Err(error) => self.data_status = error.to_string(),
        }
    }

    pub(crate) fn open_reference_source(&mut self, path: &Path) {
        self.open_reference_source_with_remote_index(path, None);
    }

    pub(crate) fn open_reference_source_with_remote_index(
        &mut self,
        path: &Path,
        remote_index_url: Option<String>,
    ) {
        self.reference_open_error = None;
        self.remote_reference_index_url = remote_index_url;
        let source = track_source(path);
        let source_label = source.redacted_label();
        let engine = ReferenceEngine::spawn(8);
        if let Err(error) = engine.try_open_with_options(
            path.to_path_buf(),
            reference_open_options(self.remote_reference_index_url.clone()),
        ) {
            self.data_status = error.to_string();
            return;
        }
        self.reference_source = Some(source);
        self.reference_dictionary = None;
        self.reference_revision = None;
        self.reference_remote_io_stats = None;
        self.reference_engine = Some(engine);
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            if let Some(panel) = self.panels.get_mut(id) {
                let interval = panel.viewport.interval();
                panel.requested_locus = Some(format!(
                    "{}:{}-{}",
                    panel.contig_name,
                    interval.start().saturating_add(1),
                    interval.end()
                ));
                panel.generation_clock.advance();
                panel.reference_slice = None;
                panel.reference_slice_revision = None;
                panel.reference_slice_generation = None;
                panel.reference_request_revision = None;
                panel.reference_request_interval = None;
                panel.reference_installs_queued.clear();
                panel.reference_settled_generation = None;
                panel.reference_query_error_generation = None;
                panel.pending_queries.reference = false;
            }
        }
        if !self.alignment_tracks.is_empty() {
            let configs = self
                .alignment_tracks
                .iter()
                .map(|track| track.config.clone())
                .collect::<Vec<_>>();
            if let Err(error) = self.replace_alignment_track_controllers(configs) {
                self.data_status = error.to_string();
                return;
            }
        }
        self.synthetic = false;
        self.primary_alignment_filters_mut().show_reference = true;
        self.data_status = format!("Opening reference genome {source_label}…");
        self.invalidate_navigation_contract("reference source changed");
        self.begin_all_panel_refresh_transactions();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
    }

    pub(crate) fn open_genome_configuration(&mut self, path: &Path) -> Result<()> {
        let configuration = GenomeConfiguration::read(path)?;
        let cytoband_path = match &configuration.cytobands {
            Some(TrackSource::Local { path }) => Some(PathBuf::from(path)),
            Some(TrackSource::Remote { .. }) => {
                anyhow::bail!("remote cytoband files are not yet supported")
            }
            None => None,
        };
        let annotations = configuration
            .default_annotations
            .iter()
            .map(|annotation| {
                track_spec_kind(annotation.kind, &annotation.source, annotation.format)
                    .map(|kind| {
                        (
                            source_path(&annotation.source),
                            kind,
                            annotation.kind,
                            annotation.name.clone(),
                            annotation.format,
                        )
                    })
                    .with_context(|| format!("unsupported default annotation {}", annotation.name))
            })
            .collect::<Result<Vec<_>>>()?;

        self.assembly_name.clone_from(&configuration.name);
        self.assembly_identity
            .clone_from(&configuration.assembly_identity);
        self.assembly
            .identity
            .clone_from(&configuration.assembly_identity);
        self.assembly.default_annotation_count = configuration.default_annotations.len();
        self.contig_aliases
            .clone_from(&configuration.contig_aliases);
        self.cytoband_path = cytoband_path;
        let reference = source_path(&configuration.reference);
        self.open_reference_source(&reference);
        for (annotation_path, kind, track_kind, name, format) in annotations {
            self.add_configured_track_source(&annotation_path, kind, track_kind, name, format)?;
        }
        self.data_status = format!("Opening genome configuration {}…", path.display());
        Ok(())
    }

    pub(crate) fn replace_alignment_track_controllers(
        &mut self,
        configs: Vec<SessionAlignmentTrack>,
    ) -> Result<()> {
        let cache_bytes = NonZeroUsize::new(self.alignment_scene_budget.budget_bytes.max(1))
            .expect("scene budget is non-zero");
        let reference_path = self.reference_source.as_ref().map(source_path);
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let mut controllers = Vec::with_capacity(configs.len());
        for config in configs {
            controllers.push(spawn_alignment_track_controller(
                config,
                cache_bytes,
                self.alignment_scene_ledger.clone(),
                reference_path.clone(),
                self.remote_reference_index_url.clone(),
                &panel_ids,
            )?);
        }
        let replaced = std::mem::replace(&mut self.alignment_tracks, controllers);
        for controller in replaced {
            self.retire_alignment_track_controller(controller);
        }
        self.reset_alignment_scene_panel_authority();
        self.reconcile_track_presentation();
        Ok(())
    }

    fn reset_alignment_scene_panel_authority(&mut self) {
        let track_ids = self
            .alignment_tracks
            .iter()
            .filter(|track| track.scene_engine.is_some())
            .map(|track| track.config.id)
            .collect::<Vec<_>>();
        self.alignment_panel_scene_stores = self
            .panels
            .iter()
            .map(|panel| {
                (
                    panel.id,
                    AlignmentPanelSceneStore::new(track_ids.iter().copied()),
                )
            })
            .collect();
        self.alignment_presented_epoch = None;
        self.alignment_gpu_residency_bundle = None;
        self.alignment_gpu_prefetch.clear();
        self.alignment_gpu_publication_retry = None;
        self.alignment_capacity_reclaim = None;
        self.alignment_gpu_speculation_gate.reset();
        self.alignment_scene_publication_authority.reset();
    }

    pub(crate) fn retire_alignment_track_controller(
        &mut self,
        controller: AlignmentTrackController,
    ) {
        for projection in controller.projections.values() {
            projection.advance_scene_generation();
        }
        let AlignmentTrackController { scene_engine, .. } = controller;
        if let Some(task) = scene_engine.and_then(AlignmentSceneEngine::retire) {
            self.alignment_retirement_tasks.push(task);
        }
    }

    pub(crate) fn activate_first_alignment_track(&mut self) -> Result<()> {
        if let Some((config, filter_editor, active_filters)) =
            self.alignment_tracks.first().map(|track| {
                (
                    track.config.clone(),
                    track.filter_editor.clone(),
                    track.active_filters.clone(),
                )
            })
        {
            self.apply_primary_alignment_config(Some(&config), self.workspace_alignment_limit)?;
            let primary = self
                .alignment_tracks
                .first_mut()
                .expect("primary alignment still exists");
            primary.filter_editor = filter_editor;
            primary.active_filters = active_filters;
        } else {
            self.apply_primary_alignment_config(None, self.workspace_alignment_limit)?;
        }
        Ok(())
    }

    pub(crate) fn remove_alignment_track(&mut self, id: TrackId) -> Result<bool> {
        if id == SYNTHETIC_ALIGNMENT_TRACK_ID {
            return Ok(false);
        }
        let Some(index) = self
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == id)
        else {
            return Ok(false);
        };
        let removed_was_primary = index == 0;
        let removed_track = self.alignment_tracks.remove(index);
        self.retire_alignment_track_controller(removed_track);
        self.reset_alignment_scene_panel_authority();
        self.track_presentation.remove_source(id);
        self.normalize_alignment_track_groups();
        if self
            .promoted_read
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.promoted_read = None;
        }
        self.metadata_selected_tracks.remove(&id);
        if matches!(
            self.open_panel,
            Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(track_id))) if track_id == id
        ) {
            self.open_panel = None;
        }
        if self.resizing_annotation_track.is_some_and(
            |(key, _)| matches!(key, TrackFrameKey::Alignment(track_id) if track_id == id),
        ) {
            self.resizing_annotation_track = None;
        }
        if self
            .resizing_read_track
            .is_some_and(|(track_id, _)| track_id == id)
        {
            self.resizing_read_track = None;
        }
        if self
            .vertical_overview_drag
            .is_some_and(|drag| drag.track_id == id)
        {
            self.vertical_overview_drag = None;
        }
        if removed_was_primary {
            self.activate_first_alignment_track()?;
        }
        for panel in self.panels.iter_mut() {
            clear_removed_alignment_panel_state(panel, id, removed_was_primary);
        }
        for track in &self.alignment_tracks {
            for projection in track.projections.values() {
                projection.advance_scene_generation();
            }
        }
        self.invalidate_navigation_contract("alignment source removed");
        self.begin_all_panel_refresh_transactions();
        self.request_all_alignment_queries();
        self.rebuild_all_panel_displays();
        self.mark_display_dirty("explicit display invalidation");
        Ok(true)
    }
}

/// Narrows a lane rectangle to the exact horizontal portion owned by a retained snapshot. The
/// integer coverage certificate may begin before or end after a fractional viewport edge, so the
/// result is clamped in camera space rather than treating the enclosing interval as fully live.
pub(crate) fn alignment_scene_covered_clip(
    viewport: Viewport,
    clip: CpuClipRect,
    covered: GenomicInterval,
) -> Option<CpuClipRect> {
    if covered.contig() != viewport.interval().contig() {
        return None;
    }
    let exact_start = viewport.exact_start();
    let exact_end = exact_start + viewport.span();
    let covered_start = (covered.start() as f64).max(exact_start);
    let covered_end = (covered.end() as f64).min(exact_end);
    if covered_start >= covered_end {
        return None;
    }
    let bases_per_pixel = viewport.bases_per_pixel();
    let clip_right = clip.x + clip.width;
    let left = (clip.x + (covered_start - exact_start) / bases_per_pixel).clamp(clip.x, clip_right);
    let right = (clip.x + (covered_end - exact_start) / bases_per_pixel).clamp(left, clip_right);
    (right > left).then_some(CpuClipRect {
        x: left,
        y: clip.y,
        width: right - left,
        height: clip.height,
    })
}

pub(crate) fn alignment_scene_gpu_scissor(
    camera: AlignmentCamera,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> Option<AlignmentGpuScissor> {
    alignment_clip_gpu_scissor(camera.clip(), scale_factor, surface_width, surface_height)
}

pub(crate) fn alignment_clip_gpu_scissor(
    clip: CpuClipRect,
    scale_factor: f32,
    surface_width: u32,
    surface_height: u32,
) -> Option<AlignmentGpuScissor> {
    let scale = scale_factor.max(f32::EPSILON) as f64;
    let left = (clip.x * scale)
        .round()
        .clamp(0.0, f64::from(surface_width)) as u32;
    let top = (clip.y * scale)
        .round()
        .clamp(0.0, f64::from(surface_height)) as u32;
    let right = ((clip.x + clip.width) * scale)
        .round()
        .clamp(f64::from(left), f64::from(surface_width)) as u32;
    let bottom = ((clip.y + clip.height) * scale)
        .round()
        .clamp(f64::from(top), f64::from(surface_height)) as u32;
    (right > left && bottom > top).then_some(AlignmentGpuScissor {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// A retained snapshot from a discontinuous locus is not a row-count or render authority for the
/// new camera. Source workers may truthfully attach that old immutable snapshot while acquiring
/// the replacement; the desktop keeps the progress frame but strips the irrelevant scene.
pub(crate) fn alignment_scene_frame_for_ui(
    frame: AlignmentCoordinatorFrame,
) -> AlignmentCoordinatorFrame {
    let retained_interval = frame
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.reads().atlas().interval());
    alignment_scene_frame_for_ui_against_retained_interval(frame, retained_interval)
}

/// Normalizes a worker progress frame against the interval carried by its retained snapshot. This
/// decomposition keeps the discontinuous-locus rule directly testable without constructing the
/// heavyweight immutable scene graph.
pub(crate) fn alignment_scene_frame_for_ui_against_retained_interval(
    mut frame: AlignmentCoordinatorFrame,
    retained_interval: Option<GenomicInterval>,
) -> AlignmentCoordinatorFrame {
    let camera_refusal_is_terminal = frame.individual_reads_refused_for_camera();
    let relevant = retained_interval.is_none_or(|interval| {
        alignment_scene_intervals_share_authority(interval, frame.camera.interval())
    });
    if !relevant {
        frame.snapshot = None;
        frame.row_activity = None;
        frame.covered_interval = None;
        frame.covered_rows = 0..0;
        frame.requested_rows = frame.camera.visible_rows();
        // The requirement describes the new desired camera, not the discarded old snapshot. It
        // must survive so the desktop can request and install the exact FASTA horizon.
        frame.scene_contracts_current = false;
        frame.reference_products_current = false;
        if !camera_refusal_is_terminal {
            frame.individual_reads = AlignmentIndividualReadCapability::Pending;
        }
        frame.vertical_residency = AlignmentVerticalResidency::ViewportBounded;
    }
    frame
}

pub(crate) fn alignment_scene_intervals_share_authority(
    atlas: GenomicInterval,
    camera: GenomicInterval,
) -> bool {
    atlas.contig() == camera.contig() && atlas.overlaps(camera)
}

#[cfg(test)]
mod gpu_publication_retry_tests {
    use super::*;

    #[test]
    fn contract_replacement_retires_only_its_reference_install_receipt() {
        let first = SourceOwnerId::new(7);
        let second = SourceOwnerId::new(8);
        let revision = ReferenceRevision::new(RevisionFingerprint::new(1, 2));
        let interval = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let mut receipts = BTreeMap::from([
            (first, (revision, interval)),
            (second, (revision, interval)),
        ]);

        assert!(retire_reference_install_receipt_for_contract_epoch(
            &mut receipts,
            first,
        ));
        assert!(!receipts.contains_key(&first));
        assert_eq!(receipts.get(&second), Some(&(revision, interval)));
        assert!(!retire_reference_install_receipt_for_contract_epoch(
            &mut receipts,
            first,
        ));
    }

    fn target(workspace_layout_revision: u64) -> AlignmentSceneTargetIdentity {
        AlignmentSceneTargetIdentity {
            workspace_layout_revision,
            surface_size: [1_200, 800],
            scale_factor: 2.0,
            panels: Vec::new(),
        }
    }

    fn waiting_retry(
        attempt: &AlignmentScenePublicationAttempt,
        epoch: LedgerProgressEpoch,
    ) -> AlignmentGpuPublicationRetry {
        AlignmentGpuPublicationRetry {
            target_revision: attempt.target_revision,
            attempt_sequence: attempt.sequence,
            blocker: AlignmentGpuPublicationBlocker::Ledger(
                LedgerError::ReplacementTransitionActive { bundle_id: 41 },
            ),
            gate: AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(epoch),
        }
    }

    fn assert_waiting(poll: AlignmentGpuPublicationRetryPoll) -> AlignmentGpuPublicationRetry {
        match poll {
            AlignmentGpuPublicationRetryPoll::Waiting(retry) => retry,
            other => panic!("expected a waiting retry, got {other:?}"),
        }
    }

    fn active_replacement(
        ledger: &AllocationLedger,
    ) -> (rgv_alignment_scene::AllocationBundleReservation, u64) {
        let snapshot = ledger.snapshot().unwrap();
        let reservation = ledger
            .reserve_replacement_bundle(AllocationBundleSpec::new(
                snapshot.policy_revision,
                Vec::new(),
                ledger.empty_retirement_claim_set(),
            ))
            .unwrap();
        let bundle_id = ledger
            .snapshot()
            .unwrap()
            .active_replacement_bundle
            .unwrap();
        (reservation, bundle_id)
    }

    fn retained_allocation(
        ledger: &AllocationLedger,
        bytes: usize,
    ) -> rgv_alignment_scene::AllocationLease {
        let mut reservation = ledger
            .reserve(
                rgv_alignment_scene::AllocationClass::GpuPage,
                bytes,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        reservation.register(bytes, 1).unwrap()
    }

    #[test]
    fn cache_ownership_handoff_permits_one_retry_without_faking_released_bytes() {
        let ledger = AllocationLedger::new(1_000, 500);
        let _retained = retained_allocation(&ledger, 900);
        let snapshot = ledger.progress().unwrap();
        let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
            requested: 200,
            charged: 900,
            reserved: 0,
            limit: 1_000,
        });
        assert!(alignment_gpu_publication_blocker_remains(
            blocker, &snapshot
        ));
        assert!(!alignment_capacity_reclaim_allows_retry(
            blocker, &snapshot, 0
        ));
        assert!(alignment_capacity_reclaim_allows_retry(
            blocker, &snapshot, 1
        ));
    }

    #[test]
    fn steady_replacement_retry_preserves_exact_retirement_credit() {
        let ledger = AllocationLedger::new(1_000, 500);
        let retained = retained_allocation(&ledger, 900);
        let releasable = retained_allocation(&ledger, 100);
        ledger.set_limits(800, 500, 800).unwrap();
        let pressure = GpuBudgetPressure::from_steady_replacement_failure(500, 1_000, 600, 800);
        let blocker = AlignmentGpuPublicationBlocker::AggregateGpu(pressure);

        let refused = ledger.progress().unwrap();
        assert!(alignment_gpu_publication_blocker_remains(blocker, &refused));
        // The exact fit point is 900 bytes: 600 bytes of the old epoch retire with the successor.
        // Reclaim deliberately continues to a 400-byte low-water mark, leaving one additional
        // measured 500-byte foreground bundle of headroom after this replacement.
        assert_eq!(
            alignment_gpu_pressure_target_charged(pressure, &refused),
            400
        );

        drop(releasable);
        let fitting = ledger.progress().unwrap();
        assert_eq!(fitting.charged, 900);
        assert!(!alignment_gpu_publication_blocker_remains(
            blocker, &fitting
        ));
        assert_eq!(
            alignment_gpu_pressure_target_charged(pressure, &fitting),
            400
        );
        drop(retained);
    }

    #[test]
    fn unchanged_ledger_epoch_never_turns_a_refusal_into_a_frame_rate_retry() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (_reservation, bundle_id) = active_replacement(&ledger);
        let snapshot = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
                bundle_id,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &snapshot),
        };

        assert_eq!(
            assert_waiting(alignment_gpu_publication_retry_poll(
                retry, 7, 11, &snapshot
            )),
            retry
        );
        assert_eq!(
            assert_waiting(alignment_gpu_publication_retry_poll(
                retry,
                7,
                11,
                &ledger.snapshot().unwrap()
            )),
            retry
        );
    }

    #[test]
    fn same_target_refresh_cannot_bypass_or_invalidate_a_waiting_attempt() {
        let ledger = AllocationLedger::new(1_024, 128);
        let snapshot = ledger.snapshot().unwrap();
        let epoch = snapshot.progress_epoch();
        let target = target(1);
        let mut authority = AlignmentScenePublicationAuthority::default();
        let mut retry = None;
        let deferred_attempt =
            begin_alignment_gpu_publication_attempt(&mut authority, &mut retry, &target, &snapshot)
                .unwrap();
        retry = Some(waiting_retry(&deferred_attempt, epoch));
        let retained = retry;

        assert!(
            begin_alignment_gpu_publication_attempt(
                &mut authority,
                &mut retry,
                &target,
                &snapshot,
            )
                .is_none()
        );
        assert_eq!(retry, retained);
        assert_eq!(authority.attempt_sequence(), deferred_attempt.sequence);
        assert_eq!(
            authority.desired_target_revision(),
            deferred_attempt.target_revision
        );
    }

    #[test]
    fn changed_target_supersedes_a_waiting_attempt_without_ledger_progress() {
        let ledger = AllocationLedger::new(1_024, 128);
        let snapshot = ledger.snapshot().unwrap();
        let epoch = snapshot.progress_epoch();
        let first_target = target(1);
        let second_target = target(2);
        let mut authority = AlignmentScenePublicationAuthority::default();
        let mut retry = None;
        let deferred_attempt = begin_alignment_gpu_publication_attempt(
            &mut authority,
            &mut retry,
            &first_target,
            &snapshot,
        )
        .unwrap();
        retry = Some(waiting_retry(&deferred_attempt, epoch));

        let changed = begin_alignment_gpu_publication_attempt(
            &mut authority,
            &mut retry,
            &second_target,
            &snapshot,
        )
        .unwrap();
        assert!(retry.is_none());
        assert_eq!(changed.sequence, deferred_attempt.sequence + 1);
        assert!(authority.desired_target_matches(&second_target));
        assert_ne!(changed.target_revision, deferred_attempt.target_revision);
    }

    #[test]
    fn same_target_retry_starts_once_after_genuine_ledger_progress() {
        let ledger = AllocationLedger::new(1_024, 128);
        let refused = ledger.snapshot().unwrap();
        let refused_epoch = refused.progress_epoch();
        let target = target(1);
        let mut authority = AlignmentScenePublicationAuthority::default();
        let mut retry = None;
        let deferred_attempt =
            begin_alignment_gpu_publication_attempt(&mut authority, &mut retry, &target, &refused)
                .unwrap();
        retry = Some(waiting_retry(&deferred_attempt, refused_epoch));
        ledger.set_budget(2_048).unwrap();

        let retried = begin_alignment_gpu_publication_attempt(
            &mut authority,
            &mut retry,
            &target,
            &ledger.snapshot().unwrap(),
        )
        .unwrap();
        assert!(retry.is_none());
        assert_eq!(retried.sequence, deferred_attempt.sequence + 1);
        assert_eq!(retried.target_revision, deferred_attempt.target_revision);
    }

    #[test]
    fn replacement_settlement_advances_progress_and_wakes_exactly_the_current_attempt() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (reservation, bundle_id) = active_replacement(&ledger);
        let refused = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
                bundle_id,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        drop(reservation);
        let settled = ledger.snapshot().unwrap();
        assert_eq!(
            alignment_gpu_publication_retry_poll(retry, 7, 11, &settled),
            AlignmentGpuPublicationRetryPoll::Ready
        );
        assert_eq!(
            alignment_gpu_publication_retry_poll(retry, 8, 11, &settled),
            AlignmentGpuPublicationRetryPoll::Superseded
        );
        assert_eq!(
            alignment_gpu_publication_retry_poll(retry, 7, 12, &settled),
            AlignmentGpuPublicationRetryPoll::Superseded
        );
    }

    #[test]
    fn unrelated_pin_churn_cannot_bypass_the_same_active_replacement_bundle() {
        let ledger = AllocationLedger::new(1_024, 128);
        let lease = retained_allocation(&ledger, 64);
        let (reservation, bundle_id) = active_replacement(&ledger);
        let refused = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
                bundle_id,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        let pin = lease.pin().unwrap();
        let churned = ledger.snapshot().unwrap();
        assert_ne!(churned.progress_epoch(), refused.progress_epoch());
        let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
        assert_eq!(
            updated.gate,
            AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
        );

        drop(reservation);
        let settled = ledger.snapshot().unwrap();
        assert_eq!(
            alignment_gpu_publication_retry_poll(updated, 7, 11, &settled),
            AlignmentGpuPublicationRetryPoll::Ready
        );
        drop(pin);
    }

    #[test]
    fn already_settled_replacement_race_is_ready_without_waiting_for_another_epoch() {
        let ledger = AllocationLedger::new(1_024, 128);
        let (reservation, bundle_id) = active_replacement(&ledger);
        drop(reservation);
        let post_error = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::ReplacementTransitionActive {
                bundle_id,
            });

        assert_eq!(
            alignment_gpu_publication_retry_gate(blocker, &post_error),
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        );
    }

    #[test]
    fn transient_release_advances_progress_before_retrying_the_current_attempt() {
        let ledger = AllocationLedger::new_with_transient_budget(1_024, 128, 1_024);
        let transient = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::GpuUpload,
                1_000,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let refused = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
                class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
                requested: 100,
                transient: 1_000,
                limit: 1_024,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };
        assert_eq!(
            assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &refused)),
            retry
        );

        drop(transient);
        assert_eq!(
            alignment_gpu_publication_retry_poll(retry, 7, 11, &ledger.snapshot().unwrap()),
            AlignmentGpuPublicationRetryPoll::Ready
        );
    }

    #[test]
    fn partial_transient_release_remains_waiting_until_the_request_fits() {
        let ledger = AllocationLedger::new_with_transient_budget(1_024, 128, 1_024);
        let large = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::GpuUpload,
                900,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let small = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::GpuUpload,
                100,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let refused = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
                class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
                requested: 200,
                transient: 1_000,
                limit: 1_024,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        drop(small);
        let partial = ledger.snapshot().unwrap();
        let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &partial));
        assert_eq!(
            updated.gate,
            AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(partial.progress_epoch())
        );

        drop(large);
        assert_eq!(
            alignment_gpu_publication_retry_poll(updated, 7, 11, &ledger.snapshot().unwrap()),
            AlignmentGpuPublicationRetryPoll::Ready
        );
    }

    #[test]
    fn unrelated_pin_churn_keeps_exact_retained_pressure_waiting() {
        let ledger = AllocationLedger::new(1_100, 128);
        let lease = retained_allocation(&ledger, 1_000);
        let refused = ledger.snapshot().unwrap();
        let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
            requested: 200,
            charged: 1_000,
            reserved: 0,
            limit: 1_100,
        });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        let pin = lease.pin().unwrap();
        let churned = ledger.snapshot().unwrap();
        assert_ne!(churned.progress_epoch(), refused.progress_epoch());
        let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
        assert_eq!(
            updated.gate,
            AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
        );
        drop(pin);
    }

    #[test]
    fn unrelated_charge_churn_keeps_a_retained_refusal_waiting_until_it_fits() {
        let ledger = AllocationLedger::new(1_500, 128);
        let original = retained_allocation(&ledger, 1_000);
        let refused = ledger.snapshot().unwrap();
        let blocker = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
            requested: 600,
            charged: 1_000,
            reserved: 0,
            limit: 1_500,
        });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        let unrelated = retained_allocation(&ledger, 100);
        let churned = ledger.snapshot().unwrap();
        let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
        assert_eq!(
            updated.gate,
            AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
        );

        drop(unrelated);
        drop(original);
        let fitting = ledger.snapshot().unwrap();
        assert_eq!(
            alignment_gpu_publication_retry_poll(updated, 7, 11, &fitting),
            AlignmentGpuPublicationRetryPoll::Ready
        );
    }

    #[test]
    fn unrelated_pin_churn_keeps_exact_transient_pressure_waiting() {
        let ledger = AllocationLedger::new_with_transient_budget(2_048, 128, 1_024);
        let lease = retained_allocation(&ledger, 64);
        let transient = ledger
            .reserve_transient(
                rgv_alignment_scene::TransientResourceClass::GpuUpload,
                1_000,
                rgv_alignment_scene::ReservationKind::Normal,
            )
            .unwrap();
        let refused = ledger.snapshot().unwrap();
        let blocker =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
                class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
                requested: 100,
                transient: 1_000,
                limit: 1_024,
            });
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &refused),
        };

        let pin = lease.pin().unwrap();
        let churned = ledger.snapshot().unwrap();
        assert_ne!(churned.progress_epoch(), refused.progress_epoch());
        let updated = assert_waiting(alignment_gpu_publication_retry_poll(retry, 7, 11, &churned));
        assert_eq!(
            updated.gate,
            AlignmentGpuPublicationRetryGate::AwaitingLedgerProgress(churned.progress_epoch())
        );
        drop(pin);
        drop(transient);
    }

    #[test]
    fn raced_budget_policy_or_occupancy_change_is_observed_before_retry_recording() {
        let ledger = AllocationLedger::new(2_048, 128);
        let post_error = ledger.snapshot().unwrap();
        let old_policy = AlignmentGpuPublicationBlocker::Ledger(LedgerError::BudgetExceeded {
            requested: 100,
            charged: 1_000,
            reserved: 0,
            limit: 1_024,
        });
        let old_occupancy =
            AlignmentGpuPublicationBlocker::Ledger(LedgerError::TransientBudgetExceeded {
                class: rgv_alignment_scene::TransientResourceClass::GpuUpload,
                requested: 100,
                transient: 1_000,
                limit: 1_024,
            });

        assert_eq!(
            alignment_gpu_publication_retry_gate(old_policy, &post_error),
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        );
        assert_eq!(
            alignment_gpu_publication_retry_gate(old_occupancy, &post_error),
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        );
    }

    #[test]
    fn allocation_policy_revision_race_is_typed_and_ready_to_replan_once() {
        let ledger = AllocationLedger::new(1_024, 128);
        ledger.set_budget(2_048).unwrap();
        let snapshot = ledger.snapshot().unwrap();
        let error = AllocationBundleError::PolicyRevisionChanged {
            planned: snapshot.policy_revision - 1,
            current: snapshot.policy_revision,
        };
        let blocker = alignment_gpu_bundle_blocker(&error).unwrap();
        assert!(matches!(
            blocker,
            AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. }
        ));
        assert_eq!(
            alignment_gpu_publication_retry_gate(blocker, &snapshot),
            AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
        );
        let retry = AlignmentGpuPublicationRetry {
            target_revision: 7,
            attempt_sequence: 11,
            blocker,
            gate: alignment_gpu_publication_retry_gate(blocker, &snapshot),
        };
        assert_eq!(
            alignment_gpu_publication_retry_poll(retry, 7, 11, &snapshot),
            AlignmentGpuPublicationRetryPoll::Ready
        );

        // Renderer-plan/reservation disagreement cannot be caused by a later ledger edit: both
        // values are already immutable tokens at this point, so it remains an invariant failure.
        assert!(
            alignment_read_replacement_blocker(
                &AlignmentGpuReplacementError::BundlePolicyRevision {
                    planned: 1,
                    reserved: 2,
                }
            )
            .is_none()
        );
        assert!(
            alignment_summary_replacement_blocker(
                &AlignmentSummaryGpuReplacementError::BundlePolicyRevision {
                    planned: 1,
                    reserved: 2,
                }
            )
            .is_none()
        );

        // If either renderer surfaces the actual allocation-bundle race, it retains the typed
        // recoverable classification through the wrapper.
        assert!(matches!(
            alignment_read_replacement_blocker(&AlignmentGpuReplacementError::Bundle(error)),
            Some(AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. })
        ));
        assert!(matches!(
            alignment_summary_replacement_blocker(&AlignmentSummaryGpuReplacementError::Bundle(
                error
            )),
            Some(AlignmentGpuPublicationBlocker::PolicyRevisionChanged { .. })
        ));
    }
}
