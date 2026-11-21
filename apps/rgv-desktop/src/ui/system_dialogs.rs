use crate::ui::{SEARCHABLE_COMMANDS, UiCommand};
use crate::{
    COMMAND_SEARCH_ID, RemoteSourceEditor, RemoteSourceKind, RemoteSourceRequest, Viewer, ui,
    validate_remote_source_editor,
};
use rgv_core::{
    MAXIMUM_AUTOSAVE_INTERVAL_SECONDS, MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_CACHE_BUDGET_MIB,
    MAXIMUM_INTERFACE_SCALE_PERCENT, MAXIMUM_READ_BROWSING_BUFFER_PERCENT,
    MINIMUM_AUTOSAVE_INTERVAL_SECONDS, MINIMUM_AUTOSAVE_RETENTION, MINIMUM_CACHE_BUDGET_MIB,
    MINIMUM_INTERFACE_SCALE_PERCENT, MINIMUM_READ_BROWSING_BUFFER_PERCENT, TrackSource,
};
use rgv_engine::AlignmentVerticalResidency;
use std::path::PathBuf;

fn exit_confirmation_modal() -> egui::Modal {
    let id = egui::Id::new("exit-confirmation");
    egui::Modal::new(id).area(egui::Modal::default_area(id).fade_in(false))
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct SystemDialogDraft {
    pub(crate) exit_confirmation_open: bool,
    pub(crate) exit_requested: bool,
    pub(crate) command_palette_open: bool,
    pub(crate) command_search: String,
    pub(crate) palette_command: Option<UiCommand>,
    pub(crate) remote_source_open: bool,
    pub(crate) remote_source_editor: RemoteSourceEditor,
    pub(crate) remote_source_request: Option<RemoteSourceRequest>,
    pub(crate) settings_requested: bool,
    pub(crate) settings_open: bool,
    pub(crate) cache_budget_mib: usize,
    pub(crate) read_browsing_buffer_percent: u16,
    pub(crate) diagnostics_opt_in: bool,
    pub(crate) autosave_enabled: bool,
    pub(crate) autosave_interval_seconds: u64,
    pub(crate) autosave_retention: usize,
    pub(crate) interface_scale_percent: u16,
    pub(crate) high_contrast: bool,
    pub(crate) interface_theme: rgv_core::InterfaceTheme,
    pub(crate) reduced_motion: bool,
    pub(crate) coverage_cache_enabled: bool,
    pub(crate) track_pipeline: rgv_core::TrackPipeline,
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
    // Borrowed, not copied: this runs every frame whether or not any dialog is open.
    let recent_alignments = &viewer.session.settings.recent_alignments;
    let recent_sessions = &viewer.session.settings.recent_sessions;
    let recent_loci = &viewer.session.settings.recent_loci;
    let settings_path = &viewer.session.settings_path;
    let unresolved_session_sources = &viewer.session.unresolved_session_sources;
    let session_migration_notices = &viewer.session.session_migration_notices;
    let igv_import_report = &viewer.session.igv_import_report;
    let recovery_candidates = &viewer.recovery_candidates;

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
                            ui.colored_label(crate::theme::adapt(egui::Color32::DARK_RED), error);
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
                        ui.horizontal(|ui| {
                            let label = ui.label("Theme");
                            egui::ComboBox::from_id_salt("rgv-interface-theme")
                                .selected_text(crate::theme::theme_label(draft.interface_theme))
                                .show_ui(ui, |ui| {
                                    for theme in crate::theme::INTERFACE_THEMES {
                                        ui.selectable_value(
                                            &mut draft.interface_theme,
                                            theme,
                                            crate::theme::theme_label(theme),
                                        );
                                    }
                                })
                                .response
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
                        ui.small(
                            "Changes apply live to one aggregate application-owned cache scope: alignment evidence/scenes/GPU pages plus contextual and generic GPU buffers. Driver/runtime memory is reported separately.",
                        );
                        ui.horizontal(|ui| {
                            let label = ui.label("Read browsing buffer");
                            ui.add(
                                egui::Slider::new(
                                    &mut draft.read_browsing_buffer_percent,
                                    MINIMUM_READ_BROWSING_BUFFER_PERCENT
                                        ..=MAXIMUM_READ_BROWSING_BUFFER_PERCENT,
                                )
                                .step_by(25.0)
                                .custom_formatter(|value, _| {
                                    format!("{:.2}× / side", value / 100.0)
                                }),
                            )
                            .labelled_by(label.id)
                            .on_hover_text(
                                "Desired prepared margin on each genomic side and above/below the visible read rows. RGV may reduce it to respect memory, density, Complete read-limit, or remote-transfer bounds.",
                            );
                        });
                        ui.small(
                            "The 1× default targets one viewport or visible row-window on each side. 0× disables speculative adjacent preparation but keeps canonical page reuse. Remote sources acquire genomic margins conservatively and stop at hard transfer bounds.",
                        );
                        ui.checkbox(
                            &mut draft.coverage_cache_enabled,
                            "Keep alignment coverage in a disk cache",
                        )
                        .on_hover_text(
                            "Coverage computed for the overview is stored per alignment file so it appears at once next time. Off keeps coverage in memory only. Tools › Clear caches empties the cache.",
                        );
                        ui.small(match rgv_hts::default_coverage_cache_directory() {
                            Some(directory) => format!(
                                "Coverage cache: {} (at most {} MiB; applies immediately).",
                                directory.display(),
                                rgv_hts::COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES / (1024 * 1024),
                            ),
                            None => "Coverage cache: no per-user cache directory is available; coverage stays in memory.".to_owned(),
                        });
                        let mut tile_pipeline =
                            draft.track_pipeline == rgv_core::TrackPipeline::Tiles;
                        if ui
                            .checkbox(
                                &mut tile_pipeline,
                                "Experimental: draw annotation tracks with the tile pipeline",
                            )
                            .on_hover_text(
                                "Off by default. When on, plain BED, bigBed, GFF3 and GTF annotation tracks with default display settings are read as cached tiles and drawn by the new track renderer; every other track keeps the established pipeline. Known gaps are listed in the user guide under “Experimental track pipeline”. Applies immediately.",
                            )
                            .changed()
                        {
                            draft.track_pipeline = if tile_pipeline {
                                rgv_core::TrackPipeline::Tiles
                            } else {
                                rgv_core::TrackPipeline::Legacy
                            };
                        }
                        let budget = viewer.alignment_scene_budget;
                        let mib = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
                        let context_gpu = viewer.context_scene_gpu_renderer.resident_bytes();
                        let generic_gpu = viewer.rectangle_renderer.resident_bytes();
                        let aggregate_occupied = budget
                            .charged_bytes
                            .saturating_add(budget.reserved_bytes)
                            .saturating_add(context_gpu)
                            .saturating_add(generic_gpu);
                        ui.group(|ui| {
                            ui.label(format!(
                                "Aggregate cache: {:.1} / {:.1} MiB occupied",
                                mib(aggregate_occupied),
                                draft.cache_budget_mib as f64,
                            ));
                            ui.label(format!(
                                "Alignment ledger: {:.1} / {:.1} MiB retained",
                                mib(budget.charged_bytes),
                                mib(budget.budget_bytes),
                            ));
                            ui.small(format!(
                                "Explicit GPU reservations: contextual {:.1} MiB · generic {:.1} MiB",
                                mib(context_gpu),
                                mib(generic_gpu),
                            ));
                            ui.small(format!(
                                "Reserved {:.1} MiB (transition {:.1} / {:.1}) · temporary {:.1} / {:.1} MiB (peak {:.1})",
                                mib(budget.reserved_bytes),
                                mib(budget.reserved_transition_bytes),
                                mib(budget.transition_reserve_bytes),
                                mib(budget.transient_bytes),
                                mib(budget.transient_budget_bytes),
                                mib(budget.transient_high_water_bytes),
                            ));
                            ui.small(format!(
                                "Pinned {:.1} MiB · replacement bundle {} · {} ledger refreshes",
                                mib(budget.pinned_bytes),
                                budget.active_replacement_bundle.map_or_else(
                                    || "none".to_owned(),
                                    |bundle| bundle.to_string(),
                                ),
                                budget.refreshes,
                            ));
                            if budget.pinned_overage_bytes > 0 {
                                ui.colored_label(
                                    crate::theme::adapt(egui::Color32::DARK_RED),
                                    format!(
                                        "Committed scene pins {:.1} MiB above the selected budget",
                                        mib(budget.pinned_overage_bytes),
                                    ),
                                );
                            }

                            let active_panel_id = viewer.session.panels.active().id;
                            if let Some(projection) = viewer
                                .alignment_tracks
                                .first()
                                .and_then(|track| track.projection(active_panel_id))
                            {
                                let viewport = viewer.session.panels.active().viewport.interval();
                                let viewport_bases = viewport.len().max(1) as f64;
                                let prepared = projection
                                    .scene_frame
                                    .as_ref()
                                    .and_then(|frame| frame.snapshot.as_ref())
                                    .map(|snapshot| snapshot.prepared_interval());
                                let left = prepared.map_or(0, |interval| {
                                    viewport.start().saturating_sub(interval.start())
                                });
                                let right = prepared.map_or(0, |interval| {
                                    interval.end().saturating_sub(viewport.end())
                                });
                                let requested = projection.scene_policy.map_or(
                                    (0.0, 0.0),
                                    |policy| (policy.behind_viewports, policy.ahead_viewports),
                                );
                                ui.small(format!(
                                    "Prepared scene margin: {:.2}× left / {:.2}× right (requested {:.2}× / {:.2}×)",
                                    left as f64 / viewport_bases,
                                    right as f64 / viewport_bases,
                                    requested.0,
                                    requested.1,
                                ));
                                ui.small(format!(
                                    "Scene {:?} · vertical {:?} · {} source acquisitions · {} camera-only updates",
                                    projection.navigation.phase(),
                                    projection
                                        .scene_frame
                                        .as_ref()
                                        .map_or(AlignmentVerticalResidency::ViewportBounded, |frame| frame.vertical_residency),
                                    projection.scene_telemetry.source_acquisitions,
                                    projection.scene_telemetry.camera_only_updates,
                                ));
                            }
                        });
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
                        for source in recent_alignments {
                            let label = source.redacted_label();
                            if ui.button(&label).on_hover_text("Open alignment").clicked() {
                                draft.recent_alignment_requested = Some(source.clone());
                            }
                        }
                        ui.heading("Recent sessions");
                        if recent_sessions.is_empty() {
                            ui.label("None");
                        }
                        for path in recent_sessions {
                            if ui.button(path).on_hover_text("Open session").clicked() {
                                draft.recent_session_requested = Some(path.clone());
                            }
                        }
                        ui.heading("Recent loci");
                        if recent_loci.is_empty() {
                            ui.label("None");
                        }
                        for locus in recent_loci {
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
                        ui.small(format!("Settings file: {}", settings_path.display()));
                    });
    }
    if draft.project_issues_open {
        let context = ui.ctx().clone();
        egui::Window::new("Project issues")
            .default_pos([260.0, 100.0])
            .default_width(640.0)
            .open(&mut draft.project_issues_open)
            .show(&context, |ui| {
                ui.label("Repairs and unresolved resources found while opening this project.");
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if !session_migration_notices.is_empty() {
                        ui.strong("Session migration repairs");
                        for notice in session_migration_notices {
                            ui.label(format!("• {notice}"));
                        }
                        ui.add_space(10.0);
                    }
                    if !unresolved_session_sources.is_empty() {
                        ui.strong("Unresolved local sources");
                        ui.label(
                            "These files could not be found. Other project tracks remain available.",
                        );
                        ui.add_space(6.0);
                    }
                    for issue in unresolved_session_sources {
                        ui.strong(&issue.label);
                        ui.monospace(issue.path.display().to_string());
                        ui.add_space(8.0);
                    }
                });
            });
    }
    if draft.igv_report_open
        && let Some(report) = igv_import_report
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
                                ("Imported", &report.imported, crate::theme::adapt(egui::Color32::from_rgb(25, 120, 65))),
                                ("Approximated", &report.approximated, crate::theme::adapt(egui::Color32::from_rgb(165, 105, 20))),
                                ("Ignored", &report.ignored, crate::theme::adapt(egui::Color32::from_rgb(170, 45, 45))),
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
    // Paint the exit confirmation last so it remains above any already-open
    // window. Modal blocks interactions with the workspace until dismissed.
    if draft.exit_confirmation_open {
        // Exit confirmation is a blocking response to an explicit command;
        // presenting it gradually makes the application feel unresponsive.
        // Disable the area's own fade rather than changing animation timing
        // globally, so the dialog appears at full opacity immediately.
        let response = exit_confirmation_modal().show(ui.ctx(), |ui| {
            ui.heading("Exit RGV?");
            let shortcut = if cfg!(target_os = "macos") {
                "Cmd+W"
            } else {
                "Ctrl+W"
            };
            ui.label(format!(
                "Press {shortcut} again to exit RGV, or choose an option below."
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    draft.exit_confirmation_open = false;
                }
                if ui.button("Exit RGV").clicked() {
                    draft.exit_requested = true;
                }
            });
        });
        if response.should_close() {
            draft.exit_confirmation_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_modal_is_full_opacity_on_its_first_visible_frame() {
        let context = egui::Context::default();
        let theme = context.theme();
        context.style_mut_of(theme, |style| style.animation_time = 0.2);
        let original_animation_time = context.style_of(theme).animation_time;
        let input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            time: Some(0.0),
            ..egui::RawInput::default()
        };

        // The first pass measures a newly-created area. The following pass is
        // the first one that users see, which is where Area normally fades in.
        let _ = context.run_ui(input(), |root_ui| {
            exit_confirmation_modal().show(root_ui.ctx(), |ui| {
                ui.label("Exit RGV?");
            });
        });
        let output = context.run_ui(input(), |root_ui| {
            exit_confirmation_modal().show(root_ui.ctx(), |ui| {
                ui.label("Exit RGV?");
            });
        });
        let maximum_alpha = context
            .tessellate(output.shapes, output.pixels_per_point)
            .into_iter()
            .filter_map(|primitive| match primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => {
                    mesh.vertices.iter().map(|vertex| vertex.color.a()).max()
                }
                egui::epaint::Primitive::Callback(_) => None,
            })
            .max();

        assert_eq!(maximum_alpha, Some(u8::MAX));
        assert_eq!(
            context.style_of(theme).animation_time,
            original_animation_time
        );
    }
}
