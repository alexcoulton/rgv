//! Desktop side of the alignment coverage pyramid (ARCH-1): the per-frame camera statements,
//! the lane labels and the GPU draw of the pyramid. The decisions themselves, the per-panel
//! coverage projection and the worker lifecycle live in `rgv_session::alignment::coverage`.

use std::collections::BTreeMap;
use std::sync::Arc;

use rgv_alignment_scene::{CpuClipRect, CpuSummaryCamera};
use rgv_core::{ContigId, DetailLevel, GenomicInterval, PanelId, TrackId, Viewport};
use rgv_engine::AlignmentCoveragePublication;
use rgv_hts::CoverageSidecarCache;
use rgv_render::{AlignmentDepthPyramidGpuRequest, AlignmentGpuScissor};

pub(crate) use rgv_session::alignment::coverage::*;

use crate::Viewer;
use crate::ui;
use crate::workspace::alignment_management::{
    alignment_clip_gpu_scissor, alignment_scene_covered_clip,
};
use crate::workspace::alignments::AlignmentTrackLifecycle;

/// The coverage subtrack's clip in the final annotation pane layout, or `None` while the lane
/// is hidden, collapsed or scrolled out of view.
pub(crate) fn alignment_coverage_lane_clip(
    layout: &ui::PaneLayoutSnapshot,
    track_id: TrackId,
    horizontal_x: f64,
    horizontal_width: f64,
) -> Option<CpuClipRect> {
    let item = layout.items.iter().find(|item| {
        item.frame.key == ui::TrackFrameKey::Alignment(track_id)
            && item.frame.channel == ui::TrackPane::Annotation
    })?;
    if !item.effective_content_visible || item.clip_rect.height() <= 0.0 || horizontal_width <= 0.0
    {
        return None;
    }
    let subtrack = item.frame.subtracks.iter().find(|subtrack| {
        !subtrack.collapsed && subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Coverage)
    })?;
    let top = (subtrack.top - layout.scroll_points).max(item.clip_rect.top());
    let bottom = (subtrack.bottom - layout.scroll_points).min(item.clip_rect.bottom());
    (bottom > top).then_some(CpuClipRect {
        x: horizontal_x,
        y: f64::from(top),
        width: horizontal_width,
        height: f64::from(bottom - top),
    })
}

/// Pages refused per lane of a prepared pyramid frame, from the per-request refusals: `lanes`
/// names the lane of each request in the order `prepare` received them. Lanes without a
/// refusal are absent, so one refused lane no longer marks every pyramid lane pending.
fn alignment_coverage_pages_refused_by_lane(
    lanes: &[(PanelId, TrackId)],
    frame: &rgv_render::AlignmentDepthPyramidGpuFrame,
) -> BTreeMap<(PanelId, TrackId), usize> {
    sum_refusals_by_lane(lanes, |index| frame.request_pages_refused(index))
}

fn sum_refusals_by_lane(
    lanes: &[(PanelId, TrackId)],
    refused: impl Fn(usize) -> usize,
) -> BTreeMap<(PanelId, TrackId), usize> {
    let mut by_lane = BTreeMap::new();
    for (index, lane) in lanes.iter().enumerate() {
        let pages = refused(index);
        if pages > 0 {
            *by_lane.entry(*lane).or_insert(0) += pages;
        }
    }
    by_lane
}

/// One pyramid lane to draw this frame, owning what the GPU request borrows.
struct OwnedPyramidDraw {
    /// The lane this draw belongs to; a lane split around an exact span has several draws.
    lane: (PanelId, TrackId),
    publication: Arc<AlignmentCoveragePublication>,
    camera: CpuSummaryCamera,
    scissor: AlignmentGpuScissor,
    scale_maximum: u32,
    transform: rgv_core::CoverageScaleTransform,
}

impl Viewer {
    /// The shared sidecar cache, opened on first use while the setting allows it.
    pub(crate) fn alignment_coverage_sidecar(&mut self) -> Option<Arc<CoverageSidecarCache>> {
        if !self.session.settings.coverage_cache_enabled {
            return None;
        }
        if self.alignment_coverage_sidecar.is_none() {
            self.alignment_coverage_sidecar = CoverageSidecarCache::open_default().map(Arc::new);
        }
        self.alignment_coverage_sidecar.clone()
    }

    /// Replaces every track's coverage worker, picking up the current cache setting and
    /// dropping all resident tiles. Old workers are retired without blocking.
    pub(crate) fn respawn_alignment_coverage_engines(&mut self) {
        let sidecar = self.alignment_coverage_sidecar();
        let reference_path = self
            .session
            .reference_source
            .as_ref()
            .map(crate::sources::source_path);
        let remote_reference_index_url = self.session.remote_reference_index_url.clone();
        let ledger = self.session.alignment_scene_ledger.clone();
        for index in 0..self.session.alignment_tracks.len() {
            let (path, open_options) = crate::workspace::alignments::alignment_source_open_request(
                &self.session.alignment_tracks[index].config,
                reference_path.clone(),
                remote_reference_index_url.clone(),
            );
            // A track whose source is not open yet starts its worker when it opens.
            let replacement = match &self.session.alignment_tracks[index].scene_metadata {
                Some(metadata) => spawn_alignment_coverage_engine(
                    path,
                    open_options,
                    ledger.clone(),
                    sidecar.clone(),
                    Some(metadata),
                ),
                None => None,
            };
            let track = &mut self.session.alignment_tracks[index];
            track.coverage_publication_sequence = 0;
            for projection in track.projections.values_mut() {
                projection.coverage.request = None;
                projection.coverage.presented = None;
            }
            if let Some(retired) = std::mem::replace(&mut track.coverage_engine, replacement)
                && let Some(task) = retire_alignment_coverage_engine(retired)
            {
                self.session.alignment_coverage_retirement_tasks.push(task);
            }
        }
        self.alignment_depth_pyramid_gpu_frame = None;
        self.mark_display_dirty("alignment coverage workers replaced");
    }

    /// Starts the coverage worker of the track at `track_index` from its scene engine's source
    /// metadata, when it has none. Called when the scene engine reports the source open.
    pub(crate) fn start_alignment_coverage_engine(&mut self, track_index: usize) {
        if self.session.alignment_tracks[track_index]
            .coverage_engine
            .is_some()
        {
            return;
        }
        let sidecar = self.alignment_coverage_sidecar();
        let reference_path = self
            .session
            .reference_source
            .as_ref()
            .map(crate::sources::source_path);
        let track = &self.session.alignment_tracks[track_index];
        let Some(metadata) = track.scene_metadata.as_ref() else {
            return;
        };
        let (path, open_options) = crate::workspace::alignments::alignment_source_open_request(
            &track.config,
            reference_path,
            self.session.remote_reference_index_url.clone(),
        );
        let engine = spawn_alignment_coverage_engine(
            path,
            open_options,
            self.session.alignment_scene_ledger.clone(),
            sidecar,
            Some(metadata),
        );
        let track = &mut self.session.alignment_tracks[track_index];
        track.coverage_publication_sequence = 0;
        track.coverage_engine = engine;
    }

    /// Applies a change of the coverage-cache setting to the running session.
    pub(crate) fn apply_coverage_cache_setting(&mut self) {
        if !self.session.settings.coverage_cache_enabled {
            self.alignment_coverage_sidecar = None;
        }
        self.respawn_alignment_coverage_engines();
    }

    /// "Clear caches": deletes the persisted coverage tiles and drops the resident ones.
    pub(crate) fn clear_alignment_coverage_caches(&mut self) {
        // The open cache clears itself (and recounts); with the cache closed, the default
        // directory is cleared the same way.
        let cleared = match &self.alignment_coverage_sidecar {
            Some(cache) => Some(cache.clear()),
            None => rgv_hts::default_coverage_cache_directory()
                .filter(|root| root.is_dir())
                .map(|root| clear_coverage_sidecar_directory(&root)),
        };
        match cleared {
            Some(Ok(bytes)) => tracing::info!(bytes, "cleared the coverage cache"),
            Some(Err(error)) => crate::app::shell_state::report_error_to(
                &mut self.session.notifications,
                &mut self.session.data_status,
                format!("Could not clear the coverage cache: {error}"),
            ),
            None => {}
        }
        self.alignment_depth_pyramid_gpu_renderer
            .discard_device_resources();
        self.respawn_alignment_coverage_engines();
    }

    /// The exactly covered interval of the committed scene for `(panel, track)`, when its
    /// contracts are current.
    fn alignment_exact_depth_interval(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> Option<GenomicInterval> {
        let presented = self.presented_alignment_lane(panel_id, track_id)?;
        (presented.lane.scene_contracts_current && presented.lane.snapshot.is_some())
            .then_some(presented.lane.covered_interval)
            .flatten()
    }

    /// The panel's viewport in the track's own contig space, at the live panel width.
    fn alignment_coverage_viewport(
        &self,
        panel_id: PanelId,
        track_index: usize,
    ) -> Option<Viewport> {
        let panel = self.session.panels.get(panel_id)?;
        let dictionary = self
            .session
            .alignment_tracks
            .get(track_index)?
            .dictionary
            .as_ref()?;
        let contig: ContigId = dictionary.id_by_name(&panel.contig_name)?;
        let length = dictionary.get(contig)?.length();
        let viewport = panel.viewport.remapped_contig(contig, length).ok()?;
        match self.panel_column_rect(panel_id) {
            Some(rect) if rect.width() > 0.0 => viewport.resized(f64::from(rect.width())).ok(),
            _ => Some(viewport),
        }
    }

    /// Whether the track's coverage lane has a drawable clip in the panel's final pane layout.
    fn alignment_coverage_lane_on_screen(&self, panel_id: PanelId, track_id: TrackId) -> bool {
        self.session.panels.get(panel_id).is_some_and(|panel| {
            self.panel_column_rect(panel_id).is_some_and(|rect| {
                alignment_coverage_lane_clip(
                    &panel.pane_layout.annotation,
                    track_id,
                    f64::from(rect.left()),
                    f64::from(rect.width()),
                )
                .is_some()
            })
        })
    }

    /// A refinement that keeps the mode (a finer lane of the same authority) changes no label,
    /// so it needs a redraw but no display rebuild.
    fn redraw_on_alignment_coverage_refinement(&mut self) {
        let mut refined = false;
        for track in &mut self.session.alignment_tracks {
            if let Some(engine) = &track.coverage_engine {
                let sequence = engine.publication_sequence();
                refined |= std::mem::replace(&mut track.coverage_publication_sequence, sequence)
                    != sequence;
            }
        }
        if refined {
            self.window.request_redraw();
        }
    }

    /// States every panel's camera to every track's coverage engine and reconciles the mode the
    /// display was built from. Runs every poll: an unchanged request is a no-op in the engine.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn synchronize_alignment_coverage(&mut self) {
        let panel_ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let mut changed = false;
        for track_index in 0..self.session.alignment_tracks.len() {
            if self.session.alignment_tracks[track_index]
                .coverage_engine
                .is_none()
            {
                continue;
            }
            let track_id = self.session.alignment_tracks[track_index].config.id;
            let ready = matches!(
                self.session.alignment_tracks[track_index].lifecycle,
                AlignmentTrackLifecycle::Ready
            );
            let filters = self.alignment_filters_for_track(track_id);
            let eligibility = alignment_coverage_eligibility(
                &filters,
                &self.session.alignment_tracks[track_index].active_filters,
            );
            for panel_id in &panel_ids {
                let panel_id = *panel_id;
                // Only a coverage lane that is on screen asks for coverage. A track scrolled out
                // of view, collapsed or hidden costs no decoding and never opens the worker's
                // own source handle, so descriptors and decode work follow visibility, not the
                // number of loaded tracks.
                let lane_on_screen = ready
                    && filters.show_coverage
                    && self.alignment_coverage_lane_on_screen(panel_id, track_id);
                let viewport = lane_on_screen
                    .then(|| self.alignment_coverage_viewport(panel_id, track_index))
                    .flatten()
                    .filter(|viewport| alignment_coverage_pyramid_zoom(viewport.bases_per_pixel()));
                let owner = alignment_coverage_owner(panel_id);
                let Some(viewport) = viewport else {
                    // Nothing is asked of the pyramid here: forget the owner's request and
                    // publication, and let the zoom alone name the lane's authority.
                    let track = &mut self.session.alignment_tracks[track_index];
                    let released = track
                        .projection_mut(panel_id)
                        .coverage
                        .request
                        .take()
                        .is_some();
                    if released && let Some(engine) = &track.coverage_engine {
                        engine.release(owner);
                    }
                    let coverage = &mut track.projection_mut(panel_id).coverage;
                    if coverage.presented.take().is_some() {
                        changed = true;
                    }
                    continue;
                };
                // Overview zoom never draws the exact lane, so it is never the authority there
                // even while an earlier scene is still retained.
                let exact_ready = viewport.detail_level() != DetailLevel::Overview
                    && exact_read_depth_ready(
                        true,
                        true,
                        self.alignment_exact_depth_interval(panel_id, track_id),
                        viewport.interval(),
                    );
                let pyramid_pages_refused = self
                    .alignment_depth_pyramid_pages_refused
                    .get(&(panel_id, track_id))
                    .copied()
                    .unwrap_or(0);
                let track = &mut self.session.alignment_tracks[track_index];
                let coverage = &mut track.projection_mut(panel_id).coverage;
                let mut request = alignment_coverage_request(
                    coverage.request.as_ref(),
                    viewport.interval(),
                    viewport.bases_per_pixel(),
                    eligibility.eligibility.clone(),
                    exact_ready,
                );
                if coverage.request.is_none() {
                    request.generation = coverage.last_generation.saturating_add(1);
                }
                coverage.last_generation = request.generation;
                coverage.unapplied_view_predicates = eligibility.unapplied_view_predicates;
                coverage.request = Some(request.clone());
                let Some(engine) = &track.coverage_engine else {
                    continue;
                };
                engine.request(owner, request.clone());
                let summary = engine
                    .publication(owner)
                    .map(|publication| CoveragePublicationSummary::of(&publication, &request));
                let mode = alignment_coverage_mode_after_gpu_refusal(
                    select_alignment_coverage_mode(
                        request.bases_per_pixel,
                        exact_ready,
                        request.generation,
                        summary.as_ref(),
                    ),
                    pyramid_pages_refused,
                );
                let current = summary.filter(|summary| summary.generation == request.generation);
                let mate_overlap_not_collapsed = current
                    .as_ref()
                    .is_some_and(|summary| summary.mate_overlap_not_collapsed);
                let failure = current.and_then(|summary| summary.failure);
                let presented = Some((mode, failure));
                let coverage = &mut track.projection_mut(panel_id).coverage;
                if coverage.presented != presented
                    || coverage.mate_overlap_not_collapsed != mate_overlap_not_collapsed
                {
                    coverage.presented = presented;
                    coverage.mate_overlap_not_collapsed = mate_overlap_not_collapsed;
                    changed = true;
                }
            }
        }
        self.redraw_on_alignment_coverage_refinement();
        if changed {
            self.mark_display_dirty("alignment coverage authority changed");
        }
    }

    /// The mode the display of `(panel, track)` is built from.
    pub(crate) fn alignment_coverage_mode(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
    ) -> AlignmentCoverageMode {
        let overview = self
            .session
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.viewport.detail_level() == DetailLevel::Overview);
        self.session
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.coverage.presented.as_ref())
            .map_or(
                if overview {
                    AlignmentCoverageMode::IndexDensity
                } else {
                    AlignmentCoverageMode::ReadEvidence
                },
                |(mode, _)| *mode,
            )
    }

    /// Relabels the coverage subtrack of an alignment annotation frame from the lane's current
    /// authority, and says at overview zoom why the other derived lanes are empty.
    pub(crate) fn label_alignment_coverage_subtrack(
        &self,
        panel_id: PanelId,
        track_id: TrackId,
        frame: &mut ui::TrackFrame,
    ) {
        let overview = self
            .session
            .panels
            .get(panel_id)
            .is_some_and(|panel| panel.viewport.detail_level() == DetailLevel::Overview);
        let coverage = self
            .session
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == track_id)
            .and_then(|track| track.projection(panel_id))
            .map(|projection| &projection.coverage);
        let mode = self.alignment_coverage_mode(panel_id, track_id);
        let failure = coverage
            .and_then(|coverage| coverage.presented.as_ref())
            .and_then(|(_, failure)| failure.as_deref());
        let unapplied = coverage.map_or(0, |coverage| coverage.unapplied_view_predicates);
        let mate_overlap_not_collapsed =
            coverage.is_some_and(|coverage| coverage.mate_overlap_not_collapsed);
        let (label, detail) =
            alignment_coverage_lane_label(mode, failure, unapplied, mate_overlap_not_collapsed);
        for subtrack in &mut frame.subtracks {
            if subtrack.collapsed {
                continue;
            }
            match subtrack.alignment_subtrack {
                Some(ui::AlignmentSubtrack::Coverage) => {
                    subtrack.label.clone_from(&label);
                    subtrack.detail.clone_from(&detail);
                }
                Some(ui::AlignmentSubtrack::Sashimi) if overview => {
                    subtrack.detail = Some("Zoom in to load junctions".to_owned());
                }
                _ => {}
            }
        }
    }

    /// The pyramid lanes this frame draws: one per `(panel, track)` whose coverage lane is
    /// visible and whose mode is a pyramid mode, split around any exactly covered span.
    fn alignment_coverage_pyramid_draws(&self, scale_factor: f32) -> Vec<OwnedPyramidDraw> {
        let mut draws = Vec::<OwnedPyramidDraw>::new();
        for panel in self.session.panels.iter() {
            let Some(panel_rect) = self.panel_column_rect(panel.id) else {
                continue;
            };
            for (track_index, track) in self.session.alignment_tracks.iter().enumerate() {
                let track_id = track.config.id;
                let (Some(engine), Some(projection)) =
                    (&track.coverage_engine, track.projection(panel.id))
                else {
                    continue;
                };
                let Some(request) = &projection.coverage.request else {
                    continue;
                };
                let Some(clip) = alignment_coverage_lane_clip(
                    &panel.pane_layout.annotation,
                    track_id,
                    f64::from(panel_rect.left()),
                    f64::from(panel_rect.width()),
                ) else {
                    continue;
                };
                let Some(viewport) = self.alignment_coverage_viewport(panel.id, track_index) else {
                    continue;
                };
                let Some(publication) = engine.publication(alignment_coverage_owner(panel.id))
                else {
                    continue;
                };
                let overview = viewport.detail_level() == DetailLevel::Overview;
                let exact_interval = (!overview)
                    .then(|| self.alignment_exact_depth_interval(panel.id, track_id))
                    .flatten();
                let exact_ready =
                    exact_read_depth_ready(true, true, exact_interval, viewport.interval());
                let summary = CoveragePublicationSummary::of(&publication, request);
                if !select_alignment_coverage_mode(
                    viewport.bases_per_pixel(),
                    exact_ready,
                    request.generation,
                    Some(&summary),
                )
                .draws_pyramid()
                {
                    continue;
                }
                let Some(lane) = publication.lane.as_deref() else {
                    continue;
                };
                let Ok(camera) = CpuSummaryCamera::from_viewport(viewport, clip) else {
                    continue;
                };
                let filters = self.alignment_filters_for_track(track_id);
                let interval = viewport.interval();
                let scale_maximum = alignment_coverage_pyramid_scale_maximum(
                    filters.coverage_scale_maximum,
                    lane.maximum_depth(interval.start(), interval.end()),
                );
                let exact_clip = exact_interval
                    .and_then(|covered| alignment_scene_covered_clip(viewport, clip, covered));
                for part in alignment_coverage_pyramid_clips(clip, exact_clip) {
                    let Some(scissor) = alignment_clip_gpu_scissor(
                        part,
                        scale_factor,
                        self.surface_config.width,
                        self.surface_config.height,
                    ) else {
                        continue;
                    };
                    draws.push(OwnedPyramidDraw {
                        lane: (panel.id, track_id),
                        publication: Arc::clone(&publication),
                        camera,
                        scissor,
                        scale_maximum,
                        transform: filters.coverage_scale_transform,
                    });
                }
            }
        }
        draws
    }

    /// Captures this frame's pyramid lanes for the GPU. Called once per submitted frame, after
    /// the pane layout is final, so the lanes follow the live camera without a display rebuild.
    pub(crate) fn prepare_alignment_coverage_pyramid_frame(&mut self, scale_factor: f32) {
        let draws = self.alignment_coverage_pyramid_draws(scale_factor);
        if draws.is_empty() {
            self.alignment_depth_pyramid_gpu_frame = None;
            self.note_alignment_coverage_pyramid_pages_refused(BTreeMap::new());
            return;
        }
        let scale = f64::from(scale_factor.max(f32::EPSILON));
        let screen_size_points = [
            (f64::from(self.surface_config.width) / scale) as f32,
            (f64::from(self.surface_config.height) / scale) as f32,
        ];
        let (lanes, requests): (Vec<_>, Vec<_>) = draws
            .iter()
            .filter_map(|draw| {
                Some((
                    draw.lane,
                    AlignmentDepthPyramidGpuRequest {
                        lane: draw.publication.lane.as_deref()?,
                        camera: draw.camera,
                        screen_size_points,
                        scissor: draw.scissor,
                        scale_maximum: draw.scale_maximum,
                        transform: draw.transform,
                    },
                ))
            })
            .unzip();
        self.alignment_depth_pyramid_gpu_frame = match self
            .alignment_depth_pyramid_gpu_renderer
            .prepare(&self.device, &self.queue, &requests)
        {
            Ok((frame, _stats)) => {
                self.note_alignment_coverage_pyramid_pages_refused(
                    alignment_coverage_pages_refused_by_lane(&lanes, &frame),
                );
                Some(frame)
            }
            Err(error) => {
                tracing::error!(%error, "could not prepare the alignment coverage pyramid");
                None
            }
        };
    }

    /// Records which lanes had pyramid pages refused by the GPU ledger this frame. A change is
    /// picked up by the next poll, which presents those lanes (and only those) as pending, or
    /// complete again.
    fn note_alignment_coverage_pyramid_pages_refused(
        &mut self,
        pages_refused: BTreeMap<(PanelId, TrackId), usize>,
    ) {
        if self.alignment_depth_pyramid_pages_refused != pages_refused {
            self.alignment_depth_pyramid_pages_refused = pages_refused;
            self.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interval(contig: u32, start: u64, end: u64) -> GenomicInterval {
        GenomicInterval::new(ContigId::new(contig), start, end).unwrap()
    }

    #[test]
    fn refusals_mark_only_the_lanes_whose_requests_were_refused() {
        let a = (PanelId::new(1), TrackId::new(7));
        let b = (PanelId::new(1), TrackId::new(8));
        let c = (PanelId::new(2), TrackId::new(7));
        // Lane `a` is split around an exact span into two requests; lane `b` had a page refused.
        let lanes = [a, b, a, c];
        let refused = [0, 3, 2, 0];
        let by_lane = sum_refusals_by_lane(&lanes, |index| refused[index]);
        assert_eq!(by_lane.get(&a), Some(&2));
        assert_eq!(by_lane.get(&b), Some(&3));
        assert_eq!(by_lane.get(&c), None);
        assert!(sum_refusals_by_lane(&lanes, |_| 0).is_empty());
    }

    fn annotation_layout(
        track_id: TrackId,
        show_coverage: bool,
        bases_per_pixel: u64,
        scroll_points: f32,
    ) -> ui::PaneLayoutSnapshot {
        let mut config = rgv_core::SessionAlignmentTrack::new(
            track_id,
            "sample".to_owned(),
            rgv_core::TrackSource::Local {
                path: "sample.bam".to_owned(),
            },
        );
        config.filters.show_coverage = show_coverage;
        let viewport = Viewport::new(interval(0, 0, 1_000 * bases_per_pixel), 1_000.0).unwrap();
        let frame = crate::display::alignment::alignment_scene_annotation_frame(
            &config, viewport, 50.0, None, true,
        );
        let row = egui::Rect::from_min_max(
            egui::pos2(0.0, frame.top),
            egui::pos2(1_000.0, frame.bottom),
        );
        let mut layout = ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation);
        layout.scroll_points = scroll_points;
        layout.items.push(ui::PaneItemLayout {
            id: rgv_core::PresentationNodeId::Track {
                channel: rgv_core::TrackChannel::Annotation,
                track_id,
            },
            frame,
            row_rect: row,
            header_rect: row,
            content_rect: Some(row),
            // The pane shows 50..400 points.
            clip_rect: egui::Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(1_000.0, 400.0)),
            desired_content_visible: true,
            effective_content_visible: true,
        });
        layout
    }

    #[test]
    fn the_coverage_lane_exists_at_overview_zoom_and_is_clipped_to_the_pane() {
        let track = TrackId::new(7);
        // Overview (5,000 bp/px) and regional (100 bp/px) zoom expose the same coverage lane.
        for bases_per_pixel in [5_000, 100] {
            let layout = annotation_layout(track, true, bases_per_pixel, 0.0);
            let clip = alignment_coverage_lane_clip(&layout, track, 120.0, 900.0)
                .expect("an expanded coverage lane is drawable");
            assert_eq!((clip.x, clip.width), (120.0, 900.0));
            assert!(clip.y >= 50.0 && clip.y + clip.height <= 400.0);
            assert!(clip.height > 10.0, "{bases_per_pixel} bp/px: {clip:?}");
        }
        let layout = annotation_layout(track, true, 5_000, 0.0);
        // Another track, a zero-width panel, a hidden frame, a lane scrolled out of the pane.
        assert!(alignment_coverage_lane_clip(&layout, TrackId::new(8), 0.0, 900.0).is_none());
        assert!(alignment_coverage_lane_clip(&layout, track, 0.0, 0.0).is_none());
        let mut hidden = layout.clone();
        hidden.items[0].effective_content_visible = false;
        assert!(alignment_coverage_lane_clip(&hidden, track, 0.0, 900.0).is_none());
        let scrolled = annotation_layout(track, true, 5_000, 2_000.0);
        assert!(alignment_coverage_lane_clip(&scrolled, track, 0.0, 900.0).is_none());
        // A collapsed coverage lane (coverage switched off) draws nothing and asks nothing.
        let collapsed = annotation_layout(track, false, 5_000, 0.0);
        assert!(alignment_coverage_lane_clip(&collapsed, track, 0.0, 900.0).is_none());
    }

    #[test]
    fn a_loading_label_does_not_hide_the_coverage_lane_from_hit_testing() {
        let track = TrackId::new(7);
        let mut layout = annotation_layout(track, true, 100, 0.0);
        layout.pane_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 50.0), egui::pos2(1_000.0, 400.0));
        let (top, bottom) = {
            let coverage = layout.items[0]
                .frame
                .subtracks
                .iter_mut()
                .find(|subtrack| {
                    subtrack.alignment_subtrack == Some(ui::AlignmentSubtrack::Coverage)
                })
                .unwrap();
            coverage.label =
                alignment_coverage_lane_label(AlignmentCoverageMode::PyramidCoarse, None, 0, false)
                    .0;
            (coverage.top, coverage.bottom)
        };
        let inside = (top + bottom) * 0.5;
        assert_eq!(
            crate::display::hit_test::alignment_lane_in_pane_layout(&layout, "Coverage", inside),
            Some(track)
        );
        assert_eq!(
            crate::display::hit_test::alignment_lane_in_pane_layout(&layout, "Sashimi", inside),
            None
        );
    }
}
