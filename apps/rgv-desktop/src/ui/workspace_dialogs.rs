use super::super::*;

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct WorkspaceDialogDraft {
    pub(crate) metadata_open: bool,
    pub(crate) regions_open: bool,
    pub(crate) review_open: bool,
    pub(crate) regions: Vec<RegionOfInterest>,
    pub(crate) review_items: Vec<ReviewItem>,
    pub(crate) review_status_filter: Option<ReviewStatus>,
    pub(crate) add_current_region: bool,
    pub(crate) add_current_review: bool,
    pub(crate) region_import_requested: bool,
    pub(crate) region_export_requested: bool,
    pub(crate) review_import_requested: bool,
    pub(crate) review_export_requested: bool,
    pub(crate) previous_review_requested: bool,
    pub(crate) next_review_requested: bool,
    pub(crate) managed_locus_requested: Option<String>,
    pub(crate) region_to_delete: Option<u64>,
    pub(crate) review_to_delete: Option<u64>,
    pub(crate) region_to_relocate: Option<u64>,
    pub(crate) review_to_relocate: Option<u64>,
    pub(crate) sample_metadata: Option<SampleMetadataTable>,
    pub(crate) metadata_filter_column: usize,
    pub(crate) metadata_filter_value: Option<String>,
    pub(crate) metadata_selected_tracks: BTreeSet<TrackId>,
    pub(crate) metadata_bulk_height: f32,
    pub(crate) metadata_remove_armed: bool,
    pub(crate) metadata_import_requested: bool,
    pub(crate) metadata_remove_requested: bool,
    pub(crate) metadata_select_requested: bool,
    pub(crate) metadata_bulk_visibility: Option<bool>,
    pub(crate) metadata_bulk_collapsed: Option<bool>,
    pub(crate) metadata_bulk_height_requested: bool,
    pub(crate) metadata_bulk_rename_requested: bool,
    pub(crate) metadata_bulk_alignment_settings_requested: bool,
    pub(crate) metadata_bulk_auxiliary_appearance_requested: bool,
    pub(crate) metadata_bulk_quantitative_autoscale_requested: bool,
    pub(crate) metadata_bulk_quantitative_overlay_requested: bool,
    pub(crate) metadata_bulk_quantitative_separate_requested: bool,
    pub(crate) metadata_bulk_remove_requested: bool,
    pub(crate) metadata_track_rows: Vec<MetadataTrackRow>,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn show_workspace_dialogs(ui: &mut egui::Ui, draft: &mut WorkspaceDialogDraft) {
    if draft.metadata_open {
        egui::Window::new("Sample metadata")
                    .open(&mut draft.metadata_open)
                    .default_width(760.0)
                    .resizable(true)
                    .show(ui.ctx(), |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Import attributes TSV…").clicked() {
                                draft.metadata_import_requested = true;
                            }
                            if ui
                                .add_enabled(
                                    draft.sample_metadata.is_some(),
                                    egui::Button::new("Remove metadata"),
                                )
                                .clicked()
                            {
                                draft.metadata_remove_requested = true;
                            }
                            if ui
                                .add_enabled(
                                    !draft.metadata_selected_tracks.is_empty(),
                                    egui::Button::new("Clear track selection"),
                                )
                                .clicked()
                            {
                                draft.metadata_selected_tracks.clear();
                            }
                        });
                        let Some(table) = &draft.sample_metadata else {
                            ui.label(
                                "Import a tab-separated table whose first column is the stable sample key.",
                            );
                            return;
                        };
                        ui.label(format!(
                            "{} samples · {} attributes · key column {}",
                            table.rows.len(),
                            table.columns.len(),
                            table.key_column
                        ));
                        if !table.columns.is_empty() {
                            draft.metadata_filter_column =
                                draft.metadata_filter_column.min(table.columns.len() - 1);
                            let values = table.distinct_values(draft.metadata_filter_column);
                            if draft.metadata_filter_value
                                .as_deref()
                                .is_none_or(|selected| !values.contains(&selected))
                            {
                                draft.metadata_filter_value = values.first().map(|value| (*value).to_owned());
                            }
                            ui.horizontal(|ui| {
                                ui.label("Select tracks where");
                                let column = egui::ComboBox::from_id_salt("metadata-filter-column")
                                    .selected_text(&table.columns[draft.metadata_filter_column])
                                    .show_ui(ui, |ui| {
                                        for (index, column) in table.columns.iter().enumerate() {
                                            if ui
                                                .selectable_value(
                                                    &mut draft.metadata_filter_column,
                                                    index,
                                                    column,
                                                )
                                                .changed()
                                            {
                                                draft.metadata_filter_value = None;
                                            }
                                        }
                                    });
                                column.response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::ComboBox,
                                        true,
                                        format!(
                                            "Sample attribute column; {}",
                                            table.columns[draft.metadata_filter_column]
                                        ),
                                    )
                                });
                                let values = table.distinct_values(draft.metadata_filter_column);
                                let value = egui::ComboBox::from_id_salt("metadata-filter-value")
                                    .selected_text(
                                        draft.metadata_filter_value.as_deref().unwrap_or("No value"),
                                    )
                                    .show_ui(ui, |ui| {
                                        for value in
                                            values.iter().take(MAX_METADATA_FILTER_VALUES)
                                        {
                                            ui.selectable_value(
                                                &mut draft.metadata_filter_value,
                                                Some((*value).to_owned()),
                                                *value,
                                            );
                                        }
                                    });
                                value.response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::ComboBox,
                                        true,
                                        format!(
                                            "Sample attribute value; {}",
                                            draft.metadata_filter_value.as_deref().unwrap_or("No value")
                                        ),
                                    )
                                });
                                if ui
                                    .add_enabled(
                                        draft.metadata_filter_value.is_some(),
                                        egui::Button::new("Select matching tracks"),
                                    )
                                    .clicked()
                                {
                                    draft.metadata_select_requested = true;
                                }
                            });
                            if values.len() > MAX_METADATA_FILTER_VALUES {
                                ui.colored_label(
                                    egui::Color32::from_rgb(150, 88, 20),
                                    format!(
                                        "Showing the first {MAX_METADATA_FILTER_VALUES} of {} distinct filter values",
                                        values.len()
                                    ),
                                );
                            }
                        }
                        ui.separator();
                        ui.strong("Track/sample associations");
                        for (id, name, sample_key, _) in &draft.metadata_track_rows {
                            ui.horizontal(|ui| {
                                let mut selected = draft.metadata_selected_tracks.contains(id);
                                let selection = ui.checkbox(&mut selected, "");
                                selection.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::Checkbox,
                                        true,
                                        selected,
                                        format!("Select {name} track"),
                                    )
                                });
                                if selection.changed() {
                                    if selected {
                                        draft.metadata_selected_tracks.insert(*id);
                                    } else {
                                        draft.metadata_selected_tracks.remove(id);
                                    }
                                }
                                ui.label(name);
                                ui.monospace(sample_key.as_deref().unwrap_or("Unassigned"));
                                if sample_key
                                    .as_ref()
                                    .is_some_and(|key| table.row(key).is_none())
                                {
                                    ui.colored_label(
                                        egui::Color32::DARK_RED,
                                        "unresolved sample key",
                                    );
                                }
                            });
                        }
                        if !draft.metadata_selected_tracks.is_empty() {
                            let selected_alignment_count = draft.metadata_track_rows
                                .iter()
                                .filter(|(id, _, _, kind)| {
                                    *kind == TrackKind::Alignment
                                        && draft.metadata_selected_tracks.contains(id)
                                })
                                .count();
                            let selected_quantitative_count = draft.metadata_track_rows
                                .iter()
                                .filter(|(id, _, _, kind)| {
                                    *kind == TrackKind::Quantitative
                                        && draft.metadata_selected_tracks.contains(id)
                                })
                                .count();
                            let selected_auxiliary_kinds = draft.metadata_track_rows
                                .iter()
                                .filter(|(id, _, _, kind)| {
                                    *kind != TrackKind::Alignment
                                        && draft.metadata_selected_tracks.contains(id)
                                })
                                .map(|(_, _, _, kind)| *kind)
                                .collect::<Vec<_>>();
                            let homogeneous_auxiliary = selected_auxiliary_kinds
                                .first()
                                .copied()
                                .filter(|kind| {
                                    selected_auxiliary_kinds.len() > 1
                                        && selected_auxiliary_kinds
                                            .iter()
                                            .all(|candidate| candidate == kind)
                                        && matches!(
                                            kind,
                                            TrackKind::Variant
                                                | TrackKind::Mutation
                                                | TrackKind::Gwas
                                                | TrackKind::Quantitative
                                                | TrackKind::CopyNumber
                                                | TrackKind::Interaction
                                        )
                                });
                            ui.separator();
                            ui.strong(format!(
                                "Bulk actions · {} selected tracks",
                                draft.metadata_selected_tracks.len()
                            ));
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Show").clicked() {
                                    draft.metadata_bulk_visibility = Some(true);
                                }
                                if ui.button("Hide").clicked() {
                                    draft.metadata_bulk_visibility = Some(false);
                                }
                                if ui.button("Expand").clicked() {
                                    draft.metadata_bulk_collapsed = Some(false);
                                }
                                if ui.button("Collapse").clicked() {
                                    draft.metadata_bulk_collapsed = Some(true);
                                }
                                if ui.button("Rename to sample keys").clicked() {
                                    draft.metadata_bulk_rename_requested = true;
                                }
                            });
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Slider::new(&mut draft.metadata_bulk_height, 40.0..=1_200.0)
                                        .text("Height"),
                                );
                                if ui.button("Apply height").clicked() {
                                    draft.metadata_bulk_height_requested = true;
                                }
                            });
                            if selected_alignment_count > 1
                                && ui
                                    .button("Copy all settings from first selected alignment")
                                    .on_hover_text(
                                        "Explicitly copies grouping, colours, filters, subtracks, and record limits to the other selected alignment tracks.",
                                    )
                                    .clicked()
                            {
                                draft.metadata_bulk_alignment_settings_requested = true;
                            }
                            if let Some(kind) = homogeneous_auxiliary
                                && ui
                                    .button(format!(
                                        "Copy compatible {kind:?} appearance from first selected"
                                    ))
                                    .clicked()
                            {
                                draft.metadata_bulk_auxiliary_appearance_requested = true;
                            }
                            if selected_quantitative_count > 0 {
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Autoscale quantitative tracks").clicked() {
                                        draft.metadata_bulk_quantitative_autoscale_requested = true;
                                    }
                                    if ui
                                        .add_enabled(
                                            selected_quantitative_count > 1,
                                            egui::Button::new("Overlay quantitative tracks"),
                                        )
                                        .clicked()
                                    {
                                        draft.metadata_bulk_quantitative_overlay_requested = true;
                                    }
                                    if ui.button("Separate quantitative tracks").clicked() {
                                        draft.metadata_bulk_quantitative_separate_requested = true;
                                    }
                                });
                            }
                            ui.horizontal(|ui| {
                                if draft.metadata_remove_armed {
                                    if ui
                                        .button("Confirm remove selected tracks")
                                        .clicked()
                                    {
                                        draft.metadata_bulk_remove_requested = true;
                                        draft.metadata_remove_armed = false;
                                    }
                                    if ui.button("Cancel").clicked() {
                                        draft.metadata_remove_armed = false;
                                    }
                                } else if ui
                                    .button("Arm removal…")
                                    .on_hover_text(
                                        "Requires a second explicit confirmation before tracks are removed.",
                                    )
                                    .clicked()
                                {
                                    draft.metadata_remove_armed = true;
                                }
                            });
                        }
                        ui.separator();
                        let visible_columns =
                            table.columns.len().min(MAX_METADATA_VISIBLE_COLUMNS);
                        ui.horizontal(|ui| {
                            ui.strong(&table.key_column);
                            for column in table.columns.iter().take(visible_columns) {
                                ui.strong(column);
                            }
                        });
                        egui::ScrollArea::vertical().max_height(360.0).show_rows(
                            ui,
                            20.0,
                            table.rows.len(),
                            |ui, rows| {
                                for row_index in rows {
                                    let row = &table.rows[row_index];
                                    ui.horizontal(|ui| {
                                        ui.monospace(&row.sample_key);
                                        for value in row.values.iter().take(visible_columns) {
                                            ui.label(bounded_preview(value, 80));
                                        }
                                    });
                                }
                            },
                        );
                        if table.columns.len() > visible_columns {
                            ui.label(format!(
                                "{} additional columns are retained in the session but hidden from this bounded preview",
                                table.columns.len() - visible_columns
                            ));
                        }
                    });
    }
    if draft.regions_open {
        egui::Window::new("Regions of interest")
            .open(&mut draft.regions_open)
            .default_width(520.0)
            .vscroll(true)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            draft.regions.len() < MAXIMUM_REGIONS_OF_INTEREST,
                            egui::Button::new("Add current view"),
                        )
                        .clicked()
                    {
                        draft.add_current_region = true;
                    }
                    if ui.button("Import BED…").clicked() {
                        draft.region_import_requested = true;
                    }
                    if ui
                        .add_enabled(!draft.regions.is_empty(), egui::Button::new("Export BED…"))
                        .clicked()
                    {
                        draft.region_export_requested = true;
                    }
                    ui.label(format!(
                        "{} / {MAXIMUM_REGIONS_OF_INTEREST}",
                        draft.regions.len()
                    ));
                });
                for region in &mut draft.regions {
                    ui.separator();
                    ui.horizontal(|ui| {
                        let name_label = ui.label("Name");
                        ui.text_edit_singleline(&mut region.name)
                            .labelled_by(name_label.id);
                        let mut color = egui::Color32::from_rgba_unmultiplied(
                            region.color[0],
                            region.color[1],
                            region.color[2],
                            region.color[3],
                        );
                        let color_button = ui.color_edit_button_srgba(&mut color);
                        color_button.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ColorButton,
                                true,
                                format!("Colour for region {}", region.name),
                            )
                        });
                        if color_button.changed() {
                            region.color = color.to_array();
                        }
                        if ui.button("Go").clicked() {
                            draft.managed_locus_requested = Some(format!(
                                "{}:{}-{}",
                                region.locus.contig,
                                region.locus.start.saturating_add(1),
                                region.locus.end
                            ));
                        }
                        if ui.button("Use current view").clicked() {
                            draft.region_to_relocate = Some(region.id);
                        }
                        if ui.button("Delete").clicked() {
                            draft.region_to_delete = Some(region.id);
                        }
                    });
                    ui.label(format!(
                        "{}:{}-{}",
                        region.locus.contig,
                        region.locus.start.saturating_add(1),
                        region.locus.end
                    ));
                }
            });
    }
    if draft.review_open {
        egui::Window::new("Review queue")
            .open(&mut draft.review_open)
            .default_width(620.0)
            .vscroll(true)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            draft.review_items.len() < MAXIMUM_REVIEW_ITEMS,
                            egui::Button::new("Add current locus"),
                        )
                        .clicked()
                    {
                        draft.add_current_review = true;
                    }
                    if ui
                        .add_enabled(
                            !draft.review_items.is_empty(),
                            egui::Button::new("Previous"),
                        )
                        .clicked()
                    {
                        draft.previous_review_requested = true;
                    }
                    if ui
                        .add_enabled(!draft.review_items.is_empty(), egui::Button::new("Next"))
                        .clicked()
                    {
                        draft.next_review_requested = true;
                    }
                    if ui.button("Import TSV…").clicked() {
                        draft.review_import_requested = true;
                    }
                    if ui
                        .add_enabled(
                            !draft.review_items.is_empty(),
                            egui::Button::new("Export TSV…"),
                        )
                        .clicked()
                    {
                        draft.review_export_requested = true;
                    }
                    egui::ComboBox::from_label("Status filter")
                        .selected_text(draft.review_status_filter.map_or("All", |status| {
                            match status {
                                ReviewStatus::Pending => "Pending",
                                ReviewStatus::InProgress => "In progress",
                                ReviewStatus::Complete => "Complete",
                                ReviewStatus::Skipped => "Skipped",
                            }
                        }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut draft.review_status_filter, None, "All");
                            for (status, label) in [
                                (ReviewStatus::Pending, "Pending"),
                                (ReviewStatus::InProgress, "In progress"),
                                (ReviewStatus::Complete, "Complete"),
                                (ReviewStatus::Skipped, "Skipped"),
                            ] {
                                ui.selectable_value(
                                    &mut draft.review_status_filter,
                                    Some(status),
                                    label,
                                );
                            }
                        });
                });
                for item in draft.review_items.iter_mut().filter(|item| {
                    draft
                        .review_status_filter
                        .is_none_or(|status| item.status == status)
                }) {
                    ui.separator();
                    ui.horizontal(|ui| {
                        let label = ui.label("Label");
                        ui.text_edit_singleline(&mut item.label)
                            .labelled_by(label.id);
                        let status = egui::ComboBox::from_id_salt(("review-status", item.id))
                            .selected_text(review_status_label(item.status))
                            .show_ui(ui, |ui| {
                                for status in [
                                    ReviewStatus::Pending,
                                    ReviewStatus::InProgress,
                                    ReviewStatus::Complete,
                                    ReviewStatus::Skipped,
                                ] {
                                    ui.selectable_value(
                                        &mut item.status,
                                        status,
                                        review_status_label(status),
                                    );
                                }
                            });
                        status.response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                true,
                                format!("Review status; {}", review_status_label(item.status)),
                            )
                        });
                        if ui.button("Go").clicked() {
                            draft.managed_locus_requested = Some(format!(
                                "{}:{}-{}",
                                item.locus.contig,
                                item.locus.start.saturating_add(1),
                                item.locus.end
                            ));
                        }
                        if ui.button("Use current view").clicked() {
                            draft.review_to_relocate = Some(item.id);
                        }
                        if ui.button("Delete").clicked() {
                            draft.review_to_delete = Some(item.id);
                        }
                    });
                    ui.monospace(session_locus_text(&item.locus));
                    ui.horizontal(|ui| {
                        let label = ui.label("Assigned to");
                        let assignment = item.assignment.get_or_insert_default();
                        ui.text_edit_singleline(assignment).labelled_by(label.id);
                        if assignment.trim().is_empty() {
                            item.assignment = None;
                        }
                    });
                    let notes_label = ui.label("Notes");
                    ui.text_edit_multiline(&mut item.notes)
                        .labelled_by(notes_label.id);
                }
                ui.separator();
                ui.label(format!(
                    "{} / {MAXIMUM_REVIEW_ITEMS} review items",
                    draft.review_items.len()
                ));
            });
    }
}
