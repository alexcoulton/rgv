//! Post-pass commit: apply the frame's drafts and deferred requests to the application.
//!
//! `apply_shell_frame` fixes the order in which requests take effect; the helpers in this module
//! and in the `apply_*` modules are its consecutive steps.

use crate::app::actions::TrackActionDraft;
use crate::diagnostics::ManualDiagnosticFramePhase;
use crate::inspection::model::next_evidence_index;
use crate::inspection::read::truncate_string_to_bytes;
use crate::ui::frame::ShellFrame;
use crate::ui::{AlignmentSubtrack, TrackUiAction, UiCommand};
use crate::workspace::lists::normalize_sample_association;
use crate::{LOCUS_EDITOR_ID, RemoteSourceEditor, UiPanel, Viewer, ui};
use rgv_core::{PresentationNodeId, TrackId};
use std::time::{Duration, Instant};

/// Chooses the single open dialog from the dialogs that asked to be open this frame, listed in
/// display precedence.
///
/// A dialog that was not open before was requested by this frame's click or command, so it
/// replaces the one that was open: a track-settings gear clicked while Regions, Review, Metadata
/// or Settings is showing switches to the track settings instead of being dropped. With no new
/// request the dialog that is already open stays, and precedence only orders simultaneous
/// requests.
pub(crate) fn resolve_open_panel(
    previous: Option<UiPanel>,
    requested: &[Option<UiPanel>],
) -> Option<UiPanel> {
    let mut requested = requested.iter().copied().flatten();
    let first = requested.next()?;
    if Some(first) != previous {
        return Some(first);
    }
    Some(
        requested
            .find(|panel| Some(*panel) != previous)
            .unwrap_or(first),
    )
}

impl Viewer {
    /// Applies everything the egui pass requested, in a fixed order, and returns the repaint delay.
    pub(super) fn apply_shell_frame(
        &mut self,
        mut frame: ShellFrame,
        full_output: &egui::FullOutput,
    ) -> Duration {
        let st = &mut frame;
        let shell_post_process_started = Instant::now();
        let repaint_delay = self.commit_shell_chrome_state(st, full_output);
        self.commit_workspace_layout(st);
        self.apply_scrollbar_interactions(st);
        self.apply_palette_command(st);
        let (alignment_track_to_remove, alignment_subtracks_to_toggle) =
            self.apply_track_actions(st);
        self.apply_locus_search_and_panel_changes(st);
        self.apply_inspector_exports(st);
        self.apply_navigation_requests(st);
        self.apply_region_and_review_changes(st);
        let metadata_selection_changed = self.apply_metadata_selection(st);
        self.apply_workspace_list_transfers(st);
        self.commit_open_panel(st);
        self.apply_settings_changes(st);
        self.apply_file_requests(st);
        self.apply_cache_clear(st);
        self.commit_editors_and_selection(st);
        let editing_primary_settings =
            st.align.settings_alignment_id == st.align.primary_alignment_id;
        self.apply_primary_alignment_query_settings(st, editing_primary_settings);
        self.apply_reference_presentation(st);
        let requested_coverage_scale = st
            .align
            .use_fixed_coverage_scale
            .then_some(st.align.coverage_scale_maximum.max(1));
        self.apply_primary_alignment_presentation(
            st,
            editing_primary_settings,
            requested_coverage_scale,
        );
        self.apply_auxiliary_track_settings(st);
        let alignment_sort_anchor =
            self.apply_primary_alignment_style(st, editing_primary_settings);
        self.apply_primary_alignment_filters(st, editing_primary_settings);
        self.commit_secondary_alignment_settings(
            st,
            editing_primary_settings,
            requested_coverage_scale,
            alignment_sort_anchor,
        );
        self.apply_bulk_and_deferred_track_actions(
            st,
            alignment_subtracks_to_toggle,
            alignment_track_to_remove,
            metadata_selection_changed,
        );
        self.capture_status_notification();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellPostProcess,
            shell_post_process_started.elapsed(),
        );
        repaint_delay
    }

    /// Notification-log edits and the answer to the unsaved-changes prompt.
    fn commit_shell_dialogs(&mut self, st: &mut ShellFrame) {
        let draft = &mut st.commands.shell;
        let shell = &mut self.shell;
        shell.notifications_open = draft.notifications_open;
        if draft.clear_notifications {
            shell.notifications.clear();
        } else if let Some(sequence) = draft.dismiss_notification.take() {
            shell.notifications.dismiss(sequence);
        }
        if let Some(choice) = draft.unsaved_choice.take() {
            self.resolve_unsaved_choice(choice);
        }
    }

    /// Context menu, status announcement, diagnostics window and recovery prompt state.
    pub(super) fn commit_shell_chrome_state(
        &mut self,
        st: &mut ShellFrame,
        full_output: &egui::FullOutput,
    ) -> Duration {
        self.egui_context.data_mut(|data| {
            data.insert_temp(st.properties_expanded_id, st.properties_expanded);
        });
        if self.alignment_context_menu.is_some() && st.alignment_context_menu.is_none() {
            self.close_alignment_context_menu(if st.alignment_context_action.is_some() {
                "action_selected"
            } else {
                "dismissed"
            });
        }
        self.alignment_context_menu = st.alignment_context_menu.take();
        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |output| output.repaint_delay);
        self.announced_status.clone_from(&st.snapshot.data_status);
        self.genomic_canvas_layer_id = st.layout.genomic_canvas_layer_id;
        self.pane_divider_rect = st.layout.pane_divider_rect;
        self.command_search = std::mem::take(&mut st.commands.command_search);
        self.exit_confirmation_open = st.commands.exit_confirmation_open;
        self.user_exit_requested |= st.commands.user_exit_requested;
        self.diagnostics_open = st.commands.diagnostics_open;
        self.commit_shell_dialogs(st);
        match st.commands.manual_trace_action {
            Some(ui::ManualInteractionTraceAction::Start) => {
                self.start_manual_diagnostic_trace("Diagnostics window", true);
            }
            Some(ui::ManualInteractionTraceAction::StopAndSave) => {
                self.stop_and_save_manual_diagnostic_trace();
            }
            None => {}
        }
        self.recovery_open = st.commands.recovery_open
            && !st.commands.dismiss_recovery
            && st.commands.recovery_requested.is_none();
        repaint_delay
    }

    /// Adopts the layout computed during the pass and reacts to geometry changes.
    pub(super) fn commit_workspace_layout(&mut self, st: &mut ShellFrame) {
        let properties_resize_active =
            st.layout.native_properties_resize_active || self.automation_properties_resize_active;
        let properties_resize_ended =
            self.properties_resize_gesture_active && !properties_resize_active;
        self.properties_resize_gesture_active = properties_resize_active;
        if properties_resize_active {
            self.pending_resolution_query_refresh = None;
        }
        let workspace_layout_changed =
            self.workspace_layout.revision != st.layout.new_workspace_layout.revision;
        if workspace_layout_changed {
            self.workspace_layout_recomputations =
                self.workspace_layout_recomputations.saturating_add(1);
        }
        // The frame's copy is not read again, so trade it for the outgoing layout.
        std::mem::swap(
            &mut self.workspace_layout,
            &mut st.layout.new_workspace_layout,
        );
        // The panel's genomic interval and the workspace's pixel geometry are separate
        // authorities. Keep their horizontal transforms synchronized even for sub-point native
        // layout changes; presentation cameras use the exact live canvas width and must never see
        // the previous frame's width here.
        let panel_widths_changed = self.workspace_layout.panels.iter().any(|layout| {
            self.panels.get(layout.panel_id).is_some_and(|panel| {
                (panel.viewport.pixel_width() - f64::from(layout.canvas.width())).abs()
                    > f64::EPSILON
            })
        });
        if panel_widths_changed {
            self.synchronize_panel_widths();
        }
        if workspace_layout_changed {
            for panel in self.panels.iter_mut() {
                panel
                    .navigation_presentation
                    .invalidate_contract("workspace layout changed");
            }
        }
        if !st
            .layout
            .committed_canvas_geometry
            .approximately_equals(st.layout.new_canvas_geometry)
        {
            let width_changed = (st.layout.committed_canvas_geometry.genomic.width()
                - st.layout.new_canvas_geometry.genomic.width())
            .abs()
                >= 0.5;
            if width_changed {
                if !properties_resize_active {
                    self.schedule_resolution_query_refresh();
                }
            } else if self.synthetic {
                self.mark_display_dirty("explicit display invalidation");
            } else {
                self.rebuild_all_panel_displays();
                self.mark_display_dirty("explicit display invalidation");
            }
        }
        if properties_resize_ended {
            self.schedule_resolution_query_refresh();
        }
        if st.layout.overview_panel_id != self.panels.active().id {
            self.activate_view_panel(st.layout.overview_panel_id);
        }
        self.handle_overview_interaction(st.layout.overview_interaction);
        self.active_stack_scrollbar_gesture = None;
    }

    pub(super) fn apply_scrollbar_interactions(&mut self, st: &mut ShellFrame) {
        for workspace in &mut st.layout.workspace_scrollbars {
            if workspace.annotation_interaction.pointer_down
                || (workspace.annotation_offset - st.layout.shared_annotation_offset).abs()
                    > f32::EPSILON
            {
                if workspace.annotation_interaction.pointer_down {
                    self.active_stack_scrollbar_gesture =
                        Some((workspace.panel_id, ui::TrackPane::Annotation));
                }
                if !self.refresh_shared_annotation_scroll_display(workspace.annotation_offset) {
                    self.mark_display_dirty("annotation scrollbar fast refresh was unavailable");
                }
                self.window.request_redraw();
            }
            let current_read_stack_offset = self
                .panels
                .get(workspace.panel_id)
                .map_or(0.0, |panel| panel.read_stack_scroll_points);
            if workspace.read_stack_interaction.pointer_down
                || (workspace.read_stack_offset - current_read_stack_offset).abs() > f32::EPSILON
            {
                if workspace.read_stack_interaction.pointer_down {
                    self.active_stack_scrollbar_gesture =
                        Some((workspace.panel_id, ui::TrackPane::Reads));
                }
                self.activate_view_panel(workspace.panel_id);
                self.panels.active_mut().read_stack_scroll_points = workspace.read_stack_offset;
                self.rebuild_genomic_display();
                self.mark_display_dirty("read-track stack scrollbar changed");
                self.window.request_redraw();
            }
            for scrollbar in &mut workspace.read_panels {
                let current_offset = self
                    .panels
                    .get(workspace.panel_id)
                    .map(|panel| {
                        panel
                            .read_panel_scroll_state(scrollbar.track_id)
                            .scroll_points
                    })
                    .unwrap_or_default();
                if scrollbar.interaction.pointer_down
                    || scrollbar.interaction.keyboard_density_pixels.abs() > f32::EPSILON
                    || (scrollbar.offset - current_offset).abs() > f32::EPSILON
                {
                    self.activate_view_panel(workspace.panel_id);
                    self.apply_read_panel_overview_interaction(
                        scrollbar.track_id,
                        scrollbar.interaction,
                        &mut scrollbar.offset,
                    );
                }
            }
        }
    }

    /// Folds the edited alignment row back into its control, then routes a command-palette choice.
    pub(super) fn apply_palette_command(&mut self, st: &mut ShellFrame) {
        if let Some(id) = st.align.editing_alignment_id
            && let Some(control) = st
                .tracks
                .alignment_track_controls
                .iter_mut()
                .find(|control| control.id == id)
        {
            if !st.align.alignment_name.trim().is_empty() {
                control.name.clone_from(&st.align.alignment_name);
            }
            control.visible = st.align.alignment_visible;
            control.collapsed = st.align.alignment_collapsed;
            control.height = st.align.alignment_height;
        }
        if let Some(command) = st.commands.palette_command {
            match command {
                UiCommand::OpenAlignment => st.commands.open_alignment_requested = true,
                UiCommand::OpenUrl => st.commands.remote_source_open = true,
                UiCommand::AddTrack => st.commands.add_track_requested = true,
                UiCommand::OpenSession => st.commands.open_session_requested = true,
                UiCommand::SaveSession => st.commands.save_session_requested = true,
                UiCommand::ExportPng => st.commands.export_png_requested = true,
                UiCommand::ClearCache => st.commands.clear_cache_requested = true,
                UiCommand::Settings => st.commands.settings_open = true,
                UiCommand::FocusLocus => self
                    .egui_context
                    .memory_mut(|memory| memory.request_focus(egui::Id::new(LOCUS_EDITOR_ID))),
                UiCommand::OpenTrackSettings => {
                    if let Some(track) = st.layout.selected_track {
                        st.tracks.track_settings_panel = Some(track);
                    } else {
                        "Select a track before opening track settings"
                            .clone_into(&mut self.data_status);
                    }
                }
                UiCommand::ToggleTrackVisibility => {
                    if let Some(track) = st.layout.selected_track {
                        st.tracks
                            .track_ui_actions
                            .push(TrackUiAction::ToggleVisibility(track));
                    }
                }
                UiCommand::ToggleTrackCollapsed => {
                    if let Some(track) = st.layout.selected_track {
                        st.tracks
                            .track_ui_actions
                            .push(TrackUiAction::ToggleCollapsed(track));
                    }
                }
                UiCommand::PreviousEvidence => self.select_loaded_track_evidence(false),
                UiCommand::NextEvidence => self.select_loaded_track_evidence(true),
                UiCommand::PreviousPanel | UiCommand::NextPanel => {
                    if let Some(current) = st
                        .nav
                        .panel_controls
                        .iter()
                        .position(|(_, id, _)| *id == st.snapshot.active_panel_id)
                    {
                        let forward = command == UiCommand::NextPanel;
                        let target = next_evidence_index(
                            Some(current),
                            st.nav.panel_controls.len(),
                            forward,
                        )
                        .unwrap_or(current);
                        st.nav.panel_to_activate =
                            st.nav.panel_controls.get(target).map(|(_, id, _)| *id);
                    }
                }
                UiCommand::Metadata => st.lists.metadata_open = true,
                UiCommand::Regions => st.lists.regions_open = true,
                UiCommand::ReviewQueue => st.lists.review_open = true,
            }
        }
    }

    /// Dispatches queued track actions and commits alignment names, sample keys and primary-row
    /// state. Returns the removal and subtrack toggles that must wait until the end of the commit.
    #[allow(clippy::too_many_lines)]
    pub(super) fn apply_track_actions(
        &mut self,
        st: &mut ShellFrame,
    ) -> (Option<TrackId>, Vec<(TrackId, AlignmentSubtrack)>) {
        if st.align.show_reads != st.align.initial_show_reads {
            st.tracks
                .track_ui_actions
                .push(TrackUiAction::SetNodeVisibility(
                    PresentationNodeId::Track {
                        channel: ui::TrackPane::Reads,
                        track_id: st.align.settings_alignment_id,
                    },
                    st.align.show_reads,
                ));
        }
        if !st.align.bulk_alignment_ids.is_empty()
            && (st.align.bulk_alignment_visibility.is_some()
                || st.align.bulk_alignment_collapsed.is_some())
        {
            let visible = st
                .align
                .bulk_alignment_visibility
                .or_else(|| {
                    st.align
                        .bulk_alignment_collapsed
                        .map(|collapsed| !collapsed)
                })
                .expect("bulk presentation action has a requested state");
            let channel = st
                .align
                .bulk_alignment_pane
                .unwrap_or(ui::TrackPane::Annotation);
            st.tracks
                .track_ui_actions
                .extend(st.align.bulk_alignment_ids.iter().copied().map(|track_id| {
                    TrackUiAction::SetNodeVisibility(
                        PresentationNodeId::Track { channel, track_id },
                        visible,
                    )
                }));
        }
        self.append_pointer_release_resize_actions(
            self.egui_context
                .input(|input| input.pointer.primary_down()),
            &mut st.tracks.track_ui_actions,
        );
        let track_effects = self.dispatch_track_actions(
            std::mem::take(&mut st.tracks.track_ui_actions),
            TrackActionDraft {
                pane_layout: &st.layout.pane_layout,
                alignment_controls: &mut st.tracks.alignment_track_controls,
                auxiliary_controls: &mut st.tracks.track_controls,
                primary_alignment_id: st.align.primary_alignment_id,
                primary_visible: &mut st.align.alignment_visible,
                primary_collapsed: &mut st.align.alignment_collapsed,
                show_reference: &mut st.align.show_reference,
                settings_panel: st.tracks.track_settings_panel,
                metadata_selection: &mut st.lists.metadata_selected_tracks,
            },
        );
        st.tracks.track_settings_panel = track_effects.settings_panel;
        st.commands.export_png_requested |= track_effects.export_requested;
        let alignment_track_to_remove = track_effects.alignment_to_remove;
        let alignment_subtracks_to_toggle = track_effects.alignment_subtracks_to_toggle;
        let mut alignment_configuration_changed =
            track_effects.annotation_geometry_changed || track_effects.read_geometry_changed;
        for control in std::mem::take(&mut st.tracks.alignment_track_controls) {
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == control.id)
            {
                alignment_configuration_changed |= track.config.name != control.name;
                track.config.name = control.name;
            }
        }
        normalize_sample_association(&mut st.align.edited_track_sample_key);
        if let Some(id) = st.align.editing_alignment_id
            && let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == id)
            && track.config.sample_key != st.align.edited_track_sample_key
        {
            track
                .config
                .sample_key
                .clone_from(&st.align.edited_track_sample_key);
            alignment_configuration_changed = true;
        }
        if alignment_configuration_changed {
            self.invalidate_navigation_contract("alignment track layout changed");
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        if self.alignment_tracks.is_empty()
            && st.align.alignment_visible != self.primary_alignment_visible()
        {
            self.set_primary_alignment_visible(st.align.alignment_visible);
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        if self.alignment_tracks.is_empty()
            && st.align.alignment_collapsed != self.primary_alignment_collapsed()
        {
            self.set_primary_alignment_collapsed(st.align.alignment_collapsed);
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        if self.alignment_tracks.is_empty()
            && !st.align.alignment_name.trim().is_empty()
            && st.align.alignment_name != self.primary_alignment_name()
        {
            self.set_primary_alignment_name(std::mem::take(&mut st.align.alignment_name));
            self.rebuild_genomic_display();
            self.mark_display_dirty("explicit display invalidation");
        }
        (alignment_track_to_remove, alignment_subtracks_to_toggle)
    }

    /// Opens a requested remote source and records which single dialog is open.
    pub(super) fn commit_open_panel(&mut self, st: &mut ShellFrame) {
        self.remote_source_editor = std::mem::take(&mut st.commands.remote_source_editor);
        if let Some(request) = st.commands.remote_source_request.take() {
            match self.open_remote_source(request) {
                Ok(()) => self.remote_source_editor = RemoteSourceEditor::default(),
                Err(error) => {
                    self.remote_source_editor.error = Some(error.to_string());
                    st.commands.remote_source_open = true;
                }
            }
        }
        self.open_panel = resolve_open_panel(
            self.open_panel,
            &[
                st.commands
                    .command_palette_open
                    .then_some(UiPanel::CommandPalette),
                st.commands
                    .remote_source_open
                    .then_some(UiPanel::RemoteSource),
                st.commands.settings_open.then_some(UiPanel::Settings),
                st.commands
                    .project_issues_open
                    .then_some(UiPanel::ProjectIssues),
                st.commands
                    .igv_report_open
                    .then_some(UiPanel::IgvImportReport),
                st.lists.metadata_open.then_some(UiPanel::Metadata),
                st.lists.regions_open.then_some(UiPanel::Regions),
                st.lists.review_open.then_some(UiPanel::Review),
                st.tracks.track_settings_panel.map(UiPanel::TrackSettings),
            ],
        );
    }

    pub(super) fn commit_editors_and_selection(&mut self, st: &mut ShellFrame) {
        truncate_string_to_bytes(&mut st.align.read_name_editor, 1_024);
        self.read_name_editor.clone_from(&st.align.read_name_editor);
        truncate_string_to_bytes(&mut st.inspect.variant_sample_search, 256);
        self.variant_sample_search
            .clone_from(&st.inspect.variant_sample_search);
        truncate_string_to_bytes(&mut st.tracks.variant_settings_sample_search, 256);
        truncate_string_to_bytes(&mut st.tracks.variant_predicate_key, 256);
        truncate_string_to_bytes(&mut st.tracks.variant_predicate_value, 256);
        self.variant_settings_sample_search
            .clone_from(&st.tracks.variant_settings_sample_search);
        self.variant_predicate_scope = st.tracks.variant_predicate_scope;
        self.variant_predicate_key
            .clone_from(&st.tracks.variant_predicate_key);
        self.variant_predicate_operator = st.tracks.variant_predicate_operator;
        self.variant_predicate_value
            .clone_from(&st.tracks.variant_predicate_value);
        self.variant_predicate_missing = st.tracks.variant_predicate_missing;
        if st.align.select_read_name_requested {
            let name = st.align.read_name_editor.trim().as_bytes().to_vec();
            if self.select_read_or_template_by_name(st.align.settings_alignment_id, &name) {
                "Selected the exact read/template name from the complete view"
                    .clone_into(&mut self.data_status);
            } else {
                "No exact read/template name is loaded in this track at the current locus"
                    .clone_into(&mut self.data_status);
            }
        }
        if st.inspect.selected_to_top_requested && self.promote_selected_read_to_top() {
            self.rebuild_genomic_display();
        }
        if st.inspect.clear_selection {
            self.clear_promoted_read();
            self.panels.active_mut().clear_selection();
            self.mark_display_dirty("explicit display invalidation");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::TrackFrameKey;

    fn gear(id: u64) -> UiPanel {
        UiPanel::TrackSettings(TrackFrameKey::Auxiliary(TrackId::new(id)))
    }

    #[test]
    fn a_track_gear_click_switches_from_another_open_dialog() {
        for open in [
            UiPanel::Regions,
            UiPanel::Review,
            UiPanel::Metadata,
            UiPanel::Settings,
        ] {
            assert_eq!(
                resolve_open_panel(Some(open), &[Some(open), Some(gear(3))]),
                Some(gear(3)),
                "the gear click must not be dropped while {open:?} is open"
            );
        }
    }

    #[test]
    fn the_open_dialog_stays_until_closed_or_replaced() {
        assert_eq!(resolve_open_panel(None, &[None, None]), None);
        assert_eq!(
            resolve_open_panel(Some(UiPanel::Regions), &[None, Some(UiPanel::Regions)]),
            Some(UiPanel::Regions)
        );
        assert_eq!(
            resolve_open_panel(Some(UiPanel::Regions), &[None, None]),
            None,
            "closing the window closes the dialog"
        );
        assert_eq!(
            resolve_open_panel(Some(gear(1)), &[Some(gear(2))]),
            Some(gear(2)),
            "another track's gear retargets the settings window"
        );
        // A lower-precedence dialog requested while a higher one is open also switches.
        assert_eq!(
            resolve_open_panel(
                Some(UiPanel::Settings),
                &[Some(UiPanel::Settings), Some(UiPanel::Review)]
            ),
            Some(UiPanel::Review)
        );
        // Simultaneous new requests fall back to display precedence.
        assert_eq!(
            resolve_open_panel(None, &[Some(UiPanel::Metadata), Some(gear(1))]),
            Some(UiPanel::Metadata)
        );
    }
}
