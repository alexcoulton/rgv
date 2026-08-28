use super::super::*;

#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
pub(crate) struct InspectorUiActions {
    pub(crate) clear_selection: bool,
    pub(crate) selected_to_top_requested: bool,
    pub(crate) chain_navigation_requested: Option<usize>,
    pub(crate) mate_panel_requested: bool,
    pub(crate) chain_panel_requested: Option<usize>,
    pub(crate) chain_all_panels_requested: bool,
    pub(crate) variant_partner_requested: Option<(String, u64)>,
    pub(crate) variant_endpoint_requested: Option<(String, u64)>,
    pub(crate) interaction_endpoint_requested: Option<(String, u64)>,
    pub(crate) variant_pileup_requested: Option<(TrackId, u64)>,
    pub(crate) variant_modification_requested: Option<(TrackId, u64)>,
    pub(crate) variant_genotypes_export_requested: bool,
    pub(crate) variant_evidence_export_requested: bool,
    pub(crate) junction_bed_export_requested: bool,
    pub(crate) sashimi_png_export_requested: bool,
    pub(crate) sashimi_svg_export_requested: bool,
    pub(crate) add_selected_feature_to_review: bool,
    pub(crate) review_open: bool,
    pub(crate) variant_sample_search: String,
}

fn break_long_monospace_runs(value: &str) -> String {
    const MAXIMUM_UNBROKEN_CHARACTERS: usize = 24;
    let mut display = String::with_capacity(value.len());
    let mut unbroken = 0_usize;
    for character in value.chars() {
        if character.is_whitespace() {
            unbroken = 0;
        } else if unbroken == MAXIMUM_UNBROKEN_CHARACTERS {
            display.push('\n');
            unbroken = 0;
        }
        display.push(character);
        if !character.is_whitespace() {
            unbroken += 1;
        }
    }
    display
}

fn wrapped_monospace(ui: &mut egui::Ui, value: &str) -> egui::Response {
    ui.add(
        egui::Label::new(egui::RichText::new(break_long_monospace_runs(value)).monospace()).wrap(),
    )
}

#[derive(Default)]
struct InspectorDocumentResponse {
    actions: Vec<InspectorActionId>,
    section_ids: Vec<egui::Id>,
}

fn inspector_tone_color(ui: &egui::Ui, tone: InspectorTone) -> egui::Color32 {
    match tone {
        InspectorTone::Neutral => ui.visuals().weak_text_color(),
        InspectorTone::Informative if ui.visuals().dark_mode => {
            egui::Color32::from_rgb(125, 177, 255)
        }
        InspectorTone::Positive if ui.visuals().dark_mode => egui::Color32::from_rgb(105, 205, 142),
        InspectorTone::Warning if ui.visuals().dark_mode => egui::Color32::from_rgb(245, 177, 88),
        InspectorTone::Informative => egui::Color32::from_rgb(45, 92, 175),
        InspectorTone::Positive => egui::Color32::from_rgb(32, 112, 72),
        InspectorTone::Warning => egui::Color32::from_rgb(150, 88, 20),
    }
}

fn show_inspector_badge(ui: &mut egui::Ui, badge: &InspectorBadge) {
    let color = inspector_tone_color(ui, badge.tone);
    egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(egui::Stroke::new(1.0, color.gamma_multiply(0.55)))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(&badge.label).color(color).size(11.0));
        });
}

fn show_inspector_field_label(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let caption_color = if ui.visuals().dark_mode {
        egui::Color32::from_rgb(184, 199, 218)
    } else {
        egui::Color32::from_rgb(58, 75, 98)
    };
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .strong()
                .size(11.0)
                .color(caption_color),
        )
        .wrap(),
    )
}

fn show_inspector_provenance(ui: &mut egui::Ui, provenance: &InspectorProvenance) {
    let message = match provenance {
        InspectorProvenance::Source | InspectorProvenance::Parsed => None,
        InspectorProvenance::Inferred(reason) => Some(format!("Inferred · {reason}")),
        InspectorProvenance::Aggregated(reason) => Some(format!("Aggregated · {reason}")),
        InspectorProvenance::Truncated {
            displayed,
            total,
            reason,
        } => Some(total.map_or_else(
            || format!("Showing a bounded {displayed}-unit value · {reason}"),
            |total| {
                format!(
                    "Showing {displayed} of {total} · {reason}; copy retains the complete bounded value"
                )
            },
        )),
        InspectorProvenance::Missing(reason) => Some(format!("Unavailable · {reason}")),
    };
    if let Some(message) = message {
        ui.label(
            egui::RichText::new(message)
                .small()
                .color(ui.visuals().weak_text_color()),
        );
    }
}

fn show_inspector_table(
    ui: &mut egui::Ui,
    table: &InspectorTable,
    search: &mut Option<&mut String>,
) {
    if let Some(label) = &table.search_label
        && let Some(search) = search.as_deref_mut()
    {
        let search_label = ui.label("Search");
        ui.add(
            egui::TextEdit::singleline(search)
                .hint_text(label)
                .desired_width(ui.available_width().min(260.0)),
        )
        .labelled_by(search_label.id);
    }
    if !table.columns.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for column in &table.columns {
                ui.strong(column);
            }
        });
        ui.separator();
    }
    let query = if table.search_label.is_some() {
        search
            .as_deref()
            .map_or_else(String::new, |search| search.trim().to_ascii_lowercase())
    } else {
        String::new()
    };
    let matching_rows = inspector_table_matching_rows(table, &query);
    if matching_rows.is_empty() {
        ui.weak(if query.is_empty() {
            "No rows are available."
        } else {
            "No rows match this search."
        });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("inspector-table")
            .max_height(280.0)
            .show_rows(ui, 20.0, matching_rows.len(), |ui, rows| {
                for row in rows {
                    ui.horizontal_wrapped(|ui| {
                        for value in &table.rows[matching_rows[row]] {
                            ui.monospace(value);
                        }
                    });
                }
            });
    }
    if table.omitted_rows > 0 {
        ui.colored_label(
            inspector_tone_color(ui, InspectorTone::Warning),
            format!(
                "{} of {} rows are omitted from the bounded presentation.",
                table.omitted_rows, table.total_rows
            ),
        );
    }
}

fn inspector_table_matching_rows(table: &InspectorTable, query: &str) -> Vec<usize> {
    table
        .rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            (query.is_empty()
                || row
                    .iter()
                    .any(|value| value.to_ascii_lowercase().contains(query)))
            .then_some(index)
        })
        .collect()
}

fn show_inspector_value(
    ui: &mut egui::Ui,
    field: &InspectorField,
    table_search: &mut Option<&mut String>,
    actions: &mut Vec<InspectorActionId>,
) {
    match &field.value {
        InspectorValue::Tags(tags) => {
            ui.horizontal_wrapped(|ui| {
                for tag in tags {
                    show_inspector_badge(
                        ui,
                        &InspectorBadge {
                            label: tag.clone(),
                            tone: InspectorTone::Neutral,
                        },
                    );
                }
            });
        }
        InspectorValue::List(values) => {
            for value in values {
                wrapped_monospace(ui, value);
            }
        }
        InspectorValue::Table(table) => show_inspector_table(ui, table, table_search),
        InspectorValue::SupplementaryChainDiagram(chain) => {
            if let Some(index) = paint_supplementary_chain_diagram(ui, chain) {
                actions.push(InspectorActionId::NavigateChain(index));
            }
        }
        InspectorValue::Status { label, tone } => {
            ui.colored_label(inspector_tone_color(ui, *tone), label);
        }
        InspectorValue::Missing {
            display,
            consequential,
        } => {
            if *consequential {
                ui.colored_label(inspector_tone_color(ui, InspectorTone::Warning), display);
            } else {
                ui.weak(display);
            }
        }
        value if value.uses_monospace() => {
            wrapped_monospace(ui, &value.display_text());
        }
        value => {
            ui.add(egui::Label::new(value.display_text()).wrap());
        }
    }
    show_inspector_provenance(ui, &field.provenance);
    if matches!(
        field.value,
        InspectorValue::Identifier(_)
            | InspectorValue::GenomicLocation(_)
            | InspectorValue::Sequence { .. }
            | InspectorValue::RawKeyValue { .. }
    ) && ui
        .small_button("Copy")
        .on_hover_text(format!("Copy complete {} value", field.label))
        .clicked()
    {
        ui.ctx().copy_text(field.complete_copy_value());
    }
}

fn show_inspector_field(
    ui: &mut egui::Ui,
    field: &InspectorField,
    table_search: &mut Option<&mut String>,
    actions: &mut Vec<InspectorActionId>,
) {
    let narrow = ui.available_width() < 360.0;
    if narrow || matches!(field.value, InspectorValue::Table(_)) {
        show_inspector_field_label(ui, &field.label);
        show_inspector_value(ui, field, table_search, actions);
    } else {
        let label_width = (ui.available_width() * 0.34).clamp(96.0, 168.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(label_width, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    show_inspector_field_label(ui, &field.label);
                },
            );
            ui.add_space(8.0);
            ui.vertical(|ui| show_inspector_value(ui, field, table_search, actions));
        });
    }
    ui.add_space(3.0);
}

#[allow(clippy::too_many_lines)]
fn show_inspector_document(
    ui: &mut egui::Ui,
    document: &InspectorDocument,
    mut table_search: Option<&mut String>,
) -> InspectorDocumentResponse {
    let mut response = InspectorDocumentResponse::default();
    ui.push_id(&document.identity, |ui| {
        ui.heading(&document.header.title);
        if let Some(subtitle) = &document.header.subtitle {
            ui.label(egui::RichText::new(subtitle).size(14.0));
        }
        if !document.header.badges.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for badge in &document.header.badges {
                    show_inspector_badge(ui, badge);
                }
            });
        }
        if let Some(location) = &document.header.location {
            ui.monospace(&location.display);
        }
        if let Some(source) = &document.header.source {
            ui.label(
                egui::RichText::new(format!("Source · {source}"))
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
        ui.add_space(8.0);

        for notice in &document.notices {
            ui.push_id(&notice.id, |ui| {
                ui.colored_label(inspector_tone_color(ui, notice.tone), &notice.message);
            });
        }

        let mut sections = document.sections.iter().collect::<Vec<_>>();
        sections.sort_by_key(|section| section.priority);
        for section in sections {
            if section.fields.is_empty() {
                continue;
            }
            let header = egui::CollapsingHeader::new(&section.title)
                .id_salt(&section.id)
                .default_open(section.initially_expanded);
            let collapsing = header.show(ui, |ui| {
                for field in &section.fields {
                    ui.push_id(&field.id, |ui| {
                        show_inspector_field(ui, field, &mut table_search, &mut response.actions);
                    });
                }
            });
            response.section_ids.push(collapsing.header_response.id);
            ui.separator();
        }

        if !document.raw_fields.is_empty() {
            egui::CollapsingHeader::new(format!("Raw attributes ({})", document.raw_fields.len()))
                .id_salt("raw-attributes")
                .default_open(false)
                .show(ui, |ui| {
                    for field in &document.raw_fields {
                        ui.push_id(&field.id, |ui| {
                            show_inspector_field(
                                ui,
                                field,
                                &mut table_search,
                                &mut response.actions,
                            );
                        });
                    }
                });
            ui.separator();
        }

        let mut ordinary_actions = document
            .actions
            .iter()
            .filter(|action| action.emphasis != InspectorActionEmphasis::Low)
            .collect::<Vec<_>>();
        ordinary_actions.sort_by_key(|action| match action.emphasis {
            InspectorActionEmphasis::Primary => 0,
            InspectorActionEmphasis::Normal => 1,
            InspectorActionEmphasis::Low => 2,
        });
        ui.horizontal_wrapped(|ui| {
            for action in ordinary_actions {
                let button = ui.add_enabled(action.enabled, egui::Button::new(&action.label));
                let button = action
                    .hover_text
                    .as_deref()
                    .map_or(button.clone(), |hover| button.on_hover_text(hover));
                let button = button.on_disabled_hover_text(
                    action.disabled_reason.as_deref().unwrap_or("Unavailable"),
                );
                if button.clicked() {
                    if action.id == InspectorActionId::CopyDetails {
                        ui.ctx().copy_text(document.copy_details_text());
                    } else {
                        response.actions.push(action.id.clone());
                    }
                }
            }
        });
        for action in document
            .actions
            .iter()
            .filter(|action| action.emphasis == InspectorActionEmphasis::Low)
        {
            let button = ui.add_enabled(
                action.enabled,
                egui::Button::new(
                    egui::RichText::new(&action.label).color(ui.visuals().weak_text_color()),
                )
                .frame(false),
            );
            let button = action
                .hover_text
                .as_deref()
                .map_or(button.clone(), |hover| button.on_hover_text(hover));
            if button
                .on_disabled_hover_text(action.disabled_reason.as_deref().unwrap_or("Unavailable"))
                .clicked()
            {
                response.actions.push(action.id.clone());
            }
        }
    });
    response
}

#[allow(clippy::too_many_lines)]
pub(crate) fn show_properties(
    viewer: &Viewer,
    ui: &mut egui::Ui,
    mut variant_sample_search: String,
) -> InspectorUiActions {
    let inspector = viewer.inspector_snapshot();
    let pileup_inspector = viewer.pileup_inspector_snapshot();
    let modification_inspector = viewer.modification_inspector_snapshot();
    let junction_inspector = viewer.junction_inspector_snapshot();
    let feature_inspector = viewer.feature_inspector_snapshot();
    let variant_inspector = viewer.variant_inspector_snapshot();
    let gwas_inspector = viewer.gwas_inspector_snapshot();
    let quantitative_inspector = viewer.quantitative_inspector_snapshot();
    let copy_number_inspector = viewer.copy_number_inspector_snapshot();
    let mutation_inspector = viewer.mutation_inspector_snapshot();
    let interaction_inspector = viewer.interaction_inspector_snapshot();
    let variant_alignment_evidence = variant_inspector
        .as_ref()
        .map_or_else(Vec::new, |inspector| {
            viewer.variant_alignment_evidence(inspector)
        });
    let chain_inspector = viewer.panels.active().selected_chain.clone();
    let mate_target_available = viewer.selected_mate_target().is_some();
    let visible_consensus = viewer.visible_consensus();
    let visible_reference_sequence = viewer.visible_reference_sequence();
    let panel_controls = viewer.panels.iter().map(|_| ()).collect::<Vec<_>>();
    let mut clear_selection = false;
    let mut selected_to_top_requested = false;
    let mut chain_navigation_requested = None;
    let mut mate_panel_requested = false;
    let mut chain_panel_requested = None;
    let mut chain_all_panels_requested = false;
    let mut variant_partner_requested = None;
    let mut variant_endpoint_requested = None;
    let mut interaction_endpoint_requested = None;
    let mut variant_pileup_requested = None;
    let mut variant_modification_requested = None;
    let mut variant_genotypes_export_requested = false;
    let mut variant_evidence_export_requested = false;
    let mut junction_bed_export_requested = false;
    let mut sashimi_png_export_requested = false;
    let mut sashimi_svg_export_requested = false;
    let mut add_selected_feature_to_review = false;
    let mut review_open = false;

    let has_inspector = inspector.is_some()
        || chain_inspector.is_some()
        || pileup_inspector.is_some()
        || modification_inspector.is_some()
        || junction_inspector.is_some()
        || feature_inspector.is_some()
        || variant_inspector.is_some()
        || gwas_inspector.is_some()
        || quantitative_inspector.is_some()
        || copy_number_inspector.is_some()
        || mutation_inspector.is_some()
        || interaction_inspector.is_some();
    if !has_inspector {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.weak("Select a read, pileup, junction, or feature to inspect it here.");
        });
    }

    if let Some(inspector) = &inspector {
        let document = alignment_inspector_document(
            inspector,
            format!(
                "alignment-segment:{}",
                viewer.panels.active().selected_record.unwrap_or_default()
            ),
            visible_reference_sequence.as_ref(),
            mate_target_available,
            MAXIMUM_VIEW_PANELS.saturating_sub(panel_controls.len()),
        );
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::CopyCigar => {
                    if let Some(cigar) = &inspector.cigar {
                        ui.ctx().copy_text(cigar.clone());
                    }
                }
                InspectorActionId::CopyReadSequence => {
                    if let Some(sequence) = &inspector.read_sequence {
                        ui.ctx().copy_text(sequence.text.clone());
                    }
                }
                InspectorActionId::CopyEventSequence => {
                    if let Some(sequence) = &inspector.event_sequence {
                        ui.ctx().copy_text(sequence.clone());
                    }
                }
                InspectorActionId::CopyAuxiliaryTags => {
                    ui.ctx()
                        .copy_text(auxiliary_tags_text(&inspector.auxiliary));
                }
                InspectorActionId::CopyVisibleReference => {
                    if let Some(reference) = &visible_reference_sequence {
                        ui.ctx().copy_text(reference.text.clone());
                    }
                }
                InspectorActionId::OpenMate => mate_panel_requested = true,
                InspectorActionId::MoveReadToTop => selected_to_top_requested = true,
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }
    if let Some(chain) = &chain_inspector {
        let document = supplementary_chain_inspector_document(
            chain,
            MAXIMUM_VIEW_PANELS.saturating_sub(panel_controls.len()),
        );
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::NavigateChain(index) => {
                    chain_navigation_requested = Some(index);
                }
                InspectorActionId::OpenChainSegment(index) => {
                    chain_panel_requested = Some(index);
                }
                InspectorActionId::OpenAllChainSegments => {
                    chain_all_panels_requested = true;
                }
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }
    if let Some(inspector) = &pileup_inspector {
        let document = pileup_inspector_document(
            inspector,
            visible_consensus.as_ref(),
            visible_reference_sequence.as_ref(),
        );
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::CopyVisibleConsensus => {
                    if let Some(consensus) = &visible_consensus {
                        ui.ctx().copy_text(consensus.text.clone());
                    }
                }
                InspectorActionId::CopyVisibleReference => {
                    if let Some(reference) = &visible_reference_sequence {
                        ui.ctx().copy_text(reference.text.clone());
                    }
                }
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }
    if let Some(inspector) = &modification_inspector {
        let document = modification_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        clear_selection |= response
            .actions
            .contains(&InspectorActionId::ClearSelection);
    }
    if let Some(inspector) = &junction_inspector {
        let document = junction_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::ExportJunctionBed => junction_bed_export_requested = true,
                InspectorActionId::ExportSashimiPng => sashimi_png_export_requested = true,
                InspectorActionId::ExportSashimiSvg => sashimi_svg_export_requested = true,
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }
    if let Some(inspector) = &feature_inspector {
        let document = feature_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::AddFeatureToReview => {
                    add_selected_feature_to_review = true;
                    review_open = true;
                }
                InspectorActionId::ClearSelection => clear_selection = true,
                InspectorActionId::CopyDetails
                | InspectorActionId::CopyCigar
                | InspectorActionId::CopyReadSequence
                | InspectorActionId::CopyEventSequence
                | InspectorActionId::CopyAuxiliaryTags
                | InspectorActionId::CopyVisibleReference
                | InspectorActionId::CopyVisibleConsensus
                | InspectorActionId::OpenMate
                | InspectorActionId::MoveReadToTop
                | InspectorActionId::NavigateChain(_)
                | InspectorActionId::OpenChainSegment(_)
                | InspectorActionId::OpenAllChainSegments
                | InspectorActionId::ExportJunctionBed
                | InspectorActionId::ExportSashimiPng
                | InspectorActionId::ExportSashimiSvg
                | InspectorActionId::OpenInteractionEndpoint(_)
                | InspectorActionId::OpenVariantPartner(_)
                | InspectorActionId::OpenVariantEndpoint
                | InspectorActionId::InspectVariantPileup(_, _)
                | InspectorActionId::InspectVariantModifications(_, _)
                | InspectorActionId::ExportVariantGenotypes
                | InspectorActionId::ExportVariantEvidence => {}
            }
        }
    }
    if let Some(inspector) = &quantitative_inspector {
        let document = quantitative_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        clear_selection |= response
            .actions
            .contains(&InspectorActionId::ClearSelection);
    }
    if let Some(inspector) = &gwas_inspector {
        let document = gwas_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        clear_selection |= response
            .actions
            .contains(&InspectorActionId::ClearSelection);
    }
    if let Some(inspector) = &copy_number_inspector {
        let document = copy_number_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        clear_selection |= response
            .actions
            .contains(&InspectorActionId::ClearSelection);
    }
    if let Some(inspector) = &mutation_inspector {
        let document = mutation_inspector_document(inspector);
        let response = show_inspector_document(ui, &document, None);
        clear_selection |= response
            .actions
            .contains(&InspectorActionId::ClearSelection);
    }
    if let Some(inspector) = &interaction_inspector {
        let record = &inspector.query.records[inspector.record_index];
        let document = interaction_inspector_presentation_document(
            inspector,
            MAXIMUM_VIEW_PANELS.saturating_sub(panel_controls.len()),
        );
        let response = show_inspector_document(ui, &document, None);
        for action in response.actions {
            match action {
                InspectorActionId::OpenInteractionEndpoint(endpoint) => {
                    let endpoint = match endpoint {
                        InspectorEndpoint::Source => &record.source,
                        InspectorEndpoint::Target => &record.target,
                    };
                    interaction_endpoint_requested = Some((
                        endpoint.contig.clone(),
                        endpoint_center_position(endpoint.start, endpoint.end),
                    ));
                }
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }
    if let Some(inspector) = &variant_inspector {
        let record = &inspector.query.records[inspector.record_index];
        let document = variant_inspector_presentation_document(
            inspector,
            record,
            &variant_alignment_evidence,
            viewer.sample_metadata.as_ref(),
            &variant_sample_search,
            MAXIMUM_VIEW_PANELS.saturating_sub(panel_controls.len()),
        );
        let response = show_inspector_document(ui, &document, Some(&mut variant_sample_search));
        for action in response.actions {
            match action {
                InspectorActionId::OpenVariantPartner(index) => {
                    if let Some(partner) = record.breakends.get(index) {
                        variant_partner_requested =
                            Some((partner.contig.clone(), partner.position));
                    }
                }
                InspectorActionId::OpenVariantEndpoint => {
                    variant_endpoint_requested =
                        Some((record.contig.clone(), record.end.saturating_sub(1)));
                }
                InspectorActionId::InspectVariantPileup(track_id, position) => {
                    variant_pileup_requested = Some((track_id, position));
                }
                InspectorActionId::InspectVariantModifications(track_id, position) => {
                    variant_modification_requested = Some((track_id, position));
                }
                InspectorActionId::ExportVariantGenotypes => {
                    variant_genotypes_export_requested = true;
                }
                InspectorActionId::ExportVariantEvidence => {
                    variant_evidence_export_requested = true;
                }
                InspectorActionId::ClearSelection => clear_selection = true,
                _ => {}
            }
        }
    }

    InspectorUiActions {
        clear_selection,
        selected_to_top_requested,
        chain_navigation_requested,
        mate_panel_requested,
        chain_panel_requested,
        chain_all_panels_requested,
        variant_partner_requested,
        variant_endpoint_requested,
        interaction_endpoint_requested,
        variant_pileup_requested,
        variant_modification_requested,
        variant_genotypes_export_requested,
        variant_evidence_export_requested,
        junction_bed_export_requested,
        sashimi_png_export_requested,
        sashimi_svg_export_requested,
        add_selected_feature_to_review,
        review_open,
        variant_sample_search,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        break_long_monospace_runs, inspector_table_matching_rows, show_inspector_document,
        wrapped_monospace,
    };
    use crate::{
        InspectorDocument, InspectorField, InspectorSection, InspectorTable, InspectorValue,
    };

    #[test]
    fn wrapped_monospace_stays_inside_a_narrow_property_row() {
        let display = break_long_monospace_runs(&"123M4I".repeat(100));
        assert!(display.lines().all(|line| line.chars().count() <= 24));
        let context = egui::Context::default();
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            ui.set_max_width(180.0);
            let maximum_x = ui.max_rect().right();
            ui.horizontal_wrapped(|ui| {
                ui.strong("CIGAR");
                let response = wrapped_monospace(ui, &"123M4I".repeat(100));
                assert!(response.rect.right() <= maximum_x + f32::EPSILON);
                assert!(response.rect.height() > ui.text_style_height(&egui::TextStyle::Monospace));
            });
        });
    }

    #[test]
    fn shared_document_renderer_handles_supported_panel_widths_and_dense_values() {
        let mut document = InspectorDocument::new("feature:width-test", "A very long feature name");
        document.sections.push(
            InspectorSection::new("summary", "Summary", 0).with_fields(vec![
                InspectorField::new(
                    "identifier",
                    "Stable identifier",
                    InspectorValue::Identifier("ENST00000373130.7-extra-long-suffix".repeat(3)),
                ),
                InspectorField::new(
                    "tags",
                    "Tags",
                    InspectorValue::Tags(vec![
                        "Basic".to_owned(),
                        "CCDS".to_owned(),
                        "APPRIS principal".to_owned(),
                    ]),
                ),
                InspectorField::new(
                    "table",
                    "Samples",
                    InspectorValue::Table(InspectorTable {
                        columns: vec!["Sample".to_owned(), "GT".to_owned()],
                        rows: (0..100)
                            .map(|index| vec![format!("sample-{index}"), "0/1".to_owned()])
                            .collect(),
                        total_rows: 100,
                        omitted_rows: 0,
                        search_label: Some("Filter samples".to_owned()),
                    }),
                ),
            ]),
        );

        for width in [280.0_f32, 340.0, 520.0] {
            for (scale, native_pixels_per_point) in [(0.8_f32, 1.0_f32), (2.0, 1.0), (1.0, 2.0)] {
                let context = egui::Context::default();
                context.set_zoom_factor(scale);
                let mut input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 900.0),
                    )),
                    ..egui::RawInput::default()
                };
                input
                    .viewports
                    .get_mut(&input.viewport_id)
                    .expect("the root viewport is present")
                    .native_pixels_per_point = Some(native_pixels_per_point);
                let output = context.run_ui(input, |ui| {
                    ui.set_max_width(width);
                    let _ = show_inspector_document(ui, &document, None);
                });
                assert!(!output.shapes.is_empty());
            }
        }
    }

    #[test]
    fn stable_section_ids_preserve_expansion_state_across_redraws() {
        let mut document = InspectorDocument::new("feature:stable", "Stable feature");
        document.sections.push(
            InspectorSection::new("raw-context", "Context", 0).with_fields(vec![
                InspectorField::new("id", "Identifier", InspectorValue::Identifier("x".into())),
            ]),
        );
        let context = egui::Context::default();
        let mut section_id = None;
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let response = show_inspector_document(ui, &document, None);
            section_id = response.section_ids.first().copied();
        });
        let section_id = section_id.expect("the section returned its persistent identity");
        let mut state = egui::collapsing_header::CollapsingState::load(&context, section_id)
            .expect("the rendered section stores expansion state");
        state.set_open(false);
        state.store(&context);

        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let _ = show_inspector_document(ui, &document, None);
        });

        assert!(
            !egui::collapsing_header::CollapsingState::load(&context, section_id)
                .unwrap()
                .is_open()
        );
    }

    #[test]
    fn table_filtering_is_bounded_and_does_not_change_source_order() {
        let table = InspectorTable {
            columns: vec!["Sample".to_owned()],
            rows: vec![
                vec!["sample-z".to_owned()],
                vec!["sample-a".to_owned()],
                vec!["sample-b".to_owned()],
            ],
            total_rows: 10_000,
            omitted_rows: 9_997,
            search_label: Some("Filter presented samples".to_owned()),
        };

        assert_eq!(
            inspector_table_matching_rows(&table, "sample"),
            vec![0, 1, 2]
        );
        assert_eq!(inspector_table_matching_rows(&table, "-a"), vec![1]);
        assert_eq!(table.rows.len(), 3);
        assert_eq!(table.omitted_rows, 9_997);
    }
}
