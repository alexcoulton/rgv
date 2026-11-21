use crate::Viewer;
use crate::workspace::alignment_management::scene_authority::retire_reference_install_receipt_for_contract_epoch;
use crate::workspace::alignment_navigation::AlignmentNavigationPhase;
use crate::workspace::alignments::{
    AlignmentSceneResolution, AlignmentTrackLifecycle, alignment_scene_build_contract,
    alignment_scene_contract_equivalent, alignment_scene_preparation_policy,
};
use rgv_alignment_scene::AlleleSummaryDetail;
use rgv_core::PanelId;
use rgv_engine::{EngineQueueError, LocalBamSourceControl};
use std::num::NonZeroUsize;
use std::time::Instant;

impl Viewer {
    /// Begins the sole liveness transaction for one panel/track target. Callers which already
    /// advanced the projection clock (ordinary viewport navigation) pass `advance_generation =
    /// false`. Contract changes advance explicitly at their call site; row-camera-only changes
    /// keep the same immutable source generation and receive only a new request token.
    pub(crate) fn queue_alignment_navigation_for_track(
        &mut self,
        track_index: usize,
        panel_id: PanelId,
        advance_generation: bool,
        reason: &'static str,
    ) {
        if track_index >= self.session.alignment_tracks.len() {
            return;
        }
        let failed = matches!(
            self.session.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Failed(_)
        );
        if failed {
            let projection = self.session.alignment_tracks[track_index].projection_mut(panel_id);
            projection.navigation.transition(
                AlignmentNavigationPhase::Failed,
                "alignment source is already failed",
            );
            return;
        }
        if advance_generation {
            self.session.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .advance_scene_generation();
        }
        let token = self.take_alignment_scene_request_token();
        let track_id = self.session.alignment_tracks[track_index].config.id;
        let projection = self.session.alignment_tracks[track_index].projection_mut(panel_id);
        let generation = projection.clock.current().get();
        projection.navigation.bind_identity(panel_id, track_id);
        projection.navigation.begin(generation, token, reason);
        projection.scene_pressure_retry = None;
    }

    /// Sends the UI's current camera to the unified source actor and changes no pixels itself.
    /// Existing immutable scene pages are captured against this camera immediately by
    /// `prepare_alignment_scene_frame`; source acquisition remains asynchronous and latest-wins.
    pub(crate) fn dispatch_alignment_query(&mut self, panel_id: PanelId) {
        if !self
            .session
            .panels
            .panel(panel_id)
            .pending_queries
            .alignment
        {
            return;
        }
        if self.session.alignment_tracks.is_empty() {
            self.session
                .panels
                .panel_mut(panel_id)
                .pending_queries
                .alignment = false;
            return;
        }
        let viewport = self.session.panels.panel(panel_id).viewport;
        let resolution = AlignmentSceneResolution::for_viewport(viewport);
        if !resolution.requires_alignment_records() {
            let mut settled = 0_usize;
            let mut retired_tracks = Vec::new();
            for track in &mut self.session.alignment_tracks {
                match &track.lifecycle {
                    AlignmentTrackLifecycle::Opening => {}
                    AlignmentTrackLifecycle::Ready => {
                        let track_id = track.config.id;
                        let projection = track.projection_mut(panel_id);
                        // Index overview is a presentation decision, not source retirement. Keep
                        // the immutable lane cached so zooming back can reuse it immediately; it
                        // is excluded from capture while `index_only_overview` is true.
                        projection.scene_pressure_retry = None;
                        projection.settle_index_only_overview(resolution);
                        retired_tracks.push(track_id);
                        settled = settled.saturating_add(1);
                    }
                    AlignmentTrackLifecycle::Failed(_) => {
                        let projection = track.projection_mut(panel_id);
                        projection.index_only_overview = false;
                        projection.navigation.transition(
                            AlignmentNavigationPhase::Failed,
                            "alignment source failed before index overview",
                        );
                    }
                }
            }
            let primary_track_id = self.primary_alignment_track_id();
            for track_id in retired_tracks {
                let panel = self.session.panels.panel_mut(panel_id);
                panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .retire_scene_authority();
                if panel
                    .pending_alignment_reveal
                    .is_some_and(|(pending_track, _)| pending_track == track_id)
                {
                    panel.pending_alignment_reveal = None;
                }
                if track_id == primary_track_id {
                    panel.vertical_scroll_points = 0.0;
                    panel.vertical_content_origin_points = 0.0;
                    panel.latent_vertical_scroll_fraction = None;
                    panel.content_bottom_points = 0.0;
                }
            }
            self.session
                .panels
                .panel_mut(panel_id)
                .pending_queries
                .alignment = self.session.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
            if settled > 0 {
                self.session.data_status = format!(
                    "Overview for {settled} alignment track(s): coverage only, reads are not loaded"
                );
            }
            self.prepare_alignment_scene_frame();
            return;
        }
        let contig_name = self.session.panels.panel(panel_id).contig_name.clone();
        let mut sent = 0_usize;
        for track_index in 0..self.session.alignment_tracks.len() {
            if matches!(
                self.session.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Opening
            ) {
                continue;
            }
            if matches!(
                self.session.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Failed(_)
            ) {
                let projection =
                    self.session.alignment_tracks[track_index].projection_mut(panel_id);
                projection.navigation.transition(
                    AlignmentNavigationPhase::Failed,
                    "alignment source is disconnected",
                );
                continue;
            }
            if !self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
            {
                continue;
            }
            let Some(metadata) = self.session.alignment_tracks[track_index]
                .scene_metadata
                .clone()
            else {
                continue;
            };
            let Some((contig, contig_length)) = metadata
                .dictionary
                .id_by_name(&contig_name)
                .and_then(|contig| {
                    metadata
                        .dictionary
                        .get(contig)
                        .map(|entry| (contig, entry.length()))
                })
            else {
                let message = format!(
                    "{} is not available in this alignment",
                    self.session.panels.panel(panel_id).contig_name
                );
                self.session.alignment_tracks[track_index].mark_unavailable(panel_id, message);
                continue;
            };
            let track_id = self.session.alignment_tracks[track_index].config.id;
            let existing_owner = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_owner);
            let existing_contig = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contig);
            let existing_contract = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_contract.as_ref())
                .cloned();
            let existing_policy = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_policy);
            let target_viewport = match viewport.remapped_contig(contig, contig_length) {
                Ok(viewport) => viewport,
                Err(_) => continue,
            };
            let old_atlas_interval = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.scene_frame.as_ref())
                .and_then(|frame| frame.snapshot.as_ref())
                .map(|snapshot| snapshot.reads().atlas().interval());
            let contig_changed = existing_contig.is_some_and(|current| current != contig);
            let atlas_disjoint = old_atlas_interval.is_some_and(|interval| {
                interval.contig() != contig || !interval.overlaps(target_viewport.interval())
            });
            let authority_discontinuity = contig_changed || atlas_disjoint;
            let primary_track_id = self.primary_alignment_track_id();
            let scroll_reset = {
                let panel = self.session.panels.panel_mut(panel_id);
                let reset = panel
                    .read_panel_scrolls
                    .entry(track_id)
                    .or_default()
                    .bind_scene_contig(contig, authority_discontinuity);
                if reset {
                    panel.pending_alignment_reveal = None;
                    if track_id == primary_track_id {
                        panel.vertical_scroll_points = 0.0;
                        panel.vertical_content_origin_points = 0.0;
                        panel.latent_vertical_scroll_fraction = None;
                        panel.content_bottom_points = 0.0;
                    }
                }
                reset
            };
            if authority_discontinuity || scroll_reset {
                let projection =
                    self.session.alignment_tracks[track_index].projection_mut(panel_id);
                projection.scene_frame = None;
                projection.scene_frame_request_token = None;
                projection.scene_pressure_retry = None;
                if let Some(store) = self.alignment_panel_scene_stores.get_mut(&panel_id)
                    && let Err(error) = store.clear_lane(track_id)
                {
                    self.report_error(format!("Could not reset alignment scene lane: {error}"));
                }
            }
            let promoted_template = self
                .promoted_read
                .filter(|(candidate, _)| *candidate == track_id)
                .map(|(_, hash)| hash);
            let reference_revision =
                self.compatible_alignment_reference_revision(&metadata.dictionary);
            let contract = alignment_scene_build_contract(
                &self.session.alignment_tracks[track_index].config,
                &self.session.alignment_tracks[track_index].active_filters,
                &metadata,
                contig,
                contig_length,
                viewport,
                promoted_template,
                reference_revision,
            );
            let reference_backed_alleles = contract.summaries.allele_detail
                != AlleleSummaryDetail::None
                && contract.presentation.reference.is_some();
            let lane_capture = match self.alignment_scene_lane_capture_for_panel_track(
                panel_id,
                track_id,
                contig,
                contig_length,
                reference_backed_alleles,
            ) {
                Ok(Some(capture)) => capture,
                Ok(None) => {
                    // Layout is committed independently of source opening. Keep this request
                    // pending until at least one real read or derived lane exists; never invent a
                    // render clip merely to force source work.
                    continue;
                }
                Err(error) => {
                    self.report_error(format!("Could not resolve alignment scene lanes: {error}"));
                    continue;
                }
            };
            let preparation_camera = lane_capture.preparation_camera;
            let policy = alignment_scene_preparation_policy(
                self.session.settings.read_browsing_buffer_percent,
                preparation_camera,
            );
            let complete_read_limit = self.session.alignment_tracks[track_index]
                .config
                .effective_complete_read_limit();
            let mut queue_error = None;
            let must_insert = existing_owner.is_none() || existing_contig != Some(contig);
            let owner = if must_insert {
                self.take_alignment_scene_owner()
            } else {
                existing_owner.expect("an existing alignment scene lane has an owner")
            };
            let request_token = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .and_then(|projection| projection.navigation.request_token())
                .expect("a queued alignment transaction has a request token");
            self.session.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .navigation
                .bind_camera(preparation_camera);
            let contract_changed = existing_contract
                .as_ref()
                .is_none_or(|existing| !alignment_scene_contract_equivalent(existing, &contract));
            let previous_view_filter = existing_contract.as_ref().map(|existing| {
                existing
                    .view_filter
                    .evaluation_fingerprint(existing.presentation.reference)
                    .get()
            });
            let previous_highlight_filter = existing_contract.as_ref().map(|existing| {
                existing
                    .highlight_filter
                    .evaluation_fingerprint(existing.presentation.reference)
                    .get()
            });
            let requested_view_filter = contract
                .view_filter
                .evaluation_fingerprint(contract.presentation.reference)
                .get();
            let requested_highlight_filter = contract
                .highlight_filter
                .evaluation_fingerprint(contract.presentation.reference)
                .get();
            let filter_authority_changed = previous_view_filter != Some(requested_view_filter)
                || previous_highlight_filter != Some(requested_highlight_filter);
            let source_acquisitions_before = self.session.alignment_tracks[track_index]
                .projection(panel_id)
                .map_or(0, |projection| {
                    projection.scene_telemetry.source_acquisitions
                });
            let limit_changed = self.session.alignment_tracks[track_index]
                .scene_complete_read_limit
                != complete_read_limit;
            let mut limit_queued = false;
            let mut lane_removed = false;
            let mut lane_inserted = false;
            let mut contract_replaced = false;
            let mut policy_replaced = false;
            let Some(engine) = self.session.alignment_tracks[track_index]
                .scene_engine
                .as_ref()
            else {
                let track = &mut self.session.alignment_tracks[track_index];
                track.lifecycle = AlignmentTrackLifecycle::Failed(
                    "Unified alignment scene source is unavailable; no legacy fallback was used"
                        .to_owned(),
                );
                let projection = track.projection_mut(panel_id);
                projection.navigation.transition(
                    AlignmentNavigationPhase::Failed,
                    "unified alignment scene source is unavailable",
                );
                continue;
            };
            if limit_changed {
                match engine.try_set_source_control(LocalBamSourceControl::SetCompleteReadLimit(
                    NonZeroUsize::new(complete_read_limit)
                        .expect("Complete read limit is non-zero"),
                )) {
                    Ok(()) => limit_queued = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && existing_owner.is_some() && existing_contig != Some(contig)
            {
                match engine.try_remove_lane(existing_owner.expect("checked existing owner")) {
                    Ok(()) => lane_removed = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && must_insert {
                match engine.try_insert_lane_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                    policy,
                ) {
                    Ok(()) => lane_inserted = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && contract_changed {
                match engine.try_replace_contract_for_request(
                    owner,
                    request_token,
                    contract.clone(),
                ) {
                    Ok(()) => contract_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none() && !must_insert && existing_policy != Some(policy) {
                match engine.try_set_preparation_policy_for_request(owner, request_token, policy) {
                    Ok(()) => policy_replaced = true,
                    Err(error) => queue_error = Some(error),
                }
            }
            if queue_error.is_none()
                && let Err(error) =
                    engine.try_update_camera_for_request(owner, request_token, preparation_camera)
            {
                queue_error = Some(error);
            }
            {
                let track = &mut self.session.alignment_tracks[track_index];
                if limit_queued {
                    track.scene_complete_read_limit = complete_read_limit;
                }
                let projection = track.projection_mut(panel_id);
                if lane_inserted {
                    projection.scene_owner = Some(owner);
                    projection.scene_optional_work_paused = false;
                    projection.scene_contig = Some(contig);
                    projection.scene_contract = Some(contract.clone());
                    projection.scene_policy = Some(policy);
                } else if lane_removed {
                    // A bounded control queue can accept removal and reject the subsequent insert.
                    // Make the retry state explicit; never pretend the old-contig lane remains.
                    projection.scene_owner = None;
                    projection.scene_optional_work_paused = false;
                    projection.scene_contig = None;
                    projection.scene_contract = None;
                    projection.scene_policy = None;
                }
                if contract_replaced {
                    projection.scene_contract = Some(contract);
                }
                if policy_replaced {
                    projection.scene_policy = Some(policy);
                }
            }
            if contract_replaced && let Some(panel) = self.session.panels.get_mut(panel_id) {
                // The receipt acknowledges delivery to the previous coordinator contract epoch.
                // A replacement may preserve compatible authority, but it may also retire it.
                // Invalidate the receipt in either case so a still-pending exact requirement can
                // enqueue the cached slice again instead of waiting forever for a command which
                // already ran against the old contract.
                retire_reference_install_receipt_for_contract_epoch(
                    &mut panel.reference_installs_queued,
                    owner,
                );
            }
            match queue_error {
                None => {
                    {
                        let projection =
                            self.session.alignment_tracks[track_index].projection_mut(panel_id);
                        projection.begin_checking(Instant::now());
                        projection.last_scene_resolution = Some(resolution);
                        projection.scene_pressure_retry = None;
                        projection.unavailable = None;
                        projection.index_only_overview = false;
                        projection.navigation.transition(
                            AlignmentNavigationPhase::Running,
                            "source worker accepted the exact target",
                        );
                    }
                    if filter_authority_changed && (lane_inserted || contract_replaced) {
                        let filter_state =
                            self.alignment_filter_diagnostic_state(panel_id, track_id);
                        self.diagnostics.event(
                            "alignment_filter",
                            "contract_queued",
                            serde_json::json!({
                                "panel_id": panel_id.get(),
                                "track_id": track_id.get(),
                                "request_token": request_token.get(),
                                "lane_inserted": lane_inserted,
                                "contract_replaced": contract_replaced,
                                "previous_view_filter_evaluation_fingerprint": previous_view_filter,
                                "requested_view_filter_evaluation_fingerprint": requested_view_filter,
                                "previous_highlight_filter_evaluation_fingerprint": previous_highlight_filter,
                                "requested_highlight_filter_evaluation_fingerprint": requested_highlight_filter,
                                "source_acquisitions_before": source_acquisitions_before,
                                "expected_source_reacquisition": false,
                                "filter_state": filter_state,
                            }),
                        );
                    }
                    sent = sent.saturating_add(1);
                }
                Some(EngineQueueError::Full) => {}
                Some(EngineQueueError::Disconnected) => {
                    let track = &mut self.session.alignment_tracks[track_index];
                    let projection = track.projection_mut(panel_id);
                    projection.navigation.transition(
                        AlignmentNavigationPhase::Failed,
                        "alignment source mailbox disconnected",
                    );
                    track.lifecycle =
                        AlignmentTrackLifecycle::Failed(EngineQueueError::Disconnected.to_string());
                }
            }
        }
        self.session
            .panels
            .panel_mut(panel_id)
            .pending_queries
            .alignment = self.session.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
        });
        if sent > 0 {
            self.session.data_status = format!("Updating {sent} alignment scene(s)…");
        } else if self
            .session
            .panels
            .panel(panel_id)
            .pending_queries
            .alignment
        {
            "Alignment queues busy; retrying the latest locus…"
                .clone_into(&mut self.session.data_status);
        }
        // Camera-only movement is visible before the worker produces another event. The capture
        // keeps the latest immutable snapshot and computes its truthful intersection with the
        // live viewport.
        self.prepare_alignment_scene_frame();
    }
}
