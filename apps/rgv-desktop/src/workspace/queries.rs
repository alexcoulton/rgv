use crate::automation::AutomationAuxiliaryOpenState;
use crate::diagnostics::HealthSignal;
use crate::workspace::alignment_navigation::{
    AlignmentNavigationPhase, AlignmentNavigationProductState,
};
use crate::workspace::alignments::{
    AlignmentSceneBudgetRetryPoll, AlignmentScenePressureRetryPoll, AlignmentSceneResolution,
    AlignmentTrackLifecycle,
};
use crate::workspace::context_lanes::buffered_context_interval;
use crate::workspace::navigation_transaction::{
    NavigationPresentationPhase, NavigationQueryTarget, NavigationTargetFingerprint,
    navigation_target_ready_for_commit,
};
use crate::{
    AlignmentGpuPublicationRetryGate, Viewer, auxiliary_query_track_ids,
    clear_removed_auxiliary_panel_state,
};
use rgv_core::{MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId, TrackKind};
use rgv_engine::{EngineQueueError, TrackEngineEvent};
use rgv_session::track_events::TrackEventRoute;
use std::collections::HashSet;
use std::time::{Duration, Instant};

pub(crate) use rgv_session::alignment::settlement::*;

impl Viewer {
    pub(crate) fn resume_alignment_optional_work_for_settled_panels(&mut self) {
        self.session
            .resume_alignment_optional_work_for_settled_panels();
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
        self.session
            .set_panel_alignment_optional_work_paused(panel_id, true);
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
        self.session.panel_alignment_queries_are_settled(panel_id)
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
    /// Settles reference-engine events; see
    /// [`rgv_session::reference_events::poll_reference_events`].
    pub(crate) fn poll_reference_events(&mut self) {
        rgv_session::reference_events::poll_reference_events(self);
    }

    /// Settles alignment scene engine events; see
    /// [`rgv_session::alignment::events::poll_alignment_events`].
    pub(crate) fn poll_alignment_events(&mut self) {
        rgv_session::alignment::events::poll_alignment_events(self);
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
