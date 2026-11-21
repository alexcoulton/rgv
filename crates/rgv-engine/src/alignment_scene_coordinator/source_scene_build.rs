//! Staged scene builds: the source coordinator's side of `lane::scene_build`.

use crate::{CanonicalAlignmentSourceBackend, SourceIntentKind};

use super::{
    AlignmentSceneCoordinatorError, AlignmentSceneOwnerFailure, AlignmentSceneOwnerPressure,
    AlignmentSceneOwnerPublication, AlignmentSceneSourceCoordinator, AlignmentSceneSourcePump,
    SceneBuildProgress, coordinator_budget_pressure, scene_build_failure_message,
};

impl<B> AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    /// Chooses how a lane derives a scene from cached evidence. Off (the default), the whole
    /// build runs inside the call which discovers it is needed. On, that call only registers the
    /// build and [`Self::pump_scene_build_once`] runs it one stage at a time, so the driver can
    /// service a newer camera, a lane removal or a budget control between stages.
    pub fn set_staged_scene_builds(&mut self, staged: bool) {
        self.staged_scene_builds = staged;
        for lane in self.lanes.values_mut() {
            lane.staged_scene_builds = staged;
        }
    }

    /// Whether any lane has a staged scene build in flight.
    #[must_use]
    pub fn has_pending_scene_build(&self) -> bool {
        self.lanes.values().any(|lane| lane.scene_build.is_some())
    }

    /// Runs one stage of one lane's in-flight scene build, or abandons the build when the lane
    /// no longer wants it. Abandoning drops every candidate the build holds, which returns its
    /// pins and reservations to the shared ledger.
    ///
    /// The result has the shape of a source pump because it settles the same way: a publication,
    /// an owner-scoped pressure or failure, or a settlement which only changes the frame.
    pub fn pump_scene_build_once(
        &mut self,
    ) -> Result<Option<AlignmentSceneSourcePump>, AlignmentSceneCoordinatorError<B::Error>> {
        let eligible = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| lane.scene_build.is_some().then_some(*owner))
            .collect::<Vec<_>>();
        let Some(first) = eligible.first().copied() else {
            return Ok(None);
        };
        let owner = self.last_scene_build_owner.map_or(first, |last| {
            eligible
                .iter()
                .copied()
                .find(|owner| *owner > last)
                .unwrap_or(first)
        });
        self.last_scene_build_owner = Some(owner);
        let mut pump = empty_source_pump();
        let lane = self
            .lanes
            .get_mut(&owner)
            .expect("an owner with a scene build remains installed");
        if lane.discard_superseded_scene_build(owner, &self.presentation) {
            // The frame's work state changed even though nothing was published.
            pump.source_settlements.push(owner);
            return Ok(Some(pump));
        }
        let build = lane
            .scene_build
            .take()
            .expect("the selected owner has a scene build");
        // Whether a failure of this build leaves visible work unserved.
        let intent = if lane
            .frame(owner, &self.presentation)
            .is_none_or(|frame| !frame.visible_complete())
        {
            SourceIntentKind::Foreground
        } else {
            SourceIntentKind::Speculative
        };
        let mut outcome =
            lane.advance_scene_build(owner, &mut self.actor, &mut self.presentation, build);
        let mut published = None;
        if let Ok(SceneBuildProgress::Published(revision)) = &outcome {
            published = Some(*revision);
            if !lane.prepared_covers_desired(owner, &self.presentation) {
                // The camera moved on inside the interval this build covered. Converge on the
                // newest target now; nothing else would ask for it once the camera is at rest.
                match lane.reconcile_from_resident_or_cache(
                    owner,
                    &mut self.actor,
                    &mut self.presentation,
                ) {
                    Ok(Some(newer)) => published = Some(newer),
                    Ok(None) => {}
                    Err(error) => outcome = Err(error),
                }
            }
        }
        if let Some(revision) = published {
            pump.publications
                .push(AlignmentSceneOwnerPublication { owner, revision });
        }
        match outcome {
            Ok(SceneBuildProgress::Continue(build) | SceneBuildProgress::RootsPublished(build)) => {
                lane.scene_build = Some(build);
            }
            Ok(SceneBuildProgress::AwaitingReference) => pump.source_settlements.push(owner),
            Ok(SceneBuildProgress::Published(_)) => {
                if lane.prepared_covers_desired(owner, &self.presentation) {
                    self.actor.cancel_foreground_for(owner);
                    self.actor.cancel_speculation_for(owner);
                } else if lane
                    .frame(owner, &self.presentation)
                    .is_some_and(|frame| frame.visible_complete())
                {
                    pump.speculation_submitted = lane.ensure_speculation(owner, &mut self.actor)?;
                } else {
                    pump.foreground_submitted = lane.ensure_foreground(owner, &mut self.actor)?;
                }
            }
            Err(error) => {
                // The failed build was dropped with everything it had reserved. As for a
                // candidate which fails after a source completion, this is lane-local: only this
                // owner's redundant source intents are cancelled.
                self.actor.cancel_foreground_for(owner);
                self.actor.cancel_speculation_for(owner);
                let pressure_target = coordinator_budget_pressure(&error)
                    .zip(lane.desired.as_ref().map(|desired| desired.source_horizon));
                if let Some((pressure, target)) = pressure_target {
                    pump.pressures.push(AlignmentSceneOwnerPressure {
                        owner,
                        target,
                        intent,
                        pressure,
                    });
                } else {
                    let Some(message) = scene_build_failure_message(&error) else {
                        return Err(error);
                    };
                    // Terminal for this owner's current authority and reported once: no worker
                    // turn rebuilds it until a camera, contract, reference or retry asks again.
                    lane.scene_build_failed = true;
                    pump.failures
                        .push(AlignmentSceneOwnerFailure { owner, message });
                }
            }
        }
        if !pump.publications.is_empty() {
            self.reclassify_presentation_cache(false);
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        Ok(Some(pump))
    }

    /// The stage the owner's in-flight build will run next.
    #[cfg(test)]
    pub(crate) fn scene_build_stage(&self, owner: crate::SourceOwnerId) -> Option<&'static str> {
        self.lanes
            .get(&owner)?
            .scene_build
            .as_ref()
            .map(super::SceneBuild::stage_name)
    }
}

fn empty_source_pump() -> AlignmentSceneSourcePump {
    AlignmentSceneSourcePump {
        source_event: None,
        publications: Vec::new(),
        source_settlements: Vec::new(),
        refusals: Vec::new(),
        pressures: Vec::new(),
        failures: Vec::new(),
        foreground_submitted: None,
        speculation_submitted: None,
    }
}
