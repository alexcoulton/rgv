//! A presenter with no window and no GPU: it lays panels out on a fixed canvas, keeps the
//! committed alignment scene frames in CPU panel-scene stores, and reports every complete lane
//! back to the session as admitted, so a [`Session`] settles exactly as it does under the
//! desktop — through the same dispatch and settlement code — and a caller can render what it
//! settled on with the CPU projectors.
//!
//! What the desktop does on the GPU (residency, prefetch, speculation, capacity reclaim) has no
//! counterpart here: nothing is resident, so source pressure has nothing to yield. The coverage
//! pyramid is not started; at read and base zoom coverage comes from the scene's own summaries,
//! as it does on the desktop.

use crate::alignment::admission::AlignmentLaneAdmission;
use crate::alignment::dispatch::{dispatch_alignment_query, request_alignment_query};
use crate::alignment::events::poll_alignment_events;
use crate::alignment::settlement::{
    alignment_scene_event_token_is_current, reference_dependency_is_settled,
};
use crate::alignment::track::AlignmentTrackLifecycle;
use crate::lane_layout::{
    LaneLayout, LaneOwner, LayoutRect, PaneLanes, PanelLaneLayout, Sublane, SublaneKind,
};
use crate::panels::PanelState;
use crate::presentation::PanelPresentation;
use crate::presentation_host::PresentationHost;
use crate::reference_dispatch::buffered_context_interval;
use crate::reference_events::poll_reference_events;
use crate::session::Session;
use anyhow::{Result, bail};
use rgv_alignment_scene::{
    AlignmentPanelLanePublication, AlignmentPanelSceneFrame, AlignmentPanelSceneStore,
    AlleleSummaryDetail, LedgerError, LedgerProgressEpoch,
};
use rgv_core::{
    MAXIMUM_BASE_MARK_BASES_PER_PIXEL, PanelId, ReferenceDictionary, TrackChannel, TrackId,
    TrackKind,
};
use rgv_engine::{EngineQueueError, TrackEngineEvent};
use rgv_hts::RemoteIoStats;
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

/// Height of the locus ruler above the track panes, in logical points.
pub const HEADLESS_RULER_HEIGHT: f32 = 24.0;
/// Height of each lane's header row (its name), as on the desktop.
pub const HEADLESS_TRACK_HEADER_HEIGHT: f32 = 22.0;
/// Height of an alignment track's coverage sublane.
pub const HEADLESS_COVERAGE_HEIGHT: f32 = 60.0;
/// Content height of an annotation track's lane.
pub const HEADLESS_ANNOTATION_CONTENT_HEIGHT: f32 = 36.0;

/// The canvas a headless host lays panels out on, in logical points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeadlessCanvas {
    pub width: f32,
    pub height: f32,
    /// Width of the track-name gutter left of the panel columns.
    pub gutter_width: f32,
}

/// A headless [`PresentationHost`] over a [`Session`] of bare panel states.
pub struct HeadlessHost {
    pub session: Session<PanelState>,
    pub canvas: HeadlessCanvas,
    scene_stores: BTreeMap<(PanelId, TrackId), AlignmentPanelSceneStore>,
    presented: BTreeMap<(PanelId, TrackId), AlignmentPanelSceneFrame>,
    camera_revision: u64,
}

impl HeadlessHost {
    pub fn new(session: Session<PanelState>, canvas: HeadlessCanvas) -> Self {
        Self {
            session,
            canvas,
            scene_stores: BTreeMap::new(),
            presented: BTreeMap::new(),
            camera_revision: 0,
        }
    }

    /// The canvas column of `panel_id`: panels share the width right of the gutter equally.
    pub fn panel_column(&self, panel_id: PanelId) -> Option<LayoutRect> {
        let count = self.session.panels.len().max(1);
        let index = self
            .session
            .panels
            .states()
            .position(|panel| panel.id == panel_id)?;
        let available = (self.canvas.width - self.canvas.gutter_width).max(1.0);
        let column = available / count as f32;
        let left = self.canvas.gutter_width + column * index as f32;
        Some(LayoutRect::from_min_max(
            left,
            0.0,
            left + column,
            self.canvas.height,
        ))
    }

    /// The fixed lane layout: a ruler, then the annotation pane (each alignment track's
    /// coverage, then each annotation track), then the reads pane split between the alignment
    /// tracks.
    pub fn lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout> {
        let column = self.panel_column(panel_id)?;
        let (left, right) = (column.left, column.right);
        let mut annotation = PaneLanes::empty(TrackChannel::Annotation);
        let mut top = HEADLESS_RULER_HEIGHT;
        for track in &self.session.alignment_tracks {
            let bottom = top + HEADLESS_TRACK_HEADER_HEIGHT + HEADLESS_COVERAGE_HEIGHT;
            annotation.lanes.push(lane(
                LaneOwner::Alignment(track.config.id),
                TrackChannel::Annotation,
                &track.config.name,
                TrackKind::Alignment,
                (left, right, top, bottom),
                vec![Sublane {
                    kind: Some(SublaneKind::Coverage),
                    label: "Coverage".to_owned(),
                    top: top + HEADLESS_TRACK_HEADER_HEIGHT,
                    bottom,
                    collapsed: false,
                }],
            ));
            top = bottom;
        }
        for track in self.session.tracks.tracks() {
            if !track.visible {
                continue;
            }
            let bottom = top + HEADLESS_TRACK_HEADER_HEIGHT + HEADLESS_ANNOTATION_CONTENT_HEIGHT;
            annotation.lanes.push(lane(
                LaneOwner::Auxiliary(track.id),
                TrackChannel::Annotation,
                &track.name,
                track.kind,
                (left, right, top, bottom),
                Vec::new(),
            ));
            top = bottom;
        }
        let annotation_bottom = top.min(self.canvas.height);
        annotation.pane_rect =
            LayoutRect::from_min_max(left, HEADLESS_RULER_HEIGHT, right, annotation_bottom);
        annotation.content_bottom = top;
        clip_pane(&mut annotation);

        let mut reads = PaneLanes::empty(TrackChannel::Reads);
        reads.pane_rect =
            LayoutRect::from_min_max(left, annotation_bottom, right, self.canvas.height);
        let alignment_count = self.session.alignment_tracks.len();
        if alignment_count > 0 {
            let share = (self.canvas.height - annotation_bottom).max(0.0) / alignment_count as f32;
            for (index, track) in self.session.alignment_tracks.iter().enumerate() {
                let top = annotation_bottom + share * index as f32;
                let bottom = top + share;
                reads.lanes.push(lane(
                    LaneOwner::Alignment(track.config.id),
                    TrackChannel::Reads,
                    &track.config.name,
                    TrackKind::Alignment,
                    (left, right, top, bottom),
                    vec![Sublane {
                        kind: Some(SublaneKind::Reads),
                        label: "Reads".to_owned(),
                        top: (top + HEADLESS_TRACK_HEADER_HEIGHT).min(bottom),
                        bottom,
                        collapsed: false,
                    }],
                ));
            }
            reads.content_bottom = self.canvas.height;
        }
        clip_pane(&mut reads);
        Some(PanelLaneLayout {
            panel_id,
            canvas: column,
            gutter_width: self.canvas.gutter_width,
            annotation,
            reads,
        })
    }

    /// The presentation of `panel_id` against the headless lane layout.
    pub fn presentation(&self, panel_id: PanelId) -> Option<PanelPresentation> {
        self.session
            .panel_presentation(panel_id, self.lane_layout(panel_id))
    }

    /// The scene frames last captured for `panel_id`, one per alignment track, in track order.
    pub fn presented_frames(&self, panel_id: PanelId) -> Vec<&AlignmentPanelSceneFrame> {
        self.session
            .alignment_tracks
            .iter()
            .filter_map(|track| self.presented.get(&(panel_id, track.config.id)))
            .collect()
    }

    /// Asks the track engine for the annotation lanes of `panel_id`'s current locus.
    pub fn dispatch_track_query(&mut self, panel_id: PanelId) {
        let session = &mut self.session;
        if !session.panels.panel_state(panel_id).pending_queries.tracks {
            return;
        }
        let Some(engine) = &session.track_engine else {
            session
                .panels
                .panel_state_mut(panel_id)
                .pending_queries
                .tracks = false;
            return;
        };
        let panel = session.panels.panel_state(panel_id);
        let interval = buffered_context_interval(
            panel.viewport,
            panel.contig_length,
            session.settings.read_browsing_buffer_percent,
            None,
        );
        let request = panel.request();
        let track_ids = session
            .tracks
            .tracks()
            .iter()
            .map(|track| track.id)
            .collect::<Vec<_>>();
        let queried = track_ids.iter().copied().collect::<HashSet<_>>();
        let tracks = session.tracks.tracks();
        let variant_settings = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Variant && queried.contains(&track.id))
            .map(|track| (track.id, track.variant.clone()))
            .collect();
        let interaction_settings = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction && queried.contains(&track.id))
            .map(|track| (track.id, track.interaction.clone()))
            .collect();
        let copy_number_settings = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber && queried.contains(&track.id))
            .map(|track| (track.id, track.copy_number.clone()))
            .collect();
        let result = engine.try_query_selected_with_settings(
            request,
            panel.generation_clock.clone(),
            panel.contig_name.clone(),
            interval,
            track_ids,
            variant_settings,
            interaction_settings,
            copy_number_settings,
        );
        let panel = session.panels.panel_state_mut(panel_id);
        match result {
            Ok(()) => {
                panel.pending_queries.tracks = false;
                panel.track_request_interval = Some(interval);
            }
            Err(EngineQueueError::Full) => {}
            Err(EngineQueueError::Disconnected) => {
                panel.pending_queries.tracks = false;
                panel.track_request_interval = None;
                session.report_error(EngineQueueError::Disconnected.to_string());
            }
        }
    }

    fn request_track_query(&mut self, panel_id: PanelId) {
        let has_engine = self.session.track_engine.is_some();
        self.session
            .panels
            .panel_state_mut(panel_id)
            .pending_queries
            .tracks = has_engine;
        self.dispatch_track_query(panel_id);
    }

    fn panel_ids(&self) -> Vec<PanelId> {
        self.session.panels.states().map(|panel| panel.id).collect()
    }

    /// Receives track-engine answers and routes them into the panels.
    pub fn poll_track_events(&mut self) {
        let (events, error) = self.session.receive_track_events();
        if let Some(message) = error {
            self.session.report_error(message);
        }
        for event in events {
            // A track that has opened (or joined) answers queries the panels already made
            // before it existed only when asked again.
            if let crate::track_events::TrackEventRoute::Unrouted(
                TrackEngineEvent::Ready { .. }
                | TrackEngineEvent::TrackAdded { .. }
                | TrackEngineEvent::TracksAdded { .. },
            ) = self.session.route_track_event(event)
            {
                for panel_id in self.panel_ids() {
                    self.request_track_query(panel_id);
                }
            }
        }
    }

    /// One turn of the event loop: settle every engine's answers, then retry whatever a full
    /// queue refused, as the desktop's frame loop does.
    pub fn poll(&mut self) {
        poll_reference_events(self);
        poll_alignment_events(self);
        self.poll_track_events();
        for panel_id in self.panel_ids() {
            let alignment_pending = self.session.alignment_tracks.iter().any(|track| {
                track
                    .projection(panel_id)
                    .is_some_and(|projection| projection.navigation.is_queued())
            });
            if self
                .session
                .panels
                .panel_state(panel_id)
                .navigation_query_schedule
                .pending
                .is_some()
            {
                continue;
            }
            self.session
                .panels
                .panel_state_mut(panel_id)
                .pending_queries
                .alignment = alignment_pending;
            dispatch_alignment_query(self, panel_id);
            self.session.dispatch_reference_query(panel_id);
            self.dispatch_track_query(panel_id);
        }
        self.prepare_alignment_scene_frame();
    }

    /// What `panel_id` still waits for, or `None` once its data has settled.
    pub fn panel_outstanding(&self, panel_id: PanelId) -> Option<String> {
        let session = &self.session;
        let panel = session.panels.state(panel_id)?;
        let mut outstanding = Vec::new();
        for track in &session.alignment_tracks {
            match &track.lifecycle {
                AlignmentTrackLifecycle::Opening => {
                    outstanding.push(format!("alignment {} is opening", track.config.name));
                }
                AlignmentTrackLifecycle::Failed(_) => {}
                AlignmentTrackLifecycle::Ready => {
                    let settled = track.projection(panel_id).is_some_and(|projection| {
                        projection
                            .navigation
                            .is_current_settled(projection.clock.current().get())
                    });
                    if !settled {
                        let phase = track
                            .projection(panel_id)
                            .map(|projection| format!("{:?}", projection.navigation.phase()))
                            .unwrap_or_default();
                        outstanding.push(format!("alignment {} ({phase})", track.config.name));
                    }
                }
            }
        }
        let generation = panel.generation_clock.current().get();
        let reference_settled = reference_dependency_is_settled(
            session.reference_engine.is_some()
                && panel.viewport.bases_per_pixel() <= MAXIMUM_BASE_MARK_BASES_PER_PIXEL,
            generation,
            session.reference_revision,
            panel.reference_slice_revision,
            panel.reference_slice.as_ref().map(|slice| slice.interval),
            panel.viewport.interval(),
            panel.reference_settled_generation,
            panel.reference_query_error_generation,
        );
        if session.reference_engine.is_some() && session.reference_dictionary.is_none() {
            outstanding.push("reference is opening".to_owned());
        } else if !reference_settled {
            outstanding.push("reference bases".to_owned());
        }
        if session.track_engine.is_some() && panel.track_settled_generation != Some(generation) {
            outstanding.push("annotation tracks".to_owned());
        }
        (!outstanding.is_empty()).then(|| outstanding.join(", "))
    }

    /// Drives the session until every panel has settled, or fails after `timeout` naming what
    /// is still outstanding.
    pub fn settle(&mut self, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        loop {
            self.poll();
            let outstanding = self
                .panel_ids()
                .into_iter()
                .filter_map(|panel_id| {
                    self.panel_outstanding(panel_id)
                        .map(|what| format!("panel {}: {what}", panel_id.get()))
                })
                .collect::<Vec<_>>();
            if outstanding.is_empty() {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                bail!(
                    "the view did not settle within {:.1} s; still waiting for {}",
                    timeout.as_secs_f64(),
                    outstanding.join("; ")
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Requests every panel's reference, track and alignment data.
    pub fn request_all_queries(&mut self) {
        for panel_id in self.panel_ids() {
            request_alignment_query(self, panel_id);
            self.session.request_reference_query(panel_id);
            self.request_track_query(panel_id);
        }
    }
}

fn lane(
    owner: LaneOwner,
    channel: TrackChannel,
    name: &str,
    track_kind: TrackKind,
    (left, right, top, bottom): (f32, f32, f32, f32),
    sublanes: Vec<Sublane>,
) -> LaneLayout {
    let row = LayoutRect::from_min_max(left, top, right, bottom);
    let header_bottom = (top + HEADLESS_TRACK_HEADER_HEIGHT).min(bottom);
    LaneLayout {
        owner,
        channel,
        name: name.to_owned(),
        track_kind,
        frame_top: top,
        frame_bottom: bottom,
        row_rect: row,
        header_rect: LayoutRect::from_min_max(left, top, right, header_bottom),
        content_rect: (bottom > header_bottom)
            .then(|| LayoutRect::from_min_max(left, header_bottom, right, bottom)),
        clip_rect: row,
        effective_content_visible: true,
        sublanes,
    }
}

/// Clips every lane to its pane, leaving a lane wholly outside it with no height, as the
/// desktop's pane layout does.
fn clip_pane(pane: &mut PaneLanes) {
    let clip = pane.pane_rect;
    let clip_rect = |rect: LayoutRect| {
        let top = rect.top.clamp(clip.top, clip.bottom.max(clip.top));
        let bottom = rect.bottom.clamp(top, clip.bottom.max(top));
        LayoutRect::from_min_max(rect.left, top, rect.right, bottom)
    };
    for lane in &mut pane.lanes {
        lane.row_rect = clip_rect(lane.row_rect);
        lane.clip_rect = lane.row_rect;
        lane.header_rect = clip_rect(lane.header_rect);
        lane.content_rect = lane
            .content_rect
            .map(clip_rect)
            .filter(|rect| rect.height() > 0.0);
    }
}

impl PresentationHost for HeadlessHost {
    type Panel = PanelState;

    fn session(&self) -> &Session<PanelState> {
        &self.session
    }

    fn session_mut(&mut self) -> &mut Session<PanelState> {
        &mut self.session
    }

    fn panel_lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout> {
        self.lane_layout(panel_id)
    }

    /// Captures each panel's alignment lanes against the current cameras from CPU panel-scene
    /// stores and admits every lane whose semantic scene is complete for its request.
    fn prepare_alignment_scene_frame(&mut self) {
        let mut admissions = Vec::new();
        for panel_id in self.panel_ids() {
            let Some(layout) = self.lane_layout(panel_id) else {
                continue;
            };
            let contig_name = self
                .session
                .panels
                .panel_state(panel_id)
                .contig_name
                .clone();
            for track in &self.session.alignment_tracks {
                let track_id = track.config.id;
                let Some(projection) = track.projection(panel_id) else {
                    continue;
                };
                if projection.index_only_overview || projection.scene_owner.is_none() {
                    continue;
                }
                let Some(frame) = projection.scene_frame.as_ref() else {
                    continue;
                };
                let Some(metadata) = track.scene_metadata.as_ref() else {
                    continue;
                };
                let Some(contig) = metadata.dictionary.id_by_name(&contig_name) else {
                    continue;
                };
                let Some(contig_length) = metadata
                    .dictionary
                    .get(contig)
                    .map(rgv_core::Contig::length)
                else {
                    continue;
                };
                let reference_backed_alleles =
                    projection.scene_contract.as_ref().is_some_and(|contract| {
                        contract.summaries.allele_detail != AlleleSummaryDetail::None
                            && contract.presentation.reference.is_some()
                    });
                let Ok(Some(capture)) = self.session.alignment_scene_lane_capture_for_panel_track(
                    &layout,
                    panel_id,
                    track_id,
                    contig,
                    contig_length,
                    reference_backed_alleles,
                ) else {
                    continue;
                };
                let store = self
                    .scene_stores
                    .entry((panel_id, track_id))
                    .or_insert_with(|| AlignmentPanelSceneStore::new([track_id]));
                if store
                    .admit(
                        track_id,
                        AlignmentPanelLanePublication::new(
                            frame.camera_sequence,
                            frame.snapshot.clone(),
                            frame.scene_contracts_current,
                        ),
                    )
                    .is_err()
                {
                    continue;
                }
                self.camera_revision = self.camera_revision.saturating_add(1);
                let viewport = capture.preparation_camera.viewport();
                let Ok(panel_frame) =
                    store.capture(panel_id, self.camera_revision, viewport, [capture])
                else {
                    continue;
                };
                let complete = panel_frame
                    .lane(track_id)
                    .is_some_and(rgv_alignment_scene::AlignmentPanelLaneFrame::visible_complete)
                    || frame.individual_reads_refused_for_camera();
                if let Some(desired_request_token) = projection.navigation.request_token()
                    && alignment_scene_event_token_is_current(
                        Some(desired_request_token),
                        projection.scene_frame_request_token,
                    )
                {
                    admissions.push(AlignmentLaneAdmission {
                        panel_id,
                        track_id,
                        desired_request_token,
                        source_camera: frame.camera,
                        semantic_target_complete: complete,
                    });
                }
                self.presented.insert((panel_id, track_id), panel_frame);
            }
        }
        self.session.commit_alignment_lane_admissions(&admissions);
    }

    fn clear_alignment_scene_lane(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Result<(), String> {
        if let Some(store) = self.scene_stores.get_mut(&(panel_id, track_id)) {
            store
                .clear_lane(track_id)
                .map_err(|error| error.to_string())?;
        }
        self.presented.remove(&(panel_id, track_id));
        Ok(())
    }

    fn yield_speculation_for_source_pressure(
        &mut self,
        _pressure: LedgerError,
    ) -> Result<LedgerProgressEpoch, LedgerError> {
        self.session
            .alignment_scene_ledger
            .progress()
            .map(rgv_alignment_scene::LedgerProgress::progress_epoch)
    }

    fn observe_capacity_reclaim(
        &mut self,
        _track_id: TrackId,
        _request_id: u64,
        _released_bytes: usize,
        _relinquished_cache_entries: usize,
    ) {
    }

    fn rebuild_all_panel_displays(&mut self) {}

    fn synchronize_alignment_scene_interactions(&mut self, _panel_id: PanelId) {}

    fn mark_display_dirty(&mut self, _reason: &'static str) {}

    fn request_redraw(&mut self) {}

    fn request_all_panel_queries(&mut self) {
        self.request_all_queries();
    }

    fn start_alignment_coverage_engine(&mut self, _track_index: usize) {}

    fn finish_imported_locus(&mut self, _feature_search: Option<String>) {
        self.session.imported_locus = None;
    }

    fn update_landmark_resolution(&mut self, _dictionary: &ReferenceDictionary) {}

    fn refresh_active_panel_context(&mut self) {}

    fn persist_settings(&mut self) {}

    fn observe_reference_remote_io(&mut self, _stats: Option<RemoteIoStats>) {}
}
