pub(crate) mod alignment;
mod alignment_lanes;
pub(crate) mod auxiliary;
mod auxiliary_lanes;
mod genomic_pass;
pub(crate) mod hit_test;
pub(crate) mod reference_lanes;

use super::*;
use genomic_pass::GenomicDisplayPass;

pub(super) const COLLAPSED_ALIGNMENT_SUBTRACK_HEIGHT: f32 = 20.0;
pub(super) const READ_PANE_HEADER_HEIGHT: f32 = ui::TRACK_HEADER_HEIGHT;
pub(super) const PINNED_ALIGNMENT_COVERAGE_HEIGHT: f32 = 48.0;
pub(super) const PINNED_ALIGNMENT_JUNCTION_COLLAPSED_HEIGHT: f32 = 52.0;
pub(super) const PINNED_ALIGNMENT_JUNCTION_EXPANDED_HEIGHT: f32 = 120.0;
pub(super) const PINNED_ALIGNMENT_MODIFICATION_HEIGHT: f32 = 26.0;
pub(crate) const ANNOTATION_PANE_BACKGROUND: Rgba = Rgba::new(0.953, 0.969, 0.984, 1.0);
const SYNTHETIC_READ_ROW_HEIGHT: f32 = 5.0;
pub(crate) const SYNTHETIC_READ_ROW_STRIDE: f32 = 7.0;

fn alignment_text_primitive(
    label: &rgv_alignment_scene::CpuProjectedPrimitive,
) -> Option<rgv_layout::TextPrimitive> {
    let text = label.text.as_deref()?;
    let color = match label.component {
        AlignmentComponent::Base { base, .. } => match base.to_ascii_uppercase() {
            // Dark ink has the stronger contrast on the bright A/G fills; white remains clearer
            // on the darker C/T fills. N and other ambiguous symbols use white on neutral grey.
            b'A' | b'G' => Rgba::new(0.06, 0.07, 0.08, 0.96),
            _ => Rgba::new(1.0, 1.0, 1.0, 0.98),
        },
        _ => Rgba::new(0.08, 0.09, 0.11, 0.94),
    };
    let font_size = if matches!(label.component, AlignmentComponent::Base { .. }) {
        (label.width.min(label.height) as f32 - 1.0).clamp(8.0, 12.0)
    } else {
        (label.height as f32 - 2.0).clamp(8.0, 12.0)
    };
    Some(rgv_layout::TextPrimitive {
        x: (label.x + label.width * 0.5) as f32,
        y: (label.y + label.height * 0.5) as f32,
        text: text.to_owned(),
        font_size,
        color,
    })
}

fn display_content_bottom(display: &DisplayList, fallback: f32) -> f32 {
    let rectangle_bottom = display
        .rectangles
        .iter()
        .map(|rectangle| rectangle.y + rectangle.height)
        .fold(fallback, f32::max);
    display
        .texts
        .iter()
        .map(|text| text.y + text.font_size)
        .fold(rectangle_bottom, f32::max)
}

/// Projects the camera-specific immutable row profile into the existing blue read navigator.
/// Raw record/connector counts are intentionally retained until this UI boundary; logarithmic
/// normalization keeps both sparse and extreme-depth loci legible without an architecture-tuned
/// count threshold.
pub(crate) fn alignment_row_activity_overview(
    profile: &AlignmentRowActivityProfile,
) -> ui::ScrollbarOverview {
    alignment_activity_overview_from_counts(
        profile
            .bins()
            .iter()
            .map(|bin| bin.record_count().saturating_add(bin.connector_count())),
    )
}

pub(crate) fn alignment_activity_overview_from_counts(
    activity: impl IntoIterator<Item = usize>,
) -> ui::ScrollbarOverview {
    let activity = activity.into_iter().collect::<Vec<_>>();
    let maximum = activity.iter().copied().max().unwrap_or(0);
    let denominator = (maximum as f64).ln_1p();
    let bins = activity
        .into_iter()
        .map(|value| ui::ScrollbarOverviewBin {
            density: if maximum == 0 {
                0.0
            } else {
                ((value as f64).ln_1p() / denominator).clamp(0.0, 1.0) as f32
            },
            kind: (value > 0).then_some(TrackKind::Alignment),
        })
        .collect();
    ui::ScrollbarOverview {
        bins,
        boundaries: Vec::new(),
    }
}

/// Returns the exclusive active row end and corresponding read-world bottom for the current
/// genomic camera. The atlas may retain many more rows for immediate horizontal navigation; that
/// broader residency is not a vertical scrollbar authority.
pub(crate) fn alignment_scene_read_extent(
    active_row_end: usize,
    content_top: f32,
    row_height: f32,
    minimum_bottom: f32,
) -> (usize, f32) {
    let content_bottom = content_top + active_row_end as f32 * row_height + 6.0;
    (active_row_end, content_bottom.max(minimum_bottom))
}

pub(crate) fn resolve_read_panel_overview(
    authoritative: Option<&ui::ScrollbarOverview>,
    provisional: impl FnOnce() -> ui::ScrollbarOverview,
) -> ui::ScrollbarOverview {
    authoritative.cloned().unwrap_or_else(provisional)
}

/// Reconciles one vertical read camera only when the current display cache has settled row
/// authority. A compatible pending scene deliberately leaves every camera scalar untouched: the
/// UI hides its extent separately, and the exact successor can therefore resume the same stable
/// row rather than jumping to row zero at a prefetch boundary.
pub(crate) fn reconcile_read_panel_scroll_for_authority(
    state: &mut ReadPanelScrollState,
    authority: ReadRowCoordinateAuthority,
    row_height: Option<f32>,
    actual_bottom: f32,
    clip_bottom: f32,
    camera_padding: f32,
    density_drag_fraction: Option<f32>,
) -> Option<(f32, ReadRowCoordinateSpace)> {
    if authority.is_pending() {
        return None;
    }

    state.content_origin_points = state
        .content_origin_points
        .clamp(0.0, camera_padding.max(0.0));
    let exact = row_height.zip(authority.exact());
    if let Some((row_height, coordinate_space)) = exact {
        if let Some(anchor) = state.stable_row_anchor
            && let Some(effective_scroll) =
                reproject_stable_row_camera(anchor, row_height, coordinate_space)
        {
            state.scroll_points = state.content_origin_points + effective_scroll;
        }
    } else {
        state.stable_row_anchor = None;
    }

    let maximum_scroll = (actual_bottom + camera_padding - clip_bottom).max(0.0);
    let previous_maximum = (state.content_bottom_points - clip_bottom).max(0.0);
    (state.scroll_points, state.latent_scroll_fraction) = reconcile_vertical_scroll(
        state.scroll_points,
        previous_maximum,
        maximum_scroll,
        state.latent_scroll_fraction,
        density_drag_fraction,
    );
    state.content_bottom_points = clip_bottom + maximum_scroll;
    exact
}

/// Pending activity keeps the last exact thumb geometry visible as a provisional, noninteractive
/// waypoint. It does not promote the retained bound to exact row authority: gesture code still
/// rejects every non-`Exact` state. `Absent` has no presented lane and exposes no extent.
pub(crate) fn read_panel_scrollbar_maximum(
    state: ReadPanelScrollState,
    clip_bottom: f32,
    authority: ReadRowCoordinateAuthority,
) -> f32 {
    match authority {
        ReadRowCoordinateAuthority::Exact(_) | ReadRowCoordinateAuthority::Pending => {
            (state.content_bottom_points - clip_bottom).max(0.0)
        }
        ReadRowCoordinateAuthority::Absent => 0.0,
    }
}

pub(crate) fn annotation_frame_content_height(frames: &[TrackFrame]) -> Option<f32> {
    let mut annotation_frames = frames
        .iter()
        .filter(|frame| frame.channel == ui::TrackPane::Annotation);
    let first = annotation_frames.next()?;
    let top = annotation_frames
        .clone()
        .map(|frame| frame.top)
        .fold(first.top, f32::min);
    let bottom = annotation_frames
        .map(|frame| frame.bottom)
        .fold(first.bottom, f32::max);
    Some((bottom - top).max(0.0))
}

pub(crate) fn synthetic_read_rows_for_bounds(read_top: f32, read_bottom: f32) -> u32 {
    let available_height = (read_bottom - read_top).max(0.0);
    (((available_height - SYNTHETIC_READ_ROW_HEIGHT).max(0.0) / SYNTHETIC_READ_ROW_STRIDE).floor()
        as u32)
        .saturating_add(1)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrackSectionGeometry {
    pub(crate) annotation_clip_top: f32,
    pub(crate) annotation_clip_bottom: f32,
    pub(crate) read_clip_top: f32,
}

pub(crate) fn track_section_geometry(
    viewport_top: f32,
    viewport_bottom: f32,
    read_pane_height: f32,
    annotation_expanded: bool,
    reads_expanded: bool,
    annotation_content_height: Option<f32>,
) -> TrackSectionGeometry {
    let section_header_height = ui::TRACK_SECTION_HEADER_HEIGHT;
    let annotation_clip_top = (viewport_top + section_header_height).min(viewport_bottom);
    let viewport_height = (viewport_bottom - viewport_top).max(1.0);
    let configured_read_height = read_pane_height.clamp(100.0, (viewport_height - 80.0).max(100.0));
    let configured_annotation_clip_bottom =
        (viewport_bottom - configured_read_height).max(annotation_clip_top + 54.0);
    let content_sized_annotation_clip_bottom =
        annotation_content_height.map_or(configured_annotation_clip_bottom, |content_height| {
            (annotation_clip_top + content_height.max(0.0)).min(configured_annotation_clip_bottom)
        });
    let annotation_clip_bottom = match (annotation_expanded, reads_expanded) {
        (false, _) => annotation_clip_top,
        (true, false) => (viewport_bottom - section_header_height).max(annotation_clip_top),
        (true, true) => content_sized_annotation_clip_bottom,
    }
    .clamp(annotation_clip_top, viewport_bottom);
    let read_clip_top = (annotation_clip_bottom + section_header_height).min(viewport_bottom);
    TrackSectionGeometry {
        annotation_clip_top,
        annotation_clip_bottom,
        read_clip_top,
    }
}

pub(crate) fn synthetic_section_display(
    width: f32,
    maximum_alignments: u32,
    viewport_bottom: f32,
    geometry: TrackSectionGeometry,
    annotation_visible: bool,
    reads_visible: bool,
) -> DisplayList {
    let annotation_content_top = geometry.annotation_clip_top + READ_PANE_HEADER_HEIGHT;
    let read_content_top = geometry.read_clip_top + READ_PANE_HEADER_HEIGHT;
    let mut display = DisplayList::default();
    if annotation_visible {
        let mut coverage = DisplayList::synthetic_coverage_at(width, annotation_content_top + 4.0);
        coverage.scroll_and_clip_vertical(
            0.0,
            annotation_content_top,
            geometry.annotation_clip_bottom,
        );
        display.rectangles.extend(coverage.rectangles);
    }
    if reads_visible && read_content_top < viewport_bottom {
        let reads = DisplayList::synthetic_reads_at(
            width,
            synthetic_read_rows_for_bounds(read_content_top, viewport_bottom),
            maximum_alignments,
            read_content_top,
        );
        display.rectangles.extend(reads.rectangles);
    }
    display
}

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

pub(super) fn collapsed_alignment_subtrack(
    alignment_subtrack: AlignmentSubtrack,
    top: f32,
) -> TrackSubtrack {
    TrackSubtrack {
        alignment_subtrack: Some(alignment_subtrack),
        label: alignment_subtrack.label().to_owned(),
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
    pub(crate) fn rebuild_genomic_display(&mut self) {
        let annotation_auto_fit = self.track_presentation.annotation_pane_auto_fit;
        let annotation_content_height = annotation_auto_fit
            .then(|| annotation_frame_content_height(&self.panels.active().track_frames))
            .flatten();
        let measured_content_height = self.rebuild_genomic_display_pass(annotation_content_height);
        if annotation_auto_fit
            && self.track_presentation.annotation_section_expanded
            && self.track_presentation.read_section_expanded
            && annotation_content_height
                .is_none_or(|previous| (previous - measured_content_height).abs() >= 0.5)
        {
            self.rebuild_genomic_display_pass(Some(measured_content_height));
        }
    }

    fn rebuild_genomic_display_pass(&mut self, annotation_content_height: Option<f32>) -> f32 {
        let primary_alignment_name = self.alignment_track_name();
        let primary_source_label = self
            .primary_alignment_source()
            .map(TrackSource::redacted_label);
        let alignment_track_groups = self.alignment_track_group_chrome();
        let mut pass =
            self.begin_genomic_display_pass(annotation_content_height, &alignment_track_groups);
        self.push_reference_lane(&mut pass);
        self.push_pinned_gene_lanes(&mut pass);
        self.push_primary_alignment_annotation_frame(
            &mut pass,
            primary_alignment_name,
            primary_source_label,
        );
        self.push_secondary_alignment_annotation_frames(&mut pass);
        self.push_read_panel_frames(&mut pass);
        self.push_auxiliary_lanes(&mut pass);
        self.settle_annotation_frames(&mut pass, &alignment_track_groups);
        self.commit_genomic_display_pass(pass)
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
        let native_junction_labels = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .filter(|bundle| {
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            })
            .and_then(|bundle| bundle.summaries.as_ref())
            .map(|frame| self.alignment_summary_gpu_renderer.junction_labels(frame))
            .unwrap_or_default();
        let native_read_labels = self
            .alignment_presented_epoch
            .as_ref()
            .and_then(|epoch| epoch.gpu_bundle.as_ref())
            .filter(|bundle| {
                alignment_gpu_bundle_is_current(bundle, &self.alignment_scene_publication_authority)
            })
            .and_then(|bundle| {
                bundle.reads.as_ref().map(|frame| {
                    bundle
                        .read_authorities
                        .iter()
                        .zip(frame.label_frames())
                        .filter(|(authority, _)| authority.panel_id == active_panel_id)
                        .flat_map(|(_, frame)| frame.labels.iter())
                        .filter_map(alignment_text_primitive)
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default();
        let active_drag = self
            .vertical_overview_drag
            .filter(|drag| drag.density_dragging && drag.panel_id == active_panel_id);
        let current_alignment_target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
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
                    read_panel.row_origin,
                    read_panel.row_height,
                    current_read_row_authority(
                        read_panel.presentation_target_revision,
                        current_alignment_target_revision,
                        self.alignment_scene_publication_authority
                            .committed_row_target_matches_desired(
                                active_panel_id,
                                read_panel.track_id,
                            ),
                        read_panel.row_coordinate_authority,
                    ),
                    self.alignment_density_for_panel_track(
                        self.panels.active().id,
                        read_panel.track_id,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let primary_track_id = self.primary_alignment_track_id();
        let pending_alignment_reveal =
            self.panels
                .active()
                .pending_alignment_reveal
                .and_then(|(track_id, handle)| {
                    let presented = self.presented_alignment_lane(active_panel_id, track_id)?;
                    let row = usize::try_from(
                        presented
                            .lane
                            .snapshot
                            .as_ref()?
                            .reads()
                            .atlas()
                            .record(handle)?
                            .record
                            .row(),
                    )
                    .ok()?;
                    Some((track_id, row))
                });
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
        let compatibility_primary = ReadPanelScrollState {
            scroll_points: panel.vertical_scroll_points,
            content_origin_points: panel.vertical_content_origin_points,
            latent_scroll_fraction: panel.latent_vertical_scroll_fraction,
            content_bottom_points: panel.content_bottom_points,
            density: None,
            stable_row_anchor: None,
            scene_contig: None,
        };
        for (
            track_id,
            actual_bottom,
            clip_top,
            clip_bottom,
            density_camera,
            row_origin,
            row_height,
            row_coordinate_authority,
            density,
        ) in &read_panel_metrics
        {
            let state = panel
                .read_panel_scrolls
                .entry(*track_id)
                .or_insert_with(|| {
                    if *track_id == primary_track_id {
                        compatibility_primary
                    } else {
                        ReadPanelScrollState::default()
                    }
                });
            let camera_padding = if *density_camera {
                vertical_density_camera_padding((clip_bottom - clip_top).max(1.0), *density)
            } else {
                0.0
            };
            let density_drag_fraction = active_drag
                .filter(|drag| drag.track_id == *track_id)
                .map(|drag| drag.virtual_scroll_fraction);
            let exact_row_camera = reconcile_read_panel_scroll_for_authority(
                state,
                *row_coordinate_authority,
                *row_height,
                *actual_bottom,
                *clip_bottom,
                camera_padding,
                density_drag_fraction,
            );
            if row_coordinate_authority.is_pending() {
                continue;
            }
            let maximum_scroll = (state.content_bottom_points - clip_bottom).max(0.0);
            if let (Some((reveal_track, row)), Some(row_origin), Some(row_height)) =
                (pending_alignment_reveal, *row_origin, *row_height)
                && reveal_track == *track_id
            {
                let maximum_effective = (maximum_scroll - state.content_origin_points).max(0.0);
                let effective_scroll = center_read_row_scroll(
                    row_origin,
                    row_height,
                    row,
                    *clip_top,
                    *clip_bottom,
                    maximum_effective,
                );
                state.scroll_points = state.content_origin_points + effective_scroll;
                state.latent_scroll_fraction = (maximum_scroll > f32::EPSILON)
                    .then_some((state.scroll_points / maximum_scroll).clamp(0.0, 1.0));
                panel.pending_alignment_reveal = None;
            }
            if let Some((row_height, coordinate_space)) = exact_row_camera
                && row_height > f32::EPSILON
            {
                state.stable_row_anchor = capture_stable_row_camera(
                    state.effective_scroll_points(),
                    row_height,
                    coordinate_space,
                );
            }
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
        display.texts.extend(
            native_junction_labels
                .into_iter()
                .filter(|label| label.coherence_scope == active_panel_id.get())
                .map(|label| rgv_layout::TextPrimitive {
                    x: label.x as f32,
                    y: label.y as f32,
                    text: label.text,
                    font_size: 10.0,
                    color: Rgba::new(0.10, 0.10, 0.12, 0.94),
                }),
        );
        display.texts.extend(native_read_labels);
        let (interaction_enabled, content_revision) = {
            let presentation = &self.panels.active().navigation_presentation;
            (
                presentation.is_interactive(),
                presentation.committed_content_revision,
            )
        };
        let previous_hover = self.panels.active().hovered_junction_arc;
        let native_hover_authority = previous_hover.is_some_and(|hover| {
            self.native_junction_lane_is_present(active_panel_id, hover.key.track_id)
        });
        let valid_hover = (interaction_enabled && native_hover_authority)
            .then_some(previous_hover)
            .flatten()
            .map(|hover| JunctionArcHover {
                content_revision,
                ..hover
            });
        if valid_hover != previous_hover {
            self.panels.active_mut().hovered_junction_arc = valid_hover;
            self.window.set_cursor(if valid_hover.is_some() {
                CursorIcon::Pointer
            } else {
                CursorIcon::Default
            });
            if valid_hover.is_none()
                && matches!(
                    self.panels.active().hover_inspector_target,
                    Some(HoverInspectorTarget::Junction(..))
                )
            {
                self.panels.active_mut().hover_inspector_target = None;
            }
        }
        self.sashimi_interaction_diagnostics
            .latest_hover_overlay_count = usize::from(valid_hover.is_some());
        self.add_region_overlays(&mut display);
        let generic_display = display.clone();
        let mut contextual_display = DisplayList::default();
        self.panels
            .active()
            .context_scene
            .projected
            .append_scrolled_and_clipped_vertical(
                annotation_scroll,
                annotation_clip_top,
                annotation_clip_bottom,
                &mut contextual_display,
            );
        contextual_display.translate_and_clip_horizontal(
            panel_rect.left(),
            panel_rect.left(),
            panel_rect.right(),
        );
        display.rectangles.extend(contextual_display.rectangles);
        display.texts.extend(contextual_display.texts);
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
        let panel = self.panels.active_mut();
        panel.generic_display_list = generic_display;
        panel.display_list = display;
        true
    }

    pub(crate) fn refresh_active_panel_scroll_display(&mut self) -> bool {
        let refusal = if self.display_dirty {
            Some("display derivative is already dirty")
        } else if !self
            .panels
            .active()
            .scroll_display_cache
            .as_ref()
            .is_some_and(|cache| cache.supports_fast_scroll)
        {
            Some("active panel has no fast-scroll display cache")
        } else if !self.active_panel_materialization_covers_scroll() {
            Some("requested rows fall outside the materialized scroll cache")
        } else {
            None
        };
        if let Some(reason) = refusal {
            if self.manual_diagnostic_trace_is_recording() {
                let panel_id = self.panels.active().id.get();
                let authorities = self.manual_active_scroll_cache_diagnostic_state();
                self.record_manual_diagnostic_event(
                    "vertical_navigation",
                    "fast_scroll_refresh_refused",
                    serde_json::json!({
                        "reason": reason,
                        "panel_id": panel_id,
                        "scroll_cache_authorities": authorities,
                    }),
                );
            }
            return false;
        }
        if !self.rebuild_active_panel_from_scroll_cache() {
            if self.manual_diagnostic_trace_is_recording() {
                let panel_id = self.panels.active().id.get();
                let authorities = self.manual_active_scroll_cache_diagnostic_state();
                self.record_manual_diagnostic_event(
                    "vertical_navigation",
                    "fast_scroll_refresh_refused",
                    serde_json::json!({
                        "reason": "scroll cache disappeared before reprojection",
                        "panel_id": panel_id,
                        "scroll_cache_authorities": authorities,
                    }),
                );
            }
            return false;
        }
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        self.prepare_rectangle_renderer();
        true
    }

    /// Reprojects immutable contextual-lane leases under the desired genomic camera. This is the
    /// horizontal-navigation hot path: it performs no source query, parsing, feature packing, or
    /// label measurement. Each lane retains independent readiness and contributes only its own
    /// geometry.
    pub(crate) fn reproject_active_context_lanes(&mut self) -> bool {
        if self.synthetic || self.panels.active().scroll_display_cache.is_none() {
            return false;
        }
        let target = self.panels.active().viewport;
        let frames = self.panels.active().track_frames.clone();
        {
            let panel = self.panels.active_mut();
            panel.context_scene.update_target_readiness(target);
            panel.context_scene.project_camera_text(target, &frames);
        }
        if !self.rebuild_active_panel_from_scroll_cache() {
            return false;
        }
        self.composite_display_list = composite_panel_display_lists(&self.panels);
        // Generic screen-space chrome is invariant under horizontal genomic movement. Contextual
        // shapes already live in source-coordinate GPU buffers, so only their camera uniforms
        // (plus the bounded text projection above) change on this path.
        self.prepare_context_scene_renderer();
        true
    }

    pub(crate) fn active_read_panel_materialization_coverage(
        &self,
        track_id: TrackId,
    ) -> ReadPanelMaterializationCoverage {
        let panel = self.panels.active();
        let Some(cache) = panel.scroll_display_cache.as_ref() else {
            return ReadPanelMaterializationCoverage::Unknown;
        };
        let Some(read_panel) = cache
            .read_panels
            .iter()
            .find(|read_panel| read_panel.track_id == track_id)
        else {
            return ReadPanelMaterializationCoverage::Unknown;
        };
        let required = read_panel
            .total_rows
            .zip(read_panel.row_origin)
            .zip(read_panel.row_height)
            .map(|((total_rows, row_origin), row_height)| {
                alignment::materialized_alignment_row_range(
                    row_origin,
                    row_height,
                    read_panel.clip_top,
                    read_panel.clip_bottom,
                    panel
                        .read_panel_scroll_state(read_panel.track_id)
                        .effective_scroll_points(),
                    total_rows,
                    0,
                )
            });
        read_panel_materialization_coverage(read_panel.materialized_row_ranges.as_deref(), required)
    }

    fn active_panel_materialization_covers_scroll(&self) -> bool {
        self.panels
            .active()
            .scroll_display_cache
            .as_ref()
            .is_some_and(|cache| {
                cache
                    .read_panels
                    .iter()
                    .filter(|read_panel| read_panel.density_camera)
                    .all(|read_panel| {
                        self.active_read_panel_materialization_coverage(read_panel.track_id)
                            .is_covered()
                    })
            })
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
        // Alignment and contextual lanes publish independently. No panel-wide transaction may
        // prevent a ready lane from rebuilding its own presentation.
        let rebuildable_panel_ids = panel_ids;
        let requested_annotation_scroll = self.panels.annotation_scroll_points;
        self.shared_annotation_frame_heights.clear();
        for panel_id in &rebuildable_panel_ids {
            self.panels.annotation_scroll_points = requested_annotation_scroll;
            self.panels.activate(*panel_id);
            self.rebuild_genomic_display();
        }
        if self.panels.len() > 1 {
            self.shared_annotation_frame_heights = maximum_annotation_frame_heights(&self.panels);
            for panel_id in &rebuildable_panel_ids {
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
            for panel_id in &rebuildable_panel_ids {
                self.panels.activate(*panel_id);
                let _ = self.rebuild_active_panel_from_scroll_cache();
            }
        }
        self.panels.activate(active_id);

        self.composite_display_list = composite_panel_display_lists(&self.panels);
    }

    pub(crate) fn rebuild_display_list(&mut self) {
        if self.begin_ready_navigation_commits() {
            self.mark_display_dirty("ready navigation transaction committed");
        }
        if !self.display_dirty {
            return;
        }
        if self.synthetic {
            let viewport = self.canvas_geometry().viewport;
            let width = self.panels.active().viewport.pixel_width() as f32;
            let annotation_section_expanded = self.track_presentation.annotation_section_expanded;
            let read_section_expanded = self.track_presentation.read_section_expanded;
            let track_visible = self.primary_alignment_visible();
            let annotation_content_visible = track_visible
                && !self.primary_alignment_collapsed()
                && annotation_section_expanded
                && self
                    .track_presentation
                    .effective_track_visibility(
                        ui::TrackPane::Annotation,
                        SYNTHETIC_ALIGNMENT_TRACK_ID,
                    )
                    .unwrap_or(true);
            let read_content_visible = track_visible
                && read_section_expanded
                && self
                    .track_presentation
                    .effective_track_visibility(ui::TrackPane::Reads, SYNTHETIC_ALIGNMENT_TRACK_ID)
                    .unwrap_or(true);
            let coverage_visible =
                annotation_content_visible && self.primary_alignment_filters().show_coverage;
            let annotation_content_height = if track_visible && annotation_section_expanded {
                READ_PANE_HEADER_HEIGHT
                    + if coverage_visible {
                        display_content_bottom(&DisplayList::synthetic_coverage_at(width, 4.0), 0.0)
                    } else {
                        0.0
                    }
            } else {
                0.0
            };
            let section_geometry = track_section_geometry(
                viewport.top(),
                viewport.bottom(),
                self.track_presentation.read_pane_height,
                annotation_section_expanded,
                read_section_expanded,
                self.track_presentation
                    .annotation_pane_auto_fit
                    .then_some(annotation_content_height),
            );
            let annotation_content_top =
                section_geometry.annotation_clip_top + READ_PANE_HEADER_HEIGHT;
            let read_content_top = section_geometry.read_clip_top + READ_PANE_HEADER_HEIGHT;
            let mut display = synthetic_section_display(
                width,
                u32::try_from(self.primary_alignment_limit()).unwrap_or(u32::MAX),
                viewport.bottom(),
                section_geometry,
                coverage_visible,
                read_content_visible,
            );
            let mut frames = Vec::new();
            if track_visible && annotation_section_expanded {
                let annotation_bottom = if annotation_content_visible {
                    section_geometry.annotation_clip_bottom
                } else {
                    annotation_content_top.min(section_geometry.annotation_clip_bottom)
                };
                frames.push(TrackFrame {
                    key: TrackFrameKey::Alignment(SYNTHETIC_ALIGNMENT_TRACK_ID),
                    channel: ui::TrackPane::Annotation,
                    name: "Synthetic reads".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: Some("Synthetic GPU workload".to_owned()),
                    top: section_geometry.annotation_clip_top,
                    bottom: annotation_bottom,
                    visible: true,
                    collapsed: !annotation_content_visible,
                    frozen_bottom: None,
                    subtracks: if annotation_content_visible {
                        vec![TrackSubtrack {
                            alignment_subtrack: Some(AlignmentSubtrack::Coverage),
                            label: "Coverage".to_owned(),
                            detail: None,
                            top: annotation_content_top,
                            bottom: annotation_bottom,
                            collapsed: false,
                            composite_member: None,
                        }]
                    } else {
                        Vec::new()
                    },
                });
            }
            if track_visible && read_section_expanded {
                let read_bottom = if read_content_visible {
                    viewport.bottom()
                } else {
                    read_content_top.min(viewport.bottom())
                };
                frames.push(TrackFrame {
                    key: TrackFrameKey::Alignment(SYNTHETIC_ALIGNMENT_TRACK_ID),
                    channel: ui::TrackPane::Reads,
                    name: "Synthetic reads".to_owned(),
                    kind: TrackKind::Alignment,
                    source_label: Some("Synthetic GPU workload".to_owned()),
                    top: section_geometry.read_clip_top,
                    bottom: read_bottom,
                    visible: true,
                    collapsed: !read_content_visible,
                    frozen_bottom: Some(read_content_top.min(read_bottom)),
                    subtracks: if read_content_visible {
                        vec![TrackSubtrack {
                            alignment_subtrack: Some(AlignmentSubtrack::Reads),
                            label: "Reads".to_owned(),
                            detail: None,
                            top: read_content_top,
                            bottom: read_bottom,
                            collapsed: false,
                            composite_member: None,
                        }]
                    } else {
                        Vec::new()
                    },
                });
            }
            let pane_layout = ui::WorkspacePaneLayout {
                annotation: pane_layout_snapshot(
                    &self.track_presentation,
                    &frames,
                    ui::TrackPane::Annotation,
                    section_geometry.annotation_clip_top,
                    section_geometry.annotation_clip_bottom,
                    section_geometry.annotation_clip_bottom,
                    0.0,
                    0.0,
                    width,
                ),
                reads: pane_layout_snapshot(
                    &self.track_presentation,
                    &frames,
                    ui::TrackPane::Reads,
                    section_geometry.read_clip_top,
                    viewport.bottom(),
                    viewport.bottom(),
                    0.0,
                    0.0,
                    width,
                ),
            };
            self.apply_canvas_scroll(
                &mut display,
                &mut DisplayList::default(),
                viewport.bottom(),
                viewport.top(),
            );
            let panel = self.panels.active_mut();
            panel.track_frames = frames;
            panel.pane_layout = pane_layout;
            panel.display_list = display;
            self.composite_display_list = self.panels.active().display_list.clone();
        } else {
            self.rebuild_all_panel_displays();
        }
        self.prepare_rectangle_renderer();
        self.clear_display_dirty("display list and renderer derivative rebuilt");
        self.refresh_alignment_scene_cameras_after_layout();
    }

    fn prepare_rectangle_renderer(&mut self) {
        self.prepare_context_scene_renderer();
        self.prepare_generic_rectangle_renderer();
    }

    /// Rebuilds or adopts the generic screen-space rectangle slab. Horizontal genomic camera
    /// movement must not call this method because it does not change generic chrome.
    fn prepare_generic_rectangle_renderer(&mut self) {
        // Layout and row-scene residency may have changed while rebuilding the
        // display. Give the GPU tier precisely the currently unused headroom
        // before admitting or reusing a slab.
        self.refresh_alignment_scene_budget();
        let trace_started = self.interaction_trace.is_active().then(Instant::now);
        if trace_started.is_some() {
            self.interaction_trace
                .record(InteractionTraceStage::RendererPrepareStarted);
        }
        let logical_height = self.surface_config.height as f32 / self.window.scale_factor() as f32;
        let logical_width = self.surface_config.width as f32 / self.window.scale_factor() as f32;
        let render_display = composite_panel_generic_display_lists(&self.panels);
        let mut rectangle_slab_key = 0xcbf2_9ce4_8422_2325_u64;
        for rectangle in &render_display.rectangles {
            mix_rectangle_slab_key(&mut rectangle_slab_key, *rectangle);
        }
        self.rectangle_renderer.prepare_with_slab_key(
            &self.device,
            &self.queue,
            [logical_width, logical_height],
            &render_display,
            None,
            None,
            Some(rectangle_slab_key),
            self.context_lane_gpu_budget_bytes,
        );
        // REND-14: an over-budget display list is drawn truncated. Say so
        // once when that starts, not on every frame it persists.
        if let Some(message) = self.shell.dropped_rectangles.observe(
            self.rectangle_renderer.dropped_rectangles(),
            RectangleRenderer::maximum_rectangles_per_frame(),
        ) {
            tracing::warn!(%message, "generic rectangle renderer dropped primitives");
            self.shell.notifications.push(
                app::shell_state::NotificationSeverity::Warning,
                None,
                message,
            );
            self.window.request_redraw();
        }
        // A newly admitted slab becomes part of the next decoded/layout
        // allowance immediately rather than waiting for another interaction.
        self.refresh_alignment_scene_budget();
        if let Some(started) = trace_started {
            let stats = self.rectangle_renderer.last_prepare_stats();
            self.interaction_trace.record_renderer_prepare(
                started.elapsed(),
                stats.total_instances,
                stats.uploaded_bytes,
                stats.buffer_reallocated,
                stats.cpu_capacity_grew,
                stats.slab_cache_hit,
            );
        }
        self.display_upload_pending_metrics = true;
    }

    fn prepare_context_scene_renderer(&mut self) {
        let scale_factor = self.window.scale_factor() as f32;
        let logical_height = self.surface_config.height as f32 / scale_factor.max(f32::EPSILON);
        let logical_width = self.surface_config.width as f32 / scale_factor.max(f32::EPSILON);
        let annotation_scroll = self.panels.annotation_scroll_points;
        let surface_width = self.surface_config.width;
        let surface_height = self.surface_config.height;
        let rectangle_resident_bytes = self.rectangle_renderer.resident_bytes();
        let context_budget_bytes = self
            .aggregate_cache_budget_bytes()
            .saturating_sub(self.alignment_ledger_occupied_bytes())
            .saturating_sub(rectangle_resident_bytes);
        let requests = self
            .panels
            .iter()
            .flat_map(|panel| {
                let Some(panel_layout) = self.workspace_layout.panel(panel.id) else {
                    return Vec::new().into_iter();
                };
                let Some(cache) = panel.scroll_display_cache.as_ref() else {
                    return Vec::new().into_iter();
                };
                panel
                    .context_scene
                    .gpu_publications()
                    .filter_map(|publication| {
                        let desired = publication.desired;
                        if !desired
                            .source_viewport
                            .interval()
                            .overlaps(panel.viewport.interval())
                        {
                            return None;
                        }
                        let camera_for = |lease: &ContextLaneLease| {
                            let frame = panel
                                .track_frames
                                .iter()
                                .find(|frame| frame.key == lease.frame_key)?;
                            let lane_top =
                                (frame.top - annotation_scroll).max(cache.annotation_clip_top);
                            let lane_bottom = (frame.bottom - annotation_scroll)
                                .min(cache.annotation_clip_bottom);
                            let logical_clip = egui::Rect::from_min_max(
                                egui::pos2(panel_layout.canvas.left(), lane_top),
                                egui::pos2(panel_layout.canvas.right(), lane_bottom.max(lane_top)),
                            );
                            let physical = self.workspace_layout.physical_rect_scissor(
                                logical_clip,
                                scale_factor,
                                surface_width,
                                surface_height,
                            );
                            Some(ContextSceneGpuCamera {
                                source_exact_start: lease.source_viewport.exact_start(),
                                source_bases_per_pixel: lease.source_viewport.bases_per_pixel(),
                                target_exact_start: panel.viewport.exact_start(),
                                target_bases_per_pixel: panel.viewport.bases_per_pixel(),
                                panel_left: panel_layout.canvas.left(),
                                frame_top: frame.top,
                                vertical_scroll: annotation_scroll,
                                scissor: ContextSceneGpuScissor {
                                    x: physical.x,
                                    y: physical.y,
                                    width: physical.width,
                                    height: physical.height,
                                },
                                selected_object_id: None,
                                hovered_object_id: None,
                            })
                        };
                        let camera = camera_for(desired)?;
                        let retained = publication.retained.and_then(|lease| {
                            lease
                                .source_viewport
                                .interval()
                                .overlaps(panel.viewport.interval())
                                .then(|| camera_for(lease))
                                .flatten()
                                .map(|camera| ContextSceneGpuRetainedLaneRequest {
                                    content_revision: lease.revision,
                                    camera,
                                    rectangles: &lease.display.rectangles,
                                })
                        });
                        Some(ContextSceneGpuLaneRequest {
                            key: ContextSceneGpuLaneKey::new(
                                panel.id.get(),
                                desired.key.numeric_id(),
                            ),
                            content_revision: desired.revision,
                            camera,
                            rectangles: &desired.display.rectangles,
                            retained,
                            coherence_group: publication.coherence_group.map(|group| {
                                ContextSceneGpuCoherenceKey::new(panel.id.get(), group)
                            }),
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
            })
            .collect::<Vec<_>>();
        let report = match self.context_scene_gpu_renderer.capture_camera_frame(
            &self.queue,
            [logical_width, logical_height],
            &requests,
        ) {
            Ok(report) => report,
            Err(ContextSceneGpuCameraCaptureError::ContentReplacementRequired { .. }) => {
                self.context_scene_gpu_renderer.prepare(
                    &self.device,
                    &self.queue,
                    [logical_width, logical_height],
                    &requests,
                    context_budget_bytes,
                )
            }
            Err(ContextSceneGpuCameraCaptureError::InvalidCamera { key }) => {
                self.report_error(format!(
                    "Could not capture contextual lane {} camera state",
                    key.lane
                ));
                return;
            }
        };
        drop(requests);

        let mut semantic_projection_changed = false;
        for panel in self.panels.iter_mut() {
            semantic_projection_changed |= panel.context_scene.apply_gpu_publication(
                panel.id,
                panel.viewport,
                &report.frame,
                &report.rejections,
            );
        }
        self.context_scene_gpu_frame = report.frame;
        if semantic_projection_changed {
            let active_id = self.panels.active().id;
            let panel_ids = self.panels.iter().map(|panel| panel.id).collect::<Vec<_>>();
            for panel_id in panel_ids {
                self.panels.activate(panel_id);
                let target = self.panels.active().viewport;
                let frames = self.panels.active().track_frames.clone();
                self.panels
                    .active_mut()
                    .context_scene
                    .project_content(target, &frames);
                let _ = self.rebuild_active_panel_from_scroll_cache();
            }
            self.panels.activate(active_id);
            self.composite_display_list = composite_panel_display_lists(&self.panels);
        }
    }
}

fn mix_rectangle_slab_key(hash: &mut u64, rectangle: RectPrimitive) {
    for bytes in [
        rectangle.x.to_bits().to_le_bytes().as_slice(),
        rectangle.y.to_bits().to_le_bytes().as_slice(),
        rectangle.width.to_bits().to_le_bytes().as_slice(),
        rectangle.height.to_bits().to_le_bytes().as_slice(),
        rectangle.color.red.to_bits().to_le_bytes().as_slice(),
        rectangle.color.green.to_bits().to_le_bytes().as_slice(),
        rectangle.color.blue.to_bits().to_le_bytes().as_slice(),
        rectangle.color.alpha.to_bits().to_le_bytes().as_slice(),
        rectangle.object_id.to_le_bytes().as_slice(),
        (rectangle.shape as u32).to_le_bytes().as_slice(),
    ] {
        for &byte in bytes {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}
