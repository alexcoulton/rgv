//! Alignment scene query dispatch: sending each panel's current camera, contract and lane
//! demand to the alignment scene engines. Moved from the desktop's
//! `workspace/alignment_management/dispatch.rs` and `scene_requests.rs`; every presentation
//! effect goes through the [`PresentationHost`].

use crate::alignment::navigation::AlignmentNavigationPhase;
use crate::alignment::track::{
    AlignmentSceneResolution, AlignmentTrackLifecycle, alignment_scene_build_contract,
    alignment_scene_contract_equivalent, alignment_scene_preparation_policy,
};
use crate::panels::Panel;
use crate::presentation_host::PresentationHost;
use crate::session::Session;
use rgv_alignment_scene::{AlleleSummaryDetail, ReferenceRevision};
use rgv_core::{GenomicInterval, PanelId, TrackId};
use rgv_engine::{
    AlignmentSceneRequestToken, EngineQueueError, LocalBamSourceControl, SourceOwnerId,
};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::time::Instant;

/// Monotonic identities for alignment scene lanes and their requests. Neither is ever reused in
/// a run: worker events for a retired lane or request may still be in flight.
#[derive(Clone, Copy, Debug, Default)]
pub struct AlignmentSceneIdentityClock {
    owner: u64,
    request: u64,
}

impl AlignmentSceneIdentityClock {
    pub fn take_owner(&mut self) -> SourceOwnerId {
        self.owner = self
            .owner
            .checked_add(1)
            .expect("alignment scene owner identity space was exhausted");
        SourceOwnerId::new(self.owner)
    }

    pub fn take_request_token(&mut self) -> AlignmentSceneRequestToken {
        self.request = self
            .request
            .checked_add(1)
            .expect("alignment scene request identity space was exhausted");
        AlignmentSceneRequestToken::new(self.request)
    }
}

impl<P: Panel> Session<P> {
    pub fn take_alignment_scene_owner(&mut self) -> SourceOwnerId {
        self.alignment_scene_identities.take_owner()
    }

    pub fn take_alignment_scene_request_token(&mut self) -> AlignmentSceneRequestToken {
        self.alignment_scene_identities.take_request_token()
    }
}

/// Forgets that a reference slice was delivered to `owner`'s previous contract epoch.
pub fn retire_reference_install_receipt_for_contract_epoch(
    receipts: &mut BTreeMap<SourceOwnerId, (ReferenceRevision, GenomicInterval)>,
    owner: SourceOwnerId,
) -> bool {
    receipts.remove(&owner).is_some()
}

impl<P: Panel> Session<P> {
    /// Begins the sole liveness transaction for one panel/track target. Callers which already
    /// advanced the projection clock (ordinary viewport navigation) pass `advance_generation =
    /// false`. Contract changes advance explicitly at their call site; row-camera-only changes
    /// keep the same immutable source generation and receive only a new request token.
    pub fn queue_alignment_navigation_for_track(
        &mut self,
        track_index: usize,
        panel_id: PanelId,
        advance_generation: bool,
        reason: &'static str,
    ) {
        if track_index >= self.alignment_tracks.len() {
            return;
        }
        let failed = matches!(
            self.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Failed(_)
        );
        if failed {
            let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
            projection.navigation.transition(
                AlignmentNavigationPhase::Failed,
                "alignment source is already failed",
            );
            return;
        }
        if advance_generation {
            self.alignment_tracks[track_index]
                .projection_mut(panel_id)
                .advance_scene_generation();
        }
        let token = self.take_alignment_scene_request_token();
        let track_id = self.alignment_tracks[track_index].config.id;
        let projection = self.alignment_tracks[track_index].projection_mut(panel_id);
        let generation = projection.clock.current().get();
        projection.navigation.bind_identity(panel_id, track_id);
        projection.navigation.begin(generation, token, reason);
        projection.scene_pressure_retry = None;
    }

    /// Recomputes `panel_id`'s pending-alignment flag from whether any of its transactions is
    /// still queued for dispatch.
    pub fn mark_alignment_queries_queued(&mut self, panel_id: PanelId) {
        self.panels
            .panel_mut(panel_id)
            .state_mut()
            .pending_queries
            .alignment = self.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
        });
    }
}

/// Queues a refresh of every alignment track of `panel_id` and dispatches it.
pub fn request_alignment_query<H: PresentationHost>(host: &mut H, panel_id: PanelId) {
    let session = host.session_mut();
    for track_index in 0..session.alignment_tracks.len() {
        session.queue_alignment_navigation_for_track(
            track_index,
            panel_id,
            false,
            "alignment refresh requested",
        );
    }
    session.mark_alignment_queries_queued(panel_id);
    dispatch_alignment_query(host, panel_id);
}

/// A row camera of `track_id` in `panel_id` changed: queue and dispatch it alone.
pub fn request_alignment_scene_camera_update<H: PresentationHost>(
    host: &mut H,
    panel_id: PanelId,
    track_id: TrackId,
) {
    let session = host.session_mut();
    let Some(track_index) = session
        .alignment_tracks
        .iter()
        .position(|track| track.config.id == track_id)
    else {
        return;
    };
    if !matches!(
        session.alignment_tracks[track_index].lifecycle,
        AlignmentTrackLifecycle::Ready
    ) {
        return;
    }
    session.queue_alignment_navigation_for_track(
        track_index,
        panel_id,
        false,
        "alignment row camera changed",
    );
    session
        .panels
        .panel_mut(panel_id)
        .state_mut()
        .pending_queries
        .alignment = true;
    dispatch_alignment_query(host, panel_id);
}

/// Sends the UI's current camera to the unified source actor and changes no pixels itself.
/// Existing immutable scene pages are captured against this camera immediately by
/// `prepare_alignment_scene_frame`; source acquisition remains asynchronous and latest-wins.
#[allow(clippy::too_many_lines)]
pub fn dispatch_alignment_query<H: PresentationHost>(host: &mut H, panel_id: PanelId) {
    let session = host.session_mut();
    if !session
        .panels
        .panel(panel_id)
        .state()
        .pending_queries
        .alignment
    {
        return;
    }
    if session.alignment_tracks.is_empty() {
        session
            .panels
            .panel_mut(panel_id)
            .state_mut()
            .pending_queries
            .alignment = false;
        return;
    }
    let viewport = session.panels.panel(panel_id).state().viewport;
    let resolution = AlignmentSceneResolution::for_viewport(viewport);
    if !resolution.requires_alignment_records() {
        let mut settled = 0_usize;
        let mut retired_tracks = Vec::new();
        for track in &mut session.alignment_tracks {
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
        let primary_track_id = session.primary_alignment_track_id();
        for track_id in retired_tracks {
            let host_panel = session.panels.panel_mut(panel_id);
            host_panel
                .state_mut()
                .read_panel_scrolls
                .entry(track_id)
                .or_default()
                .retire_scene_authority();
            host_panel.clear_pending_alignment_reveal(Some(track_id));
            let panel = host_panel.state_mut();
            if track_id == primary_track_id {
                panel.vertical_scroll_points = 0.0;
                panel.vertical_content_origin_points = 0.0;
                panel.latent_vertical_scroll_fraction = None;
                panel.content_bottom_points = 0.0;
            }
        }
        session
            .panels
            .panel_mut(panel_id)
            .state_mut()
            .pending_queries
            .alignment = session.alignment_tracks.iter().any(|track| {
            track
                .projection(panel_id)
                .is_some_and(|projection| projection.navigation.is_queued())
        });
        if settled > 0 {
            session.data_status = format!(
                "Overview for {settled} alignment track(s): coverage only, reads are not loaded"
            );
        }
        host.prepare_alignment_scene_frame();
        return;
    }
    let contig_name = session.panels.panel(panel_id).state().contig_name.clone();
    let lane_layout = host.panel_lane_layout(panel_id);
    let mut sent = 0_usize;
    for track_index in 0..host.session().alignment_tracks.len() {
        let session = host.session_mut();
        if matches!(
            session.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Opening
        ) {
            continue;
        }
        if matches!(
            session.alignment_tracks[track_index].lifecycle,
            AlignmentTrackLifecycle::Failed(_)
        ) {
            let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
            projection.navigation.transition(
                AlignmentNavigationPhase::Failed,
                "alignment source is disconnected",
            );
            continue;
        }
        if !session.alignment_tracks[track_index]
            .projection(panel_id)
            .is_some_and(|projection| projection.navigation.is_queued())
        {
            continue;
        }
        let Some(metadata) = session.alignment_tracks[track_index].scene_metadata.clone() else {
            continue;
        };
        let Some((contig, contig_length)) =
            metadata
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
                session.panels.panel(panel_id).state().contig_name
            );
            session.alignment_tracks[track_index].mark_unavailable(panel_id, message);
            continue;
        };
        let track_id = session.alignment_tracks[track_index].config.id;
        let existing_owner = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_owner);
        let existing_contig = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_contig);
        let existing_contract = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_contract.as_ref())
            .cloned();
        let existing_policy = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_policy);
        let Ok(target_viewport) = viewport.remapped_contig(contig, contig_length) else {
            continue;
        };
        let old_atlas_interval = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.scene_frame.as_ref())
            .and_then(|frame| frame.snapshot.as_ref())
            .map(|snapshot| snapshot.reads().atlas().interval());
        let contig_changed = existing_contig.is_some_and(|current| current != contig);
        let atlas_disjoint = old_atlas_interval.is_some_and(|interval| {
            interval.contig() != contig || !interval.overlaps(target_viewport.interval())
        });
        let authority_discontinuity = contig_changed || atlas_disjoint;
        let primary_track_id = session.primary_alignment_track_id();
        let scroll_reset = {
            let host_panel = session.panels.panel_mut(panel_id);
            let reset = host_panel
                .state_mut()
                .read_panel_scrolls
                .entry(track_id)
                .or_default()
                .bind_scene_contig(contig, authority_discontinuity);
            if reset {
                host_panel.clear_pending_alignment_reveal(None);
                let panel = host_panel.state_mut();
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
            let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
            projection.scene_frame = None;
            projection.scene_frame_request_token = None;
            projection.scene_pressure_retry = None;
            if let Err(error) = host.clear_alignment_scene_lane(panel_id, track_id) {
                host.report_error(format!("Could not reset alignment scene lane: {error}"));
            }
        }
        let promoted_template = host.promoted_read_template(track_id);
        let session = host.session_mut();
        let reference_revision =
            session.compatible_alignment_reference_revision(&metadata.dictionary);
        let contract = alignment_scene_build_contract(
            &session.alignment_tracks[track_index].config,
            &session.alignment_tracks[track_index].active_filters,
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
        let lane_capture = match lane_layout.as_ref().map_or(Ok(None), |layout| {
            session.alignment_scene_lane_capture_for_panel_track(
                layout,
                panel_id,
                track_id,
                contig,
                contig_length,
                reference_backed_alleles,
            )
        }) {
            Ok(Some(capture)) => capture,
            Ok(None) => {
                // Layout is committed independently of source opening. Keep this request
                // pending until at least one real read or derived lane exists; never invent a
                // render clip merely to force source work.
                continue;
            }
            Err(error) => {
                host.report_error(format!("Could not resolve alignment scene lanes: {error}"));
                continue;
            }
        };
        let session = host.session_mut();
        let preparation_camera = lane_capture.preparation_camera;
        let policy = alignment_scene_preparation_policy(
            session.settings.read_browsing_buffer_percent,
            preparation_camera,
        );
        let complete_read_limit = session.alignment_tracks[track_index]
            .config
            .effective_complete_read_limit();
        let mut queue_error = None;
        let must_insert = existing_owner.is_none() || existing_contig != Some(contig);
        let owner = if must_insert {
            session.take_alignment_scene_owner()
        } else {
            existing_owner.expect("an existing alignment scene lane has an owner")
        };
        let request_token = session.alignment_tracks[track_index]
            .projection(panel_id)
            .and_then(|projection| projection.navigation.request_token())
            .expect("a queued alignment transaction has a request token");
        session.alignment_tracks[track_index]
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
        let source_acquisitions_before = session.alignment_tracks[track_index]
            .projection(panel_id)
            .map_or(0, |projection| {
                projection.scene_telemetry.source_acquisitions
            });
        let limit_changed =
            session.alignment_tracks[track_index].scene_complete_read_limit != complete_read_limit;
        let mut limit_queued = false;
        let mut lane_removed = false;
        let mut lane_inserted = false;
        let mut contract_replaced = false;
        let mut policy_replaced = false;
        let Some(engine) = session.alignment_tracks[track_index].scene_engine.as_ref() else {
            let track = &mut session.alignment_tracks[track_index];
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
                NonZeroUsize::new(complete_read_limit).expect("Complete read limit is non-zero"),
            )) {
                Ok(()) => limit_queued = true,
                Err(error) => queue_error = Some(error),
            }
        }
        if queue_error.is_none()
            && let Some(existing_owner) = existing_owner
            && existing_contig != Some(contig)
        {
            match engine.try_remove_lane(existing_owner) {
                Ok(()) => lane_removed = true,
                Err(error) => queue_error = Some(error),
            }
        }
        if queue_error.is_none() && must_insert {
            match engine.try_insert_lane_for_request(owner, request_token, contract.clone(), policy)
            {
                Ok(()) => lane_inserted = true,
                Err(error) => queue_error = Some(error),
            }
        }
        if queue_error.is_none() && !must_insert && contract_changed {
            match engine.try_replace_contract_for_request(owner, request_token, contract.clone()) {
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
            let track = &mut session.alignment_tracks[track_index];
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
        if contract_replaced && let Some(panel) = session.panels.get_mut(panel_id) {
            // The receipt acknowledges delivery to the previous coordinator contract epoch.
            // A replacement may preserve compatible authority, but it may also retire it.
            // Invalidate the receipt in either case so a still-pending exact requirement can
            // enqueue the cached slice again instead of waiting forever for a command which
            // already ran against the old contract.
            retire_reference_install_receipt_for_contract_epoch(
                &mut panel.state_mut().reference_installs_queued,
                owner,
            );
        }
        match queue_error {
            None => {
                {
                    let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
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
                    let filter_state = host.alignment_filter_diagnostic_state(panel_id, track_id);
                    host.diagnostic_event(
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
                let track = &mut session.alignment_tracks[track_index];
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
    let session = host.session_mut();
    session
        .panels
        .panel_mut(panel_id)
        .state_mut()
        .pending_queries
        .alignment = session.alignment_tracks.iter().any(|track| {
        track
            .projection(panel_id)
            .is_some_and(|projection| projection.navigation.is_queued())
    });
    if sent > 0 {
        session.data_status = format!("Updating {sent} alignment scene(s)…");
    } else if session
        .panels
        .panel(panel_id)
        .state()
        .pending_queries
        .alignment
    {
        "Alignment queues busy; retrying the latest locus…".clone_into(&mut session.data_status);
    }
    // Camera-only movement is visible before the worker produces another event. The capture
    // keeps the latest immutable snapshot and computes its truthful intersection with the
    // live viewport.
    host.prepare_alignment_scene_frame();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation_host::RecordingHost;
    use rgv_core::{
        ContigId, DesktopSettings, GenomeAssembly, GenomicInterval, ReferenceDictionary, Viewport,
    };
    use std::path::PathBuf;

    fn host() -> RecordingHost {
        let dictionary = ReferenceDictionary::from_contigs([("chr1".to_owned(), 1_000_000)]);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let panel = crate::panels::PanelState::new(
            PanelId::new(1),
            Viewport::new(interval, 600.0).unwrap(),
            None,
            "chr1".to_owned(),
            1_000_000,
        );
        RecordingHost::new(Session::new(
            panel,
            GenomeAssembly::from_dictionary("test", dictionary),
            DesktopSettings::default(),
            PathBuf::from("settings.json"),
        ))
    }

    #[test]
    fn dispatch_without_alignment_tracks_settles_the_flag_and_presents_nothing() {
        let mut host = host();
        host.session
            .panels
            .panel_state_mut(PanelId::new(1))
            .pending_queries
            .alignment = true;
        dispatch_alignment_query(&mut host, PanelId::new(1));
        assert!(
            !host
                .session
                .panels
                .panel_state(PanelId::new(1))
                .pending_queries
                .alignment
        );
        assert!(host.calls.is_empty());
    }

    #[test]
    fn a_request_without_tracks_is_never_pending() {
        let mut host = host();
        request_alignment_query(&mut host, PanelId::new(1));
        assert!(
            !host
                .session
                .panels
                .panel_state(PanelId::new(1))
                .pending_queries
                .alignment
        );
        assert!(host.calls.is_empty());
    }

    #[test]
    fn polling_without_engines_has_no_effects() {
        let mut host = host();
        crate::alignment::events::poll_alignment_events(&mut host);
        crate::reference_events::poll_reference_events(&mut host);
        assert!(host.calls.is_empty());
    }
}
