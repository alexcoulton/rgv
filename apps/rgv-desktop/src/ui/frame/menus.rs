//! Application menu bar.

use super::*;

impl Viewer {
    pub(super) fn show_application_menu(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        egui::Panel::top("application-menu")
            .exact_size(MENU_HEIGHT)
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("File", |ui| Self::file_menu_ui(ui, st));
                    ui.menu_button("Genomes", |ui| self.genomes_menu_ui(ui, st));
                    ui.menu_button("View", |ui| Self::view_menu_ui(ui, st));
                    ui.menu_button("Tracks", |ui| self.tracks_menu_ui(ui, st));
                    ui.menu_button("Regions", |ui| Self::regions_menu_ui(ui, st));
                    ui.menu_button("Tools", |ui| Self::tools_menu_ui(ui, st));
                    ui.menu_button("Help", Self::help_menu_ui);
                });
            });
    }

    pub(super) fn view_menu_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let command_key = st.commands.command_key;
        ui.checkbox(&mut st.commands.diagnostics_open, "Diagnostics");
        ui.checkbox(&mut st.lists.metadata_open, "Sample metadata");
        ui.checkbox(&mut st.properties_expanded, "Properties panel");
        if ui.button(format!("Settings…  {command_key}+,")).clicked() {
            st.commands.settings_requested = true;
            ui.close();
        }
    }

    pub(super) fn tools_menu_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let command_key = st.commands.command_key;
        if ui
            .button(format!("Command search…  {command_key}+K"))
            .clicked()
        {
            st.commands.command_palette_open = true;
            ui.close();
        }
        if ui.button("Clear caches").clicked() {
            st.commands.clear_cache_requested = true;
            ui.close();
        }
    }

    pub(super) fn help_menu_ui(ui: &mut egui::Ui) {
        ui.label("Drag to pan; wheel/trackpad to zoom");
        ui.label("Shift+wheel or Page Up/Down scrolls tracks");
        ui.label("Chromosome overview: drag horizontally to pan and vertically to zoom");
        ui.label("Right overview: drag vertically to scroll and horizontally for density");
        ui.label("N/P selects the next/previous visible read");
        ui.label("]/[ selects the next/previous loaded item in the selected track");
        ui.label("Enter opens settings for the selected track");
        ui.label("V/C toggles a focused track's visibility/collapse");
        ui.label("Alt+Up/Down reorders a focused track; Shift+Delete removes it");
    }

    pub(super) fn file_menu_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let command_key = st.commands.command_key;
        if ui
            .button(format!("Open BAM/CRAM…  {command_key}+O"))
            .clicked()
        {
            st.commands.open_alignment_requested = true;
            ui.close();
        }
        if ui.button("Open URL…").clicked() {
            st.commands.remote_source_open = true;
            ui.close();
        }
        if ui.button("Open session…").clicked() {
            st.commands.open_session_requested = true;
            ui.close();
        }
        let save_label = if st.commands.current_session_path.is_some() {
            format!("Save project  {command_key}+S")
        } else {
            format!("Save project as…  {command_key}+S")
        };
        if ui.button(save_label).clicked() {
            st.commands.save_session_requested = true;
            ui.close();
        }
        if st.commands.current_session_path.is_some() && ui.button("Save project as…").clicked() {
            st.commands.save_session_as_requested = true;
            ui.close();
        }
        if !st.commands.unresolved_session_sources.is_empty()
            || !st.commands.session_migration_notices.is_empty()
        {
            ui.separator();
            if ui
                .button(format!(
                    "Project issues ({})…",
                    st.commands
                        .unresolved_session_sources
                        .len()
                        .saturating_add(st.commands.session_migration_notices.len())
                ))
                .clicked()
            {
                st.commands.project_issues_open = true;
                ui.close();
            }
        }
        if st.commands.igv_import_report.is_some() && ui.button("Last IGV import report…").clicked()
        {
            st.commands.igv_report_open = true;
            ui.close();
        }
        ui.separator();
        if ui.button(format!("Export PNG…  {command_key}+E")).clicked() {
            st.commands.export_png_requested = true;
            ui.close();
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn genomes_menu_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.label(&st.nav.assembly_label);
        if let Some(identity) = &self.assembly.identity {
            ui.small(format!(
                "{} · {:?} · {} fingerprint matches",
                identity.stable_id, identity.confidence, identity.matched_fingerprints
            ));
        } else if let Some(suggestion) = &self.suggested_assembly_identity {
            ui.colored_label(
                egui::Color32::from_rgb(150, 92, 20),
                format!(
                    "Suggested: {} ({} fingerprint matches); load a genome configuration to confirm",
                    suggestion.display_name, suggestion.matched_fingerprints
                ),
            );
        } else {
            ui.small("Assembly unidentified");
        }
        ui.small(format!(
            "Reference: {} contigs · cytobands: {} · default annotations: {}",
            self.assembly.reference_contig_count,
            if self.assembly.has_cytobands() {
                "loaded"
            } else {
                "missing"
            },
            self.assembly.default_annotation_count,
        ));
        if !self.assembly.has_cytobands() || self.assembly.default_annotation_count == 0 {
            ui.colored_label(
                egui::Color32::from_rgb(150, 92, 20),
                "Genome context incomplete — open a genome configuration",
            );
        }
        if let Some(resolution) = &self.landmark_resolution {
            ui.small(format!(
                "Landmarks: {:?} · {:?} · {}",
                resolution.status, resolution.evidence, resolution.reason
            ));
        }
        if let Some(error) = &self.landmark_catalogue_error {
            ui.colored_label(egui::Color32::from_rgb(170, 45, 38), error);
        }
        if ui.button("Open FASTA…").clicked() {
            st.commands.open_reference_requested = true;
            ui.close();
        }
        if ui.button("Open genome configuration…").clicked() {
            st.commands.open_genome_config_requested = true;
            ui.close();
        }
        ui.menu_button("Hosted genomes", |ui| self.hosted_genomes_menu_ui(ui, st));
        if self
            .landmark_resolution
            .as_ref()
            .and_then(|resolution| resolution.bundle_id.as_ref())
            .is_some_and(|bundle_id| self.suppressed_builtin_gene_bundles.contains(bundle_id))
            && ui.button("Restore default genes").clicked()
        {
            st.commands.restore_default_genes_requested = true;
            ui.close();
        }
        if self.landmark_resource_root.is_some()
            && ui.button("Open landmark resource folder").clicked()
        {
            st.commands.open_landmark_folder_requested = true;
            ui.close();
        }
        ui.separator();
        // Assemblies with alternate and unplaced sequences have hundreds
        // of contigs; keep the menu a usable height.
        egui::ScrollArea::vertical()
            .id_salt("genome-contig-list")
            .max_height(420.0)
            .show(ui, |ui| {
                for contig in self.assembly.dictionary.iter() {
                    if ui.button(contig.name()).clicked() {
                        st.nav.requested_contig = Some(contig.id());
                        ui.close();
                    }
                }
            });
    }

    pub(super) fn hosted_genomes_menu_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        use crate::workspace::genomes::{HostedGenomeListStatus, HostedGenomes};
        match &self.hosted_genomes.status {
            HostedGenomeListStatus::NotLoaded => {
                st.commands.refresh_hosted_genomes_requested = true;
                ui.small("Loading the genome list…");
            }
            HostedGenomeListStatus::Loading => {
                ui.small("Loading the genome list…");
            }
            HostedGenomeListStatus::Failed(error) => {
                ui.colored_label(
                    egui::Color32::from_rgb(170, 45, 38),
                    format!("The genome list could not be read: {error}"),
                );
                if ui.button("Try again").clicked() {
                    st.commands.refresh_hosted_genomes_requested = true;
                }
            }
            HostedGenomeListStatus::Loaded { from_cache } => {
                if *from_cache {
                    ui.small("Offline: showing the last saved list");
                }
                ui.add(
                    egui::TextEdit::singleline(&mut st.commands.hosted_genome_filter)
                        .hint_text("Filter by name or id")
                        .desired_width(280.0),
                );
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for entry in HostedGenomes::matching(
                            &self.hosted_genomes.entries,
                            &st.commands.hosted_genome_filter,
                        ) {
                            if ui
                                .button(format!("{} ({})", entry.name, entry.id))
                                .clicked()
                            {
                                st.commands.hosted_genome_requested = Some(entry.id.clone());
                                ui.close();
                            }
                        }
                    });
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn tracks_menu_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        if ui.button("Add track…").clicked() {
            st.commands.add_track_requested = true;
            ui.close();
        }
        if ui.button("Import sample attributes…").clicked() {
            st.lists.metadata_import_requested = true;
            st.lists.metadata_open = true;
            ui.close();
        }
        ui.separator();
        if self.alignment_tracks.is_empty() && self.synthetic {
            let id = self.primary_alignment_track_id();
            let filters = self.live_session_filters();
            ui.menu_button(self.alignment_track_name(), |ui| {
                if ui
                    .checkbox(&mut st.align.alignment_visible, "Show annotation content")
                    .changed()
                {
                    st.align.alignment_collapsed = !st.align.alignment_visible;
                }
                ui.separator();
                alignment_subtrack_menu(
                    ui,
                    id,
                    &filters,
                    st.align.show_reads,
                    true,
                    &mut st.tracks.track_ui_actions,
                );
                ui.separator();
                if ui.button("Alignment settings…").clicked() {
                    st.tracks.track_settings_panel = Some(TrackFrameKey::Alignment(id));
                    ui.close();
                }
            });
        }
        for control in &mut st.tracks.alignment_track_controls {
            let filters = st
                .tracks
                .alignment_subtrack_controls
                .iter()
                .find(|(candidate, _)| *candidate == control.id)
                .map(|(_, filters)| filters)
                .cloned()
                .unwrap_or_default();
            ui.menu_button(control.name.as_str(), |ui| {
                if ui
                    .checkbox(&mut control.visible, "Show annotation content")
                    .changed()
                {
                    control.collapsed = !control.visible;
                }
                ui.separator();
                alignment_subtrack_menu(
                    ui,
                    control.id,
                    &filters,
                    self.track_presentation
                        .pane_track_state(ui::TrackPane::Reads, control.id)
                        .is_none_or(|node| node.content_visible),
                    control.id == st.align.primary_alignment_id,
                    &mut st.tracks.track_ui_actions,
                );
                ui.separator();
                if ui.button("Alignment settings…").clicked() {
                    st.tracks.track_settings_panel = Some(TrackFrameKey::Alignment(control.id));
                    ui.close();
                }
            });
        }
        for control in &mut st.tracks.track_controls {
            if ui
                .checkbox(&mut control.visible, control.name.as_str())
                .changed()
            {
                control.collapsed = !control.visible;
            }
        }
    }

    pub(super) fn regions_menu_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        if ui.button("Add current view as region").clicked() {
            st.lists.add_current_region = true;
            ui.close();
        }
        if ui.button("Add current view to review list").clicked() {
            st.lists.add_current_review = true;
            ui.close();
        }
        if ui.button("Manage regions…").clicked() {
            st.lists.regions_open = true;
            ui.close();
        }
        if ui.button("Review queue…").clicked() {
            st.lists.review_open = true;
            ui.close();
        }
        ui.separator();
        if ui.button("Previous annotation feature").clicked() {
            st.nav.previous_feature_requested = true;
            ui.close();
        }
        if ui.button("Next annotation feature").clicked() {
            st.nav.next_feature_requested = true;
            ui.close();
        }
        ui.separator();
        for locus in &st.commands.recent_loci {
            if ui.button(locus).clicked() {
                st.commands.recent_locus_requested = Some(locus.clone());
                ui.close();
            }
        }
        if st.commands.recent_loci.is_empty() {
            ui.label("No recent loci");
        }
    }
}
