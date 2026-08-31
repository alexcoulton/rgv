use std::{
    cell::RefCell,
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
    ops::Range,
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentPresentationOwner, AlignmentPresentationSceneError, AlignmentPresentationStore,
    AlignmentPresentationStoreError, AlignmentRowActivityProfile, AlignmentRowAtlasError,
    AlignmentSceneCamera, AlignmentSceneSnapshot, AlignmentSceneSnapshotError,
    AlignmentSourceRevision, AlleleSummaryDetail, CanonicalGenomicTile, ContractFingerprint,
    EvidenceBuildError, EvidenceSelectionError, EvidenceStoreError, ExactAlleleSummaryError,
    ExactDepthSummaryError, ExactFilterSummaryError, ExactJunctionSummaryError,
    ExactModificationSummaryError, ExactPairSummaryError, ExactSummaryRootError, GeometryLod,
    JunctionAnnotationIndex, LayoutContract, LedgerError, PayloadContract, PresentationContract,
    PresentationStorePressure, ReferenceRevision, ReservationKind, RowAtlasRevision, SceneRevision,
    SemanticPageError, StorePressure, SummaryContract, SummarySemanticPageError,
};
use rgv_core::{ContigId, GenomicInterval};
use rgv_hts::ReferenceSlice;
use thiserror::Error;

use crate::{
    AlignmentSourceActor, AlignmentSourceActorError, AlignmentSourceActorEvent,
    AlignmentSourceIntentSpec, CanonicalAlignmentSourceBackend, SourceIntentKind,
    SourceIntentSequence, SourceOwnerId, SourceScheduleError,
};

const ALLELE_REFERENCE_CONTEXT_RADIUS: u64 = 2;

/// A presentation reference carries its immutable identity beside the actual bases. Keeping the
/// revision separate from `ReferenceSlice` prevents a caller from pairing current bases with a
/// stale presentation contract during scene publication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentReferenceAuthority {
    revision: ReferenceRevision,
    slice: Arc<ReferenceSlice>,
}

impl AlignmentReferenceAuthority {
    #[must_use]
    pub const fn new(revision: ReferenceRevision, slice: Arc<ReferenceSlice>) -> Self {
        Self { revision, slice }
    }

    #[must_use]
    pub const fn revision(&self) -> ReferenceRevision {
        self.revision
    }

    #[must_use]
    pub const fn slice(&self) -> &Arc<ReferenceSlice> {
        &self.slice
    }
}

/// The exact reference dependency for the lane's currently desired source horizon. A pending
/// requirement is not a source failure: evidence, summaries, and layout may still be acquired and
/// retained while the reference engine catches up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentReferenceRequirement {
    #[default]
    NotRequired,
    Pending {
        revision: ReferenceRevision,
        horizon: GenomicInterval,
    },
    Ready {
        revision: ReferenceRevision,
        horizon: GenomicInterval,
    },
}

impl AlignmentReferenceRequirement {
    #[must_use]
    pub const fn is_ready(self) -> bool {
        !matches!(self, Self::Pending { .. })
    }
}

/// Result of offering immutable reference authority to one owner lane. Stale revisions and slices
/// which do not cover the lane's exact live horizon are ordinary ignored offers, not errors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentReferenceInstallOutcome {
    pub installed: bool,
    pub published_scene: Option<SceneRevision>,
}

/// Factorised authority and semantic contracts for one alignment lane. Camera state is
/// deliberately absent: pan and vertical scroll do not change this value or any page key.
#[derive(Clone, Debug)]
pub struct AlignmentSceneBuildContract {
    pub source_revision: AlignmentSourceRevision,
    pub record_contract: ContractFingerprint,
    pub contig: ContigId,
    pub contig_length: u64,
    pub payload: PayloadContract,
    pub summaries: SummaryContract,
    pub layout: LayoutContract,
    pub presentation: PresentationContract,
    pub geometry_tile_bases: NonZeroU64,
    pub summary_tile_bases: NonZeroU64,
    pub row_block_rows: NonZeroU32,
    pub normalization_mapped_records: Option<u64>,
    pub reference: Option<AlignmentReferenceAuthority>,
    pub annotation: Option<Arc<JunctionAnnotationIndex>>,
}

impl AlignmentSceneBuildContract {
    fn validate(&self) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        if self.contig_length == 0 {
            return Err(AlignmentSceneCoordinatorConfigError::EmptyContig);
        }
        if self
            .reference
            .as_ref()
            .is_some_and(|reference| self.presentation.reference != Some(reference.revision))
        {
            return Err(AlignmentSceneCoordinatorConfigError::ReferenceAuthorityMismatch);
        }
        if self.presentation.annotation != self.annotation.as_ref().map(|value| value.revision()) {
            return Err(AlignmentSceneCoordinatorConfigError::AnnotationAuthorityMismatch);
        }
        if let Some(reference) = &self.reference
            && reference.slice.interval.contig() != self.contig
        {
            return Err(AlignmentSceneCoordinatorConfigError::ReferenceCoverageMismatch);
        }
        Ok(())
    }
}

/// Proves that a camera-local row profile still describes the exact genomic row coordinate
/// space owned by `scene` under `contract`.
///
/// Visible-row coverage is deliberately absent from this predicate. Scrolling changes which
/// geometry pages are ready to draw, but it does not change the number, identity, or activity of
/// canonical rows at the same genomic interval.
#[must_use]
pub fn alignment_row_activity_satisfies_contract(
    profile: &AlignmentRowActivityProfile,
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    let atlas = scene.reads().atlas();
    alignment_row_atlas_satisfies_contract(scene, interval, contract)
        && profile.interval() == interval
        && profile.atlas_revision() == atlas.revision()
        && profile.row_coordinate_space() == atlas.row_coordinate_space()
        && profile.stable_row_count() == atlas.row_count()
}

fn alignment_row_atlas_satisfies_contract(
    scene: &AlignmentSceneSnapshot,
    interval: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    let reads = scene.reads();
    let evidence = reads.evidence();
    let atlas = reads.atlas();
    scene.prepared_interval().contains_interval(interval)
        && evidence.coverage().covers(interval)
        && atlas.interval().contains_interval(interval)
        && atlas.derived_from_evidence() == evidence.revision()
        && atlas.source_revision() == contract.source_revision
        && atlas.record_contract() == contract.record_contract
        && atlas.layout_contract() == contract.layout
}

/// User-adjustable preparation distances. The coordinator contains no hidden hard clamp; callers
/// choose these values within the shared allocation budget and measurements can later supply the
/// product defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AlignmentScenePreparationPolicy {
    pub behind_viewports: f64,
    pub ahead_viewports: f64,
    pub horizontal_trigger_viewports: f64,
    pub vertical_margin_rows: u32,
    pub vertical_trigger_rows: u32,
    /// After the visible/hot row window is published, incrementally retain the complete vertical
    /// Span/Read scene while the shared allocation ledger continues to admit it. This is a
    /// residency policy, not a request to draw offscreen rows.
    pub full_vertical_residency: bool,
}

impl AlignmentScenePreparationPolicy {
    pub fn new(
        behind_viewports: f64,
        ahead_viewports: f64,
        horizontal_trigger_viewports: f64,
        vertical_margin_rows: u32,
        vertical_trigger_rows: u32,
    ) -> Result<Self, AlignmentSceneCoordinatorConfigError> {
        let policy = Self {
            behind_viewports,
            ahead_viewports,
            horizontal_trigger_viewports,
            vertical_margin_rows,
            vertical_trigger_rows,
            full_vertical_residency: false,
        };
        policy.validate()?;
        Ok(policy)
    }

    #[must_use]
    pub const fn with_full_vertical_residency(mut self, enabled: bool) -> Self {
        self.full_vertical_residency = enabled;
        self
    }

    fn validate(self) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        if !self.behind_viewports.is_finite()
            || self.behind_viewports < 0.0
            || !self.ahead_viewports.is_finite()
            || self.ahead_viewports < 0.0
            || !self.horizontal_trigger_viewports.is_finite()
            || self.horizontal_trigger_viewports < 0.0
        {
            return Err(AlignmentSceneCoordinatorConfigError::InvalidPreparationDistance);
        }
        Ok(())
    }
}

/// Observable state of the optional complete vertical residency pass. `Complete` means that a
/// camera-only vertical move can select any row page from the published immutable scene without
/// source, layout, or semantic-page work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlignmentVerticalResidency {
    #[default]
    ViewportBounded,
    Preparing {
        prepared_rows: u32,
        total_rows: u32,
    },
    Complete {
        total_rows: u32,
    },
    BudgetLimited {
        prepared_rows: u32,
        total_rows: u32,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CameraDirection {
    #[default]
    Stationary,
    Left,
    Right,
}

#[derive(Clone, Debug)]
struct DesiredScene {
    camera: AlignmentSceneCamera,
    camera_sequence: u64,
    prepared_interval: GenomicInterval,
    source_horizon: GenomicInterval,
    prepared_rows: Option<Range<u32>>,
    direction: CameraDirection,
}

/// Current visible publication. A missing `covered_interval` or a strict subset means only that
/// edge is loading; the retained snapshot remains available and is never replaced by a blank
/// whole-lane sentinel.
#[derive(Clone, Debug)]
pub struct AlignmentCoordinatorFrame {
    pub camera: AlignmentSceneCamera,
    pub camera_sequence: u64,
    pub snapshot: Option<Arc<AlignmentSceneSnapshot>>,
    pub covered_interval: Option<GenomicInterval>,
    pub requested_rows: Range<u32>,
    pub covered_rows: Range<u32>,
    /// A camera-exact, bounded vertical activity summary derived from the immutable row atlas in
    /// `snapshot`. This is deliberately independent of atlas-wide vertical residency totals: the
    /// scrollbar describes rows active at the live locus while the cache may retain many more
    /// rows for immediate horizontal navigation.
    pub row_activity: Option<Arc<AlignmentRowActivityProfile>>,
    pub reference_requirement: AlignmentReferenceRequirement,
    /// Whether the retained snapshot was built from the lane's currently desired
    /// reference-independent presentation, summary, and layout contracts. Reference-backed
    /// interpretation has its own readiness bit below: a pending FASTA slice must not make
    /// truthful read geometry or exact depth stale.
    pub scene_contracts_current: bool,
    /// Whether every reference-dependent presentation product in the desired contract is also
    /// current. `false` is an optional-product state, not a reason to withhold the core scene.
    pub reference_products_current: bool,
    pub individual_reads: AlignmentIndividualReadCapability,
    pub vertical_residency: AlignmentVerticalResidency,
}

/// Constant-size proof of which worker queue can still advance one owner. A frame which is not
/// terminal and reports both fields false is ownerless rather than merely "still updating".
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneOwnerWorkState {
    pub source_pending: bool,
    pub presentation_pending: bool,
}

impl AlignmentCoordinatorFrame {
    #[must_use]
    pub fn visible_complete(&self) -> bool {
        self.scene_contracts_current
            && self
                .covered_interval
                .is_some_and(|covered| covered.contains_interval(self.camera.interval()))
            && self.covered_rows.start <= self.requested_rows.start
            && self.covered_rows.end >= self.requested_rows.end
    }

    /// Whether the source has made a terminal individual-read decision for this frame's camera.
    /// A retained refusal from another request is not authority merely because it is attached to
    /// a newer frame: its counted source target must contain the exact camera interval.
    #[must_use]
    pub fn individual_reads_refused_for_camera(&self) -> bool {
        matches!(
            self.individual_reads,
            AlignmentIndividualReadCapability::Refused { target, .. }
                if target.contains_interval(self.camera.interval())
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentIndividualReadCapability {
    Pending,
    Complete {
        scene_revision: SceneRevision,
    },
    Refused {
        target: GenomicInterval,
        complete_read_limit: usize,
        eligible_records: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentSceneCoordinatorTelemetry {
    pub camera_updates: u64,
    pub camera_only_updates: u64,
    pub source_work_started: u64,
    pub source_checkpoints: u64,
    pub source_retargets: u64,
    pub source_yields: u64,
    pub source_acquisitions: u64,
    pub source_refusals: u64,
    pub evidence_candidates: u64,
    pub exact_summary_candidates: u64,
    pub row_atlas_candidates: u64,
    pub presentation_candidates: u64,
    pub scene_publications: u64,
    pub foreground_submissions: u64,
    pub speculation_submissions: u64,
    pub resource_pressure_speculation_aborts: u64,
    pub vertical_residency_checkpoints: u64,
    pub vertical_residency_publications: u64,
    pub vertical_residency_completions: u64,
    pub vertical_residency_budget_limits: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentCoordinatorUpdate {
    pub camera_sequence: u64,
    pub published_scene: Option<SceneRevision>,
    pub foreground_submitted: Option<SourceIntentSequence>,
    pub speculation_submitted: Option<SourceIntentSequence>,
    pub visible_complete: bool,
    pub reused_current_scene: bool,
}

#[derive(Clone, Debug)]
pub struct AlignmentCoordinatorPump {
    pub source_event: Option<AlignmentSourceActorEvent>,
    pub published_scene: Option<SceneRevision>,
    pub refusal: Option<AlignmentSceneOwnerRefusal>,
    pub pressure: Option<AlignmentSceneOwnerPressure>,
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

/// Convenience façade for callers that display one lane from a source. It owns no scheduling,
/// cache, or publication algorithm of its own: every operation delegates to the same source-level
/// coordinator used by multi-panel desktop sessions.
pub struct AlignmentSceneCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    source: AlignmentSceneSourceCoordinator<B>,
    owner: SourceOwnerId,
}

impl<B> AlignmentSceneCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    pub fn new(
        actor: AlignmentSourceActor<B>,
        contract: AlignmentSceneBuildContract,
        policy: AlignmentScenePreparationPolicy,
        owner: SourceOwnerId,
    ) -> Result<Self, AlignmentSceneCoordinatorConfigError> {
        let mut source = AlignmentSceneSourceCoordinator::new(actor);
        source.insert_lane(owner, contract, policy)?;
        Ok(Self { source, owner })
    }

    pub fn update_camera(
        &mut self,
        camera: impl Into<AlignmentSceneCamera>,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.update_camera(self.owner, camera)
    }

    pub fn set_preparation_policy(
        &mut self,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<Option<AlignmentCoordinatorUpdate>, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.set_preparation_policy(self.owner, policy)
    }

    #[must_use]
    pub fn preparation_policy(&self) -> AlignmentScenePreparationPolicy {
        self.source
            .preparation_policy(self.owner)
            .expect("the single-lane façade always retains its owner")
    }

    pub fn pump_once(
        &mut self,
    ) -> Result<AlignmentCoordinatorPump, AlignmentSceneCoordinatorError<B::Error>> {
        let AlignmentSceneSourcePump {
            source_event,
            publications,
            refusals,
            pressures,
            foreground_submitted,
            speculation_submitted,
        } = self.source.pump_once()?;
        let published_scene = publications
            .into_iter()
            .find(|publication| publication.owner == self.owner)
            .map(|publication| publication.revision);
        let refusal = refusals
            .into_iter()
            .find(|refusal| refusal.owner == self.owner);
        let pressure = pressures
            .into_iter()
            .find(|pressure| pressure.owner == self.owner);
        Ok(AlignmentCoordinatorPump {
            source_event,
            published_scene,
            refusal,
            pressure,
            foreground_submitted,
            speculation_submitted,
        })
    }

    #[must_use]
    pub fn has_pending_presentation_work(&self) -> bool {
        self.source.has_pending_presentation_work()
    }

    pub fn pump_presentation_once(
        &mut self,
    ) -> Result<Option<AlignmentPresentationPump>, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.pump_presentation_once()
    }

    pub fn replace_contract(
        &mut self,
        contract: AlignmentSceneBuildContract,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.replace_contract(self.owner, contract)
    }

    pub fn prewarm_presentation(
        &mut self,
        presentation: PresentationContract,
    ) -> Result<bool, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.prewarm_presentation(self.owner, presentation)
    }

    /// Offers reference authority for the current owner and reconciles only from already
    /// resident/cache-backed source roots. This call never schedules or performs BAM acquisition.
    pub fn install_reference_authority(
        &mut self,
        authority: AlignmentReferenceAuthority,
    ) -> Result<AlignmentReferenceInstallOutcome, AlignmentSceneCoordinatorError<B::Error>> {
        self.source
            .install_reference_authority(self.owner, authority)
    }

    #[must_use]
    pub fn frame(&self) -> Option<AlignmentCoordinatorFrame> {
        self.source
            .frame(self.owner)
            .expect("the single-lane façade always retains its owner")
    }

    #[must_use]
    pub fn telemetry(&self) -> AlignmentSceneCoordinatorTelemetry {
        self.source
            .telemetry(self.owner)
            .expect("the single-lane façade always retains its owner")
    }

    #[must_use]
    pub fn contract(&self) -> &AlignmentSceneBuildContract {
        self.source
            .contract(self.owner)
            .expect("the single-lane façade always retains its owner")
    }

    #[must_use]
    pub fn desired_prepared_interval(&self) -> Option<GenomicInterval> {
        self.source
            .desired_prepared_interval(self.owner)
            .expect("the single-lane façade always retains its owner")
    }

    #[must_use]
    pub const fn owner(&self) -> SourceOwnerId {
        self.owner
    }

    #[must_use]
    pub const fn actor(&self) -> &AlignmentSourceActor<B> {
        self.source.actor()
    }

    pub const fn actor_mut(&mut self) -> &mut AlignmentSourceActor<B> {
        self.source.actor_mut()
    }

    #[must_use]
    pub const fn presentation(&self) -> &AlignmentPresentationStore {
        self.source.presentation()
    }

    pub fn reduce_budget(
        &mut self,
        budget: usize,
    ) -> Result<AlignmentCoordinatorPressure, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.reduce_budget(budget)
    }
}

/// Source-level coordinator event. A source actor is shared by all panel owners; publication is
/// therefore explicitly attributed to the lane whose latest intent completed.
#[derive(Clone, Debug)]
pub struct AlignmentSceneSourcePump {
    pub source_event: Option<AlignmentSourceActorEvent>,
    pub publications: Vec<AlignmentSceneOwnerPublication>,
    pub refusals: Vec<AlignmentSceneOwnerRefusal>,
    pub pressures: Vec<AlignmentSceneOwnerPressure>,
    pub foreground_submitted: Option<SourceIntentSequence>,
    pub speculation_submitted: Option<SourceIntentSequence>,
}

/// One bounded presentation checkpoint. Source work is kept separate so the engine can always
/// drain a newer camera intent before starting the next semantic row block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentPresentationPump {
    pub owner: SourceOwnerId,
    pub published_scene: Option<SceneRevision>,
    pub budget_limited: bool,
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

/// A refused foreground lane which was returned to `Pending` after its admission policy changed.
/// The replacement source intent is queued behind the already-enqueued non-discardable control,
/// so it cannot decode under the superseded policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneOwnerRetry {
    pub owner: SourceOwnerId,
    pub foreground_submitted: SourceIntentSequence,
}

/// One mutable source actor and one immutable page repository shared by every panel displaying
/// that source. Owner lanes hold only camera/contract/publication state. This is the production
/// ownership boundary: adding a panel never opens a second BAM/CRAM decoder or creates another
/// source cache authority.
pub struct AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    actor: AlignmentSourceActor<B>,
    presentation: AlignmentPresentationStore,
    lanes: BTreeMap<SourceOwnerId, AlignmentSceneSourceLane>,
    last_presentation_owner: Option<SourceOwnerId>,
}

#[derive(Clone, Debug)]
struct AlignmentSceneSourceLane {
    contract: AlignmentSceneBuildContract,
    policy: AlignmentScenePreparationPolicy,
    desired: Option<DesiredScene>,
    previous_exact_start: Option<f64>,
    next_camera_sequence: u64,
    telemetry: AlignmentSceneCoordinatorTelemetry,
    refusal: Option<AlignmentSceneOwnerRefusal>,
    vertical_residency_budget_limited: bool,
    /// At most one derived camera projection is retained per lane. Its full authority key keeps
    /// adjacent camera frames cheap without allowing an old atlas or locus to masquerade as the
    /// current scrollbar truth.
    row_activity_cache: RefCell<Option<AlignmentRowActivityCache>>,
}

#[derive(Clone, Debug)]
struct AlignmentRowActivityCache {
    atlas_revision: RowAtlasRevision,
    interval: GenomicInterval,
    profile: Arc<AlignmentRowActivityProfile>,
}

const ALIGNMENT_ROW_ACTIVITY_BINS: usize = 128;

impl<B> AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    #[must_use]
    pub fn new(actor: AlignmentSourceActor<B>) -> Self {
        let presentation = AlignmentPresentationStore::new(actor.store().ledger());
        Self {
            actor,
            presentation,
            lanes: BTreeMap::new(),
            last_presentation_owner: None,
        }
    }

    pub fn insert_lane(
        &mut self,
        owner: SourceOwnerId,
        contract: AlignmentSceneBuildContract,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        contract.validate()?;
        policy.validate()?;
        if contract.source_revision != self.actor.store().source_revision() {
            return Err(AlignmentSceneCoordinatorConfigError::SourceAuthorityMismatch);
        }
        if contract.record_contract != self.actor.store().record_contract() {
            return Err(AlignmentSceneCoordinatorConfigError::RecordAuthorityMismatch);
        }
        if self.lanes.contains_key(&owner) {
            return Err(AlignmentSceneCoordinatorConfigError::DuplicateOwner(owner));
        }
        self.lanes.insert(
            owner,
            AlignmentSceneSourceLane {
                contract,
                policy,
                desired: None,
                previous_exact_start: None,
                next_camera_sequence: 1,
                telemetry: AlignmentSceneCoordinatorTelemetry::default(),
                refusal: None,
                vertical_residency_budget_limited: false,
                row_activity_cache: RefCell::new(None),
            },
        );
        Ok(())
    }

    pub fn remove_lane(&mut self, owner: SourceOwnerId) -> bool {
        self.actor.cancel_foreground_for(owner);
        self.actor.cancel_speculation_for(owner);
        self.presentation.remove_owner(presentation_owner(owner));
        self.lanes.remove(&owner).is_some()
    }

    pub fn update_camera(
        &mut self,
        owner: SourceOwnerId,
        camera: impl Into<AlignmentSceneCamera>,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        let camera = camera.into();
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        lane.update_camera(owner, &mut self.actor, &mut self.presentation, camera, true)
    }

    pub fn set_preparation_policy(
        &mut self,
        owner: SourceOwnerId,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<Option<AlignmentCoordinatorUpdate>, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        policy.validate()?;
        lane.policy = policy;
        lane.vertical_residency_budget_limited = false;
        let Some(camera) = lane.desired.as_ref().map(|desired| desired.camera) else {
            return Ok(None);
        };
        lane.previous_exact_start = None;
        lane.update_camera(
            owner,
            &mut self.actor,
            &mut self.presentation,
            camera,
            false,
        )
        .map(Some)
    }

    pub fn replace_contract(
        &mut self,
        owner: SourceOwnerId,
        contract: AlignmentSceneBuildContract,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        lane.replace_contract(owner, &mut self.actor, &mut self.presentation, contract)
    }

    pub fn prewarm_presentation(
        &mut self,
        owner: SourceOwnerId,
        presentation: PresentationContract,
    ) -> Result<bool, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        lane.prewarm_presentation(owner, &mut self.presentation, presentation)
    }

    /// Installs immutable reference authority only when it matches this owner's declared
    /// revision and covers its exact live source horizon. A stale or short slice is a normal
    /// no-op. Reconciliation deliberately cannot submit source work.
    pub fn install_reference_authority(
        &mut self,
        owner: SourceOwnerId,
        authority: AlignmentReferenceAuthority,
    ) -> Result<AlignmentReferenceInstallOutcome, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        lane.install_reference_authority(owner, &mut self.actor, &mut self.presentation, authority)
    }

    pub fn pump_once(
        &mut self,
    ) -> Result<AlignmentSceneSourcePump, AlignmentSceneCoordinatorError<B::Error>> {
        let event = self.actor.pump_once()?;
        let event_owner = event.as_ref().and_then(source_event_owner);
        let mut publications = Vec::new();
        let mut refusals = Vec::new();
        let mut pressures = Vec::new();
        let mut foreground_submitted = None;
        let mut speculation_submitted = None;
        if let (Some(event), Some(owner)) = (&event, event_owner)
            && let Some(lane) = self.lanes.get_mut(&owner)
        {
            lane.observe_source_event(event);
        }
        if let Some(AlignmentSourceActorEvent::AcquisitionRefused {
            work,
            coverage,
            complete_read_limit,
            publish: Some(_),
        }) = &event
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            let target = work.intent().spec().target();
            if lane
                .desired
                .as_ref()
                .is_some_and(|desired| desired.source_horizon == target)
            {
                let refusal = AlignmentSceneOwnerRefusal {
                    owner: work.intent().owner(),
                    target,
                    complete_read_limit: *complete_read_limit,
                    eligible_records: coverage.provenance().eligible_records,
                };
                lane.refusal = Some(refusal);
                refusals.push(refusal);
            }
        }
        if matches!(
            event,
            Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
        ) {
            let completing_foreground = match &event {
                Some(AlignmentSourceActorEvent::AcquisitionCompleted {
                    publish: Some(_), ..
                }) => event_owner,
                _ => None,
            };
            let owners = self.lanes.keys().copied().collect::<Vec<_>>();
            for owner in owners {
                let lane = self
                    .lanes
                    .get_mut(&owner)
                    .expect("a copied lane key remains present during reconciliation");
                let visible_incomplete = lane
                    .frame(owner, &self.presentation)
                    .is_none_or(|frame| !frame.visible_complete());
                if completing_foreground == Some(owner) || visible_incomplete {
                    let reconciliation = lane.reconcile_from_resident_or_cache(
                        owner,
                        &mut self.actor,
                        &mut self.presentation,
                    );
                    match reconciliation {
                        Ok(Some(revision)) => {
                            publications.push(AlignmentSceneOwnerPublication { owner, revision });
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let Some(pressure) = coordinator_budget_pressure(&error) else {
                                return Err(error);
                            };
                            let Some(target) =
                                lane.desired.as_ref().map(|desired| desired.source_horizon)
                            else {
                                return Err(error);
                            };
                            // Candidate construction is lane-local even though its immutable roots
                            // share one store. Cancel only this owner's redundant source intents;
                            // other owners and every already-published scene remain authoritative.
                            self.actor.cancel_foreground_for(owner);
                            self.actor.cancel_speculation_for(owner);
                            pressures.push(AlignmentSceneOwnerPressure {
                                owner,
                                target,
                                intent: if visible_incomplete {
                                    SourceIntentKind::Foreground
                                } else {
                                    SourceIntentKind::Speculative
                                },
                                pressure,
                            });
                            continue;
                        }
                    }
                }
                if lane.current_covers_desired(owner, &self.presentation) {
                    self.actor.cancel_foreground_for(owner);
                    self.actor.cancel_speculation_for(owner);
                }
            }
            if let Some(owner) = event_owner
                && let Some(lane) = self.lanes.get_mut(&owner)
                && !pressures.iter().any(|pressure| pressure.owner == owner)
                && !lane.current_covers_desired(owner, &self.presentation)
            {
                if lane
                    .frame(owner, &self.presentation)
                    .is_some_and(|frame| frame.visible_complete())
                {
                    speculation_submitted = lane.ensure_speculation(owner, &mut self.actor)?;
                } else {
                    foreground_submitted = lane.ensure_foreground(owner, &mut self.actor)?;
                }
            }
        }
        Ok(AlignmentSceneSourcePump {
            source_event: event,
            publications,
            refusals,
            pressures,
            foreground_submitted,
            speculation_submitted,
        })
    }

    #[must_use]
    pub fn frame(
        &self,
        owner: SourceOwnerId,
    ) -> Result<Option<AlignmentCoordinatorFrame>, AlignmentSceneCoordinatorConfigError> {
        let lane = self
            .lanes
            .get(&owner)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))?;
        Ok(lane.frame(owner, &self.presentation))
    }

    #[must_use]
    pub fn telemetry(
        &self,
        owner: SourceOwnerId,
    ) -> Result<AlignmentSceneCoordinatorTelemetry, AlignmentSceneCoordinatorConfigError> {
        self.lanes
            .get(&owner)
            .map(|lane| lane.telemetry)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))
    }

    #[must_use]
    pub fn preparation_policy(
        &self,
        owner: SourceOwnerId,
    ) -> Result<AlignmentScenePreparationPolicy, AlignmentSceneCoordinatorConfigError> {
        self.lanes
            .get(&owner)
            .map(|lane| lane.policy)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))
    }

    #[must_use]
    pub fn contract(
        &self,
        owner: SourceOwnerId,
    ) -> Result<&AlignmentSceneBuildContract, AlignmentSceneCoordinatorConfigError> {
        self.lanes
            .get(&owner)
            .map(|lane| &lane.contract)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))
    }

    #[must_use]
    pub fn desired_prepared_interval(
        &self,
        owner: SourceOwnerId,
    ) -> Result<Option<GenomicInterval>, AlignmentSceneCoordinatorConfigError> {
        self.lanes
            .get(&owner)
            .map(|lane| {
                lane.desired
                    .as_ref()
                    .map(|desired| desired.prepared_interval)
            })
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))
    }

    #[must_use]
    pub const fn actor(&self) -> &AlignmentSourceActor<B> {
        &self.actor
    }

    pub const fn actor_mut(&mut self) -> &mut AlignmentSourceActor<B> {
        &mut self.actor
    }

    #[must_use]
    pub const fn presentation(&self) -> &AlignmentPresentationStore {
        &self.presentation
    }

    #[must_use]
    pub fn lane_count(&self) -> usize {
        self.lanes.len()
    }

    /// Whether the single mutable source owner has control, foreground, or speculative work that
    /// can make progress. Threaded drivers use this to pump without polling the UI mailbox.
    #[must_use]
    pub fn has_pending_source_work(&self) -> bool {
        self.actor.schedule().pending_slots() > 0
    }

    #[must_use]
    pub fn owner_work_state(
        &self,
        owner: SourceOwnerId,
    ) -> Result<AlignmentSceneOwnerWorkState, AlignmentSceneCoordinatorConfigError> {
        let lane = self
            .lanes
            .get(&owner)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))?;
        Ok(AlignmentSceneOwnerWorkState {
            source_pending: self.actor.schedule().foreground_pending_for(owner),
            presentation_pending: lane.needs_vertical_residency(owner, &self.presentation),
        })
    }

    /// Whether a visible-complete lane has optional vertical presentation work. This never makes
    /// an incomplete source look ready: a lane becomes eligible only after its immutable current
    /// scene truthfully covers the live camera.
    #[must_use]
    pub fn has_pending_presentation_work(&self) -> bool {
        self.lanes
            .iter()
            .any(|(owner, lane)| lane.needs_vertical_residency(*owner, &self.presentation))
    }

    /// Builds and atomically publishes at most one additional canonical row block. A normal
    /// ledger refusal pauses this optional residency pass and leaves the existing visible scene
    /// untouched; it is not promoted to a source/read failure.
    pub fn pump_presentation_once(
        &mut self,
    ) -> Result<Option<AlignmentPresentationPump>, AlignmentSceneCoordinatorError<B::Error>> {
        let eligible = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| {
                lane.needs_vertical_residency(*owner, &self.presentation)
                    .then_some(*owner)
            })
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            return Ok(None);
        }
        let owner = self.last_presentation_owner.map_or(eligible[0], |last| {
            eligible
                .iter()
                .copied()
                .find(|owner| *owner > last)
                .unwrap_or(eligible[0])
        });
        self.last_presentation_owner = Some(owner);
        let lane = self
            .lanes
            .get_mut(&owner)
            .expect("an eligible presentation owner remains installed");
        let outcome = lane.expand_vertical_residency_once(owner, &mut self.presentation)?;
        Ok(Some(outcome))
    }

    /// Cancels every lane-owned foreground and speculative requirement while preserving the
    /// immutable published scenes. A threaded driver uses this after a source failure so it does
    /// not spin by restarting the same failed decoder transaction; a later camera intent may
    /// explicitly retry.
    pub fn cancel_pending_source_work(&mut self) {
        let owners = self.lanes.keys().copied().collect::<Vec<_>>();
        for owner in owners {
            self.actor.cancel_foreground_for(owner);
            self.actor.cancel_speculation_for(owner);
        }
    }

    /// Clears terminal foreground refusals and queues one new latest-wins acquisition per lane.
    /// Call this only after queuing a source control which changes individual-read admission.
    /// Source controls have non-discardable priority over queued foreground work in the actor.
    pub fn retry_refused_lanes(
        &mut self,
    ) -> Result<Vec<AlignmentSceneOwnerRetry>, AlignmentSceneCoordinatorError<B::Error>> {
        let owners = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| lane.refusal.map(|_| *owner))
            .collect::<Vec<_>>();
        let mut retries = Vec::with_capacity(owners.len());
        for owner in owners {
            self.actor.cancel_foreground_for(owner);
            self.actor.cancel_speculation_for(owner);
            let lane = self
                .lanes
                .get_mut(&owner)
                .expect("a copied refused-lane key remains present during retry");
            lane.refusal = None;
            let Some(foreground_submitted) = lane.ensure_foreground(owner, &mut self.actor)? else {
                continue;
            };
            retries.push(AlignmentSceneOwnerRetry {
                owner,
                foreground_submitted,
            });
        }
        Ok(retries)
    }

    /// Reconciles and resumes the still-current lane after a caller has made shared-ledger
    /// progress. This is intentionally not another camera update: it preserves the exact camera
    /// sequence and retries only when the failed source target still belongs to that lane's live
    /// preparation horizon. A stale pressure notification is therefore a harmless no-op.
    pub fn retry_resource_pressure(
        &mut self,
        owner: SourceOwnerId,
        failed_target: GenomicInterval,
    ) -> Result<Option<AlignmentCoordinatorUpdate>, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        let Some(desired) = lane.desired.as_ref() else {
            return Ok(None);
        };
        if !desired.source_horizon.contains_interval(failed_target) {
            return Ok(None);
        }

        // Vertical residency is optional and is paused after any capacity refusal so the worker
        // cannot spin at frame rate. An explicit, current-token resume makes exactly one new
        // checkpoint eligible alongside the foreground reconciliation below.
        lane.vertical_residency_budget_limited = false;

        let camera_sequence = desired.camera_sequence;
        let before = self
            .presentation
            .current_for(presentation_owner(owner))
            .map(|scene| scene.revision());
        let published =
            lane.reconcile_from_resident_or_cache(owner, &mut self.actor, &mut self.presentation)?;
        let frame = lane
            .frame(owner, &self.presentation)
            .expect("a pressure retry retains desired camera state");
        let mut update = AlignmentCoordinatorUpdate {
            camera_sequence,
            published_scene: published,
            visible_complete: frame.visible_complete(),
            reused_current_scene: published.is_none()
                && before == frame.snapshot.as_ref().map(|scene| scene.revision()),
            ..AlignmentCoordinatorUpdate::default()
        };

        if lane.current_covers_desired(owner, &self.presentation) {
            self.actor.cancel_foreground_for(owner);
            self.actor.cancel_speculation_for(owner);
        } else if update.visible_complete {
            self.actor.cancel_foreground_for(owner);
            update.speculation_submitted = lane.ensure_speculation(owner, &mut self.actor)?;
        } else {
            self.actor.cancel_speculation_for(owner);
            update.foreground_submitted = lane.ensure_foreground(owner, &mut self.actor)?;
        }
        Ok(Some(update))
    }

    pub fn reduce_budget(
        &mut self,
        budget: usize,
    ) -> Result<AlignmentCoordinatorPressure, AlignmentSceneCoordinatorError<B::Error>> {
        for lane in self.lanes.values_mut() {
            // A budget change is an explicit new admission decision. A prior background refusal
            // must not permanently suppress work after the user grants more room.
            lane.vertical_residency_budget_limited = false;
        }
        let speculation_was_pending = self.actor.schedule().desired_speculation().is_some()
            || self
                .actor
                .schedule()
                .active()
                .is_some_and(|work| work.kind() == crate::SourceIntentKind::Speculative);
        let aborted = self.actor.abort_speculation();
        if let Some(work) = &aborted
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            lane.telemetry.resource_pressure_speculation_aborts = lane
                .telemetry
                .resource_pressure_speculation_aborts
                .saturating_add(1);
        }
        let presentation = self.presentation.reduce_budget(budget)?;
        let evidence = self.actor.store_mut().reduce_budget(budget)?;
        let progress = self
            .actor
            .store()
            .ledger()
            .progress()
            .map_err(EvidenceStoreError::from)?;
        Ok(AlignmentCoordinatorPressure {
            budget,
            speculation_was_pending,
            active_speculation_aborted: aborted.is_some(),
            presentation,
            evidence,
            charged: progress.charged,
            pinned: progress.pinned,
            pinned_overage: progress.pinned_overage,
            remaining_overage: progress.charged.saturating_sub(budget),
        })
    }
}

const fn presentation_owner(owner: SourceOwnerId) -> AlignmentPresentationOwner {
    AlignmentPresentationOwner::new(owner.get())
}

fn source_event_owner(event: &AlignmentSourceActorEvent) -> Option<SourceOwnerId> {
    match event {
        AlignmentSourceActorEvent::WorkStarted(work)
        | AlignmentSourceActorEvent::SourceProgressed { work, .. }
        | AlignmentSourceActorEvent::WorkYielded { work, .. }
        | AlignmentSourceActorEvent::AcquisitionCompleted { work, .. }
        | AlignmentSourceActorEvent::AcquisitionRefused { work, .. } => Some(work.intent().owner()),
        AlignmentSourceActorEvent::WorkRetargeted { current, .. } => Some(current.intent().owner()),
        AlignmentSourceActorEvent::ControlApplied
        | AlignmentSourceActorEvent::ChunkAdmitted { .. } => None,
    }
}

impl AlignmentSceneSourceLane {
    fn update_camera<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        camera: AlignmentSceneCamera,
        retain_prepared_scene: bool,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        if camera.interval().contig() != self.contract.contig {
            return Err(AlignmentSceneCoordinatorError::CameraContigMismatch);
        }
        if camera.interval().end() > self.contract.contig_length {
            return Err(AlignmentSceneCoordinatorError::CameraOutsideContig {
                interval: camera.interval(),
                contig_length: self.contract.contig_length,
            });
        }
        let previous_source_horizon = self.desired.as_ref().map(|desired| desired.source_horizon);
        let camera_sequence = self.take_camera_sequence()?;
        let direction = camera_direction(self.previous_exact_start, camera.exact_start());
        self.previous_exact_start = Some(camera.exact_start());
        let proposed_interval = prepared_interval(camera, direction, self.policy, &self.contract)?;
        let retained = retain_prepared_scene
            .then(|| self.retained_preparation_if_safe(owner, presentation, camera, direction))
            .flatten();
        // A safe current scene may remain the live authority until its trigger is crossed. Once
        // replacement is required, however, the desired preparation target is the new bounded
        // policy window—not the union of that window with the current scene. Repeated overlapping
        // pans would otherwise turn a fixed-size buffer into a monotonically growing corridor,
        // pinning every locus traversed and eventually making an ordinary source reservation fail.
        // The current immutable scene remains published below while the bounded replacement is
        // acquired, so removing the union does not replace truthful overlap with a blank sentinel.
        let (prepared_interval, prepared_rows) = retained.unwrap_or((proposed_interval, None));
        let source_horizon = source_horizon(prepared_interval, &self.contract)?;
        if previous_source_horizon != Some(source_horizon) {
            self.vertical_residency_budget_limited = false;
        }
        if self
            .refusal
            .is_some_and(|refusal| refusal.target != source_horizon)
        {
            self.refusal = None;
        }
        self.desired = Some(DesiredScene {
            camera,
            camera_sequence,
            prepared_interval,
            source_horizon,
            prepared_rows,
            direction,
        });
        self.telemetry.camera_updates = self.telemetry.camera_updates.saturating_add(1);

        let before = presentation
            .current_for(presentation_owner(owner))
            .map(|scene| scene.revision());
        let published = self.reconcile_from_resident_or_cache(owner, actor, presentation)?;
        let frame = self
            .frame(owner, presentation)
            .expect("a camera update installs desired state");
        let mut update = AlignmentCoordinatorUpdate {
            camera_sequence,
            published_scene: published,
            visible_complete: frame.visible_complete(),
            reused_current_scene: published.is_none()
                && before == frame.snapshot.as_ref().map(|scene| scene.revision()),
            ..AlignmentCoordinatorUpdate::default()
        };
        if published.is_none() && update.reused_current_scene {
            self.telemetry.camera_only_updates =
                self.telemetry.camera_only_updates.saturating_add(1);
        }

        if self.current_covers_desired(owner, presentation) {
            actor.cancel_foreground_for(owner);
            actor.cancel_speculation_for(owner);
        } else if update.visible_complete {
            actor.cancel_foreground_for(owner);
            update.speculation_submitted = self.ensure_speculation(owner, actor)?;
        } else {
            actor.cancel_speculation_for(owner);
            update.foreground_submitted = self.ensure_foreground(owner, actor)?;
        }
        Ok(update)
    }

    fn replace_contract<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        contract: AlignmentSceneBuildContract,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        contract.validate()?;
        if contract.source_revision != self.contract.source_revision {
            return Err(AlignmentSceneCoordinatorConfigError::SourceAuthorityMismatch.into());
        }
        if contract.record_contract != self.contract.record_contract {
            return Err(AlignmentSceneCoordinatorConfigError::RecordAuthorityMismatch.into());
        }
        if contract.contig != self.contract.contig
            || contract.contig_length != self.contract.contig_length
        {
            return Err(AlignmentSceneCoordinatorError::CameraContigMismatch);
        }
        let camera = self
            .desired
            .as_ref()
            .map(|desired| desired.camera)
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        self.contract = contract;
        self.vertical_residency_budget_limited = false;
        self.update_camera(owner, actor, presentation, camera, true)
    }

    fn install_reference_authority<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        authority: AlignmentReferenceAuthority,
    ) -> Result<AlignmentReferenceInstallOutcome, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let Some(desired) = self.desired.clone() else {
            return Ok(AlignmentReferenceInstallOutcome::default());
        };
        let required_reference = required_reference_horizon(&self.contract, desired.source_horizon);
        if self.contract.presentation.reference != Some(authority.revision)
            || authority.slice.interval.contig() != self.contract.contig
            || !authority
                .slice
                .interval
                .contains_interval(required_reference)
        {
            return Ok(AlignmentReferenceInstallOutcome::default());
        }

        self.contract.reference = Some(authority);
        let published_scene = self.reconcile_from_resident_or_cache(owner, actor, presentation)?;
        if self.current_covers_desired(owner, presentation) {
            actor.cancel_foreground_for(owner);
            actor.cancel_speculation_for(owner);
        }
        Ok(AlignmentReferenceInstallOutcome {
            installed: true,
            published_scene,
        })
    }

    fn prewarm_presentation<B>(
        &mut self,
        owner: SourceOwnerId,
        presentation_store: &mut AlignmentPresentationStore,
        presentation: PresentationContract,
    ) -> Result<bool, AlignmentSceneCoordinatorError<B>> {
        if presentation.annotation
            != self
                .contract
                .annotation
                .as_ref()
                .map(|annotation| annotation.revision())
        {
            return Err(AlignmentSceneCoordinatorConfigError::AnnotationAuthorityMismatch.into());
        }
        let Some(desired) = self.desired.clone() else {
            return Err(AlignmentSceneCoordinatorError::MissingCamera);
        };
        let reference = match presentation.reference {
            None => None,
            Some(revision) => {
                if self.contract.presentation.reference != Some(revision) {
                    return Err(
                        AlignmentSceneCoordinatorConfigError::ReferenceAuthorityMismatch.into(),
                    );
                }
                let Some(reference) =
                    matching_reference_slice(&self.contract, desired.source_horizon)
                else {
                    return Ok(false);
                };
                Some(reference)
            }
        };
        let Some(current) = presentation_store
            .current_for(presentation_owner(owner))
            .cloned()
        else {
            return Ok(false);
        };
        if current.summaries().contract() != self.contract.summaries
            || current.reads().atlas().layout_contract() != self.contract.layout
            || !current
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
            || !current
                .summaries()
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || !current
                .reads()
                .atlas()
                .interval()
                .contains_interval(desired.source_horizon)
        {
            return Ok(false);
        }
        let rows = desired_rows(
            &desired,
            current.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        )?;
        let candidate = presentation_store.prepare_with_annotation(
            current.reads().evidence(),
            current.summaries(),
            current.reads().atlas(),
            presentation,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Normal,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        drop(candidate);
        Ok(true)
    }

    fn frame(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> Option<AlignmentCoordinatorFrame> {
        let desired = self.desired.as_ref()?;
        let snapshot = presentation.current_for(presentation_owner(owner)).cloned();
        let requested_rows = snapshot.as_ref().map_or_else(
            || desired.camera.visible_rows(),
            |scene| {
                let row_count =
                    u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
                let visible = desired.camera.visible_rows();
                visible.start.min(row_count)..visible.end.min(row_count)
            },
        );
        let (covered_interval, covered_rows) = snapshot.as_ref().map_or((None, 0..0), |scene| {
            (
                intersect_intervals(desired.camera.interval(), scene.prepared_interval()),
                intersect_ranges(requested_rows.clone(), scene.reads().prepared_rows()),
            )
        });
        let scene_contracts_current = snapshot.as_ref().is_some_and(|scene| {
            presentation_core_matches(scene.reads().presentation(), self.contract.presentation)
                && scene.summaries().contract() == self.contract.summaries
                && scene.reads().atlas().layout_contract() == self.contract.layout
        });
        let reference_requirement = self.reference_requirement(desired);
        let reference_products_current = snapshot.as_ref().is_some_and(|scene| {
            reference_requirement.is_ready()
                && scene.reads().presentation() == self.contract.presentation
        });
        let visible_complete = scene_contracts_current
            && covered_interval
                .is_some_and(|covered| covered.contains_interval(desired.camera.interval()))
            && covered_rows.start <= requested_rows.start
            && covered_rows.end >= requested_rows.end;
        let individual_reads = if !desired.camera.has_visible_rows() {
            // This camera requested genomic summary authority only. Do not report an
            // individual-read capability for work the caller deliberately omitted.
            AlignmentIndividualReadCapability::Pending
        } else if visible_complete {
            AlignmentIndividualReadCapability::Complete {
                scene_revision: snapshot
                    .as_ref()
                    .expect("complete frame coverage requires a scene")
                    .revision(),
            }
        } else if let Some(refusal) = self
            .refusal
            .filter(|refusal| refusal.target == desired.source_horizon)
        {
            AlignmentIndividualReadCapability::Refused {
                target: refusal.target,
                complete_read_limit: refusal.complete_read_limit,
                eligible_records: refusal.eligible_records,
            }
        } else {
            AlignmentIndividualReadCapability::Pending
        };
        let vertical_residency = if !desired.camera.has_visible_rows() {
            AlignmentVerticalResidency::ViewportBounded
        } else {
            snapshot
                .as_ref()
                .map_or(AlignmentVerticalResidency::ViewportBounded, |scene| {
                    self.vertical_residency_state(scene)
                })
        };
        let row_activity = desired
            .camera
            .has_visible_rows()
            .then(|| {
                snapshot
                    .as_ref()
                    .and_then(|scene| self.row_activity_for_frame(scene, desired.camera.interval()))
            })
            .flatten();
        Some(AlignmentCoordinatorFrame {
            camera: desired.camera,
            camera_sequence: desired.camera_sequence,
            snapshot,
            covered_interval,
            requested_rows,
            covered_rows,
            row_activity,
            reference_requirement,
            scene_contracts_current,
            reference_products_current,
            individual_reads,
            vertical_residency,
        })
    }

    fn reference_requirement(&self, desired: &DesiredScene) -> AlignmentReferenceRequirement {
        let Some(revision) = self.contract.presentation.reference else {
            return AlignmentReferenceRequirement::NotRequired;
        };
        let horizon = required_reference_horizon(&self.contract, desired.source_horizon);
        if matching_reference_slice(&self.contract, desired.source_horizon).is_some() {
            AlignmentReferenceRequirement::Ready { revision, horizon }
        } else {
            AlignmentReferenceRequirement::Pending { revision, horizon }
        }
    }

    /// Resolves the presentation contract that can be built truthfully from authority currently
    /// resident in the lane. Only the reference revision is optional: evidence, exact summaries,
    /// row layout, annotation, and every reference-independent style choice remain unchanged.
    /// A later FASTA publication upgrades this scene through the same immutable candidate path.
    fn available_presentation_contract(
        &self,
        source_horizon: GenomicInterval,
    ) -> (PresentationContract, Option<&ReferenceSlice>) {
        let reference = matching_reference_slice(&self.contract, source_horizon);
        let mut presentation = self.contract.presentation;
        if reference.is_none() {
            presentation.reference = None;
        }
        (presentation, reference)
    }

    fn row_activity_for_frame(
        &self,
        scene: &AlignmentSceneSnapshot,
        camera_interval: GenomicInterval,
    ) -> Option<Arc<AlignmentRowActivityProfile>> {
        if !alignment_row_atlas_satisfies_contract(scene, camera_interval, &self.contract) {
            *self.row_activity_cache.borrow_mut() = None;
            return None;
        }

        let atlas = scene.reads().atlas();
        let atlas_revision = atlas.revision();
        if let Some(cached) = self.row_activity_cache.borrow().as_ref()
            && cached.atlas_revision == atlas_revision
            && cached.interval == camera_interval
        {
            return Some(Arc::clone(&cached.profile));
        }

        let profile = Arc::new(
            atlas.activity_profile(
                camera_interval,
                NonZeroUsize::new(ALIGNMENT_ROW_ACTIVITY_BINS)
                    .expect("the fixed row-activity bin bound is non-zero"),
            )?,
        );
        debug_assert!(alignment_row_activity_satisfies_contract(
            &profile,
            scene,
            camera_interval,
            &self.contract,
        ));
        *self.row_activity_cache.borrow_mut() = Some(AlignmentRowActivityCache {
            atlas_revision,
            interval: camera_interval,
            profile: Arc::clone(&profile),
        });
        Some(profile)
    }

    fn vertical_residency_state(
        &self,
        scene: &AlignmentSceneSnapshot,
    ) -> AlignmentVerticalResidency {
        if !self.policy.full_vertical_residency
            || !supports_full_vertical_residency(self.contract.presentation.geometry_lod)
        {
            return AlignmentVerticalResidency::ViewportBounded;
        }
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
        let prepared = scene.reads().prepared_rows();
        if prepared.start == 0 && prepared.end == total_rows {
            AlignmentVerticalResidency::Complete { total_rows }
        } else if self.vertical_residency_budget_limited {
            AlignmentVerticalResidency::BudgetLimited {
                prepared_rows: prepared.end.saturating_sub(prepared.start),
                total_rows,
            }
        } else {
            AlignmentVerticalResidency::Preparing {
                prepared_rows: prepared.end.saturating_sub(prepared.start),
                total_rows,
            }
        }
    }

    fn reconcile_from_resident_or_cache<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<Option<SceneRevision>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let Some(desired) = self.desired.clone() else {
            return Ok(None);
        };
        if self.current_covers_desired(owner, presentation) {
            return Ok(None);
        }
        if let Some(scene) = self.prepare_from_current_roots(owner, presentation, &desired)? {
            return Ok(Some(scene.revision()));
        }
        if let Some(scene) = self.prepare_from_source_roots(owner, actor, presentation, &desired)? {
            return Ok(Some(scene.revision()));
        }
        if !actor
            .store()
            .cached_covers(desired.source_horizon, &self.contract.payload)
        {
            return Ok(None);
        }
        if !self.source_roots_cover(actor, &desired) {
            self.publish_source_roots_from_cache(actor, &desired)?;
        }
        let scene = self
            .prepare_from_source_roots(owner, actor, presentation, &desired)?
            .ok_or(AlignmentSceneCoordinatorError::IncompleteCachedHorizon(
                desired.source_horizon,
            ))?;
        Ok(Some(scene.revision()))
    }

    fn prepare_from_current_roots<B>(
        &mut self,
        owner: SourceOwnerId,
        presentation: &mut AlignmentPresentationStore,
        desired: &DesiredScene,
    ) -> Result<Option<Arc<AlignmentSceneSnapshot>>, AlignmentSceneCoordinatorError<B>> {
        let Some(current) = presentation.current_for(presentation_owner(owner)).cloned() else {
            return Ok(None);
        };
        let reads = current.reads();
        let evidence = reads.evidence();
        let atlas = reads.atlas();
        if !evidence.payload().satisfies(&self.contract.payload)
            || current.summaries().contract() != self.contract.summaries
            || atlas.layout_contract() != self.contract.layout
            || !evidence.coverage().covers(desired.source_horizon)
            || !current
                .summaries()
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || !atlas.interval().contains_interval(desired.source_horizon)
        {
            return Ok(None);
        }
        let rows = desired_rows(desired, atlas.row_count(), self.policy.vertical_margin_rows)?;
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let candidate = presentation.prepare_with_annotation(
            evidence,
            current.summaries(),
            atlas,
            presentation_contract,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Transition,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        let scene = presentation.publish_for(presentation_owner(owner), candidate);
        self.refusal = None;
        self.telemetry.scene_publications = self.telemetry.scene_publications.saturating_add(1);
        Ok(Some(scene))
    }

    /// Reuses the source store's already-published immutable authority for another panel. This
    /// is what makes equal multi-panel requests share evidence, atlas identities, semantic pages,
    /// and source I/O rather than merely sharing a byte budget.
    fn prepare_from_source_roots<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &AlignmentSourceActor<B>,
        presentation: &mut AlignmentPresentationStore,
        desired: &DesiredScene,
    ) -> Result<Option<Arc<AlignmentSceneSnapshot>>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let (Some(evidence), Some(summaries), Some(atlas)) = (
            actor.store().current().cloned(),
            actor.store().current_summaries().cloned(),
            actor.store().current_row_atlas().cloned(),
        ) else {
            return Ok(None);
        };
        if !evidence.payload().satisfies(&self.contract.payload)
            || !evidence.coverage().covers(desired.source_horizon)
            || summaries.contract() != self.contract.summaries
            || !summaries
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            || atlas.layout_contract() != self.contract.layout
            || !atlas.interval().contains_interval(desired.source_horizon)
        {
            return Ok(None);
        }
        let rows = desired_rows(desired, atlas.row_count(), self.policy.vertical_margin_rows)?;
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let candidate = presentation.prepare_with_annotation(
            &evidence,
            &summaries,
            &atlas,
            presentation_contract,
            desired.prepared_interval,
            rows,
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Transition,
        )?;
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        let scene = presentation.publish_for(presentation_owner(owner), candidate);
        self.refusal = None;
        self.telemetry.scene_publications = self.telemetry.scene_publications.saturating_add(1);
        Ok(Some(scene))
    }

    fn source_roots_cover<B>(&self, actor: &AlignmentSourceActor<B>, desired: &DesiredScene) -> bool
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let (Some(evidence), Some(summaries), Some(atlas)) = (
            actor.store().current(),
            actor.store().current_summaries(),
            actor.store().current_row_atlas(),
        ) else {
            return false;
        };
        evidence.payload().satisfies(&self.contract.payload)
            && evidence.coverage().covers(desired.source_horizon)
            && summaries.contract() == self.contract.summaries
            && summaries
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
            && atlas.layout_contract() == self.contract.layout
            && atlas.interval().contains_interval(desired.source_horizon)
    }

    fn publish_source_roots_from_cache<B>(
        &mut self,
        actor: &mut AlignmentSourceActor<B>,
        desired: &DesiredScene,
    ) -> Result<(), AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let evidence_candidate = actor.store_mut().prepare(
            desired.prepared_interval,
            self.contract.payload.clone(),
            ReservationKind::Transition,
        )?;
        if !evidence_candidate
            .root()
            .coverage()
            .covers(desired.source_horizon)
        {
            return Err(AlignmentSceneCoordinatorError::IncompleteCachedHorizon(
                desired.source_horizon,
            ));
        }
        self.telemetry.evidence_candidates = self.telemetry.evidence_candidates.saturating_add(1);
        let evidence = Arc::clone(evidence_candidate.root());
        let summary_candidate = actor.store_mut().prepare_exact_summaries(
            &evidence,
            desired.prepared_interval,
            self.contract.summary_tile_bases,
            self.contract.contig_length,
            self.contract.summaries,
            self.contract.normalization_mapped_records,
            ReservationKind::Transition,
        )?;
        self.telemetry.exact_summary_candidates =
            self.telemetry.exact_summary_candidates.saturating_add(1);
        let atlas_candidate = actor.store_mut().prepare_row_atlas(
            &evidence,
            desired.source_horizon,
            self.contract.layout,
            ReservationKind::Transition,
        )?;
        self.telemetry.row_atlas_candidates = self.telemetry.row_atlas_candidates.saturating_add(1);

        actor.store_mut().publish_complete_authority(
            evidence_candidate,
            summary_candidate,
            atlas_candidate,
        )?;
        Ok(())
    }

    fn current_covers_desired(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        let Some(desired) = &self.desired else {
            return false;
        };
        let Some(scene) = presentation.current_for(presentation_owner(owner)) else {
            return false;
        };
        let (available_presentation, _) =
            self.available_presentation_contract(desired.source_horizon);
        if scene.reads().presentation() != available_presentation
            || scene.summaries().contract() != self.contract.summaries
            || scene.reads().atlas().layout_contract() != self.contract.layout
            || !scene
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
        {
            return false;
        }
        let rows = match desired_rows(
            desired,
            scene.reads().atlas().row_count(),
            self.policy.vertical_margin_rows,
        ) {
            Ok(rows) => rows,
            Err(_) => return false,
        };
        scene.covers(desired.prepared_interval, rows)
    }

    fn retained_preparation_if_safe(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
        camera: AlignmentSceneCamera,
        direction: CameraDirection,
    ) -> Option<(GenomicInterval, Option<Range<u32>>)> {
        let scene = presentation.current_for(presentation_owner(owner))?;
        if !presentation_core_matches(scene.reads().presentation(), self.contract.presentation)
            || scene.summaries().contract() != self.contract.summaries
            || scene.reads().atlas().layout_contract() != self.contract.layout
            || !scene
                .prepared_interval()
                .contains_interval(camera.interval())
        {
            return None;
        }
        let prepared = scene.prepared_interval();
        let trigger = camera.span() * self.policy.horizontal_trigger_viewports;
        let left = camera.exact_start() - prepared.start() as f64;
        let right = prepared.end() as f64 - (camera.exact_start() + camera.span());
        let horizontal_safe = match direction {
            CameraDirection::Left => left >= trigger || prepared.start() == 0,
            CameraDirection::Right => {
                right >= trigger || prepared.end() == self.contract.contig_length
            }
            CameraDirection::Stationary => {
                (left >= trigger || prepared.start() == 0)
                    && (right >= trigger || prepared.end() == self.contract.contig_length)
            }
        };
        if !horizontal_safe {
            return None;
        }

        let visible = camera.visible_rows();
        if visible.is_empty() {
            return Some((prepared, None));
        }
        let prepared_rows = scene.reads().prepared_rows();
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).ok()?;
        let before_safe = visible.start.saturating_sub(prepared_rows.start)
            >= self.policy.vertical_trigger_rows
            || prepared_rows.start == 0;
        let after_safe = prepared_rows.end.saturating_sub(visible.end)
            >= self.policy.vertical_trigger_rows
            || prepared_rows.end == total_rows;
        (before_safe && after_safe).then_some((prepared, Some(prepared_rows)))
    }

    fn needs_vertical_residency(
        &self,
        owner: SourceOwnerId,
        presentation: &AlignmentPresentationStore,
    ) -> bool {
        if !self.policy.full_vertical_residency
            || self.vertical_residency_budget_limited
            || !supports_full_vertical_residency(self.contract.presentation.geometry_lod)
        {
            return false;
        }
        let Some(desired) = &self.desired else {
            return false;
        };
        if !desired.camera.has_visible_rows() {
            return false;
        }
        let Some(scene) = presentation.current_for(presentation_owner(owner)) else {
            return false;
        };
        let (available_presentation, _) =
            self.available_presentation_contract(desired.source_horizon);
        if !self
            .frame(owner, presentation)
            .is_some_and(|frame| frame.visible_complete())
            || scene.reads().presentation() != available_presentation
            || scene.summaries().contract() != self.contract.summaries
            || scene.reads().atlas().layout_contract() != self.contract.layout
            || !scene
                .reads()
                .evidence()
                .coverage()
                .covers(desired.source_horizon)
            || !scene
                .prepared_interval()
                .contains_interval(desired.prepared_interval)
        {
            return false;
        }
        let total_rows = u32::try_from(scene.reads().atlas().row_count()).unwrap_or(u32::MAX);
        let rows = scene.reads().prepared_rows();
        rows.start > 0 || rows.end < total_rows
    }

    fn expand_vertical_residency_once<E>(
        &mut self,
        owner: SourceOwnerId,
        presentation: &mut AlignmentPresentationStore,
    ) -> Result<AlignmentPresentationPump, AlignmentSceneCoordinatorError<E>> {
        let desired = self
            .desired
            .clone()
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let current = presentation
            .current_for(presentation_owner(owner))
            .cloned()
            .ok_or(AlignmentSceneCoordinatorError::MissingCamera)?;
        let total_rows = u32::try_from(current.reads().atlas().row_count())
            .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
        let target_rows = expanded_vertical_rows(
            current.reads().prepared_rows(),
            total_rows,
            self.contract.row_block_rows,
            desired.camera.visible_rows(),
        );
        self.telemetry.vertical_residency_checkpoints = self
            .telemetry
            .vertical_residency_checkpoints
            .saturating_add(1);
        let (presentation_contract, reference) =
            self.available_presentation_contract(desired.source_horizon);
        let candidate = match presentation.prepare_with_annotation(
            current.reads().evidence(),
            current.summaries(),
            current.reads().atlas(),
            presentation_contract,
            current.prepared_interval(),
            target_rows.clone(),
            self.contract.contig_length,
            self.contract.geometry_tile_bases,
            self.contract.row_block_rows,
            reference,
            self.contract.annotation.as_ref(),
            ReservationKind::Normal,
        ) {
            Ok(candidate) => candidate,
            Err(error) if presentation_budget_pressure(&error).is_some() => {
                let pressure = presentation_budget_pressure(&error)
                    .expect("the guarded presentation error has allocation pressure");
                self.vertical_residency_budget_limited = true;
                self.telemetry.vertical_residency_budget_limits = self
                    .telemetry
                    .vertical_residency_budget_limits
                    .saturating_add(1);
                return Ok(AlignmentPresentationPump {
                    owner,
                    published_scene: None,
                    budget_limited: true,
                    pressure: Some(pressure),
                });
            }
            Err(error) => return Err(error.into()),
        };
        self.telemetry.presentation_candidates =
            self.telemetry.presentation_candidates.saturating_add(1);
        let scene = presentation.publish_for(presentation_owner(owner), candidate);
        self.telemetry.scene_publications = self.telemetry.scene_publications.saturating_add(1);
        self.telemetry.vertical_residency_publications = self
            .telemetry
            .vertical_residency_publications
            .saturating_add(1);
        if target_rows.start == 0 && target_rows.end == total_rows {
            self.telemetry.vertical_residency_completions = self
                .telemetry
                .vertical_residency_completions
                .saturating_add(1);
        }
        Ok(AlignmentPresentationPump {
            owner,
            published_scene: Some(scene.revision()),
            budget_limited: false,
            pressure: None,
        })
    }

    fn ensure_foreground<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
    ) -> Result<Option<SourceIntentSequence>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let desired = self
            .desired
            .as_ref()
            .expect("source scheduling requires a camera");
        let Some(missing) = actor.store().cached_missing_interval(
            desired.source_horizon,
            &self.contract.payload,
            desired.direction != CameraDirection::Left,
        ) else {
            return Ok(None);
        };
        if actor
            .schedule()
            .desired_foreground_for(owner)
            .is_some_and(|intent| source_intent_covers(intent.spec(), missing, &self.contract))
        {
            return Ok(None);
        }
        let sequence = actor.submit_foreground_for(owner, source_spec(missing, &self.contract))?;
        self.telemetry.foreground_submissions =
            self.telemetry.foreground_submissions.saturating_add(1);
        Ok(Some(sequence))
    }

    fn ensure_speculation<B>(
        &mut self,
        owner: SourceOwnerId,
        actor: &mut AlignmentSourceActor<B>,
    ) -> Result<Option<SourceIntentSequence>, AlignmentSceneCoordinatorError<B::Error>>
    where
        B: CanonicalAlignmentSourceBackend,
    {
        let desired = self
            .desired
            .as_ref()
            .expect("source scheduling requires a camera");
        let Some(missing) = actor.store().cached_missing_interval(
            desired.source_horizon,
            &self.contract.payload,
            desired.direction != CameraDirection::Left,
        ) else {
            return Ok(None);
        };
        if actor
            .schedule()
            .desired_speculation()
            .filter(|intent| intent.owner() == owner)
            .is_some_and(|intent| source_intent_covers(intent.spec(), missing, &self.contract))
        {
            return Ok(None);
        }
        let sequence = actor.submit_speculation_for(owner, source_spec(missing, &self.contract))?;
        self.telemetry.speculation_submissions =
            self.telemetry.speculation_submissions.saturating_add(1);
        Ok(Some(sequence))
    }

    fn observe_source_event(&mut self, event: &AlignmentSourceActorEvent) {
        self.telemetry.source_checkpoints = self.telemetry.source_checkpoints.saturating_add(1);
        match event {
            AlignmentSourceActorEvent::WorkStarted(_) => {
                self.telemetry.source_work_started =
                    self.telemetry.source_work_started.saturating_add(1);
            }
            AlignmentSourceActorEvent::WorkRetargeted { .. } => {
                self.telemetry.source_retargets = self.telemetry.source_retargets.saturating_add(1);
            }
            AlignmentSourceActorEvent::WorkYielded { .. } => {
                self.telemetry.source_yields = self.telemetry.source_yields.saturating_add(1);
            }
            AlignmentSourceActorEvent::AcquisitionCompleted { .. } => {
                self.telemetry.source_acquisitions =
                    self.telemetry.source_acquisitions.saturating_add(1);
            }
            AlignmentSourceActorEvent::AcquisitionRefused { .. } => {
                self.telemetry.source_refusals = self.telemetry.source_refusals.saturating_add(1);
            }
            AlignmentSourceActorEvent::ControlApplied
            | AlignmentSourceActorEvent::ChunkAdmitted { .. }
            | AlignmentSourceActorEvent::SourceProgressed { .. } => {}
        }
    }

    fn take_camera_sequence<B>(&mut self) -> Result<u64, AlignmentSceneCoordinatorError<B>> {
        let sequence = self.next_camera_sequence;
        self.next_camera_sequence = self
            .next_camera_sequence
            .checked_add(1)
            .ok_or(AlignmentSceneCoordinatorError::CameraSequenceOverflow)?;
        Ok(sequence)
    }
}

fn source_spec(
    target: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> AlignmentSourceIntentSpec {
    AlignmentSourceIntentSpec::new(
        contract.source_revision,
        contract.record_contract,
        contract.payload.clone(),
        target,
    )
}

fn matching_reference_slice(
    contract: &AlignmentSceneBuildContract,
    horizon: GenomicInterval,
) -> Option<&ReferenceSlice> {
    let revision = contract.presentation.reference?;
    let reference = contract.reference.as_ref()?;
    let required = required_reference_horizon(contract, horizon);
    (reference.revision == revision
        && reference.slice.interval.contig() == contract.contig
        && reference.slice.interval.contains_interval(required))
    .then_some(reference.slice.as_ref())
}

/// Reference revisions invalidate only reference-interpreted page products. Comparing contracts
/// with that optional dependency erased lets the immutable core scene remain current while FASTA
/// work is pending, unavailable, or being upgraded.
fn presentation_core_matches(
    mut actual: PresentationContract,
    mut desired: PresentationContract,
) -> bool {
    actual.reference = None;
    desired.reference = None;
    actual == desired
}

fn required_reference_horizon(
    contract: &AlignmentSceneBuildContract,
    source_horizon: GenomicInterval,
) -> GenomicInterval {
    let radius = if contract.summaries.allele_detail != AlleleSummaryDetail::None {
        ALLELE_REFERENCE_CONTEXT_RADIUS
    } else {
        0
    };
    GenomicInterval::new(
        source_horizon.contig(),
        source_horizon.start().saturating_sub(radius),
        source_horizon
            .end()
            .saturating_add(radius)
            .min(contract.contig_length),
    )
    .expect("validated source horizon remains non-empty within its reference contig")
}

fn source_intent_covers(
    intent: &AlignmentSourceIntentSpec,
    target: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> bool {
    intent.source_revision() == contract.source_revision
        && intent.record_contract() == contract.record_contract
        && intent.payload().satisfies(&contract.payload)
        && intent.target().contains_interval(target)
}

fn camera_direction(previous: Option<f64>, current: f64) -> CameraDirection {
    previous.map_or(CameraDirection::Stationary, |previous| {
        if current > previous {
            CameraDirection::Right
        } else if current < previous {
            CameraDirection::Left
        } else {
            CameraDirection::Stationary
        }
    })
}

fn prepared_interval(
    camera: AlignmentSceneCamera,
    direction: CameraDirection,
    policy: AlignmentScenePreparationPolicy,
    contract: &AlignmentSceneBuildContract,
) -> Result<GenomicInterval, AlignmentSceneCoordinatorConfigError> {
    let (left_viewports, right_viewports) = match direction {
        CameraDirection::Left => (policy.ahead_viewports, policy.behind_viewports),
        CameraDirection::Right => (policy.behind_viewports, policy.ahead_viewports),
        CameraDirection::Stationary => {
            let margin = policy.ahead_viewports.max(policy.behind_viewports);
            (margin, margin)
        }
    };
    let span = camera.span();
    let start = (camera.exact_start() - span * left_viewports)
        .floor()
        .max(0.0) as u64;
    let end = (camera.exact_start() + span * (1.0 + right_viewports))
        .ceil()
        .min(contract.contig_length as f64) as u64;
    Ok(GenomicInterval::bounded(
        contract.contig,
        start,
        end.max(start.saturating_add(1)),
        contract.contig_length,
    )?)
}

fn source_horizon(
    prepared: GenomicInterval,
    contract: &AlignmentSceneBuildContract,
) -> Result<GenomicInterval, AlignmentSceneCoordinatorConfigError> {
    let mut start = prepared.start();
    let mut end = prepared.end();
    for tile_bases in [contract.geometry_tile_bases, contract.summary_tile_bases] {
        let tiles = CanonicalGenomicTile::covering(prepared, tile_bases, contract.contig_length)?;
        if let Some(first) = tiles.first() {
            start = start.min(first.interval(contract.contig_length)?.start());
        }
        if let Some(last) = tiles.last() {
            end = end.max(last.interval(contract.contig_length)?.end());
        }
    }
    Ok(GenomicInterval::new(contract.contig, start, end)?)
}

fn prepared_rows(
    row_count: usize,
    camera: AlignmentSceneCamera,
    margin: u32,
) -> Result<Range<u32>, AlignmentSceneCoordinatorConfigError> {
    let row_count = u32::try_from(row_count)
        .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
    if row_count == 0 {
        return Ok(0..0);
    }
    let visible = camera.visible_rows();
    if visible.is_empty() {
        return Ok(0..0);
    }
    Ok(visible.start.saturating_sub(margin).min(row_count)
        ..visible.end.saturating_add(margin).min(row_count))
}

fn desired_rows(
    desired: &DesiredScene,
    row_count: usize,
    margin: u32,
) -> Result<Range<u32>, AlignmentSceneCoordinatorConfigError> {
    if let Some(rows) = &desired.prepared_rows {
        let row_count = u32::try_from(row_count)
            .map_err(|_| AlignmentSceneCoordinatorConfigError::RowCoordinateOverflow)?;
        return Ok(rows.start.min(row_count)..rows.end.min(row_count));
    }
    prepared_rows(row_count, desired.camera, margin)
}

const fn supports_full_vertical_residency(lod: GeometryLod) -> bool {
    matches!(lod, GeometryLod::Span | GeometryLod::Read)
}

/// Expands a contiguous prepared row range by at most one previously absent canonical block.
/// When both sides are available, the edge closest to the live camera wins; ties favour the
/// usual top-to-bottom browsing direction. Existing partial edge blocks are normalised for free
/// because their semantic page is already cached.
fn expanded_vertical_rows(
    current: Range<u32>,
    total_rows: u32,
    block_rows: NonZeroU32,
    visible: Range<u32>,
) -> Range<u32> {
    if total_rows == 0 {
        return 0..0;
    }
    let rows = block_rows.get();
    let current = current.start.min(total_rows)..current.end.min(total_rows);
    let can_expand_before = current.start > 0;
    let can_expand_after = current.end < total_rows;
    if !can_expand_before && !can_expand_after {
        return 0..total_rows;
    }
    let before_distance = visible.start.saturating_sub(current.start);
    let after_distance = current.end.saturating_sub(visible.end);
    let expand_after =
        can_expand_after && (!can_expand_before || after_distance <= before_distance);
    if expand_after {
        let covered_blocks = current.end.saturating_add(rows.saturating_sub(1)) / rows;
        let end = covered_blocks
            .saturating_add(1)
            .saturating_mul(rows)
            .min(total_rows);
        current.start..end
    } else {
        let first_block = current.start / rows;
        let start = first_block.saturating_sub(1).saturating_mul(rows);
        start..current.end
    }
}

const fn budget_pressure(error: &LedgerError) -> Option<LedgerError> {
    match error {
        LedgerError::BudgetExceeded { .. }
        | LedgerError::TransientBudgetExceeded { .. }
        | LedgerError::ReplacementTransitionActive { .. } => Some(*error),
        LedgerError::ReservationExceeded { .. }
        | LedgerError::UnknownAllocation(_)
        | LedgerError::IdOverflow
        | LedgerError::Poisoned => None,
    }
}

fn evidence_selection_budget_pressure(error: &EvidenceSelectionError) -> Option<LedgerError> {
    match error {
        EvidenceSelectionError::Ledger(error) => budget_pressure(error),
        EvidenceSelectionError::UncertifiedInterval(_)
        | EvidenceSelectionError::IndexOverflow
        | EvidenceSelectionError::InvalidChunkIndex
        | EvidenceSelectionError::MissingHandle(_) => None,
    }
}

fn evidence_store_budget_pressure(error: &EvidenceStoreError) -> Option<LedgerError> {
    match error {
        EvidenceStoreError::Build(EvidenceBuildError::Ledger(error))
        | EvidenceStoreError::Ledger(error)
        | EvidenceStoreError::ExactDepthSummary(ExactDepthSummaryError::Ledger(error))
        | EvidenceStoreError::ExactAlleleSummary(ExactAlleleSummaryError::Ledger(error))
        | EvidenceStoreError::ExactJunctionSummary(ExactJunctionSummaryError::Ledger(error))
        | EvidenceStoreError::ExactPairSummary(ExactPairSummaryError::Ledger(error))
        | EvidenceStoreError::ExactModificationSummary(ExactModificationSummaryError::Ledger(
            error,
        ))
        | EvidenceStoreError::ExactFilterSummary(ExactFilterSummaryError::Ledger(error))
        | EvidenceStoreError::ExactSummaryRoot(ExactSummaryRootError::Ledger(error))
        | EvidenceStoreError::RowAtlas(AlignmentRowAtlasError::Ledger(error)) => {
            budget_pressure(error)
        }
        EvidenceStoreError::ExactDepthSummary(ExactDepthSummaryError::Evidence(error))
        | EvidenceStoreError::ExactAlleleSummary(ExactAlleleSummaryError::Evidence(error))
        | EvidenceStoreError::ExactJunctionSummary(ExactJunctionSummaryError::Evidence(error))
        | EvidenceStoreError::ExactPairSummary(ExactPairSummaryError::Evidence(error))
        | EvidenceStoreError::ExactModificationSummary(ExactModificationSummaryError::Evidence(
            error,
        ))
        | EvidenceStoreError::RowAtlas(AlignmentRowAtlasError::EvidenceSelection(error)) => {
            evidence_selection_budget_pressure(error)
        }
        EvidenceStoreError::Build(_)
        | EvidenceStoreError::ExactDepthSummary(_)
        | EvidenceStoreError::ExactAlleleSummary(_)
        | EvidenceStoreError::ExactJunctionSummary(_)
        | EvidenceStoreError::ExactPairSummary(_)
        | EvidenceStoreError::ExactModificationSummary(_)
        | EvidenceStoreError::ExactFilterSummary(_)
        | EvidenceStoreError::ExactSummaryRoot(_)
        | EvidenceStoreError::RowAtlas(_)
        | EvidenceStoreError::SourceRevisionMismatch
        | EvidenceStoreError::RecordContractMismatch
        | EvidenceStoreError::SummaryEvidenceMismatch
        | EvidenceStoreError::RowAtlasEvidenceMismatch
        | EvidenceStoreError::UncoveredTarget(_)
        | EvidenceStoreError::RevisionOverflow => None,
    }
}

fn presentation_budget_pressure(error: &AlignmentPresentationStoreError) -> Option<LedgerError> {
    match error {
        AlignmentPresentationStoreError::Ledger(ledger)
        | AlignmentPresentationStoreError::SemanticPage(SemanticPageError::Ledger(ledger))
        | AlignmentPresentationStoreError::SummaryPage(SummarySemanticPageError::Ledger(ledger))
        | AlignmentPresentationStoreError::ReadScene(AlignmentPresentationSceneError::Ledger(
            ledger,
        ))
        | AlignmentPresentationStoreError::Snapshot(AlignmentSceneSnapshotError::Ledger(ledger)) => {
            budget_pressure(ledger)
        }
        AlignmentPresentationStoreError::SemanticPage(SemanticPageError::EvidenceSelection(
            error,
        )) => evidence_selection_budget_pressure(error),
        AlignmentPresentationStoreError::Tile(_)
        | AlignmentPresentationStoreError::SemanticPage(_)
        | AlignmentPresentationStoreError::SummaryPage(_)
        | AlignmentPresentationStoreError::ReadScene(_)
        | AlignmentPresentationStoreError::Snapshot(_)
        | AlignmentPresentationStoreError::AuthorityMismatch
        | AlignmentPresentationStoreError::AnnotationAuthorityMismatch
        | AlignmentPresentationStoreError::UnpreparedInterval(_)
        | AlignmentPresentationStoreError::RowCoordinateOverflow
        | AlignmentPresentationStoreError::UnpreparedRows { .. }
        | AlignmentPresentationStoreError::RevisionOverflow => None,
    }
}

/// Extracts only shared-ledger admission pressure from a coordinator failure. Keeping this typed
/// at the coordinator boundary avoids relying on `Error::source` traversal across generic nested
/// allocation wrappers in the threaded engine.
pub(crate) fn coordinator_budget_pressure<E>(
    error: &AlignmentSceneCoordinatorError<E>,
) -> Option<LedgerError> {
    match error {
        AlignmentSceneCoordinatorError::Store(error) => evidence_store_budget_pressure(error),
        AlignmentSceneCoordinatorError::Presentation(error) => presentation_budget_pressure(error),
        AlignmentSceneCoordinatorError::Source(_)
        | AlignmentSceneCoordinatorError::Schedule(_)
        | AlignmentSceneCoordinatorError::Config(_)
        | AlignmentSceneCoordinatorError::CameraContigMismatch
        | AlignmentSceneCoordinatorError::UnknownOwner(_)
        | AlignmentSceneCoordinatorError::CameraOutsideContig { .. }
        | AlignmentSceneCoordinatorError::CameraSequenceOverflow
        | AlignmentSceneCoordinatorError::MissingCamera
        | AlignmentSceneCoordinatorError::IncompleteCachedHorizon(_) => None,
    }
}

fn intersect_intervals(left: GenomicInterval, right: GenomicInterval) -> Option<GenomicInterval> {
    if left.contig() != right.contig() {
        return None;
    }
    let start = left.start().max(right.start());
    let end = left.end().min(right.end());
    (start < end).then(|| {
        GenomicInterval::new(left.contig(), start, end)
            .expect("intersection of valid intervals is valid")
    })
}

fn intersect_ranges(left: Range<u32>, right: Range<u32>) -> Range<u32> {
    let start = left.start.max(right.start);
    let end = left.end.min(right.end);
    start..end.max(start)
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum AlignmentSceneCoordinatorConfigError {
    #[error("alignment scene coordinator requires a non-empty contig")]
    EmptyContig,
    #[error("alignment scene source revision does not match its actor store")]
    SourceAuthorityMismatch,
    #[error("alignment scene record contract does not match its actor store")]
    RecordAuthorityMismatch,
    #[error("alignment scene source already contains owner {0:?}")]
    DuplicateOwner(SourceOwnerId),
    #[error("alignment scene source does not contain owner {0:?}")]
    UnknownOwner(SourceOwnerId),
    #[error("alignment presentation reference revision does not match its supplied bases")]
    ReferenceAuthorityMismatch,
    #[error("alignment presentation annotation revision does not match its supplied index")]
    AnnotationAuthorityMismatch,
    #[error("alignment reference bases do not cover the configured contig")]
    ReferenceCoverageMismatch,
    #[error("alignment preparation distances must be finite and non-negative")]
    InvalidPreparationDistance,
    #[error("alignment row coordinates exceed u32")]
    RowCoordinateOverflow,
    #[error(transparent)]
    Interval(#[from] rgv_core::IntervalError),
    #[error(transparent)]
    Tile(#[from] rgv_alignment_scene::CanonicalTileError),
}

#[derive(Debug, Error)]
pub enum AlignmentSceneCoordinatorError<E> {
    #[error(transparent)]
    Source(#[from] AlignmentSourceActorError<E>),
    #[error(transparent)]
    Schedule(#[from] SourceScheduleError<()>),
    #[error(transparent)]
    Store(#[from] EvidenceStoreError),
    #[error(transparent)]
    Presentation(#[from] AlignmentPresentationStoreError),
    #[error(transparent)]
    Config(#[from] AlignmentSceneCoordinatorConfigError),
    #[error("alignment camera belongs to a different contig")]
    CameraContigMismatch,
    #[error("alignment scene source does not contain owner {0:?}")]
    UnknownOwner(SourceOwnerId),
    #[error(
        "alignment camera interval {interval:?} exceeds configured contig length {contig_length}"
    )]
    CameraOutsideContig {
        interval: GenomicInterval,
        contig_length: u64,
    },
    #[error("alignment camera sequence space exhausted")]
    CameraSequenceOverflow,
    #[error("alignment scene coordinator has no live camera")]
    MissingCamera,
    #[error("cached evidence did not cover required semantic-page horizon {0:?}")]
    IncompleteCachedHorizon(GenomicInterval),
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, path::Path, sync::Arc};

    use rgv_alignment_scene::{
        AlignmentCamera, AlignmentComponent, AlignmentPanelLaneCapture,
        AlignmentPanelLanePublication, AlignmentPanelSceneAdmission, AlignmentPanelSceneStore,
        AlignmentStyleContract, AlleleSummaryDetail, AllocationLedger, CapabilityRequirement,
        EvidenceStore, GeometryLod, RevisionFingerprint,
    };
    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, OverlappingMatePolicy,
        PanelId, ReadStackingMode, TrackId,
    };
    use rgv_hts::{
        AlignedBaseClassification, AlignmentOpenOptions, AlignmentSource, LocalAlignmentSource,
        QueryOptions, TemplateLengthBounds,
    };

    use super::*;
    use crate::{LocalBamCanonicalBackend, LocalBamSourceControl};

    type TestCoordinator = AlignmentSceneCoordinator<LocalBamCanonicalBackend>;

    fn no_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [*b"XS"],
            None,
        )
    }

    fn test_coordinator_with_ledger(
        ledger: AllocationLedger,
    ) -> (TestCoordinator, AlignmentCamera) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        let source = LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let contig = ContigId::new(0);
        let contig_length = source.reference_dictionary().get(contig).unwrap().length();
        let backend = LocalBamCanonicalBackend::new(
            source,
            QueryOptions {
                max_records: NonZeroUsize::new(1_000).unwrap(),
                ..QueryOptions::default()
            },
        )
        .unwrap();
        let source_revision = backend.source_revision();
        let record_contract = backend.record_contract();
        let store = EvidenceStore::new(source_revision, record_contract, ledger);
        let actor = AlignmentSourceActor::new(
            backend,
            store,
            NonZeroUsize::new(4).unwrap(),
            NonZeroUsize::new(2).unwrap(),
        );
        let style = AlignmentStyleContract {
            show_modified_bases: false,
            ..AlignmentStyleContract::default()
        };
        let contract = AlignmentSceneBuildContract {
            source_revision,
            record_contract,
            contig,
            contig_length,
            payload: no_payload(),
            summaries: SummaryContract {
                overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
                minimum_base_quality: 0,
                minimum_junction_flank: 0,
                allele_detail: AlleleSummaryDetail::None,
                collect_modifications: false,
            },
            layout: LayoutContract {
                row_scale_class: 0,
                stacking: ReadStackingMode::Compact,
                grouping: AlignmentGroupMode::None,
                group_order: AlignmentOrderDirection::Natural,
                sorting: AlignmentSortMode::Start,
                sort_order: AlignmentOrderDirection::Natural,
                sort_anchor: None,
                view_as_pairs: false,
                pair_length_bounds: TemplateLengthBounds::default(),
                promoted_template: None,
            },
            presentation: PresentationContract {
                geometry_lod: GeometryLod::Read,
                style,
                reference: None,
                annotation: None,
            },
            geometry_tile_bases: NonZeroU64::new(1).unwrap(),
            summary_tile_bases: NonZeroU64::new(1).unwrap(),
            row_block_rows: NonZeroU32::new(8).unwrap(),
            normalization_mapped_records: None,
            reference: None,
            annotation: None,
        };
        let policy = AlignmentScenePreparationPolicy::new(1.0, 1.0, 0.25, 4, 1).unwrap();
        let coordinator =
            AlignmentSceneCoordinator::new(actor, contract, policy, SourceOwnerId::new(7)).unwrap();
        let interval = GenomicInterval::new(contig, 9_999, 10_001).unwrap();
        let camera = AlignmentCamera::new(
            interval,
            0.0,
            12.0,
            9.0,
            rgv_alignment_scene::CpuClipRect {
                x: 0.0,
                y: 0.0,
                width: 400.0,
                height: 72.0,
            },
        )
        .unwrap();
        (coordinator, camera)
    }

    fn test_coordinator() -> (TestCoordinator, AlignmentCamera) {
        test_coordinator_with_ledger(AllocationLedger::new_with_transient_budget(
            64 << 20,
            32 << 20,
            64 << 20,
        ))
    }

    fn test_source_coordinator() -> (
        AlignmentSceneSourceCoordinator<LocalBamCanonicalBackend>,
        AlignmentSceneBuildContract,
        AlignmentScenePreparationPolicy,
        AlignmentCamera,
    ) {
        test_source_coordinator_with_ledger(AllocationLedger::new_with_transient_budget(
            64 << 20,
            32 << 20,
            64 << 20,
        ))
    }

    fn test_source_coordinator_with_ledger(
        ledger: AllocationLedger,
    ) -> (
        AlignmentSceneSourceCoordinator<LocalBamCanonicalBackend>,
        AlignmentSceneBuildContract,
        AlignmentScenePreparationPolicy,
        AlignmentCamera,
    ) {
        let (coordinator, camera) = test_coordinator_with_ledger(ledger);
        let contract = coordinator.contract().clone();
        let policy = coordinator.preparation_policy();
        let AlignmentSceneCoordinator { mut source, owner } = coordinator;
        assert!(source.remove_lane(owner));
        let AlignmentSceneSourceCoordinator { actor, .. } = source;
        (
            AlignmentSceneSourceCoordinator::new(actor),
            contract,
            policy,
            camera,
        )
    }

    fn test_coordinator_with_contract(
        configure: impl FnOnce(&mut AlignmentSceneBuildContract),
    ) -> (TestCoordinator, AlignmentCamera) {
        let (source, mut contract, policy, camera) = test_source_coordinator();
        configure(&mut contract);
        let AlignmentSceneSourceCoordinator { actor, .. } = source;
        let coordinator =
            AlignmentSceneCoordinator::new(actor, contract, policy, SourceOwnerId::new(7)).unwrap();
        (coordinator, camera)
    }

    fn test_reference_revision(value: u64) -> ReferenceRevision {
        ReferenceRevision::new(RevisionFingerprint::new(0x5245_4645_5245_4e43, value))
    }

    fn base_reference_coordinator(
        revision: ReferenceRevision,
    ) -> (TestCoordinator, AlignmentCamera) {
        test_coordinator_with_contract(|contract| {
            contract.payload = PayloadContract::new(
                CapabilityRequirement::Required,
                CapabilityRequirement::Required,
                [*b"XS"],
                None,
            );
            contract.presentation.geometry_lod = GeometryLod::Base;
            contract.presentation.reference = Some(revision);
            contract.reference = None;
        })
    }

    fn allele_reference_coordinator(
        revision: ReferenceRevision,
    ) -> (TestCoordinator, AlignmentCamera) {
        test_coordinator_with_contract(|contract| {
            contract.payload = PayloadContract::new(
                CapabilityRequirement::Required,
                CapabilityRequirement::Required,
                [*b"XS"],
                None,
            );
            contract.summaries.allele_detail = AlleleSummaryDetail::Counts;
            contract.presentation.reference = Some(revision);
            contract.reference = None;
        })
    }

    fn reference_authority(
        revision: ReferenceRevision,
        interval: GenomicInterval,
    ) -> AlignmentReferenceAuthority {
        let bases = vec![b'A'; usize::try_from(interval.len()).unwrap()];
        AlignmentReferenceAuthority::new(revision, Arc::new(ReferenceSlice { interval, bases }))
    }

    fn pump_until_source_complete_with_core_publication(coordinator: &mut TestCoordinator) {
        for _ in 0..256 {
            let event = coordinator.pump_once().unwrap();
            if matches!(
                event.source_event,
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                let frame = coordinator.frame().unwrap();
                assert!(frame.visible_complete());
                assert!(frame.scene_contracts_current);
                assert!(!frame.reference_products_current);
                assert_eq!(
                    frame
                        .snapshot
                        .as_ref()
                        .unwrap()
                        .reads()
                        .presentation()
                        .reference,
                    None
                );
                return;
            }
        }
        panic!("bounded local fixture did not complete source acquisition");
    }

    fn pump_until_publication(coordinator: &mut TestCoordinator) -> SceneRevision {
        for _ in 0..256 {
            let event = coordinator.pump_once().unwrap();
            if let Some(revision) = event.published_scene {
                return revision;
            }
        }
        panic!("bounded local fixture did not publish a scene");
    }

    fn pump_until_speculation_complete(coordinator: &mut TestCoordinator) {
        for _ in 0..256 {
            let event = coordinator.pump_once().unwrap();
            if matches!(
                event.source_event,
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { publish: None, .. })
            ) {
                return;
            }
        }
        panic!("bounded local fixture did not complete speculation");
    }

    fn scene_population(
        scene: &AlignmentSceneSnapshot,
    ) -> Vec<rgv_alignment_scene::AlignmentHandle> {
        let mut handles = scene
            .reads()
            .evidence()
            .select(scene.prepared_interval())
            .unwrap()
            .records()
            .iter()
            .map(|record| record.handle)
            .collect::<Vec<_>>();
        handles.sort_unstable();
        handles
    }

    #[test]
    fn frame_row_activity_is_camera_exact_while_residency_remains_atlas_wide() {
        let (mut coordinator, camera) = test_coordinator();
        let policy = coordinator
            .preparation_policy()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);

        let frame = coordinator.frame().unwrap();
        let snapshot = frame.snapshot.as_ref().unwrap();
        let atlas = snapshot.reads().atlas();
        let profile = frame
            .row_activity
            .as_ref()
            .expect("a fully covered read-row camera has exact activity");
        assert_eq!(profile.atlas_revision(), atlas.revision());
        assert_eq!(profile.interval(), camera.interval());
        assert_ne!(profile.interval(), atlas.interval());
        assert_eq!(profile.stable_row_count(), atlas.row_count());
        assert!(profile.bins().len() <= ALIGNMENT_ROW_ACTIVITY_BINS);
        assert!(profile.visible_record_count() > 0);

        let atlas_wide_total = match frame.vertical_residency {
            AlignmentVerticalResidency::Preparing { total_rows, .. }
            | AlignmentVerticalResidency::Complete { total_rows }
            | AlignmentVerticalResidency::BudgetLimited { total_rows, .. } => total_rows,
            AlignmentVerticalResidency::ViewportBounded => {
                panic!("the explicitly enabled complete-residency policy must report totals")
            }
        };
        assert_eq!(
            usize::try_from(atlas_wide_total).unwrap(),
            atlas.row_count()
        );
    }

    #[test]
    fn row_activity_authority_ignores_visible_rows_but_rejects_genomic_or_layout_change() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);

        let frame = coordinator.frame().unwrap();
        let snapshot = frame.snapshot.as_ref().unwrap();
        let profile = frame.row_activity.as_ref().unwrap();
        let vertically_scrolled = AlignmentCamera::from_viewport(
            camera.viewport(),
            camera.row_scroll() + 1_000.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        assert_ne!(camera.visible_rows(), vertically_scrolled.visible_rows());
        assert!(alignment_row_activity_satisfies_contract(
            profile,
            snapshot,
            vertically_scrolled.interval(),
            coordinator.contract(),
        ));

        let shifted = GenomicInterval::new(
            camera.interval().contig(),
            camera.interval().start() + 1,
            camera.interval().end() + 1,
        )
        .unwrap();
        assert!(!alignment_row_activity_satisfies_contract(
            profile,
            snapshot,
            shifted,
            coordinator.contract(),
        ));

        let mut changed_layout = coordinator.contract().clone();
        changed_layout.layout.row_scale_class =
            changed_layout.layout.row_scale_class.saturating_add(1);
        assert!(!alignment_row_activity_satisfies_contract(
            profile,
            snapshot,
            camera.interval(),
            &changed_layout,
        ));
    }

    #[test]
    fn row_activity_rejects_disjoint_or_stale_snapshot_authority() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let initial = coordinator.frame().unwrap();
        let initial_snapshot = Arc::clone(initial.snapshot.as_ref().unwrap());
        let initial_profile = initial.row_activity.as_ref().unwrap();
        assert_eq!(initial_profile.interval(), camera.interval());

        let disjoint_viewport = camera
            .viewport()
            .pan_pixels(600.0, coordinator.contract().contig_length)
            .unwrap();
        let disjoint = AlignmentCamera::from_viewport(
            disjoint_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        coordinator.update_camera(disjoint).unwrap();
        let disjoint_frame = coordinator.frame().unwrap();
        assert!(Arc::ptr_eq(
            disjoint_frame.snapshot.as_ref().unwrap(),
            &initial_snapshot
        ));
        assert!(
            !initial_snapshot
                .prepared_interval()
                .contains_interval(disjoint.interval())
        );
        assert!(disjoint_frame.row_activity.is_none());

        // A cached profile is also rejected when the snapshot's row-layout contract no longer
        // matches the lane. Restoring that exact contract produces a newly validated profile for
        // the camera rather than reusing stale authority.
        coordinator.update_camera(camera).unwrap();
        let original_layout = coordinator.contract().layout;
        coordinator
            .source
            .lanes
            .get_mut(&coordinator.owner)
            .unwrap()
            .contract
            .layout
            .row_scale_class = original_layout.row_scale_class.saturating_add(1);
        assert!(coordinator.frame().unwrap().row_activity.is_none());
        coordinator
            .source
            .lanes
            .get_mut(&coordinator.owner)
            .unwrap()
            .contract
            .layout = original_layout;
        let restored = coordinator.frame().unwrap();
        let restored_profile = restored.row_activity.as_ref().unwrap();
        assert_eq!(restored_profile.interval(), camera.interval());
        assert_eq!(
            restored_profile.atlas_revision(),
            restored
                .snapshot
                .as_ref()
                .unwrap()
                .reads()
                .atlas()
                .revision()
        );
    }

    #[test]
    fn base_contract_without_bases_reports_the_exact_pending_source_horizon() {
        let revision = test_reference_revision(1);
        let (mut coordinator, camera) = base_reference_coordinator(revision);

        let update = coordinator.update_camera(camera).unwrap();
        assert!(update.foreground_submitted.is_some());
        assert!(!update.visible_complete);
        let frame = coordinator.frame().unwrap();
        let expected = GenomicInterval::new(camera.interval().contig(), 9_997, 10_003).unwrap();
        assert_eq!(
            frame.reference_requirement,
            AlignmentReferenceRequirement::Pending {
                revision,
                horizon: expected,
            }
        );
        assert!(frame.snapshot.is_none());
        assert!(!frame.scene_contracts_current);
        assert!(!frame.visible_complete());
    }

    #[test]
    fn allele_reference_requirement_adds_an_exact_bounded_context_halo() {
        let revision = test_reference_revision(101);
        let (mut coordinator, camera) = allele_reference_coordinator(revision);
        coordinator.update_camera(camera).unwrap();
        let source_horizon =
            GenomicInterval::new(camera.interval().contig(), 9_997, 10_003).unwrap();
        let reference_horizon =
            GenomicInterval::new(camera.interval().contig(), 9_995, 10_005).unwrap();
        assert_eq!(
            coordinator.frame().unwrap().reference_requirement,
            AlignmentReferenceRequirement::Pending {
                revision,
                horizon: reference_horizon,
            }
        );

        pump_until_source_complete_with_core_publication(&mut coordinator);
        let before = coordinator.telemetry();
        assert_eq!(
            coordinator
                .install_reference_authority(reference_authority(revision, source_horizon))
                .unwrap(),
            AlignmentReferenceInstallOutcome::default(),
            "a source-only horizon must not masquerade as bisulfite context authority"
        );
        let installed = coordinator
            .install_reference_authority(reference_authority(revision, reference_horizon))
            .unwrap();
        assert!(installed.installed);
        assert!(installed.published_scene.is_some());
        assert_eq!(
            coordinator.frame().unwrap().reference_requirement,
            AlignmentReferenceRequirement::Ready {
                revision,
                horizon: reference_horizon,
            }
        );
        assert_eq!(
            coordinator.telemetry().source_acquisitions,
            before.source_acquisitions,
            "installing the FASTA halo must reuse completed source roots"
        );

        let contract = coordinator.contract();
        assert_eq!(
            required_reference_horizon(
                contract,
                GenomicInterval::new(contract.contig, 0, 3).unwrap()
            ),
            GenomicInterval::new(contract.contig, 0, 5).unwrap()
        );
        assert_eq!(
            required_reference_horizon(
                contract,
                GenomicInterval::new(
                    contract.contig,
                    contract.contig_length - 3,
                    contract.contig_length,
                )
                .unwrap()
            ),
            GenomicInterval::new(
                contract.contig,
                contract.contig_length - 5,
                contract.contig_length,
            )
            .unwrap()
        );
    }

    #[test]
    fn core_scene_completes_while_reference_is_pending_and_upgrade_reuses_source_roots() {
        let revision = test_reference_revision(2);
        let (mut coordinator, camera) = base_reference_coordinator(revision);
        coordinator.update_camera(camera).unwrap();
        let horizon = match coordinator.frame().unwrap().reference_requirement {
            AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
            other => panic!("expected pending reference requirement, got {other:?}"),
        };

        pump_until_source_complete_with_core_publication(&mut coordinator);
        assert!(coordinator.actor().store().current().is_some());
        assert!(coordinator.actor().store().current_summaries().is_some());
        assert!(coordinator.actor().store().current_row_atlas().is_some());
        let pending = coordinator.frame().unwrap();
        assert!(pending.snapshot.is_some());
        assert!(pending.scene_contracts_current);
        assert!(pending.visible_complete());
        assert!(!pending.reference_products_current);
        assert_eq!(
            pending.reference_requirement,
            AlignmentReferenceRequirement::Pending { revision, horizon }
        );
        let before = coordinator.telemetry();
        assert_eq!(before.source_acquisitions, 1);
        assert_eq!(before.presentation_candidates, 1);

        let installed = coordinator
            .install_reference_authority(reference_authority(revision, horizon))
            .unwrap();
        assert!(installed.installed);
        assert!(installed.published_scene.is_some());
        let after = coordinator.telemetry();
        assert_eq!(after.source_acquisitions, before.source_acquisitions);
        assert_eq!(after.source_work_started, before.source_work_started);
        assert_eq!(after.evidence_candidates, before.evidence_candidates);
        assert_eq!(
            after.exact_summary_candidates,
            before.exact_summary_candidates
        );
        assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
        assert_eq!(
            after.presentation_candidates,
            before.presentation_candidates + 1
        );

        let ready = coordinator.frame().unwrap();
        assert_eq!(
            ready.reference_requirement,
            AlignmentReferenceRequirement::Ready { revision, horizon }
        );
        assert!(ready.scene_contracts_current);
        assert!(ready.reference_products_current);
        assert!(ready.visible_complete());
        assert_eq!(
            ready
                .snapshot
                .as_ref()
                .unwrap()
                .reads()
                .presentation()
                .reference,
            Some(revision)
        );
        let substitutions = ready
            .snapshot
            .as_ref()
            .unwrap()
            .reads()
            .pages()
            .iter()
            .flat_map(|page| page.primitives())
            .filter(|primitive| {
                matches!(
                    primitive.component,
                    AlignmentComponent::Base {
                        classification: AlignedBaseClassification::Substitution
                            | AlignedBaseClassification::DeclaredSubstitution,
                        ..
                    }
                )
            })
            .count();
        assert!(substitutions > 0);
    }

    #[test]
    fn stale_or_insufficient_reference_authority_is_an_owner_local_no_op() {
        let revision = test_reference_revision(3);
        let (mut coordinator, camera) = base_reference_coordinator(revision);
        coordinator.update_camera(camera).unwrap();
        let horizon = match coordinator.frame().unwrap().reference_requirement {
            AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
            other => panic!("expected pending reference requirement, got {other:?}"),
        };
        pump_until_source_complete_with_core_publication(&mut coordinator);
        let before = coordinator.telemetry();

        let stale = coordinator
            .install_reference_authority(reference_authority(test_reference_revision(300), horizon))
            .unwrap();
        assert_eq!(stale, AlignmentReferenceInstallOutcome::default());

        let short = GenomicInterval::new(
            horizon.contig(),
            horizon.start(),
            horizon.end().saturating_sub(1),
        )
        .unwrap();
        let insufficient = coordinator
            .install_reference_authority(reference_authority(revision, short))
            .unwrap();
        assert_eq!(insufficient, AlignmentReferenceInstallOutcome::default());
        assert!(coordinator.contract().reference.is_none());
        assert_eq!(coordinator.telemetry(), before);
        assert_eq!(
            coordinator.frame().unwrap().reference_requirement,
            AlignmentReferenceRequirement::Pending { revision, horizon }
        );
    }

    #[test]
    fn installing_reference_for_one_lane_does_not_authorize_another_lane() {
        let (mut source, mut contract, policy, camera) = test_source_coordinator();
        let revision = test_reference_revision(5);
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.presentation.geometry_lod = GeometryLod::Base;
        contract.presentation.reference = Some(revision);
        contract.reference = None;
        let first = SourceOwnerId::new(41);
        let second = SourceOwnerId::new(42);
        source.insert_lane(first, contract.clone(), policy).unwrap();
        source.insert_lane(second, contract, policy).unwrap();

        source.update_camera(first, camera).unwrap();
        for _ in 0..256 {
            let pump = source.pump_once().unwrap();
            if matches!(
                pump.source_event,
                Some(AlignmentSourceActorEvent::AcquisitionCompleted { .. })
            ) {
                break;
            }
        }
        source.update_camera(second, camera).unwrap();
        let horizon = match source.frame(first).unwrap().unwrap().reference_requirement {
            AlignmentReferenceRequirement::Pending { horizon, .. } => horizon,
            other => panic!("expected pending reference requirement, got {other:?}"),
        };

        let installed = source
            .install_reference_authority(first, reference_authority(revision, horizon))
            .unwrap();
        assert!(installed.installed);
        assert!(installed.published_scene.is_some());
        assert_eq!(
            source.frame(first).unwrap().unwrap().reference_requirement,
            AlignmentReferenceRequirement::Ready { revision, horizon }
        );
        let second_frame = source.frame(second).unwrap().unwrap();
        assert_eq!(
            second_frame.reference_requirement,
            AlignmentReferenceRequirement::Pending { revision, horizon }
        );
        assert!(second_frame.snapshot.is_some());
        assert!(second_frame.visible_complete());
        assert!(!second_frame.reference_products_current);
        assert_eq!(
            second_frame
                .snapshot
                .as_ref()
                .unwrap()
                .reads()
                .presentation()
                .reference,
            None
        );
        assert!(source.contract(second).unwrap().reference.is_none());
    }

    #[test]
    fn span_and_read_publication_do_not_require_reference_authority() {
        for geometry_lod in [GeometryLod::Span, GeometryLod::Read] {
            let (mut coordinator, camera) = test_coordinator_with_contract(|contract| {
                contract.presentation.geometry_lod = geometry_lod;
            });
            let update = coordinator.update_camera(camera).unwrap();
            assert!(update.foreground_submitted.is_some());
            assert_eq!(
                coordinator.frame().unwrap().reference_requirement,
                AlignmentReferenceRequirement::NotRequired
            );
            pump_until_publication(&mut coordinator);
            let frame = coordinator.frame().unwrap();
            assert!(frame.visible_complete());
            assert!(frame.scene_contracts_current);
            assert_eq!(
                frame.reference_requirement,
                AlignmentReferenceRequirement::NotRequired
            );
            assert_eq!(
                frame
                    .snapshot
                    .as_ref()
                    .unwrap()
                    .reads()
                    .presentation()
                    .geometry_lod,
                geometry_lod
            );
        }
    }

    #[test]
    fn geometric_overlap_does_not_make_a_stale_contract_visible_complete() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);

        let revision = test_reference_revision(4);
        let mut contract = coordinator.contract().clone();
        contract.payload = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"XS"],
            None,
        );
        contract.presentation.geometry_lod = GeometryLod::Base;
        contract.presentation.reference = Some(revision);
        contract.reference = None;
        let update = coordinator.replace_contract(contract).unwrap();
        assert!(!update.visible_complete);

        let frame = coordinator.frame().unwrap();
        assert!(frame.snapshot.is_some());
        assert!(frame.covered_interval.is_some());
        assert!(!frame.scene_contracts_current);
        assert!(!frame.visible_complete());
        assert!(matches!(
            frame.reference_requirement,
            AlignmentReferenceRequirement::Pending {
                revision: pending,
                ..
            } if pending == revision
        ));
        assert_eq!(
            frame.individual_reads,
            AlignmentIndividualReadCapability::Pending
        );
    }

    #[test]
    fn genomic_only_camera_acquires_exact_summaries_without_read_row_demand() {
        let (mut coordinator, render_camera) = test_coordinator();
        let policy = coordinator
            .preparation_policy()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        let camera = AlignmentSceneCamera::without_visible_rows(render_camera.viewport());
        let initial = coordinator.update_camera(camera).unwrap();
        assert!(initial.foreground_submitted.is_some());
        assert!(!initial.visible_complete);

        pump_until_publication(&mut coordinator);
        let frame = coordinator.frame().unwrap();
        assert_eq!(frame.camera, camera);
        assert_eq!(frame.requested_rows, 0..0);
        assert_eq!(frame.covered_rows, 0..0);
        assert!(frame.visible_complete(), "{frame:#?}");
        assert!(frame.row_activity.is_none());
        assert_eq!(
            frame.vertical_residency,
            AlignmentVerticalResidency::ViewportBounded
        );
        assert_eq!(
            frame.individual_reads,
            AlignmentIndividualReadCapability::Pending
        );
        let snapshot = frame
            .snapshot
            .expect("summary acquisition publishes a scene");
        assert!(
            snapshot
                .summaries()
                .prepared_interval()
                .contains_interval(camera.interval())
        );
        assert!(!snapshot.summary_pages().is_empty());
        assert_eq!(snapshot.reads().prepared_rows(), 0..0);
        assert!(!coordinator.has_pending_presentation_work());
    }

    #[test]
    fn contained_pan_and_scroll_change_only_the_live_camera() {
        let (mut coordinator, camera) = test_coordinator();
        let initial = coordinator.update_camera(camera).unwrap();
        assert!(initial.foreground_submitted.is_some());
        assert!(!initial.visible_complete);
        let revision = pump_until_publication(&mut coordinator);
        let final_frame = coordinator.frame().unwrap();
        assert!(final_frame.visible_complete(), "{final_frame:#?}");

        let before = coordinator.telemetry();
        let viewport = camera
            .viewport()
            .pan_pixels(20.0, coordinator.contract().contig_length)
            .unwrap();
        let panned = AlignmentCamera::from_viewport(
            viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(panned).unwrap();
        assert!(update.reused_current_scene);
        assert!(update.published_scene.is_none());
        assert!(update.foreground_submitted.is_none());
        assert!(update.speculation_submitted.is_none());
        assert_eq!(
            coordinator.frame().unwrap().snapshot.unwrap().revision(),
            revision
        );

        let scrolled = AlignmentCamera::from_viewport(
            viewport,
            1.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(scrolled).unwrap();
        assert!(update.reused_current_scene);
        assert!(update.published_scene.is_none());
        let after = coordinator.telemetry();
        assert_eq!(after.source_work_started, before.source_work_started);
        assert_eq!(after.evidence_candidates, before.evidence_candidates);
        assert_eq!(
            after.exact_summary_candidates,
            before.exact_summary_candidates
        );
        assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
        assert_eq!(
            after.presentation_candidates,
            before.presentation_candidates
        );
        assert_eq!(after.scene_publications, before.scene_publications);
        assert_eq!(after.camera_only_updates, before.camera_only_updates + 2);
    }

    #[test]
    fn pan_zoom_reversal_and_vertical_return_converge_without_new_scene_work() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        let revision = pump_until_publication(&mut coordinator);
        let contig_length = coordinator.contract().contig_length;
        let anchor = camera.clip().width * 0.5;
        let zoomed_viewport = camera
            .viewport()
            .zoom_at(anchor, 0.5, contig_length)
            .unwrap();
        let zoomed = AlignmentCamera::from_viewport(
            zoomed_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let panned_viewport = zoomed_viewport.pan_pixels(40.0, contig_length).unwrap();
        let panned = AlignmentCamera::from_viewport(
            panned_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let scrolled = AlignmentCamera::from_viewport(
            zoomed_viewport,
            1.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let trace = [zoomed, panned, zoomed, scrolled, zoomed, camera];
        let operations_before = coordinator
            .actor()
            .store()
            .ledger()
            .operation_counters()
            .unwrap();

        for next in trace {
            let update = coordinator.update_camera(next).unwrap();
            assert!(update.visible_complete);
            assert!(update.reused_current_scene);
            assert!(update.published_scene.is_none());
            assert!(update.foreground_submitted.is_none());
            assert!(update.speculation_submitted.is_none());
        }
        let warmed_telemetry = coordinator.telemetry();
        let warmed_ledger = coordinator.actor().store().ledger().progress().unwrap();

        for _ in 0..32 {
            for next in trace {
                let update = coordinator.update_camera(next).unwrap();
                assert!(update.visible_complete);
                assert!(update.reused_current_scene);
            }
        }
        let final_telemetry = coordinator.telemetry();
        let final_ledger = coordinator.actor().store().ledger().progress().unwrap();
        let operations_after = coordinator
            .actor()
            .store()
            .ledger()
            .operation_counters()
            .unwrap();
        assert_eq!(
            final_telemetry.source_work_started,
            warmed_telemetry.source_work_started
        );
        assert_eq!(
            final_telemetry.evidence_candidates,
            warmed_telemetry.evidence_candidates
        );
        assert_eq!(
            final_telemetry.exact_summary_candidates,
            warmed_telemetry.exact_summary_candidates
        );
        assert_eq!(
            final_telemetry.row_atlas_candidates,
            warmed_telemetry.row_atlas_candidates
        );
        assert_eq!(
            final_telemetry.presentation_candidates,
            warmed_telemetry.presentation_candidates
        );
        assert_eq!(
            final_telemetry.scene_publications,
            warmed_telemetry.scene_publications
        );
        assert_eq!(final_ledger.charged, warmed_ledger.charged);
        assert_eq!(final_ledger.retained, warmed_ledger.retained);
        assert_eq!(
            operations_after.full_inventory_snapshots, operations_before.full_inventory_snapshots,
            "camera-only recovery traces must not enumerate the allocation inventory"
        );
        assert_eq!(
            operations_after.allocation_records_enumerated,
            operations_before.allocation_records_enumerated
        );
        assert_eq!(
            coordinator.frame().unwrap().snapshot.unwrap().revision(),
            revision
        );
    }

    #[test]
    fn outrunning_preparation_preserves_the_truthfully_covered_edge() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        let revision = pump_until_publication(&mut coordinator);
        let current_interval = coordinator
            .frame()
            .unwrap()
            .snapshot
            .as_ref()
            .unwrap()
            .prepared_interval();
        let beyond = camera
            .viewport()
            .pan_pixels(600.0, coordinator.contract().contig_length)
            .unwrap();
        let beyond = AlignmentCamera::from_viewport(
            beyond,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        assert!(beyond.interval().overlaps(current_interval));
        assert!(!current_interval.contains_interval(beyond.interval()));

        let update = coordinator.update_camera(beyond).unwrap();
        assert!(!update.visible_complete);
        assert!(update.foreground_submitted.is_some());
        let frame = coordinator.frame().unwrap();
        assert_eq!(frame.snapshot.as_ref().unwrap().revision(), revision);
        let covered = frame
            .covered_interval
            .expect("the old immutable scene must remain visible over its truthful overlap");
        assert!(covered.overlaps(beyond.interval()));
        assert!(covered.len() > 0);
    }

    #[test]
    fn adjacent_pans_keep_a_bounded_rolling_window_and_converge_to_a_clean_scene() {
        let (mut coordinator, initial_camera) = test_coordinator();
        coordinator.update_camera(initial_camera).unwrap();
        pump_until_publication(&mut coordinator);
        let maximum_window_bases = 6_u64;
        let initial_start = initial_camera.interval().start();
        let mut final_camera = initial_camera;

        // Each camera overlaps the preceding camera by one base. This is deliberately long
        // enough to cross the horizontal trigger repeatedly; the former overlap-union behavior
        // grew the desired interval on every crossing even though the configured buffer stayed
        // fixed at one viewport per side.
        for offset in 1..=64_u64 {
            let interval = GenomicInterval::new(
                coordinator.contract().contig,
                initial_start + offset,
                initial_start + offset + initial_camera.interval().len(),
            )
            .unwrap();
            final_camera = AlignmentCamera::new(
                interval,
                0.0,
                initial_camera.row_height(),
                initial_camera.read_height(),
                initial_camera.clip(),
            )
            .unwrap();
            coordinator.update_camera(final_camera).unwrap();

            let desired = coordinator
                .desired_prepared_interval()
                .expect("every camera update installs a preparation target");
            assert!(desired.contains_interval(final_camera.interval()));
            assert!(
                desired.len() <= maximum_window_bases,
                "fixed preparation policy grew into a corridor at offset {offset}: {desired:?}"
            );

            for _ in 0..256 {
                if !coordinator.source.has_pending_source_work() {
                    break;
                }
                coordinator.pump_once().unwrap();
            }
            assert!(!coordinator.source.has_pending_source_work());

            // A speculative edge acquisition intentionally does not replace a still-visible
            // scene. Reapplying the same camera lets the now-complete cache publish the bounded
            // rolling window, exactly as the next native camera checkpoint would.
            coordinator.update_camera(final_camera).unwrap();
            let desired = coordinator.desired_prepared_interval().unwrap();
            assert!(desired.contains_interval(final_camera.interval()));
            assert!(desired.len() <= maximum_window_bases);
        }

        // The final pan lands on a trigger crossing, so the rolling scene itself—not a forced
        // reset—must match a clean load at that camera. Its authority cannot retain population
        // from any of the traversed loci.
        let final_scene = coordinator.frame().unwrap().snapshot.unwrap();

        let (mut clean, _) = test_coordinator();
        clean.update_camera(final_camera).unwrap();
        pump_until_publication(&mut clean);
        let clean_scene = clean.frame().unwrap().snapshot.unwrap();
        assert_eq!(
            final_scene.prepared_interval(),
            clean_scene.prepared_interval()
        );
        assert_eq!(
            scene_population(&final_scene),
            scene_population(&clean_scene)
        );
        assert_eq!(
            final_scene.reads().atlas().assignment_fingerprint(),
            clean_scene.reads().atlas().assignment_fingerprint()
        );
    }

    #[test]
    fn panel_capture_applies_live_pan_immediately_without_waiting_for_the_source_actor() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let source_frame = coordinator.frame().unwrap();
        let snapshot = source_frame.snapshot.unwrap();
        let before = coordinator.telemetry();
        let track = TrackId::new(17);
        let mut panel = AlignmentPanelSceneStore::new([track]);
        assert_eq!(
            panel
                .admit(
                    track,
                    AlignmentPanelLanePublication::new(
                        source_frame.camera_sequence,
                        Some(snapshot.clone()),
                        source_frame.scene_contracts_current,
                    ),
                )
                .unwrap(),
            AlignmentPanelSceneAdmission::SceneReplaced
        );
        let initial_capture =
            AlignmentPanelLaneCapture::new(track, camera, Some(camera), []).unwrap();
        let initial = panel
            .capture(PanelId::new(3), 1, camera.viewport(), [initial_capture])
            .unwrap();
        assert!(initial.lanes[0].visible_complete());

        let live_viewport = camera
            .viewport()
            .pan_pixels(600.0, coordinator.contract().contig_length)
            .unwrap();
        let live_camera = AlignmentCamera::from_viewport(
            live_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let live_capture =
            AlignmentPanelLaneCapture::new(track, live_camera, Some(live_camera), []).unwrap();
        let panned = panel
            .capture(PanelId::new(3), 2, live_camera.viewport(), [live_capture])
            .unwrap();
        assert_eq!(panned.camera_revision, 2);
        assert_eq!(panned.authority_revision, initial.authority_revision);
        assert!(Arc::ptr_eq(
            panned.lanes[0].snapshot.as_ref().unwrap(),
            &snapshot
        ));
        assert!(panned.lanes[0].covered_interval.is_some());
        assert!(!panned.lanes[0].visible_complete());
        assert_eq!(coordinator.telemetry(), before);
    }

    #[test]
    fn panel_capture_keeps_exact_row_authority_while_vertical_pages_are_pending() {
        let (mut coordinator, camera) = test_coordinator();
        let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);

        let source_frame = coordinator.frame().unwrap();
        let snapshot = Arc::clone(source_frame.snapshot.as_ref().unwrap());
        let profile = Arc::clone(source_frame.row_activity.as_ref().unwrap());
        let prepared_rows = snapshot.reads().prepared_rows();
        let total_rows = u32::try_from(snapshot.reads().atlas().row_count()).unwrap();
        assert!(prepared_rows.end < total_rows);

        let track = TrackId::new(18);
        let mut panel = AlignmentPanelSceneStore::new([track]);
        panel
            .admit(
                track,
                AlignmentPanelLanePublication::new(
                    source_frame.camera_sequence,
                    Some(snapshot),
                    source_frame.scene_contracts_current,
                ),
            )
            .unwrap();
        let scrolled = AlignmentCamera::from_viewport(
            camera.viewport(),
            f64::from(prepared_rows.end),
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let capture = AlignmentPanelLaneCapture::new(track, scrolled, Some(scrolled), [])
            .unwrap()
            .with_row_activity(Some(Arc::clone(&profile)));
        let captured = panel
            .capture(PanelId::new(3), 2, scrolled.viewport(), [capture])
            .unwrap();
        let lane = &captured.lanes[0];

        assert!(!lane.reads_complete());
        assert!(lane.covered_rows.is_empty());
        assert!(Arc::ptr_eq(lane.row_activity.as_ref().unwrap(), &profile));
        assert_eq!(
            lane.row_activity.as_ref().unwrap().interval(),
            scrolled.interval()
        );
    }

    #[test]
    fn edge_prefetch_is_speculative_and_later_pan_publishes_without_another_query() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let initial_scene = coordinator.frame().unwrap().snapshot.unwrap();
        let initial_work = coordinator.telemetry().source_work_started;

        let near_edge_viewport = camera
            .viewport()
            .pan_pixels(340.0, coordinator.contract().contig_length)
            .unwrap();
        let near_edge = AlignmentCamera::from_viewport(
            near_edge_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(near_edge).unwrap();
        assert!(update.visible_complete);
        assert!(update.speculation_submitted.is_some());
        assert!(update.foreground_submitted.is_none());
        assert_eq!(
            coordinator.frame().unwrap().snapshot.unwrap().revision(),
            initial_scene.revision()
        );
        pump_until_speculation_complete(&mut coordinator);
        let work_after_prefetch = coordinator.telemetry().source_work_started;
        assert_eq!(work_after_prefetch, initial_work + 1);

        let resident_viewport = camera
            .viewport()
            .pan_pixels(400.0, coordinator.contract().contig_length)
            .unwrap();
        let resident_camera = AlignmentCamera::from_viewport(
            resident_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(resident_camera).unwrap();
        assert!(update.published_scene.is_some());
        assert!(update.foreground_submitted.is_none());
        assert_eq!(
            coordinator.telemetry().source_work_started,
            work_after_prefetch
        );
        assert!(coordinator.frame().unwrap().visible_complete());

        let resident_scene = coordinator.frame().unwrap().snapshot.unwrap();
        let (mut clean, _) = test_coordinator();
        assert!(
            clean
                .set_preparation_policy(
                    AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 4, 1).unwrap(),
                )
                .unwrap()
                .is_none()
        );
        let clean_camera = AlignmentCamera::new(
            resident_scene.prepared_interval(),
            0.0,
            resident_camera.row_height(),
            resident_camera.read_height(),
            resident_camera.clip(),
        )
        .unwrap();
        clean.update_camera(clean_camera).unwrap();
        pump_until_publication(&mut clean);
        let clean_scene = clean.frame().unwrap().snapshot.unwrap();
        assert_eq!(
            resident_scene.prepared_interval(),
            clean_scene.prepared_interval()
        );
        assert_eq!(
            scene_population(&resident_scene),
            scene_population(&clean_scene)
        );
        assert_eq!(
            resident_scene.reads().atlas().assignment_fingerprint(),
            clean_scene.reads().atlas().assignment_fingerprint()
        );
        assert_eq!(
            resident_scene
                .summaries()
                .project_depth_bins(resident_scene.prepared_interval(), 16)
                .unwrap(),
            clean_scene
                .summaries()
                .project_depth_bins(clean_scene.prepared_interval(), 16)
                .unwrap()
        );
    }

    #[test]
    fn adjacent_lod_prewarm_and_zoom_switch_reuse_source_evidence() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let original = coordinator.contract().clone();
        let mut span_contract = original.clone();
        span_contract.presentation.geometry_lod = GeometryLod::Span;
        let source_work = coordinator.telemetry().source_work_started;
        let pages_before = coordinator.presentation().read_page_count();

        assert!(
            coordinator
                .prewarm_presentation(span_contract.presentation)
                .unwrap()
        );
        assert!(coordinator.presentation().read_page_count() > pages_before);
        assert_eq!(coordinator.telemetry().source_work_started, source_work);

        let update = coordinator.replace_contract(span_contract).unwrap();
        assert!(update.published_scene.is_some());
        assert!(update.foreground_submitted.is_none());
        assert!(update.speculation_submitted.is_none());
        assert_eq!(coordinator.telemetry().source_work_started, source_work);
        assert_eq!(
            coordinator
                .frame()
                .unwrap()
                .snapshot
                .unwrap()
                .reads()
                .presentation()
                .geometry_lod,
            GeometryLod::Span
        );

        let update = coordinator.replace_contract(original).unwrap();
        assert!(update.published_scene.is_some());
        assert_eq!(coordinator.telemetry().source_work_started, source_work);
        assert_eq!(
            coordinator
                .frame()
                .unwrap()
                .snapshot
                .unwrap()
                .reads()
                .presentation()
                .geometry_lod,
            GeometryLod::Read
        );
    }

    #[test]
    fn vertical_page_extension_reuses_the_atlas_without_row_assignment_work() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let before = coordinator.telemetry();
        let atlas_revision = coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .reads()
            .atlas()
            .revision();
        let scrolled = AlignmentCamera::from_viewport(
            camera.viewport(),
            6.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(scrolled).unwrap();
        assert!(update.published_scene.is_some());
        assert!(update.visible_complete);
        let after = coordinator.telemetry();
        assert_eq!(after.source_work_started, before.source_work_started);
        assert_eq!(after.evidence_candidates, before.evidence_candidates);
        assert_eq!(
            after.exact_summary_candidates,
            before.exact_summary_candidates
        );
        assert_eq!(after.row_atlas_candidates, before.row_atlas_candidates);
        assert_eq!(
            after.presentation_candidates,
            before.presentation_candidates + 1
        );
        assert_eq!(
            coordinator
                .frame()
                .unwrap()
                .snapshot
                .unwrap()
                .reads()
                .atlas()
                .revision(),
            atlas_revision
        );
    }

    #[test]
    fn visible_scene_publishes_before_incremental_full_vertical_residency() {
        let (mut coordinator, camera) = test_coordinator();
        let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);

        let initial = coordinator.frame().unwrap();
        assert!(initial.visible_complete());
        let initial_scene = initial.snapshot.unwrap();
        let total_rows = u32::try_from(initial_scene.reads().atlas().row_count()).unwrap();
        let initial_rows = initial_scene.reads().prepared_rows();
        assert!(initial_rows.end.saturating_sub(initial_rows.start) < total_rows);
        assert!(matches!(
            initial.vertical_residency,
            AlignmentVerticalResidency::Preparing { .. }
        ));
        let source_work = coordinator.telemetry().source_work_started;
        let atlas_revision = initial_scene.reads().atlas().revision();

        let mut checkpoints = 0_usize;
        while coordinator.has_pending_presentation_work() {
            let pump = coordinator
                .pump_presentation_once()
                .unwrap()
                .expect("pending vertical work produces one checkpoint");
            assert!(!pump.budget_limited);
            assert!(pump.published_scene.is_some());
            checkpoints = checkpoints.saturating_add(1);
            assert!(checkpoints < 256, "vertical warmup did not converge");
        }

        let complete = coordinator.frame().unwrap();
        assert!(matches!(
            complete.vertical_residency,
            AlignmentVerticalResidency::Complete {
                total_rows: completed
            } if completed == total_rows
        ));
        let complete_scene = complete.snapshot.unwrap();
        assert_eq!(complete_scene.reads().prepared_rows(), 0..total_rows);
        assert_eq!(complete_scene.reads().atlas().revision(), atlas_revision);
        let warmed = coordinator.telemetry();
        assert_eq!(warmed.source_work_started, source_work);
        assert_eq!(warmed.vertical_residency_checkpoints as usize, checkpoints);
        assert_eq!(warmed.vertical_residency_publications as usize, checkpoints);
        assert_eq!(warmed.vertical_residency_completions, 1);

        let bottom_scroll = f64::from(total_rows.saturating_sub(1)) * camera.row_height();
        let bottom = AlignmentCamera::from_viewport(
            camera.viewport(),
            bottom_scroll,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(bottom).unwrap();
        assert!(update.visible_complete);
        assert!(update.reused_current_scene);
        assert!(update.published_scene.is_none());
        assert_eq!(
            coordinator.telemetry().presentation_candidates,
            warmed.presentation_candidates
        );
    }

    #[test]
    fn vertical_residency_expansion_is_block_aligned_and_nearest_first() {
        let block = NonZeroU32::new(8).unwrap();
        assert_eq!(expanded_vertical_rows(0..3, 30, block, 0..3), 0..16);
        assert_eq!(expanded_vertical_rows(9..15, 30, block, 10..14), 9..24);
        assert_eq!(expanded_vertical_rows(9..24, 30, block, 10..14), 0..24);
        assert_eq!(expanded_vertical_rows(0..24, 30, block, 10..14), 0..30);
    }

    #[test]
    fn vertical_residency_budget_limit_preserves_visible_authority_and_retries_after_increase() {
        let (mut coordinator, camera) = test_coordinator();
        let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        coordinator.update_camera(camera).unwrap();
        let visible_revision = pump_until_publication(&mut coordinator);
        let charged = coordinator
            .actor()
            .store()
            .ledger()
            .snapshot()
            .unwrap()
            .charged;
        coordinator.reduce_budget(charged).unwrap();

        let limited = coordinator
            .pump_presentation_once()
            .unwrap()
            .expect("full vertical residency is initially pending");
        assert!(limited.budget_limited);
        assert!(limited.published_scene.is_none());
        assert!(!coordinator.has_pending_presentation_work());
        let frame = coordinator.frame().unwrap();
        assert!(frame.visible_complete());
        assert_eq!(frame.snapshot.unwrap().revision(), visible_revision);
        assert!(matches!(
            frame.vertical_residency,
            AlignmentVerticalResidency::BudgetLimited { .. }
        ));
        assert_eq!(coordinator.telemetry().vertical_residency_budget_limits, 1);

        coordinator.reduce_budget(64 << 20).unwrap();
        assert!(coordinator.has_pending_presentation_work());
        for _ in 0..256 {
            if !coordinator.has_pending_presentation_work() {
                break;
            }
            let pump = coordinator.pump_presentation_once().unwrap().unwrap();
            assert!(!pump.budget_limited);
        }
        assert!(matches!(
            coordinator.frame().unwrap().vertical_residency,
            AlignmentVerticalResidency::Complete { .. }
        ));
    }

    #[test]
    fn disjoint_camera_retarget_stops_stale_vertical_residency_at_the_next_block_boundary() {
        let (mut coordinator, camera) = test_coordinator();
        let policy = AlignmentScenePreparationPolicy::new(0.0, 0.0, 0.0, 0, 1)
            .unwrap()
            .with_full_vertical_residency(true);
        assert!(
            coordinator
                .set_preparation_policy(policy)
                .unwrap()
                .is_none()
        );
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let first = coordinator.pump_presentation_once().unwrap().unwrap();
        assert!(first.published_scene.is_some());
        let checkpoints = coordinator.telemetry().vertical_residency_checkpoints;

        let disjoint_viewport = camera
            .viewport()
            .pan_pixels(600.0, coordinator.contract().contig_length)
            .unwrap();
        let disjoint = AlignmentCamera::from_viewport(
            disjoint_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(disjoint).unwrap();
        assert!(!update.visible_complete);
        assert!(update.foreground_submitted.is_some());
        assert!(!coordinator.has_pending_presentation_work());
        assert!(coordinator.pump_presentation_once().unwrap().is_none());
        assert_eq!(
            coordinator.telemetry().vertical_residency_checkpoints,
            checkpoints
        );

        pump_until_publication(&mut coordinator);
        assert!(coordinator.frame().unwrap().visible_complete());
        assert!(coordinator.has_pending_presentation_work());
    }

    #[test]
    fn base_lod_keeps_vertical_residency_viewport_bounded() {
        assert!(!supports_full_vertical_residency(GeometryLod::Overview));
        assert!(supports_full_vertical_residency(GeometryLod::Span));
        assert!(supports_full_vertical_residency(GeometryLod::Read));
        assert!(!supports_full_vertical_residency(GeometryLod::Base));
    }

    #[test]
    fn an_active_gpu_replacement_is_retryable_pressure_not_a_terminal_scene_error() {
        let pressure = LedgerError::ReplacementTransitionActive { bundle_id: 17 };
        assert_eq!(budget_pressure(&pressure), Some(pressure));
    }

    #[test]
    fn pressure_aborts_speculation_and_evicts_inactive_pages_without_clearing_current() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        let revision = pump_until_publication(&mut coordinator);

        let mut span = coordinator.contract().presentation;
        span.geometry_lod = GeometryLod::Span;
        assert!(coordinator.prewarm_presentation(span).unwrap());
        let pages_after_prewarm = coordinator.presentation().read_page_count();

        let near_edge_viewport = camera
            .viewport()
            .pan_pixels(340.0, coordinator.contract().contig_length)
            .unwrap();
        let near_edge = AlignmentCamera::from_viewport(
            near_edge_viewport,
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(near_edge).unwrap();
        assert!(update.speculation_submitted.is_some());
        assert!(matches!(
            coordinator.pump_once().unwrap().source_event,
            Some(AlignmentSourceActorEvent::WorkStarted(ref work))
                if work.kind() == crate::SourceIntentKind::Speculative
        ));

        let ledger_before = coordinator.actor().store().ledger().snapshot().unwrap();
        assert!(ledger_before.charged > ledger_before.pinned);
        let pressure = coordinator.reduce_budget(ledger_before.pinned).unwrap();
        assert!(pressure.speculation_was_pending);
        assert!(pressure.active_speculation_aborted);
        assert!(pressure.presentation.evicted_pages() > 0);
        assert!(coordinator.presentation().read_page_count() < pages_after_prewarm);
        assert!(coordinator.actor().schedule().active().is_none());
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_speculation()
                .is_none()
        );
        assert_eq!(
            coordinator.frame().unwrap().snapshot.unwrap().revision(),
            revision
        );
        assert!(coordinator.frame().unwrap().visible_complete());
        assert_eq!(
            coordinator.telemetry().resource_pressure_speculation_aborts,
            1
        );
        assert_eq!(
            pressure.remaining_overage,
            pressure.charged.saturating_sub(pressure.budget)
        );
    }

    #[test]
    fn foreground_jump_is_latest_wins_and_matches_a_clean_load() {
        let (mut coordinator, initial_camera) = test_coordinator();
        let initial = coordinator.update_camera(initial_camera).unwrap();
        assert!(initial.foreground_submitted.is_some());
        assert!(matches!(
            coordinator.pump_once().unwrap().source_event,
            Some(AlignmentSourceActorEvent::WorkStarted(ref work))
                if work.kind() == crate::SourceIntentKind::Foreground
        ));

        let final_start = coordinator.contract().contig_length.saturating_sub(1_000);
        assert!(final_start > initial_camera.interval().end());
        let final_interval =
            GenomicInterval::new(coordinator.contract().contig, final_start, final_start + 2)
                .unwrap();
        let final_camera = AlignmentCamera::new(
            final_interval,
            0.0,
            initial_camera.row_height(),
            initial_camera.read_height(),
            initial_camera.clip(),
        )
        .unwrap();
        let update = coordinator.update_camera(final_camera).unwrap();
        let latest = update
            .foreground_submitted
            .expect("a disjoint camera jump must replace the old foreground intent");

        let mut published = None;
        for _ in 0..256 {
            let pump = coordinator.pump_once().unwrap();
            if let Some(AlignmentSourceActorEvent::AcquisitionCompleted {
                publish: Some(sequence),
                ..
            }) = pump.source_event
            {
                assert_eq!(sequence, latest, "stale foreground work must never publish");
            }
            if pump.published_scene.is_some() {
                assert!(published.replace(pump.published_scene.unwrap()).is_none());
                break;
            }
        }
        assert!(published.is_some());
        let final_frame = coordinator.frame().unwrap();
        assert!(final_frame.visible_complete());
        assert!(
            coordinator.telemetry().source_yields + coordinator.telemetry().source_retargets > 0,
            "the old in-flight decoder must yield or retarget at a canonical checkpoint"
        );

        let final_scene = coordinator.frame().unwrap().snapshot.unwrap();
        let (mut clean, _) = test_coordinator();
        clean.update_camera(final_camera).unwrap();
        pump_until_publication(&mut clean);
        let clean_scene = clean.frame().unwrap().snapshot.unwrap();
        assert_eq!(
            final_scene.prepared_interval(),
            clean_scene.prepared_interval()
        );
        assert_eq!(
            scene_population(&final_scene),
            scene_population(&clean_scene)
        );
        assert_eq!(
            final_scene.reads().atlas().assignment_fingerprint(),
            clean_scene.reads().atlas().assignment_fingerprint()
        );
        assert_eq!(
            final_scene
                .summaries()
                .project_depth_bins(final_scene.prepared_interval(), 16)
                .unwrap(),
            clean_scene
                .summaries()
                .project_depth_bins(clean_scene.prepared_interval(), 16)
                .unwrap()
        );
    }

    #[test]
    fn preparation_policy_changes_apply_immediately_without_a_hidden_clamp() {
        let (mut coordinator, camera) = test_coordinator();
        coordinator.update_camera(camera).unwrap();
        pump_until_publication(&mut coordinator);
        let initial = coordinator
            .frame()
            .unwrap()
            .snapshot
            .unwrap()
            .prepared_interval();

        let expanded = AlignmentScenePreparationPolicy::new(2.0, 3.0, 0.5, 12, 2).unwrap();
        let update = coordinator
            .set_preparation_policy(expanded)
            .unwrap()
            .unwrap();
        assert_eq!(coordinator.preparation_policy(), expanded);
        assert!(update.visible_complete);
        assert!(update.speculation_submitted.is_some());
        let desired = coordinator.desired_prepared_interval().unwrap();
        assert!(desired.start() < initial.start());
        assert!(desired.end() > initial.end());

        let mut invalid = expanded;
        invalid.ahead_viewports = f64::INFINITY;
        assert!(matches!(
            coordinator.set_preparation_policy(invalid),
            Err(AlignmentSceneCoordinatorError::Config(
                AlignmentSceneCoordinatorConfigError::InvalidPreparationDistance
            ))
        ));
        assert_eq!(coordinator.preparation_policy(), expanded);
    }

    #[test]
    fn camera_outside_the_contig_is_refused_instead_of_silently_clamped() {
        let (mut coordinator, camera) = test_coordinator();
        let invalid = AlignmentCamera::new(
            GenomicInterval::new(
                coordinator.contract().contig,
                coordinator.contract().contig_length,
                coordinator.contract().contig_length + 1,
            )
            .unwrap(),
            0.0,
            camera.row_height(),
            camera.read_height(),
            camera.clip(),
        )
        .unwrap();
        assert!(matches!(
            coordinator.update_camera(invalid),
            Err(AlignmentSceneCoordinatorError::CameraOutsideContig { .. })
        ));
        assert!(coordinator.frame().is_none());
    }

    #[test]
    fn exact_dense_refusal_is_terminal_retains_current_and_retries_after_limit_change() {
        let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator();
        let owner = SourceOwnerId::new(303);
        coordinator.insert_lane(owner, contract, policy).unwrap();

        let empty_camera = AlignmentCamera::new(
            GenomicInterval::new(dense_camera.interval().contig(), 0, 2).unwrap(),
            0.0,
            dense_camera.row_height(),
            dense_camera.read_height(),
            dense_camera.clip(),
        )
        .unwrap();
        coordinator.update_camera(owner, empty_camera).unwrap();
        let initial_revision = (0..256)
            .find_map(|_| {
                coordinator
                    .pump_once()
                    .unwrap()
                    .publications
                    .into_iter()
                    .find(|publication| publication.owner == owner)
                    .map(|publication| publication.revision)
            })
            .expect("the empty foreground target should publish");
        assert!(
            coordinator
                .frame(owner)
                .unwrap()
                .unwrap()
                .visible_complete()
        );

        coordinator
            .actor_mut()
            .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
                NonZeroUsize::new(1).unwrap(),
            ))
            .unwrap();
        assert!(matches!(
            coordinator.pump_once().unwrap().source_event,
            Some(AlignmentSourceActorEvent::ControlApplied)
        ));

        coordinator.update_camera(owner, dense_camera).unwrap();
        let refusal = (0..256)
            .find_map(|_| {
                coordinator
                    .pump_once()
                    .unwrap()
                    .refusals
                    .into_iter()
                    .find(|refusal| refusal.owner == owner)
            })
            .expect("the dense foreground target should refuse individual reads");
        assert_eq!(refusal.complete_read_limit, 1);
        assert_eq!(refusal.eligible_records, 13);

        let refused = coordinator.frame(owner).unwrap().unwrap();
        assert_eq!(
            refused.snapshot.as_ref().map(|scene| scene.revision()),
            Some(initial_revision),
            "a refused candidate must not erase or replace the immutable current scene"
        );
        assert!(!refused.visible_complete());
        assert_eq!(
            refused.individual_reads,
            AlignmentIndividualReadCapability::Refused {
                target: refusal.target,
                complete_read_limit: 1,
                eligible_records: 13,
            }
        );
        assert!(
            !coordinator.has_pending_source_work(),
            "a terminal refusal must not resubmit the same target in a loop"
        );
        let idle = coordinator.pump_once().unwrap();
        assert!(idle.source_event.is_none());
        assert!(idle.refusals.is_empty());

        coordinator
            .actor_mut()
            .submit_control(LocalBamSourceControl::SetCompleteReadLimit(
                NonZeroUsize::new(1_000).unwrap(),
            ))
            .unwrap();
        let retries = coordinator.retry_refused_lanes().unwrap();
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].owner, owner);
        assert_eq!(
            coordinator.frame(owner).unwrap().unwrap().individual_reads,
            AlignmentIndividualReadCapability::Pending
        );
        assert!(matches!(
            coordinator.pump_once().unwrap().source_event,
            Some(AlignmentSourceActorEvent::ControlApplied)
        ));

        let final_revision = (0..256)
            .find_map(|_| {
                coordinator
                    .pump_once()
                    .unwrap()
                    .publications
                    .into_iter()
                    .find(|publication| publication.owner == owner)
                    .map(|publication| publication.revision)
            })
            .expect("raising the limit should publish the previously refused target");
        let complete = coordinator.frame(owner).unwrap().unwrap();
        assert!(complete.visible_complete());
        assert_eq!(
            complete.individual_reads,
            AlignmentIndividualReadCapability::Complete {
                scene_revision: final_revision,
            }
        );
    }

    #[test]
    fn equal_panel_lanes_share_one_query_authority_at_identical_page_identity() {
        let (mut coordinator, contract, policy, camera) = test_source_coordinator();
        let first = SourceOwnerId::new(101);
        let second = SourceOwnerId::new(202);
        coordinator
            .insert_lane(first, contract.clone(), policy)
            .unwrap();
        coordinator.insert_lane(second, contract, policy).unwrap();
        assert!(
            coordinator
                .update_camera(first, camera)
                .unwrap()
                .foreground_submitted
                .is_some()
        );
        assert!(
            coordinator
                .update_camera(second, camera)
                .unwrap()
                .foreground_submitted
                .is_some()
        );

        let mut publications = BTreeMap::new();
        for _ in 0..256 {
            let pump = coordinator.pump_once().unwrap();
            for publication in pump.publications {
                assert!(
                    publications
                        .insert(publication.owner, publication.revision)
                        .is_none(),
                    "an owner must publish at most once for an unchanged camera"
                );
            }
            if publications.len() == 2 {
                break;
            }
        }
        assert_eq!(publications.len(), 2);
        assert_eq!(coordinator.actor().store().acquisition_count(), 1);
        assert_eq!(coordinator.presentation().owner_count(), 2);
        assert!(
            coordinator
                .frame(first)
                .unwrap()
                .unwrap()
                .visible_complete()
        );
        assert!(
            coordinator
                .frame(second)
                .unwrap()
                .unwrap()
                .visible_complete()
        );
        assert_eq!(
            coordinator.telemetry(first).unwrap().source_work_started
                + coordinator.telemetry(second).unwrap().source_work_started,
            1
        );

        let first_scene = coordinator.frame(first).unwrap().unwrap().snapshot.unwrap();
        let second_scene = coordinator
            .frame(second)
            .unwrap()
            .unwrap()
            .snapshot
            .unwrap();
        assert!(Arc::ptr_eq(
            first_scene.reads().evidence(),
            second_scene.reads().evidence()
        ));
        assert!(Arc::ptr_eq(
            first_scene.reads().atlas(),
            second_scene.reads().atlas()
        ));
        assert_eq!(
            first_scene.reads().pages().len(),
            second_scene.reads().pages().len()
        );
        assert!(
            first_scene
                .reads()
                .pages()
                .iter()
                .zip(second_scene.reads().pages())
                .all(|(left, right)| Arc::ptr_eq(left, right))
        );
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(first)
                .is_none()
        );
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(second)
                .is_none()
        );

        assert!(coordinator.remove_lane(first));
        assert_eq!(coordinator.lane_count(), 1);
        assert_eq!(coordinator.presentation().owner_count(), 1);
        assert!(coordinator.frame(second).unwrap().is_some());
    }

    #[test]
    fn disjoint_panel_foregrounds_complete_serially_without_restart_livelock() {
        let (mut coordinator, contract, policy, first_camera) = test_source_coordinator();
        let first = SourceOwnerId::new(11);
        let second = SourceOwnerId::new(22);
        let second_start = contract.contig_length.saturating_sub(1_000);
        let second_camera = AlignmentCamera::new(
            GenomicInterval::new(contract.contig, second_start, second_start + 2).unwrap(),
            0.0,
            first_camera.row_height(),
            first_camera.read_height(),
            first_camera.clip(),
        )
        .unwrap();
        coordinator
            .insert_lane(first, contract.clone(), policy)
            .unwrap();
        coordinator.insert_lane(second, contract, policy).unwrap();
        coordinator.update_camera(first, first_camera).unwrap();
        coordinator.update_camera(second, second_camera).unwrap();

        let mut published = BTreeMap::new();
        for _ in 0..512 {
            let pump = coordinator.pump_once().unwrap();
            for publication in pump.publications {
                published.insert(publication.owner, publication.revision);
            }
            if published.len() == 2 {
                break;
            }
        }
        assert_eq!(published.len(), 2);
        assert_eq!(coordinator.actor().store().acquisition_count(), 2);
        assert_eq!(
            coordinator.telemetry(first).unwrap().source_work_started
                + coordinator.telemetry(second).unwrap().source_work_started,
            2
        );
        assert!(
            coordinator
                .frame(first)
                .unwrap()
                .unwrap()
                .visible_complete()
        );
        assert!(
            coordinator
                .frame(second)
                .unwrap()
                .unwrap()
                .visible_complete()
        );
    }

    #[test]
    fn post_acquisition_root_pressure_is_owner_scoped_and_preserves_other_lane() {
        let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator_with_ledger(
            AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
        );
        let resident_owner = SourceOwnerId::new(401);
        let pressured_owner = SourceOwnerId::new(402);
        coordinator
            .insert_lane(resident_owner, contract.clone(), policy)
            .unwrap();
        let resident_camera = AlignmentCamera::new(
            GenomicInterval::new(contract.contig, 0, 2).unwrap(),
            0.0,
            dense_camera.row_height(),
            dense_camera.read_height(),
            dense_camera.clip(),
        )
        .unwrap();
        coordinator
            .update_camera(resident_owner, resident_camera)
            .unwrap();
        let resident_revision = (0..256)
            .find_map(|_| {
                coordinator
                    .pump_once()
                    .unwrap()
                    .publications
                    .into_iter()
                    .find(|publication| publication.owner == resident_owner)
                    .map(|publication| publication.revision)
            })
            .expect("the resident owner should publish before pressure is introduced");

        coordinator
            .insert_lane(pressured_owner, contract, policy)
            .unwrap();
        coordinator
            .update_camera(pressured_owner, dense_camera)
            .unwrap();
        let mut observed_chunk = false;
        for _ in 0..256 {
            let pump = coordinator.pump_once().unwrap();
            if matches!(
                pump.source_event,
                Some(AlignmentSourceActorEvent::ChunkAdmitted { .. })
            ) {
                observed_chunk = true;
                break;
            }
        }
        assert!(
            observed_chunk,
            "the dense target should admit a canonical chunk"
        );
        let ledger = coordinator.actor().store().ledger();
        let charged = ledger.snapshot().unwrap().charged;
        ledger.set_budget(charged).unwrap();

        let pressures = (0..256)
            .find_map(|_| {
                let pressures = coordinator.pump_once().unwrap().pressures;
                (!pressures.is_empty()).then_some(pressures)
            })
            .expect("scene-root admission should report owner-local pressure");
        assert_eq!(
            pressures.len(),
            1,
            "a failed candidate must not widen pressure to another lane"
        );
        let pressure = pressures[0];
        assert_eq!(pressure.owner, pressured_owner);
        assert_eq!(pressure.intent, SourceIntentKind::Foreground);
        assert!(matches!(
            pressure.pressure,
            LedgerError::BudgetExceeded { .. }
                | LedgerError::TransientBudgetExceeded { .. }
                | LedgerError::ReplacementTransitionActive { .. }
        ));
        assert_eq!(pressure.target.contig(), dense_camera.interval().contig());
        assert!(pressure.target.contains_interval(dense_camera.interval()));

        let resident = coordinator.frame(resident_owner).unwrap().unwrap();
        assert!(resident.visible_complete());
        assert_eq!(
            resident.snapshot.as_ref().map(|scene| scene.revision()),
            Some(resident_revision),
            "another owner's failed candidate must not replace the resident immutable scene"
        );
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(resident_owner)
                .is_none()
        );
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(pressured_owner)
                .is_none(),
            "the failed owner must not restart the same source target in a busy loop"
        );
        assert!(
            !coordinator
                .frame(pressured_owner)
                .unwrap()
                .unwrap()
                .visible_complete()
        );
    }

    #[test]
    fn post_acquisition_root_pressure_keeps_another_owner_foreground_queued() {
        let (mut coordinator, contract, policy, dense_camera) = test_source_coordinator_with_ledger(
            AllocationLedger::new_with_transient_budget(64 << 20, 0, 64 << 20),
        );
        let pressured_owner = SourceOwnerId::new(501);
        let waiting_owner = SourceOwnerId::new(502);
        let waiting_camera = AlignmentCamera::new(
            GenomicInterval::new(contract.contig, 0, 2).unwrap(),
            0.0,
            dense_camera.row_height(),
            dense_camera.read_height(),
            dense_camera.clip(),
        )
        .unwrap();
        coordinator
            .insert_lane(pressured_owner, contract.clone(), policy)
            .unwrap();
        coordinator
            .insert_lane(waiting_owner, contract, policy)
            .unwrap();
        coordinator
            .update_camera(pressured_owner, dense_camera)
            .unwrap();
        coordinator
            .update_camera(waiting_owner, waiting_camera)
            .unwrap();

        let mut observed_chunk = false;
        for _ in 0..256 {
            let pump = coordinator.pump_once().unwrap();
            if matches!(
                pump.source_event,
                Some(AlignmentSourceActorEvent::ChunkAdmitted { .. })
            ) {
                observed_chunk = true;
                break;
            }
        }
        assert!(
            observed_chunk,
            "the first foreground should admit a canonical chunk"
        );
        let ledger = coordinator.actor().store().ledger();
        let charged = ledger.snapshot().unwrap().charged;
        ledger.set_budget(charged).unwrap();

        let pressures = (0..256)
            .find_map(|_| {
                let pressures = coordinator.pump_once().unwrap().pressures;
                (!pressures.is_empty()).then_some(pressures)
            })
            .expect("the first owner's scene-root admission should encounter pressure");
        assert_eq!(pressures.len(), 1);
        assert_eq!(pressures[0].owner, pressured_owner);
        assert_eq!(pressures[0].intent, SourceIntentKind::Foreground);
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(pressured_owner)
                .is_none()
        );
        assert!(
            coordinator
                .actor()
                .schedule()
                .desired_foreground_for(waiting_owner)
                .is_some(),
            "owner-local pressure must not cancel another lane's queued foreground"
        );
        assert!(coordinator.has_pending_source_work());
    }
}
