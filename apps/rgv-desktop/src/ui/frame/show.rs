//! The egui pass for one shell frame, in fixed widget order.
//!
//! egui is immediate-mode: the order of the calls below is the order widgets are laid out and
//! receive ids, so it must not be rearranged.

use super::*;

impl Viewer {
    /// Runs every shell widget for this frame.
    pub(super) fn show_shell_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        self.show_application_menu(ui, st);
        Self::show_status_bar(ui, st);
        self.show_navigation_toolbar(ui, st);
        Self::show_annotation_search_results(ui, st);
        let displayed_properties_expanded = st.properties_expanded;
        let properties_rect = self.show_properties_panel(ui, st, displayed_properties_expanded);
        st.layout.native_properties_resize_active = displayed_properties_expanded
            && ui
                .ctx()
                .read_response(egui::Id::new("properties-panel").with("__resize"))
                .is_some_and(|response| response.dragged());
        self.show_workspace_canvas(ui, st, properties_rect, displayed_properties_expanded);
        self.show_track_settings_window(ui, st);
        Self::show_workspace_dialogs(ui, st);
        st.commands.manual_trace_action = ui::show_diagnostics(
            ui.ctx(),
            &mut st.commands.diagnostics_open,
            &st.snapshot.diagnostics,
            &st.snapshot.manual_trace,
        );
        self.show_system_dialogs(ui, st);
        super::shell_dialogs::show_shell_dialogs(ui, &mut st.commands.shell);
        show_alignment_context_menu(
            ui,
            &mut st.alignment_context_menu,
            st.alignment_context_menu_view.as_ref(),
            &mut st.alignment_context_action,
        );
    }

    /// Round-trips the workspace list state through the metadata, regions and review dialogs.
    #[allow(clippy::too_many_lines)]
    pub(super) fn show_workspace_dialogs(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let mut workspace_dialogs = ui::workspace_dialogs::WorkspaceDialogDraft {
            metadata_open: st.lists.metadata_open,
            regions_open: st.lists.regions_open,
            review_open: st.lists.review_open,
            regions: std::mem::take(&mut st.lists.regions),
            review_items: std::mem::take(&mut st.lists.review_items),
            review_status_filter: st.lists.review_status_filter,
            add_current_region: st.lists.add_current_region,
            add_current_review: st.lists.add_current_review,
            region_import_requested: st.lists.region_import_requested,
            region_export_requested: st.lists.region_export_requested,
            review_import_requested: st.lists.review_import_requested,
            review_export_requested: st.lists.review_export_requested,
            previous_review_requested: st.lists.previous_review_requested,
            next_review_requested: st.lists.next_review_requested,
            managed_locus_requested: st.lists.managed_locus_requested.clone(),
            region_to_delete: st.lists.region_to_delete,
            review_to_delete: st.lists.review_to_delete,
            region_to_relocate: st.lists.region_to_relocate,
            review_to_relocate: st.lists.review_to_relocate,
            sample_metadata: st.lists.sample_metadata.take(),
            metadata_filter_column: st.lists.metadata_filter_column,
            metadata_filter_value: st.lists.metadata_filter_value.clone(),
            metadata_selected_tracks: std::mem::take(&mut st.lists.metadata_selected_tracks),
            metadata_bulk_height: st.lists.metadata_bulk_height,
            metadata_remove_armed: st.lists.metadata_remove_armed,
            metadata_import_requested: st.lists.metadata_import_requested,
            metadata_remove_requested: st.lists.metadata_remove_requested,
            metadata_select_requested: st.lists.metadata_select_requested,
            metadata_bulk_visibility: st.lists.metadata_bulk_visibility,
            metadata_bulk_collapsed: st.lists.metadata_bulk_collapsed,
            metadata_bulk_height_requested: st.lists.metadata_bulk_height_requested,
            metadata_bulk_rename_requested: st.lists.metadata_bulk_rename_requested,
            metadata_bulk_alignment_settings_requested: st
                .lists
                .metadata_bulk_alignment_settings_requested,
            metadata_bulk_auxiliary_appearance_requested: st
                .lists
                .metadata_bulk_auxiliary_appearance_requested,
            metadata_bulk_quantitative_autoscale_requested: st
                .lists
                .metadata_bulk_quantitative_autoscale_requested,
            metadata_bulk_quantitative_overlay_requested: st
                .lists
                .metadata_bulk_quantitative_overlay_requested,
            metadata_bulk_quantitative_separate_requested: st
                .lists
                .metadata_bulk_quantitative_separate_requested,
            metadata_bulk_remove_requested: st.lists.metadata_bulk_remove_requested,
            metadata_track_rows: std::mem::take(&mut st.lists.metadata_track_rows),
        };
        ui::workspace_dialogs::show_workspace_dialogs(ui, &mut workspace_dialogs);
        st.lists.metadata_open = workspace_dialogs.metadata_open;
        st.lists.regions_open = workspace_dialogs.regions_open;
        st.lists.review_open = workspace_dialogs.review_open;
        st.lists.regions = workspace_dialogs.regions;
        st.lists.review_items = workspace_dialogs.review_items;
        st.lists.review_status_filter = workspace_dialogs.review_status_filter;
        st.lists.add_current_region = workspace_dialogs.add_current_region;
        st.lists.add_current_review = workspace_dialogs.add_current_review;
        st.lists.region_import_requested = workspace_dialogs.region_import_requested;
        st.lists.region_export_requested = workspace_dialogs.region_export_requested;
        st.lists.review_import_requested = workspace_dialogs.review_import_requested;
        st.lists.review_export_requested = workspace_dialogs.review_export_requested;
        st.lists.previous_review_requested = workspace_dialogs.previous_review_requested;
        st.lists.next_review_requested = workspace_dialogs.next_review_requested;
        st.lists.managed_locus_requested = workspace_dialogs.managed_locus_requested;
        st.lists.region_to_delete = workspace_dialogs.region_to_delete;
        st.lists.review_to_delete = workspace_dialogs.review_to_delete;
        st.lists.region_to_relocate = workspace_dialogs.region_to_relocate;
        st.lists.review_to_relocate = workspace_dialogs.review_to_relocate;
        st.lists.sample_metadata = workspace_dialogs.sample_metadata;
        st.lists.metadata_filter_column = workspace_dialogs.metadata_filter_column;
        st.lists.metadata_filter_value = workspace_dialogs.metadata_filter_value;
        st.lists.metadata_selected_tracks = workspace_dialogs.metadata_selected_tracks;
        st.lists.metadata_bulk_height = workspace_dialogs.metadata_bulk_height;
        st.lists.metadata_remove_armed = workspace_dialogs.metadata_remove_armed;
        st.lists.metadata_import_requested = workspace_dialogs.metadata_import_requested;
        st.lists.metadata_remove_requested = workspace_dialogs.metadata_remove_requested;
        st.lists.metadata_select_requested = workspace_dialogs.metadata_select_requested;
        st.lists.metadata_bulk_visibility = workspace_dialogs.metadata_bulk_visibility;
        st.lists.metadata_bulk_collapsed = workspace_dialogs.metadata_bulk_collapsed;
        st.lists.metadata_bulk_height_requested = workspace_dialogs.metadata_bulk_height_requested;
        st.lists.metadata_bulk_rename_requested = workspace_dialogs.metadata_bulk_rename_requested;
        st.lists.metadata_bulk_alignment_settings_requested =
            workspace_dialogs.metadata_bulk_alignment_settings_requested;
        st.lists.metadata_bulk_auxiliary_appearance_requested =
            workspace_dialogs.metadata_bulk_auxiliary_appearance_requested;
        st.lists.metadata_bulk_quantitative_autoscale_requested =
            workspace_dialogs.metadata_bulk_quantitative_autoscale_requested;
        st.lists.metadata_bulk_quantitative_overlay_requested =
            workspace_dialogs.metadata_bulk_quantitative_overlay_requested;
        st.lists.metadata_bulk_quantitative_separate_requested =
            workspace_dialogs.metadata_bulk_quantitative_separate_requested;
        st.lists.metadata_bulk_remove_requested = workspace_dialogs.metadata_bulk_remove_requested;
        st.lists.metadata_track_rows = workspace_dialogs.metadata_track_rows;
    }

    /// Round-trips the command and settings state through the system dialogs.
    pub(super) fn show_system_dialogs(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        let mut system_dialogs = ui::system_dialogs::SystemDialogDraft {
            exit_confirmation_open: st.commands.exit_confirmation_open,
            exit_requested: false,
            command_palette_open: st.commands.command_palette_open,
            command_search: st.commands.command_search.clone(),
            palette_command: st.commands.palette_command,
            remote_source_open: st.commands.remote_source_open,
            remote_source_editor: st.commands.remote_source_editor.clone(),
            remote_source_request: st.commands.remote_source_request.clone(),
            settings_requested: st.commands.settings_requested,
            settings_open: st.commands.settings_open,
            cache_budget_mib: st.commands.cache_budget_mib,
            read_browsing_buffer_percent: st.commands.read_browsing_buffer_percent,
            diagnostics_opt_in: st.commands.diagnostics_opt_in,
            autosave_enabled: st.commands.autosave_enabled,
            autosave_interval_seconds: st.commands.autosave_interval_seconds,
            autosave_retention: st.commands.autosave_retention,
            interface_scale_percent: st.commands.interface_scale_percent,
            high_contrast: st.commands.high_contrast,
            reduced_motion: st.commands.reduced_motion,
            clear_recent_requested: st.commands.clear_recent_requested,
            recent_alignment_requested: st.commands.recent_alignment_requested.clone(),
            recent_session_requested: st.commands.recent_session_requested.clone(),
            recent_locus_requested: st.commands.recent_locus_requested.clone(),
            project_issues_open: st.commands.project_issues_open,
            igv_report_open: st.commands.igv_report_open,
            recovery_open: st.commands.recovery_open,
            recovery_requested: st.commands.recovery_requested.clone(),
            dismiss_recovery: st.commands.dismiss_recovery,
        };
        ui::system_dialogs::show_system_dialogs(self, ui, &mut system_dialogs);
        st.commands.exit_confirmation_open = system_dialogs.exit_confirmation_open;
        st.commands.user_exit_requested |= system_dialogs.exit_requested;
        st.commands.command_palette_open = system_dialogs.command_palette_open;
        st.commands.command_search = system_dialogs.command_search;
        st.commands.palette_command = system_dialogs.palette_command;
        st.commands.remote_source_open = system_dialogs.remote_source_open;
        st.commands.remote_source_editor = system_dialogs.remote_source_editor;
        st.commands.remote_source_request = system_dialogs.remote_source_request;
        st.commands.settings_requested = system_dialogs.settings_requested;
        st.commands.settings_open = system_dialogs.settings_open;
        st.commands.cache_budget_mib = system_dialogs.cache_budget_mib;
        st.commands.read_browsing_buffer_percent = system_dialogs.read_browsing_buffer_percent;
        st.commands.diagnostics_opt_in = system_dialogs.diagnostics_opt_in;
        st.commands.autosave_enabled = system_dialogs.autosave_enabled;
        st.commands.autosave_interval_seconds = system_dialogs.autosave_interval_seconds;
        st.commands.autosave_retention = system_dialogs.autosave_retention;
        st.commands.interface_scale_percent = system_dialogs.interface_scale_percent;
        st.commands.high_contrast = system_dialogs.high_contrast;
        st.commands.reduced_motion = system_dialogs.reduced_motion;
        st.commands.clear_recent_requested = system_dialogs.clear_recent_requested;
        st.commands.recent_alignment_requested = system_dialogs.recent_alignment_requested;
        st.commands.recent_session_requested = system_dialogs.recent_session_requested;
        st.commands.recent_locus_requested = system_dialogs.recent_locus_requested;
        st.commands.project_issues_open = system_dialogs.project_issues_open;
        st.commands.igv_report_open = system_dialogs.igv_report_open;
        st.commands.recovery_open = system_dialogs.recovery_open;
        st.commands.recovery_requested = system_dialogs.recovery_requested;
        st.commands.dismiss_recovery = system_dialogs.dismiss_recovery;
    }
}
