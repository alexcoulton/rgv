use super::super::*;

pub(crate) type TrackControl = (TrackId, String, bool, bool, f32);

pub(crate) struct TrackActionDraft<'a> {
    pub track_frames: &'a [TrackFrame],
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
    pub alignment_move: Option<(TrackId, usize)>,
    pub auxiliary_move: Option<(TrackId, usize)>,
    pub alignment_subtracks_to_toggle: Vec<(TrackId, AlignmentSubtrack)>,
    pub annotation_geometry_changed: bool,
    pub read_geometry_changed: bool,
}

fn toggle_composite_members(controls: &mut [TrackControl], members: &BTreeSet<TrackId>) {
    let collapse = controls
        .iter()
        .any(|(id, _, visible, collapsed, _)| *visible && members.contains(id) && !*collapsed);
    for (id, _, visible, collapsed, _) in controls {
        if *visible && members.contains(id) {
            *collapsed = collapse;
        }
    }
}

fn toggle_track_visibility(visible: &mut bool, collapsed: &mut bool) {
    if *visible {
        *visible = false;
    } else {
        *visible = true;
        *collapsed = false;
    }
}

impl Viewer {
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
        for action in actions {
            match action {
                TrackUiAction::OpenSettings(TrackFrameKey::Group(_)) => {}
                TrackUiAction::OpenSettings(key) => outcome.settings_panel = Some(key),
                TrackUiAction::ToggleVisibility(TrackFrameKey::Alignment(id)) => {
                    if let Some((_, _, visible, collapsed, _)) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        toggle_track_visibility(visible, collapsed);
                    } else if id == draft.primary_alignment_id {
                        toggle_track_visibility(draft.primary_visible, draft.primary_collapsed);
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Auxiliary(id)) => {
                    if let Some((_, _, visible, collapsed, _)) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        toggle_track_visibility(visible, collapsed);
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Reference) => {
                    *draft.show_reference = !*draft.show_reference;
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Group(_)) => {}
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Alignment(id)) => {
                    if let Some((_, _, _, collapsed, _)) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *collapsed = !*collapsed;
                    } else if id == draft.primary_alignment_id {
                        *draft.primary_collapsed = !*draft.primary_collapsed;
                    }
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Auxiliary(id)) => {
                    let overlay_group = self
                        .tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == id)
                        .and_then(|track| track.quantitative.overlay_group);
                    if let Some(group) = overlay_group {
                        let member_ids = self
                            .tracks
                            .tracks()
                            .iter()
                            .filter(|track| {
                                track.visible
                                    && track.kind == TrackKind::Quantitative
                                    && track.quantitative.overlay_group == Some(group)
                            })
                            .map(|track| track.id)
                            .collect::<BTreeSet<_>>();
                        toggle_composite_members(draft.auxiliary_controls, &member_ids);
                    } else if let Some((_, _, _, collapsed, _)) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *collapsed = !*collapsed;
                    }
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Reference) => {
                    *draft.show_reference = !*draft.show_reference;
                }
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Group(group)) => {
                    if self.toggle_alignment_track_group(ui::TrackPane::Annotation, group) {
                        outcome.annotation_geometry_changed = true;
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::Remove(TrackFrameKey::Reference | TrackFrameKey::Group(_)) => {}
                TrackUiAction::ToggleAlignmentSubtrack(track_id, subtrack) => outcome
                    .alignment_subtracks_to_toggle
                    .push((track_id, subtrack)),
                TrackUiAction::ToggleCompositeMember(track_id) => {
                    if let Some((_, _, visible, collapsed, _)) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == track_id)
                    {
                        if *visible {
                            *collapsed = !*collapsed;
                        } else {
                            toggle_track_visibility(visible, collapsed);
                        }
                    }
                }
                TrackUiAction::GroupSelectedTracks(pane) => {
                    if self.group_selected_alignment_tracks(pane) {
                        outcome.annotation_geometry_changed = true;
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::ToggleTrackGroup(pane, group) => {
                    if self.toggle_alignment_track_group(pane, group) {
                        outcome.annotation_geometry_changed = true;
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::UngroupTracks(pane, group) => {
                    if self.ungroup_alignment_tracks(pane, group) {
                        outcome.annotation_geometry_changed = true;
                        outcome.read_geometry_changed = true;
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
                TrackUiAction::ReorderDropped(id, pointer_y) => {
                    if draft
                        .alignment_controls
                        .iter()
                        .any(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        let order = draft
                            .alignment_controls
                            .iter()
                            .map(|(track, _, _, _, _)| *track)
                            .collect::<Vec<_>>();
                        let target = ui::track_drop_index(
                            draft.track_frames,
                            &order,
                            pointer_y,
                            self.panels.active().annotation_scroll_points,
                            self.panels
                                .active()
                                .scroll_display_cache
                                .as_ref()
                                .map_or(self.canvas_geometry.viewport.top(), |cache| {
                                    cache.annotation_clip_top
                                }),
                        );
                        outcome.alignment_move = Some((id, target));
                    } else {
                        let order = draft
                            .auxiliary_controls
                            .iter()
                            .map(|(track, _, _, _, _)| *track)
                            .collect::<Vec<_>>();
                        let target = ui::track_drop_index(
                            draft.track_frames,
                            &order,
                            pointer_y,
                            self.panels.active().annotation_scroll_points,
                            self.panels
                                .active()
                                .scroll_display_cache
                                .as_ref()
                                .map_or(self.canvas_geometry.viewport.top(), |cache| {
                                    cache.annotation_clip_top
                                }),
                        );
                        outcome.auxiliary_move = Some((id, target));
                    }
                }
                TrackUiAction::ReorderKeyboard(id, direction) => {
                    if let Some(current) = draft
                        .alignment_controls
                        .iter()
                        .position(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        let target = ui::keyboard_reorder_index(
                            current,
                            draft.alignment_controls.len(),
                            direction,
                        );
                        if target != current {
                            outcome.alignment_move = Some((id, target));
                        }
                    } else if let Some(current) = draft
                        .auxiliary_controls
                        .iter()
                        .position(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        let target = ui::keyboard_reorder_index(
                            current,
                            draft.auxiliary_controls.len(),
                            direction,
                        );
                        if target != current {
                            outcome.auxiliary_move = Some((id, target));
                        }
                    }
                }
                TrackUiAction::ResizeStarted(id) => {
                    self.resizing_track = self
                        .alignment_tracks
                        .iter()
                        .find(|track| track.config.id == id)
                        .map(|track| (id, track.config.height))
                        .or_else(|| {
                            self.tracks
                                .tracks()
                                .iter()
                                .find(|track| track.id == id)
                                .map(|track| (id, track.height))
                        });
                }
                TrackUiAction::ResizeDragged(id, delta) => {
                    if let Some((active, current)) = self.resizing_track
                        && active == id
                    {
                        // `Response::drag_delta` is movement since the previous
                        // frame, so accumulate it onto the last accepted height.
                        let requested = ui::resized_track_height(current, delta);
                        self.resizing_track = Some((id, requested));
                        if let Some((_, _, _, _, height)) = draft
                            .alignment_controls
                            .iter_mut()
                            .find(|(candidate, _, _, _, _)| *candidate == id)
                        {
                            *height = requested;
                        } else if let Some((_, _, _, _, height)) = draft
                            .auxiliary_controls
                            .iter_mut()
                            .find(|(candidate, _, _, _, _)| *candidate == id)
                        {
                            *height = requested;
                        }
                    }
                }
                TrackUiAction::ResizeStopped(id) => {
                    if self.resizing_track.is_some_and(|(active, _)| active == id) {
                        self.resizing_track = None;
                    }
                }
                TrackUiAction::ResizeKeyboard(id, delta) => {
                    if let Some((_, _, _, _, height)) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *height = ui::resized_track_height(*height, delta);
                    } else if let Some((_, _, _, _, height)) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *height = ui::resized_track_height(*height, delta);
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
                        match key {
                            TrackFrameKey::Reference => {
                                self.reference_annotation_height = Some(requested);
                                outcome.annotation_geometry_changed = true;
                            }
                            TrackFrameKey::Alignment(id) => {
                                if let Some(track) = self
                                    .alignment_tracks
                                    .iter_mut()
                                    .find(|track| track.config.id == id)
                                {
                                    track.config.annotation_height = Some(requested);
                                    outcome.annotation_geometry_changed = true;
                                }
                            }
                            TrackFrameKey::Group(_) | TrackFrameKey::Auxiliary(_) => {}
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
                    match key {
                        TrackFrameKey::Reference => {
                            self.reference_annotation_height = Some(requested);
                            outcome.annotation_geometry_changed = true;
                        }
                        TrackFrameKey::Alignment(id) => {
                            if let Some(track) = self
                                .alignment_tracks
                                .iter_mut()
                                .find(|track| track.config.id == id)
                            {
                                track.config.annotation_height = Some(requested);
                                outcome.annotation_geometry_changed = true;
                            }
                        }
                        TrackFrameKey::Group(_) | TrackFrameKey::Auxiliary(_) => {}
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
                        if let Some(track) = self
                            .alignment_tracks
                            .iter_mut()
                            .find(|track| track.config.id == id)
                        {
                            track.config.read_group_height = Some(requested);
                            outcome.read_geometry_changed = true;
                        }
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
                    if let Some(track) = self
                        .alignment_tracks
                        .iter_mut()
                        .find(|track| track.config.id == id)
                    {
                        track.config.read_group_height = Some(requested);
                        outcome.read_geometry_changed = true;
                    }
                }
                TrackUiAction::ResizeReadPaneStarted => {
                    self.resizing_read_pane = draft
                        .alignment_controls
                        .iter()
                        .find(|(id, _, _, _, _)| *id == draft.primary_alignment_id)
                        .map(|(_, _, _, _, height)| *height);
                }
                TrackUiAction::ResizeReadPaneDragged(delta) => {
                    if let Some(current) = self.resizing_read_pane {
                        let requested = ui::resized_read_pane_height(
                            current,
                            delta,
                            self.canvas_geometry.viewport.height(),
                        );
                        self.resizing_read_pane = Some(requested);
                        if let Some((_, _, _, _, height)) = draft
                            .alignment_controls
                            .iter_mut()
                            .find(|(id, _, _, _, _)| *id == draft.primary_alignment_id)
                        {
                            *height = requested;
                        }
                    }
                }
                TrackUiAction::ResizeReadPaneStopped => {
                    self.resizing_read_pane = None;
                }
                TrackUiAction::ResizeReadPaneKeyboard(delta) => {
                    if let Some((_, _, _, _, height)) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|(id, _, _, _, _)| *id == draft.primary_alignment_id)
                    {
                        *height = ui::resized_read_pane_height(
                            *height,
                            delta,
                            self.canvas_geometry.viewport.height(),
                        );
                    }
                }
                TrackUiAction::Select(key, pane) => self.select_track_key(key, pane),
            }
        }
        outcome
    }

    pub(crate) fn toggle_alignment_subtrack(
        &mut self,
        track_id: TrackId,
        subtrack: AlignmentSubtrack,
    ) {
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
            AlignmentSubtrack::Reads => {
                filters.show_reads = !filters.show_reads;
                filters.show_reads
            }
        };

        if !visible && self.panels.active().selected_alignment_track == Some(track_id) {
            match subtrack {
                AlignmentSubtrack::Coverage => {
                    self.panels.active_mut().selected_pileup_position = None;
                }
                AlignmentSubtrack::Sashimi => {
                    self.panels.active_mut().selected_junction = None;
                }
                AlignmentSubtrack::Modifications => {
                    self.panels.active_mut().selected_modification_position = None;
                }
                AlignmentSubtrack::Reads => {
                    self.panels.active_mut().selected_record = None;
                    self.panels.active_mut().selected_read_event = None;
                    self.panels.active_mut().selected_chain = None;
                }
                AlignmentSubtrack::Reference => {}
            }
        }

        let query_changed = subtrack == AlignmentSubtrack::Modifications;
        self.invalidate_automation_alignment_layouts(Some(&[track_id]), query_changed);
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
    fn composite_eye_collapses_and_expands_every_visible_member_atomically() {
        let mut controls = vec![
            (TrackId::new(1), "one".to_owned(), true, false, 120.0),
            (TrackId::new(2), "two".to_owned(), true, true, 120.0),
            (TrackId::new(3), "other".to_owned(), true, false, 120.0),
        ];
        let members = BTreeSet::from([TrackId::new(1), TrackId::new(2)]);

        toggle_composite_members(&mut controls, &members);
        assert!(controls[0].3);
        assert!(controls[1].3);
        assert!(!controls[2].3);

        toggle_composite_members(&mut controls, &members);
        assert!(!controls[0].3);
        assert!(!controls[1].3);
        assert!(!controls[2].3);
    }

    #[test]
    fn restoring_track_visibility_also_expands_its_content() {
        let mut visible = false;
        let mut collapsed = true;

        toggle_track_visibility(&mut visible, &mut collapsed);

        assert!(visible);
        assert!(!collapsed);
    }
}
