use super::super::*;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrackControl {
    pub(crate) id: TrackId,
    pub(crate) name: String,
    pub(crate) visible: bool,
    pub(crate) collapsed: bool,
    pub(crate) height: f32,
    pub(crate) initial_visible: bool,
    pub(crate) initial_height: f32,
}

pub(crate) struct TrackActionDraft<'a> {
    pub pane_layout: &'a ui::WorkspacePaneLayout,
    pub alignment_controls: &'a mut [TrackControl],
    pub auxiliary_controls: &'a mut [TrackControl],
    pub primary_alignment_id: TrackId,
    pub primary_visible: &'a mut bool,
    pub primary_collapsed: &'a mut bool,
    pub show_reference: &'a mut bool,
    pub settings_panel: Option<TrackFrameKey>,
    pub metadata_selection: &'a mut BTreeSet<TrackId>,
}

#[derive(Default)]
pub(crate) struct TrackActionOutcome {
    pub settings_panel: Option<TrackFrameKey>,
    pub export_requested: bool,
    pub alignment_to_remove: Option<TrackId>,
    pub alignment_subtracks_to_toggle: Vec<(TrackId, AlignmentSubtrack)>,
    pub annotation_geometry_changed: bool,
    pub read_geometry_changed: bool,
}

fn toggle_track_visibility(visible: &mut bool, collapsed: &mut bool) {
    if *visible {
        *visible = false;
    } else {
        *visible = true;
        *collapsed = false;
    }
}

fn intent_channel(intent: &TrackIntent) -> ui::TrackPane {
    match intent {
        TrackIntent::SetSectionExpanded { channel, .. }
        | TrackIntent::SetPaneRowsExpanded { channel, .. }
        | TrackIntent::ResetPaneRowHeights { channel } => *channel,
        TrackIntent::SetNodeVisibility { target, .. }
        | TrackIntent::SetGroupExpanded { target, .. }
        | TrackIntent::SetGroupVisibility { target, .. }
        | TrackIntent::ResizeNode { target, .. }
        | TrackIntent::Ungroup { target }
        | TrackIntent::MoveNode { target, .. } => target.channel(),
        TrackIntent::SetCompositeVisibility { channel, .. } => *channel,
        TrackIntent::ResizeReadPane { .. } => ui::TrackPane::Reads,
        TrackIntent::GroupNodes { channel, .. } => *channel,
    }
}

fn presentation_drop_target(
    presentation: &WorkspacePresentation,
    layout: &ui::PaneLayoutSnapshot,
    moving: PresentationNodeId,
    pointer_y: f32,
) -> Option<PresentationNodeId> {
    let roots = presentation.pane_root_nodes(moving.channel());
    let candidates = roots
        .iter()
        .copied()
        .filter(|node| *node != moving)
        .collect::<Vec<_>>();
    candidates.iter().copied().find(|node| {
        layout
            .items
            .iter()
            .find(|item| item.id == *node)
            .is_some_and(|item| pointer_y < item.row_rect.center().y)
    })
}

/// `None` means no movement; `Some(None)` means move to the end.
fn presentation_keyboard_move(
    presentation: &WorkspacePresentation,
    moving: PresentationNodeId,
    direction: i32,
) -> Option<Option<PresentationNodeId>> {
    let roots = presentation.pane_root_nodes(moving.channel());
    let current = roots.iter().position(|node| *node == moving)?;
    if direction < 0 {
        (current > 0).then(|| Some(roots[current - 1]))
    } else if direction > 0 && current + 1 < roots.len() {
        let before = roots.get(current + 2).copied();
        Some(before)
    } else {
        None
    }
}

fn resize_stop_actions_on_pointer_release(
    pointer_down: bool,
    annotation: Option<TrackFrameKey>,
    read_track: Option<TrackId>,
    read_pane: bool,
) -> Vec<TrackUiAction> {
    if pointer_down {
        return Vec::new();
    }
    annotation
        .map(TrackUiAction::ResizeAnnotationStopped)
        .into_iter()
        .chain(read_track.map(TrackUiAction::ResizeReadTrackStopped))
        .chain(read_pane.then_some(TrackUiAction::ResizeReadPaneStopped))
        .collect()
}

fn read_pane_resize_origin(
    configured_height: f32,
    viewport_top: f32,
    viewport_bottom: f32,
    rendered_divider_y: f32,
) -> f32 {
    let viewport_height = (viewport_bottom - viewport_top).max(1.0);
    let maximum = (viewport_height - 80.0).max(100.0);
    if rendered_divider_y.is_finite()
        && rendered_divider_y >= viewport_top
        && rendered_divider_y <= viewport_bottom
    {
        (viewport_bottom - rendered_divider_y).clamp(100.0, maximum)
    } else {
        configured_height.clamp(100.0, maximum)
    }
}

impl Viewer {
    pub(crate) fn append_pointer_release_resize_actions(
        &self,
        pointer_down: bool,
        actions: &mut Vec<TrackUiAction>,
    ) {
        for action in resize_stop_actions_on_pointer_release(
            pointer_down,
            self.resizing_annotation_track.map(|(key, _)| key),
            self.resizing_read_track.map(|(track_id, _)| track_id),
            self.resizing_read_pane.is_some(),
        ) {
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
    }

    #[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
    pub(crate) fn dispatch_track_actions(
        &mut self,
        actions: Vec<TrackUiAction>,
        draft: TrackActionDraft<'_>,
    ) -> TrackActionOutcome {
        let mut outcome = TrackActionOutcome {
            settings_panel: draft.settings_panel,
            ..TrackActionOutcome::default()
        };
        let mut presentation_intents = Vec::new();
        for action in actions {
            match action {
                TrackUiAction::ToggleSection(channel) => {
                    presentation_intents.push(TrackIntent::SetSectionExpanded {
                        channel,
                        expanded: !self.track_presentation.section_expanded(channel),
                    });
                    match channel {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::SetPaneRowsExpanded(channel, expanded) => {
                    presentation_intents
                        .push(TrackIntent::SetPaneRowsExpanded { channel, expanded });
                    match channel {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::ResetPaneRowHeights(channel) => {
                    presentation_intents.push(TrackIntent::ResetPaneRowHeights { channel });
                    match channel {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::OpenSettings(TrackFrameKey::Group { .. }) => {}
                TrackUiAction::OpenSettings(key) => outcome.settings_panel = Some(key),
                TrackUiAction::RenameTrack(key, name) => {
                    let name = name.trim();
                    if name.is_empty() {
                        continue;
                    }
                    match key {
                        TrackFrameKey::Alignment(id) => {
                            if let Some(control) = draft
                                .alignment_controls
                                .iter_mut()
                                .find(|control| control.id == id)
                            {
                                name.clone_into(&mut control.name);
                            } else if id == draft.primary_alignment_id {
                                self.set_primary_alignment_name(name.to_owned());
                                outcome.annotation_geometry_changed = true;
                                outcome.read_geometry_changed = true;
                            }
                        }
                        TrackFrameKey::Auxiliary(id) => {
                            if let Some(control) = draft
                                .auxiliary_controls
                                .iter_mut()
                                .find(|control| control.id == id)
                            {
                                name.clone_into(&mut control.name);
                            }
                        }
                        TrackFrameKey::Reference | TrackFrameKey::Group { .. } => {}
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Alignment(id)) => {
                    if let Some(control) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|control| control.id == id)
                    {
                        toggle_track_visibility(&mut control.visible, &mut control.collapsed);
                    } else if id == draft.primary_alignment_id {
                        toggle_track_visibility(draft.primary_visible, draft.primary_collapsed);
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Auxiliary(id)) => {
                    if let Some(control) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|control| control.id == id)
                    {
                        toggle_track_visibility(&mut control.visible, &mut control.collapsed);
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Reference) => {
                    *draft.show_reference = !*draft.show_reference;
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Group { .. }) => {}
                TrackUiAction::SetNodeVisibility(target, visible) => {
                    presentation_intents.push(TrackIntent::SetNodeVisibility { target, visible });
                    match target {
                        PresentationNodeId::Reference => *draft.show_reference = visible,
                        PresentationNodeId::Track {
                            channel: ui::TrackPane::Annotation,
                            track_id,
                        } => {
                            if let Some(control) = draft
                                .alignment_controls
                                .iter_mut()
                                .chain(draft.auxiliary_controls.iter_mut())
                                .find(|control| control.id == track_id)
                            {
                                control.visible = true;
                                control.collapsed = !visible;
                            }
                        }
                        PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            ..
                        }
                        | PresentationNodeId::Group { .. } => {}
                    }
                    match target.channel() {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Alignment(id)) => {
                    if let Some(control) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|control| control.id == id)
                    {
                        control.collapsed = !control.collapsed;
                    } else if id == draft.primary_alignment_id {
                        *draft.primary_collapsed = !*draft.primary_collapsed;
                    }
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Auxiliary(id)) => {
                    if let Some(control) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|control| control.id == id)
                    {
                        control.collapsed = !control.collapsed;
                    }
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Reference) => {
                    *draft.show_reference = !*draft.show_reference;
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Group { channel, id }) => {
                    if let Some(entry) = self
                        .track_presentation
                        .groups
                        .iter()
                        .find(|entry| entry.channel == channel && entry.id == id)
                    {
                        presentation_intents.push(TrackIntent::SetGroupExpanded {
                            target: PresentationNodeId::Group {
                                channel,
                                group_id: id,
                            },
                            expanded: !entry.expanded,
                        });
                    }
                }
                TrackUiAction::ToggleCompositeVisibility(TrackFrameKey::Auxiliary(id)) => {
                    let composite_id = self
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == id)
                        .and_then(|track| track.quantitative.overlay_group);
                    if let Some(composite_id) = composite_id {
                        let visible = !self
                            .track_presentation
                            .composite_content_visible(ui::TrackPane::Annotation, composite_id);
                        presentation_intents.push(TrackIntent::SetCompositeVisibility {
                            channel: ui::TrackPane::Annotation,
                            composite_id,
                            visible,
                        });
                        outcome.annotation_geometry_changed = true;
                    }
                }
                TrackUiAction::ToggleCompositeVisibility(_) => {}
                TrackUiAction::Remove(TrackFrameKey::Reference | TrackFrameKey::Group { .. }) => {}
                TrackUiAction::ToggleAlignmentSubtrack(track_id, subtrack) => outcome
                    .alignment_subtracks_to_toggle
                    .push((track_id, subtrack)),
                TrackUiAction::ToggleCompositeMember(track_id) => {
                    if let Some(control) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|control| control.id == track_id)
                    {
                        if control.visible {
                            control.collapsed = !control.collapsed;
                        } else {
                            toggle_track_visibility(&mut control.visible, &mut control.collapsed);
                        }
                    }
                }
                TrackUiAction::GroupSelectedTracks(pane) => {
                    let selected = self
                        .panels
                        .active()
                        .selected_presentation_nodes
                        .iter()
                        .filter_map(|node| match node {
                            PresentationNodeId::Track { channel, track_id } if *channel == pane => {
                                Some(*track_id)
                            }
                            PresentationNodeId::Reference
                            | PresentationNodeId::Track { .. }
                            | PresentationNodeId::Group { .. } => None,
                        })
                        .collect::<Vec<_>>();
                    if selected.len() > 1 {
                        presentation_intents.push(TrackIntent::GroupNodes {
                            channel: pane,
                            members: selected,
                        });
                        match pane {
                            ui::TrackPane::Annotation => {
                                outcome.annotation_geometry_changed = true;
                            }
                            ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                        }
                    }
                }
                TrackUiAction::ToggleTrackGroup(pane, group) => {
                    if let Some(entry) = self
                        .track_presentation
                        .groups
                        .iter()
                        .find(|entry| entry.channel == pane && entry.id == group)
                    {
                        let expanded = !entry.expanded;
                        presentation_intents.push(TrackIntent::SetGroupExpanded {
                            target: PresentationNodeId::Group {
                                channel: pane,
                                group_id: group,
                            },
                            expanded,
                        });
                        match pane {
                            ui::TrackPane::Annotation => {
                                outcome.annotation_geometry_changed = true;
                            }
                            ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                        }
                    }
                }
                TrackUiAction::ToggleTrackGroupVisibility(pane, group) => {
                    if let Some(entry) = self
                        .track_presentation
                        .groups
                        .iter()
                        .find(|entry| entry.channel == pane && entry.id == group)
                    {
                        let visible = !entry.content_visible;
                        presentation_intents.push(TrackIntent::SetGroupVisibility {
                            target: PresentationNodeId::Group {
                                channel: pane,
                                group_id: group,
                            },
                            visible,
                        });
                        match pane {
                            ui::TrackPane::Annotation => {
                                outcome.annotation_geometry_changed = true;
                            }
                            ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                        }
                    }
                }
                TrackUiAction::UngroupTracks(pane, group) => {
                    presentation_intents.push(TrackIntent::Ungroup {
                        target: PresentationNodeId::Group {
                            channel: pane,
                            group_id: group,
                        },
                    });
                    match pane {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::Export(_) => outcome.export_requested = true,
                TrackUiAction::Remove(TrackFrameKey::Auxiliary(id)) => {
                    match self.remove_auxiliary_tracks(&[id]) {
                        Ok(removed) if removed > 0 => {
                            draft.metadata_selection.remove(&id);
                            if outcome.settings_panel == Some(TrackFrameKey::Auxiliary(id)) {
                                outcome.settings_panel = None;
                            }
                            "Removed auxiliary track".clone_into(&mut self.data_status);
                        }
                        Ok(_) => {}
                        Err(error) => self.data_status = error.to_string(),
                    }
                }
                TrackUiAction::Remove(TrackFrameKey::Alignment(id)) => {
                    outcome.alignment_to_remove = Some(id);
                }
                TrackUiAction::ReorderDropped(target, pointer_y) => {
                    let before = presentation_drop_target(
                        &self.track_presentation,
                        draft.pane_layout.for_channel(target.channel()),
                        target,
                        pointer_y,
                    );
                    presentation_intents.push(TrackIntent::MoveNode { target, before });
                    match target.channel() {
                        ui::TrackPane::Annotation => outcome.annotation_geometry_changed = true,
                        ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                    }
                }
                TrackUiAction::ReorderKeyboard(target, direction) => {
                    if let Some(before) =
                        presentation_keyboard_move(&self.track_presentation, target, direction)
                    {
                        presentation_intents.push(TrackIntent::MoveNode { target, before });
                        match target.channel() {
                            ui::TrackPane::Annotation => {
                                outcome.annotation_geometry_changed = true;
                            }
                            ui::TrackPane::Reads => outcome.read_geometry_changed = true,
                        }
                    }
                }
                TrackUiAction::ResizeAnnotationStarted(key, height) => {
                    self.resizing_annotation_track = Some((key, height));
                }
                TrackUiAction::ResizeAnnotationDragged(key, delta) => {
                    if let Some((active, current)) = self.resizing_annotation_track
                        && active == key
                    {
                        let requested = ui::resized_annotation_track_height(current, delta);
                        self.resizing_annotation_track = Some((key, requested));
                        let target = match key {
                            TrackFrameKey::Reference => Some(PresentationNodeId::Reference),
                            TrackFrameKey::Alignment(track_id)
                            | TrackFrameKey::Auxiliary(track_id) => {
                                Some(PresentationNodeId::Track {
                                    channel: ui::TrackPane::Annotation,
                                    track_id,
                                })
                            }
                            TrackFrameKey::Group { .. } => None,
                        };
                        if let Some(target) = target {
                            presentation_intents.push(TrackIntent::ResizeNode {
                                target,
                                preferred_height: requested,
                            });
                            outcome.annotation_geometry_changed = true;
                        }
                    }
                }
                TrackUiAction::ResizeAnnotationStopped(key) => {
                    if self
                        .resizing_annotation_track
                        .is_some_and(|(active, _)| active == key)
                    {
                        self.resizing_annotation_track = None;
                    }
                }
                TrackUiAction::ResizeAnnotationKeyboard(key, current, delta) => {
                    let requested = ui::resized_annotation_track_height(current, delta);
                    let target = match key {
                        TrackFrameKey::Reference => Some(PresentationNodeId::Reference),
                        TrackFrameKey::Alignment(track_id) | TrackFrameKey::Auxiliary(track_id) => {
                            Some(PresentationNodeId::Track {
                                channel: ui::TrackPane::Annotation,
                                track_id,
                            })
                        }
                        TrackFrameKey::Group { .. } => None,
                    };
                    if let Some(target) = target {
                        presentation_intents.push(TrackIntent::ResizeNode {
                            target,
                            preferred_height: requested,
                        });
                        outcome.annotation_geometry_changed = true;
                    }
                }
                TrackUiAction::ResizeReadTrackStarted(id, height) => {
                    self.resizing_read_track = Some((id, height));
                }
                TrackUiAction::ResizeReadTrackDragged(id, delta) => {
                    if let Some((active, current)) = self.resizing_read_track
                        && active == id
                    {
                        let requested = ui::resized_read_track_height(current, delta);
                        self.resizing_read_track = Some((id, requested));
                        presentation_intents.push(TrackIntent::ResizeNode {
                            target: PresentationNodeId::Track {
                                channel: ui::TrackPane::Reads,
                                track_id: id,
                            },
                            preferred_height: requested,
                        });
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::ResizeReadTrackStopped(id) => {
                    if self
                        .resizing_read_track
                        .is_some_and(|(active, _)| active == id)
                    {
                        self.resizing_read_track = None;
                    }
                }
                TrackUiAction::ResizeReadTrackKeyboard(id, current, delta) => {
                    let requested = ui::resized_read_track_height(current, delta);
                    presentation_intents.push(TrackIntent::ResizeNode {
                        target: PresentationNodeId::Track {
                            channel: ui::TrackPane::Reads,
                            track_id: id,
                        },
                        preferred_height: requested,
                    });
                    outcome.read_geometry_changed = true;
                }
                TrackUiAction::ResizeReadPaneStarted => {
                    let viewport = self.canvas_geometry().viewport;
                    self.resizing_read_pane = Some(read_pane_resize_origin(
                        self.track_presentation.read_pane_height,
                        viewport.top(),
                        viewport.bottom(),
                        draft.pane_layout.annotation.pane_rect.bottom(),
                    ));
                }
                TrackUiAction::ResizeReadPaneDragged(delta) => {
                    if let Some(current) = self.resizing_read_pane {
                        let requested = ui::resized_read_pane_height(
                            current,
                            delta,
                            self.canvas_geometry().viewport.height(),
                        );
                        self.resizing_read_pane = Some(requested);
                        presentation_intents
                            .push(TrackIntent::ResizeReadPane { height: requested });
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::ResizeReadPaneStopped => {
                    self.resizing_read_pane = None;
                }
                TrackUiAction::ResizeReadPaneKeyboard(delta) => {
                    let viewport = self.canvas_geometry().viewport;
                    let origin = read_pane_resize_origin(
                        self.track_presentation.read_pane_height,
                        viewport.top(),
                        viewport.bottom(),
                        draft.pane_layout.annotation.pane_rect.bottom(),
                    );
                    let height = ui::resized_read_pane_height(origin, delta, viewport.height());
                    presentation_intents.push(TrackIntent::ResizeReadPane { height });
                    outcome.read_geometry_changed = true;
                }
                TrackUiAction::Select(key, pane) => self.select_track_key(key, pane),
            }
        }
        for control in draft
            .alignment_controls
            .iter()
            .chain(draft.auxiliary_controls.iter())
        {
            let desired_visible = control.visible && !control.collapsed;
            if desired_visible != control.initial_visible {
                presentation_intents.push(TrackIntent::SetNodeVisibility {
                    target: PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id: control.id,
                    },
                    visible: desired_visible,
                });
                outcome.annotation_geometry_changed = true;
            }
            if (control.height - control.initial_height).abs() > f32::EPSILON {
                presentation_intents.push(TrackIntent::ResizeNode {
                    target: PresentationNodeId::Track {
                        channel: ui::TrackPane::Annotation,
                        track_id: control.id,
                    },
                    preferred_height: control.height,
                });
                outcome.annotation_geometry_changed = true;
            }
        }
        if !presentation_intents.is_empty() {
            match self.apply_track_intents(&presentation_intents) {
                Ok(changes) => {
                    outcome.annotation_geometry_changed |= changes.layout_changed
                        && presentation_intents
                            .iter()
                            .any(|intent| intent_channel(intent) == ui::TrackPane::Annotation);
                    outcome.read_geometry_changed |= changes.layout_changed
                        && presentation_intents
                            .iter()
                            .any(|intent| intent_channel(intent) == ui::TrackPane::Reads);
                }
                Err(error) => self.data_status = error.to_string(),
            }
        }
        outcome
    }

    pub(crate) fn toggle_alignment_subtrack(
        &mut self,
        track_id: TrackId,
        subtrack: AlignmentSubtrack,
    ) {
        if subtrack == AlignmentSubtrack::Reads {
            let visible = !self
                .track_presentation
                .pane_track_state(ui::TrackPane::Reads, track_id)
                .is_none_or(|node| node.content_visible);
            if let Err(error) = self.apply_track_intents(&[TrackIntent::SetNodeVisibility {
                target: PresentationNodeId::Track {
                    channel: ui::TrackPane::Reads,
                    track_id,
                },
                visible,
            }]) {
                self.data_status = error.to_string();
                return;
            }
            self.invalidate_navigation_contract("read track visibility changed");
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
            return;
        }
        let primary_id = self.primary_alignment_track_id();
        let filters = if track_id == primary_id {
            Some(self.primary_alignment_filters_mut())
        } else {
            self.alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == track_id)
                .map(|track| &mut track.config.filters)
        };
        let Some(filters) = filters else {
            return;
        };
        let visible = match subtrack {
            AlignmentSubtrack::Reference => {
                filters.show_reference = !filters.show_reference;
                filters.show_reference
            }
            AlignmentSubtrack::Coverage => {
                filters.show_coverage = !filters.show_coverage;
                filters.show_coverage
            }
            AlignmentSubtrack::Sashimi => {
                filters.show_junctions = !filters.show_junctions;
                filters.show_junctions
            }
            AlignmentSubtrack::Modifications => {
                filters.show_modification_frequency = !filters.show_modification_frequency;
                filters.show_modification_frequency
            }
            AlignmentSubtrack::Reads => unreachable!("reads use pane presentation state"),
        };

        if !visible && self.panels.active().selected_alignment_track == Some(track_id) {
            match subtrack {
                AlignmentSubtrack::Coverage => {
                    self.panels.active_mut().selected_pileup_position = None;
                }
                AlignmentSubtrack::Sashimi => {
                    self.panels.active_mut().selected_junction = None;
                    self.panels.active_mut().selected_junction_component = None;
                }
                AlignmentSubtrack::Modifications => {
                    self.panels.active_mut().selected_modification_position = None;
                }
                AlignmentSubtrack::Reads => {
                    self.panels.active_mut().pending_read_selection = None;
                    self.panels.active_mut().pending_alignment_reveal = None;
                    self.panels.active_mut().selected_chain = None;
                }
                AlignmentSubtrack::Reference => {}
            }
        }

        let query_changed = subtrack == AlignmentSubtrack::Modifications;
        self.invalidate_automation_alignment_layouts(Some(&[track_id]), query_changed);
        self.invalidate_navigation_contract("alignment subtrack presentation changed");
        if query_changed {
            self.request_all_alignment_queries();
        }
        self.rebuild_all_panel_displays();
        self.display_dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_track_visibility_also_expands_its_content() {
        let mut visible = false;
        let mut collapsed = true;

        toggle_track_visibility(&mut visible, &mut collapsed);

        assert!(visible);
        assert!(!collapsed);
    }

    #[test]
    fn pointer_release_stops_resize_gestures_even_when_their_rows_are_not_painted() {
        let annotation = TrackFrameKey::Auxiliary(TrackId::new(7));
        assert_eq!(
            resize_stop_actions_on_pointer_release(
                false,
                Some(annotation),
                Some(TrackId::new(8)),
                true,
            ),
            vec![
                TrackUiAction::ResizeAnnotationStopped(annotation),
                TrackUiAction::ResizeReadTrackStopped(TrackId::new(8)),
                TrackUiAction::ResizeReadPaneStopped,
            ]
        );
        assert!(
            resize_stop_actions_on_pointer_release(true, Some(annotation), None, false).is_empty()
        );
    }

    #[test]
    fn pane_resize_starts_from_the_rendered_auto_fit_divider() {
        assert_eq!(read_pane_resize_origin(360.0, 100.0, 900.0, 452.0), 448.0);
        assert_eq!(
            read_pane_resize_origin(360.0, 100.0, 900.0, f32::NAN),
            360.0
        );
    }
}
