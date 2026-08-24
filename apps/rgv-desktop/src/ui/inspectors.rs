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

fn read_inspector_max_height(content_height: f32) -> f32 {
    (content_height * 0.45).clamp(260.0, 520.0)
}

/// Builder-compatible adapter used while the inspector UI is docked. Keeping
/// the old window sizing calls as no-ops lets every inspector retain its full
/// contents and actions without allowing it to float over the genomic canvas.
struct PropertySection<'a> {
    ui: &'a mut egui::Ui,
    title: &'static str,
}

impl<'a> PropertySection<'a> {
    fn new(ui: &'a mut egui::Ui, title: &'static str) -> Self {
        Self { ui, title }
    }

    fn default_pos<T>(self, _position: T) -> Self {
        self
    }

    fn default_size<T>(self, _size: T) -> Self {
        self
    }

    fn max_size<T>(self, _size: T) -> Self {
        self
    }

    fn default_width(self, _width: f32) -> Self {
        self
    }

    fn resizable(self, _resizable: bool) -> Self {
        self
    }

    fn show<R>(self, _context: &egui::Context, add_contents: impl FnOnce(&mut egui::Ui) -> R) {
        egui::Frame::group(self.ui.style()).show(self.ui, |ui| {
            egui::CollapsingHeader::new(self.title)
                .default_open(true)
                .show(ui, add_contents);
        });
        self.ui.add_space(6.0);
    }
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
    let sample_metadata = viewer.sample_metadata.clone();
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
        let context = ui.ctx().clone();
        let inspector_max_height = read_inspector_max_height(context.content_rect().height());
        PropertySection::new(ui, "Read")
            .default_pos([24.0, 210.0])
            .default_size([520.0, 360.0])
            .max_size([720.0, inspector_max_height])
            .resizable(true)
            .show(&context, |ui| {
                egui::ScrollArea::vertical()
                    .max_height((inspector_max_height - 105.0).max(140.0))
                    .show(ui, |ui| {
                        for (label, value) in &inspector.fields {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(label);
                                ui.monospace(value);
                            });
                        }
                        ui.collapsing(
                            format!("Auxiliary tags ({})", inspector.auxiliary.len()),
                            |ui| {
                                for (tag, value) in &inspector.auxiliary {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.monospace(tag);
                                        ui.monospace(value);
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(format!("{tag}:{value}"));
                                        }
                                    });
                                }
                            },
                        );
                        if let Some(cigar) = &inspector.cigar
                            && cigar.len() > 256
                        {
                            ui.collapsing(
                                format!("Exact CIGAR ({} characters)", cigar.len()),
                                |ui| {
                                    egui::ScrollArea::horizontal().max_width(660.0).show(
                                        ui,
                                        |ui| {
                                            ui.monospace(cigar);
                                        },
                                    );
                                },
                            );
                        }
                    });
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&inspector.fields));
                    }
                    if let Some(cigar) = &inspector.cigar
                        && ui.button("Copy CIGAR").clicked()
                    {
                        ui.ctx().copy_text(cigar.clone());
                    }
                    if ui
                        .add_enabled(
                            inspector.read_sequence.is_some(),
                            egui::Button::new("Copy read sequence"),
                        )
                        .on_disabled_hover_text(
                            "Sequence was not loaded or is missing from the record",
                        )
                        .on_hover_text(sequence_copy_hover(
                            inspector.read_sequence.as_ref(),
                            "read",
                        ))
                        .clicked()
                        && let Some(sequence) = &inspector.read_sequence
                    {
                        ui.ctx().copy_text(sequence.text.clone());
                    }
                    if ui
                        .add_enabled(
                            !inspector.auxiliary.is_empty(),
                            egui::Button::new("Copy tags"),
                        )
                        .on_disabled_hover_text("This record has no loaded auxiliary tags")
                        .clicked()
                    {
                        ui.ctx()
                            .copy_text(auxiliary_tags_text(&inspector.auxiliary));
                    }
                    if ui
                        .add_enabled(
                            visible_reference_sequence.is_some(),
                            egui::Button::new("Copy visible reference"),
                        )
                        .on_disabled_hover_text(
                            "Zoom to base detail with a reference FASTA available",
                        )
                        .on_hover_text(sequence_copy_hover(
                            visible_reference_sequence.as_ref(),
                            "reference",
                        ))
                        .clicked()
                        && let Some(reference) = &visible_reference_sequence
                    {
                        ui.ctx().copy_text(reference.text.clone());
                    }
                });
                if let Some(sequence) = &inspector.event_sequence
                    && ui.button("Copy event sequence").clicked()
                {
                    ui.ctx().copy_text(sequence.clone());
                }
                if ui
                    .add_enabled(
                        mate_target_available && panel_controls.len() < MAXIMUM_VIEW_PANELS,
                        egui::Button::new("Open mate in new panel"),
                    )
                    .on_disabled_hover_text(
                        "A mapped mate and space for another panel are required",
                    )
                    .clicked()
                {
                    mate_panel_requested = true;
                }
                if ui.button("Move selected to top").clicked() {
                    selected_to_top_requested = true;
                }
                if ui.button("Clear selection").clicked() {
                    clear_selection = true;
                }
            });
    }
    if let Some(chain) = &chain_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Supplementary alignments")
                    .default_pos([470.0, 210.0])
                    .default_width(620.0)
                    .resizable(true)
                    .show(&context, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong("Read");
                            ui.monospace(String::from_utf8_lossy(&chain.read_name));
                        });
                        ui.label(format!(
                            "{} segments · {} malformed entries{}{}",
                            chain.segments.len(),
                            chain.malformed_entries,
                            if chain.invalid_tag_type {
                                " · invalid SA tag type"
                            } else {
                                ""
                            },
                            if chain.truncated {
                                " · chain capped"
                            } else {
                                ""
                            }
                        ));
                        ui.separator();
                        if let Some(index) = paint_supplementary_chain_diagram(ui, chain) {
                            chain_navigation_requested = Some(index);
                        }
                        ui.collapsing("Exact segment details", |ui| {
                            for (index, segment) in chain.segments.iter().enumerate() {
                                let strand = if segment.reverse { '-' } else { '+' };
                                let breakpoint = if segment.reverse {
                                    segment.start + 1
                                } else {
                                    segment.end
                                };
                                let mapping_quality = segment.mapping_quality.map_or_else(
                                    || "missing".to_owned(),
                                    |value| value.to_string(),
                                );
                                let label = format!(
                                    "{}:{}-{} {strand} · MAPQ {mapping_quality} · NM {} · breakpoint {}:{} · {}",
                                    segment.reference_name,
                                    segment.start + 1,
                                    segment.end,
                                    segment.edit_distance,
                                    segment.reference_name,
                                    breakpoint,
                                    format_cigar(&segment.cigar)
                                );
                                if ui
                                    .selectable_label(index == chain.active_segment, label)
                                    .on_hover_text("Navigate the active view to this segment")
                                    .clicked()
                                {
                                    chain_navigation_requested = Some(index);
                                }
                            }
                        });
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            let count = chain.segments.len();
                            if ui.add_enabled(count > 1, egui::Button::new("Previous segment")).clicked() {
                                chain_navigation_requested = Some(
                                    chain.active_segment.checked_sub(1).unwrap_or(count - 1),
                                );
                            }
                            if ui.add_enabled(count > 1, egui::Button::new("Next segment")).clicked() {
                                chain_navigation_requested =
                                    Some((chain.active_segment + 1) % count);
                            }
                            if ui
                                .add_enabled(
                                    count > 0 && panel_controls.len() < MAXIMUM_VIEW_PANELS,
                                    egui::Button::new("Open active segment in new panel"),
                                )
                                .clicked()
                            {
                                chain_panel_requested = Some(chain.active_segment);
                            }
                            if ui
                                .add_enabled(
                                    count > 1 && panel_controls.len() < MAXIMUM_VIEW_PANELS,
                                    egui::Button::new("Open all segments in panels"),
                                )
                                .on_disabled_hover_text(
                                    "More than one segment and space for another panel are required",
                                )
                                .clicked()
                            {
                                chain_all_panels_requested = true;
                            }
                            if ui.button("Clear chain").clicked() {
                                clear_selection = true;
                            }
                        });
                    });
    }
    if let Some(inspector) = &pileup_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Pileup")
            .default_pos([24.0, 210.0])
            .default_width(460.0)
            .resizable(true)
            .show(&context, |ui| {
                for (label, value) in &inspector.fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&inspector.fields));
                    }
                    if ui
                        .add_enabled(
                            visible_consensus.is_some(),
                            egui::Button::new("Copy visible consensus"),
                        )
                        .on_disabled_hover_text(
                            "Zoom to exact base resolution to calculate consensus",
                        )
                        .on_hover_text(sequence_copy_hover(visible_consensus.as_ref(), "consensus"))
                        .clicked()
                        && let Some(consensus) = &visible_consensus
                    {
                        ui.ctx().copy_text(consensus.text.clone());
                    }
                    if ui
                        .add_enabled(
                            visible_reference_sequence.is_some(),
                            egui::Button::new("Copy visible reference"),
                        )
                        .on_disabled_hover_text(
                            "Zoom to base detail with a reference FASTA available",
                        )
                        .on_hover_text(sequence_copy_hover(
                            visible_reference_sequence.as_ref(),
                            "reference",
                        ))
                        .clicked()
                        && let Some(reference) = &visible_reference_sequence
                    {
                        ui.ctx().copy_text(reference.text.clone());
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &modification_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Modification frequency")
            .default_pos([24.0, 210.0])
            .default_width(620.0)
            .resizable(true)
            .show(&context, |ui| {
                for (label, value) in &inspector.fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&inspector.fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &junction_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Junction")
            .default_pos([24.0, 210.0])
            .default_width(520.0)
            .resizable(true)
            .show(&context, |ui| {
                for (label, value) in &inspector.fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&inspector.fields));
                    }
                    if ui.button("Export junction BED…").clicked() {
                        junction_bed_export_requested = true;
                    }
                    if ui.button("Export Sashimi PNG…").clicked() {
                        sashimi_png_export_requested = true;
                    }
                    if ui.button("Export Sashimi SVG…").clicked() {
                        sashimi_svg_export_requested = true;
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &feature_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Feature")
            .default_pos([24.0, 210.0])
            .default_width(560.0)
            .resizable(true)
            .show(&context, |ui| {
                let fields = feature_inspector_fields(inspector);
                egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| {
                        for (label, value) in &fields {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(label);
                                ui.monospace(value);
                            });
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Add feature to review queue").clicked() {
                        add_selected_feature_to_review = true;
                        review_open = true;
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &quantitative_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Quantitative value")
            .default_pos([24.0, 210.0])
            .default_width(500.0)
            .resizable(true)
            .show(&context, |ui| {
                let fields = quantitative_inspector_fields(inspector);
                for (label, value) in &fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &gwas_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "GWAS association")
            .default_pos([24.0, 210.0])
            .default_width(560.0)
            .resizable(true)
            .show(&context, |ui| {
                let fields = gwas_inspector_fields(inspector);
                egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| {
                        for (label, value) in &fields {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(label);
                                ui.monospace(value);
                            });
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &copy_number_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Copy number")
            .default_pos([24.0, 210.0])
            .default_width(540.0)
            .resizable(true)
            .show(&context, |ui| {
                let fields = copy_number_inspector_fields(inspector);
                for (label, value) in &fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &mutation_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Mutation")
            .default_pos([24.0, 210.0])
            .default_width(620.0)
            .resizable(true)
            .show(&context, |ui| {
                let fields = mutation_inspector_fields(inspector);
                egui::ScrollArea::vertical()
                    .max_height(500.0)
                    .show(ui, |ui| {
                        for (label, value) in &fields {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(label);
                                ui.monospace(value);
                            });
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &interaction_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Interaction")
            .default_pos([24.0, 210.0])
            .default_width(560.0)
            .resizable(true)
            .show(&context, |ui| {
                let record = &inspector.query.records[inspector.record_index];
                let fields = interaction_inspector_fields(inspector);
                for (label, value) in &fields {
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(label);
                        ui.monospace(value);
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            panel_controls.len() < MAXIMUM_VIEW_PANELS,
                            egui::Button::new("Open source endpoint"),
                        )
                        .clicked()
                    {
                        interaction_endpoint_requested = Some((
                            record.source.contig.clone(),
                            endpoint_center_position(record.source.start, record.source.end),
                        ));
                    }
                    if ui
                        .add_enabled(
                            panel_controls.len() < MAXIMUM_VIEW_PANELS,
                            egui::Button::new("Open target endpoint"),
                        )
                        .clicked()
                    {
                        interaction_endpoint_requested = Some((
                            record.target.contig.clone(),
                            endpoint_center_position(record.target.start, record.target.end),
                        ));
                    }
                    if ui.button("Copy details").clicked() {
                        ui.ctx().copy_text(inspector_fields_text(&fields));
                    }
                    if ui.button("Clear selection").clicked() {
                        clear_selection = true;
                    }
                });
            });
    }
    if let Some(inspector) = &variant_inspector {
        let context = ui.ctx().clone();
        PropertySection::new(ui, "Variant")
                    .default_pos([24.0, 210.0])
                    .default_width(680.0)
                    .resizable(true)
                    .show(&context, |ui| {
                        let record = &inspector.query.records[inspector.record_index];
                        let fields = variant_inspector_fields(inspector, record);
                        for (label, value) in &fields {
                            ui.horizontal_wrapped(|ui| {
                                ui.strong(label);
                                ui.monospace(value);
                            });
                        }
                        ui.collapsing(format!("INFO ({})", record.info.len()), |ui| {
                            for field in &record.info {
                                let description = inspector
                                    .query
                                    .header
                                    .info
                                    .iter()
                                    .find(|schema| schema.id == field.key)
                                    .map_or("No header description", |schema| {
                                        schema.description.as_str()
                                    });
                                ui.horizontal_wrapped(|ui| {
                                    ui.monospace(&field.key).on_hover_text(description);
                                    ui.monospace(format_variant_field(field));
                                });
                            }
                        });
                        if !record.breakends.is_empty() {
                            ui.collapsing(
                                format!("Breakend partners ({})", record.breakends.len()),
                                |ui| {
                                    for partner in &record.breakends {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.monospace(format!(
                                                "ALT {} → {}:{}",
                                                partner.alternate_index + 1,
                                                partner.contig,
                                                partner.position + 1
                                            ));
                                            if ui
                                                .add_enabled(
                                                    panel_controls.len() < MAXIMUM_VIEW_PANELS,
                                                    egui::Button::new("Open partner"),
                                                )
                                                .clicked()
                                            {
                                                variant_partner_requested = Some((
                                                    partner.contig.clone(),
                                                    partner.position,
                                                ));
                                            }
                                        });
                                    }
                                },
                            );
                        }
                        ui.collapsing(
                            format!(
                                "Alignment evidence ({})",
                                variant_alignment_evidence.len()
                            ),
                            |ui| {
                                if variant_alignment_evidence.is_empty() {
                                    ui.label(
                                        "No alignment query currently covers this variant position.",
                                    );
                                }
                                for evidence in &variant_alignment_evidence {
                                    ui.group(|ui| {
                                        ui.strong(&evidence.track_name);
                                        ui.monospace(format!(
                                            "{}:{}-{} · depth {}× (forward {} · reverse {})",
                                            record.contig,
                                            evidence.bin_start + 1,
                                            evidence.bin_end,
                                            evidence.depth,
                                            evidence.forward_depth,
                                            evidence.reverse_depth,
                                        ));
                                        ui.monospace(&evidence.allele_summary);
                                        ui.label(format!(
                                            "Co-located modifications: {} calls across {} series",
                                            evidence.modification_calls,
                                            evidence.modification_series,
                                        ));
                                        ui.small(format!(
                                            "Read context: {} retained records{} · template-length model sampled {} proper pairs{}",
                                            evidence.retained_records,
                                            if evidence.retained_records_sampled {
                                                " (display safety-limited)"
                                            } else {
                                                ""
                                            },
                                            evidence.pair_templates_sampled,
                                            if evidence.complete_pileup {
                                                " · pileup complete"
                                            } else {
                                                " · pileup incomplete"
                                            },
                                        ));
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Inspect pileup").clicked() {
                                                variant_pileup_requested = Some((
                                                    evidence.track_id,
                                                    evidence.position,
                                                ));
                                            }
                                            if ui
                                                .add_enabled(
                                                    evidence.modification_calls > 0,
                                                    egui::Button::new("Inspect modifications"),
                                                )
                                                .clicked()
                                            {
                                                variant_modification_requested = Some((
                                                    evidence.track_id,
                                                    evidence.position,
                                                ));
                                            }
                                            if ui
                                                .add_enabled(
                                                    record.end > record.start + 1
                                                        && panel_controls.len()
                                                            < MAXIMUM_VIEW_PANELS,
                                                    egui::Button::new(
                                                        "Open event end for pair/split review",
                                                    ),
                                                )
                                                .clicked()
                                            {
                                                variant_endpoint_requested = Some((
                                                    record.contig.clone(),
                                                    record.end.saturating_sub(1),
                                                ));
                                            }
                                        });
                                    });
                                }
                                ui.small(
                                    "These are observations at the selected coordinate, not an RGV variant call. Read overlap, pair context, and modification co-location are not labelled as support without allele-specific proof.",
                                );
                            },
                        );
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(format!(
                                "Genotypes ({} loaded / {} selected / {} in file)",
                                record.genotypes.len(),
                                inspector.query.provenance.selected_samples,
                                inspector.query.provenance.total_samples,
                            ));
                            let sample_label = ui.label("Sample");
                            ui.add(
                                egui::TextEdit::singleline(&mut variant_sample_search)
                                    .desired_width(220.0)
                                    .hint_text("Filter loaded samples"),
                            )
                            .labelled_by(sample_label.id);
                        });
                        let search = variant_sample_search.to_ascii_lowercase();
                        let mut matching = record
                            .genotypes
                            .iter()
                            .enumerate()
                            .filter_map(|(genotype_index, genotype)| {
                                let sample = inspector
                                    .query
                                    .header
                                    .samples
                                    .get(genotype.sample_index)?;
                                (search.is_empty()
                                    || sample.to_ascii_lowercase().contains(&search))
                                .then_some(genotype_index)
                            })
                            .collect::<Vec<_>>();
                        let pedigree_order = (inspector.settings.genotype_grouping
                            == VariantGenotypeGrouping::Pedigree)
                            .then(|| {
                                sample_metadata.as_ref().and_then(|metadata| {
                                    metadata
                                        .pedigree_sample_order(&inspector.query.header.samples)
                                })
                            })
                            .flatten();
                        if inspector.settings.genotype_grouping
                            == VariantGenotypeGrouping::Genotype
                        {
                            matching.sort_by_key(|index| {
                                let genotype = &record.genotypes[*index];
                                (variant_genotype_group_rank(genotype), genotype.sample_index)
                            });
                        } else if let Some(order) = &pedigree_order
                        {
                            let mut rank = vec![usize::MAX; inspector.query.header.samples.len()];
                            for (position, &sample_index) in order.iter().enumerate() {
                                if let Some(rank) = rank.get_mut(sample_index) {
                                    *rank = position;
                                }
                            }
                            matching.sort_by_key(|index| {
                                let sample_index = record.genotypes[*index].sample_index;
                                (
                                    rank.get(sample_index).copied().unwrap_or(usize::MAX),
                                    sample_index,
                                )
                            });
                        }
                        if inspector.settings.genotype_grouping
                            == VariantGenotypeGrouping::Pedigree
                            && pedigree_order.is_none()
                        {
                            ui.colored_label(
                                egui::Color32::from_rgb(150, 88, 20),
                                "Pedigree order requires imported father_id or mother_id sample attributes; retaining VCF header order.",
                            );
                        }
                        egui::ScrollArea::vertical().max_height(280.0).show_rows(
                            ui,
                            19.0,
                            matching.len(),
                            |ui, rows| {
                                for row in rows {
                                    let genotype = &record.genotypes[matching[row]];
                                    let sample = inspector
                                        .query
                                        .header
                                        .samples
                                        .get(genotype.sample_index)
                                        .map_or("?", String::as_str);
                                    ui.horizontal_wrapped(|ui| {
                                        let group = variant_genotype_group_label(genotype);
                                        ui.colored_label(
                                            variant_genotype_color(genotype),
                                            "●",
                                        )
                                        .on_hover_text(group);
                                        if inspector.settings.genotype_grouping
                                            == VariantGenotypeGrouping::Genotype
                                            && (row == 0
                                                || variant_genotype_group_rank(genotype)
                                                    != variant_genotype_group_rank(
                                                        &record.genotypes[matching[row - 1]],
                                                    ))
                                        {
                                            ui.strong(group);
                                        }
                                        ui.monospace(sample);
                                        ui.monospace(format_genotype(genotype));
                                        for field in &genotype.fields {
                                            ui.monospace(format!(
                                                "{}={}",
                                                field.key,
                                                format_variant_field(field)
                                            ));
                                        }
                                    });
                                }
                            },
                        );
                        let provenance = inspector.query.provenance;
                        if provenance.genotype_cells_omitted > 0
                            || provenance.records_truncated
                            || provenance.header_samples_omitted > 0
                            || provenance.info_fields_truncated > 0
                            || provenance.format_fields_truncated > 0
                        {
                            ui.colored_label(
                                egui::Color32::from_rgb(150, 88, 20),
                                format!(
                                    "Bounded query: {} genotype cells omitted · {} header samples omitted · {} INFO schemas omitted · {} FORMAT schemas omitted{}",
                                    provenance.genotype_cells_omitted,
                                    provenance.header_samples_omitted,
                                    provenance.info_fields_truncated,
                                    provenance.format_fields_truncated,
                                    if provenance.records_truncated {
                                        " · record limit reached"
                                    } else {
                                        ""
                                    }
                                ),
                            );
                        }
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Copy details").clicked() {
                                ui.ctx().copy_text(variant_details_text(&fields, record));
                            }
                            if ui.button("Export variant + genotypes TSV…").clicked() {
                                variant_genotypes_export_requested = true;
                            }
                            if ui
                                .add_enabled(
                                    !variant_alignment_evidence.is_empty(),
                                    egui::Button::new("Export alignment evidence TSV…"),
                                )
                                .clicked()
                            {
                                variant_evidence_export_requested = true;
                            }
                            if ui.button("Clear selection").clicked() {
                                clear_selection = true;
                            }
                        });
                    });
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
    use super::{PropertySection, read_inspector_max_height};

    #[test]
    fn read_inspector_is_bounded_at_supported_window_heights() {
        for height in [600.0_f32, 800.0, 1_080.0, 1_600.0] {
            let inspector = read_inspector_max_height(height);
            assert!(inspector <= height * 0.45 + f32::EPSILON);
            assert!(inspector <= 520.0);
        }
    }

    #[test]
    fn property_section_renders_inside_its_parent_ui() {
        let context = egui::Context::default();
        let output = context.run_ui(egui::RawInput::default(), |ui| {
            let child_context = ui.ctx().clone();
            PropertySection::new(ui, "Read").show(&child_context, |ui| {
                ui.label("read-name");
            });
        });
        assert!(output.shapes.len() > 1);
    }
}
