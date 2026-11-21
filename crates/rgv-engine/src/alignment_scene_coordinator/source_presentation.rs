use std::sync::Arc;

use crate::{CanonicalAlignmentSourceBackend, SourceOwnerId};

use super::{
    AlignmentPresentationPump, AlignmentSceneCoordinatorError, AlignmentSceneSourceCoordinator,
    ForegroundRowPresentationPauseCause,
};

impl<B> AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    /// Tells the coordinator whether its driver still has a capacity reclaim in flight. While
    /// one is, a pressure-paused foreground row block is not resumed by ledger progress: letting
    /// it rebuild between reclaim quanta would consume the bytes each quantum just released.
    pub const fn set_capacity_reclaim_active(&mut self, active: bool) {
        self.capacity_reclaim_active = active;
    }

    /// The message of a terminal (non-pressure) foreground row-block failure that still applies
    /// to `owner`'s current authority.
    #[must_use]
    pub fn foreground_presentation_failure(&self, owner: SourceOwnerId) -> Option<Arc<str>> {
        let lane = self.lanes.get(&owner)?;
        match &lane.foreground_row_pause(owner, &self.presentation)?.cause {
            ForegroundRowPresentationPauseCause::Failed(message) => Some(Arc::clone(message)),
            ForegroundRowPresentationPauseCause::Pressure(_) => None,
        }
    }

    /// Whether a visible-complete lane has optional vertical presentation work. This never makes
    /// an incomplete source look ready: a lane becomes eligible only after its immutable current
    /// scene truthfully covers the live camera.
    #[must_use]
    pub fn has_pending_presentation_work(&self) -> bool {
        self.lanes.iter().any(|(owner, lane)| {
            lane.foreground_row_presentation_runnable(
                *owner,
                &self.presentation,
                !self.capacity_reclaim_active,
            ) || lane.needs_vertical_residency(*owner, &self.presentation)
        })
    }

    /// Whether an exact visible row block is missing and can be built now. This is separate from
    /// optional whole-stack warming so the worker can yield bounded cache maintenance to
    /// user-visible work without allowing speculation to delay reclaim. A block paused by
    /// allocation pressure or a terminal failure is not runnable and is not reported here.
    #[must_use]
    pub fn has_pending_foreground_presentation_work(&self) -> bool {
        self.lanes.iter().any(|(owner, lane)| {
            lane.foreground_row_presentation_runnable(
                *owner,
                &self.presentation,
                !self.capacity_reclaim_active,
            )
        })
    }

    /// Builds and atomically publishes at most one additional canonical row block. A normal
    /// ledger refusal pauses this optional residency pass and leaves the existing visible scene
    /// untouched; it is not promoted to a source/read failure.
    pub fn pump_presentation_once(
        &mut self,
    ) -> Result<Option<AlignmentPresentationPump>, AlignmentSceneCoordinatorError<B::Error>> {
        let foreground = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| {
                lane.foreground_row_presentation_runnable(
                    *owner,
                    &self.presentation,
                    !self.capacity_reclaim_active,
                )
                .then_some(*owner)
            })
            .collect::<Vec<_>>();
        if !foreground.is_empty() {
            let owner = self.next_presentation_owner(&foreground);
            let outcome = {
                let lane = self
                    .lanes
                    .get_mut(&owner)
                    .expect("an eligible foreground presentation owner remains installed");
                lane.prepare_foreground_row_presentation_once(owner, &mut self.presentation)?
            };
            if outcome.published_scene.is_some() {
                self.reclassify_presentation_cache(false);
                self.reclassify_exact_summary_cache();
                self.observe_prepared_foreground_working_sets()?;
            }
            return Ok(Some(outcome));
        }

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
        let owner = self.next_presentation_owner(&eligible);
        let lane = self
            .lanes
            .get_mut(&owner)
            .expect("an eligible presentation owner remains installed");
        match lane.expand_vertical_residency_once(owner, &mut self.presentation) {
            Ok(outcome) => Ok(Some(outcome)),
            Err(error) => {
                // A checkpoint that could not even be planned would be selected again on the
                // next worker turn. Optional warming stops for this authority instead.
                lane.vertical_residency_failed = true;
                Err(error)
            }
        }
    }

    pub(crate) fn next_presentation_owner(&mut self, eligible: &[SourceOwnerId]) -> SourceOwnerId {
        let owner = self.last_presentation_owner.map_or(eligible[0], |last| {
            eligible
                .iter()
                .copied()
                .find(|owner| *owner > last)
                .unwrap_or(eligible[0])
        });
        self.last_presentation_owner = Some(owner);
        owner
    }
}
