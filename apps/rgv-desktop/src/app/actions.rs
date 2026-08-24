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
                TrackUiAction::OpenSettings(key) => outcome.settings_panel = Some(key),
                TrackUiAction::ToggleVisibility(TrackFrameKey::Alignment(id)) => {
                    if let Some((_, _, visible, _, _)) = draft
                        .alignment_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *visible = !*visible;
                    } else if id == draft.primary_alignment_id {
                        *draft.primary_visible = !*draft.primary_visible;
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Auxiliary(id)) => {
                    if let Some((_, _, visible, _, _)) = draft
                        .auxiliary_controls
                        .iter_mut()
                        .find(|(candidate, _, _, _, _)| *candidate == id)
                    {
                        *visible = !*visible;
                    }
                }
                TrackUiAction::ToggleVisibility(TrackFrameKey::Reference) => {
                    *draft.show_reference = !*draft.show_reference;
                }
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
                    if let Some((_, _, _, collapsed, _)) = draft
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
                TrackUiAction::Remove(TrackFrameKey::Reference) => {}
                TrackUiAction::ToggleAlignmentSubtrack(track_id, subtrack) => outcome
                    .alignment_subtracks_to_toggle
                    .push((track_id, subtrack)),
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
                            self.panels.active().vertical_scroll_points,
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
                            self.panels.active().vertical_scroll_points,
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
                    if let Some((active, origin)) = self.resizing_track
                        && active == id
                    {
                        let requested = ui::resized_track_height(origin, delta);
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
                TrackUiAction::Select(key) => self.select_track_key(key),
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
