//! Settlement of alignment scene engine events: source opening, scene frames, individual-read
//! refusals, source and budget pressure, capacity reclaim and errors. Moved from the desktop's
//! `workspace/queries.rs`; every presentation effect (yielding GPU speculation, observing a
//! capacity reclaim, diagnostics, display rebuilds, frame preparation) goes through the
//! [`PresentationHost`] at the point in the event stream where the desktop made it, so event
//! ordering is unchanged.

use crate::alignment::navigation::AlignmentNavigationPhase;
use crate::alignment::settlement::{
    AlignmentSceneFrameSettlement, alignment_scene_budget_event_is_current,
    alignment_scene_event_token_is_current, alignment_scene_frame_settlement,
    alignment_scene_pressure_is_current, alignment_scene_pressure_owns_navigation,
    alignment_scene_pressure_preserves_visible_epoch, alignment_scene_pressure_requires_resume,
    alignment_scene_refusal_is_current,
};
use crate::alignment::track::{
    AlignmentReferenceCompatibility, AlignmentSceneBudgetRetry, AlignmentScenePressureRetry,
    AlignmentScenePressureRetryPoll, AlignmentTrackLifecycle, alignment_reference_compatibility,
    alignment_scene_pressure_after_relief,
};
use crate::navigation::RequestedLocus;
use crate::notifications::NotificationSeverity;
use crate::panels::Panel;
use crate::presentation_host::PresentationHost;
use crate::session::Session;
use crate::sources::load_assembly_reporting;
use rgv_core::{GenomicInterval, ReferenceDictionary, TrackSource, Viewport};
use rgv_engine::{
    AlignmentCoordinatorFrame, AlignmentIndividualReadCapability, AlignmentSceneEngineEvent,
    AlignmentSceneFrameCause, AlignmentSceneResourcePressureDisposition,
    AlignmentVerticalResidency, EngineQueueError, SourceOwnerId,
};
use std::collections::BTreeSet;

/// A retained snapshot from a discontinuous locus is not a row-count or render authority for the
/// new camera. Source workers may truthfully attach that old immutable snapshot while acquiring
/// the replacement; settlement keeps the progress frame but strips the irrelevant scene.
pub fn alignment_scene_frame_for_ui(frame: AlignmentCoordinatorFrame) -> AlignmentCoordinatorFrame {
    let retained_interval = frame
        .snapshot
        .as_ref()
        .map(|snapshot| snapshot.reads().atlas().interval());
    alignment_scene_frame_for_ui_against_retained_interval(frame, retained_interval)
}

/// Normalizes a worker progress frame against the interval carried by its retained snapshot. This
/// decomposition keeps the discontinuous-locus rule directly testable without constructing the
/// heavyweight immutable scene graph.
pub fn alignment_scene_frame_for_ui_against_retained_interval(
    mut frame: AlignmentCoordinatorFrame,
    retained_interval: Option<GenomicInterval>,
) -> AlignmentCoordinatorFrame {
    let camera_refusal_is_terminal = frame.individual_reads_refused_for_camera();
    let relevant = retained_interval.is_none_or(|interval| {
        alignment_scene_intervals_share_authority(interval, frame.camera.interval())
    });
    if !relevant {
        frame.snapshot = None;
        frame.row_activity = None;
        frame.covered_interval = None;
        frame.covered_rows = 0..0;
        frame.requested_rows = frame.camera.visible_rows();
        // The requirement describes the new desired camera, not the discarded old snapshot. It
        // must survive so the host can request and install the exact FASTA horizon.
        frame.scene_contracts_current = false;
        frame.reference_products_current = false;
        if !camera_refusal_is_terminal {
            frame.individual_reads = AlignmentIndividualReadCapability::Pending;
        }
        frame.vertical_residency = AlignmentVerticalResidency::ViewportBounded;
    }
    frame
}

pub fn alignment_scene_intervals_share_authority(
    atlas: GenomicInterval,
    camera: GenomicInterval,
) -> bool {
    atlas.contig() == camera.contig() && atlas.overlaps(camera)
}

impl<P: Panel> Session<P> {
    pub fn primary_alignment_source(&self) -> Option<&TrackSource> {
        self.alignment_tracks
            .first()
            .map(|track| &track.config.source)
    }

    /// Why `alignment` cannot use the open reference, if it cannot.
    pub fn alignment_reference_incompatibility(
        &self,
        alignment: &ReferenceDictionary,
    ) -> Option<String> {
        let reference = self.reference_dictionary.as_ref()?;
        match alignment_reference_compatibility(alignment, reference, &self.contig_aliases) {
            AlignmentReferenceCompatibility::Compatible => None,
            AlignmentReferenceCompatibility::Incompatible(reason) => Some(reason),
        }
    }
}

#[allow(clippy::too_many_lines)]
pub fn poll_alignment_events<H: PresentationHost>(host: &mut H) {
    let session = host.session_mut();
    let active_panel_id = session.panels.active().state().id;
    let primary_id = session
        .alignment_tracks
        .first()
        .map(|track| track.config.id);
    let mut events = Vec::new();
    for track in &mut session.alignment_tracks {
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
                        let message = error.to_string();
                        session.notifications.push_reported(
                            NotificationSeverity::Error,
                            None,
                            &message,
                        );
                        session.data_status = message;
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
        let session = host.session_mut();
        let Some(track_index) = session
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == track_id)
        else {
            continue;
        };
        match event {
            AlignmentSceneEngineEvent::SourceOpened { metadata } => {
                let dictionary = metadata.dictionary.clone();
                let track = &mut session.alignment_tracks[track_index];
                track.lifecycle = AlignmentTrackLifecycle::Ready;
                track.dictionary = Some(dictionary.clone());
                track.index_overview.clone_from(&metadata.index_overview);
                track.scene_metadata = Some(metadata);
                let panel_ids = track.projections.keys().copied().collect::<Vec<_>>();
                let track_id = track.config.id;
                host.start_alignment_coverage_engine(track_index);
                let session = host.session_mut();
                for panel_id in panel_ids {
                    let token = session.take_alignment_scene_request_token();
                    let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
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
                let panel_id = session.alignment_tracks[track_index]
                    .projections
                    .iter()
                    .find_map(|(panel_id, projection)| {
                        (projection.scene_owner == Some(owner)).then_some(*panel_id)
                    });
                let Some(panel_id) = panel_id else {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                };
                if host.diagnostics_recording()
                    && cause == AlignmentSceneFrameCause::SourceSettlement
                {
                    host.diagnostic_event(
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
                if host.diagnostics_recording()
                    && let Some(capacity) = update.and_then(|update| update.capacity_plan)
                {
                    host.diagnostic_event(
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
                let session = host.session_mut();
                if session.alignment_tracks[track_index]
                    .projection(panel_id)
                    .is_some_and(|projection| projection.index_only_overview)
                {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                }
                let modification_calls = frame.snapshot.as_ref().is_some_and(|snapshot| {
                    crate::alignment::track::scene_has_modification_calls(snapshot)
                });
                let track = &mut session.alignment_tracks[track_index];
                let newly_observed = modification_calls && !track.modification_tags_observed();
                let projection = track.projection_mut(panel_id);
                projection.modification_tags_observed |= modification_calls;
                // The Modifications lane joins the layout the first time calls arrive.
                rebuild_display |= newly_observed;
                if !projection
                    .navigation
                    .worker_frame_is_admissible(request_token, frame.camera)
                {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                }
                let monotonic = projection
                    .scene_frame
                    .as_ref()
                    .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                if !monotonic {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
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
                let current_target = projection.navigation.desired_camera() == Some(frame.camera);
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
                            if projection.navigation.phase() != AlignmentNavigationPhase::Committed
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
                            if projection.navigation.phase() != AlignmentNavigationPhase::Committed
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
                            if projection.navigation.phase() != AlignmentNavigationPhase::Running {
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
                if Some(track_id) == primary_id && panel_id == active_panel_id && current_target {
                    match frame.individual_reads {
                        AlignmentIndividualReadCapability::Complete { .. } => {
                            let records = frame
                                .snapshot
                                .as_ref()
                                .map_or(0, |snapshot| snapshot.reads().evidence().record_count());
                            session.data_status = match frame.vertical_residency {
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
                                .clone_into(&mut session.data_status);
                        }
                        AlignmentIndividualReadCapability::Refused {
                            complete_read_limit,
                            eligible_records,
                            ..
                        } => {
                            session.data_status = format!(
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
                let panel_id = session.alignment_tracks[track_index]
                    .projections
                    .iter()
                    .find_map(|(panel_id, projection)| {
                        (projection.scene_owner == Some(owner)).then_some(*panel_id)
                    });
                let Some(panel_id) = panel_id else {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                };
                if session.alignment_tracks[track_index]
                    .projection(panel_id)
                    .is_some_and(|projection| projection.index_only_overview)
                {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                }
                let track = &mut session.alignment_tracks[track_index];
                let projection = track.projection_mut(panel_id);
                if !alignment_scene_refusal_is_current(
                    projection.navigation.request_token(),
                    request_token,
                    projection.navigation.desired_camera(),
                    frame.camera,
                    target,
                ) {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                }
                let monotonic = projection
                    .scene_frame
                    .as_ref()
                    .is_none_or(|current| frame.camera_sequence >= current.camera_sequence);
                if !monotonic {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
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
                    session.data_status = format!(
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
                let panel_id = session.alignment_tracks[track_index]
                    .projections
                    .iter()
                    .find_map(|(panel_id, projection)| {
                        (projection.scene_owner == Some(owner)).then_some(*panel_id)
                    });
                let Some(panel_id) = panel_id else {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                };
                let current = session.alignment_tracks[track_index]
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
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                }

                // Source read-ahead is optional once the current frame is complete. It yields
                // at its own checkpoint and must not displace a useful GPU scene or reopen the
                // foreground navigation transaction.
                if !alignment_scene_pressure_requires_resume(operation, intent) {
                    continue;
                }

                if alignment_scene_pressure_preserves_visible_epoch(operation, intent, disposition)
                {
                    let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
                    projection.scene_pressure_retry = None;
                    if Some(track_id) == primary_id && panel_id == active_panel_id {
                        let records = projection
                            .scene_frame
                            .as_ref()
                            .and_then(|frame| frame.snapshot.as_ref())
                            .map_or(0, |snapshot| snapshot.reads().evidence().record_count());
                        session.data_status = projection.scene_frame.as_ref().map_or_else(
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
                    let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
                    projection.scene_pressure_retry = None;
                    projection.settle_unavailable(message.clone());
                    if Some(track_id) == primary_id && panel_id == active_panel_id {
                        host.report_warning(message);
                    }
                    rebuild_display = true;
                    continue;
                }

                let snapshot_before = match session.alignment_scene_ledger.progress() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        session.data_status =
                            format!("Could not inspect alignment cache capacity: {error}");
                        continue;
                    }
                };
                let answers_existing_retry = session.alignment_tracks[track_index]
                    .projection(panel_id)
                    .and_then(|projection| projection.scene_pressure_retry)
                    .is_some_and(|retry| retry.same_request(request_token, target));
                let post_relief = if answers_existing_retry {
                    // This is the typed answer to the queued retry. Revalidate its newest exact
                    // obstruction even if occupancy changed while the request was in flight.
                    snapshot_before
                } else {
                    match host.yield_speculation_for_source_pressure(pressure) {
                        Ok(_) => {}
                        Err(error) => {
                            host.session_mut().data_status =
                                format!("Could not yield optional GPU residency: {error}");
                            continue;
                        }
                    }
                    match host.session().alignment_scene_ledger.progress() {
                        Ok(snapshot) => snapshot,
                        Err(error) => {
                            host.session_mut().data_status =
                                format!("Could not inspect alignment cache capacity: {error}");
                            continue;
                        }
                    }
                };
                let session = host.session_mut();
                let retry_epoch = post_relief.progress_epoch();
                let pressure_owns_navigation = alignment_scene_pressure_owns_navigation(
                    session.alignment_tracks[track_index]
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
                    session.alignment_tracks[track_index]
                        .projection_mut(panel_id)
                        .scene_pressure_retry = Some(waiting);
                    if pressure_owns_navigation {
                        session.alignment_tracks[track_index]
                            .projection_mut(panel_id)
                            .navigation
                            .transition(
                                AlignmentNavigationPhase::WaitingForSourcePressure,
                                "source allocation is paused on an exact ledger blocker",
                            );
                        if Some(track_id) == primary_id && panel_id == active_panel_id {
                            "Waiting for shared alignment cache capacity…"
                                .clone_into(&mut session.data_status);
                        }
                    }
                    continue;
                }
                let retry_result = session.alignment_tracks[track_index]
                    .scene_engine
                    .as_ref()
                    .map(|engine| engine.try_retry_resource_pressure(owner, request_token, target));
                let retry_queued = matches!(retry_result, Some(Ok(())));
                session.alignment_tracks[track_index]
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
                            session.alignment_tracks[track_index]
                                .projection_mut(panel_id)
                                .navigation
                                .transition(
                                    AlignmentNavigationPhase::Running,
                                    "source pressure retry was accepted",
                                );
                            if Some(track_id) == primary_id && panel_id == active_panel_id {
                                "Updating alignment scene…".clone_into(&mut session.data_status);
                            }
                        }
                    }
                    Some(Err(EngineQueueError::Disconnected)) | None => {
                        session.alignment_tracks[track_index]
                            .projection_mut(panel_id)
                            .settle_failed("alignment source mailbox disconnected");
                        session.alignment_tracks[track_index].lifecycle =
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
                let desired_budget = session.alignment_scene_budget.budget_bytes;
                let submitted_budget = session.alignment_tracks[track_index]
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
                let snapshot = match session.alignment_scene_ledger.progress() {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        session.data_status =
                            format!("Could not inspect alignment cache capacity: {error}");
                        continue;
                    }
                };
                session.alignment_tracks[track_index].scene_budget_retry =
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
                if session.alignment_tracks[track_index]
                    .scene_budget_retry
                    .is_some_and(|retry| retry.requested_budget == pressure.budget)
                {
                    session.alignment_tracks[track_index].scene_budget_retry = None;
                }
            }
            AlignmentSceneEngineEvent::InactiveEvicted {
                request_id,
                eviction,
            } => {
                if host.diagnostics_recording() {
                    host.diagnostic_event(
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
                host.observe_capacity_reclaim(
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
                if host.diagnostics_recording() {
                    host.diagnostic_event(
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
                if host.diagnostics_recording() {
                    host.diagnostic_event(
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
                host.observe_capacity_reclaim(
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
                if host.diagnostics_recording() {
                    host.diagnostic_event(
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
                    session.alignment_tracks[track_index]
                        .projections
                        .iter()
                        .find_map(|(panel_id, projection)| {
                            (projection.scene_owner == Some(owner)).then_some(*panel_id)
                        })
                });
                if let Some(panel_id) = panel_id {
                    let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
                    if !alignment_scene_event_token_is_current(
                        projection.navigation.request_token(),
                        request_token,
                    ) {
                        session.navigation_stale_result_rejections =
                            session.navigation_stale_result_rejections.saturating_add(1);
                        continue;
                    }
                    projection.settle_failed(message.to_string());
                } else if owner.is_some() {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                    continue;
                } else if matches!(
                    operation,
                    rgv_engine::AlignmentSceneEngineOperation::OpenSource
                ) {
                    session.alignment_tracks[track_index].lifecycle =
                        AlignmentTrackLifecycle::Failed(message.to_string());
                    for projection in session.alignment_tracks[track_index]
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
                    for projection in session.alignment_tracks[track_index]
                        .projections
                        .values_mut()
                    {
                        projection.settle_failed(message.to_string());
                    }
                }
                if Some(track_id) == primary_id
                    && panel_id.is_none_or(|panel_id| panel_id == active_panel_id)
                {
                    host.report_error(message.to_string());
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
        let session = host.session_mut();
        let Some(track_index) = session
            .alignment_tracks
            .iter()
            .position(|track| track.config.id == track_id)
        else {
            continue;
        };
        let projection = session.alignment_tracks[track_index].projection_mut(panel_id);
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
                host.report_error("Internal liveness failure: the alignment worker became idle before completing this target".to_owned());
            }
            rebuild_display = true;
        }
    }

    if !reference_panels_to_request.is_empty() {
        for panel_id in reference_panels_to_request {
            let session = host.session_mut();
            session
                .panels
                .panel_state_mut(panel_id)
                .pending_queries
                .reference = true;
            if session.dispatch_reference_query(panel_id) {
                host.request_redraw();
            }
        }
    }

    let session = host.session_mut();
    let panel_ids = session
        .panels
        .states()
        .map(|panel| panel.id)
        .collect::<Vec<_>>();
    for panel_id in panel_ids {
        let pending = session.alignment_tracks.iter().any(|track| {
            !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                && track.projection(panel_id).is_some_and(|projection| {
                    !projection
                        .navigation
                        .is_current_settled(projection.clock.current().get())
                })
        });
        if let Some(panel) = session.panels.state_mut(panel_id) {
            panel.pending_queries.alignment = pending;
        }
    }

    if let Some(dictionary) = opened_primary_dictionary {
        let session = host.session_mut();
        let reference_incompatibility = session.alignment_reference_incompatibility(&dictionary);
        if let Some(source) = session.primary_alignment_source().cloned() {
            session.settings.remember_alignment(&source);
            host.persist_settings();
        }
        let ids = host
            .session()
            .panels
            .states()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let mut imported_feature_search = None;
        for id in ids {
            let session = host.session_mut();
            let (requested, pixel_width) = session
                .panels
                .state(id)
                .map(|panel| (panel.requested_locus.clone(), panel.viewport.pixel_width()))
                .expect("panel id came from the panel collection");
            match session
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
                    let panel = session
                        .panels
                        .state_mut(id)
                        .expect("panel id came from the panel collection");
                    panel.viewport = viewport;
                    panel.contig_name = name;
                    panel.contig_length = length;
                    panel.reset_history(interval);
                    panel.vertical_scroll_points = 0.0;
                    panel.vertical_content_origin_points = 0.0;
                    panel.latent_vertical_scroll_fraction = None;
                }
                Err(error) if id == active_panel_id => host.report_error(error.to_string()),
                Err(_) => {}
            }
        }
        host.finish_imported_locus(imported_feature_search);
        let session = host.session_mut();
        let default_annotation_count = session.assembly.default_annotation_count;
        let (assembly, cytoband_failure) = load_assembly_reporting(
            &session.assembly_name,
            dictionary.clone(),
            session.cytoband_path.as_deref(),
        );
        session.assembly = assembly;
        if let Some(failure) = cytoband_failure {
            host.report_error(failure);
        }
        let session = host.session_mut();
        session.assembly.default_annotation_count = default_annotation_count;
        session.assembly.reference_contig_count = session
            .reference_dictionary
            .as_ref()
            .map_or(dictionary.len(), ReferenceDictionary::len);
        session
            .assembly
            .identity
            .clone_from(&session.assembly_identity);
        if let Some(reason) = reference_incompatibility {
            for panel in session.panels.states_mut() {
                panel.reference_slice = None;
                panel.reference_slice_revision = None;
                panel.reference_slice_generation = None;
                panel.reference_request_revision = None;
                panel.reference_request_interval = None;
                panel.reference_installs_queued.clear();
            }
            session.data_status = format!(
                "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
            );
        }
    }
    if source_opened {
        // Rebuild first so dispatch can derive the read-pane clip from committed layout.
        host.rebuild_all_panel_displays();
        host.request_all_panel_queries();
        rebuild_display = false;
        prepare_frame = true;
    }
    for panel_id in highlight_panels_to_synchronize {
        host.synchronize_alignment_scene_interactions(panel_id);
    }
    if rebuild_display {
        host.rebuild_all_panel_displays();
        prepare_frame = true;
    }
    if prepare_frame {
        host.prepare_alignment_scene_frame();
    }
    host.request_redraw();
}
