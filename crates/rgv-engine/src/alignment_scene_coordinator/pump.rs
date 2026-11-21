use std::sync::Arc;

use rgv_alignment_scene::{LedgerError, PresentationStorePressure, SceneRevision, StorePressure};
use rgv_core::GenomicInterval;

use crate::{AlignmentSourceActorEvent, SourceIntentKind, SourceIntentSequence, SourceOwnerId};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentCoordinatorUpdate {
    pub camera_sequence: u64,
    pub published_scene: Option<SceneRevision>,
    pub foreground_submitted: Option<SourceIntentSequence>,
    pub speculation_submitted: Option<SourceIntentSequence>,
    pub visible_complete: bool,
    pub reused_current_scene: bool,
    /// Byte-derived aggregate capacity policy calculated for this exact latest demand.
    pub capacity_plan: Option<AlignmentForegroundCapacityPlan>,
    /// A disjoint transition must establish this low-water mark before old-locus cache ownership
    /// can compete with foreground work. `None` means no proactive reclaim is currently needed.
    pub proactive_reclaim_target: Option<usize>,
}

/// Constant-size explanation of one foreground admission/reclaim decision.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentForegroundCapacityPlan {
    pub budget: usize,
    pub charged: usize,
    pub pinned: usize,
    pub reserved: usize,
    /// Measured upper envelope of a complete visible+near CPU/GPU successor at this island.
    pub foreground_reserve_bytes: usize,
    pub low_water_target: usize,
    pub return_cache_bytes: usize,
    pub disposable_speculation_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct AlignmentCoordinatorPump {
    pub source_event: Option<AlignmentSourceActorEvent>,
    pub published_scene: Option<SceneRevision>,
    pub refusal: Option<AlignmentSceneOwnerRefusal>,
    pub pressure: Option<AlignmentSceneOwnerPressure>,
    /// Terminal scene-build failure for this lane's current authority.
    pub failure: Option<AlignmentSceneOwnerFailure>,
    pub foreground_submitted: Option<SourceIntentSequence>,
    pub speculation_submitted: Option<SourceIntentSequence>,
}

/// One coordinated pressure pass over the source and presentation caches which share the same
/// enforcing allocation ledger. The published scene is pinned throughout; a non-zero remaining
/// overage therefore describes real live ownership rather than an eviction bookkeeping failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentCoordinatorPressure {
    pub budget: usize,
    pub speculation_was_pending: bool,
    pub active_speculation_aborted: bool,
    pub presentation: PresentationStorePressure,
    pub evidence: StorePressure,
    pub charged: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
    pub remaining_overage: usize,
}

/// One eviction-only pass across the source and semantic-page owners which share the alignment
/// ledger. Unlike [`AlignmentCoordinatorPressure`], this never edits the configured budget: it
/// merely asks this coordinator to release every legal inactive allocation until the aggregate
/// ledger reaches `target_charged` or only protected ownership remains.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentCoordinatorEviction {
    pub target_charged: usize,
    pub speculation_was_pending: bool,
    pub active_speculation_aborted: bool,
    pub presentation: PresentationStorePressure,
    pub evidence: StorePressure,
    pub charged_before: usize,
    pub charged: usize,
    pub pinned: usize,
    pub released: usize,
    /// Reloadable cache `Arc`s relinquished even when the currently presented epoch's pins kept
    /// their allocations charged. A non-zero value is ownership progress: the next aggregate
    /// replacement may be able to prove those allocations retire atomically with the old epoch.
    pub relinquished_cache_entries: usize,
    pub remaining: usize,
    /// Cache entries processed by this bounded maintenance quantum.
    pub quantum_entries: usize,
    pub quanta: u64,
    pub elapsed_nanoseconds: u64,
    /// Whether the target was reached or every legal inactive owner was exhausted.
    pub reclaim_complete: bool,
}

/// Source-level coordinator event. A source actor is shared by all panel owners; publication is
/// therefore explicitly attributed to the lane whose latest intent completed.
#[derive(Clone, Debug)]
pub struct AlignmentSceneSourcePump {
    pub source_event: Option<AlignmentSourceActorEvent>,
    pub publications: Vec<AlignmentSceneOwnerPublication>,
    /// Owners whose successful source transaction reached a terminal reconciliation checkpoint
    /// without publishing a new scene. Their externally visible frame may nevertheless have
    /// changed—for example, reference-backed filtering can discover its exact FASTA horizon only
    /// after all overlapping read spans are known. Threaded drivers must republish these frames
    /// so liveness ownership can transfer away from an idle source worker.
    pub source_settlements: Vec<SourceOwnerId>,
    pub refusals: Vec<AlignmentSceneOwnerRefusal>,
    pub pressures: Vec<AlignmentSceneOwnerPressure>,
    /// Owners whose own scene candidate could not be built for a reason other than allocation
    /// pressure. Each is terminal for that owner's current authority only; the shared source and
    /// every other owner keep working.
    pub failures: Vec<AlignmentSceneOwnerFailure>,
    pub foreground_submitted: Option<SourceIntentSequence>,
    pub speculation_submitted: Option<SourceIntentSequence>,
}

/// One bounded presentation checkpoint. Source work is kept separate so the engine can always
/// drain a newer camera intent before starting the next semantic row block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentPresentationPump {
    pub owner: SourceOwnerId,
    /// Foreground row-camera work publishes one exact bounded scene; independent warm-up leaves
    /// this empty and changes cache residency only.
    pub published_scene: Option<SceneRevision>,
    pub intent: SourceIntentKind,
    pub budget_limited: bool,
    /// An unexpected optional warm-up failure was latched without invalidating the visible scene.
    pub failed: bool,
    pub completed: bool,
    /// Allocation pressure which paused this optional vertical-residency checkpoint. The current
    /// immutable scene remains authoritative; callers may resume the lane after shared-ledger
    /// progress instead of treating the source as unavailable.
    pub pressure: Option<LedgerError>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerPublication {
    pub owner: SourceOwnerId,
    pub revision: SceneRevision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerRefusal {
    pub owner: SourceOwnerId,
    pub target: GenomicInterval,
    pub complete_read_limit: usize,
    pub eligible_records: u64,
}

/// A scene-root admission refusal attributed to the live owner and consumer intent which required
/// it. Unlike a decoder failure, this is not evidence that the shared source is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerPressure {
    pub owner: SourceOwnerId,
    pub target: GenomicInterval,
    pub intent: SourceIntentKind,
    pub pressure: LedgerError,
}

/// A scene-build failure attributed to the one owner whose candidate failed. It is not evidence
/// that the shared source is unavailable and never cancels another owner's work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerFailure {
    pub owner: SourceOwnerId,
    pub message: Arc<str>,
}

/// A refused foreground lane which was returned to `Pending` after its admission policy changed.
/// The replacement source intent is queued behind the already-enqueued non-discardable control,
/// so it cannot decode under the superseded policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerRetry {
    pub owner: SourceOwnerId,
    pub foreground_submitted: SourceIntentSequence,
}
