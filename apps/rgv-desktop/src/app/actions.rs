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
                TrackUiAction::ToggleCollapsed(TrackFrameKey::Reference)
                | TrackUiAction::Remove(TrackFrameKey::Reference) => {}
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
}
