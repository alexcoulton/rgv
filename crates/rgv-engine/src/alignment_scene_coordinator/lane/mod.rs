use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
    sync::Arc,
    time::Instant,
};

use rgv_alignment_scene::{
    AlignmentPresentationOwner, AlignmentRowActivityCursor, AlignmentRowActivityProfile,
    CanonicalRowBlock, EvidenceRevision, LedgerProgress, PresentationContract, RowAtlasRevision,
    SceneRevision,
};
use rgv_core::GenomicInterval;

use crate::SourceOwnerId;

mod camera;
mod foreground_rows;
mod frame;
mod reconcile;
mod reference;
mod scene_build;
mod source_intent;
mod vertical_residency;

pub(crate) use camera::*;
pub(crate) use reconcile::*;
#[cfg(test)]
pub(crate) use reference::required_reference_horizon;
pub(crate) use scene_build::{SceneBuild, SceneBuildProgress};
#[cfg(test)]
pub(crate) use vertical_residency::{
    next_unwarmed_vertical_block, supports_full_vertical_residency,
};

use super::{
    ALLELE_REFERENCE_CONTEXT_RADIUS, AlignmentCoordinatorFrame, AlignmentCoordinatorUpdate,
    AlignmentIndividualReadCapability, AlignmentPresentationPump, AlignmentReferenceAuthority,
    AlignmentReferenceInstallOutcome, AlignmentReferenceRequirement, AlignmentRowActivityTelemetry,
    AlignmentSceneBuildContract, AlignmentSceneCoordinatorConfigError,
    AlignmentSceneCoordinatorError, AlignmentSceneCoordinatorTelemetry, AlignmentSceneOwnerRefusal,
    AlignmentScenePreparationPolicy, AlignmentVerticalResidency,
    AlignmentVerticalResidencyPumpOutcome, AlignmentVerticalResidencyPumpTelemetry,
    AllocationPressure, CameraDirection, DesiredScene, alignment_row_activity_satisfies_contract,
    alignment_row_atlas_satisfies_contract, coordinator_budget_pressure,
    scene_build_failure_message,
};

#[derive(Debug)]
pub(crate) struct AlignmentSceneSourceLane {
    pub(crate) contract: AlignmentSceneBuildContract,
    pub(crate) policy: AlignmentScenePreparationPolicy,
    pub(crate) desired: Option<DesiredScene>,
    pub(crate) previous_exact_start: Option<f64>,
    pub(crate) next_camera_sequence: u64,
    pub(crate) telemetry: AlignmentSceneCoordinatorTelemetry,
    /// Terminal refusal for the exact visible camera population.
    pub(crate) refusal: Option<AlignmentSceneOwnerRefusal>,
    /// Optional margin which exceeded its own bounded acquisition policy. This suppresses only
    /// an identical speculative retry and is cleared by a new source horizon.
    pub(crate) speculation_refusal: Option<GenomicInterval>,
    /// Source horizon whose evidence alone exceeded the evidence owner's hard cap under the
    /// `Bounded` memory policy. The request was reported once; the same horizon is not decoded
    /// again until the horizon, the budget or an explicit retry changes the decision.
    pub(crate) foreground_pressure_refusal: Option<GenomicInterval>,
    pub(crate) vertical_residency_budget_limited: bool,
    /// Terminal only for optional full-height warming under the current scene authority. This
    /// prevents a deterministic semantic-page failure from spinning in the worker and from
    /// turning already complete visible reads into a failed foreground navigation.
    pub(crate) vertical_residency_failed: bool,
    pub(crate) vertical_read_residency: Option<VerticalReadResidency>,
    /// Foreground pointer/navigation ownership suppresses optional vertical semantic-page work.
    /// The latest state is mailbox-coalesced and takes effect at the next canonical block
    /// checkpoint without cancelling complete evidence or the visible committed scene.
    pub(crate) optional_work_paused: bool,
    /// At most one derived camera projection is retained per lane. Its full authority key keeps
    /// adjacent camera frames cheap without allowing an old atlas or locus to masquerade as the
    /// current scrollbar truth.
    pub(crate) row_activity_cache: RefCell<Option<AlignmentRowActivityCache>>,
    pub(crate) row_activity_telemetry: Cell<AlignmentRowActivityTelemetry>,
    /// Once broad evidence is resident we can derive the exact aligned span needed by
    /// reference-backed view/highlight predicates. Until then the ordinary source horizon is the
    /// truthful initial request; the expanded requirement is published on the next frame.
    pub(crate) filter_reference_horizon: Option<GenomicInterval>,
    /// Why the exact visible row block is not being rebuilt right now. Without this latch a
    /// refused or failing block would be retried on every worker iteration.
    pub(crate) foreground_row_pause: Option<ForegroundRowPresentationPause>,
    /// This lane's scene candidate failed for a reason other than allocation pressure. Source
    /// completions belonging to other owners must not rebuild (and re-report) it; a new camera,
    /// contract, policy, reference, or explicit retry clears it.
    pub(crate) scene_build_failed: bool,
    /// Whether deriving new roots from cached evidence is deferred to `pump_scene_build_once`
    /// (one stage per call) instead of running to completion inside the reconciling call.
    pub(crate) staged_scene_builds: bool,
    /// The staged build in flight, with everything it has reserved so far.
    pub(crate) scene_build: Option<SceneBuild>,
    /// Advanced by every contract or reference change; an in-flight build from an older value
    /// is superseded.
    pub(crate) contract_epoch: u64,
}

/// Everything a foreground row-block build depends on. A pause applies only while this is
/// unchanged, so a new scene, row range, interval, or presentation is always attempted afresh.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForegroundRowPresentationAuthority {
    pub(crate) scene: SceneRevision,
    pub(crate) presentation: PresentationContract,
    pub(crate) prepared_interval: GenomicInterval,
    pub(crate) rows: Range<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct ForegroundRowPresentationPause {
    /// `None` when the authority itself could not be derived; that state is equally stable.
    pub(crate) authority: Option<ForegroundRowPresentationAuthority>,
    pub(crate) cause: ForegroundRowPresentationPauseCause,
}

#[derive(Clone, Debug)]
pub(crate) enum ForegroundRowPresentationPauseCause {
    /// Paused until `RetryResourcePressure`, a camera/contract change, or the shared ledger
    /// gaining capacity relative to this snapshot taken after the refusal.
    Pressure(Option<Box<LedgerProgress>>),
    /// Terminal for this authority.
    Failed(Arc<str>),
}

/// Whether a refused allocation has become worth retrying: the policy changed, or occupancy
/// fell. Unrelated allocations elsewhere (which only raise occupancy) never resume the lane.
pub(crate) fn ledger_capacity_improved(paused: &LedgerProgress, now: &LedgerProgress) -> bool {
    now.policy_revision != paused.policy_revision
        || now.budget > paused.budget
        || now.transient_budget > paused.transient_budget
        || now.active_replacement_bundle != paused.active_replacement_bundle
        || now.charged.saturating_add(now.reserved) < paused.charged.saturating_add(paused.reserved)
        || now.transient < paused.transient
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerticalReadResidencyAuthority {
    pub(crate) evidence_revision: EvidenceRevision,
    pub(crate) atlas_revision: RowAtlasRevision,
    pub(crate) presentation: PresentationContract,
    pub(crate) prepared_interval: GenomicInterval,
    pub(crate) geometry_tile_bases: NonZeroU64,
    pub(crate) row_block_rows: NonZeroU32,
}

#[derive(Clone, Debug)]
pub(crate) struct VerticalReadResidency {
    pub(crate) authority: VerticalReadResidencyAuthority,
    pub(crate) warmed_blocks: BTreeSet<CanonicalRowBlock>,
}

#[derive(Clone, Debug)]
pub(crate) struct AlignmentRowActivityCache {
    pub(crate) atlas_revision: RowAtlasRevision,
    pub(crate) cursor: AlignmentRowActivityCursor,
    pub(crate) profile: Arc<AlignmentRowActivityProfile>,
}

pub(crate) const ALIGNMENT_ROW_ACTIVITY_BINS: usize = 128;

pub(crate) const fn presentation_owner(owner: SourceOwnerId) -> AlignmentPresentationOwner {
    AlignmentPresentationOwner::new(owner.get())
}

pub(crate) fn elapsed_nanoseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

pub(crate) fn usize_as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}
