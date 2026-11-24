//! Central workspace canvas: genome context, track chrome, genomic text and scrollbars.

use crate::ui::frame::{ShellFrame, read_panel_scrollbar_rect, remap_genomic_texts_for_layout};
use crate::{Viewer, ui};

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn show_workspace_canvas(
        &self,
        ui: &mut egui::Ui,
        st: &mut ShellFrame,
        properties_rect: egui::Rect,
        displayed_properties_expanded: bool,
    ) {
        egui::CentralPanel::no_frame().show(ui, |ui| {
            let panel_inputs = st
                .panel_contexts
                .iter()
                .map(|(id, name, length, viewport, _)| {
                    let cytobands = if self
                        .session
                        .assembly
                        .cytobands_for(viewport.interval().contig())
                        .next()
                        .is_some()
                    {
                        ui::CytobandAvailability::Present
                    } else {
                        ui::CytobandAvailability::Absent
                    };
                    ui::WorkspacePanelLayoutInput {
                        panel_id: *id,
                        contig_name: name.clone(),
                        contig_length: *length,
                        cytobands,
                    }
                })
                .collect::<Vec<_>>();
            let layout_input = ui::WorkspaceLayoutInput::new(
                ui.max_rect(),
                properties_rect,
                displayed_properties_expanded,
                panel_inputs,
                ui::GenomeContextMetrics::measured(ui),
                self.window.scale_factor() as f32,
                self.egui_context.zoom_factor(),
            );
            let candidate_layout = ui::WorkspaceLayoutSnapshot::compute(
                layout_input.clone(),
                st.layout.previous_workspace_layout.revision,
            );
            let workspace_layout = if candidate_layout == st.layout.previous_workspace_layout {
                candidate_layout
            } else {
                ui::WorkspaceLayoutSnapshot::compute(
                    layout_input,
                    st.layout
                        .previous_workspace_layout
                        .revision
                        .saturating_add(1),
                )
            };
            let geometry = workspace_layout.canvas_geometry();
            st.layout.new_canvas_geometry = geometry;
            ui.painter().rect_filled(
                workspace_layout.genome_context,
                0.0,
                crate::theme::adapt(egui::Color32::from_gray(250)),
            );
            ui.painter().line_segment(
                [
                    workspace_layout.context_gutter.right_top(),
                    workspace_layout.context_gutter.right_bottom(),
                ],
                egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(190))),
            );
            for (index, ((id, name, length, viewport, genome_overview), panel_layout)) in st
                .panel_contexts
                .iter()
                .zip(&workspace_layout.panels)
                .enumerate()
            {
                let interaction = ui::paint_genome_context(
                    ui,
                    &self.session.assembly,
                    name,
                    *length,
                    *viewport,
                    genome_overview,
                    id.get(),
                    panel_layout,
                );
                if interaction.pointer_down
                    || interaction.activate_requested
                    || interaction.clicked_position.is_some()
                    || interaction.show_all
                    || interaction.keyboard_pan_fraction.is_some()
                    || interaction.keyboard_zoom_factor.is_some()
                {
                    st.layout.overview_interaction = interaction;
                    st.layout.overview_panel_id = *id;
                }
                if index > 0 {
                    ui.painter().line_segment(
                        [
                            panel_layout.header.left_top(),
                            panel_layout.header.left_bottom(),
                        ],
                        egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(180))),
                    );
                }
            }
            st.layout.new_workspace_layout = workspace_layout.clone();
            let gutter = ui::paint_track_viewport_chrome_with_sections(
                ui,
                geometry,
                &st.layout.track_frames,
                self.session.panels.annotation_scroll_points,
                0.0,
                // Section chrome belongs to the current pane-layout
                // revision. A scroll display cache can legitimately lag
                // by one frame during a window resize; mixing its old
                // annotation edge with the new read-pane edge stretches
                // the Read data header across the intervening space.
                st.layout.pane_annotation_top,
                st.layout.pane_annotation_bottom,
                st.layout.selected_track,
                &st.layout.selected_annotation_track_ids,
                &self.session.panels.active().selected_read_tracks,
                &st.layout.track_groups,
                ui::TrackSectionChrome {
                    annotation_expanded: self
                        .session
                        .track_presentation
                        .annotation_section_expanded,
                    reads_expanded: self.session.track_presentation.read_section_expanded,
                    annotation_track_count: st.layout.annotation_track_count,
                    read_track_count: st.layout.read_track_count,
                    annotation_content_top: st.layout.pane_layout.annotation.pane_rect.top(),
                    read_content_top: st.layout.pane_layout.reads.pane_rect.top(),
                },
            );
            st.layout.pane_divider_rect = gutter.pane_divider_rect;
            st.tracks.track_ui_actions.extend(gutter.actions);
            let frame_genomic_texts = remap_genomic_texts_for_layout(
                &self.composite_display_list.texts,
                &st.layout.previous_workspace_layout,
                &workspace_layout,
            );
            ui::paint_genomic_text(ui, geometry.genomic, &frame_genomic_texts);
            // Read-level text (base letters, event lengths, junction counts) belongs to the
            // alignment capture the GPU draws, which this frame's commit or second display
            // rebuild may still replace. Reserve its place in the shape order; it is laid out
            // from the drawn capture in `submit_ui_frame`.
            ui.painter()
                .with_clip_rect(geometry.genomic)
                .add(ui::presented_text::presented_text_slot(geometry.genomic));
            self.paint_sashimi_overlays(ui, st, geometry.genomic);
            self.paint_read_hover_tooltip(ui, geometry.genomic);
            self.paint_modification_legends(ui, geometry.genomic);
            self.paint_read_notice_banners(ui, st, geometry.genomic);
            // Labels of lanes drawn by the experimental tile pipeline; none under `Legacy`.
            let tile_label_texts = self.track_tiles.label_texts().cloned().collect::<Vec<_>>();
            if !tile_label_texts.is_empty() {
                ui::paint_genomic_text(ui, geometry.genomic, &tile_label_texts);
            }
            for ((_, panel_id, locus), panel_layout) in
                st.nav.panel_controls.iter().zip(&workspace_layout.panels)
            {
                let canvas_accessibility = ui.interact(
                    panel_layout.canvas,
                    egui::Id::new(("genomic-canvas-accessibility", panel_id)),
                    egui::Sense::focusable_noninteractive(),
                );
                st.layout.genomic_canvas_layer_id = Some(canvas_accessibility.layer_id);
                let canvas_label = if *panel_id == st.snapshot.active_panel_id {
                    st.snapshot.selection_summary.as_ref().map_or_else(
                        || format!("Genome tracks at {locus}; no evidence selected"),
                        |selection| format!("Genome tracks at {locus}; {selection}"),
                    )
                } else {
                    format!("Genome tracks at {locus}; inactive locus panel")
                };
                canvas_accessibility.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Panel, true, &canvas_label)
                });
            }
            if st.nav.panel_controls.len() > 1 {
                for index in 1..st.nav.panel_controls.len() {
                    let x = workspace_layout.panels[index].track_column.left();
                    ui.painter().line_segment(
                        [
                            egui::pos2(x, geometry.viewport.top()),
                            egui::pos2(x, geometry.viewport.bottom()),
                        ],
                        egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(175))),
                    );
                }
                if let Some(active_index) = st
                    .nav
                    .panel_controls
                    .iter()
                    .position(|(_, id, _)| *id == st.snapshot.active_panel_id)
                {
                    ui.painter().rect_stroke(
                        workspace_layout.panels[active_index].track_column,
                        0.0,
                        egui::Stroke::new(
                            1.5,
                            crate::theme::adapt(egui::Color32::from_rgb(45, 92, 175)),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            Self::paint_workspace_scrollbars(ui, st, &workspace_layout);
        });
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn paint_workspace_scrollbars(
        ui: &mut egui::Ui,
        st: &mut ShellFrame,
        workspace_layout: &ui::WorkspaceLayoutSnapshot,
    ) {
        for workspace in &mut st.layout.workspace_scrollbars {
            let full = workspace_layout.panels[workspace.index].scrollbar;
            let split_y = workspace
                .annotation_clip_bottom
                .clamp(full.top(), full.bottom());
            let annotation_top = workspace.annotation_clip_top.clamp(full.top(), split_y);
            let annotation_rect = egui::Rect::from_min_max(
                egui::pos2(full.left(), annotation_top),
                egui::pos2(full.right(), split_y),
            );
            if annotation_rect.height() > 1.0 {
                workspace.annotation_interaction = ui::paint_workspace_annotation_scrollbar(
                    ui,
                    annotation_rect,
                    workspace.panel_id,
                    workspace.annotation_maximum,
                    &mut workspace.annotation_offset,
                    workspace.annotation_overview.as_ref(),
                );
            }
            ui.painter().line_segment(
                [
                    egui::pos2(full.left(), split_y),
                    egui::pos2(full.right(), split_y),
                ],
                egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(125))),
            );
            let read_top = workspace.read_clip_top.clamp(split_y, full.bottom());
            let read_scrollbar_left =
                if workspace.read_stack_maximum > f32::EPSILON && full.bottom() - read_top > 1.0 {
                    let stack_rect = egui::Rect::from_min_max(
                        egui::pos2(full.left(), read_top),
                        egui::pos2(full.center().x, full.bottom()),
                    );
                    workspace.read_stack_interaction = ui::paint_read_stack_scrollbar(
                        ui,
                        stack_rect,
                        workspace.panel_id,
                        workspace.read_stack_maximum,
                        &mut workspace.read_stack_offset,
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(full.center().x, read_top),
                            egui::pos2(full.center().x, full.bottom()),
                        ],
                        egui::Stroke::new(0.75, crate::theme::adapt(egui::Color32::from_gray(150))),
                    );
                    full.center().x
                } else {
                    full.left()
                };
            for scrollbar in &mut workspace.read_panels {
                let rect =
                    read_panel_scrollbar_rect(full, read_scrollbar_left, scrollbar.allocation);
                scrollbar.interaction = ui::paint_read_panel_scrollbar(
                    ui,
                    rect,
                    workspace.panel_id,
                    scrollbar.track_id,
                    scrollbar.maximum,
                    &mut scrollbar.offset,
                    scrollbar.density,
                    rect.height().max(1.0),
                    scrollbar.preview_fraction,
                    Some(&scrollbar.overview),
                    scrollbar.provisional,
                );
                ui.painter().line_segment(
                    [
                        egui::pos2(rect.left(), rect.top()),
                        egui::pos2(rect.right(), rect.top()),
                    ],
                    egui::Stroke::new(1.0, crate::theme::adapt(egui::Color32::from_gray(125))),
                );
            }
        }
    }
}
