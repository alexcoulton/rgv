use super::super::*;
use crate::display::alignment;

impl Viewer {
    fn mark_active_panel_navigation_queries_pending(&mut self) {
        let panel_id = self.panels.active().id;
        for track in &mut self.alignment_tracks {
            track.projection_mut(panel_id).pending_query =
                !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_));
        }
        let alignment_pending = self.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.pending_query)
        });
        let base_marks_visible =
            self.panels.active().viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        let reference_pending = self.reference_engine.is_some() && base_marks_visible;
        let tracks_pending = self.track_engine.is_some();
        let panel = self.panels.active_mut();
        panel.pending_queries.alignment = alignment_pending;
        panel.pending_queries.reference = reference_pending;
        panel.pending_queries.tracks = tracks_pending;
        panel.reference_query_error_generation = None;
        panel.track_query_error_generation = None;
    }

    pub(crate) fn begin_viewport_navigation(
        &mut self,
        gesture_active: bool,
        hold_commit_until_end: bool,
    ) {
        if self
            .panels
            .active()
            .navigation_presentation
            .targets_viewport(self.panels.active().viewport)
        {
            return;
        }
        if self.synthetic
            || !(self.has_alignment_engine()
                || self.reference_engine.is_some()
                || self.track_engine.is_some())
        {
            self.rebuild_genomic_display();
            self.display_dirty = true;
            return;
        }

        let now = Instant::now();
        self.retain_active_read_selection_for_query_refresh();
        let panel_id = self.panels.active().id;
        let previous_target_generation = self
            .panels
            .active()
            .navigation_presentation
            .target_generation;
        let previous_phase = self.panels.active().navigation_presentation.phase;
        self.panels.active().generation_clock.advance();
        self.advance_alignment_generations(panel_id);
        self.mark_active_panel_navigation_queries_pending();
        let generation = self.panels.active().generation_clock.current().get();
        let viewport = self.panels.active().viewport;
        let target = NavigationQueryTarget {
            fingerprint: NavigationTargetFingerprint::from_viewport(viewport),
            generation,
        };
        let source_status = self.data_status.clone();
        let captured_source_revision = self.navigation_texture_revisions.get(&panel_id).copied();
        let panel = self.panels.active_mut();
        if matches!(
            previous_phase,
            NavigationPresentationPhase::Settled
                | NavigationPresentationPhase::ReadyToCommit
                | NavigationPresentationPhase::Committing
        ) {
            panel.navigation_query_schedule.begin_transaction();
        } else if previous_target_generation != generation {
            panel.navigation_query_schedule.record_superseded_target();
        }
        panel.navigation_presentation.begin_navigation(
            viewport,
            generation,
            self.workspace_layout.revision,
            self.navigation_contract_revision,
            &source_status,
            now,
        );
        panel
            .navigation_presentation
            .require_captured_source_revision(captured_source_revision);
        panel.navigation_provisional_display = None;
        panel
            .navigation_query_schedule
            .update(target, now, gesture_active, hold_commit_until_end);
        if !gesture_active {
            let target = self
                .panels
                .active_mut()
                .navigation_query_schedule
                .take_immediate(now, false);
            if let Some(target) = target {
                self.dispatch_navigation_query_target(panel_id, target);
            }
        }
        self.window.request_redraw();
    }

    pub(crate) fn finish_navigation_gesture(&mut self) {
        let now = Instant::now();
        let panel_id = self.panels.active().id;
        let target = self
            .panels
            .active_mut()
            .navigation_query_schedule
            .take_immediate(now, true);
        if let Some(target) = target {
            self.dispatch_navigation_query_target(panel_id, target);
        }
        self.window.request_redraw();
    }

    fn dispatch_due_navigation_queries(&mut self, now: Instant) {
        let active_id = self.panels.active().id;
        let due = self
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
        self.panels.activate(active_id);
    }

    fn dispatch_navigation_query_target(
        &mut self,
        panel_id: PanelId,
        target: NavigationQueryTarget,
    ) {
        let Some(panel) = self.panels.get(panel_id) else {
            return;
        };
        if panel.generation_clock.current().get() != target.generation
            || NavigationTargetFingerprint::from_viewport(panel.viewport) != target.fingerprint
        {
            return;
        }
        self.panels.activate(panel_id);
        self.dispatch_alignment_query();
        self.dispatch_reference_query();
        self.dispatch_track_query();
        self.try_build_complete_horizon_preview(panel_id, target);
    }

    pub(crate) fn panel_navigation_data_is_settled(&self, panel_id: PanelId) -> bool {
        let Some(panel) = self.panels.get(panel_id) else {
            return false;
        };
        if panel.navigation_query_schedule.pending.is_some()
            || panel.pending_queries.alignment
            || panel.pending_queries.reference
            || panel.pending_queries.tracks
        {
            return false;
        }
        let panel_generation = panel.generation_clock.current().get();
        let alignments_settled = self
            .alignment_tracks
            .iter()
            .all(|track| match track.lifecycle {
                AlignmentTrackLifecycle::Opening => false,
                AlignmentTrackLifecycle::Ready => track
                    .projection(panel_id)
                    .is_some_and(AlignmentPanelProjection::query_is_current),
                AlignmentTrackLifecycle::Failed(_) => true,
            });
        let reference_settled = self.reference_engine.is_none()
            || panel.viewport.bases_per_pixel() > MAXIMUM_BASE_MARK_BASES_PER_PIXEL
            || panel.reference_query_error_generation == Some(panel_generation)
            || (panel.reference_settled_generation == Some(panel_generation)
                && panel.reference_slice.as_ref().is_some_and(|slice| {
                    slice.interval.contains_interval(panel.viewport.interval())
                }));
        let tracks_settled = self.track_engine.is_none()
            || panel.track_query_error_generation == Some(panel_generation)
            || (panel.track_settled_generation == Some(panel_generation)
                && panel.track_snapshot_interval == Some(panel.viewport.interval()));
        alignments_settled && reference_settled && tracks_settled
    }

    fn update_navigation_readiness(&mut self) {
        let ready = self
            .panels
            .iter()
            .filter(|panel| {
                matches!(
                    panel.navigation_presentation.phase,
                    NavigationPresentationPhase::Preview
                        | NavigationPresentationPhase::CheckingWithoutPreview
                ) && !panel.navigation_query_schedule.hold_commit_until_end
                    && self.panel_navigation_data_is_settled(panel.id)
            })
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        if ready.is_empty() {
            return;
        }
        for panel_id in ready {
            if let Some(panel) = self.panels.get_mut(panel_id) {
                panel.navigation_presentation.mark_ready_to_commit();
            }
        }
        self.display_dirty = true;
        self.window.request_redraw();
    }

    pub(crate) fn begin_ready_navigation_commits(&mut self) -> bool {
        let mut changed = false;
        for panel in self.panels.iter_mut() {
            changed |= panel.navigation_presentation.begin_commit();
        }
        changed
    }

    pub(crate) fn finish_navigation_commits(&mut self) -> bool {
        let now = Instant::now();
        let mut committed = false;
        for panel in self.panels.iter_mut() {
            let elapsed = now.saturating_duration_since(panel.navigation_presentation.started_at);
            if panel.navigation_presentation.finish_commit(
                self.workspace_layout.revision,
                self.navigation_contract_revision,
            ) {
                committed = true;
                panel.navigation_provisional_display = None;
                panel.navigation_query_schedule.record_commit(elapsed);
            }
        }
        committed
    }

    pub(crate) fn invalidate_navigation_preview_contract(&mut self, reason: &str) {
        self.navigation_contract_revision = self.navigation_contract_revision.saturating_add(1);
        for panel in self.panels.iter_mut() {
            panel
                .navigation_presentation
                .invalidate_preview(reason.to_owned());
            panel.navigation_provisional_display = None;
        }
        self.discard_navigation_preview_texture();
    }

    pub(crate) fn discard_navigation_preview_texture(&mut self) {
        self.navigation_texture_revisions.clear();
        self.navigation_preview_renderer.invalidate_texture();
    }

    /// Starts an atomic same-locus refresh after a query/presentation contract
    /// change. The old pixels are deliberately ineligible for transformation,
    /// so the panel uses the neutral checking state until all replacement data
    /// can commit in one frame.
    pub(crate) fn begin_all_panel_refresh_transactions(&mut self) {
        let now = Instant::now();
        let source_status = self.data_status.clone();
        for panel in self.panels.iter_mut() {
            panel.navigation_query_schedule.cancel();
            panel.navigation_provisional_display = None;
            panel.navigation_presentation.begin_navigation(
                panel.viewport,
                panel.generation_clock.current().get(),
                self.workspace_layout.revision,
                self.navigation_contract_revision,
                &source_status,
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
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            let resolution = AlignmentQueryResolution::for_viewport(self.panels.active().viewport);
            let alignment_refresh_required = self.alignment_tracks.iter().any(|track| {
                matches!(track.lifecycle, AlignmentTrackLifecycle::Ready)
                    && track.projection(id).is_none_or(|projection| {
                        projection
                            .last_query_resolution
                            .is_none_or(|previous| resolution.requires_refresh_from(previous))
                    })
            });
            if alignment_refresh_required {
                self.retain_active_read_selection_for_query_refresh();
                self.advance_alignment_generations(id);
                self.request_alignment_query();
                source_refresh_requested = true;
            }
            let reference_missing = self.reference_engine.is_some()
                && self.panels.active().viewport.bases_per_pixel()
                    <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL
                && self
                    .panels
                    .active()
                    .reference_slice
                    .as_ref()
                    .is_none_or(|slice| {
                        !slice
                            .interval
                            .contains_interval(self.panels.active().viewport.interval())
                    });
            if reference_missing {
                self.request_reference_query();
                source_refresh_requested = true;
            }
        }
        self.panels.activate(active_id);
        if source_refresh_requested {
            self.resize_query_refreshes = self.resize_query_refreshes.saturating_add(1);
        }
    }

    pub(crate) fn request_track_query(&mut self) {
        self.panels.active_mut().pending_queries.tracks = self.track_engine.is_some();
        self.dispatch_track_query();
    }

    pub(crate) fn request_reference_query(&mut self) {
        let base_marks_visible = self.panels.active_mut().viewport.bases_per_pixel()
            <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL;
        self.panels.active_mut().pending_queries.reference =
            self.reference_engine.is_some() && base_marks_visible;
        self.dispatch_reference_query();
    }

    pub(crate) fn dispatch_reference_query(&mut self) {
        if !self.panels.active().pending_queries.reference {
            return;
        }
        let Some(engine) = &self.reference_engine else {
            self.panels.active_mut().pending_queries.reference = false;
            return;
        };
        let request = self.panels.active().request();
        match engine.try_query(
            request,
            self.panels.active_mut().generation_clock.clone(),
            self.panels.active_mut().contig_name.clone(),
            self.panels.active_mut().viewport.interval(),
        ) {
            Ok(()) => self.panels.active_mut().pending_queries.reference = false,
            Err(EngineQueueError::Full) => {
                "Reference queue busy; retrying the latest locus…"
                    .clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.reference = false;
                self.data_status = EngineQueueError::Disconnected.to_string();
            }
        }
    }

    pub(crate) fn request_all_panel_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_alignment_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.retain_active_read_selection_for_query_refresh();
            self.request_alignment_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn invalidate_and_request_all_auxiliary_track_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            self.panels.activate(id);
            self.panels.active().generation_clock.advance();
            self.request_reference_query();
            self.request_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn dispatch_track_query(&mut self) {
        if !self.panels.active().pending_queries.tracks {
            return;
        }
        let Some(engine) = &self.track_engine else {
            self.panels.active_mut().pending_queries.tracks = false;
            return;
        };
        let interval = self.panels.active().viewport.interval();
        let request = self.panels.active().request();
        let track_ids = auxiliary_query_track_ids(self.tracks.tracks());
        let queried = track_ids.iter().copied().collect::<HashSet<_>>();
        let variant_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant && queried.contains(&track.id))
            .map(|track| (track.id, track.variant.clone()))
            .collect();
        let interaction_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction && queried.contains(&track.id))
            .map(|track| (track.id, track.interaction.clone()))
            .collect();
        let copy_number_settings = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber && queried.contains(&track.id))
            .map(|track| (track.id, track.copy_number.clone()))
            .collect();
        match engine.try_query_selected_with_settings(
            request,
            self.panels.active_mut().generation_clock.clone(),
            self.panels.active_mut().contig_name.clone(),
            interval.start(),
            interval.end(),
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        ) {
            Ok(()) => self.panels.active_mut().pending_queries.tracks = false,
            Err(EngineQueueError::Full) => {
                "Track queue busy; retrying the latest locus…".clone_into(&mut self.data_status);
            }
            Err(EngineQueueError::Disconnected) => {
                self.panels.active_mut().pending_queries.tracks = false;
                self.data_status = EngineQueueError::Disconnected.to_string();
            }
        }
    }

    pub(crate) fn retry_pending_queries(&mut self) {
        let active_id = self.panels.active().id;
        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for id in ids {
            let alignment_pending = self.alignment_tracks.iter().any(|track| {
                track
                    .projection(id)
                    .is_some_and(|projection| projection.pending_query)
            });
            self.panels.activate(id);
            if self
                .panels
                .active()
                .navigation_query_schedule
                .pending
                .is_some()
            {
                continue;
            }
            self.panels.active_mut().pending_queries.alignment |= alignment_pending;
            self.dispatch_alignment_query();
            self.dispatch_reference_query();
            self.dispatch_track_query();
        }
        self.panels.activate(active_id);
    }

    pub(crate) fn poll_background_work(&mut self) {
        let now = Instant::now();
        self.dispatch_due_navigation_queries(now);
        self.dispatch_due_resolution_query_refresh(now);
        self.poll_reference_events();
        self.poll_alignment_events();
        self.poll_track_events();
        self.poll_alignment_retirement_tasks();
        self.poll_session_load_tasks();
        self.poll_session_save_tasks();
        self.poll_export_tasks();
        self.poll_workspace_list_import_tasks();
        self.retry_pending_queries();
        self.update_navigation_readiness();
        if self.reveal_due_alignment_checking_notices(now)
            || self.reveal_due_navigation_notices(now)
        {
            self.display_dirty = true;
        }
        if self.display_dirty {
            self.window.request_redraw();
        }
    }

    fn reveal_due_navigation_notices(&mut self, now: Instant) -> bool {
        self.panels.iter_mut().fold(false, |changed, panel| {
            panel.navigation_presentation.reveal_notice_if_due(now) || changed
        })
    }

    fn reveal_due_alignment_checking_notices(&mut self, now: Instant) -> bool {
        self.alignment_tracks
            .iter_mut()
            .flat_map(|track| track.projections.values_mut())
            .fold(false, |changed, projection| {
                projection.reveal_checking_notice_if_due(now) || changed
            })
    }

    pub(crate) fn poll_alignment_retirement_tasks(&mut self) {
        let mut index = 0;
        while index < self.alignment_retirement_tasks.len() {
            if !self.alignment_retirement_tasks[index].is_finished() {
                index += 1;
                continue;
            }
            let task = self.alignment_retirement_tasks.remove(index);
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked");
            }
        }
    }

    pub(crate) fn finish_alignment_retirement_tasks(&mut self) {
        for task in self.alignment_retirement_tasks.drain(..) {
            if task.join().is_err() {
                tracing::warn!("alignment reaper panicked during shutdown");
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_reference_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.reference_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.data_status = error.to_string();
                        break;
                    }
                }
            }
        }
        if events.is_empty() {
            return;
        }
        let active_panel_id = self.panels.active().id;
        for event in events {
            match event {
                ReferenceEngineEvent::Opened {
                    dictionary,
                    remote_io_stats,
                } => {
                    let reference_contig_count = dictionary.len();
                    self.reference_open_error = None;
                    self.reference_remote_io_stats = remote_io_stats;
                    self.reference_dictionary = Some(dictionary.clone());
                    if self.primary_alignment_dictionary().is_none() {
                        let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                        for id in ids {
                            let (requested, pixel_width) = self
                                .panels
                                .get(id)
                                .map(|panel| {
                                    (panel.requested_locus.clone(), panel.viewport.pixel_width())
                                })
                                .expect("panel id came from the panel collection");
                            match resolve_initial_locus(
                                &dictionary,
                                &self.contig_aliases,
                                requested.as_deref(),
                            )
                            .and_then(|(interval, name, length)| {
                                Ok((
                                    Viewport::new(interval, pixel_width)?,
                                    interval,
                                    name,
                                    length,
                                ))
                            }) {
                                Ok((viewport, interval, name, length)) => {
                                    let panel = self
                                        .panels
                                        .get_mut(id)
                                        .expect("panel id came from the panel collection");
                                    panel.generation_clock.advance();
                                    panel.viewport = viewport;
                                    panel.contig_name = name;
                                    panel.contig_length = length;
                                    panel.home_interval = interval;
                                    panel.navigation_history = NavigationHistory::new(interval);
                                    panel.vertical_scroll_points = 0.0;
                                    panel.vertical_content_origin_points = 0.0;
                                    panel.latent_vertical_scroll_fraction = None;
                                    panel.reference_slice = None;
                                }
                                Err(error) if id == active_panel_id => {
                                    self.data_status = error.to_string();
                                }
                                Err(_) => {}
                            }
                        }
                        let default_annotation_count = self.assembly.default_annotation_count;
                        self.assembly = load_assembly(
                            &self.assembly_name,
                            dictionary,
                            self.cytoband_path.as_deref(),
                        );
                        self.assembly.default_annotation_count = default_annotation_count;
                    }
                    self.assembly.reference_contig_count = reference_contig_count;
                    self.assembly.identity.clone_from(&self.assembly_identity);
                    self.synthetic = false;
                    if !self.has_alignment_engine() {
                        let source = self.reference_source.as_ref().map_or_else(
                            || "reference genome".to_owned(),
                            TrackSource::redacted_label,
                        );
                        self.data_status = format!("Opened {source}");
                    }
                    self.refresh_active_panel_context();
                    self.request_all_panel_queries();
                    self.display_dirty = true;
                }
                ReferenceEngineEvent::QueryCompleted {
                    request,
                    slice,
                    remote_io_stats,
                } => {
                    self.reference_remote_io_stats = remote_io_stats;
                    let is_active = request.panel_id() == active_panel_id;
                    let mut accepted = false;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                        && slice.interval.contains_interval(panel.viewport.interval())
                    {
                        accepted = true;
                        panel.reference_slice = Some(slice);
                        panel.reference_settled_generation = Some(request.generation().get());
                        panel.reference_query_error_generation = None;
                        self.display_dirty = true;
                        if is_active && !self.has_alignment_engine() {
                            "Reference sequence loaded".clone_into(&mut self.data_status);
                        }
                    }
                    if !accepted {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                    }
                }
                ReferenceEngineEvent::Error { request, message } => {
                    if request.is_none() {
                        self.reference_open_error = Some(message.clone());
                    }
                    let current = request.is_none_or(|request| {
                        self.panels
                            .get(request.panel_id())
                            .is_some_and(|panel| panel.accepts(request))
                    });
                    if request.is_some() && !current {
                        self.navigation_stale_result_rejections =
                            self.navigation_stale_result_rejections.saturating_add(1);
                    }
                    if current {
                        if let Some(request) = request
                            && let Some(panel) = self.panels.get_mut(request.panel_id())
                        {
                            panel.reference_settled_generation = Some(request.generation().get());
                            panel.reference_query_error_generation =
                                Some(request.generation().get());
                        }
                        self.data_status = message;
                    }
                }
            }
        }
        self.window.request_redraw();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_alignment_events(&mut self) {
        let mut events = Vec::new();
        let active_panel_id = self.panels.active().id;
        let primary_id = self.alignment_tracks.first().map(|track| track.config.id);
        for track in &mut self.alignment_tracks {
            let id = track.config.id;
            loop {
                match track.engine.try_receive() {
                    Ok(Some(event)) => events.push((id, event)),
                    Ok(None) => break,
                    Err(error) => {
                        track.lifecycle = AlignmentTrackLifecycle::Failed(error.to_string());
                        if Some(id) == primary_id {
                            self.data_status = error.to_string();
                        }
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for (track_id, event) in events {
            let mut event_is_current = true;
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
            {
                match &event {
                    AlignmentEngineEvent::Opened {
                        dictionary,
                        index_overview,
                    } => {
                        track.lifecycle = AlignmentTrackLifecycle::Ready;
                        track.dictionary = Some(dictionary.clone());
                        track.index_overview.clone_from(index_overview);
                        for projection in track.projections.values_mut() {
                            projection.pending_query = true;
                        }
                    }
                    AlignmentEngineEvent::QueryCompleted {
                        request,
                        query,
                        layout_query,
                        layout_cache_hit,
                        cache_stats,
                        remote_io_stats,
                        ..
                    } => {
                        let projection = track.projection_mut(request.panel_id());
                        event_is_current = projection.clock.is_current(request.generation());
                        if event_is_current {
                            projection.finish_checking();
                            projection.query_stats = Some(query.stats);
                            projection.current_query = Some(Arc::clone(query));
                            projection.layout_query = Some(Arc::clone(layout_query));
                            projection.layout_query_contract_revision =
                                self.navigation_contract_revision;
                            projection.layout_query_cache_hit = *layout_cache_hit;
                            projection.unavailable = None;
                            projection.index_only_overview = false;
                            projection.settled_generation = Some(request.generation().get());
                            track.cache_stats = Some(*cache_stats);
                            track.remote_io_stats = *remote_io_stats;
                        }
                    }
                    AlignmentEngineEvent::Error { request, message } => {
                        event_is_current = request.is_none_or(|request| {
                            track
                                .projection(request.panel_id())
                                .is_some_and(|projection| {
                                    projection.clock.is_current(request.generation())
                                })
                        });
                        if event_is_current {
                            if let Some(request) = request {
                                track.projection_mut(request.panel_id()).finish_checking();
                            }
                            track.lifecycle = AlignmentTrackLifecycle::Failed(message.clone());
                        }
                    }
                    AlignmentEngineEvent::CacheCleared { cache_stats } => {
                        track.cache_stats = Some(*cache_stats);
                    }
                }
            }
            if !event_is_current
                && matches!(
                    &event,
                    AlignmentEngineEvent::QueryCompleted { .. }
                        | AlignmentEngineEvent::Error {
                            request: Some(_),
                            ..
                        }
                )
            {
                self.navigation_stale_result_rejections =
                    self.navigation_stale_result_rejections.saturating_add(1);
            }
            if matches!(event, AlignmentEngineEvent::Opened { .. }) {
                self.panels.active_mut().pending_queries.alignment = true;
            }
            if event_is_current && matches!(event, AlignmentEngineEvent::QueryCompleted { .. }) {
                // Results for inactive panels and secondary tracks still alter
                // the composite workspace even when they do not change the
                // active status line.
                self.display_dirty = true;
            }
            if Some(track_id) != primary_id {
                if event_is_current {
                    match &event {
                        AlignmentEngineEvent::QueryCompleted { request, query, .. } => {
                            self.resolve_pending_read_selection(
                                request.panel_id(),
                                track_id,
                                query,
                            );
                        }
                        AlignmentEngineEvent::Error { request, message }
                            if request
                                .is_none_or(|request| request.panel_id() == active_panel_id) =>
                        {
                            let name = self
                                .alignment_tracks
                                .iter()
                                .find(|track| track.config.id == track_id)
                                .map_or("Alignment track", |track| track.config.name.as_str());
                            self.data_status = format!("{name}: {message}");
                        }
                        _ => {}
                    }
                }
                continue;
            }
            match event {
                AlignmentEngineEvent::Opened { dictionary, .. } => {
                    if let Some(source) = self.primary_alignment_source().cloned() {
                        self.settings.remember_alignment(&source);
                        self.persist_settings();
                    }
                    let ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
                    for id in ids {
                        let (requested, pixel_width) = self
                            .panels
                            .get(id)
                            .map(|panel| {
                                (panel.requested_locus.clone(), panel.viewport.pixel_width())
                            })
                            .expect("panel id came from the panel collection");
                        match resolve_initial_locus(
                            &dictionary,
                            &self.contig_aliases,
                            requested.as_deref(),
                        )
                        .and_then(|(interval, name, length)| {
                            Ok((
                                Viewport::new(interval, pixel_width)?,
                                interval,
                                name,
                                length,
                            ))
                        }) {
                            Ok((viewport, interval, name, length)) => {
                                let panel = self
                                    .panels
                                    .get_mut(id)
                                    .expect("panel id came from the panel collection");
                                panel.viewport = viewport;
                                panel.contig_name = name;
                                panel.contig_length = length;
                                panel.home_interval = interval;
                                panel.navigation_history = NavigationHistory::new(interval);
                                panel.vertical_scroll_points = 0.0;
                                panel.vertical_content_origin_points = 0.0;
                                panel.latent_vertical_scroll_fraction = None;
                            }
                            Err(error) if id == active_panel_id => {
                                self.data_status = error.to_string();
                            }
                            Err(_) => {}
                        }
                    }
                    let default_annotation_count = self.assembly.default_annotation_count;
                    self.assembly = load_assembly(
                        &self.assembly_name,
                        dictionary.clone(),
                        self.cytoband_path.as_deref(),
                    );
                    self.assembly.default_annotation_count = default_annotation_count;
                    self.assembly.reference_contig_count = self
                        .reference_dictionary
                        .as_ref()
                        .map_or(dictionary.len(), ReferenceDictionary::len);
                    self.assembly.identity.clone_from(&self.assembly_identity);
                    self.request_all_panel_queries();
                }
                AlignmentEngineEvent::QueryCompleted {
                    request,
                    query,
                    layout_query,
                    cache_hit,
                    cache_stats,
                    remote_io_stats,
                    ..
                } if event_is_current => {
                    self.suggested_assembly_identity =
                        rgv_hts::recognize_assembly_from_header(&query.header);
                    let is_active = request.panel_id() == active_panel_id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                        let has_pending_selection = panel
                            .pending_read_selection
                            .as_ref()
                            .is_some_and(|pending| pending.track_id == track_id);
                        panel.selected_record = None;
                        panel.selected_read_event = None;
                        if !has_pending_selection {
                            panel.selected_alignment_track = None;
                        }
                    }
                    self.resolve_pending_read_selection(request.panel_id(), track_id, &query);
                    if !is_active {
                        continue;
                    }
                    let cache_label = if cache_hit { "cache hit · " } else { "" };
                    let refusal = alignment::individual_read_refusal(
                        &query,
                        &layout_query,
                        self.primary_alignment_filters().read_stacking_mode,
                    );
                    self.data_status = if let Some(refusal) = refusal {
                        format!("{cache_label}{}", alignment::read_refusal_detail(refusal))
                    } else {
                        format!(
                            "{cache_label}{} complete alignments",
                            query.stats.records_emitted
                        )
                    };
                    if self
                        .panels
                        .active()
                        .navigation_presentation
                        .permits_staged_display()
                    {
                        self.rebuild_genomic_display();
                    }
                    self.display_dirty = true;
                }
                AlignmentEngineEvent::Error { request, message }
                    if event_is_current
                        && request.is_none_or(|request| request.panel_id() == active_panel_id) =>
                {
                    self.data_status = message;
                }
                AlignmentEngineEvent::CacheCleared { .. } => {
                    "Alignment cache cleared".clone_into(&mut self.data_status);
                }
                AlignmentEngineEvent::QueryCompleted { .. }
                | AlignmentEngineEvent::Error { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn poll_track_events(&mut self) {
        let mut events = Vec::new();
        if let Some(engine) = &self.track_engine {
            loop {
                match engine.try_receive() {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => break,
                    Err(error) => {
                        self.data_status = error.to_string();
                        break;
                    }
                }
            }
        }
        let had_events = !events.is_empty();
        for event in events {
            let stale_query_result = match &event {
                TrackEngineEvent::QueryCompleted { request, .. }
                | TrackEngineEvent::Error {
                    request: Some(request),
                    ..
                } => self
                    .panels
                    .get(request.panel_id())
                    .is_none_or(|panel| !panel.accepts(*request)),
                _ => false,
            };
            if stale_query_result {
                self.navigation_stale_result_rejections =
                    self.navigation_stale_result_rejections.saturating_add(1);
            }
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
                    if !self.synthetic {
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
                    for panel in self.panels.iter_mut() {
                        clear_removed_auxiliary_panel_state(panel, &removed);
                    }
                    self.annotation_search_results
                        .hits
                        .retain(|hit| !removed.contains(&hit.track_id));
                    self.display_dirty = true;
                }
                TrackEngineEvent::TdfChannelsDiscovered {
                    id,
                    track_names,
                    selected_index,
                } => {
                    if let Err(error) = self.expand_tdf_channels(id, &track_names, selected_index) {
                        self.data_status = error.to_string();
                    }
                }
                TrackEngineEvent::QueryCompleted { request, tracks } => {
                    let is_active = request.panel_id() == self.panels.active().id;
                    if let Some(panel) = self.panels.get_mut(request.panel_id())
                        && panel.accepts(request)
                    {
                        panel.track_snapshots = tracks;
                        panel.track_snapshot_interval = Some(panel.viewport.interval());
                        panel.track_settled_generation = Some(request.generation().get());
                        panel.track_query_error_generation = None;
                        panel.selected_feature = None;
                        panel.selected_variant = None;
                        panel.selected_gwas = None;
                        panel.selected_quantitative = None;
                        panel.selected_copy_number = None;
                        panel.selected_mutation = None;
                        panel.selected_interaction = None;
                        self.display_dirty = true;
                        if is_active
                            && self
                                .panels
                                .active()
                                .navigation_presentation
                                .permits_staged_display()
                        {
                            self.rebuild_genomic_display();
                        }
                    }
                }
                TrackEngineEvent::SearchCompleted {
                    generation,
                    results,
                } if generation == self.annotation_search_generation => {
                    let pending = self
                        .pending_annotation_navigation
                        .take()
                        .filter(|query| query.eq_ignore_ascii_case(results.query.trim()));
                    self.annotation_search_results = results;
                    if let Some(query) = pending {
                        self.submit_annotation_search_results(&query);
                    }
                    self.display_dirty = true;
                }
                TrackEngineEvent::AdjacentFeatureCompleted { generation, hit }
                    if generation == self.annotation_navigation_generation =>
                {
                    if let Some(hit) = hit {
                        self.navigate_to_annotation_hit(&hit);
                    } else {
                        "No further annotation feature on this contig"
                            .clone_into(&mut self.data_status);
                    }
                }
                TrackEngineEvent::Error {
                    request,
                    track: _,
                    message,
                } => {
                    if let Some(request) = request {
                        let is_active = request.panel_id() == self.panels.active().id;
                        let accepted = self
                            .panels
                            .get(request.panel_id())
                            .is_some_and(|panel| panel.accepts(request));
                        if accepted {
                            if let Some(panel) = self.panels.get_mut(request.panel_id()) {
                                panel.track_query_error_generation =
                                    Some(request.generation().get());
                            }
                            self.display_dirty = true;
                            if is_active {
                                self.data_status = message;
                            }
                        }
                    } else {
                        self.data_status = message;
                    }
                }
                TrackEngineEvent::SearchCompleted { .. }
                | TrackEngineEvent::AdjacentFeatureCompleted { .. } => {}
            }
        }
        if had_events {
            self.window.request_redraw();
        }
    }
}
