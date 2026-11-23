//! Settlement of reference-engine events: installing FASTA slices into panels and the alignment
//! lanes that need them, the reference dictionary opening (which may place the panels), and
//! failures. Moved from the desktop's `workspace/queries.rs`; every presentation effect goes
//! through the [`PresentationHost`] at the point in the event stream where the desktop made it.

use crate::alignment::navigation::AlignmentNavigationPhase;
use crate::alignment::settlement::{
    AlignmentReferenceLanePlan, alignment_scene_event_token_is_current,
    plan_alignment_reference_lanes, reference_failure_matches_requirement,
    reference_query_completion_matches,
};
use crate::alignment::track::{
    AlignmentReferenceCompatibility, AlignmentTrackLifecycle, alignment_reference_compatibility,
};
use crate::navigation::{ImportedLocusRequest, RequestedLocus, resolve_requested_locus};
use crate::panels::Panel;
use crate::presentation_host::PresentationHost;
use crate::reference_dispatch::buffered_context_interval;
use crate::session::Session;
use crate::sources::load_assembly_reporting;
use anyhow::Result;
use rgv_core::{ReferenceDictionary, TrackSource, Viewport};
use rgv_engine::{MAX_REFERENCE_QUERY_BASES, ReferenceEngineEvent};
use std::collections::BTreeSet;
use std::sync::Arc;

impl<P: Panel> Session<P> {
    /// Finishes resolving a panel's requested locus against `dictionary`, queuing a feature
    /// search when it was an imported gene name.
    pub fn resolve_panel_requested_locus(
        &self,
        dictionary: &ReferenceDictionary,
        requested: Option<&str>,
    ) -> Result<RequestedLocus> {
        resolve_requested_locus(
            dictionary,
            &self.contig_aliases,
            requested,
            self.imported_locus
                .as_ref()
                .and_then(ImportedLocusRequest::feature_name),
        )
    }
}

#[allow(clippy::too_many_lines)]
pub fn poll_reference_events<H: PresentationHost>(host: &mut H) {
    let mut events = Vec::new();
    let mut receive_error = None;
    if let Some(engine) = &host.session().reference_engine {
        loop {
            match engine.try_receive() {
                Ok(Some(event)) => events.push(event),
                Ok(None) => break,
                Err(error) => {
                    receive_error = Some(error.to_string());
                    break;
                }
            }
        }
    }
    if let Some(message) = receive_error {
        host.report_error(message);
    }
    if events.is_empty() {
        return;
    }
    let session = host.session_mut();
    let active_panel_id = session.panels.active().state().id;
    let mut reference_panels_to_retry = BTreeSet::new();
    let mut highlight_panels_to_synchronize = BTreeSet::new();
    let mut splice_motifs_arrived = false;
    for event in events {
        let session = host.session_mut();
        match event {
            ReferenceEngineEvent::Opened {
                revision,
                dictionary,
                remote_io_stats,
            } => {
                let reference_contig_count = dictionary.len();
                let alignment_incompatibility =
                    session
                        .primary_alignment_dictionary()
                        .and_then(|alignment| {
                            match alignment_reference_compatibility(
                                alignment,
                                &dictionary,
                                &session.contig_aliases,
                            ) {
                                AlignmentReferenceCompatibility::Compatible => None,
                                AlignmentReferenceCompatibility::Incompatible(reason) => {
                                    Some(reason)
                                }
                            }
                        });
                session.reference_open_error = None;
                session.reference_revision = Some(revision);
                host.observe_reference_remote_io(remote_io_stats);
                let session = host.session_mut();
                session.reference_dictionary = Some(dictionary.clone());
                // Opening (including reopening the same path) establishes a new immutable
                // authority. No slice or in-flight request from the previous handle may be
                // paired with contracts carrying this revision.
                for panel in session.panels.states_mut() {
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
                    .extend(session.panels.states().map(|panel| panel.id));
                if session.primary_alignment_dictionary().is_none() {
                    let ids = session
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
                            .map(|panel| {
                                (panel.requested_locus.clone(), panel.viewport.pixel_width())
                            })
                            .expect("panel id came from the panel collection");
                        match session
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
                                let panel = session
                                    .panels
                                    .state_mut(id)
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
                                host.report_error(error.to_string());
                            }
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
                    host.session_mut().assembly.default_annotation_count = default_annotation_count;
                }
                host.update_landmark_resolution(&dictionary);
                let session = host.session_mut();
                session.assembly.reference_contig_count = reference_contig_count;
                session
                    .assembly
                    .identity
                    .clone_from(&session.assembly_identity);
                session.synthetic = false;
                if session.alignment_tracks.is_empty() {
                    let source = session.reference_source.as_ref().map_or_else(
                        || "reference genome".to_owned(),
                        TrackSource::redacted_label,
                    );
                    session.data_status = format!("Opened {source}");
                } else if let Some(reason) = alignment_incompatibility {
                    session.data_status = format!(
                        "Reference incompatible with alignment: {reason}. Reference-dependent bases and substitutions are disabled"
                    );
                }
                host.refresh_active_panel_context();
                host.request_all_panel_queries();
                host.mark_display_dirty("explicit display invalidation");
            }
            ReferenceEngineEvent::QueryCompleted {
                query,
                slice,
                remote_io_stats,
            } => {
                let request = query.request();
                let revision = query.revision();
                host.observe_reference_remote_io(remote_io_stats);
                let session = host.session_mut();
                let is_active = request.panel_id() == active_panel_id;
                let mut accepted = false;
                let current_revision = session.reference_revision;
                if let Some(panel) = session.panels.state_mut(request.panel_id())
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
                    if is_active && session.alignment_tracks.is_empty() {
                        "Reference sequence loaded".clone_into(&mut session.data_status);
                    }
                }
                if accepted {
                    // Display invalidation and the status line are independent; marking
                    // after the status update keeps the session borrow in one piece.
                    host.mark_display_dirty("explicit display invalidation");
                }
                let session = host.session_mut();
                if accepted {
                    highlight_panels_to_synchronize.insert(request.panel_id());
                    let installed = session.install_reference_authority_for_panel(
                        request.panel_id(),
                        revision,
                        &slice,
                    );
                    let contextual_horizon =
                        session.panels.state(request.panel_id()).map(|panel| {
                            buffered_context_interval(
                                panel.viewport,
                                panel.contig_length,
                                session.settings.read_browsing_buffer_percent,
                                Some(MAX_REFERENCE_QUERY_BASES),
                            )
                        });
                    let context_covered = contextual_horizon
                        .is_some_and(|interval| slice.interval.contains_interval(interval));
                    let settled = installed && context_covered;
                    if let Some(panel) = session.panels.state_mut(request.panel_id()) {
                        panel.pending_queries.reference = !settled;
                        if settled {
                            panel.reference_settled_generation = Some(request.generation().get());
                        }
                    }
                    if !settled {
                        reference_panels_to_retry.insert(request.panel_id());
                    }
                } else {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                }
            }
            ReferenceEngineEvent::SpliceMotifs {
                revision,
                contig,
                motifs,
            } => {
                let current = session.reference_revision;
                if session
                    .splice_motifs
                    .accept(current, revision, contig, motifs)
                {
                    // Junction strands may change: the Sashimi presentation is rebuilt from the
                    // published scene, with no source query.
                    splice_motifs_arrived = true;
                }
            }
            ReferenceEngineEvent::Error { query, message } => {
                if query.is_none() {
                    session.reference_open_error = Some(message.clone());
                    session.reference_revision = None;
                    session.reference_dictionary = None;
                    // A failed open is terminal for this actor. Leaving it installed would
                    // make every later Base navigation advertise reference work even though
                    // no open revision exists and no query can be issued.
                    session.retire_reference_engine();
                    for panel in session.panels.states_mut() {
                        panel.settle_reference_open_failure();
                    }
                    for projection in session
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
                    session
                        .panels
                        .state(request.panel_id())
                        .is_some_and(|panel| {
                            panel.accepts(request)
                                && reference_query_completion_matches(
                                    session.reference_revision,
                                    panel.reference_request_revision,
                                    panel.reference_request_interval,
                                    query.revision(),
                                    query.interval(),
                                )
                        })
                });
                if query.is_some() && !current {
                    session.navigation_stale_result_rejections =
                        session.navigation_stale_result_rejections.saturating_add(1);
                }
                if current {
                    let mut failed_authority = None;
                    if let Some(query) = query {
                        let request = query.request();
                        if let Some(panel) = session.panels.state_mut(request.panel_id()) {
                            failed_authority =
                                Some((request.panel_id(), query.revision(), query.interval()));
                            panel.reference_request_revision = None;
                            panel.reference_request_interval = None;
                            panel.reference_settled_generation = Some(request.generation().get());
                            panel.reference_query_error_generation =
                                Some(request.generation().get());
                        }
                    }
                    if let Some((panel_id, revision, interval)) = failed_authority {
                        let mut failed_owners = Vec::new();
                        for track in &mut session.alignment_tracks {
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
                        let alignment_pending = session.alignment_tracks.iter().any(|track| {
                            !matches!(track.lifecycle, AlignmentTrackLifecycle::Failed(_))
                                && track.projection(panel_id).is_some_and(|projection| {
                                    !projection
                                        .navigation
                                        .is_current_settled(projection.clock.current().get())
                                })
                        });
                        if let Some(panel) = session.panels.state_mut(panel_id) {
                            panel.pending_queries.alignment = alignment_pending;
                            for owner in failed_owners {
                                panel.reference_installs_queued.remove(&owner);
                            }
                        }
                        let remaining =
                            session.current_alignment_reference_requirements(panel_id, revision);
                        let needs_another_query =
                            session.panels.state(panel_id).is_some_and(|panel| {
                                matches!(
                                    plan_alignment_reference_lanes(
                                        &remaining,
                                        &panel.reference_installs_queued,
                                    ),
                                    AlignmentReferenceLanePlan::Query { .. }
                                )
                            });
                        if needs_another_query {
                            if let Some(panel) = session.panels.state_mut(panel_id) {
                                panel.pending_queries.reference = true;
                            }
                            reference_panels_to_retry.insert(panel_id);
                        }
                    }
                    session.data_status = message;
                }
            }
        }
    }
    if !reference_panels_to_retry.is_empty() {
        for panel_id in reference_panels_to_retry {
            if host.session_mut().dispatch_reference_query(panel_id) {
                host.request_redraw();
            }
        }
    }
    if !highlight_panels_to_synchronize.is_empty() {
        for panel_id in highlight_panels_to_synchronize {
            host.synchronize_alignment_scene_interactions(panel_id);
        }
        host.rebuild_all_panel_displays();
        host.prepare_alignment_scene_frame();
    } else if splice_motifs_arrived {
        host.prepare_alignment_scene_frame();
    }
    host.request_redraw();
}
