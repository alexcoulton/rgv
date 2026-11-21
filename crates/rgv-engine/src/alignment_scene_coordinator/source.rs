use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

use rgv_alignment_scene::{
    AlignmentPresentationOwnerDemand, AlignmentPresentationStore, AlignmentSceneCamera,
    PresentationContract, SceneRevision,
};
use rgv_core::GenomicInterval;

use crate::{
    AlignmentSourceActor, AlignmentSourceActorEvent, CanonicalAlignmentSourceBackend,
    SourceIntentKind, SourceOwnerId,
};

use super::{
    AlignmentCoordinatorFrame, AlignmentCoordinatorUpdate, AlignmentReferenceAuthority,
    AlignmentReferenceInstallOutcome, AlignmentRowActivityTelemetry, AlignmentSceneBuildContract,
    AlignmentSceneCoordinatorConfigError, AlignmentSceneCoordinatorError,
    AlignmentSceneCoordinatorTelemetry, AlignmentSceneOwnerFailure, AlignmentSceneOwnerPressure,
    AlignmentSceneOwnerPublication, AlignmentSceneOwnerRefusal, AlignmentSceneOwnerRetry,
    AlignmentSceneOwnerWorkState, AlignmentScenePreparationPolicy, AlignmentSceneSourceLane,
    AlignmentSceneSourcePump, CameraDirection, ForegroundWorkingSetSample,
    coordinator_budget_pressure, prepared_interval, presentation_owner,
    scene_build_failure_message, source_horizon, usize_as_u64,
};

/// One mutable source actor and one immutable page repository shared by every panel displaying
/// that source. Owner lanes hold only camera/contract/publication state. This is the production
/// ownership boundary: adding a panel never opens a second BAM/CRAM decoder or creates another
/// source cache authority.
pub struct AlignmentSceneSourceCoordinator<B>
where
    B: CanonicalAlignmentSourceBackend,
{
    pub(crate) actor: AlignmentSourceActor<B>,
    pub(crate) presentation: AlignmentPresentationStore,
    pub(crate) lanes: BTreeMap<SourceOwnerId, AlignmentSceneSourceLane>,
    pub(crate) last_presentation_owner: Option<SourceOwnerId>,
    /// Bounded measured upper envelopes by overlapping working-set island. This preserves the
    /// cost learned at ACTB across a disjoint visit without retaining ACTB's derivative pages.
    pub(crate) foreground_working_set_history: VecDeque<ForegroundWorkingSetSample>,
    pub(crate) last_measured_scene_by_owner: BTreeMap<SourceOwnerId, SceneRevision>,
    /// While the driver is still reclaiming capacity, a pressure-paused foreground row block
    /// waits for that reclaim to finish instead of consuming each quantum's freed bytes.
    pub(crate) capacity_reclaim_active: bool,
    /// Whether lanes defer multi-stage scene builds to `pump_scene_build_once`.
    pub(crate) staged_scene_builds: bool,
    pub(crate) last_scene_build_owner: Option<SourceOwnerId>,
}

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
            foreground_working_set_history: VecDeque::new(),
            last_measured_scene_by_owner: BTreeMap::new(),
            capacity_reclaim_active: false,
            staged_scene_builds: false,
            last_scene_build_owner: None,
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
                speculation_refusal: None,
                foreground_pressure_refusal: None,
                vertical_residency_budget_limited: false,
                vertical_residency_failed: false,
                vertical_read_residency: None,
                optional_work_paused: false,
                row_activity_cache: RefCell::new(None),
                row_activity_telemetry: Cell::new(AlignmentRowActivityTelemetry::default()),
                filter_reference_horizon: None,
                foreground_row_pause: None,
                scene_build_failed: false,
                staged_scene_builds: self.staged_scene_builds,
                scene_build: None,
                contract_epoch: 0,
            },
        );
        Ok(())
    }

    pub fn remove_lane(&mut self, owner: SourceOwnerId) -> bool {
        self.actor.cancel_foreground_for(owner);
        self.actor.cancel_speculation_for(owner);
        self.presentation.remove_owner(presentation_owner(owner));
        self.last_measured_scene_by_owner.remove(&owner);
        let removed = self.lanes.remove(&owner).is_some();
        if removed {
            self.reclassify_presentation_cache(true);
            self.reclassify_exact_summary_cache();
        }
        removed
    }

    pub fn set_optional_work_paused(
        &mut self,
        owner: SourceOwnerId,
        paused: bool,
    ) -> Result<(), AlignmentSceneCoordinatorConfigError> {
        let lane = self
            .lanes
            .get_mut(&owner)
            .ok_or(AlignmentSceneCoordinatorConfigError::UnknownOwner(owner))?;
        if lane.optional_work_paused == paused {
            return Ok(());
        }
        lane.optional_work_paused = paused;
        if paused {
            lane.telemetry.optional_work_pauses =
                lane.telemetry.optional_work_pauses.saturating_add(1);
        } else {
            lane.telemetry.optional_work_resumes =
                lane.telemetry.optional_work_resumes.saturating_add(1);
        }
        Ok(())
    }

    pub fn update_camera(
        &mut self,
        owner: SourceOwnerId,
        camera: impl Into<AlignmentSceneCamera>,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        let camera = camera.into();
        let previous_horizon = self
            .lanes
            .get(&owner)
            .and_then(|lane| lane.desired.as_ref())
            .map(|desired| desired.source_horizon);
        // `update_camera` installs the durable desired camera before it attempts synchronous
        // reconciliation. Keep that desired state even when admission reports pressure: cache
        // ownership must still be reclassified for the new locus before reclaim is scheduled.
        let update_result = {
            let lane = self
                .lanes
                .get_mut(&owner)
                .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
            lane.update_camera(owner, &mut self.actor, &mut self.presentation, camera, true)
        };
        let Some(current_horizon) = self
            .lanes
            .get(&owner)
            .and_then(|lane| lane.desired.as_ref())
            .map(|desired| desired.source_horizon)
        else {
            return update_result;
        };
        let entered_disjoint_island =
            previous_horizon.is_some_and(|previous| !previous.overlaps(current_horizon));
        let working_set_changed = previous_horizon != Some(current_horizon);
        if entered_disjoint_island && update_result.is_err() {
            // A failed disjoint candidate must not leave the worker's obsolete private scene in
            // the presentation store's protected-key set. The desktop's committed epoch keeps
            // its own immutable leases/pins, so detaching this redundant owner is visually safe
            // and lets cache lookup ownership yield during low-water reclaim.
            self.presentation.remove_owner(presentation_owner(owner));
        }
        let active = self.active_source_horizons();
        let demoted = if entered_disjoint_island {
            self.presentation
                .demote_cache_outside(&active)
                .total()
                .saturating_add(self.actor.store_mut().demote_cache_outside(&active))
        } else if working_set_changed {
            // A same-island zoom or pan can substantially contract a formerly retained summary
            // horizon. Keep decoded BAM evidence reusable, but immediately lower the priority of
            // exact derived tiles which no longer intersect any live source buffer.
            self.actor
                .store_mut()
                .demote_exact_summary_cache_outside(&active)
        } else {
            0
        };
        if demoted > 0 {
            if let Some(lane) = self.lanes.get_mut(&owner) {
                lane.telemetry.return_cache_demotions =
                    lane.telemetry.return_cache_demotions.saturating_add(1);
                lane.telemetry.return_cache_entries_demoted = lane
                    .telemetry
                    .return_cache_entries_demoted
                    .saturating_add(usize_as_u64(demoted));
            }
        }
        let published_scene = update_result
            .as_ref()
            .ok()
            .and_then(|update| update.published_scene);
        if entered_disjoint_island || published_scene.is_some() {
            self.reclassify_presentation_cache(entered_disjoint_island);
        }
        if published_scene.is_some() {
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        let capacity_plan = entered_disjoint_island
            .then(|| self.foreground_capacity_plan(owner, 0))
            .transpose()?;
        // Under `Bounded` every owner trims itself after each worker turn, so no low-water
        // mark has to be established ahead of a disjoint transition.
        let bounded = self.actor.store().ledger().is_bounded();
        update_result.map(|mut update| {
            update.capacity_plan = capacity_plan;
            update.proactive_reclaim_target =
                capacity_plan.filter(|_| !bounded).and_then(|capacity| {
                    (capacity.charged > capacity.low_water_target)
                        .then_some(capacity.low_water_target)
                });
            update
        })
    }

    pub(crate) fn active_source_horizons(&self) -> Vec<GenomicInterval> {
        self.lanes
            .values()
            .filter_map(|lane| lane.desired.as_ref().map(|desired| desired.source_horizon))
            .collect()
    }

    pub(crate) fn reclassify_presentation_cache(&mut self, reclassify_unowned: bool) {
        let active = self.active_source_horizons();
        let demands = self
            .lanes
            .iter()
            .filter_map(|(owner, lane)| {
                let desired = lane.desired.as_ref()?;
                Some(AlignmentPresentationOwnerDemand {
                    owner: presentation_owner(*owner),
                    visible_interval: desired.camera.interval(),
                    visible_rows: desired.camera.visible_rows(),
                })
            })
            .collect::<Vec<_>>();
        self.presentation
            .reclassify_for_demands(&active, &demands, reclassify_unowned);
    }

    pub(crate) fn reclassify_exact_summary_cache(&mut self) {
        let active = self.active_source_horizons();
        let prepared = self
            .lanes
            .keys()
            .filter_map(|owner| self.presentation.prepared_for(presentation_owner(*owner)))
            .map(|scene| Arc::clone(scene.summaries()))
            .collect::<Vec<_>>();
        self.actor
            .store_mut()
            .reclassify_exact_summary_cache(&active, &prepared);
    }

    pub fn set_preparation_policy(
        &mut self,
        owner: SourceOwnerId,
        policy: AlignmentScenePreparationPolicy,
    ) -> Result<Option<AlignmentCoordinatorUpdate>, AlignmentSceneCoordinatorError<B::Error>> {
        let lane = self
            .lanes
            .get(&owner)
            .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
        policy.validate()?;
        let Some(camera) = lane.desired.as_ref().map(|desired| desired.camera) else {
            self.lanes
                .get_mut(&owner)
                .expect("the validated owner remains installed")
                .policy = policy;
            return Ok(None);
        };
        let replacement_prepared =
            prepared_interval(camera, CameraDirection::Stationary, policy, &lane.contract)?;
        let replacement_horizon = source_horizon(replacement_prepared, &lane.contract)?;
        let mut active_horizons = self
            .lanes
            .iter()
            .filter(|(candidate, _)| **candidate != owner)
            .filter_map(|(_, lane)| lane.desired.as_ref().map(|desired| desired.source_horizon))
            .collect::<Vec<_>>();
        active_horizons.push(replacement_horizon);
        let demoted = self
            .actor
            .store_mut()
            .demote_exact_summary_cache_outside(&active_horizons);
        let result = {
            let lane = self
                .lanes
                .get_mut(&owner)
                .expect("the validated owner remains installed");
            lane.policy = policy;
            lane.vertical_residency_budget_limited = false;
            lane.vertical_residency_failed = false;
            lane.previous_exact_start = None;
            if demoted > 0 {
                lane.telemetry.return_cache_demotions =
                    lane.telemetry.return_cache_demotions.saturating_add(1);
                lane.telemetry.return_cache_entries_demoted = lane
                    .telemetry
                    .return_cache_entries_demoted
                    .saturating_add(usize_as_u64(demoted));
            }
            lane.update_camera(
                owner,
                &mut self.actor,
                &mut self.presentation,
                camera,
                false,
            )
        };
        if result
            .as_ref()
            .is_ok_and(|update| update.published_scene.is_some())
        {
            self.reclassify_presentation_cache(false);
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        result.map(Some)
    }

    pub fn replace_contract(
        &mut self,
        owner: SourceOwnerId,
        contract: AlignmentSceneBuildContract,
    ) -> Result<AlignmentCoordinatorUpdate, AlignmentSceneCoordinatorError<B::Error>> {
        let other_active_horizons = self
            .lanes
            .iter()
            .filter(|(candidate, _)| **candidate != owner)
            .filter_map(|(_, lane)| lane.desired.as_ref().map(|desired| desired.source_horizon))
            .collect::<Vec<_>>();
        let result = {
            let lane = self
                .lanes
                .get_mut(&owner)
                .ok_or(AlignmentSceneCoordinatorError::UnknownOwner(owner))?;
            lane.replace_contract(
                owner,
                &mut self.actor,
                &mut self.presentation,
                &other_active_horizons,
                contract,
            )
        };
        if result
            .as_ref()
            .is_ok_and(|update| update.published_scene.is_some())
        {
            self.reclassify_presentation_cache(false);
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        result
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
        let mut source_settlements = Vec::new();
        let mut refusals = Vec::new();
        let mut pressures = Vec::new();
        let mut failures = Vec::new();
        let mut foreground_submitted = None;
        let mut speculation_submitted = None;
        if let (Some(event), Some(owner)) = (&event, event_owner)
            && let Some(lane) = self.lanes.get_mut(&owner)
        {
            lane.observe_source_event(event);
        }
        if let Some(AlignmentSourceActorEvent::AcquisitionRefused {
            work,
            complete_read_limit,
            eligible_records,
            publish: Some(_),
            ..
        }) = &event
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            let target = work.intent().spec().admission_target();
            if lane.desired.as_ref().is_some_and(|desired| {
                work.kind() == SourceIntentKind::Foreground && desired.camera.interval() == target
            }) {
                let refusal = AlignmentSceneOwnerRefusal {
                    owner: work.intent().owner(),
                    target,
                    complete_read_limit: *complete_read_limit,
                    eligible_records: u64::try_from(*eligible_records).unwrap_or(u64::MAX),
                };
                lane.refusal = Some(refusal);
                refusals.push(refusal);
            } else if work.kind() == SourceIntentKind::Foreground
                && !lane.scene_build_failed
                && !lane.prepared_covers_desired(work.intent().owner(), &self.presentation)
            {
                // The decoder counted the camera this stream started with, and the camera has
                // moved since (a covering stream is no longer restarted per sample). That count
                // says nothing about the live camera, so admission is asked again for it: once
                // per finished stream, never once per camera sample.
                foreground_submitted =
                    lane.ensure_foreground(work.intent().owner(), &mut self.actor)?;
            }
        }
        if let Some(AlignmentSourceActorEvent::AcquisitionRefused { work, .. }) = &event
            && work.kind() == SourceIntentKind::Speculative
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
            && lane.desired.as_ref().is_some_and(|desired| {
                desired
                    .source_horizon
                    .contains_interval(work.intent().spec().target())
            })
        {
            lane.speculation_refusal = Some(work.intent().spec().target());
        }
        if let Some(AlignmentSourceActorEvent::AcquisitionCompleted { work, .. }) = &event
            && work.kind() == SourceIntentKind::Speculative
            && let Some(lane) = self.lanes.get_mut(&work.intent().owner())
        {
            lane.speculation_refusal = None;
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
                if lane.scene_build_failed {
                    // Already reported as terminal for this lane's authority. Another owner's
                    // completion must not rebuild and re-report the same failing candidate.
                    continue;
                }
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
                        Ok(None) => {
                            // Completing canonical evidence can change a frame even when the
                            // semantic scene cannot publish yet. Reference-backed filters are the
                            // important case: exact eligible read spans can expand the required
                            // FASTA horizon at this point. Preserve that terminal edge for the
                            // threaded engine instead of letting it go idle with the UI still
                            // assigning liveness to the source worker. A staged scene build
                            // registered by this reconciliation is not settled yet: its own
                            // final stage reports the publication, settlement, or failure.
                            if lane.scene_build.is_none() {
                                source_settlements.push(owner);
                            }
                        }
                        Err(error) => {
                            // Candidate construction is lane-local even though its immutable roots
                            // share one store. Cancel only this owner's redundant source intents;
                            // other owners and every already-published scene remain authoritative.
                            self.actor.cancel_foreground_for(owner);
                            self.actor.cancel_speculation_for(owner);
                            let pressure_target = coordinator_budget_pressure(&error)
                                .zip(lane.desired.as_ref().map(|desired| desired.source_horizon));
                            let Some((pressure, target)) = pressure_target else {
                                let Some(message) = scene_build_failure_message(&error) else {
                                    return Err(error);
                                };
                                // Not a capacity refusal: terminal for this owner's authority,
                                // reported once, and invisible to every other owner.
                                lane.scene_build_failed = true;
                                failures.push(AlignmentSceneOwnerFailure { owner, message });
                                continue;
                            };
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
                if lane.prepared_covers_desired(owner, &self.presentation) {
                    self.actor.cancel_foreground_for(owner);
                    self.actor.cancel_speculation_for(owner);
                }
            }
            if let Some(owner) = event_owner
                && let Some(lane) = self.lanes.get_mut(&owner)
                && !pressures.iter().any(|pressure| pressure.owner == owner)
                && !lane.scene_build_failed
                && !lane.prepared_covers_desired(owner, &self.presentation)
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
        if !publications.is_empty() {
            self.reclassify_presentation_cache(false);
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        Ok(AlignmentSceneSourcePump {
            source_event: event,
            publications,
            source_settlements,
            refusals,
            pressures,
            failures,
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
            .map(|lane| {
                let mut telemetry = lane.telemetry;
                let row_activity = lane.row_activity_telemetry.get();
                telemetry.row_activity_queries = row_activity.queries;
                telemetry.row_activity_cache_hits = row_activity.cache_hits;
                telemetry.row_activity_cursor_updates = row_activity.cursor_updates;
                telemetry.row_activity_rebuilds = row_activity.rebuilds;
                telemetry.row_activity_intervals_examined = row_activity.intervals_examined;
                telemetry.row_activity_intervals_changed = row_activity.intervals_changed;
                telemetry
            })
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
            // A pressure-paused block is still owned work (it resumes on capacity progress or an
            // explicit retry); a terminally failed one is not.
            presentation_pending: (lane
                .needs_foreground_row_presentation(owner, &self.presentation)
                && !lane.foreground_row_presentation_failed(owner, &self.presentation))
                || lane.needs_vertical_residency(owner, &self.presentation)
                // A staged scene build is worker-owned presentation work for this owner.
                || lane.scene_build.is_some(),
        })
    }

    /// Whether user-visible source work (a control, an unserved foreground intent, or an active
    /// foreground transaction) can run, as opposed to optional read-ahead only.
    #[must_use]
    pub fn has_pending_foreground_source_work(&self) -> bool {
        self.actor.schedule().foreground_work_pending()
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
        // The same explicit signal resumes a pressure-paused visible row block and lets a failed
        // scene candidate be attempted once more.
        lane.foreground_row_pause = None;
        lane.scene_build_failed = false;
        lane.foreground_pressure_refusal = None;

        let camera_sequence = desired.camera_sequence;
        let before = self
            .presentation
            .prepared_for(presentation_owner(owner))
            .map(|scene| scene.revision());
        let published = match lane.reconcile_from_resident_or_cache(
            owner,
            &mut self.actor,
            &mut self.presentation,
        ) {
            Ok(published) => published,
            Err(error) => {
                // The caller reports this outcome for the retry itself. Stay paused so the
                // worker does not immediately rebuild the same candidate and report it twice.
                if coordinator_budget_pressure(&error).is_some() {
                    lane.pause_foreground_rows_for_pressure(owner, &self.presentation);
                } else {
                    lane.scene_build_failed = true;
                }
                return Err(error);
            }
        };
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

        if lane.prepared_covers_desired(owner, &self.presentation) {
            self.actor.cancel_foreground_for(owner);
            self.actor.cancel_speculation_for(owner);
        } else if update.visible_complete {
            self.actor.cancel_foreground_for(owner);
            update.speculation_submitted = lane.ensure_speculation(owner, &mut self.actor)?;
        } else {
            self.actor.cancel_speculation_for(owner);
            update.foreground_submitted = lane.ensure_foreground(owner, &mut self.actor)?;
        }
        if published.is_some() {
            self.reclassify_presentation_cache(false);
            self.reclassify_exact_summary_cache();
            self.observe_prepared_foreground_working_sets()?;
        }
        update.capacity_plan = Some(self.foreground_capacity_plan(owner, 0)?);
        Ok(Some(update))
    }
}

pub(crate) fn source_event_owner(event: &AlignmentSourceActorEvent) -> Option<SourceOwnerId> {
    match event {
        AlignmentSourceActorEvent::WorkStarted(work)
        | AlignmentSourceActorEvent::SourceProgressed { work, .. }
        | AlignmentSourceActorEvent::WorkYielded { work, .. }
        | AlignmentSourceActorEvent::AcquisitionCompleted { work, .. }
        | AlignmentSourceActorEvent::AcquisitionRefused { work, .. } => Some(work.intent().owner()),
        AlignmentSourceActorEvent::WorkRetargeted { current, .. } => Some(current.intent().owner()),
        AlignmentSourceActorEvent::ControlApplied
        | AlignmentSourceActorEvent::SourceWaiting { .. }
        | AlignmentSourceActorEvent::ChunkAdmitted { .. } => None,
    }
}
