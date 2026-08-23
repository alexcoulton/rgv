use super::super::*;

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SystemDialogDraft {
    pub(crate) command_palette_open: bool,
    pub(crate) command_search: String,
    pub(crate) palette_command: Option<UiCommand>,
    pub(crate) remote_source_open: bool,
    pub(crate) remote_source_editor: RemoteSourceEditor,
    pub(crate) remote_source_request: Option<RemoteSourceRequest>,
    pub(crate) settings_requested: bool,
    pub(crate) settings_open: bool,
    pub(crate) cache_budget_mib: usize,
    pub(crate) diagnostics_opt_in: bool,
    pub(crate) autosave_enabled: bool,
    pub(crate) autosave_interval_seconds: u64,
    pub(crate) autosave_retention: usize,
    pub(crate) interface_scale_percent: u16,
    pub(crate) high_contrast: bool,
    pub(crate) reduced_motion: bool,
    pub(crate) clear_recent_requested: bool,
    pub(crate) recent_alignment_requested: Option<TrackSource>,
    pub(crate) recent_session_requested: Option<String>,
    pub(crate) recent_locus_requested: Option<String>,
    pub(crate) project_issues_open: bool,
    pub(crate) igv_report_open: bool,
    pub(crate) recovery_open: bool,
    pub(crate) recovery_requested: Option<PathBuf>,
    pub(crate) dismiss_recovery: bool,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn show_system_dialogs(
    viewer: &Viewer,
    ui: &mut egui::Ui,
    draft: &mut SystemDialogDraft,
) {
    let recent_alignments = viewer.settings.recent_alignments.clone();
    let recent_sessions = viewer.settings.recent_sessions.clone();
    let recent_loci = viewer.settings.recent_loci.clone();
    let settings_path = viewer.settings_path.display().to_string();
    let unresolved_session_sources = viewer.unresolved_session_sources.clone();
    let igv_import_report = viewer.igv_import_report.clone();
    let recovery_candidates = viewer.recovery_candidates.clone();

    if draft.command_palette_open {
        let context = ui.ctx().clone();
        egui::Window::new("Command search")
            .default_pos([360.0, 120.0])
            .default_width(360.0)
            .collapsible(false)
            .show(&context, |ui| {
                let search_label = ui.label("Command");
                let search_response = ui
                    .add(
                        egui::TextEdit::singleline(&mut draft.command_search)
                            .id(egui::Id::new(COMMAND_SEARCH_ID))
                            .hint_text("Type a command"),
                    )
                    .labelled_by(search_label.id);
                let execute_first = search_response.has_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let mut first_match = true;
                for &(label, command) in SEARCHABLE_COMMANDS {
                    if ui::command_matches(label, &draft.command_search) {
                        if (execute_first && first_match) || ui.button(label).clicked() {
                            draft.palette_command = Some(command);
                            draft.command_palette_open = false;
                        }
                        first_match = false;
                    }
                }
                if ui.button("Close").clicked() {
                    draft.command_palette_open = false;
                }
            });
    }
    if draft.remote_source_open {
        let context = ui.ctx().clone();
        let mut close_remote_source = false;
        egui::Window::new("Open remote source")
                    .default_pos([300.0, 90.0])
                    .default_width(560.0)
                    .collapsible(false)
                    .open(&mut draft.remote_source_open)
                    .show(&context, |ui| {
                        egui::ComboBox::from_label("Source type")
                            .selected_text(match draft.remote_source_editor.kind {
                                RemoteSourceKind::Alignment => "BAM or CRAM alignment",
                                RemoteSourceKind::Reference => "Reference FASTA",
                                RemoteSourceKind::IndexedTrack => "Indexed genomic track",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut draft.remote_source_editor.kind,
                                    RemoteSourceKind::Alignment,
                                    "BAM or CRAM alignment",
                                );
                                ui.selectable_value(
                                    &mut draft.remote_source_editor.kind,
                                    RemoteSourceKind::Reference,
                                    "Reference FASTA",
                                );
                                ui.selectable_value(
                                    &mut draft.remote_source_editor.kind,
                                    RemoteSourceKind::IndexedTrack,
                                    "VCF/BCF, BigWig, TDF, BigBed, bigGenePred, or bigNarrowPeak",
                                );
                            });
                        let source_label = ui.label("Source URL");
                        ui.add(
                            egui::TextEdit::singleline(&mut draft.remote_source_editor.source_url)
                                .hint_text("https://example.org/data.bam"),
                        )
                        .labelled_by(source_label.id);
                        if draft.remote_source_editor.kind != RemoteSourceKind::IndexedTrack {
                            let index_label = ui.label(match draft.remote_source_editor.kind {
                                RemoteSourceKind::Alignment => {
                                    "Custom BAI, CSI, or CRAI URL (optional)"
                                }
                                RemoteSourceKind::Reference => "Custom FAI URL (optional)",
                                RemoteSourceKind::IndexedTrack => unreachable!(),
                            });
                            ui.add(
                                egui::TextEdit::singleline(&mut draft.remote_source_editor.index_url)
                                    .hint_text("Leave empty to use the adjacent index"),
                            )
                            .labelled_by(index_label.id);
                        }
                        if draft.remote_source_editor.kind == RemoteSourceKind::Alignment {
                            ui.separator();
                            ui.label("Reference for CRAM (optional)");
                            let reference_label = ui.label("Reference FASTA URL");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut draft.remote_source_editor.reference_url,
                                )
                                .hint_text("https://example.org/reference.fa"),
                            )
                            .labelled_by(reference_label.id);
                            let reference_index_label =
                                ui.label("Custom reference FAI URL (optional)");
                            ui.add(
                                egui::TextEdit::singleline(
                                    &mut draft.remote_source_editor.reference_index_url,
                                )
                                .hint_text("Leave empty to use reference.fa.fai"),
                            )
                            .labelled_by(reference_index_label.id);
                        }
                        if let Some(error) = &draft.remote_source_editor.error {
                            ui.colored_label(egui::Color32::DARK_RED, error);
                        }
                        ui.small(
                            "Indexed HTTP and HTTPS sources use range requests. BAM/CRAM/FASTA bearer authentication can use RGV_REMOTE_BEARER_TOKEN; signed source query strings are preserved.",
                        );
                        ui.horizontal(|ui| {
                            if ui.button("Open").clicked() {
                                match validate_remote_source_editor(&draft.remote_source_editor) {
                                    Ok(request) => {
                                        draft.remote_source_request = Some(request);
                                        draft.remote_source_editor.error = None;
                                        close_remote_source = true;
                                    }
                                    Err(error) => {
                                        draft.remote_source_editor.error = Some(error.to_string());
                                    }
                                }
                            }
                            if ui.button("Cancel").clicked() {
                                close_remote_source = true;
                            }
                        });
                    });
        if close_remote_source {
            draft.remote_source_open = false;
        }
    }
    if draft.settings_requested {
        draft.settings_open = true;
    }
    if draft.settings_open {
        let context = ui.ctx().clone();
        egui::Window::new("Settings")
                    .default_pos([300.0, 100.0])
                    .default_width(460.0)
                    .open(&mut draft.settings_open)
                    .show(&context, |ui| {
                        ui.heading("Accessibility");
                        ui.horizontal(|ui| {
                            let label = ui.label("Interface scale");
                            ui.add(
                                egui::Slider::new(
                                    &mut draft.interface_scale_percent,
                                    MINIMUM_INTERFACE_SCALE_PERCENT
                                        ..=MAXIMUM_INTERFACE_SCALE_PERCENT,
                                )
                                .suffix("%"),
                            )
                            .labelled_by(label.id);
                        });
                        ui.checkbox(&mut draft.high_contrast, "Increase interface contrast");
                        ui.checkbox(&mut draft.reduced_motion, "Reduce interface motion");
                        ui.small(
                            "Scaling applies immediately. Reduced motion replaces animated loading indicators with the status text.",
                        );
                        ui.separator();
                        ui.heading("Performance");
                        ui.horizontal(|ui| {
                            let label = ui.label("Total cache budget");
                            ui.add(
                                egui::Slider::new(
                                    &mut draft.cache_budget_mib,
                                    MINIMUM_CACHE_BUDGET_MIB..=MAXIMUM_CACHE_BUDGET_MIB,
                                )
                                .suffix(" MiB")
                                .logarithmic(true),
                            )
                            .labelled_by(label.id);
                        });
                        ui.small("A changed cache budget takes effect after RGV restarts.");
                        ui.separator();
                        ui.heading("Recovery autosave");
                        ui.checkbox(
                            &mut draft.autosave_enabled,
                            "Periodically save recovery snapshots",
                        );
                        ui.add_enabled_ui(draft.autosave_enabled, |ui| {
                            ui.horizontal(|ui| {
                                let label = ui.label("Interval");
                                ui.add(
                                    egui::Slider::new(
                                        &mut draft.autosave_interval_seconds,
                                        MINIMUM_AUTOSAVE_INTERVAL_SECONDS
                                            ..=MAXIMUM_AUTOSAVE_INTERVAL_SECONDS,
                                    )
                                    .suffix(" seconds")
                                    .logarithmic(true),
                                )
                                .labelled_by(label.id);
                            });
                            ui.horizontal(|ui| {
                                let label = ui.label("Snapshots kept per project");
                                ui.add(egui::Slider::new(
                                    &mut draft.autosave_retention,
                                    MINIMUM_AUTOSAVE_RETENTION..=MAXIMUM_AUTOSAVE_RETENTION,
                                ))
                                .labelled_by(label.id);
                            });
                        });
                        ui.small(
                            "Disabled by default. Snapshots are stored locally and offered after an unclean exit.",
                        );
                        ui.separator();
                        ui.heading("Privacy");
                        ui.checkbox(
                            &mut draft.diagnostics_opt_in,
                            "Allow opt-in diagnostic collection",
                        )
                        .on_hover_text(
                            "Disabled by default. This build does not transmit diagnostics.",
                        );
                        ui.separator();
                        ui.heading("Recent alignments");
                        if recent_alignments.is_empty() {
                            ui.label("None");
                        }
                        for source in &recent_alignments {
                            let label = source.redacted_label();
                            if ui.button(&label).on_hover_text("Open alignment").clicked() {
                                draft.recent_alignment_requested = Some(source.clone());
                            }
                        }
                        ui.heading("Recent sessions");
                        if recent_sessions.is_empty() {
                            ui.label("None");
                        }
                        for path in &recent_sessions {
                            if ui.button(path).on_hover_text("Open session").clicked() {
                                draft.recent_session_requested = Some(path.clone());
                            }
                        }
                        ui.heading("Recent loci");
                        if recent_loci.is_empty() {
                            ui.label("None");
                        }
                        for locus in &recent_loci {
                            if ui
                                .button(locus)
                                .on_hover_text("Navigate to locus")
                                .clicked()
                            {
                                draft.recent_locus_requested = Some(locus.clone());
                            }
                        }
                        ui.separator();
                        if ui.button("Clear recent items").clicked() {
                            draft.clear_recent_requested = true;
                        }
                        ui.small(format!("Settings file: {settings_path}"));
                    });
    }
    if draft.project_issues_open {
        let context = ui.ctx().clone();
        egui::Window::new("Unresolved project sources")
            .default_pos([260.0, 100.0])
            .default_width(640.0)
            .open(&mut draft.project_issues_open)
            .show(&context, |ui| {
                ui.label(
                    "These local files could not be found. Other project tracks remain available.",
                );
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for issue in &unresolved_session_sources {
                        ui.strong(&issue.label);
                        ui.monospace(issue.path.display().to_string());
                        ui.add_space(8.0);
                    }
                });
            });
    }
    if draft.igv_report_open
        && let Some(report) = &igv_import_report
    {
        let context = ui.ctx().clone();
        egui::Window::new("IGV session import report")
                    .default_pos([240.0, 80.0])
                    .default_width(700.0)
                    .default_height(560.0)
                    .open(&mut draft.igv_report_open)
                    .show(&context, |ui| {
                        ui.strong(report.summary());
                        ui.small(
                            "RGV lists translated, approximate, and unsupported IGV XML semantics explicitly.",
                        );
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (heading, notices, color) in [
                                ("Imported", &report.imported, egui::Color32::from_rgb(25, 120, 65)),
                                ("Approximated", &report.approximated, egui::Color32::from_rgb(165, 105, 20)),
                                ("Ignored", &report.ignored, egui::Color32::from_rgb(170, 45, 45)),
                            ] {
                                ui.colored_label(color, heading);
                                if notices.is_empty() {
                                    ui.label("None");
                                }
                                for notice in notices {
                                    ui.label(format!("• {notice}"));
                                }
                                ui.add_space(10.0);
                            }
                            if report.omitted_notice_count > 0 {
                                ui.label(format!(
                                    "{} additional unique notices exceeded the bounded report limit.",
                                    report.omitted_notice_count
                                ));
                            }
                        });
                    });
    }
    if draft.recovery_open {
        let context = ui.ctx().clone();
        egui::Window::new("Recover an autosaved project")
                    .default_pos([320.0, 120.0])
                    .default_width(560.0)
                    .collapsible(false)
                    .open(&mut draft.recovery_open)
                    .show(&context, |ui| {
                        ui.label(
                            "RGV did not shut down cleanly. Choose a local recovery snapshot or continue without restoring one.",
                        );
                        ui.separator();
                        for (index, path) in recovery_candidates.iter().enumerate() {
                            let name = path
                                .file_name()
                                .map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
                            if ui
                                .button(format!("Recover {} · {name}", index + 1))
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                draft.recovery_requested = Some(path.clone());
                            }
                        }
                        ui.separator();
                        if ui.button("Continue without recovery").clicked() {
                            draft.dismiss_recovery = true;
                        }
                        ui.small(
                            "A recovered snapshot opens as an unsaved project, so it cannot overwrite the original accidentally.",
                        );
                    });
    }
}
