use rgv_alignment_scene::{AlignmentPresentationStore, AlignmentSceneCamera, PresentationContract};
use rgv_core::GenomicInterval;

use crate::{AlignmentSourceActor, CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentCoordinatorEviction, AlignmentCoordinatorFrame, AlignmentCoordinatorPressure,
    AlignmentCoordinatorPump, AlignmentCoordinatorUpdate, AlignmentPresentationPump,
    AlignmentReferenceAuthority, AlignmentReferenceInstallOutcome, AlignmentSceneBuildContract,
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorError,
    AlignmentSceneCoordinatorTelemetry, AlignmentScenePreparationPolicy,
    AlignmentSceneSourceCoordinator, AlignmentSceneSourcePump,
};

/// Convenience façade for callers that display one lane from a source. It owns no scheduling,
/// cache, or publication algorithm of its own: every operation delegates to the same source-level
/// coordinator used by multi-panel desktop sessions.
pub struct AlignmentSceneCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    pub(crate) source: AlignmentSceneSourceCoordinator<B>,
    pub(crate) owner: SourceOwnerId,
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
            source_settlements: _,
            refusals,
            pressures,
            failures,
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
        let failure = failures
            .into_iter()
            .find(|failure| failure.owner == self.owner);
        Ok(AlignmentCoordinatorPump {
            source_event,
            published_scene,
            refusal,
            pressure,
            failure,
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

    pub fn set_optional_work_paused(
        &mut self,
        paused: bool,
    ) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        self.source.set_optional_work_paused(self.owner, paused)
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

    pub fn evict_inactive_until(
        &mut self,
        target_charged: usize,
    ) -> Result<AlignmentCoordinatorEviction, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.evict_inactive_until(target_charged)
    }

    /// See [`AlignmentSceneSourceCoordinator::trim_bounded_caches`].
    #[allow(clippy::result_large_err)]
    pub fn trim_bounded_caches(
        &mut self,
        maximum_entries: usize,
    ) -> Result<super::AlignmentBoundedTrim, AlignmentSceneCoordinatorError<B::Error>> {
        self.source.trim_bounded_caches(maximum_entries)
    }
}
