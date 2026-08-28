pub(crate) mod alignment;
pub(crate) mod auxiliary;
pub(crate) mod hit_test;

use super::*;

pub(super) const COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT: f32 = 20.0;
pub(super) const READ_PANE_HEADER_HEIGHT: f32 = ui::TRACK_HEADER_HEIGHT;
pub(super) const PINNED_ALIGNMENT_REFERENCE_HEIGHT: f32 = 28.0;
pub(super) const PINNED_ALIGNMENT_COVERAGE_HEIGHT: f32 = 48.0;
pub(super) const PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT: f32 = 52.0;
pub(super) const PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT: f32 = 120.0;
pub(super) const PINNED_ALIGNMENT_MODIFICATION_HEIGHT: f32 = 26.0;
pub(crate) const ANNOTATION_PANE_BACKGROUND: Rgba = Rgba::new(0.953, 0.969, 0.984, 1.0);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AlignmentAnnotationLaneHeights {
    pub(crate) coverage: f32,
    pub(crate) junction: f32,
    pub(crate) modification: f32,
}

pub(crate) fn alignment_annotation_lane_heights(
    filters: &SessionFilters,
) -> AlignmentAnnotationLaneHeights {
    AlignmentAnnotationLaneHeights {
        coverage: PINNED_ALIGNMENT_COVERAGE_HEIGHT,
        junction: match filters.junction_display_mode {
            JunctionDisplayMode::Collapsed => PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT,
            JunctionDisplayMode::Expanded => PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT,
        },
        modification: PINNED_ALIGNMENT_MODIFICATION_HEIGHT,
    }
}

pub(crate) fn append_annotation_pane_background(
    display: &mut DisplayList,
    width: f32,
    top: f32,
    bottom: f32,
) {
    if width <= 0.0 || bottom <= top {
        return;
    }
    display.rectangles.push(RectPrimitive {
        x: 0.0,
        y: top,
        width,
        height: bottom - top,
        color: ANNOTATION_PANE_BACKGROUND,
        object_id: 0,
        shape: Default::default(),
    });
}

pub(crate) fn reference_bases_label(reverse_complement: bool) -> String {
    if reverse_complement {
        "< Bases · reverse strand".to_owned()
    } else {
        "> Bases · forward".to_owned()
    }
}

pub(super) fn collapsed_alignment_subtrack(label: &str, top: f32) -> TrackSubtrack {
    TrackSubtrack {
        label: label.to_owned(),
        detail: None,
        top,
        bottom: top + COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT,
        collapsed: true,
        composite_member: None,
    }
}

pub(super) fn primary_pinned_gene_id(tracks: &[TrackConfig]) -> Option<TrackId> {
    tracks
        .iter()
        .find(|track| track.visible && track.kind == TrackKind::GeneModel)
        .map(|track| track.id)
}

pub(super) fn pinned_gene_track_height(track: &TrackConfig) -> f32 {
    if track.collapsed {
        26.0
    } else {
        track.height.clamp(40.0, 1_200.0)
    }
}

fn pane_layout_item_id(frame: &TrackFrame) -> PresentationNodeId {
    match frame.key {
        TrackFrameKey::Reference => PresentationNodeId::Reference,
        TrackFrameKey::Group { channel, id } => PresentationNodeId::Group {
            channel,
            group_id: id,
        },
        TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
            PresentationNodeId::Track {
                channel: frame.channel,
                track_id,
            }
        }
    }
}

fn translate_track_frame(frame: &mut TrackFrame, delta: f32) {
    frame.top += delta;
    frame.bottom += delta;
    frame.frozen_bottom = frame.frozen_bottom.map(|value| value + delta);
    for subtrack in &mut frame.subtracks {
        subtrack.top += delta;
        subtrack.bottom += delta;
    }
}

/// Reconciles renderer output with the canonical annotation tree. Renderers
/// are allowed to omit data content while a source opens, is unavailable at
/// the locus, or changes implementation; they are not allowed to erase that
/// source's presentation control. Composite members count as represented by
/// the parent row that owns their subtrack controls.
pub(crate) fn ensure_annotation_presentation_frames(
    presentation: &WorkspacePresentation,
    frames: &mut Vec<TrackFrame>,
    fallback_frames: &[TrackFrame],
    mut fallback_top: f32,
) -> f32 {
    let mut represented = frames
        .iter()
        .filter(|frame| frame.channel == ui::TrackPane::Annotation)
        .map(pane_layout_item_id)
        .collect::<BTreeSet<_>>();
    represented.extend(
        frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Annotation)
            .flat_map(|frame| frame.subtracks.iter())
            .filter_map(|subtrack| subtrack.composite_member)
            .map(|track_id| PresentationNodeId::Track {
                channel: ui::TrackPane::Annotation,
                track_id,
            }),
    );
    let fallbacks = fallback_frames
        .iter()
        .filter(|frame| frame.channel == ui::TrackPane::Annotation)
        .map(|frame| (pane_layout_item_id(frame), frame))
        .collect::<BTreeMap<_, _>>();

    for node in presentation.pane_interactive_nodes(ui::TrackPane::Annotation) {
        if represented.contains(&node) {
            continue;
        }
        let Some(fallback) = fallbacks.get(&node) else {
            continue;
        };
        let mut fallback = (*fallback).clone();
        fallback.top = fallback_top;
        fallback.bottom = fallback_top + READ_PANE_HEADER_HEIGHT.max(26.0);
        fallback.frozen_bottom = None;
        fallback.subtracks.clear();
        fallback_top = fallback.bottom;
        frames.push(fallback);
        represented.insert(node);
    }
    fallback_top
}

/// Reflows rendered annotation rows into presentation-tree order without
/// changing controller/source order. Layout producers may emit their natural
/// geometry independently; this composition boundary translates both their
/// primitives and chrome as one immutable segment.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn reflow_annotation_presentation(
    presentation: &WorkspacePresentation,
    frames: &mut Vec<TrackFrame>,
    display: &mut DisplayList,
    pane_top: f32,
) -> f32 {
    reflow_annotation_presentation_with_heights(
        presentation,
        frames,
        display,
        pane_top,
        &BTreeMap::new(),
    )
}

/// Reflows annotation rows using the workspace-wide height allocated to each
/// presentation node. A locus may render less content at its current LOD, but
/// it still owns the same row geometry as every other locus.
pub(crate) fn reflow_annotation_presentation_with_heights(
    presentation: &WorkspacePresentation,
    frames: &mut Vec<TrackFrame>,
    display: &mut DisplayList,
    pane_top: f32,
    shared_heights: &BTreeMap<PresentationNodeId, f32>,
) -> f32 {
    let mut annotation = frames
        .iter()
        .filter(|frame| frame.channel == ui::TrackPane::Annotation)
        .cloned()
        .collect::<Vec<_>>();
    if annotation.is_empty() {
        return pane_top;
    }
    let mut order = BTreeMap::new();
    let mut index = 0_usize;
    if annotation
        .iter()
        .any(|frame| frame.key == TrackFrameKey::Reference)
    {
        order.insert(PresentationNodeId::Reference, index);
        index += 1;
    }
    for node in presentation.pane_interactive_nodes(ui::TrackPane::Annotation) {
        order.entry(node).or_insert_with(|| {
            let current = index;
            index += 1;
            current
        });
    }
    annotation.sort_by_key(|frame| {
        order
            .get(&pane_layout_item_id(frame))
            .copied()
            .unwrap_or(usize::MAX)
    });

    let mut cursor = pane_top;
    let mut translations = Vec::with_capacity(annotation.len());
    for frame in &mut annotation {
        let old_top = frame.top;
        let old_bottom = frame.bottom;
        let natural_height = (old_bottom - old_top).max(0.0);
        let height = shared_heights
            .get(&pane_layout_item_id(frame))
            .copied()
            .map_or(natural_height, |shared| shared.max(natural_height));
        let delta = cursor - old_top;
        translate_track_frame(frame, delta);
        frame.bottom = frame.top + height;
        translations.push((old_top, old_bottom, delta));
        cursor += height;
    }
    let translation_at = |y: f32| {
        translations
            .iter()
            .find(|(top, bottom, _)| y >= *top && y < *bottom)
            .map_or(0.0, |(_, _, delta)| *delta)
    };
    for rectangle in &mut display.rectangles {
        rectangle.y += translation_at(rectangle.y + rectangle.height * 0.5);
    }
    for text in &mut display.texts {
        text.y += translation_at(text.y);
    }

    frames.retain(|frame| frame.channel != ui::TrackPane::Annotation);
    frames.extend(annotation);
    cursor
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn pane_layout_snapshot(
    presentation: &WorkspacePresentation,
    frames: &[TrackFrame],
    channel: ui::TrackPane,
    pane_top: f32,
    pane_bottom: f32,
    content_bottom: f32,
    scroll_points: f32,
    stack_scroll_points: f32,
    width: f32,
) -> ui::PaneLayoutSnapshot {
    let pane_rect = egui::Rect::from_min_max(
        egui::pos2(0.0, pane_top),
        egui::pos2(width.max(1.0), pane_bottom.max(pane_top)),
    );
    let items = frames
        .iter()
        .filter(|frame| frame.channel == channel)
        .map(|frame| {
            let id = pane_layout_item_id(frame);
            let composite = frame
                .subtracks
                .iter()
                .any(|subtrack| subtrack.composite_member.is_some());
            let (desired_content_visible, effective_content_visible) = match id {
                PresentationNodeId::Reference => {
                    let desired = presentation.reference_content_visible;
                    (desired, desired && frame.visible)
                }
                PresentationNodeId::Track { channel, track_id } => {
                    if composite {
                        (frame.visible, frame.visible && !frame.collapsed)
                    } else {
                        let desired = presentation
                            .pane_track_state(channel, track_id)
                            .is_some_and(|node| node.content_visible);
                        let effective = presentation
                            .effective_track_visibility(channel, track_id)
                            .unwrap_or(false)
                            && frame.visible;
                        (desired, effective)
                    }
                }
                PresentationNodeId::Group { channel, group_id } => presentation
                    .groups
                    .iter()
                    .find(|group| group.channel == channel && group.id == group_id)
                    .map_or((false, false), |group| {
                        (group.content_visible, group.content_visible)
                    }),
            };
            ui::PaneItemLayout {
                id,
                frame: frame.clone(),
                row_rect: egui::Rect::ZERO,
                header_rect: egui::Rect::ZERO,
                content_rect: None,
                clip_rect: egui::Rect::ZERO,
                desired_content_visible,
                effective_content_visible,
            }
        })
        .collect();
    let mut snapshot = ui::PaneLayoutSnapshot {
        channel,
        pane_rect,
        content_bottom,
        scroll_points: 0.0,
        stack_scroll_points,
        items,
    };
    snapshot.reproject_scroll(scroll_points);
    snapshot
}

impl Viewer {
    pub(crate) fn ensure_alignment_rows(&mut self) {
        let panel_id = self.panels.active().id;
        let primary_id = self.primary_alignment_track_id();
        let filters = self.primary_alignment_filters().clone();
        let Some(query) = self.alignment_query_for_track(primary_id).cloned() else {
            return;
        };
        let minimum_rows = query.batch.len().max(1);
        let max_rows = self
            .alignment_tracks
            .first()
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.alignment_rows.as_ref())
            .filter(|cached| cached.max_rows >= cached.query.batch.len().max(1))
            .map_or(minimum_rows, |cached| cached.max_rows.max(minimum_rows));
        let pair_length_bounds = self.effective_template_length_bounds(&query);
        let sort_position =
            alignment_sort_position(filters.alignment_sort_mode, self.panels.active().viewport);
        let promoted_read_name_hash = self
            .promoted_read
            .filter(|(track_id, _)| *track_id == primary_id)
            .map(|(_, hash)| hash);
        let rows_match = self
            .alignment_tracks
            .first()
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.alignment_rows.as_ref())
            .is_some_and(|cached| {
                Arc::ptr_eq(&cached.query, &query)
                    && cached.group_mode == filters.alignment_group_mode
                    && cached.group_order == filters.alignment_group_order
                    && cached.sort_mode == filters.alignment_sort_mode
                    && cached.sort_order == filters.alignment_sort_order
                    && cached.sort_position == sort_position
                    && cached.promoted_read_name_hash == promoted_read_name_hash
                    && cached.max_rows == max_rows
                    && cached.view_as_pairs == filters.view_as_pairs
                    && cached.pair_length_bounds == pair_length_bounds
            });
        if rows_match {
            return;
        }
        let options = AlignmentLayoutOptions {
            group_mode: filters.alignment_group_mode,
            group_order: filters.alignment_group_order,
            sort_mode: filters.alignment_sort_mode,
            sort_order: filters.alignment_sort_order,
            sort_position,
            promoted_read_name_hash,
            max_rows,
            view_as_pairs: filters.view_as_pairs,
            pair_length_bounds,
            ..AlignmentLayoutOptions::default()
        };
        let previous = self
            .alignment_tracks
            .first()
            .and_then(|track| track.projection(panel_id))
            .and_then(|projection| projection.alignment_rows.as_ref())
            .and_then(|cached| {
                (cached.group_mode == filters.alignment_group_mode
                    && cached.group_order == filters.alignment_group_order
                    && cached.sort_mode == filters.alignment_sort_mode
                    && cached.sort_order == filters.alignment_sort_order
                    && cached.sort_position == sort_position
                    && cached.promoted_read_name_hash == promoted_read_name_hash
                    && cached.view_as_pairs == filters.view_as_pairs
                    && cached.pair_length_bounds == pair_length_bounds)
                    .then_some(cached.layout.as_ref())
            });
        let layout = Arc::new(assign_alignment_rows(&query, options, previous));
        if let Some(track) = self.alignment_tracks.first_mut() {
            track.projection_mut(panel_id).alignment_rows = Some(CachedAlignmentRows {
                query,
                layout,
                group_mode: filters.alignment_group_mode,
                group_order: filters.alignment_group_order,
                sort_mode: filters.alignment_sort_mode,
                sort_order: filters.alignment_sort_order,
                sort_position,
                promoted_read_name_hash,
                max_rows,
                view_as_pairs: filters.view_as_pairs,
                pair_length_bounds,
            });
        }
        self.layout_cache.clear();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn rebuild_genomic_display(&mut self) {
        self.ensure_alignment_rows();
        let shared_sashimi_raw = self.shared_sashimi_maximum(SashimiCountMode::Raw);
        let shared_sashimi_cpm = self.shared_sashimi_maximum(SashimiCountMode::CountsPerMillion);
        let primary_alignment_id = self.primary_alignment_track_id();
        let primary_alignment_name = self.alignment_track_name();
        let primary_source_label = self
            .primary_alignment_source()
            .map(TrackSource::redacted_label);
        let mut annotation_display = DisplayList::default();
        let mut pinned_display = DisplayList::default();
        let geometry = self.canvas_geometry();
        let annotation_section_expanded = self.track_presentation.annotation_section_expanded;
        let read_section_expanded = self.track_presentation.read_section_expanded;
        let section_header_height = ui::TRACK_SECTION_HEADER_HEIGHT;
        let annotation_clip_top =
            (geometry.viewport.top() + section_header_height).min(geometry.viewport.bottom());
        let mut top = annotation_clip_top;
        let viewport_height = geometry.viewport.height().max(1.0);
        let configured_read_height = self
            .track_presentation
            .read_pane_height
            .clamp(100.0, (viewport_height - 80.0).max(100.0));
        let default_annotation_clip_bottom =
            (geometry.viewport.bottom() - configured_read_height).max(annotation_clip_top + 54.0);
        let annotation_clip_bottom = match (annotation_section_expanded, read_section_expanded) {
            (false, _) => annotation_clip_top,
            (true, false) => {
                (geometry.viewport.bottom() - section_header_height).max(annotation_clip_top)
            }
            (true, true) => default_annotation_clip_bottom,
        }
        .clamp(annotation_clip_top, geometry.viewport.bottom());
        let read_clip_top =
            (annotation_clip_bottom + section_header_height).min(geometry.viewport.bottom());
        let scroll_clip_bottom = geometry.viewport.bottom();
        let alignment_track_groups = self.alignment_track_group_chrome();
        let annotation_track_groups = alignment_track_groups
            .iter()
            .filter(|group| group.pane == ui::TrackPane::Annotation)
            .collect::<Vec<_>>();
        let read_track_groups = alignment_track_groups
            .iter()
            .filter(|group| group.pane == ui::TrackPane::Reads)
            .collect::<Vec<_>>();
        let collapsed_annotation_group_members = annotation_track_groups
            .iter()
            .filter(|group| group.collapsed)
            .flat_map(|group| group.members.iter().copied())
            .collect::<BTreeSet<_>>();
        let annotation_group_headers_by_anchor = annotation_track_groups
            .iter()
            .map(|group| {
                (
                    group.anchor,
                    (group.id, group.members.len(), group.collapsed),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let collapsed_read_group_members = read_track_groups
            .iter()
            .filter(|group| group.collapsed)
            .flat_map(|group| group.members.iter().copied())
            .collect::<BTreeSet<_>>();
        let read_group_headers_by_anchor = read_track_groups
            .iter()
            .map(|group| {
                (
                    group.anchor,
                    (group.id, group.members.len(), group.collapsed),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let primary_source_available = self.primary_alignment_source().is_some();
        let primary_annotation_desired = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Annotation, primary_alignment_id)
            .is_some_and(|node| node.content_visible);
        let primary_read_desired = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Reads, primary_alignment_id)
            .is_some_and(|node| node.content_visible);
        let primary_annotation_effective = annotation_section_expanded
            && primary_source_available
            && self
                .track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, primary_alignment_id)
                .unwrap_or(false);
        let primary_read_effective = read_section_expanded
            && primary_source_available
            && self
                .track_presentation
                .track_content_exposed(ui::TrackPane::Reads, primary_alignment_id)
                .unwrap_or(false);
        let previous_read_heights = self
            .panels
            .active()
            .track_frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Reads)
            .filter_map(|frame| match frame.key {
                TrackFrameKey::Alignment(track_id) => {
                    Some((track_id, (frame.bottom - frame.top).max(40.0)))
                }
                TrackFrameKey::Reference
                | TrackFrameKey::Group { .. }
                | TrackFrameKey::Auxiliary(_) => None,
            })
            .collect::<BTreeMap<_, _>>();
        let read_track_order = self
            .track_presentation
            .pane_tracks
            .iter()
            .filter(|node| read_section_expanded && node.channel == ui::TrackPane::Reads)
            .map(|node| node.track_id)
            .collect::<Vec<_>>();
        let mut read_panel_specs = Vec::new();
        for track_id in read_track_order {
            if !self
                .alignment_tracks
                .iter()
                .any(|track| track.config.id == track_id)
            {
                continue;
            }
            if let Some((group, _, _)) = read_group_headers_by_anchor.get(&track_id) {
                read_panel_specs.push(alignment::ReadPanelSpec::header(
                    alignment::ReadPanelKey::Group(*group),
                ));
            }
            if !collapsed_read_group_members.contains(&track_id) {
                let key = alignment::ReadPanelKey::Track(track_id);
                let effective_visible = self
                    .track_presentation
                    .track_content_exposed(ui::TrackPane::Reads, track_id)
                    .unwrap_or(false);
                if effective_visible {
                    read_panel_specs.push(alignment::ReadPanelSpec::expanded(
                        key,
                        self.track_presentation
                            .pane_track_state(ui::TrackPane::Reads, track_id)
                            .and_then(|node| node.preferred_height)
                            .or_else(|| previous_read_heights.get(&track_id).copied()),
                    ));
                } else {
                    read_panel_specs.push(alignment::ReadPanelSpec::header(key));
                }
            }
        }
        let read_panel_densities = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.config.id,
                    self.alignment_density_for_panel_track(
                        self.panels.active().id,
                        track.config.id,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let read_pane_layout =
            alignment::read_pane_layout(&read_panel_specs, read_clip_top, scroll_clip_bottom);
        let read_stack_maximum = (read_pane_layout.content_bottom - scroll_clip_bottom).max(0.0);
        let read_stack_scroll = if read_section_expanded {
            self.panels
                .active()
                .read_stack_scroll_points
                .clamp(0.0, read_stack_maximum)
        } else {
            self.panels.active().read_stack_scroll_points
        };
        {
            let panel = self.panels.active_mut();
            panel.read_stack_scroll_points = read_stack_scroll;
            if read_section_expanded {
                panel.read_stack_content_bottom_points = read_pane_layout.content_bottom;
            }
        }
        let read_panel_bounds = read_pane_layout
            .bounds
            .iter()
            .map(|bounds| alignment::ReadPanelBounds {
                key: bounds.key,
                top: bounds.top - read_stack_scroll,
                bottom: bounds.bottom - read_stack_scroll,
            })
            .collect::<Vec<_>>();
        let mut read_panel_displays = Vec::<ReadPanelDisplayCache>::new();
        self.panels.active_mut().track_frames.clear();
        for group in &read_track_groups {
            let Some(bounds) = alignment::read_panel_bounds_for_group(&read_panel_bounds, group.id)
            else {
                continue;
            };
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Group {
                    channel: ui::TrackPane::Reads,
                    id: group.id,
                },
                channel: ui::TrackPane::Reads,
                name: format!("Read group {} · {} tracks", group.id, group.members.len()),
                kind: TrackKind::Alignment,
                source_label: None,
                top: bounds.top,
                bottom: bounds.bottom.max(bounds.top + READ_PANE_HEADER_HEIGHT),
                visible: true,
                collapsed: true,
                frozen_bottom: Some(
                    (bounds.top + READ_PANE_HEADER_HEIGHT)
                        .min(bounds.bottom.max(bounds.top + READ_PANE_HEADER_HEIGHT)),
                ),
                subtracks: Vec::new(),
            });
        }
        let viewport_interval = self.panels.active().viewport.interval();
        let junction_annotations = junction_annotation_context(
            &self.tracks,
            &self.panels.active().track_snapshots,
            self.panels.active().track_snapshot_interval,
            viewport_interval,
        );
        let tracks_overlap_viewport = self
            .panels
            .active()
            .track_snapshot_interval
            .is_some_and(|loaded| retained_data_overlaps(loaded, viewport_interval));
        let effective_auxiliary_configs = self.effective_auxiliary_presentation_configs();
        let supports_fast_scroll = !effective_auxiliary_configs.iter().any(|track| {
            track.visible
                && !track.collapsed
                && matches!(track.kind, TrackKind::CopyNumber | TrackKind::Mutation)
        });
        if self.reference_engine.is_some() {
            let frame_top = top;
            let reference_expanded = self.track_presentation.reference_content_visible;
            let sequence_top = frame_top + 25.0;
            let translation_top = sequence_top + 23.0;
            let natural_bottom = if reference_expanded {
                frame_top + 49.0 + self.reference_translation_frames.len() as f32 * 20.0
            } else {
                frame_top + 26.0
            };
            let bottom = if reference_expanded {
                self.track_presentation
                    .reference_preferred_height
                    .map_or(natural_bottom, |height| {
                        frame_top + height.clamp(80.0, 600.0)
                    })
            } else {
                frame_top + 26.0
            };
            if reference_expanded
                && let Some(reference) = self
                    .panels
                    .active()
                    .reference_slice
                    .as_deref()
                    .filter(|slice| retained_data_overlaps(slice.interval, viewport_interval))
            {
                let reference_display = layout_reference_slice_oriented(
                    reference,
                    self.panels.active().viewport,
                    sequence_top,
                    self.reference_reverse_complement,
                );
                pinned_display
                    .rectangles
                    .extend(reference_display.rectangles);
                pinned_display.texts.extend(reference_display.texts);
                for (index, frame) in self
                    .reference_translation_frames
                    .iter()
                    .copied()
                    .enumerate()
                {
                    let translation = layout_reference_translation(
                        reference,
                        self.panels.active().viewport,
                        translation_top + index as f32 * 20.0,
                        frame,
                        self.reference_genetic_code,
                    );
                    pinned_display.rectangles.extend(translation.rectangles);
                    pinned_display.texts.extend(translation.texts);
                }
            }
            let mut subtracks = Vec::new();
            if reference_expanded {
                let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
                let reference_detail = if bases_per_pixel > MAXIMUM_BASE_MARK_BASES_PER_PIXEL {
                    Some("zoom in to see sequence".to_owned())
                } else if 1.0 / bases_per_pixel < f64::from(MINIMUM_BASE_GLYPH_PIXEL_WIDTH) {
                    Some("zoom in to see base letters".to_owned())
                } else {
                    None
                };
                subtracks.push(TrackSubtrack {
                    label: reference_bases_label(self.reference_reverse_complement),
                    detail: reference_detail,
                    top: sequence_top,
                    bottom: sequence_top + 20.0,
                    collapsed: false,
                    composite_member: None,
                });
                subtracks.extend(
                    self.reference_translation_frames
                        .iter()
                        .copied()
                        .enumerate()
                        .map(|(index, frame)| TrackSubtrack {
                            label: format!("Translation {}", frame.label()),
                            detail: Some(format!("{:?}", self.reference_genetic_code)),
                            top: translation_top + index as f32 * 20.0,
                            bottom: translation_top + (index + 1) as f32 * 20.0,
                            collapsed: false,
                            composite_member: None,
                        }),
                );
            }
            let frame = TrackFrame {
                key: TrackFrameKey::Reference,
                channel: ui::TrackPane::Annotation,
                name: "Reference sequence".to_owned(),
                kind: TrackKind::Reference,
                source_label: self
                    .reference_source
                    .as_ref()
                    .map(TrackSource::redacted_label),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: !reference_expanded,
                frozen_bottom: None,
                subtracks,
            };
            self.panels.active_mut().track_frames.push(frame);
            top = bottom;
        }
        // Gene models are reference context, so keep them immediately above the
        // primary alignment instead of placing them below an arbitrarily tall
        // read stack. This preserves visible gene context while reads scroll.
        // Keep one primary annotation alongside the reference as persistent
        // genomic context. Additional gene-model tracks are comparative data:
        // pinning every one can consume the complete viewport and leave no
        // visible scrollable region (MF-10 has seven of them).
        let pinned_gene_id = primary_pinned_gene_id(&effective_auxiliary_configs);
        let pinned_gene_configs = effective_auxiliary_configs
            .iter()
            .filter(|track| Some(track.id) == pinned_gene_id)
            .cloned()
            .collect::<Vec<_>>();
        for config in pinned_gene_configs {
            let frame_top = top;
            // A density gesture may resize the scrollable stack, but persistent
            // context must not move the scroll clip boundary underneath it.
            let height = pinned_gene_track_height(&config);
            let bottom = frame_top + height;
            if !config.collapsed
                && tracks_overlap_viewport
                && let Some(TrackSnapshot {
                    data: TrackSnapshotData::Features(records),
                    ..
                }) = self
                    .panels
                    .active()
                    .track_snapshots
                    .iter()
                    .find(|snapshot| snapshot.id == config.id)
            {
                let options = TrackLayoutOptions {
                    top: frame_top,
                    height,
                    maximum_rows: pinned_gene_layout_maximum_rows(height),
                    selected_record: self
                        .panels
                        .active()
                        .selected_feature
                        .filter(|selection| selection.track_id == config.id)
                        .map(|selection| selection.record_index),
                    object_id_base: 1_000_000_u32.saturating_add(
                        u32::try_from(config.id.get())
                            .unwrap_or(u32::MAX)
                            .saturating_mul(100_000),
                    ),
                    feature_display_mode: config.feature.display_mode,
                    ..TrackLayoutOptions::default()
                };
                let gene_display =
                    layout_features(records, self.panels.active().viewport, options).0;
                pinned_display.rectangles.extend(gene_display.rectangles);
                pinned_display.texts.extend(gene_display.texts);
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Auxiliary(config.id),
                channel: ui::TrackPane::Annotation,
                name: config.name,
                kind: config.kind,
                source_label: Some(config.source.redacted_label()),
                top: frame_top,
                bottom,
                visible: true,
                collapsed: config.collapsed,
                frozen_bottom: None,
                subtracks: Vec::new(),
            });
            top = bottom;
        }
        if let Some((group, members, collapsed)) = annotation_group_headers_by_anchor
            .get(&primary_alignment_id)
            .copied()
        {
            let bottom = top + 26.0;
            self.panels.active_mut().track_frames.push(TrackFrame {
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
        if self.panels.active().viewport.detail_level() != DetailLevel::Overview
            && primary_source_available
            && (primary_annotation_effective || primary_read_effective)
            && let Some(query) = self
                .alignment_query_for_track(primary_alignment_id)
                .cloned()
                .filter(|query| retained_data_overlaps(query.interval, viewport_interval))
        {
            let mut options = AlignmentLayoutOptions::default();
            let density = read_panel_densities
                .get(&primary_alignment_id)
                .copied()
                .unwrap_or(self.primary_alignment_filters().alignment_density);
            let annotation_lane_heights =
                alignment_annotation_lane_heights(self.primary_alignment_filters());
            let modification_frequency_supported =
                self.panels.active().viewport.detail_level() != DetailLevel::Overview;
            let show_modification_frequency =
                self.primary_alignment_filters().show_modification_frequency
                    && modification_frequency_supported;
            let junctions_available = !query.junctions.junctions.is_empty();
            let show_junctions =
                self.primary_alignment_filters().show_junctions && junctions_available;
            let embedded_reference_supported = self.reference_engine.is_none();
            let embedded_reference =
                self.primary_alignment_filters().show_reference && embedded_reference_supported;
            let reference_height = PINNED_ALIGNMENT_REFERENCE_HEIGHT;
            let frame_top = top;
            let annotation_group_collapsed =
                collapsed_annotation_group_members.contains(&primary_alignment_id);
            let read_group_collapsed = !read_section_expanded
                || collapsed_read_group_members.contains(&primary_alignment_id);
            let primary_read_bounds = alignment::read_panel_bounds_for_track(
                &read_panel_bounds,
                primary_alignment_id,
                scroll_clip_bottom,
                read_group_collapsed,
            )
            .unwrap_or(alignment::ReadPanelBounds {
                key: alignment::ReadPanelKey::Track(primary_alignment_id),
                top: read_clip_top,
                bottom: scroll_clip_bottom,
            });
            let content_top = frame_top + 22.0;
            let mut lane_top = content_top;
            options.show_reference = embedded_reference;
            options.reference_top = lane_top;
            if embedded_reference {
                // Reserve a stable reference band at every horizontal scale. Its
                // contents appear at base detail, but later lanes must not move
                // when a horizontal gesture crosses an LOD boundary.
                lane_top += reference_height;
            } else if embedded_reference_supported
                && !self.primary_alignment_filters().show_reference
            {
                lane_top += COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT;
            }
            options.show_coverage = self.primary_alignment_filters().show_coverage;
            options.coverage_top = lane_top;
            options.coverage_height = annotation_lane_heights.coverage;
            options.coverage_scale_maximum =
                self.primary_alignment_filters().coverage_scale_maximum;
            options.coverage_scale_transform =
                self.primary_alignment_filters().coverage_scale_transform;
            if self.primary_alignment_filters().show_coverage {
                lane_top += options.coverage_height + 6.0;
            } else {
                lane_top += COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT;
            }
            options.show_junctions = show_junctions;
            options.junction_top = lane_top;
            options.junction_height = annotation_lane_heights.junction;
            options.minimum_junction_depth =
                self.primary_alignment_filters().minimum_junction_depth;
            options.junction_display_mode = self.primary_alignment_filters().junction_display_mode;
            options.junction_count_mode = self.primary_alignment_filters().sashimi_count_mode;
            options.show_junction_labels = self.primary_alignment_filters().show_junction_labels;
            options.junction_annotation_fingerprint = junction_annotations.lookup.fingerprint();
            options.junction_scale_maximum =
                match self.primary_alignment_filters().sashimi_scale_mode {
                    SashimiScaleMode::Automatic => None,
                    SashimiScaleMode::Fixed => Some(f64::from(
                        self.primary_alignment_filters().sashimi_fixed_scale_maximum,
                    )),
                    SashimiScaleMode::Shared => {
                        match self.primary_alignment_filters().sashimi_count_mode {
                            SashimiCountMode::Raw => shared_sashimi_raw,
                            SashimiCountMode::CountsPerMillion => shared_sashimi_cpm,
                        }
                    }
                };
            options.selected_junction = if self.panels.active_mut().selected_alignment_track
                == Some(primary_alignment_id)
            {
                self.panels
                    .active()
                    .selected_junction
                    .map_or([u64::MAX; 2], |(start, end)| [start, end])
            } else {
                [u64::MAX; 2]
            };
            if show_junctions {
                lane_top += options.junction_height + 6.0;
            } else if junctions_available {
                lane_top += COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT;
            }
            options.show_modification_frequency = show_modification_frequency;
            options.modification_top = lane_top;
            options.modification_height = annotation_lane_heights.modification;
            if show_modification_frequency {
                lane_top += options.modification_height + 8.0;
            } else if modification_frequency_supported {
                lane_top += COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT;
            }
            let mut annotation_context_bottom = lane_top;
            options.show_reads = primary_read_effective;
            options.reads_top =
                primary_read_bounds.top + if primary_read_effective { 22.0 } else { 0.0 };
            let frozen_bottom = if options.show_reads {
                options.reads_top
            } else {
                primary_read_bounds.top + COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT
            };
            options.row_height = (7.0 * density).clamp(2.5, MAX_ALIGNMENT_ROW_HEIGHT);
            options.read_height = (5.0 * density).clamp(1.5, (options.row_height - 1.0).max(1.5));
            options.color_mode = self.primary_alignment_filters().alignment_color_mode;
            options.group_mode = self.primary_alignment_filters().alignment_group_mode;
            options.group_order = self.primary_alignment_filters().alignment_group_order;
            options.sort_mode = self.primary_alignment_filters().alignment_sort_mode;
            options.sort_order = self.primary_alignment_filters().alignment_sort_order;
            options.sort_position = alignment_sort_position(
                self.primary_alignment_filters().alignment_sort_mode,
                self.panels.active().viewport,
            );
            options.view_as_pairs = self.primary_alignment_filters().view_as_pairs;
            options.pair_length_bounds = self.effective_template_length_bounds(&query);
            options.minimum_allele_fraction =
                self.primary_alignment_filters().minimum_allele_fraction;
            options.quality_weighted_alleles =
                self.primary_alignment_filters().quality_weighted_alleles;
            options.show_coverage_center_line =
                self.primary_alignment_filters().show_coverage_center_line;
            options.selected_pileup_position = (self.panels.active_mut().selected_alignment_track
                == Some(primary_alignment_id))
            .then_some(self.panels.active().selected_pileup_position)
            .flatten();
            options.read_base_display_mode =
                self.primary_alignment_filters().read_base_display_mode;
            options.shade_read_bases_by_quality =
                self.primary_alignment_filters().shade_read_bases_by_quality;
            options.minimum_indel_size = self.primary_alignment_filters().minimum_indel_size;
            options.minimum_event_label_size =
                self.primary_alignment_filters().minimum_event_label_size;
            options.show_modified_bases = self.primary_alignment_filters().show_modified_bases;
            options.selected_modification_position =
                (self.panels.active_mut().selected_alignment_track == Some(primary_alignment_id))
                    .then_some(self.panels.active().selected_modification_position)
                    .flatten();
            options.selected_chain_read_name_hash =
                (self.panels.active_mut().selected_alignment_track == Some(primary_alignment_id))
                    .then(|| {
                        self.panels
                            .active()
                            .selected_chain
                            .as_ref()
                            .map(|chain| alignment_read_name_hash(&chain.read_name))
                    })
                    .flatten();
            options.promoted_read_name_hash = self
                .promoted_read
                .filter(|(track_id, _)| *track_id == primary_alignment_id)
                .map(|(_, hash)| hash);
            options.selected_chain_read_name_hash = options
                .selected_chain_read_name_hash
                .or(options.promoted_read_name_hash);
            options.minimum_modification_probability = self
                .primary_alignment_filters()
                .minimum_modification_probability;
            options.modified_base_color_mode =
                self.primary_alignment_filters().modified_base_color_mode;
            for (slot, code) in options
                .hidden_modification_codes
                .iter_mut()
                .zip(self.primary_hidden_modification_codes())
            {
                *slot = Some(code);
            }
            annotation_context_bottom = alignment::apply_alignment_annotation_height(
                &mut options,
                frame_top,
                annotation_context_bottom,
                self.alignment_tracks.first().and_then(|_| {
                    self.track_presentation
                        .pane_track_state(ui::TrackPane::Annotation, primary_alignment_id)
                        .and_then(|node| node.preferred_height)
                }),
            );
            // The query safety cap already bounds retained records. The
            // scrollable primary track must give every retained record a row
            // opportunity instead of imposing a second display-only cap.
            options.max_rows = query.batch.len().max(1);
            let key = alignment_layout_key(
                PanelTrackId::new(self.panels.active().id, primary_alignment_id),
                self.primary_alignment_source(),
                self.panels.active().viewport,
                query.interval,
                self.primary_active_filters(),
                &options,
                top,
            );
            let layout = if let Some(layout) = self.layout_cache.get(&key).cloned() {
                layout
            } else {
                let rows = self
                    .alignment_tracks
                    .first()
                    .and_then(|track| track.projection(self.panels.active().id))
                    .and_then(|projection| projection.alignment_rows.as_ref())
                    .expect("alignment rows exist whenever an alignment query exists");
                let layout = Arc::new(layout_alignment_query_with_rows_and_annotations(
                    &query,
                    self.panels.active().viewport,
                    options,
                    &rows.layout,
                    Some(&junction_annotations.lookup),
                ));
                let weight = layout
                    .display_list
                    .rectangles
                    .len()
                    .saturating_mul(std::mem::size_of::<rgv_layout::RectPrimitive>());
                let _ = self.layout_cache.insert(key, Arc::clone(&layout), weight);
                layout
            };
            if let Some(track) = self.alignment_tracks.first_mut() {
                track.projection_mut(self.panels.active().id).cached_layout =
                    Some((key, Arc::clone(&layout)));
            }
            let actual_read_content_bottom = if primary_read_effective {
                let natural_bottom = layout
                    .display_list
                    .rectangles
                    .iter()
                    .skip(layout.pinned_rectangle_count)
                    .map(|rectangle| rectangle.y + rectangle.height)
                    .fold(options.reads_top + options.row_height, f32::max)
                    + 6.0;
                natural_bottom
            } else {
                frozen_bottom + 6.0
            };
            let mut primary_read_display = DisplayList::default();
            let mut suppressed_annotation_display = DisplayList::default();
            extend_alignment_layers(
                if !primary_annotation_effective {
                    &mut suppressed_annotation_display
                } else {
                    &mut pinned_display
                },
                &mut primary_read_display,
                &layout,
                None,
            );
            if primary_read_effective
                && primary_read_bounds.bottom > read_clip_top
                && primary_read_bounds.top < scroll_clip_bottom
            {
                read_panel_displays.push(ReadPanelDisplayCache {
                    track_id: primary_alignment_id,
                    scrollable: primary_read_display,
                    density_camera: true,
                    overview: ui::ScrollbarOverview::default(),
                    actual_content_bottom: actual_read_content_bottom,
                    clip_top: primary_read_bounds.top.max(read_clip_top),
                    clip_bottom: primary_read_bounds.bottom.min(scroll_clip_bottom),
                });
            }
            let mut subtracks = {
                let mut subtracks = Vec::with_capacity(5);
                if embedded_reference {
                    subtracks.push(TrackSubtrack {
                        label: "Reference".to_owned(),
                        detail: None,
                        top: options.reference_top,
                        bottom: options.reference_top + reference_height,
                        collapsed: false,
                        composite_member: None,
                    });
                } else if embedded_reference_supported
                    && !self.primary_alignment_filters().show_reference
                {
                    subtracks.push(collapsed_alignment_subtrack(
                        "Reference",
                        options.reference_top,
                    ));
                }
                subtracks
            };
            let mut alignment_context_frame = TrackFrame {
                key: TrackFrameKey::Alignment(primary_alignment_id),
                channel: ui::TrackPane::Annotation,
                name: primary_alignment_name.clone(),
                kind: TrackKind::Alignment,
                source_label: primary_source_label.clone(),
                top: frame_top,
                bottom: annotation_context_bottom.max(frame_top + 22.0),
                visible: true,
                collapsed: false,
                frozen_bottom: None,
                subtracks: {
                    if self.primary_alignment_filters().show_coverage {
                        let scale = alignment::coverage_scale_detail(
                            layout.stats.maximum_coverage,
                            self.primary_alignment_filters().coverage_scale_maximum,
                            self.primary_alignment_filters().coverage_scale_transform,
                            query.pileup.provenance.exact_depth_complete,
                        );
                        subtracks.push(TrackSubtrack {
                            label: "Coverage".to_owned(),
                            detail: Some(scale),
                            top: options.coverage_top,
                            bottom: options.coverage_top + options.coverage_height,
                            collapsed: false,
                            composite_member: None,
                        });
                    } else {
                        subtracks.push(collapsed_alignment_subtrack(
                            "Coverage",
                            options.coverage_top,
                        ));
                    }
                    if show_junctions {
                        subtracks.push(TrackSubtrack {
                            label: "Sashimi".to_owned(),
                            detail: Some(sashimi_track_detail(
                                &layout.stats,
                                &query.junctions,
                                self.primary_alignment_filters().sashimi_scale_mode,
                                self.primary_alignment_filters().sashimi_fixed_scale_maximum,
                                self.primary_alignment_filters().sashimi_count_mode,
                            )),
                            top: options.junction_top,
                            bottom: options.junction_top + options.junction_height,
                            collapsed: false,
                            composite_member: None,
                        });
                    } else if junctions_available {
                        subtracks.push(collapsed_alignment_subtrack(
                            "Sashimi",
                            options.junction_top,
                        ));
                    }
                    if show_modification_frequency {
                        subtracks.push(TrackSubtrack {
                            label: "Modifications".to_owned(),
                            detail: Some(format!(
                                "{} types · {} tagged reads · {} malformed · {} unavailable{}",
                                query.pileup.modifications.series.len(),
                                query
                                    .pileup
                                    .modifications
                                    .provenance
                                    .records_with_modification_tags,
                                query.pileup.modifications.provenance.malformed_records,
                                query.pileup.modifications.provenance.unavailable_records,
                                if query.pileup.modifications.provenance.dropped_series > 0 {
                                    " · capped"
                                } else {
                                    ""
                                }
                            )),
                            top: options.modification_top,
                            bottom: options.modification_top + options.modification_height,
                            collapsed: false,
                            composite_member: None,
                        });
                    } else if modification_frequency_supported {
                        subtracks.push(collapsed_alignment_subtrack(
                            "Modifications",
                            options.modification_top,
                        ));
                    }
                    if primary_read_effective {
                        let mut read_details = Vec::new();
                        let overview =
                            self.panels.active().viewport.detail_level() == DetailLevel::Overview;
                        if overview {
                            read_details.push(alignment::overview_read_detail(
                                query.stats.records_eligible,
                                query.stats.cancelled,
                            ));
                        } else {
                            read_details.push(format!(
                                "{} visible · {} loaded",
                                layout.stats.rendered_alignments, query.stats.records_emitted,
                            ));
                            if query.stats.truncated {
                                read_details.push(format!(
                                    "safety cap: {}/{} loaded",
                                    query.stats.records_emitted, query.stats.records_eligible
                                ));
                            }
                        }
                        read_details.extend(alignment::filtered_read_details(query.stats));
                        if let Some(detail) = alignment::regional_insertion_detail(layout.stats) {
                            read_details.push(detail);
                        }
                        read_details.push(alignment::row_occupancy_detail(layout.stats));
                        if layout.stats.bisulfite_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} bisulfite marks omitted at display cap",
                                layout.stats.bisulfite_marks_omitted
                            ));
                        }
                        if layout.stats.smrt_kinetics_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} PacBio kinetic marks omitted at display cap",
                                layout.stats.smrt_kinetics_marks_omitted
                            ));
                        }
                        if layout.stats.ultima_quality_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} Ultima quality marks omitted at display cap",
                                layout.stats.ultima_quality_marks_omitted
                            ));
                        }
                        if layout.stats.sbx_tail_marks_omitted > 0 {
                            read_details.push(format!(
                                "{} SBX simplex-tail marks omitted at display cap",
                                layout.stats.sbx_tail_marks_omitted
                            ));
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::SmrtKinetics(_)
                        ) {
                            if layout.stats.smrt_kinetics_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing the selected PacBio kinetic array or sequence",
                                    layout.stats.smrt_kinetics_unavailable_records
                                ));
                            }
                            if layout.stats.smrt_kinetics_malformed_records > 0 {
                                read_details.push(format!(
                                    "{} malformed PacBio kinetic arrays",
                                    layout.stats.smrt_kinetics_malformed_records
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Ultima(_)
                        ) {
                            if layout.stats.ultima_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing Ultima sequence, qualities, or tp",
                                    layout.stats.ultima_unavailable_records
                                ));
                            }
                            if layout.stats.ultima_malformed_records > 0 {
                                read_details.push(format!(
                                    "{} reads with malformed Ultima evidence",
                                    layout.stats.ultima_malformed_records
                                ));
                            }
                            if layout.stats.ultima_unavailable_events > 0 {
                                read_details.push(format!(
                                    "{} indels shown without Ultima quality evidence",
                                    layout.stats.ultima_unavailable_events
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Sbx(_)
                        ) {
                            if layout.stats.sbx_unavailable_records > 0 {
                                read_details.push(format!(
                                    "{} reads missing usable SBX sequence/qualities",
                                    layout.stats.sbx_unavailable_records
                                ));
                            }
                            if layout.stats.sbx_unavailable_insertions > 0 {
                                read_details.push(format!(
                                    "{} SBX insertions shown without quality classification",
                                    layout.stats.sbx_unavailable_insertions
                                ));
                            }
                        }
                        if matches!(
                            self.primary_alignment_filters().alignment_color_mode,
                            AlignmentColorMode::Bisulfite(_)
                        ) && query.reference.is_none()
                        {
                            read_details.push(
                                "bisulfite conversion unavailable: load a matching reference FASTA"
                                    .to_owned(),
                            );
                        }
                        subtracks.push(TrackSubtrack {
                            label: "Reads".to_owned(),
                            detail: (!read_details.is_empty()).then(|| read_details.join(" · ")),
                            top: options.reads_top,
                            bottom: actual_read_content_bottom.max(options.reads_top),
                            collapsed: false,
                            composite_member: None,
                        });
                    } else {
                        subtracks.push(collapsed_alignment_subtrack("Reads", options.reads_top));
                    }
                    subtracks
                },
            };
            let reads = alignment_context_frame
                .subtracks
                .iter()
                .position(|subtrack| subtrack.label == "Reads")
                .map(|index| alignment_context_frame.subtracks.remove(index))
                .into_iter()
                .collect();
            if !annotation_group_collapsed {
                if !primary_annotation_effective {
                    alignment_context_frame.bottom = alignment_context_frame.top + 26.0;
                    alignment_context_frame.subtracks.clear();
                    alignment_context_frame.collapsed = !primary_annotation_desired;
                }
                top = alignment_context_frame.bottom;
                self.panels
                    .active_mut()
                    .track_frames
                    .push(alignment_context_frame);
            }
            if !read_group_collapsed {
                self.panels.active_mut().track_frames.push(TrackFrame {
                    key: TrackFrameKey::Alignment(primary_alignment_id),
                    channel: ui::TrackPane::Reads,
                    name: format!("Reads · {primary_alignment_name}"),
                    kind: TrackKind::Alignment,
                    source_label: primary_source_label.clone(),
                    top: primary_read_bounds.top,
                    bottom: primary_read_bounds
                        .bottom
                        .max(primary_read_bounds.top + 22.0),
                    visible: true,
                    collapsed: !primary_read_desired,
                    frozen_bottom: Some(frozen_bottom),
                    subtracks: reads,
                });
            }
        } else if self.primary_alignment_source().is_some()
            && !collapsed_annotation_group_members.contains(&primary_alignment_id)
        {
            let index_only_overview = self
                .alignment_tracks
                .first()
                .and_then(|track| track.projection(self.panels.active().id))
                .is_some_and(|projection| projection.index_only_overview)
                || self.panels.active().viewport.detail_level() == DetailLevel::Overview;
            if let Some(track) = self.alignment_tracks.first_mut() {
                track.projection_mut(self.panels.active().id).cached_layout = None;
            }
            let visible = true;
            let collapsed = !primary_annotation_desired;
            let natural_bottom = top + if index_only_overview { 48.0 } else { 26.0 };
            let bottom = natural_bottom;
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Alignment(primary_alignment_id),
                channel: ui::TrackPane::Annotation,
                name: primary_alignment_name,
                kind: TrackKind::Alignment,
                source_label: primary_source_label,
                top,
                bottom,
                visible,
                collapsed,
                frozen_bottom: None,
                subtracks: index_only_overview
                    .then(|| TrackSubtrack {
                        label: "Index-only overview".to_owned(),
                        detail: Some(
                            "BAI/CSI occupancy · no alignment records requested".to_owned(),
                        ),
                        top: top + 22.0,
                        bottom,
                        collapsed: false,
                        composite_member: None,
                    })
                    .into_iter()
                    .collect(),
            });
            top = bottom;
        }
        let selected_alignment_track = self.panels.active().selected_alignment_track;
        let selected_chain_read_name_hash = self
            .panels
            .active()
            .selected_chain
            .as_ref()
            .map(|chain| alignment_read_name_hash(&chain.read_name));
        let promoted_read = self.promoted_read;
        for track in self.alignment_tracks.iter_mut().skip(1) {
            let track_id = track.config.id;
            let annotation_group_collapsed = collapsed_annotation_group_members.contains(&track_id);
            let read_group_collapsed =
                !read_section_expanded || collapsed_read_group_members.contains(&track_id);
            let annotation_desired = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Annotation, track_id)
                .is_some_and(|node| node.content_visible);
            let read_desired = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Reads, track_id)
                .is_some_and(|node| node.content_visible);
            let annotation_effective = self
                .track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, track_id)
                .unwrap_or(false);
            let read_effective = read_section_expanded
                && self
                    .track_presentation
                    .track_content_exposed(ui::TrackPane::Reads, track_id)
                    .unwrap_or(false);
            if let Some((group, members, _)) =
                annotation_group_headers_by_anchor.get(&track_id).copied()
            {
                let bottom = top + 26.0;
                self.panels.active_mut().track_frames.push(TrackFrame {
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
            let read_bounds = alignment::read_panel_bounds_for_track(
                &read_panel_bounds,
                track_id,
                scroll_clip_bottom,
                read_group_collapsed,
            )
            .unwrap_or(alignment::ReadPanelBounds {
                key: alignment::ReadPanelKey::Track(track_id),
                top: scroll_clip_bottom,
                bottom: scroll_clip_bottom,
            });
            let (layout, mut frame, mut read_frame) = secondary_alignment_track_layout(
                self.panels.active_mut().id,
                track,
                self.panels.active_mut().viewport,
                &self.panels.active_mut().contig_name,
                top,
                read_bounds.top,
                read_bounds.bottom,
                read_panel_densities
                    .get(&track_id)
                    .copied()
                    .unwrap_or(track.config.filters.alignment_density),
                self.track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track_id)
                    .and_then(|node| node.preferred_height),
                read_effective,
                (selected_alignment_track == Some(track_id))
                    .then_some(selected_chain_read_name_hash)
                    .flatten(),
                promoted_read
                    .filter(|(promoted_track, _)| *promoted_track == track_id)
                    .map(|(_, hash)| hash),
                shared_sashimi_raw,
                shared_sashimi_cpm,
                &junction_annotations.lookup,
            );
            if let Some(layout) = layout {
                // CPU semantic picking resolves the owning TrackId from the frame.
                // Keep GPU object IDs clear because they are only unique within
                // one alignment query, not across the complete workspace.
                if annotation_effective {
                    annotation_display.rectangles.extend(
                        layout
                            .display_list
                            .rectangles
                            .iter()
                            .take(layout.pinned_rectangle_count)
                            .copied()
                            .map(|mut rectangle| {
                                rectangle.object_id = 0;
                                rectangle
                            }),
                    );
                    annotation_display.texts.extend(
                        layout
                            .display_list
                            .texts
                            .iter()
                            .take(layout.pinned_text_count)
                            .cloned(),
                    );
                }
                let read_display = DisplayList {
                    rectangles: layout
                        .display_list
                        .rectangles
                        .iter()
                        .skip(layout.pinned_rectangle_count)
                        .copied()
                        .map(|mut rectangle| {
                            rectangle.object_id = 0;
                            rectangle
                        })
                        .collect(),
                    texts: layout
                        .display_list
                        .texts
                        .iter()
                        .skip(layout.pinned_text_count)
                        .cloned()
                        .collect(),
                };
                if read_effective
                    && read_bounds.bottom > read_clip_top
                    && read_bounds.top < scroll_clip_bottom
                {
                    let natural_bottom = read_display
                        .rectangles
                        .iter()
                        .map(|rectangle| rectangle.y + rectangle.height)
                        .fold(read_bounds.top + 22.0, f32::max)
                        + 6.0;
                    read_panel_displays.push(ReadPanelDisplayCache {
                        track_id,
                        scrollable: read_display,
                        density_camera: true,
                        overview: ui::ScrollbarOverview::default(),
                        actual_content_bottom: natural_bottom,
                        clip_top: read_bounds.top.max(read_clip_top),
                        clip_bottom: read_bounds.bottom.min(scroll_clip_bottom),
                    });
                }
            }
            if !annotation_group_collapsed {
                if !annotation_effective {
                    frame.bottom = frame.top + 26.0;
                    frame.collapsed = !annotation_desired;
                    frame.subtracks.clear();
                }
                top = frame.bottom;
                self.panels.active_mut().track_frames.push(frame);
            }
            if !read_group_collapsed && let Some(read_frame) = read_frame.as_mut() {
                read_frame.collapsed = !read_desired;
                if !read_effective {
                    read_frame.bottom = read_frame.top + READ_PANE_HEADER_HEIGHT;
                    read_frame.frozen_bottom = Some(read_frame.bottom);
                    read_frame.subtracks.clear();
                }
            }
            if !read_group_collapsed && let Some(read_frame) = read_frame {
                self.panels.active_mut().track_frames.push(read_frame);
            }
        }
        // Read-pane controls are a projection of the presentation tree, not
        // of query/render availability. Preserve every exposed restore row
        // while a source opens, has no data at the locus, is unavailable, or
        // has its own/group content gate closed.
        for bounds in &read_panel_bounds {
            let alignment::ReadPanelKey::Track(track_id) = bounds.key else {
                continue;
            };
            if self.panels.active().track_frames.iter().any(|frame| {
                frame.channel == ui::TrackPane::Reads
                    && frame.key == TrackFrameKey::Alignment(track_id)
            }) {
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
                .projection(self.panels.active().id)
                .is_some_and(|projection| projection.index_only_overview)
                || self.panels.active().viewport.detail_level() == DetailLevel::Overview;
            let mapped_record_count = track
                .dictionary
                .as_ref()
                .and_then(|dictionary| dictionary.id_by_name(&self.panels.active().contig_name))
                .and_then(|contig| {
                    track
                        .index_overview
                        .as_ref()
                        .and_then(|overview| overview.contig_mapped_record_count(contig))
                });
            let bottom = bounds.bottom.max(bounds.top + READ_PANE_HEADER_HEIGHT);
            let content_top = bounds.top + READ_PANE_HEADER_HEIGHT;
            if index_only_overview
                && desired
                && bottom > content_top
                && bottom > read_clip_top
                && bounds.top < scroll_clip_bottom
            {
                read_panel_displays.push(ReadPanelDisplayCache {
                    track_id,
                    scrollable: alignment::index_only_read_display(
                        self.active_panel_rect().width(),
                        content_top,
                        bottom,
                        mapped_record_count,
                    ),
                    density_camera: false,
                    overview: ui::ScrollbarOverview::default(),
                    actual_content_bottom: bottom,
                    clip_top: bounds.top.max(read_clip_top),
                    clip_bottom: bottom.min(scroll_clip_bottom),
                });
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Alignment(track_id),
                channel: ui::TrackPane::Reads,
                name: format!("Reads · {}", track.config.name),
                kind: TrackKind::Alignment,
                source_label: Some(track.config.source.redacted_label()),
                top: bounds.top,
                bottom,
                visible: true,
                collapsed: !desired,
                frozen_bottom: Some(if index_only_overview && desired {
                    content_top
                } else {
                    bottom
                }),
                subtracks: if index_only_overview && desired {
                    vec![TrackSubtrack {
                        label: "Reads".to_owned(),
                        detail: None,
                        top: content_top,
                        bottom,
                        collapsed: false,
                        composite_member: None,
                    }]
                } else {
                    vec![collapsed_alignment_subtrack("Reads", bounds.top)]
                },
            });
        }
        let display_configs = effective_auxiliary_configs
            .iter()
            .filter(|track| Some(track.id) != pinned_gene_id)
            .collect::<Vec<_>>();
        let mut rendered_overlay_groups = BTreeSet::new();
        for config in &display_configs {
            let members = if config.kind == TrackKind::Quantitative
                && let Some(group) = config.quantitative.overlay_group
            {
                if !rendered_overlay_groups.insert(group) {
                    continue;
                }
                quantitative_overlay_members(config, &display_configs)
            } else {
                vec![*config]
            };
            let overlay_group = config.quantitative.overlay_group;
            let composite_visible = overlay_group.is_none_or(|group| {
                self.track_presentation
                    .composite_content_visible(ui::TrackPane::Annotation, group)
            });
            let member_content_visible = members.iter().any(|member| member.visible);
            let visible = composite_visible && member_content_visible;
            let collapsed = !visible
                || members
                    .iter()
                    .filter(|member| member.visible)
                    .all(|member| member.collapsed);
            let effective_copy_number_settings = config.copy_number.clone();
            let effective_mutation_settings = config.mutation.clone();
            let height = if collapsed {
                26.0
            } else {
                members
                    .iter()
                    .filter(|member| member.visible && !member.collapsed)
                    .map(|member| member.height.clamp(40.0, 1_200.0))
                    .reduce(f32::max)
                    .unwrap_or(40.0)
            };
            let bottom = top + height;
            let mut subtracks = quantitative_overlay_subtracks(&members, top, height);
            if visible && !collapsed && tracks_overlap_viewport {
                for member in members
                    .iter()
                    .filter(|member| member.visible && !member.collapsed)
                {
                    let Some(snapshot) = self
                        .panels
                        .active()
                        .track_snapshots
                        .iter()
                        .find(|snapshot| snapshot.id == member.id)
                    else {
                        continue;
                    };
                    let options = TrackLayoutOptions {
                        top,
                        height,
                        selected_record: self
                            .panels
                            .active()
                            .selected_feature
                            .filter(|selection| selection.track_id == member.id)
                            .map(|selection| selection.record_index)
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_variant
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_gwas
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_quantitative
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_interaction
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_copy_number
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.segment_index)
                            })
                            .or_else(|| {
                                self.panels
                                    .active()
                                    .selected_mutation
                                    .filter(|selection| selection.track_id == member.id)
                                    .map(|selection| selection.record_index)
                            }),
                        object_id_base: 1_000_000_u32.saturating_add(
                            u32::try_from(member.id.get())
                                .unwrap_or(u32::MAX)
                                .saturating_mul(100_000),
                        ),
                        feature_display_mode: member.feature.display_mode,
                        ..TrackLayoutOptions::default()
                    };
                    let track_display = match &snapshot.data {
                        TrackSnapshotData::Features(records) => {
                            layout_features(records, self.panels.active().viewport, options).0
                        }
                        TrackSnapshotData::Variants(records) => {
                            layout_variants(
                                &records.records,
                                self.panels.active().viewport,
                                options,
                                &member.variant,
                            )
                            .0
                        }
                        TrackSnapshotData::Gwas(query) => {
                            if query.provenance.records_truncated
                                || query.provenance.source_rows_ignored > 0
                            {
                                subtracks.push(TrackSubtrack {
                                    label: if query.provenance.records_truncated {
                                        "Sampled points".to_owned()
                                    } else {
                                        "Missing p-values".to_owned()
                                    },
                                    detail: Some(if query.provenance.records_truncated {
                                        format!(
                                            "{} / {}",
                                            query.provenance.records_retained,
                                            query.provenance.records_examined
                                        )
                                    } else {
                                        format!("{} ignored", query.provenance.source_rows_ignored)
                                    }),
                                    top: top + 19.0,
                                    bottom: (top + 39.0).min(bottom),
                                    collapsed: false,
                                    composite_member: None,
                                });
                            }
                            layout_gwas(
                                &query.records,
                                self.panels.active().viewport,
                                options,
                                &member.gwas,
                            )
                            .0
                        }
                        TrackSnapshotData::Quantitative(_) => {
                            let shared_range = (member.quantitative.scale_mode
                                == QuantitativeScaleMode::Shared)
                                .then(|| {
                                    shared_quantitative_range(
                                        &effective_auxiliary_configs,
                                        &self.panels.active().track_snapshots,
                                        self.panels.active().viewport,
                                        member.quantitative.scale_transform,
                                        member.quantitative.scale_group,
                                    )
                                })
                                .flatten();
                            quantitative_display_records(
                                member,
                                &self.panels.active().track_snapshots,
                                self.panels.active().viewport,
                            )
                            .map_or_else(
                                DisplayList::default,
                                |records| {
                                    layout_quantitative(
                                        &records,
                                        self.panels.active().viewport,
                                        options,
                                        &member.quantitative,
                                        shared_range,
                                    )
                                    .0
                                },
                            )
                        }
                        TrackSnapshotData::CopyNumber(query) => {
                            let shared_range = (member.quantitative.scale_mode
                                == QuantitativeScaleMode::Shared)
                                .then(|| {
                                    shared_copy_number_range(
                                        member,
                                        &effective_auxiliary_configs,
                                        &self.panels.active().track_snapshots,
                                    )
                                })
                                .flatten();
                            let layout = layout_copy_number(
                                query,
                                self.panels.active().viewport,
                                options,
                                &effective_copy_number_settings,
                                &member.quantitative,
                                shared_range,
                                self.panels.annotation_scroll_points
                                    + self.canvas_geometry().viewport.top(),
                                self.panels.annotation_scroll_points + annotation_clip_bottom,
                            );
                            if effective_copy_number_settings.show_sample_names {
                                subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                                    query.samples.get(row.sample_index).map(|sample| {
                                        TrackSubtrack {
                                            label: sample.clone(),
                                            detail: query
                                                .sample_sort_values
                                                .get(row.sample_index)
                                                .copied()
                                                .flatten()
                                                .map(|value| format!("{value:.3}")),
                                            top: row.top,
                                            bottom: row.bottom,
                                            collapsed: false,
                                            composite_member: None,
                                        }
                                    })
                                }));
                            }
                            layout.display_list
                        }
                        TrackSnapshotData::Mutations(query) => {
                            let layout = layout_mutations(
                                query,
                                self.panels.active().viewport,
                                options,
                                &effective_mutation_settings,
                                self.panels.annotation_scroll_points
                                    + self.canvas_geometry().viewport.top(),
                                self.panels.annotation_scroll_points + annotation_clip_bottom,
                            );
                            if effective_mutation_settings.show_sample_names {
                                subtracks.extend(layout.visible_rows.iter().filter_map(|row| {
                                    query.samples.get(row.sample_index).map(|sample| {
                                        TrackSubtrack {
                                            label: sample.clone(),
                                            detail: Some("MAF tumour sample".to_owned()),
                                            top: row.top,
                                            bottom: row.bottom,
                                            collapsed: false,
                                            composite_member: None,
                                        }
                                    })
                                }));
                            }
                            layout.display_list
                        }
                        TrackSnapshotData::Interactions(query) => {
                            layout_interactions(
                                &query.records,
                                &self.panels.active().contig_name,
                                self.panels.active().viewport,
                                options,
                                &member.interaction,
                            )
                            .0
                        }
                    };
                    annotation_display
                        .rectangles
                        .extend(track_display.rectangles);
                    annotation_display.texts.extend(track_display.texts);
                }
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key: TrackFrameKey::Auxiliary(config.id),
                channel: ui::TrackPane::Annotation,
                name: overlay_group.map_or_else(
                    || config.name.clone(),
                    |group| format!("Overlay {group} · {} tracks", members.len()),
                ),
                kind: config.kind,
                source_label: Some(
                    members
                        .iter()
                        .map(|member| member.source.redacted_label())
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                top,
                bottom,
                visible: overlay_group.map_or(member_content_visible, |_| composite_visible),
                collapsed,
                frozen_bottom: None,
                subtracks,
            });
            top = bottom;
        }
        // Reference, gene models, alignment summaries, and every auxiliary
        // track belong to the independently scrollable annotation pane.
        annotation_display
            .rectangles
            .append(&mut pinned_display.rectangles);
        annotation_display.texts.append(&mut pinned_display.texts);
        for group in &annotation_track_groups {
            let key = TrackFrameKey::Group {
                channel: ui::TrackPane::Annotation,
                id: group.id,
            };
            if self
                .panels
                .active()
                .track_frames
                .iter()
                .any(|frame| frame.key == key)
            {
                continue;
            }
            self.panels.active_mut().track_frames.push(TrackFrame {
                key,
                channel: ui::TrackPane::Annotation,
                name: format!("Group {} · {} tracks", group.id, group.members.len()),
                kind: TrackKind::Feature,
                source_label: None,
                top,
                bottom: top + 26.0,
                visible: true,
                collapsed: true,
                frozen_bottom: None,
                subtracks: Vec::new(),
            });
            top += 26.0;
        }
        let mut annotation_fallback_frames = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let track_id = track.config.id;
                let desired = self
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track_id)
                    .is_some_and(|node| node.content_visible);
                TrackFrame {
                    key: TrackFrameKey::Alignment(track_id),
                    channel: ui::TrackPane::Annotation,
                    name: track.config.name.clone(),
                    kind: TrackKind::Alignment,
                    source_label: Some(track.config.source.redacted_label()),
                    top: 0.0,
                    bottom: 26.0,
                    visible: true,
                    collapsed: !desired,
                    frozen_bottom: None,
                    subtracks: Vec::new(),
                }
            })
            .collect::<Vec<_>>();
        annotation_fallback_frames.extend(self.tracks.tracks().iter().map(|track| {
            let desired = self
                .track_presentation
                .pane_track_state(ui::TrackPane::Annotation, track.id)
                .is_some_and(|node| node.content_visible);
            TrackFrame {
                key: TrackFrameKey::Auxiliary(track.id),
                channel: ui::TrackPane::Annotation,
                name: track.name.clone(),
                kind: track.kind,
                source_label: Some(track.source.redacted_label()),
                top: 0.0,
                bottom: 26.0,
                visible: true,
                collapsed: !desired,
                frozen_bottom: None,
                subtracks: Vec::new(),
            }
        }));
        if annotation_section_expanded {
            let _fallback_bottom = ensure_annotation_presentation_frames(
                &self.track_presentation,
                &mut self.panels.active_mut().track_frames,
                &annotation_fallback_frames,
                top,
            );
            let shared_annotation_frame_heights = self.shared_annotation_frame_heights.clone();
            top = reflow_annotation_presentation_with_heights(
                &self.track_presentation,
                &mut self.panels.active_mut().track_frames,
                &mut annotation_display,
                annotation_clip_top,
                &shared_annotation_frame_heights,
            );
        } else {
            // Folding the section is a presentation gate, not a mutation of
            // child rows. Remove only this render revision's annotation
            // frames and primitives; their state and shared camera survive.
            self.panels
                .active_mut()
                .track_frames
                .retain(|frame| frame.channel != ui::TrackPane::Annotation);
            annotation_display = DisplayList::default();
            pinned_display = DisplayList::default();
            top = annotation_clip_top;
        }
        self.panels
            .active_mut()
            .track_frames
            .sort_by(|left, right| {
                left.channel
                    .cmp(&right.channel)
                    .then_with(|| left.top.total_cmp(&right.top))
                    .then_with(|| left.bottom.total_cmp(&right.bottom))
            });
        let annotation_content_bottom = annotation_display
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(top, f32::max)
            .max(annotation_clip_bottom);
        let pane_layout = {
            let frames = &self.panels.active().track_frames;
            ui::WorkspacePaneLayout {
                annotation: pane_layout_snapshot(
                    &self.track_presentation,
                    frames,
                    ui::TrackPane::Annotation,
                    annotation_clip_top,
                    annotation_clip_bottom,
                    annotation_content_bottom,
                    self.panels.annotation_scroll_points,
                    0.0,
                    self.active_panel_rect().width(),
                ),
                reads: pane_layout_snapshot(
                    &self.track_presentation,
                    frames,
                    ui::TrackPane::Reads,
                    read_clip_top,
                    scroll_clip_bottom,
                    read_pane_layout.content_bottom,
                    0.0,
                    read_stack_scroll,
                    self.active_panel_rect().width(),
                ),
            }
        };
        self.panels.active_mut().pane_layout = pane_layout;
        let read_frames = self
            .panels
            .active()
            .track_frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Reads)
            .cloned()
            .collect::<Vec<_>>();
        let annotation_frames = self
            .panels
            .active()
            .track_frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Annotation)
            .cloned()
            .collect::<Vec<_>>();
        for read_panel in &mut read_panel_displays {
            let density = self
                .alignment_density_for_panel_track(self.panels.active().id, read_panel.track_id);
            let padding = if read_panel.density_camera {
                vertical_density_camera_padding(
                    (read_panel.clip_bottom - read_panel.clip_top).max(1.0),
                    density,
                )
            } else {
                0.0
            };
            let content_bottom = read_panel.actual_content_bottom + padding;
            let state = self
                .panels
                .active()
                .read_panel_scroll_state(read_panel.track_id);
            let frame = read_frames
                .iter()
                .filter(|frame| frame.key == TrackFrameKey::Alignment(read_panel.track_id))
                .cloned()
                .collect::<Vec<_>>();
            read_panel.overview = ui::scrollbar_overview(
                &frame,
                &read_panel.scrollable,
                read_panel.clip_top,
                content_bottom,
                read_panel.clip_top,
                state.content_origin_points,
                self.active_panel_rect().width(),
                128,
            );
            for bin in &mut read_panel.overview.bins {
                if bin.density > f32::EPSILON {
                    bin.kind = Some(TrackKind::Alignment);
                }
            }
            read_panel.overview.boundaries.clear();
        }
        let annotation_overview = ui::scrollbar_overview(
            &annotation_frames,
            &annotation_display,
            annotation_clip_top,
            annotation_content_bottom,
            annotation_clip_top,
            0.0,
            self.active_panel_rect().width(),
            128,
        );
        self.panels.active_mut().scroll_display_cache = Some(PanelScrollDisplayCache {
            annotations: annotation_display,
            pinned: pinned_display,
            annotation_overview,
            annotation_content_bottom,
            annotation_clip_top,
            annotation_clip_bottom,
            supports_fast_scroll,
            read_panels: read_panel_displays,
        });
        let rebuilt = self.rebuild_active_panel_from_scroll_cache();
        debug_assert!(rebuilt);
    }

    fn rebuild_active_panel_from_scroll_cache(&mut self) -> bool {
        let trace_started = self.interaction_trace.is_active().then(Instant::now);
        if trace_started.is_some() {
            self.interaction_trace
                .record(InteractionTraceStage::ScrollDisplayStarted);
        }
        let Some((annotation_content_bottom, annotation_clip_top, annotation_clip_bottom)) = self
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .map(|cache| {
                (
                    cache.annotation_content_bottom,
                    cache.annotation_clip_top,
                    cache.annotation_clip_bottom,
                )
            })
        else {
            return false;
        };
        let active_panel_id = self.panels.active().id;
        let active_drag = self
            .vertical_overview_drag
            .filter(|drag| drag.density_dragging && drag.panel_id == active_panel_id);
        let read_panel_metrics = self
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .expect("scroll display cache was observed above")
            .read_panels
            .iter()
            .map(|read_panel| {
                (
                    read_panel.track_id,
                    read_panel.actual_content_bottom,
                    read_panel.clip_top,
                    read_panel.clip_bottom,
                    read_panel.density_camera,
                    self.alignment_density_for_panel_track(
                        self.panels.active().id,
                        read_panel.track_id,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let primary_track_id = self.primary_alignment_track_id();
        let annotation_scroll = if self.track_presentation.annotation_section_expanded {
            self.panels.annotation_scroll_points.clamp(
                0.0,
                (annotation_content_bottom - annotation_clip_bottom).max(0.0),
            )
        } else {
            // A folded section has no current scroll extent, but its camera is
            // latent state and must be restored when the section is reopened.
            self.panels.annotation_scroll_points
        };
        if self.track_presentation.annotation_section_expanded {
            self.panels.annotation_scroll_points = annotation_scroll;
        }
        let panel = self.panels.active_mut();
        panel
            .pane_layout
            .annotation
            .reproject_scroll(annotation_scroll);
        let legacy_primary = ReadPanelScrollState {
            scroll_points: panel.vertical_scroll_points,
            content_origin_points: panel.vertical_content_origin_points,
            latent_scroll_fraction: panel.latent_vertical_scroll_fraction,
            content_bottom_points: panel.content_bottom_points,
            density: None,
        };
        for (track_id, actual_bottom, clip_top, clip_bottom, density_camera, density) in
            &read_panel_metrics
        {
            let state = panel
                .read_panel_scrolls
                .entry(*track_id)
                .or_insert_with(|| {
                    if *track_id == primary_track_id {
                        legacy_primary
                    } else {
                        ReadPanelScrollState::default()
                    }
                });
            let camera_padding = if *density_camera {
                vertical_density_camera_padding((clip_bottom - clip_top).max(1.0), *density)
            } else {
                0.0
            };
            state.content_origin_points = state.content_origin_points.clamp(0.0, camera_padding);
            let maximum_scroll = (actual_bottom + camera_padding - clip_bottom).max(0.0);
            let previous_maximum = (state.content_bottom_points - clip_bottom).max(0.0);
            let density_drag_fraction = active_drag
                .filter(|drag| drag.track_id == *track_id)
                .map(|drag| drag.virtual_scroll_fraction);
            (state.scroll_points, state.latent_scroll_fraction) = reconcile_vertical_scroll(
                state.scroll_points,
                previous_maximum,
                maximum_scroll,
                state.latent_scroll_fraction,
                density_drag_fraction,
            );
            state.content_bottom_points = clip_bottom + maximum_scroll;
        }
        // A panel that is temporarily hidden by its own eye or by a collapsed
        // group is still part of the workspace. Keep its camera state until
        // the source track is actually removed; rebuilding renderable panels
        // must not erase scroll, density, or latent zoom state.
        if let Some(primary) = panel.read_panel_scrolls.get(&primary_track_id).copied() {
            panel.vertical_scroll_points = primary.scroll_points;
            panel.vertical_content_origin_points = primary.content_origin_points;
            panel.latent_vertical_scroll_fraction = primary.latent_scroll_fraction;
            panel.content_bottom_points = primary.content_bottom_points;
        }
        let mut display = std::mem::take(&mut self.panels.active_mut().display_list);
        let rectangle_capacity = display.rectangles.capacity();
        let text_capacity = display.texts.capacity();
        display.rectangles.clear();
        display.texts.clear();
        let panel_rect = self.active_panel_rect();
        append_annotation_pane_background(
            &mut display,
            panel_rect.width(),
            annotation_clip_top,
            annotation_clip_bottom,
        );
        {
            let cache = self
                .panels
                .active()
                .scroll_display_cache
                .as_ref()
                .expect("scroll display cache was observed above");
            for read_panel in &cache.read_panels {
                let scroll = self
                    .panels
                    .active()
                    .read_panel_scroll_state(read_panel.track_id)
                    .effective_scroll_points();
                read_panel.scrollable.append_scrolled_and_clipped_vertical(
                    scroll,
                    read_panel.clip_top,
                    read_panel.clip_bottom,
                    &mut display,
                );
            }
            cache.annotations.append_scrolled_and_clipped_vertical(
                annotation_scroll,
                annotation_clip_top,
                annotation_clip_bottom,
                &mut display,
            );
            cache.pinned.append_scrolled_and_clipped_vertical(
                0.0,
                annotation_clip_top,
                annotation_clip_bottom,
                &mut display,
            );
        }
        display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        self.add_region_overlays(&mut display);
        let capacity_grew = display.rectangles.capacity() > rectangle_capacity
            || display.texts.capacity() > text_capacity;
        if let Some(started) = trace_started {
            self.interaction_trace.record_scroll_display(
                started.elapsed(),
                display.rectangles.len(),
                display.texts.len(),
                capacity_grew,
            );
        }
        self.panels.active_mut().display_list = display;
        true
    }

    pub(crate) fn refresh_active_panel_scroll_display(&mut self) -> bool {
        if self.display_dirty
            || !self
                .panels
                .active()
                .scroll_display_cache
                .as_ref()
                .is_some_and(|cache| cache.supports_fast_scroll)
            || !self.rebuild_active_panel_from_scroll_cache()
        {
            return false;
        }
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        self.prepare_rectangle_renderer();
        true
    }

    /// Applies the one workspace annotation camera to every locus and refreshes
    /// their cached annotation display lists as a single operation.
    pub(crate) fn refresh_shared_annotation_scroll_display(
        &mut self,
        requested_scroll: f32,
    ) -> bool {
        let next = requested_scroll.clamp(0.0, self.panels.maximum_annotation_scroll_points());
        self.panels.annotation_scroll_points = next;
        for panel in self.panels.iter_mut() {
            panel.pane_layout.annotation.reproject_scroll(next);
        }
        if self.synthetic
            || self.display_dirty
            || self.panels.iter().any(|panel| {
                !panel
                    .scroll_display_cache
                    .as_ref()
                    .is_some_and(|cache| cache.supports_fast_scroll)
            })
        {
            return false;
        }

        let active_id = self.panels.active().id;
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        for panel_id in panel_ids {
            self.panels.activate(panel_id);
            if !self.rebuild_active_panel_from_scroll_cache() {
                self.panels.activate(active_id);
                return false;
            }
        }
        self.panels.activate(active_id);
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        self.prepare_rectangle_renderer();
        true
    }

    pub(crate) fn alignment_track_name(&self) -> String {
        self.primary_alignment_name().to_owned()
    }

    pub(crate) fn add_region_overlays(&self, display: &mut DisplayList) {
        let viewport = self.panels.active().viewport.interval();
        let panel = self.active_panel_rect();
        let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
        for (region, x, right) in self
            .regions_of_interest
            .iter()
            .filter_map(|region| {
                region_overlay_horizontal_span(
                    region,
                    &self.panels.active().contig_name,
                    viewport,
                    bases_per_pixel,
                    panel.left(),
                )
                .map(|(x, right)| (region, x, right))
            })
            .take(MAX_RENDERED_REGIONS_OF_INTEREST)
        {
            let color = Rgba::new(
                f32::from(region.color[0]) / 255.0,
                f32::from(region.color[1]) / 255.0,
                f32::from(region.color[2]) / 255.0,
                (f32::from(region.color[3]) / 255.0).min(0.18),
            );
            display.rectangles.push(RectPrimitive {
                x,
                y: self.canvas_geometry().viewport.top(),
                width: (right - x).max(1.0),
                height: self.canvas_geometry().viewport.height(),
                color,
                object_id: 0,
                shape: Default::default(),
            });
            for boundary in [x, right - 1.0] {
                display.rectangles.push(RectPrimitive {
                    x: boundary,
                    y: self.canvas_geometry().viewport.top(),
                    width: 1.0,
                    height: self.canvas_geometry().viewport.height(),
                    color: Rgba::new(color.red, color.green, color.blue, 0.72),
                    object_id: 0,
                    shape: Default::default(),
                });
            }
        }
    }

    pub(crate) fn apply_canvas_scroll(
        &mut self,
        display: &mut DisplayList,
        pinned_display: &mut DisplayList,
        minimum_bottom: f32,
        scroll_clip_top: f32,
    ) {
        let panel_rect = self.active_panel_rect();
        let active_panel_id = self.panels.active().id;
        let previous_maximum_scroll = (self.panels.active().content_bottom_points
            - self.canvas_geometry().viewport.bottom())
        .max(0.0);
        let density_drag_fraction = self
            .vertical_overview_drag
            .filter(|drag| drag.density_dragging && drag.panel_id == active_panel_id)
            .map(|drag| drag.virtual_scroll_fraction);
        let actual_content_bottom = display
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(minimum_bottom, f32::max)
            .max(self.canvas_geometry().viewport.bottom());
        let camera_padding = vertical_density_camera_padding(
            (self.canvas_geometry().viewport.bottom() - scroll_clip_top).max(1.0),
            self.primary_alignment_filters().alignment_density,
        );
        self.panels.active_mut().vertical_content_origin_points = self
            .panels
            .active()
            .vertical_content_origin_points
            .clamp(0.0, camera_padding);
        self.panels.active_mut().content_bottom_points = actual_content_bottom + camera_padding;
        let maximum_scroll = (self.panels.active().content_bottom_points
            - self.canvas_geometry().viewport.bottom())
        .max(0.0);
        let current_scroll = self.panels.active().vertical_scroll_points;
        let (scroll, latent_fraction) = reconcile_vertical_scroll(
            current_scroll,
            previous_maximum_scroll,
            maximum_scroll,
            self.panels.active().latent_vertical_scroll_fraction,
            density_drag_fraction,
        );
        self.panels.active_mut().vertical_scroll_points = scroll;
        self.panels.active_mut().latent_vertical_scroll_fraction = latent_fraction;
        let effective_scroll = self.panels.active().effective_vertical_scroll_points();
        display.scroll_and_clip_vertical(
            effective_scroll,
            scroll_clip_top,
            self.canvas_geometry().viewport.bottom(),
        );
        display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        pinned_display.scroll_and_clip_vertical(
            0.0,
            self.canvas_geometry().viewport.top(),
            scroll_clip_top,
        );
        pinned_display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        // Frozen lanes are appended last so their backgrounds remain above any
        // scrollable primitive that touches the clipping boundary.
        display.rectangles.append(&mut pinned_display.rectangles);
        display.texts.append(&mut pinned_display.texts);
    }

    pub(crate) fn rebuild_all_panel_displays(&mut self) {
        let active_id = self.panels.active().id;
        let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
        let requested_annotation_scroll = self.panels.annotation_scroll_points;
        self.shared_annotation_frame_heights.clear();
        for panel_id in &panel_ids {
            self.panels.annotation_scroll_points = requested_annotation_scroll;
            self.panels.activate(*panel_id);
            self.rebuild_genomic_display();
        }
        if self.panels.len() > 1 {
            self.shared_annotation_frame_heights = maximum_annotation_frame_heights(&self.panels);
            for panel_id in &panel_ids {
                self.panels.annotation_scroll_points = requested_annotation_scroll;
                self.panels.activate(*panel_id);
                self.rebuild_genomic_display();
            }
        }

        // Layout can reduce the legal range after tracks are hidden, grouped,
        // or collapsed. Clamp once against all panels, then reproject every
        // cache to the exact same workspace offset.
        let shared_annotation_scroll = if self.track_presentation.annotation_section_expanded {
            requested_annotation_scroll.clamp(0.0, self.panels.maximum_annotation_scroll_points())
        } else {
            requested_annotation_scroll
        };
        let needs_scroll_reprojection = self.panels.iter().any(|panel| {
            (panel.pane_layout.annotation.scroll_points - shared_annotation_scroll).abs()
                > f32::EPSILON
        });
        self.panels.annotation_scroll_points = shared_annotation_scroll;
        if needs_scroll_reprojection {
            for panel_id in &panel_ids {
                self.panels.activate(*panel_id);
                let _ = self.rebuild_active_panel_from_scroll_cache();
            }
        }
        self.panels.activate(active_id);

        self.composite_display_list = composite_panel_display_lists(&self.panels);
    }

    pub(crate) fn rebuild_display_list(&mut self) {
        if !self.display_dirty {
            return;
        }
        if self.synthetic {
            let top = self.canvas_geometry().viewport.top();
            let mut display =
                if self.primary_alignment_visible() && !self.primary_alignment_collapsed() {
                    DisplayList::synthetic_coverage_and_reads_at(
                        self.panels.active().viewport.pixel_width() as f32,
                        100,
                        u32::try_from(self.primary_alignment_limit()).unwrap_or(u32::MAX),
                        top + 30.0,
                    )
                } else {
                    DisplayList::default()
                };
            let bottom = if self.primary_alignment_visible() && !self.primary_alignment_collapsed()
            {
                top + 730.0
            } else if self.primary_alignment_visible() {
                top + 26.0
            } else {
                top
            };
            self.panels.active_mut().track_frames = if self.primary_alignment_visible() {
                vec![TrackFrame {
                    key: TrackFrameKey::Alignment(SYNTHETIC_ALIGNMENT_TRACK_ID),
                    channel: ui::TrackPane::Annotation,
                    name: "Synthetic reads".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: Some("Synthetic GPU workload".to_owned()),
                    top,
                    bottom,
                    visible: true,
                    collapsed: self.primary_alignment_collapsed(),
                    frozen_bottom: None,
                    subtracks: if self.primary_alignment_collapsed() {
                        Vec::new()
                    } else {
                        vec![
                            TrackSubtrack {
                                label: "Coverage".to_owned(),
                                detail: None,
                                top: top + 30.0,
                                bottom: top + 90.0,
                                collapsed: false,
                                composite_member: None,
                            },
                            TrackSubtrack {
                                label: "Reads".to_owned(),
                                detail: None,
                                top: top + 100.0,
                                bottom,
                                collapsed: false,
                                composite_member: None,
                            },
                        ]
                    },
                }]
            } else {
                Vec::new()
            };
            self.apply_canvas_scroll(
                &mut display,
                &mut DisplayList::default(),
                bottom,
                self.canvas_geometry().viewport.top(),
            );
            self.panels.active_mut().display_list = display;
            self.composite_display_list = self.panels.active().display_list.clone();
        } else {
            self.rebuild_all_panel_displays();
        }
        self.prepare_rectangle_renderer();
        self.display_dirty = false;
    }

    fn prepare_rectangle_renderer(&mut self) {
        let trace_started = self.interaction_trace.is_active().then(Instant::now);
        if trace_started.is_some() {
            self.interaction_trace
                .record(InteractionTraceStage::RendererPrepareStarted);
        }
        let logical_height = self.surface_config.height as f32 / self.window.scale_factor() as f32;
        let logical_width = self.surface_config.width as f32 / self.window.scale_factor() as f32;
        self.rectangle_renderer.prepare(
            &self.device,
            &self.queue,
            [logical_width, logical_height],
            &self.composite_display_list,
            (self.panels.active().selected_alignment_track
                == Some(self.primary_alignment_track_id()))
            .then_some(self.panels.active().selected_record)
            .flatten()
            .and_then(|index| u32::try_from(index + 1).ok()),
            self.panels
                .active()
                .hovered_record
                .filter(|(track_id, _)| *track_id == self.primary_alignment_track_id())
                .and_then(|(_, index)| u32::try_from(index + 1).ok()),
        );
        if let Some(started) = trace_started {
            let stats = self.rectangle_renderer.last_prepare_stats();
            self.interaction_trace.record_renderer_prepare(
                started.elapsed(),
                stats.total_instances,
                stats.uploaded_bytes,
                stats.buffer_reallocated,
                stats.cpu_capacity_grew,
            );
        }
        self.display_upload_pending_metrics = true;
    }
}
