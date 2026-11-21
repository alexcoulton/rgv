use super::alignment_management::alignment_scene_frame_for_ui;
use crate::automation::AutomationAuxiliaryOpenState;
use crate::diagnostics::HealthSignal;
use crate::sources::load_assembly_reporting;
use crate::workspace::alignment_navigation::{
    AlignmentNavigationPhase, AlignmentNavigationProductState,
};
use crate::workspace::alignments::{
    AlignmentReferenceCompatibility, AlignmentSceneBudgetRetry, AlignmentSceneBudgetRetryPoll,
    AlignmentScenePressureRetry, AlignmentScenePressureRetryPoll, AlignmentSceneResolution,
    AlignmentTrackLifecycle, alignment_reference_compatibility,
    alignment_scene_pressure_after_relief,
};
use crate::workspace::context_lanes::buffered_context_interval;
use crate::workspace::navigation::RequestedLocus;
use crate::workspace::navigation_transaction::{
    NavigationPresentationPhase, NavigationQueryTarget, NavigationTargetFingerprint,
    navigation_target_ready_for_commit,
};
use crate::{
    AlignmentGpuPublicationRetryGate, Viewer, auxiliary_query_track_ids,
    clear_removed_auxiliary_panel_state,
};
use rgv_core::{
    MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId, ReferenceDictionary, TrackKind, TrackSource,
    Viewport,
};
use rgv_engine::{
    AlignmentIndividualReadCapability, AlignmentSceneEngineEvent, AlignmentSceneFrameCause,
    AlignmentSceneResourcePressureDisposition, AlignmentVerticalResidency, EngineQueueError,
    MAX_REFERENCE_QUERY_BASES, ReferenceEngineEvent, SourceOwnerId, TrackEngineEvent,
};
use rgv_session::track_events::TrackEventRoute;
use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) use rgv_session::alignment::settlement::*;

impl Viewer {
    fn set_panel_alignment_optional_work_paused(&mut self, panel_id: PanelId, paused: bool) {
        for track_index in 0..self.session.alignment_tracks.len() {
            let Some(owner) = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| {
                    (projection.scene_optional_work_paused != paused)
                        .then_some(projection.scene_owner)
                        .flatten()
                })
            else {
                continue;
            };
            let result = self.session.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| engine.try_set_optional_work_paused(owner, paused));
            match result {
                Some(Ok(())) => {
                    self.session.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_optional_work_paused = paused;
                }
                Some(Err(EngineQueueError::Disconnected)) => {
                    self.session.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
                Some(Err(EngineQueueError::Full)) | None => {}
            }
        }
    }

    pub(crate) fn resume_alignment_optional_work_for_settled_panels(&mut self) {
        let settled = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .filter(|panel_id| self.panel_alignment_queries_are_settled(*panel_id))
            .collect::<Vec<_>>();
        for panel_id in settled {
            self.set_panel_alignment_optional_work_paused(panel_id, false);
        }
    }

    fn mark_panel_navigation_queries_pending(&mut self, panel_id: PanelId) {
        for track_index in 0..self.session.alignment_tracks.len() {
            self.queue_alignment_navigation_for_track(
                track_index,
                panel_id,
                false,
                "viewport navigation requested",
            );
        }
        let alignment_pending = self.session.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
        });
        let base_marks_visible = self
            .session
            .panels
            .panel(panel_id)
            .viewport
            .bases_per_pixel()
            <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        let reference_pending = self.session.reference_engine.is_some() && base_marks_visible;
        let tracks_pending = self.session.track_engine.is_some();
        let panel = self.session.panels.panel_mut(panel_id);
        panel.pending_queries.alignment = alignment_pending;
        panel.pending_queries.reference = reference_pending;
        panel.pending_queries.tracks = tracks_pending;
        panel.reference_request_revision = None;
        panel.reference_request_interval = None;
        panel.reference_installs_queued.clear();
        panel.track_request_interval = None;
        panel.reference_query_error_generation = None;
        panel.track_query_error_generations.clear();
    }

    pub(crate) fn begin_viewport_navigation(
        &mut self,
        panel_id: PanelId,
        gesture_active: bool,
        hold_commit_until_end: bool,
    ) {
        if self
            .session
            .panels
            .panel(panel_id)
            .navigation_presentation
            .targets_viewport(self.session.panels.panel(panel_id).viewport)
        {
            return;
        }
        self.set_panel_alignment_optional_work_paused(panel_id, true);
        self.revoke_alignment_gpu_optional_residency_for_foreground();
        if self.session.synthetic
            || !(self.has_alignment_engine()
                || self.session.reference_engine.is_some()
                || self.session.track_engine.is_some())
        {
            self.rebuild_genomic_display(panel_id);
            self.mark_display_dirty("explicit display invalidation");
            return;
        }

        let now = Instant::now();
        self.retain_active_read_selection_for_query_refresh();
        let previous_target_generation = self
            .session
            .panels
            .panel(panel_id)
            .navigation_presentation
            .target_generation;
        let previous_phase = self
            .session
            .panels
            .panel(panel_id)
            .navigation_presentation
            .phase;
        self.session
            .panels
            .panel(panel_id)
            .generation_clock
            .advance();
        self.advance_alignment_generations(panel_id);
        self.mark_panel_navigation_queries_pending(panel_id);
        let generation = self
            .session
            .panels
            .panel(panel_id)
            .generation_clock
            .current()
            .get();
        let viewport = self.session.panels.panel(panel_id).viewport;
        let target = NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(viewport),
            generation,
        };
        let source_status = self.session.data_status.clone();
        let source_activity = self.status_activity();
        let panel = self.session.panels.panel_mut(panel_id);
        if previous_phase == NavigationPresentationPhase::Acquiring
            && previous_target_generation != generation
        {
            panel.navigation_query_schedule.record_superseded_target();
        }
        panel
            .navigation_presentation
            .begin_navigation_with_activity(
                viewport,
                generation,
                self.workspace_layout.revision,
                self.session.navigation_contract_revision,
                &source_status,
                source_activity,
                now,
            );
        panel
            .navigation_query_schedule
            .update(target, now, gesture_active, hold_commit_until_end);
        // Context lanes retain immutable buffered geometry and move immediately under the desired
        // camera. Acquisition remains independently scheduled below and replaces only the lane
        // whose new horizon becomes ready.
        self.reproject_panel_context_lanes(panel_id);
        if gesture_active {
            // Alignment cameras are not debounced with source queries. The bounded scene mailbox
            // replaces unread camera intents per panel, while the UI immediately reprojects the
            // resident immutable scene under this exact viewport.
            self.dispatch_alignment_query(panel_id);
        } else {
            let target = self
                .session
                .panels
                .panel_mut(panel_id)
                .navigation_query_schedule
                .take_immediate(now, false);
            if let Some(target) = target {
                self.dispatch_navigation_query_target(panel_id, target);
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn finish_navigation_gesture(&mut self, panel_id: PanelId) {
        let now = Instant::now();
        let target = self
            .session
            .panels
            .panel_mut(panel_id)
            .navigation_query_schedule
            .take_immediate(now, true);
        if let Some(target) = target {
            self.dispatch_navigation_query_target(panel_id, target);
        }
        self.resume_alignment_optional_work_for_settled_panels();
        self.window.request_redraw();
    }

    fn dispatch_due_navigation_queries(&mut self, now: Instant) {
        let due = self
            .session
            .panels
            .iter_mut()
            .filter_map(|panel| {
                panel
                    .navigation_query_schedule
                    .take_due(now)
                    .map(|target| (panel.id, target))
            })
            .collect::<Vec<_>>();
        for (panel_id, target) in due {
            self.dispatch_navigation_query_target(panel_id, target);
        }
    }

    fn dispatch_navigation_query_target(
        &mut self,
        panel_id: PanelId,
        target: NavigationQueryTarget,
    ) {
        let Some(panel) = self.session.panels.get(panel_id) else {
            return;
        };
        if panel.generation_clock.current().get() != target.generation
            || NavigationTargetFingerprint::from_viewport(panel.viewport) != target.fingerprint
        {
            return;
        }
        self.dispatch_alignment_query(panel_id);
        self.dispatch_reference_query(panel_id);
        self.dispatch_track_query(panel_id);
    }

    pub(crate) fn panel_navigation_data_is_settled(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.session.panels.get(panel_id) else {
            return false;
        };
        if panel.navigation_query_schedule.pending.is_some() {
            return false;
        }
        // Alignment pixels own their own atomic scene epoch. Reference interpretation and generic
        // contextual tracks publish independently and cannot silently hold a complete alignment
        // target behind the panel navigation transaction.
        if !self.session.alignment_tracks.is_empty() {
            return self.panel_alignment_queries_are_settled(panel_id);
        }
        let panel_generation = panel.generation_clock.current().get();
        let reference_settled = reference_dependency_is_settled(
            self.session.reference_engine.is_some()
                && panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
            panel_generation,
            self.session.reference_revision,
            panel.reference_slice_revision,
            panel.reference_slice.as_ref().map(|slice| slice.interval),
            panel.viewport.interval(),
            panel.reference_settled_generation,
            panel.reference_query_error_generation,
        );
        let tracks_settled = self.session.track_engine.is_none()
            || panel.track_settled_generation == Some(panel_generation);
        reference_settled && tracks_settled
    }

    fn panel_alignment_queries_are_settled(&self, panel_id: PanelId) -> bool {
        self.session
            .alignment_tracks
            .iter()
            .all(|track| match track.lifecycle {
                AlignmentTrackLifecycle::Opening => false,
                AlignmentTrackLifecycle::Ready => {
                    track.projection(panel_id).is_some_and(|projection| {
                        projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
                }
                AlignmentTrackLifecycle::Failed(_) => true,
            })
    }

    fn update_navigation_readiness(&mut self) {
        let candidate_ids = self
            .session
            .panels
            .iter()
            .filter(|panel| {
                panel.navigation_presentation.phase == NavigationPresentationPhase::Acquiring
            })
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let mut ready = Vec::new();
        for panel_id in candidate_ids {
            let data_settled = self.panel_navigation_data_is_settled(panel_id);
            let panel = self
                .session
                .panels
                .get(panel_id)
                .expect("navigation candidate belongs to the panel workspace");
            if navigation_target_ready_for_commit(
                panel.navigation_presentation.phase,
                panel.navigation_query_schedule.hold_commit_until_end,
                data_settled,
                true,
            ) {
                ready.push(panel_id);
            }
        }
        if ready.is_empty() {
            return;
        }
        for panel_id in ready {
            if let Some(panel) = self.session.panels.get_mut(panel_id) {
                panel.navigation_presentation.mark_ready_to_commit();
            }
        }
        self.mark_display_dirty("explicit display invalidation");
        self.window.request_redraw();
    }

    pub(crate) fn begin_ready_navigation_commits(&mut self) -> bool {
        let mut changed = false;
        for panel in self.session.panels.iter_mut() {
            changed |= panel.navigation_presentation.begin_commit();
        }
        changed
    }

    pub(crate) fn finish_navigation_commits(&mut self) -> bool {
        let now = Instant::now();
        let mut committed = false;
        for panel in self.session.panels.iter_mut() {
            let elapsed = now.saturating_duration_since(panel.navigation_presentation.started_at);
            if panel.navigation_presentation.finish_commit(
                self.workspace_layout.revision,
                self.session.navigation_contract_revision,
            ) {
                committed = true;
                panel.navigation_query_schedule.record_commit(elapsed);
            }
        }
        committed
    }

    pub(crate) fn invalidate_navigation_contract(&mut self, reason: &str) {
        self.session.invalidate_navigation_contract(reason);
    }

    /// Starts a same-locus source refresh after a query/presentation contract change. Each live
    /// lane retains only scene pixels that remain valid under its own contract; there is no
    /// panel-wide neutral or retained-texture presentation.
    pub(crate) fn begin_all_panel_refresh_transactions(&mut self) {
        let now = Instant::now();
        let source_status = self.session.data_status.clone();
        let source_activity = self.status_activity();
        for panel in self.session.panels.iter_mut() {
            let panel = &mut panel.state;
            panel.navigation_query_schedule.cancel();
            panel
                .navigation_presentation
                .begin_navigation_with_activity(
                    panel.viewport,
                    panel.generation_clock.current().get(),
                    self.workspace_layout.revision,
                    self.session.navigation_contract_revision,
                    &source_status,
                    source_activity,
                    now,
                );
        }
    }

    pub(crate) fn schedule_resolution_query_refresh(&mut self) {
        self.pending_resolution_query_refresh = Some(Instant::now() + Duration::from_millis(150));
    }

    fn dispatch_due_resolution_query_refresh(&mut self, now: Instant) {
        if !self
            .pending_resolution_query_refresh
            .is_some_and(|deadline| deadline <= now)
        {
            return;
        }
        self.pending_resolution_query_refresh = None;
        let mut source_refresh_requested = false;
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            let resolution =
                AlignmentSceneResolution::for_viewport(self.session.panels.panel(id).viewport);
            let alignment_refresh_required = self.session.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(id).is_none_or(|projection| {
                        projection
                            .last_scene_resolution
                            .is_none_or(|previous| resolution.requires_refresh_from(previous))
                    })
            });
            if alignment_refresh_required {
                self.retain_active_read_selection_for_query_refresh();
                self.advance_alignment_generations(id);
                self.request_alignment_query(id);
                source_refresh_requested = true;
            }
            let reference_missing = self.session.reference_engine.is_some()
                && self.session.panels.panel(id).viewport.bases_per_pixel()
                    <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
                && (self.session.panels.panel(id).reference_slice_revision
                    != self.session.reference_revision
                    || self
                        .session
                        .panels
                        .panel(id)
                        .reference_slice
                        .as_ref()
                        .is_none_or(|slice| {
                            !slice.interval.contains_interval(
                                self.session.panels.panel(id).viewport.interval(),
                            )
                        }));
            if reference_missing {
                self.request_reference_query(id);
                source_refresh_requested = true;
            }
        }
        if source_refresh_requested {
            self.resize_query_refreshes = self.resize_query_refreshes.saturating_add(1);
        }
    }

    pub(crate) fn request_track_query(&mut self, panel_id: PanelId) {
        self.session
            .panels
            .panel_mut(panel_id)
            .pending_queries
            .tracks = self.session.track_engine.is_some();
        self.dispatch_track_query(panel_id);
    }

    pub(crate) fn request_reference_query(&mut self, panel_id: PanelId) {
        if self.session.request_reference_query(panel_id) {
            self.window.request_redraw();
        }
    }

    pub(crate) fn dispatch_reference_query(&mut self, panel_id: PanelId) {
        if self.session.dispatch_reference_query(panel_id) {
            self.window.request_redraw();
        }
    }

    pub(crate) fn request_all_panel_queries(&mut self) {
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query(id);
            self.request_reference_query(id);
            self.request_track_query(id);
        }
    }

    pub(crate) fn request_all_alignment_queries(&mut self) {
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query(id);
        }
    }

    pub(crate) fn request_all_auxiliary_track_queries(&mut self) {
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.request_track_query(id);
        }
    }

    pub(crate) fn invalidate_and_request_all_auxiliary_track_queries(&mut self) {
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.session.panels.panel(id).generation_clock.advance();
            self.request_reference_query(id);
            self.request_track_query(id);
        }
    }

    pub(crate) fn dispatch_track_query(&mut self, panel_id: PanelId) {
        if !self.session.panels.panel(panel_id).pending_queries.tracks {
            return;
        }
        let Some(engine) = &self.session.track_engine else {
            self.session
                .panels
                .panel_mut(panel_id)
                .pending_queries
                .tracks = false;
            return;
        };
        let interval = buffered_context_interval(
            self.session.panels.panel(panel_id).viewport,
            self.session.panels.panel(panel_id).contig_length,
            self.session.settings.read_browsing_buffer_percent,
            None,
        );
        let request = self.session.panels.panel(panel_id).request();
        // Tracks drawn by the experimental tile pipeline are read by its engine only. Nothing
        // is routed under the default setting, which leaves the list untouched.
        let track_ids = crate::workspace::track_tiles::legacy_query_track_ids(
            auxiliary_query_track_ids(self.session.tracks.tracks()),
            self.track_tiles.routed(),
        );
        let queried = track_ids.iter().copied().collect::<HashSet<_>>();
        let variant_settings = self
            .session
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant && queried.contains(&track.id))
            .map(|track| (track.id, track.variant.clone()))
            .collect();
        let interaction_settings = self
            .session
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction && queried.contains(&track.id))
            .map(|track| (track.id, track.interaction.clone()))
            .collect();
        let copy_number_settings = self
            .session
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber && queried.contains(&track.id))
            .map(|track| (track.id, track.copy_number.clone()))
            .collect();
        match engine.try_query_selected_with_settings(
            request,
            self.session
                .panels
                .panel_mut(panel_id)
                .generation_clock
                .clone(),
            self.session.panels.panel_mut(panel_id).contig_name.clone(),
            interval,
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(()) => {
                let panel = self.session.panels.panel_mut(panel_id);
                panel.pending_queries.tracks = false;
                panel.track_request_interval = Some(interval);
            }
            Err(EngineQueueError::Full) => {
                self.set_busy_status("Track queue busy; retrying the latest locus…");
            }
            Err(EngineQueueError::Disconnected) => {
                self.session
                    .panels
                    .panel_mut(panel_id)
                    .pending_queries
                    .tracks = false;
                self.session
                    .panels
                    .panel_mut(panel_id)
                    .track_request_interval = None;
                self.report_error(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    pub(crate) fn retry_pending_queries(&mut self) {
        let ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for id in ids {
            let alignment_pending = self.session.alignment_tracks.iter().any(|track| {
                track
                    .projection(id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
            if self
                .session
                .panels
                .panel(id)
                .navigation_query_schedule
                .pending
                .is_some()
            {
                continue;
            }
            self.session.panels.panel_mut(id).pending_queries.alignment = alignment_pending;
            self.dispatch_alignment_query(id);
            self.dispatch_reference_query(id);
            self.dispatch_track_query(id);
        }
    }

    /// Resumes paused, exact source intents only after their typed ledger obstruction has cleared.
    /// The epoch is a cheap change detector, not proof of capacity: unrelated ledger churn is
    /// consumed without reopening the same request.
    fn retry_deferred_alignment_source_pressures(&mut self) {
        if !self.session.alignment_tracks.iter().any(|track| {
            track
                .projections
                .values()
                .any(|projection| projection.scene_pressure_retry.is_some())
        }) {
            return;
        }
        let Ok(snapshot) = self.session.alignment_scene_ledger.progress() else {
            return;
        };
        let epoch = snapshot.progress_epoch();
        let mut waiting_updates = Vec::new();
        let mut retries = Vec::new();
        for (track_index, track) in self.session.alignment_tracks.iter().enumerate() {
            if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
                continue;
            }
            for (panel_id, projection) in &track.projections {
                let Some(retry) = projection.scene_pressure_retry else {
                    continue;
                };
                let Some(owner) = projection.scene_owner else {
                    continue;
                };
                if !alignment_scene_pressure_is_current(
                    projection.navigation.request_token(),
                    retry.request_token,
                    projection.navigation.desired_camera(),
                    retry.target,
                ) {
                    continue;
                }
                match retry.poll(&snapshot) {
                    AlignmentScenePressureRetryPoll::Waiting(updated) => {
                        if updated != retry {
                            waiting_updates.push((track_index, *panel_id, retry, updated));
                        }
                    }
                    AlignmentScenePressureRetryPoll::Ready => retries.push((
                        track_index,
                        *panel_id,
                        owner,
                        retry.request_token,
                        retry.target,
                    )),
                }
            }
        }

        for (track_index, panel_id, observed, updated) in waiting_updates {
            if let Some(retry) = self.session.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .scene_pressure_retry
                .as_mut()
            {
                // Preserve a newer engine response if one was consumed between collection and
                // mutation (the desktop currently drains serially, but this keeps the authority
                // local and explicit).
                if *retry == observed {
                    *retry = updated;
                }
            }
        }

        for (track_index, panel_id, owner, request_token, target) in retries {
            let result = self.session.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| engine.try_retry_resource_pressure(owner, request_token, target));
            {
                let projection =
                    self.session.alignment_tracks[track_index].projection_mut(panel_id);
                if let Some(retry) = projection.scene_pressure_retry.as_mut()
                    && retry.same_request(request_token, target)
                {
                    retry.ledger_epoch = epoch;
                    retry.in_flight = matches!(result, Some(Ok(())));
                }
                if matches!(result, Some(Ok(())))
                    && projection.navigation.phase()
                        == AlignmentNavigationPhase::WaitingForSourcePressure
                {
                    projection.navigation.transition(
                        AlignmentNavigationPhase::Running,
                        "ledger progress resumed the exact source target",
                    );
                }
            }
            if matches!(result, Some(Err(EngineQueueError::Disconnected)) | None) {
                self.session.alignment_tracks[track_index]
                    .projection_mut(panel_id)
                    .settle_failed("alignment source mailbox disconnected during pressure retry");
                self.session.alignment_tracks[track_index].lifecycle =
                    AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                self.session.alignment_tracks[track_index].scene_budget_retry = None;
            }
        }
    }

    /// Replays a source coordinator's latest refused budget edit exactly once after its prepared
    /// replacement obstruction clears. Every source still runs its own eviction pass even though
    /// all sources and renderers share one enforcing ledger.
    fn retry_deferred_alignment_budget_pressures(&mut self) {
        if !self
            .session
            .alignment_tracks
            .iter()
            .any(|track| track.scene_budget_retry.is_some())
        {
            return;
        }
        let Ok(snapshot) = self.session.alignment_scene_ledger.progress() else {
            return;
        };
        let desired_budget = self.session.alignment_scene_budget.budget_bytes;
        let mut stale = Vec::new();
        let mut waiting_updates = Vec::new();
        let mut retries = Vec::new();
        for (track_index, track) in self.session.alignment_tracks.iter().enumerate() {
            let Some(retry) = track.scene_budget_retry else {
                continue;
            };
            if !alignment_scene_budget_event_is_current(
                desired_budget,
                track.scene_budget_bytes.get(),
                retry.requested_budget,
            ) {
                stale.push(track_index);
                continue;
            }
            match retry.poll(&snapshot) {
                AlignmentSceneBudgetRetryPoll::Waiting(updated) => {
                    if updated != retry {
                        waiting_updates.push((track_index, updated));
                    }
                }
                AlignmentSceneBudgetRetryPoll::Ready => {
                    retries.push((
                        track_index,
                        retry.requested_budget,
                        retry.clear_before_apply,
                    ));
                }
            }
        }

        for track_index in stale {
            self.session.alignment_tracks[track_index].scene_budget_retry = None;
        }
        for (track_index, updated) in waiting_updates {
            if self.session.alignment_tracks[track_index]
                .scene_budget_retry
                .is_some_and(|retry| retry.requested_budget == updated.requested_budget)
            {
                self.session.alignment_tracks[track_index].scene_budget_retry = Some(updated);
            }
        }
        for (track_index, requested_budget, clear_before_apply) in retries {
            let result = self.session.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
                .map(|engine| {
                    engine.try_retry_budget_pressure(requested_budget, clear_before_apply)
                });
            if let Some(retry) = self.session.alignment_tracks[track_index]
                .scene_budget_retry
                .as_mut()
                && retry.requested_budget == requested_budget
            {
                retry.ledger_epoch = snapshot.progress_epoch();
                retry.in_flight = matches!(result, Some(Ok(())));
            }
            if matches!(result, Some(Err(EngineQueueError::Disconnected)) | None) {
                self.session.alignment_tracks[track_index].lifecycle =
                    AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    pub(crate) fn poll_background_work(&mut self) {
        let now = Instant::now();
        self.dispatch_due_navigation_queries(now);
        self.dispatch_due_resolution_query_refresh(now);
        self.poll_reference_events();
        self.poll_alignment_events();
        // Both are drained every poll; under the strict policy they watch the same ledger.
        let scene_ledger_progressed = self.alignment_ledger_progress.take_pending();
        let gpu_ledger_progressed = self.alignment_gpu_ledger_progress.take_pending();
        let ledger_progressed = scene_ledger_progressed || gpu_ledger_progressed;
        if ledger_progressed {
            self.diagnostics.health(HealthSignal::WorkerProgress);
        }
        let immediate_budget_retry = self.session.alignment_tracks.iter().any(|track| {
            track
                .scene_budget_retry
                .is_some_and(|retry| retry.ready_after_observed_progress)
        });
        let immediate_gpu_retry = self.alignment_gpu_publication_retry.is_some_and(|retry| {
            matches!(
                retry.gate,
                AlignmentGpuPublicationRetryGate::ReadyAfterObservedProgress
            )
        });
        if ledger_progressed || immediate_budget_retry {
            self.retry_deferred_alignment_budget_pressures();
            self.retry_deferred_alignment_source_pressures();
        }
        if ledger_progressed || immediate_gpu_retry {
            self.retry_deferred_alignment_gpu_publication();
        }
        self.top_up_partial_alignment_gpu_residency();
        self.synchronize_alignment_coverage();
        self.synchronize_track_tiles();
        self.poll_track_events();
        self.synchronize_alignment_contextual_product_states();
        self.poll_alignment_retirement_tasks();
        self.poll_engine_retirement_tasks();
        self.poll_session_load_tasks();
        self.poll_session_save_tasks();
        self.poll_settings_writer();
        self.poll_export_tasks();
        self.poll_workspace_list_import_tasks();
        self.poll_genome_tasks();
        self.retry_pending_queries();
        self.update_navigation_readiness();
        if self.reveal_due_alignment_checking_notices(now)
            || self.reveal_due_navigation_notices(now)
        {
            self.mark_display_dirty("explicit display invalidation");
        }
        if self.display_dirty {
            self.window.request_redraw();
        }
        self.maybe_start_pending_manual_diagnostic_trace();
        let desired_generation = self
            .session
            .panels
            .iter()
            .map(|panel| panel.generation_clock.current().get())
            .max()
            .unwrap_or_default();
        let committed_generation = if self.automation_is_settled() {
            desired_generation
        } else {
            desired_generation.saturating_sub(1)
        };
        self.diagnostics.health(HealthSignal::Generations {
            desired: desired_generation,
            committed: committed_generation,
        });
    }

    fn synchronize_alignment_contextual_product_states(&mut self) {
        let has_contextual_lanes = !self.session.tracks.tracks().is_empty();
        let states = self
            .session
            .panels
            .iter()
            .map(|panel| {
                let generation = panel.generation_clock.current().get();
                let state = if !has_contextual_lanes {
                    AlignmentNavigationProductState::Independent
                } else if panel
                    .track_query_error_generations
                    .values()
                    .any(|failed_generation| *failed_generation == generation)
                {
                    AlignmentNavigationProductState::IndependentFailed
                } else if panel.pending_queries.tracks
                    || panel.track_settled_generation != Some(generation)
                {
                    AlignmentNavigationProductState::IndependentPending
                } else {
                    AlignmentNavigationProductState::IndependentReady
                };
                (panel.id, state)
            })
            .collect::<Vec<_>>();
        for track in &mut self.session.alignment_tracks {
            for (panel_id, state) in &states {
                let reason = match state {
                    AlignmentNavigationProductState::Independent => {
                        "no contextual annotation lanes are required"
                    }
                    AlignmentNavigationProductState::IndependentPending => {
                        "contextual annotation lanes remain independently pending"
                    }
                    AlignmentNavigationProductState::IndependentReady => {
                        "contextual annotation lanes are independently ready"
                    }
                    AlignmentNavigationProductState::IndependentFailed => {
                        "contextual annotation lanes failed independently"
                    }
                    _ => unreachable!("contextual product synchronization uses independent states"),
                };
                track
                    .projection_mut(*panel_id)
                    .navigation
                    .observe_contextual_lanes(*state, reason);
            }
        }
    }

    fn reveal_due_navigation_notices(&mut self, now: Instant) -> bool {
        self.session
            .panels
            .iter_mut()
            .fold(false, |changed, panel| {
                panel.navigation_presentation.reveal_notice_if_due(now) || changed
            })
    }

    fn reveal_due_alignment_checking_notices(&mut self, now: Instant) -> bool {
        self.session
            .alignment_tracks
            .iter_mut()
            .flat_map(|track| track.projections.values_mut())
            .fold(false, |changed, projection| {
                projection.reveal_checking_notice_if_due(now) || changed
            })
    }

    pub(crate) fn poll_alignment_retirement_tasks(&mut self) {
        self.session.poll_alignment_retirement_tasks();
    }

    pub(crate) fn finish_alignment_retirement_tasks(&mut self) {
        self.session.finish_alignment_retirement_tasks();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_reference_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.session.reference_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.report_error(error.to_string());
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }
        let active_panel_id = self.session.panels.active().id;
        let mut reference_panels_to_retry = BTreeSet::new();
        let mut highlight_panels_to_synchronize = BTreeSet::new();
        for event in events {
            match event {
                ReferenceEngineEvent::Opened {
                    revision,
                    dictionary,
                    remote_io_stats,
                } => {
                    let reference_contig_count = dictionary.len();
                    let alignment_incompatibility =
                        self.primary_alignment_dictionary().and_then(|alignment| {
                            match alignment_reference_compatibility(
                                alignment,
                                &dictionary,
                                &self.session.contig_aliases,
                            ) {
                                AlignmentReferenceCompatibility::Compatible => None,
                                AlignmentReferenceCompatibility::Incompatible(reason) => {
                                    Some(reason)
                                }
                            }
                        });
                    self.session.reference_open_error = None;
                    self.session.reference_revision = Some(revision);
                    self.reference_remote_io_stats = remote_io_stats;
                    self.session.reference_dictionary = Some(dictionary.clone());
                    // Opening (including reopening the same path) establishes a new immutable
                    // authority. No slice or in-flight request from the previous handle may be
                    // paired with contracts carrying this revision.
                    for panel in self.session.panels.iter_mut() {
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
                    highlight_panels_to_synchronize
                        .extend(self.session.panels.iter().map(|panel| panel.id));
                    if self.primary_alignment_dictionary().is_none() {
                        let ids = self
                            .session
                            .panels
                            .iter()
                            .map(|panel| panel.id)
                            .collect::<Vec<_>>();
                        let mut imported_feature_search = None;
                        for id in ids {
                            let (requested, pixel_width) = self
                                .session
                                .panels
                                .get(id)
                                .map(|panel| {
                                    (panel.requested_locus.clone(), panel.viewport.pixel_width())
                                })
                                .expect("panel id came from the panel collection");
                            match self
                                .resolve_panel_requested_locus(&dictionary, requested.as_deref())
                                .and_then(|locus| {
                                    Ok((Viewport::new(locus.interval, pixel_width)?, locus))
                                }) {
                                Ok((viewport, locus)) => {
                                    let RequestedLocus {
                                        interval,
                                        contig_name: name,
                                        contig_length: length,
                                        feature_search,
                                    } = locus;
                                    imported_feature_search =
                                        imported_feature_search.or(feature_search);
                                    let panel = self
                                        .session
                                        .panels
                                        .get_mut(id)
                                        .expect("panel id came from the panel collection");
                                    panel.generation_clock.advance();
                                    panel.viewport = viewport;
                                    panel.contig_name = name;
                                    panel.contig_length = length;
                                    panel.reset_history(interval);
                                    panel.vertical_scroll_points = 0.0;
                                    panel.vertical_content_origin_points = 0.0;
                                    panel.latent_vertical_scroll_fraction = None;
                                }
                                Err(error) if id == active_panel_id => {
                                    self.report_error(error.to_string());
                                }
                                Err(_) => {}
                            }
                        }
                        self.finish_imported_locus(imported_feature_search);
                        let default_annotation_count =
                            self.session.assembly.default_annotation_count;
                        let (assembly, cytoband_failure) = load_assembly_reporting(
                            &self.session.assembly_name,
                            dictionary.clone(),
                            self.session.cytoband_path.as_deref(),
                        );
                        self.session.assembly = assembly;
                        if let Some(failure) = cytoband_failure {
                            self.report_error(failure);
                        }
                        self.session.assembly.default_annotation_count = default_annotation_count;
                    }
                    self.update_landmark_resolution_from_dictionary(&dictionary);
                    self.session.assembly.reference_contig_count = reference_contig_count;
                    self.session
                        .assembly
                        .identity
                        .clone_from(&self.session.assembly_identity);
                    self.session.synthetic = false;
                    if !self.has_alignment_engine() {
                        let source = self.session.reference_source.as_ref().map_or_else(
                            || "reference genome".to_owned(),
                            TrackSource::redacted_label,
                        );
                        self.session.data_status = format!("Opened {source}");
                    } else if let Some(reason) = alignment_incompatibility {
                        self.session.data_status = format!(
                            "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                        );
                    }
                    self.refresh_active_panel_context();
                    self.request_all_panel_queries();
                    self.mark_display_dirty("explicit display invalidation");
                }
                ReferenceEngineEvent::QueryCompleted {
                    query,
                    slice,
                    remote_io_stats,
                } => {
                    let request = query.request();
                    let revision = query.revision();
                    self.reference_remote_io_stats = remote_io_stats;
                    let is_active = request.panel_id() == active_panel_id;
                    let mut accepted = false;
                    let current_revision = self.session.reference_revision;
                    if let Some(panel) = self.session.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && reference_query_completion_matches(
                            current_revision,
                            panel.reference_request_revision,
                            panel.reference_request_interval,
                            revision,
                            query.interval(),
                        )
                        && query.interval() == slice.interval
                    {
                        accepted = true;
                        panel.reference_slice = Some(Arc::clone(&slice));
                        panel.reference_slice_revision = Some(revision);
                        panel.reference_slice_generation = Some(request.generation().get());
                        panel.reference_request_revision = None;
                        panel.reference_request_interval = None;
                        panel.reference_query_error_generation = None;
                        self.mark_display_dirty("explicit display invalidation");
                        if is_active && !self.has_alignment_engine() {
                            "Reference sequence loaded".clone_into(&mut self.session.data_status);
                        }
                    }
                    if accepted {
                        highlight_panels_to_synchronize.insert(request.panel_id());
                        let installed = self.session.install_reference_authority_for_panel(
                            request.panel_id(),
                            revision,
                            &slice,
                        );
                        let contextual_horizon =
                            self.session.panels.get(request.panel_id()).map(|panel| {
                                buffered_context_interval(
                                    panel.viewport,
                                    panel.contig_length,
                                    self.session.settings.read_browsing_buffer_percent,
                                    Some(MAX_REFERENCE_QUERY_BASES),
                                )
                            });
                        let context_covered = contextual_horizon
                            .is_some_and(|interval| slice.interval.contains_interval(interval));
                        let settled = installed && context_covered;
                        if let Some(panel) = self.session.panels.get_mut(request.panel_id()) {
                            panel.pending_queries.reference = !settled;
                            if settled {
                                panel.reference_settled_generation =
                                    Some(request.generation().get());
                            }
                        }
                        if !settled {
                            reference_panels_to_retry.insert(request.panel_id());
                        }
                    } else {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                    }
                }
                ReferenceEngineEvent::Error { query, message } => {
                    if query.is_none() {
                        self.session.reference_open_error = Some(message.clone());
                        self.session.reference_revision = None;
                        self.session.reference_dictionary = None;
                        // A failed open is terminal for this actor. Leaving it installed would
                        // make every later Base navigation advertise reference work even though
                        // no open revision exists and no query can be issued.
                        self.retire_reference_engine();
                        for panel in self.session.panels.iter_mut() {
                            panel.settle_reference_open_failure();
                        }
                        for projection in self
                            .session
                            .alignment_tracks
                            .iter_mut()
                            .flat_map(|track| track.projections.values_mut())
                        {
                            if projection.navigation.phase()
                                == AlignmentNavigationPhase::WaitingForReference
                            {
                                projection.settle_failed(
                                    "reference source failed while evaluating the alignment view filter",
                                );
                            } else {
                                projection.navigation.observe_reference_failure(
                                    "reference source failed; core alignment navigation remains independent",
                                );
                            }
                        }
                    }
                    let current = query.is_none_or(|query| {
                        let request = query.request();
                        self.session
                            .panels
                            .get(request.panel_id())
                            .is_some_and(|panel| {
                                panel.accepts(request)
                                    && reference_query_completion_matches(
                                        self.session.reference_revision,
                                        panel.reference_request_revision,
                                        panel.reference_request_interval,
                                        query.revision(),
                                        query.interval(),
                                    )
                            })
                    });
                    if query.is_some() && !current {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                    }
                    if current {
                        let mut failed_authority = None;
                        if let Some(query) = query {
                            let request = query.request();
                            if let Some(panel) = self.session.panels.get_mut(request.panel_id()) {
                                failed_authority =
                                    Some((request.panel_id(), query.revision(), query.interval()));
                                panel.reference_request_revision = None;
                                panel.reference_request_interval = None;
                                panel.reference_settled_generation =
                                    Some(request.generation().get());
                                panel.reference_query_error_generation =
                                    Some(request.generation().get());
                            }
                        }
                        if let Some((panel_id, revision, interval)) = failed_authority {
                            let mut failed_owners = Vec::new();
                            for track in &mut self.session.alignment_tracks {
                                let projection = track.projection_mut(panel_id);
                                let matching_current_requirement =
                                    projection.scene_frame.as_ref().is_some_and(|frame| {
                                        projection.navigation.desired_camera() == Some(frame.camera)
                                            && alignment_scene_event_token_is_current(
                                                projection.navigation.request_token(),
                                                projection.scene_frame_request_token,
                                            )
                                            && reference_failure_matches_requirement(
                                                revision,
                                                interval,
                                                frame.reference_requirement,
                                            )
                                    });
                                if matching_current_requirement {
                                    if projection.navigation.phase()
                                        == AlignmentNavigationPhase::WaitingForReference
                                    {
                                        // Reference interpretation is optional for an already
                                        // complete scene, but it is a mandatory dependency of a
                                        // reference-backed view filter. Settle that exact request
                                        // truthfully instead of leaving it ownerless or showing
                                        // rows evaluated under the previous filter.
                                        projection.settle_failed(
                                            "reference query failed while evaluating the alignment view filter",
                                        );
                                    } else {
                                        projection.finish_checking();
                                        projection.navigation.observe_reference_failure(
                                            "reference query failed; core alignment navigation remains independent",
                                        );
                                    }
                                    if let Some(owner) = projection.scene_owner {
                                        failed_owners.push(owner);
                                    }
                                }
                            }
                            let alignment_pending =
                                self.session.alignment_tracks.iter().any(|track| {
                                    !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                                        && track.projection(panel_id).is_some_and(|projection| {
                                            !projection.navigation.is_current_settled(
                                                projection.clock.current().get(),
                                            )
                                        })
                                });
                            if let Some(panel) = self.session.panels.get_mut(panel_id) {
                                panel.pending_queries.alignment = alignment_pending;
                                for owner in failed_owners {
                                    panel.reference_installs_queued.remove(&owner);
                                }
                            }
                            let remaining = self
                                .session
                                .current_alignment_reference_requirements(panel_id, revision);
                            let needs_another_query =
                                self.session.panels.get(panel_id).is_some_and(|panel| {
                                    matches!(
                                        plan_alignment_reference_lanes(
                                            &remaining,
                                            &panel.reference_installs_queued,
                                        ),
                                        AlignmentReferenceLanePlan::Query { .. }
                                    )
                                });
                            if needs_another_query {
                                if let Some(panel) = self.session.panels.get_mut(panel_id) {
                                    panel.pending_queries.reference = true;
                                }
                                reference_panels_to_retry.insert(panel_id);
                            }
                        }
                        self.session.data_status = message;
                    }
                }
            }
        }
        if !reference_panels_to_retry.is_empty() {
            for panel_id in reference_panels_to_retry {
                self.dispatch_reference_query(panel_id);
            }
        }
        if !highlight_panels_to_synchronize.is_empty() {
            for panel_id in highlight_panels_to_synchronize {
                self.synchronize_alignment_scene_interactions(panel_id);
            }
            self.rebuild_all_panel_displays();
            self.prepare_alignment_scene_frame();
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_alignment_events(&mut self) {
        let active_panel_id = self.session.panels.active().id;
        let primary_id = self
            .session
            .alignment_tracks
            .first()
            .map(|track| track.config.id);
        let mut events = Vec::new();
        for track in &mut self.session.alignment_tracks {
            let track_id = track.config.id;
            let Some(engine) = &track.scene_engine else {
                continue;
            };
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push((track_id, event)),
                    Ok(None) => break,
                    Err(error) => {
                        track.lifecycle = AlignmentTrackLifecycle::Failed(error.to_string());
                        if Some(track_id) == primary_id {
                            crate::app::shell_state::report_error_to(
                                &mut self.session.notifications,
                                &mut self.session.data_status,
                                error.to_string(),
                            );
                        }
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }

        let mut opened_primary_dictionary = None;
        let mut source_opened = false;
        let mut rebuild_display = false;
        let mut prepare_frame = false;
        let mut reference_panels_to_request = BTreeSet::new();
        let mut highlight_panels_to_synchronize = BTreeSet::new();
        let mut ownerless_progress_frames = Vec::new();
        for (track_id, event) in events {
            let Some(track_index) = self
                .session
                .alignment_tracks
                .iter()
                .position(|track| track.config.id == track_id)
            else {
                continue;
            };
            match event {
                AlignmentSceneEngineEvent::SourceOpened { metadata } => {
                    let dictionary = metadata.dictionary.clone();
                    let track = &mut self.session.alignment_tracks[track_index];
                    track.lifecycle = AlignmentTrackLifecycle::Ready;
                    track.dictionary = Some(dictionary.clone());
                    track.index_overview = metadata.index_overview.clone();
                    track.scene_metadata = Some(metadata);
                    let panel_ids = track.projections.keys().copied().collect::<Vec<_>>();
                    let track_id = track.config.id;
                    self.start_alignment_coverage_engine(track_index);
                    for panel_id in panel_ids {
                        let token = self.take_alignment_scene_request_token();
                        let projection =
                            self.session.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.navigation.bind_identity(panel_id, track_id);
                        projection.navigation.begin(
                            projection.clock.current().get(),
                            token,
                            "alignment source opened",
                        );
                    }
                    source_opened = true;
                    rebuild_display = true;
                    if Some(track_id) == primary_id {
                        opened_primary_dictionary = Some(dictionary);
                    }
                }
                AlignmentSceneEngineEvent::FrameUpdated {
                    owner,
                    request_token,
                    cause,
                    published_scene,
                    frame,
                    work,
                    telemetry,
                    update,
                } => {
                    let frame = alignment_scene_frame_for_ui(frame);
                    let panel_id = self.session.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    };
                    if self.diagnostics.recording()
                        && cause == AlignmentSceneFrameCause::SourceSettlement
                    {
                        self.diagnostics.event(
                            "alignment_worker",
                            "source_settlement_without_scene_publication",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "panel_id": panel_id.get(),
                                "owner": owner.get(),
                                "request_token": request_token.get(),
                                "camera_sequence": frame.camera_sequence,
                                "camera": format!("{:?}", frame.camera.interval()),
                                "reference_requirement": format!("{:?}", frame.reference_requirement),
                                "individual_reads": format!("{:?}", frame.individual_reads),
                                "visible_complete": frame.visible_complete(),
                                "scene_contracts_current": frame.scene_contracts_current,
                                "reference_products_current": frame.reference_products_current,
                                "source_pending": work.source_pending,
                                "presentation_pending": work.presentation_pending,
                            }),
                        );
                    }
                    if self.diagnostics.recording()
                        && let Some(capacity) = update.and_then(|update| update.capacity_plan)
                    {
                        self.diagnostics.event(
                            "alignment_cache",
                            "foreground_capacity_plan",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "panel_id": panel_id.get(),
                                "owner": owner.get(),
                                "camera_sequence": frame.camera_sequence,
                                "budget": capacity.budget,
                                "charged": capacity.charged,
                                "pinned": capacity.pinned,
                                "reserved": capacity.reserved,
                                "foreground_reserve_bytes": capacity.foreground_reserve_bytes,
                                "low_water_target": capacity.low_water_target,
                                "return_cache_bytes": capacity.return_cache_bytes,
                                "disposable_speculation_bytes": capacity.disposable_speculation_bytes,
                                "proactive_reclaim_target": update.and_then(|update| update.proactive_reclaim_target),
                            }),
                        );
                    }
                    if self.session.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| projection.index_only_overview)
                    {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    let track = &mut self.session.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    if !projection
                        .navigation
                        .worker_frame_is_admissible(request_token, frame.camera)
                    {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    let monotonic = projection
                        .scene_frame
                        .as_ref()
                        .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                    if !monotonic {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    let row_activity_changed = projection
                        .scene_frame
                        .as_ref()
                        .and_then(|current| current.row_activity.as_deref())
                        != frame.row_activity.as_deref();
                    let previous_highlight_authority = projection
                        .scene_frame
                        .as_ref()
                        .and_then(|current| current.snapshot.as_ref())
                        .map(|snapshot| {
                            (
                                snapshot.reads().evidence().revision(),
                                snapshot.prepared_interval(),
                            )
                        });
                    let next_highlight_authority = frame.snapshot.as_ref().map(|snapshot| {
                        (
                            snapshot.reads().evidence().revision(),
                            snapshot.prepared_interval(),
                        )
                    });
                    if previous_highlight_authority != next_highlight_authority {
                        highlight_panels_to_synchronize.insert(panel_id);
                        rebuild_display = true;
                        prepare_frame = true;
                    }
                    let camera_refusal_is_terminal = frame.individual_reads_refused_for_camera();
                    projection.scene_frame = Some(frame.clone());
                    projection.scene_frame_request_token = Some(request_token);
                    projection.scene_telemetry = telemetry;
                    let current_target =
                        projection.navigation.desired_camera() == Some(frame.camera);
                    if current_target {
                        projection.unavailable = None;
                        projection.index_only_overview = false;
                        projection.navigation.observe_frame_products(&frame);
                        let reference_blocks_core_scene = projection
                            .scene_contract
                            .as_ref()
                            .is_some_and(|contract| contract.view_filter.requires_reference());
                        let settlement =
                            alignment_scene_frame_settlement(&frame, reference_blocks_core_scene);
                        if settlement != AlignmentSceneFrameSettlement::WaitingForScene {
                            projection.scene_pressure_retry = None;
                        }
                        match settlement {
                            AlignmentSceneFrameSettlement::Settled => {
                                // A later terminal frame in this same mailbox drain supersedes an
                                // earlier progress frame which may have queued a FASTA request.
                                reference_panels_to_request.remove(&panel_id);
                                projection.finish_checking();
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Committed
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::CandidateReady,
                                        "complete semantic scene is ready for atomic GPU publication",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::SettledWithReferencePending => {
                                // The core scene is complete and can commit now. FASTA-backed
                                // substitutions continue as an optional product upgrade under the
                                // same request token; they do not own alignment navigation.
                                reference_panels_to_request.insert(panel_id);
                                projection.finish_checking();
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Committed
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::CandidateReady,
                                        "reference-independent scene is ready; substitution interpretation remains pending",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::WaitingForReference => {
                                // A reference-backed view filter cannot produce a truthful row
                                // atlas or semantic scene until the exact FASTA authority arrives.
                                // Transfer navigation ownership away from the now-idle alignment
                                // source and queue the panel's current reference requirement.
                                reference_panels_to_request.insert(panel_id);
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::WaitingForReference
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::WaitingForReference,
                                        "reference authority is required to evaluate the alignment view filter",
                                    );
                                }
                            }
                            AlignmentSceneFrameSettlement::WaitingForScene => {
                                if projection.navigation.phase()
                                    != AlignmentNavigationPhase::Running
                                {
                                    projection.navigation.transition(
                                        AlignmentNavigationPhase::Running,
                                        "source or row-presentation work is active",
                                    );
                                }
                                ownerless_progress_frames.retain(
                                    |(candidate_track, candidate_panel, _, _)| {
                                        *candidate_track != track_id || *candidate_panel != panel_id
                                    },
                                );
                                if !work.source_pending && !work.presentation_pending {
                                    ownerless_progress_frames.push((
                                        track_id,
                                        panel_id,
                                        request_token,
                                        frame.camera,
                                    ));
                                }
                            }
                        }
                    }
                    if Some(track_id) == primary_id && panel_id == active_panel_id && current_target
                    {
                        match frame.individual_reads {
                            AlignmentIndividualReadCapability::Complete { .. } => {
                                let records = frame.snapshot.as_ref().map_or(0, |snapshot| {
                                    snapshot.reads().evidence().record_count()
                                });
                                self.session.data_status = match frame.vertical_residency {
                                    AlignmentVerticalResidency::Preparing {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · preparing instant vertical browsing ({prepared_rows}/{total_rows} rows)"
                                    ),
                                    AlignmentVerticalResidency::Complete { total_rows } => {
                                        format!(
                                            "{records} complete alignments · all {total_rows} rows prepared"
                                        )
                                    }
                                    AlignmentVerticalResidency::BudgetLimited {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows resident (cache budget reached)"
                                    ),
                                    AlignmentVerticalResidency::Unavailable {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows prepared (instant deep scrolling unavailable)"
                                    ),
                                    AlignmentVerticalResidency::ViewportBounded => {
                                        format!("{records} complete alignments")
                                    }
                                };
                            }
                            AlignmentIndividualReadCapability::Pending => {
                                "Preparing surrounding alignment scene…"
                                    .clone_into(&mut self.session.data_status);
                            }
                            AlignmentIndividualReadCapability::Refused {
                                complete_read_limit,
                                eligible_records,
                                ..
                            } => {
                                self.session.data_status = format!(
                                    "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                                );
                            }
                        }
                    }
                    rebuild_display |= camera_refusal_is_terminal
                        || row_activity_changed
                        || (published_scene.is_some()
                            && cause != AlignmentSceneFrameCause::VerticalResidency);
                    // Optional vertical warm-up now populates independently keyed semantic pages
                    // without publishing a scene. Its progress/status event must not recapture or
                    // replace the foreground GPU presentation on every background row block.
                    prepare_frame |= cause != AlignmentSceneFrameCause::VerticalResidency
                        || published_scene.is_some()
                        || row_activity_changed;
                }
                AlignmentSceneEngineEvent::IndividualReadsRefused {
                    owner,
                    request_token,
                    target,
                    complete_read_limit,
                    eligible_records,
                    frame,
                    telemetry,
                } => {
                    let frame = alignment_scene_frame_for_ui(frame);
                    let panel_id = self.session.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    };
                    if self.session.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| projection.index_only_overview)
                    {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    let track = &mut self.session.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    if !alignment_scene_refusal_is_current(
                        projection.navigation.request_token(),
                        request_token,
                        projection.navigation.desired_camera(),
                        frame.camera,
                        target,
                    ) {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    let monotonic = projection
                        .scene_frame
                        .as_ref()
                        .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                    if !monotonic {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }
                    projection.scene_frame = Some(frame.clone());
                    projection.scene_frame_request_token = Some(request_token);
                    projection.scene_telemetry = telemetry;
                    projection.scene_pressure_retry = None;
                    if projection.navigation.desired_camera() == Some(frame.camera) {
                        reference_panels_to_request.remove(&panel_id);
                        projection.finish_checking();
                        projection.navigation.transition(
                            AlignmentNavigationPhase::CandidateReady,
                            "complete source count produced a terminal individual-read refusal",
                        );
                    }
                    if Some(track_id) == primary_id && panel_id == active_panel_id {
                        self.session.data_status = format!(
                            "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                        );
                    }
                    rebuild_display = true;
                    prepare_frame = true;
                }
                AlignmentSceneEngineEvent::ResourcePressure {
                    owner,
                    request_token,
                    target,
                    operation,
                    intent,
                    pressure,
                    disposition,
                } => {
                    let panel_id = self.session.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        });
                    let Some(panel_id) = panel_id else {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    };
                    let current = self.session.alignment_tracks[track_index]
                        .projection(panel_id)
                        .is_some_and(|projection| {
                            !projection.index_only_overview
                                && alignment_scene_pressure_is_current(
                                    projection.navigation.request_token(),
                                    request_token,
                                    projection.navigation.desired_camera(),
                                    target,
                                )
                        });
                    if !current {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    }

                    // Source read-ahead is optional once the current frame is complete. It yields
                    // at its own checkpoint and must not displace a useful GPU scene or reopen the
                    // foreground navigation transaction.
                    if !alignment_scene_pressure_requires_resume(operation, intent) {
                        continue;
                    }

                    if alignment_scene_pressure_preserves_visible_epoch(
                        operation,
                        intent,
                        disposition,
                    ) {
                        let projection =
                            self.session.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.scene_pressure_retry = None;
                        if Some(track_id) == primary_id && panel_id == active_panel_id {
                            let records = projection
                                .scene_frame
                                .as_ref()
                                .and_then(|frame| frame.snapshot.as_ref())
                                .map_or(0, |snapshot| snapshot.reads().evidence().record_count());
                            self.session.data_status = projection.scene_frame.as_ref().map_or_else(
                                || {
                                    "Complete alignments remain available · instant deep scrolling is limited by the cache budget"
                                        .to_owned()
                                },
                                |frame| match frame.vertical_residency {
                                    AlignmentVerticalResidency::Preparing {
                                        prepared_rows,
                                        total_rows,
                                    }
                                    | AlignmentVerticalResidency::BudgetLimited {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows resident (cache budget reached)"
                                    ),
                                    AlignmentVerticalResidency::Unavailable {
                                        prepared_rows,
                                        total_rows,
                                    } => format!(
                                        "{records} complete alignments · {prepared_rows}/{total_rows} rows prepared (instant deep scrolling unavailable)"
                                    ),
                                    AlignmentVerticalResidency::Complete { total_rows } => format!(
                                        "{records} complete alignments · all {total_rows} rows prepared"
                                    ),
                                    AlignmentVerticalResidency::ViewportBounded => format!(
                                        "{records} complete alignments · instant deep scrolling is limited by the cache budget"
                                    ),
                                },
                            );
                        }
                        continue;
                    }

                    if disposition == AlignmentSceneResourcePressureDisposition::Infeasible {
                        let message = format!(
                            "Alignment scene requires more cache capacity than the configured limit: {pressure}"
                        );
                        let projection =
                            self.session.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.scene_pressure_retry = None;
                        projection.settle_unavailable(message.clone());
                        if Some(track_id) == primary_id && panel_id == active_panel_id {
                            self.report_warning(message);
                        }
                        rebuild_display = true;
                        continue;
                    }

                    let snapshot_before = match self.session.alignment_scene_ledger.progress() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.session.data_status =
                                format!("Could not inspect alignment cache capacity: {error}");
                            continue;
                        }
                    };
                    let answers_existing_retry = self.session.alignment_tracks[track_index]
                        .projection(panel_id)
                        .and_then(|projection| projection.scene_pressure_retry)
                        .is_some_and(|retry| retry.same_request(request_token, target));
                    let post_relief = if answers_existing_retry {
                        // This is the typed answer to the queued retry. Revalidate its newest exact
                        // obstruction even if occupancy changed while the request was in flight.
                        snapshot_before
                    } else {
                        match self.yield_alignment_gpu_speculation_for_source_pressure(pressure) {
                            Ok(_) => {}
                            Err(error) => {
                                self.session.data_status =
                                    format!("Could not yield optional GPU residency: {error}");
                                continue;
                            }
                        };
                        match self.session.alignment_scene_ledger.progress() {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                self.session.data_status =
                                    format!("Could not inspect alignment cache capacity: {error}");
                                continue;
                            }
                        }
                    };
                    let retry_epoch = post_relief.progress_epoch();
                    let pressure_owns_navigation = alignment_scene_pressure_owns_navigation(
                        self.session.alignment_tracks[track_index]
                            .projection(panel_id)
                            .map_or(AlignmentNavigationPhase::Dormant, |projection| {
                                projection.navigation.phase()
                            }),
                    );
                    if let AlignmentScenePressureRetryPoll::Waiting(waiting) =
                        alignment_scene_pressure_after_relief(
                            request_token,
                            target,
                            pressure,
                            &post_relief,
                        )
                    {
                        // A first pressure edge is not itself permission for a guaranteed-failing
                        // retry. Record the exact post-yield blocker and wait for relevant ledger
                        // progress; a later pressure answer replaces it through the same path.
                        self.session.alignment_tracks[track_index]
                            .projection_mut(panel_id)
                            .scene_pressure_retry = Some(waiting);
                        if pressure_owns_navigation {
                            self.session.alignment_tracks[track_index]
                                .projection_mut(panel_id)
                                .navigation
                                .transition(
                                    AlignmentNavigationPhase::WaitingForSourcePressure,
                                    "source allocation is paused on an exact ledger blocker",
                                );
                            if Some(track_id) == primary_id && panel_id == active_panel_id {
                                "Waiting for shared alignment cache capacity…"
                                    .clone_into(&mut self.session.data_status);
                            }
                        }
                        continue;
                    }
                    let retry_result = self.session.alignment_tracks[track_index]
                        .scene_engine
                        .as_ref()
                        .map(|engine| {
                            engine.try_retry_resource_pressure(owner, request_token, target)
                        });
                    let retry_queued = matches!(retry_result, Some(Ok(())));
                    self.session.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_pressure_retry = Some(AlignmentScenePressureRetry {
                        request_token,
                        target,
                        pressure,
                        ledger_epoch: retry_epoch,
                        in_flight: retry_queued,
                    });
                    match retry_result {
                        Some(Ok(())) => {
                            if pressure_owns_navigation {
                                self.session.alignment_tracks[track_index]
                                    .projection_mut(panel_id)
                                    .navigation
                                    .transition(
                                        AlignmentNavigationPhase::Running,
                                        "source pressure retry was accepted",
                                    );
                                if Some(track_id) == primary_id && panel_id == active_panel_id {
                                    "Updating alignment scene…"
                                        .clone_into(&mut self.session.data_status);
                                }
                            }
                        }
                        Some(Err(EngineQueueError::Disconnected)) | None => {
                            self.session.alignment_tracks[track_index]
                                .projection_mut(panel_id)
                                .settle_failed("alignment source mailbox disconnected");
                            self.session.alignment_tracks[track_index].lifecycle =
                                AlignmentTrackLifecycle::Failed(
                                    EngineQueueError::Disconnected.to_string(),
                                );
                        }
                        Some(Err(EngineQueueError::Full)) => {
                            // The retry API is replaceable and currently cannot report Full, but
                            // keep the state paused if that mailbox contract ever changes.
                        }
                    }
                }
                AlignmentSceneEngineEvent::BudgetPressure {
                    requested_budget,
                    clear_before_apply,
                    current_policy_revision,
                    current_budget,
                    pressure,
                } => {
                    let desired_budget = self.session.alignment_scene_budget.budget_bytes;
                    let submitted_budget = self.session.alignment_tracks[track_index]
                        .scene_budget_bytes
                        .get();
                    if !alignment_scene_budget_event_is_current(
                        desired_budget,
                        submitted_budget,
                        requested_budget,
                    ) {
                        // A later settings/rebalance command is already ordered behind this
                        // response. Never revive the stale value when the old GPU transition
                        // eventually settles.
                        continue;
                    }
                    let snapshot = match self.session.alignment_scene_ledger.progress() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            self.session.data_status =
                                format!("Could not inspect alignment cache capacity: {error}");
                            continue;
                        }
                    };
                    self.session.alignment_tracks[track_index].scene_budget_retry =
                        Some(AlignmentSceneBudgetRetry::after_pressure_event(
                            requested_budget,
                            clear_before_apply,
                            current_policy_revision,
                            current_budget,
                            pressure,
                            &snapshot,
                        ));
                }
                AlignmentSceneEngineEvent::BudgetReduced { pressure } => {
                    if self.session.alignment_tracks[track_index]
                        .scene_budget_retry
                        .is_some_and(|retry| retry.requested_budget == pressure.budget)
                    {
                        self.session.alignment_tracks[track_index].scene_budget_retry = None;
                    }
                }
                AlignmentSceneEngineEvent::InactiveEvicted {
                    request_id,
                    eviction,
                } => {
                    if self.diagnostics.recording() {
                        self.diagnostics.event(
                            "alignment_cache",
                            "reclaim_completed",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": eviction.target_charged,
                                "charged_before": eviction.charged_before,
                                "charged_after": eviction.charged,
                                "pinned_after": eviction.pinned,
                                "released_bytes": eviction.released,
                                "relinquished_cache_entries": eviction.relinquished_cache_entries,
                                "quantum_entries": eviction.quantum_entries,
                                "quanta": eviction.quanta,
                                "elapsed_nanoseconds": eviction.elapsed_nanoseconds,
                                "reclaim_complete": eviction.reclaim_complete,
                                "remaining_bytes": eviction.remaining,
                                "semantic_candidates_inspected": eviction.presentation.candidates_inspected,
                                "semantic_speculation_entries": eviction.presentation.residency.disposable_speculation_entries,
                                "semantic_speculation_bytes": eviction.presentation.residency.disposable_speculation_bytes,
                                "semantic_active_cold_entries": eviction.presentation.residency.active_locus_cold_entries,
                                "semantic_active_cold_bytes": eviction.presentation.residency.active_locus_cold_bytes,
                                "semantic_active_near_entries": eviction.presentation.residency.active_near_buffer_entries,
                                "semantic_active_near_bytes": eviction.presentation.residency.active_near_buffer_bytes,
                                "semantic_foreground_entries": eviction.presentation.residency.foreground_visible_entries,
                                "semantic_foreground_bytes": eviction.presentation.residency.foreground_visible_bytes,
                                "semantic_return_cache_entries": eviction.presentation.residency.recent_locus_return_cache_entries,
                                "semantic_return_cache_bytes": eviction.presentation.residency.recent_locus_return_cache_bytes,
                                "semantic_read_pages_evicted": eviction.presentation.evicted_read_pages,
                                "semantic_summary_pages_evicted": eviction.presentation.evicted_summary_pages,
                                "evidence_candidates_inspected": eviction.evidence.candidates_inspected,
                                "evidence_active_near_entries": eviction.evidence.residency.active_near_buffer_entries,
                                "evidence_active_near_bytes": eviction.evidence.residency.active_near_buffer_bytes,
                                "evidence_return_cache_entries": eviction.evidence.residency.recent_locus_return_cache_entries,
                                "evidence_return_cache_bytes": eviction.evidence.residency.recent_locus_return_cache_bytes,
                                "evidence_acquisitions_evicted": eviction.evidence.evicted_acquisitions,
                                "evidence_chunks_evicted": eviction.evidence.evicted_chunks,
                                "exact_summaries_evicted": eviction.evidence.evicted_exact_summaries,
                                "row_atlases_evicted": eviction.evidence.evicted_row_atlases,
                            }),
                        );
                    }
                    self.observe_alignment_capacity_reclaim(
                        track_id,
                        request_id,
                        eviction.released,
                        eviction.relinquished_cache_entries,
                    );
                }
                AlignmentSceneEngineEvent::InactiveEvictionProgress {
                    request_id,
                    eviction,
                } => {
                    if self.diagnostics.recording() {
                        self.diagnostics.event(
                            "alignment_cache",
                            "reclaim_quantum",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": eviction.target_charged,
                                "charged_before": eviction.charged_before,
                                "charged_after": eviction.charged,
                                "pinned_after": eviction.pinned,
                                "released_bytes": eviction.released,
                                "relinquished_cache_entries": eviction.relinquished_cache_entries,
                                "quantum_entries": eviction.quantum_entries,
                                "elapsed_nanoseconds": eviction.elapsed_nanoseconds,
                                "reclaim_complete": eviction.reclaim_complete,
                                "remaining_bytes": eviction.remaining,
                            }),
                        );
                    }
                }
                AlignmentSceneEngineEvent::InactiveEvictionSuperseded {
                    request_id,
                    target_charged,
                    released_bytes,
                    relinquished_cache_entries,
                    reason,
                } => {
                    if self.diagnostics.recording() {
                        self.diagnostics.event(
                            "alignment_cache",
                            "reclaim_superseded",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "request_id": request_id,
                                "target_charged": target_charged,
                                "released_bytes": released_bytes,
                                "relinquished_cache_entries": relinquished_cache_entries,
                                "reason": format!("{reason:?}"),
                            }),
                        );
                    }
                    self.observe_alignment_capacity_reclaim(
                        track_id,
                        request_id,
                        released_bytes,
                        relinquished_cache_entries,
                    );
                }
                AlignmentSceneEngineEvent::WorkerPhaseCompleted {
                    phase,
                    owner,
                    elapsed_nanoseconds,
                } => {
                    if self.diagnostics.recording() {
                        self.diagnostics.event(
                            "alignment_worker",
                            "slow_phase_completed",
                            serde_json::json!({
                                "track_id": track_id.get(),
                                "owner": owner.map(SourceOwnerId::get),
                                "phase": format!("{phase:?}"),
                                "elapsed_nanoseconds": elapsed_nanoseconds,
                            }),
                        );
                    }
                }
                AlignmentSceneEngineEvent::Error {
                    owner,
                    request_token,
                    operation,
                    message,
                } => {
                    let panel_id = owner.and_then(|owner| {
                        self.session.alignment_tracks[track_index]
                            .projections
                            .iter()
                            .find_map(|(panel_id, projection)| {
                                (projection.scene_owner == Some(owner)).then_some(*panel_id)
                            })
                    });
                    if let Some(panel_id) = panel_id {
                        let projection =
                            self.session.alignment_tracks[track_index].projection_mut(panel_id);
                        if !alignment_scene_event_token_is_current(
                            projection.navigation.request_token(),
                            request_token,
                        ) {
                            self.session.navigation_stale_result_rejections = self
                                .session
                                .navigation_stale_result_rejections
                                .saturating_add(1);
                            continue;
                        }
                        projection.settle_failed(message.to_string());
                    } else if owner.is_some() {
                        self.session.navigation_stale_result_rejections = self
                            .session
                            .navigation_stale_result_rejections
                            .saturating_add(1);
                        continue;
                    } else if matches!(
                        operation,
                        rgv_engine::AlignmentSceneEngineOperation::OpenSource
                    ) {
                        self.session.alignment_tracks[track_index].lifecycle =
                            AlignmentTrackLifecycle::Failed(message.to_string());
                        for projection in self.session.alignment_tracks[track_index]
                            .projections
                            .values_mut()
                        {
                            projection.settle_failed(message.to_string());
                        }
                    } else {
                        // Source pumping is shared by every lane and therefore has no owner in its
                        // error event. It is nevertheless terminal for each currently requested
                        // panel generation. Settle those generations explicitly so a truthful
                        // error cannot be hidden forever behind "Updating view…"; the next camera
                        // or contract request may retry using the last immutable scene.
                        for projection in self.session.alignment_tracks[track_index]
                            .projections
                            .values_mut()
                        {
                            projection.settle_failed(message.to_string());
                        }
                    }
                    if Some(track_id) == primary_id
                        && panel_id.is_none_or(|panel_id| panel_id == active_panel_id)
                    {
                        self.report_error(message.to_string());
                    }
                    rebuild_display = true;
                }
                AlignmentSceneEngineEvent::LaneInserted { .. }
                | AlignmentSceneEngineEvent::LaneRemoved { .. }
                | AlignmentSceneEngineEvent::PresentationPrewarmed { .. }
                | AlignmentSceneEngineEvent::SourceControlQueued => {}
            }
        }

        // Error and progress events can already coexist in one drained mailbox batch. Defer the
        // ownerless-worker invariant until every event has had a chance to settle the request;
        // otherwise an earlier progress frame can replace the real terminal error with a
        // synthetic liveness failure. A later frame for the same lane supersedes the candidate
        // above, and a terminal event makes it inadmissible here.
        for (track_id, panel_id, request_token, camera) in ownerless_progress_frames {
            let Some(track_index) = self
                .session
                .alignment_tracks
                .iter()
                .position(|track| track.config.id == track_id)
            else {
                continue;
            };
            let projection = self.session.alignment_tracks[track_index].projection_mut(panel_id);
            if !projection
                .navigation
                .worker_frame_is_admissible(request_token, camera)
            {
                continue;
            }
            if projection
                .navigation
                .fail_if_source_owner_is_idle(false, false)
            {
                projection.finish_checking();
                projection.unavailable = Some(
                    "Internal liveness failure: the alignment worker became idle before completing this target"
                        .to_owned(),
                );
                if Some(track_id) == primary_id && panel_id == active_panel_id {
                    self.report_error("Internal liveness failure: the alignment worker became idle before completing this target");
                }
                rebuild_display = true;
            }
        }

        if !reference_panels_to_request.is_empty() {
            for panel_id in reference_panels_to_request {
                self.session
                    .panels
                    .panel_mut(panel_id)
                    .pending_queries
                    .reference = true;
                self.dispatch_reference_query(panel_id);
            }
        }

        let panel_ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        for panel_id in panel_ids {
            let pending = self.session.alignment_tracks.iter().any(|track| {
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                    && track.projection(panel_id).is_some_and(|projection| {
                        !projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    })
            });
            if let Some(panel) = self.session.panels.get_mut(panel_id) {
                panel.pending_queries.alignment = pending;
            }
        }

        if let Some(dictionary) = opened_primary_dictionary {
            let reference_incompatibility = self.alignment_reference_incompatibility(&dictionary);
            if let Some(source) = self.primary_alignment_source().cloned() {
                self.session.settings.remember_alignment(&source);
                self.persist_settings();
            }
            let ids = self
                .session
                .panels
                .iter()
                .map(|panel| panel.id)
                .collect::<Vec<_>>();
            let mut imported_feature_search = None;
            for id in ids {
                let (requested, pixel_width) = self
                    .session
                    .panels
                    .get(id)
                    .map(|panel| (panel.requested_locus.clone(), panel.viewport.pixel_width()))
                    .expect("panel id came from the panel collection");
                match self
                    .resolve_panel_requested_locus(&dictionary, requested.as_deref())
                    .and_then(|locus| Ok((Viewport::new(locus.interval, pixel_width)?, locus)))
                {
                    Ok((viewport, locus)) => {
                        let RequestedLocus {
                            interval,
                            contig_name: name,
                            contig_length: length,
                            feature_search,
                        } = locus;
                        imported_feature_search = imported_feature_search.or(feature_search);
                        let panel = self
                            .session
                            .panels
                            .get_mut(id)
                            .expect("panel id came from the panel collection");
                        panel.viewport = viewport;
                        panel.contig_name = name;
                        panel.contig_length = length;
                        panel.reset_history(interval);
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                    }
                    Err(error) if id == active_panel_id => self.report_error(error.to_string()),
                    Err(_) => {}
                }
            }
            self.finish_imported_locus(imported_feature_search);
            let default_annotation_count = self.session.assembly.default_annotation_count;
            let (assembly, cytoband_failure) = load_assembly_reporting(
                &self.session.assembly_name,
                dictionary.clone(),
                self.session.cytoband_path.as_deref(),
            );
            self.session.assembly = assembly;
            if let Some(failure) = cytoband_failure {
                self.report_error(failure);
            }
            self.session.assembly.default_annotation_count = default_annotation_count;
            self.session.assembly.reference_contig_count = self
                .session
                .reference_dictionary
                .as_ref()
                .map_or(dictionary.len(), ReferenceDictionary::len);
            self.session
                .assembly
                .identity
                .clone_from(&self.session.assembly_identity);
            if let Some(reason) = reference_incompatibility {
                for panel in self.session.panels.iter_mut() {
                    panel.reference_slice = None;
                    panel.reference_slice_revision = None;
                    panel.reference_slice_generation = None;
                    panel.reference_request_revision = None;
                    panel.reference_request_interval = None;
                    panel.reference_installs_queued.clear();
                }
                self.session.data_status = format!(
                    "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                );
            }
        }
        if source_opened {
            // Rebuild first so dispatch can derive the read-pane clip from committed layout.
            self.rebuild_all_panel_displays();
            self.request_all_panel_queries();
            rebuild_display = false;
            prepare_frame = true;
        }
        for panel_id in highlight_panels_to_synchronize {
            self.synchronize_alignment_scene_interactions(panel_id);
        }
        if rebuild_display {
            self.rebuild_all_panel_displays();
            prepare_frame = true;
        }
        if prepare_frame {
            self.prepare_alignment_scene_frame();
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_track_events(&mut self) {
        let (events, receive_error) = self.session.receive_track_events();
        if let Some(error) = receive_error {
            self.report_error(error);
        }
        let had_events = !events.is_empty();
        // Ready / TrackAdded / TracksAdded / LaneCompleted may each have met malformed lines;
        // the reporter logs each track once however many events arrive.
        if had_events && self.session.report_track_source_diagnostics() {
            self.window.request_redraw();
        }
        for event in events {
            // An answer to an automation `query_features` or `goto_gene` command belongs to no
            // panel and to no interactive search.
            let Some(event) = self.route_automation_track_event(event) else {
                continue;
            };
            if let TrackEngineEvent::Error {
                track: Some(track_id),
                message,
                ..
            } = &event
                && self.automation_auxiliary_open_state.contains_key(track_id)
            {
                tracing::warn!(
                    track_id = track_id.get(),
                    error = %message,
                    "auxiliary track failed to open"
                );
                self.automation_auxiliary_open_state.insert(
                    *track_id,
                    AutomationAuxiliaryOpenState::Failed(message.clone()),
                );
            }
            let event = match self.session.route_track_event(event) {
                TrackEventRoute::PanelChanged { panel_id } => {
                    self.mark_display_dirty("explicit display invalidation");
                    let active = self.session.panels.active().id;
                    if panel_id == active {
                        self.rebuild_genomic_display(active);
                    }
                    continue;
                }
                TrackEventRoute::ErrorReported { display_stale } => {
                    if display_stale {
                        self.mark_display_dirty("explicit display invalidation");
                    }
                    self.window.request_redraw();
                    continue;
                }
                TrackEventRoute::Discarded => continue,
                TrackEventRoute::Unrouted(event) => event,
            };
            match event {
                TrackEngineEvent::Ready { opened, failed } => {
                    tracing::info!(opened, failed, "auxiliary tracks opened");
                    let pending = self
                        .automation_auxiliary_open_state
                        .iter()
                        .filter_map(|(id, state)| {
                            (state == &AutomationAuxiliaryOpenState::Pending).then_some(*id)
                        })
                        .collect::<Vec<_>>();
                    if pending.len() == 1 {
                        let state = if failed == 0 && opened > 0 {
                            AutomationAuxiliaryOpenState::Ready
                        } else {
                            AutomationAuxiliaryOpenState::Failed(
                                "The auxiliary source could not be opened".to_owned(),
                            )
                        };
                        self.automation_auxiliary_open_state
                            .insert(pending[0], state);
                    }
                    if !self.session.synthetic {
                        self.request_all_panel_queries();
                    }
                }
                TrackEngineEvent::TrackAdded { id } => {
                    if self.automation_auxiliary_open_state.contains_key(&id) {
                        self.automation_auxiliary_open_state
                            .insert(id, AutomationAuxiliaryOpenState::Ready);
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksAdded { ids } => {
                    for id in ids {
                        if self.automation_auxiliary_open_state.contains_key(&id) {
                            self.automation_auxiliary_open_state
                                .insert(id, AutomationAuxiliaryOpenState::Ready);
                        }
                    }
                    self.request_all_panel_queries();
                }
                TrackEngineEvent::TracksRemoved { ids } => {
                    let removed = ids.into_iter().collect::<HashSet<_>>();
                    for panel in self.session.panels.iter_mut() {
                        clear_removed_auxiliary_panel_state(panel, &removed);
                    }
                    self.annotation_search_results
                        .hits
                        .retain(|hit| !removed.contains(&hit.track_id));
                    self.mark_display_dirty("explicit display invalidation");
                }
                TrackEngineEvent::TdfChannelsDiscovered {
                    id,
                    track_names,
                    selected_index,
                } => {
                    if let Err(error) = self.expand_tdf_channels(id, &track_names, selected_index) {
                        self.report_error(error.to_string());
                    }
                }
                TrackEngineEvent::SearchCompleted {
                    generation,
                    mut results,
                } if generation == self.annotation_search_generation => {
                    let pending = self
                        .pending_annotation_navigation
                        .take()
                        .filter(|query| query.eq_ignore_ascii_case(results.query.trim()));
                    self.merge_landmark_search_results(&results.query.clone(), &mut results);
                    self.annotation_search_results = results;
                    if let Some(query) = pending {
                        self.submit_annotation_search_results(&query);
                    }
                    self.mark_display_dirty("explicit display invalidation");
                }
                TrackEngineEvent::AdjacentFeatureCompleted { generation, hit }
                    if generation == self.annotation_navigation_generation =>
                {
                    if let Some(hit) = hit {
                        self.navigate_to_annotation_hit(&hit);
                    } else {
                        "No further annotation feature on this contig"
                            .clone_into(&mut self.session.data_status);
                    }
                }
                // Answers to panel queries were routed above; a stale search or step is
                // dropped.
                TrackEngineEvent::LaneCompleted { .. }
                | TrackEngineEvent::QueryCompleted { .. }
                | TrackEngineEvent::Error { .. }
                | TrackEngineEvent::SearchCompleted { .. }
                | TrackEngineEvent::AdjacentFeatureCompleted { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }
}
