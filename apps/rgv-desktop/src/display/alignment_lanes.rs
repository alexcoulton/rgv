use crate::display::{
    GenomicDisplayPass, READ_PANE_HEADER_HEIGHT, alignment, alignment_row_activity_overview,
    alignment_scene_read_extent, collapsed_alignment_subtrack,
};
use crate::ui::{AlignmentSubtrack, TrackFrame, TrackFrameKey, TrackSubtrack};
use crate::workspace::alignments::{
    AlignmentPanelProjection, AlignmentTrackController, AlignmentTrackLifecycle,
    alignment_active_filter_count,
};
use crate::workspace::panels::{ReadPanelDisplayCache, ReadRowCoordinateAuthority};
use crate::{MAX_ALIGNMENT_ROW_HEIGHT, PresentedAlignmentLane, Viewer, ui};
use rgv_alignment_scene::AlignmentRowActivityProfile;
use rgv_core::PanelId;
use rgv_core::{AlignmentViewPredicate, DetailLevel, TrackKind};
use rgv_engine::AlignmentIndividualReadCapability;
use rgv_layout::DisplayList;
use std::sync::Arc;

impl Viewer {
    /// Annotation-pane group header and summary frame of the primary
    /// alignment track.
    pub(super) fn push_primary_alignment_annotation_frame(
        &mut self,
        panel_id: PanelId,
        pass: &mut GenomicDisplayPass,
        primary_alignment_name: String,
        primary_source_label: Option<String>,
    ) {
        let primary_alignment_id = pass.primary_alignment_id;
        let primary_annotation_desired = pass.primary_annotation_desired;
        let primary_annotation_effective = pass.primary_annotation_effective;
        let collapsed_annotation_group_members = &pass.collapsed_annotation_group_members;
        let annotation_group_headers_by_anchor = &pass.annotation_group_headers_by_anchor;
        let mut top = pass.top;
        if let Some((group, members, collapsed)) = annotation_group_headers_by_anchor
            .get(&primary_alignment_id)
            .copied()
        {
            let bottom = top + 26.0;
            self.panels
                .panel_mut(panel_id)
                .track_frames
                .push(TrackFrame {
                    key: TrackFrameKey::Group {
                        channel: ui::TrackPane::Annotation,
                        id: group,
                    },
                    channel: ui::TrackPane::Annotation,
                    name: format!("Group {group} · {members} tracks"),
                    kind: TrackKind::Alignment,
                    source_label: None,
                    top,
                    bottom,
                    visible: true,
                    // Group headers are fixed-height controls, not resizable data
                    // tracks. Their eye state comes from TrackGroupChrome.
                    collapsed: true,
                    frozen_bottom: None,
                    subtracks: Vec::new(),
                });
            top = bottom;
            debug_assert_eq!(
                collapsed,
                collapsed_annotation_group_members.contains(&primary_alignment_id)
            );
        }
        if self.primary_alignment_source().is_some()
            && !collapsed_annotation_group_members.contains(&primary_alignment_id)
        {
            // Overview zoom keeps the same lane geometry as regional zoom: the coverage lane is
            // then drawn from the coverage pyramid (or labelled as the index-only placeholder
            // until its first publication) instead of from read evidence.
            let mut frame =
                if let Some(config) = self.alignment_tracks.first().map(|track| &track.config) {
                    alignment::alignment_scene_annotation_frame(
                        config,
                        self.panels.panel(panel_id).viewport,
                        top,
                        self.track_presentation
                            .pane_track_state(ui::TrackPane::Annotation, primary_alignment_id)
                            .and_then(|node| node.preferred_height),
                    )
                } else {
                    unreachable!("the primary source exists only with a primary track")
                };
            if self.panels.panel(panel_id).viewport.detail_level() == DetailLevel::Overview {
                // The overview frame has always carried the primary track's display identity.
                frame.name = primary_alignment_name;
                frame.source_label = primary_source_label;
            }
            self.label_alignment_coverage_subtrack(panel_id, primary_alignment_id, &mut frame);
            if !primary_annotation_effective {
                frame.bottom = frame.top + 26.0;
                frame.subtracks.clear();
                frame.collapsed = !primary_annotation_desired;
            }
            top = frame.bottom;
            self.panels.panel_mut(panel_id).track_frames.push(frame);
        }
        pass.top = top;
    }

    /// Annotation-pane group headers and summary frames of every alignment
    /// track after the primary one.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_secondary_alignment_annotation_frames(
        &mut self,
        panel_id: PanelId,
        pass: &mut GenomicDisplayPass,
    ) {
        let collapsed_annotation_group_members = &pass.collapsed_annotation_group_members;
        let annotation_group_headers_by_anchor = &pass.annotation_group_headers_by_anchor;
        let mut top = pass.top;
        let secondary_tracks = self
            .alignment_tracks
            .iter()
            .skip(1)
            .map(|track| {
                (
                    track.config.clone(),
                    track.lifecycle.clone(),
                    track.scene_engine.is_some(),
                )
            })
            .collect::<Vec<_>>();
        for (config, lifecycle, scene_backed) in secondary_tracks {
            let track_id = config.id;
            let annotation_group_collapsed = collapsed_annotation_group_members.contains(&track_id);
            let annotation_desired = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Annotation, track_id)
                .is_some_and(|node| node.content_visible);
            let annotation_effective = self
                .track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, track_id)
                .unwrap_or(false);
            if let Some((group, members, _)) =
                annotation_group_headers_by_anchor.get(&track_id).copied()
            {
                let bottom = top + 26.0;
                self.panels
                    .panel_mut(panel_id)
                    .track_frames
                    .push(TrackFrame {
                        key: TrackFrameKey::Group {
                            channel: ui::TrackPane::Annotation,
                            id: group,
                        },
                        channel: ui::TrackPane::Annotation,
                        name: format!("Group {group} · {members} tracks"),
                        kind: TrackKind::Alignment,
                        source_label: None,
                        top,
                        bottom,
                        visible: true,
                        collapsed: true,
                        frozen_bottom: None,
                        subtracks: Vec::new(),
                    });
                top = bottom;
            }
            if annotation_group_collapsed {
                continue;
            }

            let mut frame = if scene_backed {
                alignment::alignment_scene_annotation_frame(
                    &config,
                    self.panels.panel(panel_id).viewport,
                    top,
                    self.track_presentation
                        .pane_track_state(ui::TrackPane::Annotation, track_id)
                        .and_then(|node| node.preferred_height),
                )
            } else {
                let bottom = top + 48.0;
                let detail = match lifecycle {
                    AlignmentTrackLifecycle::Failed(message) => Some(message),
                    AlignmentTrackLifecycle::Opening => Some("Opening alignment source".to_owned()),
                    AlignmentTrackLifecycle::Ready => {
                        Some("Unified alignment scene source is unavailable".to_owned())
                    }
                };
                TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    channel: ui::TrackPane::Annotation,
                    name: config.name.clone(),
                    kind: TrackKind::Alignment,
                    source_label: Some(config.source.redacted_label()),
                    top,
                    bottom,
                    visible: true,
                    collapsed: !annotation_desired,
                    frozen_bottom: None,
                    subtracks: vec![TrackSubtrack {
                        alignment_subtrack: None,
                        label: "Unavailable".to_owned(),
                        detail,
                        top: top + 22.0,
                        bottom,
                        collapsed: false,
                        composite_member: None,
                    }],
                }
            };
            self.label_alignment_coverage_subtrack(panel_id, track_id, &mut frame);
            if !annotation_effective {
                frame.bottom = frame.top + 26.0;
                frame.subtracks.clear();
                frame.collapsed = !annotation_desired;
            }
            top = frame.bottom;
            self.panels.panel_mut(panel_id).track_frames.push(frame);
        }
        pass.top = top;
    }

    /// Read-pane frame and scroll display cache entry for every read panel
    /// that the read-pane layout exposes.
    #[allow(clippy::too_many_lines)]
    pub(super) fn push_read_panel_frames(
        &mut self,
        panel_id: PanelId,
        pass: &mut GenomicDisplayPass,
    ) {
        let read_clip_top = pass.read_clip_top;
        let scroll_clip_bottom = pass.scroll_clip_bottom;
        let read_panel_bounds = &pass.read_panel_bounds;
        let read_panel_displays = &mut pass.read_panel_displays;
        // Read-pane controls are a projection of the presentation tree, not
        // of query/render availability. Preserve every exposed restore row
        // while a source opens, has no data at the locus, is unavailable, or
        // has its own/group content gate closed.
        for bounds in read_panel_bounds {
            let alignment::ReadPanelKey::Track(track_id) = bounds.key else {
                continue;
            };
            if self
                .panels
                .panel(panel_id)
                .track_frames
                .iter()
                .any(|frame| {
                    frame.channel == ui::TrackPane::Reads
                        && frame.key == TrackFrameKey::Alignment(track_id)
                })
            {
                continue;
            }
            let Some(track) = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == track_id)
            else {
                continue;
            };
            let desired = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Reads, track_id)
                .is_some_and(|node| node.content_visible);
            let index_only_overview = track
                .projection(panel_id)
                .is_some_and(|projection| projection.index_only_overview)
                || self.panels.panel(panel_id).viewport.detail_level() == DetailLevel::Overview;
            let mapped_record_count = track
                .dictionary
                .as_ref()
                .and_then(|dictionary| {
                    dictionary.id_by_name(&self.panels.panel(panel_id).contig_name)
                })
                .and_then(|contig| {
                    track
                        .index_overview
                        .as_ref()
                        .and_then(|overview| overview.contig_mapped_record_count(contig))
                });
            let projection = track.projection(panel_id);
            let unified_lane_desired = !index_only_overview
                && projection.is_some_and(|projection| {
                    projection.scene_owner.is_some()
                        && projection.navigation.desired_camera().is_some()
                });
            let presented = (!index_only_overview)
                .then(|| self.presented_alignment_lane(panel_id, track_id))
                .flatten();
            let committed_row_activity = (!index_only_overview)
                .then(|| {
                    self.committed_alignment_row_activity_for_desired_target(panel_id, track_id)
                })
                .flatten();
            let row_activity = presented
                .and_then(|presented| {
                    presented
                        .lane
                        .row_activity
                        .as_ref()
                        .filter(|profile| {
                            profile.interval() == presented.source.source_camera.interval()
                        })
                        .cloned()
                })
                .or_else(|| {
                    committed_row_activity
                        .as_ref()
                        .map(|(_, profile)| Arc::clone(profile))
                });
            let native_reads_present =
                presented.is_some_and(|presented| presented.read_authority.is_some());
            let presentation_target_revision = presented.map_or_else(
                || {
                    committed_row_activity
                        .as_ref()
                        .map_or(0, |(target_revision, _)| *target_revision)
                },
                |presented| presented.target_revision,
            );
            let committed_read_capability =
                presented.map(|presented| presented.source.individual_reads);
            let row_coordinate_authority = row_activity.as_ref().map_or_else(
                || {
                    if unified_lane_desired {
                        ReadRowCoordinateAuthority::Pending
                    } else {
                        ReadRowCoordinateAuthority::Absent
                    }
                },
                |profile| ReadRowCoordinateAuthority::Exact(profile.row_coordinate_space()),
            );
            let read_state_message = (!index_only_overview)
                .then(|| {
                    read_panel_state_message(
                        projection,
                        &track.lifecycle,
                        committed_read_capability,
                        native_reads_present,
                    )
                })
                .flatten();
            let bottom = bounds.bottom.max(bounds.top + READ_PANE_HEADER_HEIGHT);
            let content_top = bounds.top + READ_PANE_HEADER_HEIGHT;
            let density = self.alignment_density_for_panel_track(panel_id, track_id);
            let row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            let (total_rows, scene_content_bottom) =
                row_activity.as_deref().map_or((None, bottom), |profile| {
                    let (active_row_end, content_bottom) = alignment_scene_read_extent(
                        profile.active_row_end(),
                        content_top,
                        row_height,
                        bottom,
                    );
                    (Some(active_row_end), content_bottom)
                });
            if desired
                && bottom > content_top
                && bottom > read_clip_top
                && bounds.top < scroll_clip_bottom
            {
                let scrollable = if index_only_overview {
                    alignment::index_only_read_display(
                        self.panel_rect(panel_id).width(),
                        content_top,
                        bottom,
                        mapped_record_count,
                    )
                } else {
                    read_state_message
                        .clone()
                        .map_or_else(DisplayList::default, |message| {
                            alignment::read_state_display(
                                self.panel_rect(panel_id).width(),
                                content_top,
                                bottom,
                                message,
                            )
                        })
                };
                let retained_pending_overview = row_coordinate_authority
                    .is_pending()
                    .then(|| {
                        self.panels
                            .panel(panel_id)
                            .scroll_display_cache
                            .as_ref()
                            .and_then(|cache| {
                                cache.read_panels.iter().find(|panel| {
                                    panel.track_id == track_id
                                        && panel.row_coordinate_authority.is_exact()
                                })
                            })
                            .map(|panel| panel.overview.clone())
                    })
                    .flatten();
                read_panel_displays.push(ReadPanelDisplayCache {
                    track_id,
                    presentation_target_revision,
                    scrollable,
                    density_camera: native_reads_present,
                    row_origin: native_reads_present.then_some(content_top),
                    row_height: native_reads_present.then_some(row_height),
                    row_coordinate_authority,
                    row_activity_atlas_revision: row_activity
                        .as_deref()
                        .map(AlignmentRowActivityProfile::atlas_revision),
                    materialized_row_ranges: None,
                    total_rows,
                    authoritative_overview: match row_activity.as_deref() {
                        Some(profile) => Some(alignment_row_activity_overview(profile)),
                        None if row_coordinate_authority.is_pending() => retained_pending_overview,
                        None => None,
                    },
                    overview: ui::ScrollbarOverview::default(),
                    actual_content_bottom: scene_content_bottom.max(bottom),
                    clip_top: bounds.top.max(read_clip_top),
                    clip_bottom: bottom.min(scroll_clip_bottom),
                });
            }
            let read_frame_name = self.read_panel_frame_name(panel_id, track, presented);
            self.panels
                .panel_mut(panel_id)
                .track_frames
                .push(TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    channel: ui::TrackPane::Reads,
                    name: read_frame_name,
                    kind: TrackKind::Alignment,
                    source_label: Some(track.config.source.redacted_label()),
                    top: bounds.top,
                    bottom,
                    visible: true,
                    collapsed: !desired,
                    frozen_bottom: Some(if desired { content_top } else { bottom }),
                    subtracks: if desired {
                        vec![TrackSubtrack {
                            alignment_subtrack: Some(AlignmentSubtrack::Reads),
                            label: "Reads".to_owned(),
                            detail: read_state_message,
                            top: content_top,
                            bottom,
                            collapsed: false,
                            composite_member: None,
                        }]
                    } else {
                        vec![collapsed_alignment_subtrack(
                            AlignmentSubtrack::Reads,
                            bounds.top,
                        )]
                    },
                });
        }
    }

    /// Read-pane frame title: track name followed by filter, highlight, and
    /// downsampling status.
    fn read_panel_frame_name(
        &self,
        panel_id: PanelId,
        track: &AlignmentTrackController,
        presented: Option<PresentedAlignmentLane<'_>>,
    ) -> String {
        let filter_count = alignment_active_filter_count(&track.config.filters);
        let filter_requires_missing_reference = track
            .config
            .filters
            .alignment_view_filter
            .all
            .iter()
            .any(AlignmentViewPredicate::requires_reference)
            && track.dictionary.as_ref().is_none_or(|dictionary| {
                self.compatible_alignment_reference_revision(dictionary)
                    .is_none()
            });
        let filter_status = if filter_count == 0 {
            String::new()
        } else if filter_requires_missing_reference {
            format!(" · Filters {filter_count}: reference required")
        } else {
            presented
                .filter(|presented| presented.lane.scene_contracts_current)
                .and_then(|presented| presented.lane.snapshot.as_ref())
                .map(|snapshot| snapshot.reads().evidence().view_filter())
                .map(|filter| {
                    format!(
                        " · Filters {filter_count}: {}/{}",
                        filter.matching_records(),
                        filter.eligible_records()
                    )
                })
                .unwrap_or_else(|| format!(" · Filters {filter_count}: updating"))
        };
        let highlight_count = track.config.filters.alignment_highlight_filter.all.len();
        let highlight_status = if highlight_count == 0 {
            String::new()
        } else if let Some(status) = track
            .projection(panel_id)
            .and_then(|projection| projection.scene_highlight_status.as_deref())
        {
            format!(" · {status}")
        } else if let Some(projection) = track
            .projection(panel_id)
            .filter(|projection| projection.scene_highlight_key.is_some())
        {
            format!(
                " · Highlights {highlight_count}: {} reads",
                projection.scene_highlighted.len()
            )
        } else {
            format!(" · Highlights {highlight_count}: updating")
        };
        // Sampling must never be silent: say how many loaded reads hold rows.
        let sampling_status = presented
            .filter(|presented| presented.lane.scene_contracts_current)
            .and_then(|presented| presented.lane.snapshot.as_ref())
            .and_then(|snapshot| snapshot.reads().atlas().downsampled_read_counts())
            .map(|(shown, eligible)| format!(" · Downsampled: {shown} of {eligible} reads shown"))
            .unwrap_or_default();
        // A partially resident picture says so: it is drawn from the pages that fit, not
        // presented as complete.
        let residency_status = crate::workspace::alignment_management::gpu_publication::alignment_partial_residency_status(
            self.alignment_gpu_deferred_read_pages(),
        )
        .map(|status| format!(" · {status}"))
        .unwrap_or_default();
        format!(
            "Reads · {}{filter_status}{highlight_status}{sampling_status}{residency_status}",
            track.config.name
        )
    }
}

/// Status line shown in a read panel while complete reads are not drawn.
fn read_panel_state_message(
    projection: Option<&AlignmentPanelProjection>,
    lifecycle: &AlignmentTrackLifecycle,
    committed_read_capability: Option<AlignmentIndividualReadCapability>,
    native_reads_present: bool,
) -> Option<String> {
    if let Some(message) = projection.and_then(|projection| projection.unavailable.as_deref()) {
        Some(format!("Complete reads unavailable: {message}"))
    } else {
        match lifecycle {
            AlignmentTrackLifecycle::Opening => Some("Opening read source…".to_owned()),
            AlignmentTrackLifecycle::Ready => match committed_read_capability
                .unwrap_or(AlignmentIndividualReadCapability::Pending)
            {
                AlignmentIndividualReadCapability::Complete { .. } if native_reads_present => None,
                AlignmentIndividualReadCapability::Complete { .. } => {
                    Some("Preparing complete reads…".to_owned())
                }
                AlignmentIndividualReadCapability::Pending if native_reads_present => None,
                AlignmentIndividualReadCapability::Pending => {
                    Some("Preparing complete reads…".to_owned())
                }
                AlignmentIndividualReadCapability::Refused {
                    complete_read_limit,
                    eligible_records,
                    ..
                } => Some(format!(
                    "{eligible_records} eligible alignments exceed the Complete read limit of {complete_read_limit}"
                )),
            },
            AlignmentTrackLifecycle::Failed(message) => {
                Some(format!("Read source failed: {message}"))
            }
        }
    }
}
