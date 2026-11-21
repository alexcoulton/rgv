//! Status bar, navigation toolbar, annotation search popup and the Properties side panel.

use crate::app::status_activity::StatusActivity;
use crate::ui::frame::ShellFrame;
use crate::ui::{NAVIGATION_HEIGHT, STATUS_HEIGHT};
use crate::workspace::navigation::{
    annotation_search_hit_primary_label, annotation_search_hit_secondary_identity,
};
use crate::{LOCUS_EDITOR_ID, Viewer, ui};

impl Viewer {
    pub(super) fn show_status_bar(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let data_status = &st.snapshot.data_status;
        egui::Panel::bottom("application-status")
            .exact_size(STATUS_HEIGHT)
            .show(ui, |ui| {
                let badge = super::shell_dialogs::notification_badge_label(
                    st.commands.shell.error_count,
                    st.commands.shell.unseen_error_count,
                );
                let (status_response, badge_clicked) = ui::status_bar_with_badge(
                    ui,
                    st.layout.track_frames.len(),
                    &st.snapshot.status_locus,
                    &st.snapshot.data_status,
                    st.snapshot.selection_summary.as_deref(),
                    badge.as_deref(),
                );
                if badge_clicked {
                    st.commands.shell.notifications_open = !st.commands.shell.notifications_open;
                }
                if st.snapshot.announce_status {
                    status_response.output_event(egui::output::OutputEvent::ValueChanged(
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Label,
                            true,
                            format!("Application status: {data_status}"),
                        ),
                    ));
                }
            });
    }

    pub(super) fn show_navigation_toolbar(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        egui::Panel::top("navigation-toolbar")
            .exact_size(NAVIGATION_HEIGHT)
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let assembly_response = egui::ComboBox::from_id_salt("assembly-selector")
                        .selected_text(&st.nav.assembly_label)
                        .width(145.0)
                        .show_ui(ui, |ui| {
                            let _ = ui.selectable_label(true, &st.nav.assembly_label);
                        });
                    assembly_response.response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ComboBox,
                            true,
                            format!("Genome assembly; {}", st.nav.assembly_label),
                        )
                    });
                    let contig_response = egui::ComboBox::from_id_salt("contig-selector")
                        .selected_text(&st.snapshot.contig_name)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for contig in self.assembly.dictionary.iter() {
                                if ui
                                    .selectable_label(
                                        contig.name() == st.snapshot.contig_name,
                                        contig.name(),
                                    )
                                    .clicked()
                                {
                                    st.nav.requested_contig = Some(contig.id());
                                }
                            }
                        });
                    contig_response.response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::ComboBox,
                            true,
                            format!("Chromosome or contig; {}", st.snapshot.contig_name),
                        )
                    });
                    let locus_label = ui.label("Locus");
                    let locus_response = ui
                        .add(
                            egui::TextEdit::singleline(&mut st.nav.locus_editor)
                                .id(egui::Id::new(LOCUS_EDITOR_ID))
                                .desired_width(190.0)
                                .hint_text("chr1:1-100000 chr2:1-100000"),
                        )
                        .labelled_by(locus_label.id);
                    st.nav.locus_search_changed = locus_response.changed();
                    st.nav.search_popup_position = Some(locus_response.rect.left_bottom());
                    if st.commands.focus_locus_requested {
                        locus_response.request_focus();
                    }
                    if locus_response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    {
                        st.nav.navigate_requested = true;
                    }
                    if ui.button("Go").clicked() {
                        st.nav.navigate_requested = true;
                    }
                    Self::panel_selector_ui(ui, st);
                    self.locus_history_and_zoom_ui(ui, st);
                    ui.separator();
                    ui.small(format!(
                        "{:.3} bp/px · {:?}",
                        st.snapshot.bases_per_pixel, st.snapshot.detail
                    ));
                    ui.allocate_ui_with_layout(
                        egui::vec2(18.0, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if !st.commands.reduced_motion
                                && st.snapshot.status_activity == StatusActivity::Busy
                            {
                                ui.spinner();
                            }
                        },
                    );
                });
            });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn panel_selector_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        let panel_response = egui::ComboBox::from_id_salt("view-panel-selector")
            .selected_text(format!("Panels {}", st.nav.panel_controls.len()))
            .width(110.0)
            .show_ui(ui, |ui| {
                for (index, id, label) in &st.nav.panel_controls {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(*id == st.snapshot.active_panel_id, label)
                            .clicked()
                        {
                            st.nav.panel_to_activate = Some(*id);
                        }
                        let move_left = ui
                            .add_enabled(
                                *index > 0,
                                ui::icons::IconButton::new(
                                    ui::icons::AppIcon::MoveLeft,
                                    "Move panel left",
                                )
                                .small(),
                            )
                            .on_hover_text("Move panel left");
                        move_left.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                *index > 0,
                                format!("Move panel {} left", id.get()),
                            )
                        });
                        if move_left.clicked() {
                            st.nav.panel_to_move = Some((*id, index - 1));
                        }
                        let move_right = ui
                            .add_enabled(
                                index + 1 < st.nav.panel_controls.len(),
                                ui::icons::IconButton::new(
                                    ui::icons::AppIcon::MoveRight,
                                    "Move panel right",
                                )
                                .small(),
                            )
                            .on_hover_text("Move panel right");
                        move_right.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                index + 1 < st.nav.panel_controls.len(),
                                format!("Move panel {} right", id.get()),
                            )
                        });
                        if move_right.clicked() {
                            st.nav.panel_to_move = Some((*id, index + 1));
                        }
                        let close = ui
                            .add_enabled(
                                st.nav.panel_controls.len() > 1,
                                ui::icons::IconButton::new(
                                    ui::icons::AppIcon::Close,
                                    "Close panel",
                                )
                                .small(),
                            )
                            .on_hover_text("Close panel");
                        close.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                st.nav.panel_controls.len() > 1,
                                format!("Close panel {}", id.get()),
                            )
                        });
                        if close.clicked() {
                            st.nav.panel_to_close = Some(*id);
                        }
                    });
                }
            });
        panel_response.response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::ComboBox,
                true,
                format!("View panels; {} open", st.nav.panel_controls.len()),
            )
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn locus_history_and_zoom_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        let previous_locus = ui
            .add_enabled(
                self.panels.active().navigation_history.can_go_back(),
                ui::icons::IconButton::new(ui::icons::AppIcon::Previous, "Previous locus"),
            )
            .on_hover_text("Previous locus");
        previous_locus.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                self.panels.active().navigation_history.can_go_back(),
                "Previous locus",
            )
        });
        if previous_locus.clicked() {
            st.nav.navigate_back_requested = true;
        }
        let next_locus = ui
            .add_enabled(
                self.panels.active().navigation_history.can_go_forward(),
                ui::icons::IconButton::new(ui::icons::AppIcon::Next, "Next locus"),
            )
            .on_hover_text("Next locus");
        next_locus.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                self.panels.active().navigation_history.can_go_forward(),
                "Next locus",
            )
        });
        if next_locus.clicked() {
            st.nav.navigate_forward_requested = true;
        }
        let home_locus = ui
            .add(ui::icons::IconButton::new(
                ui::icons::AppIcon::Home,
                "Home locus",
            ))
            .on_hover_text("Home locus");
        home_locus.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Home locus")
        });
        if home_locus.clicked() {
            st.nav.navigate_home_requested = true;
        }
        let zoom_out = ui
            .add(ui::icons::IconButton::new(
                ui::icons::AppIcon::ZoomOut,
                "Zoom out",
            ))
            .on_hover_text("Zoom out");
        zoom_out
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Zoom out"));
        if zoom_out.clicked() {
            st.nav.zoom_out_requested = true;
        }
        let maximum_zoom = (self.panels.active().contig_length.max(1) as f64)
            .log10()
            .max(1.0);
        let mut zoom_level =
            (self.panels.active().contig_length as f64 / st.snapshot.interval.len() as f64).log10();
        let zoom_response = ui
            .add(egui::Slider::new(&mut zoom_level, 0.0..=maximum_zoom).show_value(false))
            .on_hover_text("Genome zoom");
        zoom_response
            .widget_info(|| egui::WidgetInfo::slider(true, zoom_level, "Genome zoom level"));
        if zoom_response.changed() {
            st.nav.requested_span = Some(
                (self.panels.active().contig_length as f64 / 10_f64.powf(zoom_level))
                    .round()
                    .clamp(1.0, self.panels.active().contig_length as f64) as u64,
            );
        }
        let zoom_in = ui
            .add(ui::icons::IconButton::new(
                ui::icons::AppIcon::ZoomIn,
                "Zoom in",
            ))
            .on_hover_text("Zoom in");
        zoom_in
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Zoom in"));
        if zoom_in.clicked() {
            st.nav.zoom_in_requested = true;
        }
    }

    pub(super) fn show_annotation_search_results(ui: &mut egui::Ui, st: &mut ShellFrame) {
        if crate::app::shell_controller::search_popup_visible(
            &st.nav.locus_editor,
            &st.nav.annotation_search_results.query,
            !st.nav.annotation_search_results.hits.is_empty(),
        ) && let Some(position) = st.nav.search_popup_position
        {
            egui::Window::new("Annotation search results")
                .id(egui::Id::new("annotation-search-results"))
                .fixed_pos(position)
                .fixed_size(egui::vec2(460.0, 260.0))
                .collapsible(false)
                .resizable(false)
                .title_bar(false)
                .show(ui.ctx(), |ui| {
                    for hit in st.nav.annotation_search_results.hits.iter().take(12) {
                        let name = annotation_search_hit_primary_label(hit);
                        let identifiers = annotation_search_hit_secondary_identity(hit);
                        let detail = format!(
                            "{}{}:{}-{} · {}",
                            if identifiers.is_empty() {
                                String::new()
                            } else {
                                format!("{identifiers} · ")
                            },
                            hit.record.contig,
                            hit.record.start.saturating_add(1),
                            hit.record.end,
                            hit.track_name
                        );
                        let mut label = egui::text::LayoutJob::default();
                        label.append(
                            name,
                            0.0,
                            egui::TextFormat::simple(
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_gray(30),
                            ),
                        );
                        label.append(
                            &format!("  ·  {detail}"),
                            0.0,
                            egui::TextFormat::simple(
                                egui::FontId::proportional(10.5),
                                egui::Color32::from_gray(105),
                            ),
                        );
                        if ui
                            .add_sized(
                                [ui.available_width(), 26.0],
                                egui::Button::new(egui::WidgetText::from(label)),
                            )
                            .clicked()
                        {
                            st.nav.annotation_hit_requested = Some(hit.clone());
                        }
                    }
                    if st.nav.annotation_search_results.hits.len() > 12 {
                        ui.label(format!(
                            "{} more matches; continue typing to narrow the list",
                            st.nav.annotation_search_results.hits.len() - 12
                        ));
                    }
                    if st.nav.annotation_search_results.index_truncated
                        || st.nav.annotation_search_results.scan_truncated
                    {
                        ui.label(
                            "Search was bounded; a more specific prefix may find omitted matches",
                        );
                    }
                });
        }
    }

    /// Shows the expanded or collapsed Properties panel and returns its rectangle.
    #[allow(clippy::too_many_lines)]
    pub(super) fn show_properties_panel(
        &self,
        ui: &mut egui::Ui,
        st: &mut ShellFrame,
        displayed_properties_expanded: bool,
    ) -> egui::Rect {
        if displayed_properties_expanded {
            egui::Panel::right("properties-panel")
                .default_size(340.0)
                .size_range(280.0..=520.0)
                .resizable(true)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Properties");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(ui::icons::IconButton::new(
                                    ui::icons::AppIcon::ChevronRight,
                                    "Collapse Properties panel",
                                ))
                                .on_hover_text("Collapse Properties panel")
                                .clicked()
                            {
                                st.properties_expanded = false;
                            }
                        });
                    });
                    ui.separator();
                    if let Some(panel_id) = ui::inspectors::show_inspected_item_switcher(self, ui) {
                        st.nav.panel_to_activate = Some(panel_id);
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("properties-panel-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let inspector_actions = ui::inspectors::show_properties(
                                self,
                                ui,
                                &st.snapshot.inspectors,
                                st.inspect.variant_sample_search.clone(),
                            );
                            st.inspect.clear_selection |= inspector_actions.clear_selection;
                            st.inspect.selected_to_top_requested |=
                                inspector_actions.selected_to_top_requested;
                            st.inspect.chain_navigation_requested =
                                inspector_actions.chain_navigation_requested;
                            st.inspect.mate_panel_requested |=
                                inspector_actions.mate_panel_requested;
                            st.inspect.chain_panel_requested =
                                inspector_actions.chain_panel_requested;
                            st.inspect.chain_all_panels_requested |=
                                inspector_actions.chain_all_panels_requested;
                            st.inspect.variant_partner_requested =
                                inspector_actions.variant_partner_requested;
                            st.inspect.variant_endpoint_requested =
                                inspector_actions.variant_endpoint_requested;
                            st.inspect.interaction_endpoint_requested =
                                inspector_actions.interaction_endpoint_requested;
                            st.inspect.variant_pileup_requested =
                                inspector_actions.variant_pileup_requested;
                            st.inspect.variant_modification_requested =
                                inspector_actions.variant_modification_requested;
                            st.inspect.variant_genotypes_export_requested |=
                                inspector_actions.variant_genotypes_export_requested;
                            st.inspect.variant_evidence_export_requested |=
                                inspector_actions.variant_evidence_export_requested;
                            st.inspect.junction_bed_export_requested |=
                                inspector_actions.junction_bed_export_requested;
                            st.inspect.sashimi_png_export_requested |=
                                inspector_actions.sashimi_png_export_requested;
                            st.inspect.sashimi_svg_export_requested |=
                                inspector_actions.sashimi_svg_export_requested;
                            st.lists.add_selected_feature_to_review |=
                                inspector_actions.add_selected_feature_to_review;
                            if inspector_actions.show_all_transcripts_requested.is_some() {
                                st.inspect.show_all_transcripts_requested =
                                    inspector_actions.show_all_transcripts_requested;
                            }
                            st.lists.review_open |= inspector_actions.review_open;
                            st.inspect.variant_sample_search =
                                inspector_actions.variant_sample_search;
                        });
                })
                .response
                .rect
        } else {
            egui::Panel::right("properties-panel-collapsed")
                .exact_size(30.0)
                .resizable(false)
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        if ui
                            .add(ui::icons::IconButton::new(
                                ui::icons::AppIcon::ChevronLeft,
                                "Expand Properties panel",
                            ))
                            .on_hover_text("Expand Properties panel")
                            .clicked()
                        {
                            st.properties_expanded = true;
                        }
                    });
                })
                .response
                .rect
        }
    }
}
