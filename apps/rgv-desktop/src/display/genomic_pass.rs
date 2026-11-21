use crate::display::auxiliary::FIRST_CONTEXT_TRACK_OBJECT_ID;
use crate::display::{
    READ_PANE_HEADER_HEIGHT, alignment, ensure_annotation_presentation_frames,
    pane_layout_snapshot, reflow_annotation_presentation_with_heights, resolve_read_panel_overview,
    track_section_geometry,
};
use crate::ui::gestures::vertical_density_camera_padding;
use crate::ui::{TrackFrame, TrackFrameKey};
use crate::workspace::context_lanes::{
    ContextLaneDraft, ContextLaneKey, buffered_context_interval, context_lane_coherence_groups,
};
use crate::workspace::panels::{PanelScrollDisplayCache, ReadPanelDisplayCache};
use crate::{Viewer, ui};
use rgv_core::PanelId;
use rgv_core::{GenomicInterval, TrackConfig, TrackId, TrackKind, Viewport};
use rgv_layout::{DisplayList, ObjectIdAllocator};
use std::collections::{BTreeMap, BTreeSet};

/// Layout metrics and accumulators shared by the sections of one genomic
/// display rebuild. Sections run in a fixed order and append to the
/// accumulators; the order of those appends is the draw and frame order.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct GenomicDisplayPass {
    pub(super) primary_alignment_id: TrackId,
    pub(super) primary_annotation_desired: bool,
    pub(super) primary_annotation_effective: bool,
    pub(super) annotation_section_expanded: bool,
    pub(super) annotation_clip_top: f32,
    pub(super) annotation_clip_bottom: f32,
    pub(super) read_clip_top: f32,
    pub(super) scroll_clip_bottom: f32,
    pub(super) collapsed_annotation_group_members: BTreeSet<TrackId>,
    pub(super) annotation_group_headers_by_anchor: BTreeMap<TrackId, (u32, usize, bool)>,
    pub(super) read_pane_layout: alignment::ReadPaneLayout,
    pub(super) read_stack_scroll: f32,
    pub(super) read_panel_bounds: Vec<alignment::ReadPanelBounds>,
    pub(super) viewport_interval: GenomicInterval,
    pub(super) target_viewport: Viewport,
    pub(super) generation: u64,
    pub(super) desired_context_interval: GenomicInterval,
    pub(super) effective_auxiliary_configs: Vec<TrackConfig>,
    pub(super) supports_fast_scroll: bool,
    /// Top of the next annotation-pane frame.
    pub(super) top: f32,
    pub(super) annotation_display: DisplayList,
    pub(super) pinned_display: DisplayList,
    pub(super) context_lane_drafts: Vec<ContextLaneDraft>,
    pub(super) visible_context_lanes:
        BTreeMap<ContextLaneKey, (TrackFrameKey, Option<GenomicInterval>)>,
    pub(super) pending_context_lanes: BTreeSet<ContextLaneKey>,
    pub(super) unavailable_context_lanes: BTreeSet<ContextLaneKey>,
    pub(super) read_panel_displays: Vec<ReadPanelDisplayCache>,
    /// Hands every contextual track a disjoint block of pickable object ids
    /// for this pass, sized to its record count.
    pub(super) object_ids: ObjectIdAllocator,
}

impl Viewer {
    /// Resolves section geometry, lays out the read pane, resets the active
    /// panel's track frames, and pushes the read-pane group header frames.
    #[allow(clippy::too_many_lines)]
    pub(super) fn begin_genomic_display_pass(
        &mut self,
        panel_id: PanelId,
        annotation_content_height: Option<f32>,
        alignment_track_groups: &[ui::TrackGroupChrome],
    ) -> GenomicDisplayPass {
        let primary_alignment_id = self.primary_alignment_track_id();
        let geometry = self.canvas_geometry();
        let annotation_section_expanded =
            self.session.track_presentation.annotation_section_expanded;
        let read_section_expanded = self.session.track_presentation.read_section_expanded;
        let section_geometry = track_section_geometry(
            geometry.viewport.top(),
            geometry.viewport.bottom(),
            self.session.track_presentation.read_pane_height,
            annotation_section_expanded,
            read_section_expanded,
            annotation_content_height,
        );
        let annotation_clip_top = section_geometry.annotation_clip_top;
        let annotation_clip_bottom = section_geometry.annotation_clip_bottom;
        let read_clip_top = section_geometry.read_clip_top;
        let scroll_clip_bottom = geometry.viewport.bottom();
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
            .session
            .track_presentation
            .pane_track_state(ui::TrackPane::Annotation, primary_alignment_id)
            .is_some_and(|node| node.content_visible);
        let primary_annotation_effective = annotation_section_expanded
            && primary_source_available
            && self
                .session
                .track_presentation
                .track_content_exposed(ui::TrackPane::Annotation, primary_alignment_id)
                .unwrap_or(false);
        let previous_read_heights = self
            .session
            .panels
            .panel(panel_id)
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
            .session
            .track_presentation
            .pane_tracks
            .iter()
            .filter(|node| read_section_expanded && node.channel == ui::TrackPane::Reads)
            .map(|node| node.track_id)
            .collect::<Vec<_>>();
        let mut read_panel_specs = Vec::new();
        for track_id in read_track_order {
            if !self
                .session
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
                    .session
                    .track_presentation
                    .track_content_exposed(ui::TrackPane::Reads, track_id)
                    .unwrap_or(false);
                if effective_visible {
                    read_panel_specs.push(alignment::ReadPanelSpec::expanded(
                        key,
                        self.session
                            .track_presentation
                            .pane_track_state(ui::TrackPane::Reads, track_id)
                            .and_then(|node| node.preferred_height)
                            .or_else(|| previous_read_heights.get(&track_id).copied()),
                    ));
                } else {
                    read_panel_specs.push(alignment::ReadPanelSpec::header(key));
                }
            }
        }
        let read_pane_layout =
            alignment::read_pane_layout(&read_panel_specs, read_clip_top, scroll_clip_bottom);
        let read_stack_maximum = (read_pane_layout.content_bottom - scroll_clip_bottom).max(0.0);
        let read_stack_scroll = if read_section_expanded {
            self.session
                .panels
                .panel(panel_id)
                .read_stack_scroll_points
                .clamp(0.0, read_stack_maximum)
        } else {
            self.session.panels.panel(panel_id).read_stack_scroll_points
        };
        {
            let panel = self.session.panels.panel_mut(panel_id);
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
        self.session.panels.panel_mut(panel_id).track_frames.clear();
        for group in &read_track_groups {
            let Some(bounds) = alignment::read_panel_bounds_for_group(&read_panel_bounds, group.id)
            else {
                continue;
            };
            self.session
                .panels
                .panel_mut(panel_id)
                .track_frames
                .push(TrackFrame {
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
        let viewport_interval = self.session.panels.panel(panel_id).viewport.interval();
        let target_viewport = self.session.panels.panel(panel_id).viewport;
        let generation = self
            .session
            .panels
            .panel(panel_id)
            .generation_clock
            .current()
            .get();
        let desired_context_interval = buffered_context_interval(
            target_viewport,
            self.session.panels.panel(panel_id).contig_length,
            self.session.settings.read_browsing_buffer_percent,
            None,
        );
        let effective_auxiliary_configs = self.effective_auxiliary_presentation_configs();
        let supports_fast_scroll = !effective_auxiliary_configs.iter().any(|track| {
            track.visible
                && !track.collapsed
                && matches!(track.kind, TrackKind::CopyNumber | TrackKind::Mutation)
        });
        GenomicDisplayPass {
            primary_alignment_id,
            primary_annotation_desired,
            primary_annotation_effective,
            annotation_section_expanded,
            annotation_clip_top,
            annotation_clip_bottom,
            read_clip_top,
            scroll_clip_bottom,
            collapsed_annotation_group_members,
            annotation_group_headers_by_anchor,
            read_pane_layout,
            read_stack_scroll,
            read_panel_bounds,
            viewport_interval,
            target_viewport,
            generation,
            desired_context_interval,
            effective_auxiliary_configs,
            supports_fast_scroll,
            top: annotation_clip_top,
            annotation_display: DisplayList::default(),
            pinned_display: DisplayList::default(),
            context_lane_drafts: Vec::new(),
            visible_context_lanes: BTreeMap::new(),
            pending_context_lanes: BTreeSet::new(),
            unavailable_context_lanes: BTreeSet::new(),
            read_panel_displays: Vec::new(),
            object_ids: ObjectIdAllocator::starting_at(FIRST_CONTEXT_TRACK_OBJECT_ID),
        }
    }

    /// Appends the frames that exist only in the presentation tree, reflows
    /// the annotation pane (or removes it when the section is folded), and
    /// sorts the active panel's track frames.
    #[allow(clippy::too_many_lines)]
    pub(super) fn settle_annotation_frames(
        &mut self,
        panel_id: PanelId,
        pass: &mut GenomicDisplayPass,
        alignment_track_groups: &[ui::TrackGroupChrome],
    ) {
        let annotation_section_expanded = pass.annotation_section_expanded;
        let annotation_clip_top = pass.annotation_clip_top;
        let mut top = pass.top;
        // Reference, gene models, alignment summaries, and every auxiliary
        // track belong to the independently scrollable annotation pane.
        pass.annotation_display
            .rectangles
            .append(&mut pass.pinned_display.rectangles);
        pass.annotation_display
            .texts
            .append(&mut pass.pinned_display.texts);
        for group in alignment_track_groups
            .iter()
            .filter(|group| group.pane == ui::TrackPane::Annotation)
        {
            let key = TrackFrameKey::Group {
                channel: ui::TrackPane::Annotation,
                id: group.id,
            };
            if self
                .session
                .panels
                .panel(panel_id)
                .track_frames
                .iter()
                .any(|frame| frame.key == key)
            {
                continue;
            }
            self.session
                .panels
                .panel_mut(panel_id)
                .track_frames
                .push(TrackFrame {
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
            .session
            .alignment_tracks
            .iter()
            .map(|track| {
                let track_id = track.config.id;
                let desired = self
                    .session
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
        annotation_fallback_frames.extend(self.session.tracks.tracks().iter().map(|track| {
            let desired = self
                .session
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
                &self.session.track_presentation,
                &mut self.session.panels.panel_mut(panel_id).track_frames,
                &annotation_fallback_frames,
                top,
            );
            let shared_annotation_frame_heights = self.shared_annotation_frame_heights.clone();
            top = reflow_annotation_presentation_with_heights(
                &self.session.track_presentation,
                &mut self.session.panels.panel_mut(panel_id).track_frames,
                &mut pass.annotation_display,
                annotation_clip_top,
                &shared_annotation_frame_heights,
            );
        } else {
            // Folding the section is a presentation gate, not a mutation of
            // child rows. Remove only this render revision's annotation
            // frames and primitives; their state and shared camera survive.
            self.session
                .panels
                .panel_mut(panel_id)
                .track_frames
                .retain(|frame| frame.channel != ui::TrackPane::Annotation);
            pass.annotation_display = DisplayList::default();
            pass.pinned_display = DisplayList::default();
            pass.visible_context_lanes.clear();
            pass.context_lane_drafts.clear();
            pass.pending_context_lanes.clear();
            pass.unavailable_context_lanes.clear();
            top = annotation_clip_top;
        }
        self.session
            .panels
            .panel_mut(panel_id)
            .track_frames
            .sort_by(|left, right| {
                left.channel
                    .cmp(&right.channel)
                    .then_with(|| left.top.total_cmp(&right.top))
                    .then_with(|| left.bottom.total_cmp(&right.bottom))
            });
        pass.top = top;
    }

    /// Reconciles the context scene, publishes the pane layout and scroll
    /// display cache, and rebuilds the active panel from that cache. Returns
    /// the measured annotation content height.
    #[allow(clippy::too_many_lines)]
    pub(super) fn commit_genomic_display_pass(
        &mut self,
        panel_id: PanelId,
        pass: GenomicDisplayPass,
    ) -> f32 {
        let GenomicDisplayPass {
            annotation_clip_top,
            annotation_clip_bottom,
            read_clip_top,
            scroll_clip_bottom,
            read_pane_layout,
            read_stack_scroll,
            target_viewport,
            generation,
            effective_auxiliary_configs,
            supports_fast_scroll,
            top,
            annotation_display,
            pinned_display,
            context_lane_drafts,
            visible_context_lanes,
            pending_context_lanes,
            unavailable_context_lanes,
            mut read_panel_displays,
            ..
        } = pass;
        {
            let frames = self.session.panels.panel(panel_id).track_frames.clone();
            let coherence_groups =
                context_lane_coherence_groups(&effective_auxiliary_configs, &visible_context_lanes);
            let panel = self.session.panels.panel_mut(panel_id);
            panel.context_scene.reconcile(
                generation,
                target_viewport,
                &visible_context_lanes,
                context_lane_drafts,
                &pending_context_lanes,
                &unavailable_context_lanes,
                &coherence_groups,
            );
            panel
                .context_scene
                .project_content(target_viewport, &frames);
        }
        let mut annotation_semantic_display = annotation_display.clone();
        annotation_semantic_display.rectangles.extend(
            self.session
                .panels
                .panel(panel_id)
                .context_scene
                .projected
                .rectangles
                .iter()
                .copied(),
        );
        annotation_semantic_display.texts.extend(
            self.session
                .panels
                .panel(panel_id)
                .context_scene
                .projected
                .texts
                .iter()
                .cloned(),
        );
        let measured_annotation_content_height = (top - annotation_clip_top).max(0.0);
        let annotation_content_bottom = annotation_semantic_display
            .rectangles
            .iter()
            .map(|rectangle| rectangle.y + rectangle.height)
            .fold(top, f32::max)
            .max(annotation_clip_bottom);
        let pane_layout = {
            let frames = &self.session.panels.panel(panel_id).track_frames;
            ui::WorkspacePaneLayout {
                annotation: pane_layout_snapshot(
                    &self.session.track_presentation,
                    frames,
                    ui::TrackPane::Annotation,
                    annotation_clip_top,
                    annotation_clip_bottom,
                    annotation_content_bottom,
                    self.session.panels.annotation_scroll_points,
                    0.0,
                    self.panel_rect(panel_id).width(),
                ),
                reads: pane_layout_snapshot(
                    &self.session.track_presentation,
                    frames,
                    ui::TrackPane::Reads,
                    read_clip_top,
                    scroll_clip_bottom,
                    read_pane_layout.content_bottom,
                    0.0,
                    read_stack_scroll,
                    self.panel_rect(panel_id).width(),
                ),
            }
        };
        self.session.panels.panel_mut(panel_id).pane_layout = pane_layout;
        let read_frames = self
            .session
            .panels
            .panel(panel_id)
            .track_frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Reads)
            .cloned()
            .collect::<Vec<_>>();
        let annotation_frames = self
            .session
            .panels
            .panel(panel_id)
            .track_frames
            .iter()
            .filter(|frame| frame.channel == ui::TrackPane::Annotation)
            .cloned()
            .collect::<Vec<_>>();
        for read_panel in &mut read_panel_displays {
            let density = self.alignment_density_for_panel_track(panel_id, read_panel.track_id);
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
                .session
                .panels
                .panel(panel_id)
                .read_panel_scroll_state(read_panel.track_id);
            let frame = read_frames
                .iter()
                .filter(|frame| frame.key == TrackFrameKey::Alignment(read_panel.track_id))
                .cloned()
                .collect::<Vec<_>>();
            read_panel.overview =
                resolve_read_panel_overview(read_panel.authoritative_overview.as_ref(), || {
                    ui::scrollbar_overview(
                        &frame,
                        &read_panel.scrollable,
                        read_panel.clip_top,
                        content_bottom,
                        read_panel.clip_top,
                        state.content_origin_points,
                        self.panel_rect(panel_id).width(),
                        128,
                    )
                });
            for bin in &mut read_panel.overview.bins {
                if bin.density > f32::EPSILON {
                    bin.kind = Some(TrackKind::Alignment);
                }
            }
            read_panel.overview.boundaries.clear();
        }
        let annotation_overview = ui::scrollbar_overview(
            &annotation_frames,
            &annotation_semantic_display,
            annotation_clip_top,
            annotation_content_bottom,
            annotation_clip_top,
            0.0,
            self.panel_rect(panel_id).width(),
            128,
        );
        self.session.panels.panel_mut(panel_id).scroll_display_cache =
            Some(PanelScrollDisplayCache {
                annotations: annotation_display,
                pinned: pinned_display,
                annotation_overview,
                annotation_content_bottom,
                annotation_clip_top,
                annotation_clip_bottom,
                supports_fast_scroll,
                read_panels: read_panel_displays,
            });
        let rebuilt = self.rebuild_panel_from_scroll_cache(panel_id);
        debug_assert!(rebuilt);
        measured_annotation_content_height
    }
}
