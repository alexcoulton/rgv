use crate::display::{
    alignment_row_activity_overview, alignment_scene_read_extent,
    reconcile_read_panel_scroll_for_authority,
};
use crate::ui::gestures::{capture_stable_row_camera, vertical_density_camera_padding};
use crate::workspace::alignment_management::scene_authority::{
    CapturedAlignmentPanelSceneEpoch, read_panel_display_derivative_authority,
};
use crate::workspace::alignment_management::uncovered_vertical_candidate_retains_committed_pixels;
use crate::workspace::alignments::AlignmentTrackLifecycle;
use crate::workspace::panels::{
    ReadPanelScrollState, ReadRowCoordinateAuthority, read_row_cache_matches_committed_activity,
};
use crate::{
    AlignmentCommittedLaneAuthority, AlignmentGpuFrameBundle, Viewer,
    alignment_gpu_bundle_is_current, ui,
};
use anyhow::Result;
use rgv_alignment_scene::{
    AlignmentCamera, AlignmentPanelLaneCapture, AlignmentRowActivityProfile, AlleleSummaryDetail,
};
use rgv_core::{ContigId, PanelId, TrackId};
use rgv_session::alignment::admission::AlignmentLaneAdmission;
use rgv_session::lane_layout::PanelLaneLayout;
use std::sync::Arc;

impl Viewer {
    pub(crate) fn request_alignment_query(&mut self, panel_id: PanelId) {
        rgv_session::alignment::dispatch::request_alignment_query(self, panel_id);
    }

    pub(crate) fn request_alignment_scene_camera_update(
        &mut self,
        panel_id: PanelId,
        track_id: TrackId,
    ) {
        rgv_session::alignment::dispatch::request_alignment_scene_camera_update(
            self, panel_id, track_id,
        );
    }

    /// The device-free lane layout of `panel_id` as last committed: its canvas column and both
    /// panes. `None` when the panel has left the layout.
    pub(crate) fn panel_lane_layout(&self, panel_id: PanelId) -> Option<PanelLaneLayout> {
        let panel = self.session.panels.get(panel_id)?;
        let canvas = self.panel_column_rect(panel_id)?;
        Some(panel.pane_layout.to_panel_lanes(
            panel_id,
            canvas,
            self.workspace_layout.track_gutter.width(),
        ))
    }

    /// Resolves the complete live presentation demand for one track from the final lane layout;
    /// see [`rgv_session::Session::alignment_scene_lane_capture_for_panel_track`].
    pub(super) fn alignment_scene_lane_capture_for_panel_track(
        &self,
        layout: Option<&PanelLaneLayout>,
        panel_id: PanelId,
        track_id: TrackId,
        contig: ContigId,
        contig_length: u64,
        reference_backed_alleles: bool,
    ) -> Result<Option<AlignmentPanelLaneCapture>> {
        let Some(layout) = layout else {
            return Ok(None);
        };
        self.session.alignment_scene_lane_capture_for_panel_track(
            layout,
            panel_id,
            track_id,
            contig,
            contig_length,
            reference_backed_alleles,
        )
    }

    /// Reconciles scene cameras after a desktop layout pass. Pane resizing, density changes,
    /// track expansion, and window-scale changes alter only camera/clip state; they must not wait
    /// for an unrelated genomic navigation transaction or rebuild alignment evidence.
    pub(crate) fn refresh_alignment_scene_cameras_after_layout(&mut self) {
        let panel_ids = self
            .session
            .panels
            .iter()
            .map(|panel| panel.id)
            .collect::<Vec<_>>();
        let mut changed_by_panel = Vec::<(PanelId, Vec<TrackId>)>::new();
        for panel_id in panel_ids {
            let Some(panel) = self.session.panels.get(panel_id) else {
                continue;
            };
            let contig_name = panel.contig_name.clone();
            let lane_layout = self.panel_lane_layout(panel_id);
            let changed = self
                .session
                .alignment_tracks
                .iter()
                .filter_map(|track| {
                    if !matches!(track.lifecycle, AlignmentTrackLifecycle::Ready) {
                        return None;
                    }
                    let metadata = track.scene_metadata.as_ref()?;
                    let contig = metadata.dictionary.id_by_name(&contig_name)?;
                    let contig_length = metadata.dictionary.get(contig)?.length();
                    let reference_backed_alleles = track
                        .projection(panel_id)
                        .and_then(|projection| projection.scene_contract.as_ref())
                        .is_some_and(|contract| {
                            contract.summaries.allele_detail != AlleleSummaryDetail::None
                                && contract.presentation.reference.is_some()
                        });
                    let camera = self
                        .alignment_scene_lane_capture_for_panel_track(
                            lane_layout.as_ref(),
                            panel_id,
                            track.config.id,
                            contig,
                            contig_length,
                            reference_backed_alleles,
                        )
                        .ok()
                        .flatten()?
                        .preparation_camera;
                    track
                        .projection(panel_id)
                        .and_then(|projection| projection.navigation.desired_camera())
                        .filter(|current| *current == camera)
                        .is_none()
                        .then_some(track.config.id)
                })
                .collect::<Vec<_>>();
            if !changed.is_empty() {
                changed_by_panel.push((panel_id, changed));
            }
        }
        if changed_by_panel.is_empty() {
            // Workspace geometry, surface scale, or physical extent can change without changing
            // coordinator demand. Still publish (or stale-gate) the native derivative for that
            // exact target instead of leaving an older layout revision implicitly current.
            self.prepare_alignment_scene_frame();
            return;
        }

        for (panel_id, track_ids) in changed_by_panel {
            for track_id in track_ids {
                let Some(track_index) = self
                    .session
                    .alignment_tracks
                    .iter()
                    .position(|track| track.config.id == track_id)
                else {
                    continue;
                };
                self.queue_alignment_navigation_for_track(
                    track_index,
                    panel_id,
                    true,
                    "alignment pane geometry changed",
                );
            }
            self.session
                .panels
                .panel_mut(panel_id)
                .pending_queries
                .alignment = true;
            self.dispatch_alignment_query(panel_id);
        }
    }

    /// Captures every visible alignment lane from one immutable panel-scene epoch and prepares
    /// its disposable GPU derivative. The source worker is never touched here: held pan and
    /// vertical scroll can call this on every input event using already-published pages.
    pub(crate) fn prepare_alignment_scene_frame(&mut self) {
        self.prepare_alignment_scene_frame_atomic();
    }

    pub(super) fn uncovered_vertical_candidate_lane_count(
        &self,
        candidate: &CapturedAlignmentPanelSceneEpoch,
    ) -> usize {
        let Some(committed_epoch) = self.alignment_presented_epoch.as_ref() else {
            return 0;
        };
        let Some(committed_bundle) = committed_epoch.gpu_bundle.as_ref() else {
            return 0;
        };
        if !alignment_gpu_bundle_is_current(
            committed_bundle,
            &self.alignment_scene_publication_authority,
        ) {
            return 0;
        }

        candidate
            .panel_frames
            .iter()
            .flat_map(|(panel_id, candidate_frame)| {
                candidate_frame.lanes.iter().filter(move |candidate_lane| {
                    let Some(committed_lane) = committed_epoch
                        .panel_frames
                        .get(panel_id)
                        .and_then(|frame| frame.lane(candidate_lane.track_id))
                    else {
                        return false;
                    };
                    let committed_native_reads = committed_bundle.reads.is_some()
                        && committed_bundle.read_authorities.iter().any(|authority| {
                            authority.panel_id == *panel_id
                                && authority.track_id == candidate_lane.track_id
                        });
                    candidate_lane.read_camera.is_some()
                        && committed_lane.read_camera.is_some()
                        && uncovered_vertical_candidate_retains_committed_pixels(
                            committed_lane.preparation_camera.viewport(),
                            committed_lane.reads_complete(),
                            committed_native_reads,
                            candidate_lane.preparation_camera.viewport(),
                            &candidate_lane.requested_rows,
                            &candidate_lane.covered_rows,
                        )
                })
            })
            .count()
    }

    pub(super) fn commit_alignment_navigation_transactions(
        &mut self,
        bundle: &AlignmentGpuFrameBundle,
    ) {
        self.session
            .commit_alignment_lane_admissions(&alignment_lane_admissions(&bundle.lane_authorities));
    }

    pub(super) fn wait_alignment_navigation_transactions_for_gpu(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
    ) {
        self.session
            .wait_alignment_lane_admissions_for_gpu(&alignment_lane_admissions(candidates));
    }

    pub(super) fn resume_alignment_navigation_transactions_after_gpu_progress(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
    ) {
        self.session
            .resume_alignment_lane_admissions_after_gpu_progress(&alignment_lane_admissions(
                candidates,
            ));
    }

    pub(super) fn fail_alignment_navigation_gpu_candidates(
        &mut self,
        candidates: &[AlignmentCommittedLaneAuthority],
        message: &str,
    ) {
        self.session
            .fail_alignment_lane_admissions(&alignment_lane_admissions(candidates), message);
    }

    pub(super) fn settle_alignment_navigation_gpu_budget_limited(&mut self, message: &str) {
        self.session
            .settle_alignment_lane_admissions_budget_limited(message);
    }

    /// Ensures the scroll/display derivative catches up after the semantic/native epoch commits.
    /// Worker polling historically rebuilt this cache before GPU publication, which could leave
    /// a Pending or old-target full thumb beside newly committed exact pixels indefinitely.
    pub(super) fn invalidate_stale_alignment_scroll_cache(&mut self) {
        struct CommittedReadCacheUpdate {
            panel_id: PanelId,
            track_id: TrackId,
            target_revision: u64,
            camera: Option<AlignmentCamera>,
            row_activity: Option<Arc<AlignmentRowActivityProfile>>,
            native_reads_present: bool,
            materialized_row_ranges: Option<Arc<[(usize, usize)]>>,
        }

        let expected = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| {
                let bundle = epoch.gpu_bundle.as_ref()?;
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
                    .then(|| {
                        epoch
                            .panel_frames
                            .values()
                            .flat_map(|frame| {
                                frame.lanes.iter().map(|lane| {
                                    let read_authority =
                                        bundle.read_authorities.iter().find(|authority| {
                                            authority.panel_id == frame.panel_id
                                                && authority.track_id == lane.track_id
                                                && authority.target_revision
                                                    == bundle.target_revision
                                        });
                                    let native_reads_present = read_authority.is_some();
                                    let materialized_row_ranges = read_authority.map(|authority| {
                                        Arc::clone(&authority.materialized_row_ranges)
                                    });
                                    CommittedReadCacheUpdate {
                                        panel_id: frame.panel_id,
                                        track_id: lane.track_id,
                                        target_revision: bundle.target_revision,
                                        camera: lane.read_camera,
                                        row_activity: lane.row_activity.as_ref().map(Arc::clone),
                                        native_reads_present,
                                        materialized_row_ranges,
                                    }
                                })
                            })
                            .collect::<Vec<_>>()
                    })
            })
            .unwrap_or_default();
        let primary_track_id = self.primary_alignment_track_id();
        let mut needs_rebuild = false;
        let record_cache_mismatches = self.diagnostics.recording();
        let mut cache_mismatches = Vec::new();
        for update in expected {
            let density = self.alignment_density_for_panel_track(update.panel_id, update.track_id);
            let density_drag_fraction = self
                .vertical_overview_drag
                .filter(|drag| {
                    drag.panel_id == update.panel_id
                        && drag.track_id == update.track_id
                        && drag.density_dragging
                })
                .map(|drag| drag.virtual_scroll_fraction);
            let Some(panel) = self.session.panels.get_mut(update.panel_id) else {
                needs_rebuild = true;
                continue;
            };
            let Some(read_panel_index) = panel.scroll_display_cache.as_ref().and_then(|cache| {
                cache
                    .read_panels
                    .iter()
                    .position(|read_panel| read_panel.track_id == update.track_id)
            }) else {
                // A summary-only/hidden read lane with no cached read panel is already absent.
                needs_rebuild |= update.camera.is_some();
                continue;
            };
            let display_derivative_before =
                read_panel_display_derivative_authority(panel, read_panel_index, update.track_id);

            let activity_identity = update.row_activity.as_deref().map(|profile| {
                (
                    profile.row_coordinate_space(),
                    profile.atlas_revision(),
                    profile.active_row_end(),
                )
            });
            let cache_was_current = {
                let read_panel = &panel
                    .scroll_display_cache
                    .as_ref()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let current = if update.camera.is_none() {
                    read_panel.presentation_target_revision == update.target_revision
                        && read_panel.row_coordinate_authority == ReadRowCoordinateAuthority::Absent
                        && read_panel.row_activity_atlas_revision.is_none()
                        && read_panel.total_rows.is_none()
                } else {
                    read_row_cache_matches_committed_activity(
                        read_panel.presentation_target_revision,
                        read_panel.row_coordinate_authority,
                        read_panel.row_activity_atlas_revision,
                        read_panel.total_rows,
                        update.target_revision,
                        activity_identity,
                    )
                };
                current
            };
            if record_cache_mismatches && !cache_was_current {
                let read_panel = &panel
                    .scroll_display_cache
                    .as_ref()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let cached_authority = serde_json::json!({
                    "presentation_target_revision": read_panel.presentation_target_revision,
                    "raw_row_coordinate_authority": format!("{:?}", read_panel.row_coordinate_authority),
                    "row_activity_atlas_revision": read_panel.row_activity_atlas_revision.map(|revision| format!("{revision:?}")),
                    "total_rows": read_panel.total_rows,
                    "materialized_row_ranges": read_panel.materialized_row_ranges.as_deref(),
                });
                cache_mismatches.push(serde_json::json!({
                    "panel_id": update.panel_id.get(),
                    "track_id": update.track_id.get(),
                    "desired_target_revision": self.alignment_scene_publication_authority.desired_target_revision(),
                    "committed_target_revision": self.alignment_scene_publication_authority.target_revision,
                    "committed_row_target_matches_desired": self.alignment_scene_publication_authority.committed_row_target_matches_desired(update.panel_id, update.track_id),
                    "incoming_target_revision": update.target_revision,
                    "incoming_camera_present": update.camera.is_some(),
                    "incoming_native_reads_present": update.native_reads_present,
                    "incoming_row_activity_identity": activity_identity.map(|(coordinate_space, atlas_revision, rows)| serde_json::json!({
                        "coordinate_space": format!("{coordinate_space:?}"),
                        "atlas_revision": format!("{atlas_revision:?}"),
                        "active_rows": rows,
                    })),
                    "cached": cached_authority,
                }));
            }

            let Some(camera) = update.camera else {
                // A committed summary-only lane has no live read coordinate space. Clear current
                // scrollbar authority while retaining the panel's latent camera for restoration
                // if Reads is expanded again.
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.density_camera = false;
                read_panel.row_origin = None;
                read_panel.row_height = None;
                read_panel.row_coordinate_authority = ReadRowCoordinateAuthority::Absent;
                read_panel.row_activity_atlas_revision = None;
                read_panel.materialized_row_ranges = None;
                read_panel.total_rows = None;
                read_panel.authoritative_overview = None;
                read_panel.overview = ui::ScrollbarOverview::default();
                read_panel.actual_content_bottom = read_panel.clip_bottom;
                needs_rebuild |= display_derivative_before
                    != read_panel_display_derivative_authority(
                        panel,
                        read_panel_index,
                        update.track_id,
                    );
                continue;
            };

            let Some(profile) = update.row_activity.as_deref() else {
                // Preserve the last exact extent/overview as a visible waypoint, but revoke exact
                // gesture and hit authority synchronously. The exact successor will atomically
                // replace these provisional metrics.
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.row_coordinate_authority = ReadRowCoordinateAuthority::Pending;
                read_panel.row_activity_atlas_revision = None;
                needs_rebuild |= display_derivative_before
                    != read_panel_display_derivative_authority(
                        panel,
                        read_panel_index,
                        update.track_id,
                    );
                continue;
            };

            let row_height = camera.row_height() as f32;
            let row_origin = camera.clip().y as f32;
            let authoritative_overview = alignment_row_activity_overview(profile);
            let (clip_top, clip_bottom, actual_content_bottom, row_coordinate_space) = {
                let read_panel = &mut panel
                    .scroll_display_cache
                    .as_mut()
                    .expect("read panel index came from this cache")
                    .read_panels[read_panel_index];
                let (_, actual_content_bottom) = alignment_scene_read_extent(
                    profile.active_row_end(),
                    row_origin,
                    row_height,
                    read_panel.clip_bottom,
                );
                read_panel.presentation_target_revision = update.target_revision;
                read_panel.density_camera = update.native_reads_present;
                read_panel.row_origin = update.native_reads_present.then_some(row_origin);
                read_panel.row_height = update.native_reads_present.then_some(row_height);
                read_panel.row_coordinate_authority =
                    ReadRowCoordinateAuthority::Exact(profile.row_coordinate_space());
                read_panel.row_activity_atlas_revision = Some(profile.atlas_revision());
                read_panel.materialized_row_ranges = update.materialized_row_ranges;
                read_panel.total_rows = Some(profile.active_row_end());
                read_panel.authoritative_overview = Some(authoritative_overview.clone());
                read_panel.overview = authoritative_overview;
                read_panel.actual_content_bottom = actual_content_bottom;
                (
                    read_panel.clip_top,
                    read_panel.clip_bottom,
                    actual_content_bottom,
                    profile.row_coordinate_space(),
                )
            };

            let compatibility_primary = ReadPanelScrollState {
                scroll_points: panel.vertical_scroll_points,
                content_origin_points: panel.vertical_content_origin_points,
                latent_scroll_fraction: panel.latent_vertical_scroll_fraction,
                content_bottom_points: panel.content_bottom_points,
                density: None,
                stable_row_anchor: None,
                scene_contig: None,
            };
            let state = panel
                .state
                .read_panel_scrolls
                .entry(update.track_id)
                .or_insert_with(|| {
                    if update.track_id == primary_track_id {
                        compatibility_primary
                    } else {
                        ReadPanelScrollState::default()
                    }
                });
            let camera_padding = if update.native_reads_present {
                vertical_density_camera_padding((clip_bottom - clip_top).max(1.0), density)
            } else {
                0.0
            };
            let exact_row_camera = reconcile_read_panel_scroll_for_authority(
                state,
                ReadRowCoordinateAuthority::Exact(row_coordinate_space),
                update.native_reads_present.then_some(row_height),
                actual_content_bottom,
                clip_bottom,
                camera_padding,
                density_drag_fraction,
            );
            if let Some((settled_row_height, coordinate_space)) = exact_row_camera
                && settled_row_height > f32::EPSILON
            {
                state.stable_row_anchor = capture_stable_row_camera(
                    state.effective_scroll_points(),
                    settled_row_height,
                    coordinate_space,
                );
            }
            if update.track_id == primary_track_id {
                panel.state.vertical_scroll_points = state.scroll_points;
                panel.state.vertical_content_origin_points = state.content_origin_points;
                panel.state.latent_vertical_scroll_fraction = state.latent_scroll_fraction;
                panel.state.content_bottom_points = state.content_bottom_points;
            }
            needs_rebuild |= display_derivative_before
                != read_panel_display_derivative_authority(
                    panel,
                    read_panel_index,
                    update.track_id,
                );
        }
        for mismatch in cache_mismatches {
            self.diagnostics.event(
                "display_invalidation",
                "scroll_cache_authority_mismatch",
                mismatch,
            );
        }
        if needs_rebuild {
            self.mark_display_dirty("committed scroll-cache authority changed");
            self.window.request_redraw();
        }
    }
}

/// The session's view of committed-lane authorities: which request and camera each answers.
fn alignment_lane_admissions(
    authorities: &[AlignmentCommittedLaneAuthority],
) -> Vec<AlignmentLaneAdmission> {
    authorities
        .iter()
        .map(|authority| AlignmentLaneAdmission {
            panel_id: authority.panel_id,
            track_id: authority.track_id,
            desired_request_token: authority.desired_request_token,
            source_camera: authority.source_camera,
            semantic_target_complete: authority.semantic_target_complete,
        })
        .collect()
}
