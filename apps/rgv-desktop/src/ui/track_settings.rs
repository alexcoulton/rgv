use crate::VariantPredicateOperator;
use rgv_core::{
    CopyNumberSortLocus, CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode,
    GwasColorMode, GwasTrackSettings, InteractionDisplayMode, InteractionTrackSettings,
    MutationColorMode, MutationTrackSettings, QuantitativeArithmetic, QuantitativeDisplayMode,
    QuantitativeMissingMode, QuantitativeScaleMode, QuantitativeScaleTransform,
    QuantitativeTrackSettings, QuantitativeWindowFunction, TrackId, VariantColorMode,
    VariantFieldComparison, VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter,
    VariantGenotypeGrouping, VariantMissingValueRule, VariantTrackSettings, VariantTypeGroup,
};

pub(crate) fn interaction_track_settings_ui(
    ui: &mut egui::Ui,
    settings: &mut InteractionTrackSettings,
) {
    ui.heading("Interaction display");
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Layout")
            .selected_text(match settings.display_mode {
                InteractionDisplayMode::Arcs => "Arcs",
                InteractionDisplayMode::Blocks => "Blocks and connectors",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.display_mode,
                    InteractionDisplayMode::Arcs,
                    "Arcs",
                );
                ui.selectable_value(
                    &mut settings.display_mode,
                    InteractionDisplayMode::Blocks,
                    "Blocks and connectors",
                );
            });
    });
    let mut filter_enabled = settings.minimum_score.is_some();
    if ui
        .checkbox(&mut filter_enabled, "Require minimum score")
        .changed()
    {
        settings.minimum_score = filter_enabled.then_some(0.0);
    }
    if let Some(minimum) = &mut settings.minimum_score {
        ui.add(
            egui::DragValue::new(minimum)
                .range(0.0..=f32::MAX)
                .speed(1.0)
                .prefix("Minimum "),
        );
    }
    let mut fixed_scale = settings.fixed_maximum_score.is_some();
    if ui.checkbox(&mut fixed_scale, "Fixed score scale").changed() {
        settings.fixed_maximum_score = fixed_scale.then_some(1_000.0);
    }
    if let Some(maximum) = &mut settings.fixed_maximum_score {
        ui.add(
            egui::DragValue::new(maximum)
                .range(f32::EPSILON..=f32::MAX)
                .speed(1.0)
                .prefix("Maximum "),
        );
    }
    ui.checkbox(&mut settings.show_labels, "Show bounded interaction labels");
    ui.small(
        "Minimum score is applied before the query cap. Cross-chromosome endpoints are shown as labelled vertical markers and can open linked panels.",
    );
}

pub(crate) fn gwas_track_settings_ui(ui: &mut egui::Ui, settings: &mut GwasTrackSettings) {
    ui.heading("GWAS Manhattan display");
    egui::ComboBox::from_label("Point colours")
        .selected_text(match settings.color_mode {
            GwasColorMode::Chromosome => "Chromosome palette",
            GwasColorMode::Alternating => "Alternating chromosomes",
            GwasColorMode::Uniform => "Uniform",
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut settings.color_mode,
                GwasColorMode::Chromosome,
                "Chromosome palette",
            );
            ui.selectable_value(
                &mut settings.color_mode,
                GwasColorMode::Alternating,
                "Alternating chromosomes",
            );
            ui.selectable_value(&mut settings.color_mode, GwasColorMode::Uniform, "Uniform");
        });
    ui.add(egui::Slider::new(&mut settings.point_size, 1.0..=16.0).text("Point size (px)"));
    ui.add(
        egui::Slider::new(&mut settings.maximum_score, 1.0..=100.0)
            .logarithmic(true)
            .text("Maximum -log10(p)"),
    );
    match settings.color_mode {
        GwasColorMode::Chromosome => {
            ui.small("Chromosome names map deterministically to a bounded palette.");
        }
        GwasColorMode::Alternating => {
            ui.horizontal(|ui| {
                ui.label("Odd chromosomes");
                color_setting_button(ui, &mut settings.alternating_color_one);
                ui.label("Even chromosomes");
                color_setting_button(ui, &mut settings.alternating_color_two);
            });
        }
        GwasColorMode::Uniform => {
            ui.horizontal(|ui| {
                ui.label("Point colour");
                color_setting_button(ui, &mut settings.uniform_color);
            });
        }
    }
    ui.small(
        "P-values remain exact in the inspector. Values above the selected scale maximum are clipped only for drawing.",
    );
}

pub(crate) fn color_setting_button(ui: &mut egui::Ui, color: &mut [u8; 4]) {
    let mut value = egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
    if ui.color_edit_button_srgba(&mut value).changed() {
        *color = value.to_array();
    }
}

pub(crate) fn effective_copy_number_semantics_for_settings(
    settings: &CopyNumberTrackSettings,
    inferred: rgv_hts::CopyNumberValueSemantics,
) -> rgv_hts::CopyNumberValueSemantics {
    match settings.value_mode {
        CopyNumberValueMode::Automatic => inferred,
        CopyNumberValueMode::LogRatio => rgv_hts::CopyNumberValueSemantics::LogRatio,
        CopyNumberValueMode::Absolute => rgv_hts::CopyNumberValueSemantics::Absolute,
    }
}

pub(crate) fn quantitative_arithmetic_value(
    left: f32,
    right: f32,
    operation: QuantitativeArithmetic,
) -> Option<f32> {
    if !left.is_finite() || !right.is_finite() {
        return None;
    }
    let value = match operation {
        QuantitativeArithmetic::None => left,
        QuantitativeArithmetic::Add => left + right,
        QuantitativeArithmetic::Subtract => left - right,
        QuantitativeArithmetic::Multiply => left * right,
        QuantitativeArithmetic::Divide if right == 0.0 => return None,
        QuantitativeArithmetic::Divide => left / right,
    };
    value.is_finite().then_some(value)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn quantitative_track_settings_ui(
    ui: &mut egui::Ui,
    settings: &mut QuantitativeTrackSettings,
    track_id: TrackId,
    track_choices: &[(TrackId, String)],
) {
    ui.heading("Quantitative display");
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Scale range")
            .selected_text(match settings.scale_mode {
                QuantitativeScaleMode::Automatic => "Per-track automatic",
                QuantitativeScaleMode::Fixed => "Fixed",
                QuantitativeScaleMode::Shared => "Shared visible tracks",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.scale_mode,
                    QuantitativeScaleMode::Automatic,
                    "Per-track automatic",
                );
                ui.selectable_value(
                    &mut settings.scale_mode,
                    QuantitativeScaleMode::Fixed,
                    "Fixed minimum and maximum",
                );
                ui.selectable_value(
                    &mut settings.scale_mode,
                    QuantitativeScaleMode::Shared,
                    "Shared across visible compatible tracks",
                );
            });
    });
    if settings.scale_mode == QuantitativeScaleMode::Fixed {
        ui.horizontal(|ui| {
            ui.label("Range");
            ui.add(
                egui::DragValue::new(&mut settings.fixed_minimum)
                    .speed(0.1)
                    .prefix("min "),
            );
            ui.add(
                egui::DragValue::new(&mut settings.fixed_maximum)
                    .speed(0.1)
                    .prefix("max "),
            );
        });
        if settings.fixed_minimum >= settings.fixed_maximum {
            ui.colored_label(
                egui::Color32::DARK_RED,
                "The fixed maximum must be greater than the minimum.",
            );
        }
    }
    if settings.scale_mode == QuantitativeScaleMode::Shared {
        let mut grouped = settings.scale_group.is_some();
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut grouped, "Restrict shared scale to group")
                .changed()
            {
                settings.scale_group = grouped.then_some(1);
            }
            if let Some(group) = &mut settings.scale_group {
                ui.add(
                    egui::DragValue::new(group)
                        .range(1..=u32::MAX)
                        .prefix("group "),
                );
            }
        });
    } else {
        settings.scale_group = None;
    }
    let mut overlaid = settings.overlay_group.is_some();
    ui.horizontal(|ui| {
        if ui.checkbox(&mut overlaid, "Overlay with group").changed() {
            settings.overlay_group = overlaid.then_some(1);
        }
        if let Some(group) = &mut settings.overlay_group {
            ui.add(
                egui::DragValue::new(group)
                    .range(1..=u32::MAX)
                    .prefix("group "),
            );
        }
    });
    if let Some(group) = settings.overlay_group {
        settings.scale_mode = QuantitativeScaleMode::Shared;
        settings.scale_group = Some(group);
    }
    ui.small(
        "Tracks with the same overlay group share one frame. Clear the checkbox to separate this track again.",
    );
    ui.horizontal(|ui| {
        ui.label("Baseline");
        ui.add(egui::DragValue::new(&mut settings.baseline).speed(0.1));
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Value scale")
            .selected_text(match settings.scale_transform {
                QuantitativeScaleTransform::Linear => "Linear",
                QuantitativeScaleTransform::SignedLog10 => "Signed log10(1 + |value|)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.scale_transform,
                    QuantitativeScaleTransform::Linear,
                    "Linear",
                );
                ui.selectable_value(
                    &mut settings.scale_transform,
                    QuantitativeScaleTransform::SignedLog10,
                    "Signed log10(1 + |value|)",
                );
            });
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Draw as")
            .selected_text(match settings.display_mode {
                QuantitativeDisplayMode::Bar => "Bars",
                QuantitativeDisplayMode::Line => "Line",
                QuantitativeDisplayMode::Point => "Points",
                QuantitativeDisplayMode::Heatmap => "Heatmap",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.display_mode,
                    QuantitativeDisplayMode::Bar,
                    "Bars",
                );
                ui.selectable_value(
                    &mut settings.display_mode,
                    QuantitativeDisplayMode::Line,
                    "Line",
                );
                ui.selectable_value(
                    &mut settings.display_mode,
                    QuantitativeDisplayMode::Point,
                    "Points",
                );
                ui.selectable_value(
                    &mut settings.display_mode,
                    QuantitativeDisplayMode::Heatmap,
                    "Heatmap",
                );
            });
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Window function")
            .selected_text(match settings.window_function {
                QuantitativeWindowFunction::Raw => "Raw intervals",
                QuantitativeWindowFunction::Minimum => "Minimum",
                QuantitativeWindowFunction::Mean => "Mean",
                QuantitativeWindowFunction::Maximum => "Maximum",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.window_function,
                    QuantitativeWindowFunction::Raw,
                    "Raw / no windowing",
                );
                ui.selectable_value(
                    &mut settings.window_function,
                    QuantitativeWindowFunction::Minimum,
                    "Minimum per screen window",
                );
                ui.selectable_value(
                    &mut settings.window_function,
                    QuantitativeWindowFunction::Mean,
                    "Base-weighted mean per screen window",
                );
                ui.selectable_value(
                    &mut settings.window_function,
                    QuantitativeWindowFunction::Maximum,
                    "Maximum per screen window",
                );
            });
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Arithmetic")
            .selected_text(match settings.arithmetic {
                QuantitativeArithmetic::None => "None",
                QuantitativeArithmetic::Add => "Add another track",
                QuantitativeArithmetic::Subtract => "Subtract another track",
                QuantitativeArithmetic::Multiply => "Multiply by another track",
                QuantitativeArithmetic::Divide => "Divide by another track",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.arithmetic,
                    QuantitativeArithmetic::None,
                    "None",
                );
                for (operation, label) in [
                    (QuantitativeArithmetic::Add, "Add"),
                    (QuantitativeArithmetic::Subtract, "Subtract"),
                    (QuantitativeArithmetic::Multiply, "Multiply"),
                    (QuantitativeArithmetic::Divide, "Divide"),
                ] {
                    ui.add_enabled_ui(track_choices.iter().any(|(id, _)| *id != track_id), |ui| {
                        ui.selectable_value(&mut settings.arithmetic, operation, label);
                    });
                }
            });
    });
    if settings.arithmetic == QuantitativeArithmetic::None {
        settings.operand_track = None;
    } else {
        let choices = track_choices
            .iter()
            .filter(|(id, _)| *id != track_id)
            .collect::<Vec<_>>();
        if !choices
            .iter()
            .any(|(id, _)| Some(*id) == settings.operand_track)
        {
            settings.operand_track = choices.first().map(|(id, _)| *id);
        }
        if let Some(selected) = settings.operand_track {
            let selected_name = choices
                .iter()
                .find(|(id, _)| *id == selected)
                .map_or("Unavailable track", |(_, name)| name.as_str());
            ui.horizontal(|ui| {
                egui::ComboBox::from_label("Operand track")
                    .selected_text(selected_name)
                    .show_ui(ui, |ui| {
                        for (id, name) in &choices {
                            ui.selectable_value(&mut settings.operand_track, Some(*id), name);
                        }
                    });
            });
            ui.small(
                "Arithmetic is evaluated only where both source tracks have finite data. Division by zero remains missing.",
            );
        } else {
            ui.colored_label(
                egui::Color32::DARK_RED,
                "Load another quantitative track to use arithmetic.",
            );
        }
    }
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Missing windows")
            .selected_text(match settings.missing_mode {
                QuantitativeMissingMode::Gap => "Leave gaps",
                QuantitativeMissingMode::Baseline => "Baseline marks",
                QuantitativeMissingMode::Marker => "Grey markers",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.missing_mode,
                    QuantitativeMissingMode::Gap,
                    "Leave gaps",
                );
                ui.selectable_value(
                    &mut settings.missing_mode,
                    QuantitativeMissingMode::Baseline,
                    "Mark at baseline",
                );
                ui.selectable_value(
                    &mut settings.missing_mode,
                    QuantitativeMissingMode::Marker,
                    "Grey missing-data markers",
                );
            });
    });
    ui.add(egui::Slider::new(&mut settings.opacity, 0.0..=1.0).text("Opacity"));
    ui.horizontal(|ui| {
        let mut positive = egui::Color32::from_rgba_unmultiplied(
            settings.positive_color[0],
            settings.positive_color[1],
            settings.positive_color[2],
            settings.positive_color[3],
        );
        let mut negative = egui::Color32::from_rgba_unmultiplied(
            settings.negative_color[0],
            settings.negative_color[1],
            settings.negative_color[2],
            settings.negative_color[3],
        );
        let positive_label = ui.label("Positive");
        if ui
            .color_edit_button_srgba(&mut positive)
            .labelled_by(positive_label.id)
            .changed()
        {
            settings.positive_color = positive.to_array();
        }
        let negative_label = ui.label("Negative");
        if ui
            .color_edit_button_srgba(&mut negative)
            .labelled_by(negative_label.id)
            .changed()
        {
            settings.negative_color = negative.to_array();
        }
    });
    ui.checkbox(
        &mut settings.show_range_labels,
        "Show displayed range labels",
    );
    ui.small(
        "Absent windows remain missing unless a marker mode is selected; they are never silently converted to zero.",
    );
}

#[allow(clippy::too_many_lines)]
pub(crate) fn copy_number_track_settings_ui(
    ui: &mut egui::Ui,
    settings: &mut CopyNumberTrackSettings,
    quantitative: &mut QuantitativeTrackSettings,
    current_locus: &CopyNumberSortLocus,
) {
    ui.heading("Copy-number matrix");
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Values")
            .selected_text(match settings.value_mode {
                CopyNumberValueMode::Automatic => "Infer from source",
                CopyNumberValueMode::LogRatio => "Log ratio",
                CopyNumberValueMode::Absolute => "Absolute copy number",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.value_mode,
                    CopyNumberValueMode::Automatic,
                    "Infer: negative values mean log ratio",
                );
                ui.selectable_value(
                    &mut settings.value_mode,
                    CopyNumberValueMode::LogRatio,
                    "Log ratio; neutral at 0",
                );
                ui.selectable_value(
                    &mut settings.value_mode,
                    CopyNumberValueMode::Absolute,
                    "Absolute copy number; neutral at 2",
                );
            });
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Scale range")
            .selected_text(match quantitative.scale_mode {
                QuantitativeScaleMode::Automatic => "Per-track automatic",
                QuantitativeScaleMode::Fixed => "Fixed",
                QuantitativeScaleMode::Shared => "Shared compatible matrices",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut quantitative.scale_mode,
                    QuantitativeScaleMode::Automatic,
                    "Per-track automatic",
                );
                ui.selectable_value(
                    &mut quantitative.scale_mode,
                    QuantitativeScaleMode::Fixed,
                    "Fixed minimum and maximum",
                );
                ui.selectable_value(
                    &mut quantitative.scale_mode,
                    QuantitativeScaleMode::Shared,
                    "Shared compatible matrices",
                );
            });
    });
    if quantitative.scale_mode == QuantitativeScaleMode::Fixed {
        ui.horizontal(|ui| {
            ui.label("Range");
            ui.add(
                egui::DragValue::new(&mut quantitative.fixed_minimum)
                    .speed(0.1)
                    .prefix("min "),
            );
            ui.add(
                egui::DragValue::new(&mut quantitative.fixed_maximum)
                    .speed(0.1)
                    .prefix("max "),
            );
        });
        if quantitative.fixed_minimum >= quantitative.fixed_maximum {
            ui.colored_label(
                egui::Color32::DARK_RED,
                "The fixed maximum must be greater than the minimum.",
            );
        }
    }
    if quantitative.scale_mode == QuantitativeScaleMode::Shared {
        let mut grouped = quantitative.scale_group.is_some();
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut grouped, "Restrict shared scale to group")
                .changed()
            {
                quantitative.scale_group = grouped.then_some(1);
            }
            if let Some(group) = &mut quantitative.scale_group {
                ui.add(
                    egui::DragValue::new(group)
                        .range(1..=u32::MAX)
                        .prefix("group "),
                );
            }
        });
    } else {
        quantitative.scale_group = None;
    }
    let previous_sort = settings.sort_mode;
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Sample order")
            .selected_text(match settings.sort_mode {
                CopyNumberSortMode::FileOrder => "File order",
                CopyNumberSortMode::Amplification => "Amplification at locus",
                CopyNumberSortMode::Deletion => "Deletion at locus",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.sort_mode,
                    CopyNumberSortMode::FileOrder,
                    "File order",
                );
                ui.selectable_value(
                    &mut settings.sort_mode,
                    CopyNumberSortMode::Amplification,
                    "Highest base-weighted value first",
                );
                ui.selectable_value(
                    &mut settings.sort_mode,
                    CopyNumberSortMode::Deletion,
                    "Lowest base-weighted value first",
                );
            });
    });
    if settings.sort_mode != previous_sort && settings.sort_mode != CopyNumberSortMode::FileOrder {
        settings.sort_locus = Some(current_locus.clone());
    }
    if settings.sort_mode != CopyNumberSortMode::FileOrder {
        ui.horizontal(|ui| {
            if ui.button("Use current view as sort locus").clicked() {
                settings.sort_locus = Some(current_locus.clone());
            }
            if let Some(locus) = &settings.sort_locus {
                ui.monospace(format!(
                    "{}:{}-{}",
                    locus.contig,
                    locus.start.saturating_add(1),
                    locus.end
                ));
            }
        });
    }
    ui.add(egui::Slider::new(&mut settings.row_height, 6.0..=40.0).text("Sample row height"));
    ui.checkbox(&mut settings.show_sample_names, "Show sample names");
    ui.checkbox(
        &mut settings.show_values,
        "Show values when segments are wide enough",
    );
    ui.add(egui::Slider::new(&mut quantitative.opacity, 0.0..=1.0).text("Opacity"));
    ui.horizontal(|ui| {
        let mut amplification = egui::Color32::from_rgba_unmultiplied(
            quantitative.positive_color[0],
            quantitative.positive_color[1],
            quantitative.positive_color[2],
            quantitative.positive_color[3],
        );
        let mut deletion = egui::Color32::from_rgba_unmultiplied(
            quantitative.negative_color[0],
            quantitative.negative_color[1],
            quantitative.negative_color[2],
            quantitative.negative_color[3],
        );
        let amplification_label = ui.label("Amplification");
        if ui
            .color_edit_button_srgba(&mut amplification)
            .labelled_by(amplification_label.id)
            .changed()
        {
            quantitative.positive_color = amplification.to_array();
        }
        let deletion_label = ui.label("Deletion");
        if ui
            .color_edit_button_srgba(&mut deletion)
            .labelled_by(deletion_label.id)
            .changed()
        {
            quantitative.negative_color = deletion.to_array();
        }
    });
    ui.checkbox(
        &mut quantitative.show_range_labels,
        "Show displayed range and neutral value",
    );
    ui.small(
        "SEG coordinates are converted from one-based inclusive input unless a track line declares coords=0. Sample sorting uses a base-weighted mean over the saved sort locus.",
    );
}

pub(crate) fn mutation_track_settings_ui(ui: &mut egui::Ui, settings: &mut MutationTrackSettings) {
    ui.heading("Mutation Annotation Format");
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Colour by")
            .selected_text(match settings.color_mode {
                MutationColorMode::Classification => "Variant classification",
                MutationColorMode::VariantType => "Variant type",
                MutationColorMode::Uniform => "Uniform",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.color_mode,
                    MutationColorMode::Classification,
                    "Variant classification",
                );
                ui.selectable_value(
                    &mut settings.color_mode,
                    MutationColorMode::VariantType,
                    "Variant type (SNP/INS/DEL)",
                );
                ui.selectable_value(
                    &mut settings.color_mode,
                    MutationColorMode::Uniform,
                    "Uniform dark markers",
                );
            });
    });
    ui.add(egui::Slider::new(&mut settings.row_height, 8.0..=40.0).text("Sample row height"));
    ui.checkbox(&mut settings.show_sample_names, "Show sample names");
    ui.checkbox(
        &mut settings.show_gene_labels,
        "Show gene labels when sufficiently zoomed in",
    );
    ui.small(
        "Each tumour sample has its own row. Repeated calls at one sample and locus have a horizontal centre line.",
    );
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn variant_track_settings_ui(
    ui: &mut egui::Ui,
    settings: &mut VariantTrackSettings,
    query: Option<&rgv_hts::VariantQuery>,
    sample_search: &mut String,
    predicate_scope: &mut VariantFieldScope,
    predicate_key: &mut String,
    predicate_operator: &mut VariantPredicateOperator,
    predicate_value: &mut String,
    predicate_missing: &mut VariantMissingValueRule,
    predicate_error: &mut Option<String>,
) {
    ui.heading("Variant review");
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Marker colours")
            .selected_text(match settings.color_mode {
                VariantColorMode::Type => "Variant type",
                VariantColorMode::Genotype => "Selected-sample genotype",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.color_mode,
                    VariantColorMode::Type,
                    "Variant type",
                );
                ui.selectable_value(
                    &mut settings.color_mode,
                    VariantColorMode::Genotype,
                    "Selected-sample genotype",
                );
            });
    });
    ui.horizontal(|ui| {
        egui::ComboBox::from_label("Genotype rows")
            .selected_text(match settings.genotype_grouping {
                VariantGenotypeGrouping::SampleOrder => "Header sample order",
                VariantGenotypeGrouping::Genotype => "Group by genotype",
                VariantGenotypeGrouping::Pedigree => "Pedigree order",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.genotype_grouping,
                    VariantGenotypeGrouping::SampleOrder,
                    "Header sample order",
                );
                ui.selectable_value(
                    &mut settings.genotype_grouping,
                    VariantGenotypeGrouping::Genotype,
                    "Group by genotype",
                );
                ui.selectable_value(
                    &mut settings.genotype_grouping,
                    VariantGenotypeGrouping::Pedigree,
                    "Pedigree order from sample metadata",
                );
            });
    });

    ui.collapsing("Record filters", |ui| {
        let mut quality_enabled = settings.minimum_quality.is_some();
        if ui
            .checkbox(&mut quality_enabled, "Require minimum QUAL")
            .changed()
        {
            settings.minimum_quality = quality_enabled.then_some(20.0);
        }
        if let Some(quality) = &mut settings.minimum_quality {
            ui.horizontal(|ui| {
                ui.label("QUAL ≥");
                ui.add(egui::DragValue::new(quality).range(0.0..=1_000_000.0));
            });
            missing_rule_ui(
                ui,
                "Missing QUAL",
                &mut settings.quality_missing,
                "variant-quality-missing",
            );
        }
        ui.checkbox(&mut settings.pass_only, "PASS records only");

        if let Some(query) = query {
            let mut available_filters = vec!["PASS".to_owned()];
            available_filters.extend(
                query
                    .header
                    .filters
                    .iter()
                    .map(|filter| filter.id.clone()),
            );
            available_filters.sort();
            available_filters.dedup();
            let mut all_filters = settings.allowed_filters.is_empty();
            if ui
                .checkbox(&mut all_filters, "Allow every FILTER value")
                .changed()
            {
                if all_filters {
                    settings.allowed_filters.clear();
                } else {
                    settings.allowed_filters.clone_from(&available_filters);
                }
            }
            if !settings.allowed_filters.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for filter in available_filters {
                        let mut selected = settings.allowed_filters.contains(&filter);
                        if ui.checkbox(&mut selected, &filter).changed() {
                            update_string_membership(
                                &mut settings.allowed_filters,
                                &filter,
                                selected,
                            );
                        }
                    }
                });
            }
        }

        let type_groups = variant_type_groups();
        let mut all_types = settings.variant_types.is_empty();
        if ui
            .checkbox(&mut all_types, "Show every variant type")
            .changed()
        {
            if all_types {
                settings.variant_types.clear();
            } else {
                settings.variant_types = type_groups.iter().map(|(group, _)| *group).collect();
            }
        }
        if !settings.variant_types.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (group, label) in type_groups {
                    let mut selected = settings.variant_types.contains(&group);
                    if ui.checkbox(&mut selected, label).changed() {
                        if selected {
                            if !settings.variant_types.contains(&group) {
                                settings.variant_types.push(group);
                            }
                        } else {
                            settings.variant_types.retain(|value| *value != group);
                        }
                    }
                }
            });
        }

        let mut minimum_af_enabled = settings.minimum_allele_frequency.is_some();
        let mut maximum_af_enabled = settings.maximum_allele_frequency.is_some();
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut minimum_af_enabled, "Minimum AF")
                .changed()
            {
                settings.minimum_allele_frequency = minimum_af_enabled.then_some(0.0);
            }
            if let Some(value) = &mut settings.minimum_allele_frequency {
                ui.add(egui::DragValue::new(value).range(0.0..=1.0).speed(0.01));
            }
            if ui
                .checkbox(&mut maximum_af_enabled, "Maximum AF")
                .changed()
            {
                settings.maximum_allele_frequency = maximum_af_enabled.then_some(1.0);
            }
            if let Some(value) = &mut settings.maximum_allele_frequency {
                ui.add(egui::DragValue::new(value).range(0.0..=1.0).speed(0.01));
            }
        });
        if minimum_af_enabled || maximum_af_enabled {
            missing_rule_ui(
                ui,
                "Missing AF",
                &mut settings.allele_frequency_missing,
                "variant-af-missing",
            );
        }

        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Selected-sample genotype")
                .selected_text(variant_genotype_filter_label(settings.genotype))
                .show_ui(ui, |ui| {
                    for filter in [
                        VariantGenotypeFilter::Any,
                        VariantGenotypeFilter::Called,
                        VariantGenotypeFilter::Missing,
                        VariantGenotypeFilter::HomReference,
                        VariantGenotypeFilter::Heterozygous,
                        VariantGenotypeFilter::HomAlternate,
                        VariantGenotypeFilter::NonReference,
                        VariantGenotypeFilter::Phased,
                        VariantGenotypeFilter::Unphased,
                    ] {
                        ui.selectable_value(
                            &mut settings.genotype,
                            filter,
                            variant_genotype_filter_label(filter),
                        );
                    }
                });
        });
        ui.small(
            "Record clauses use AND. Alternate alleles, field values, and selected samples use ANY within one clause.",
        );
    });

    ui.collapsing("Sample subset", |ui| {
        ui.checkbox(
            &mut settings.sample_subset_enabled,
            "Restrict decoding and review to selected samples",
        );
        if !settings.sample_subset_enabled {
            ui.small("All header samples are decoded within the query's explicit cell budget.");
            return;
        }
        let Some(query) = query else {
            ui.small("Sample names become available after this track finishes loading.");
            return;
        };
        ui.horizontal(|ui| {
            let label = ui.label("Find sample");
            ui.add(
                egui::TextEdit::singleline(sample_search)
                    .desired_width(220.0)
                    .hint_text("Sample name"),
            )
            .labelled_by(label.id);
            if ui.button("Clear selection").clicked() {
                settings.sample_subset.clear();
            }
        });
        let search = sample_search.to_ascii_lowercase();
        let matching = query
            .header
            .samples
            .iter()
            .enumerate()
            .filter_map(|(index, sample)| {
                (search.is_empty() || sample.to_ascii_lowercase().contains(&search))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        ui.horizontal(|ui| {
            if ui.button("Select matching").clicked() {
                for index in &matching {
                    let sample = &query.header.samples[*index];
                    if settings.sample_subset.len() == rgv_core::MAX_VARIANT_SAMPLE_SUBSET {
                        break;
                    }
                    if !settings.sample_subset.contains(sample) {
                        settings.sample_subset.push(sample.clone());
                    }
                }
            }
            ui.label(format!(
                "{} selected · {} matching · {} total",
                settings.sample_subset.len(),
                matching.len(),
                query.header.samples.len()
            ));
        });
        egui::ScrollArea::vertical().max_height(180.0).show_rows(
            ui,
            19.0,
            matching.len(),
            |ui, rows| {
                for row in rows {
                    let sample = &query.header.samples[matching[row]];
                    let mut selected = settings.sample_subset.contains(sample);
                    if ui.checkbox(&mut selected, sample).changed() {
                        update_string_membership(&mut settings.sample_subset, sample, selected);
                    }
                }
            },
        );
    });

    ui.collapsing("INFO and FORMAT conditions", |ui| {
        let mut remove = None;
        for (index, predicate) in settings.field_predicates.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                ui.monospace(format_variant_predicate(predicate));
                if ui.small_button("Remove").clicked() {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            settings.field_predicates.remove(index);
        }
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_label("Field scope")
                .selected_text(match predicate_scope {
                    VariantFieldScope::Info => "INFO",
                    VariantFieldScope::Format => "FORMAT",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(predicate_scope, VariantFieldScope::Info, "INFO");
                    ui.selectable_value(predicate_scope, VariantFieldScope::Format, "FORMAT");
                });
            if let Some(query) = query {
                let schemas = match predicate_scope {
                    VariantFieldScope::Info => &query.header.info,
                    VariantFieldScope::Format => &query.header.formats,
                };
                egui::ComboBox::from_label("Field")
                    .selected_text(predicate_key.as_str())
                    .show_ui(ui, |ui| {
                        for schema in schemas {
                            ui.selectable_value(
                                predicate_key,
                                schema.id.clone(),
                                &schema.id,
                            )
                            .on_hover_text(&schema.description);
                        }
                    });
            } else {
                let label = ui.label("Field");
                ui.add(
                    egui::TextEdit::singleline(predicate_key)
                        .desired_width(90.0)
                        .hint_text("Field"),
                )
                .labelled_by(label.id);
            }
            egui::ComboBox::from_label("Operator")
                .selected_text(variant_predicate_operator_label(*predicate_operator))
                .show_ui(ui, |ui| {
                    for operator in [
                        VariantPredicateOperator::Exists,
                        VariantPredicateOperator::Equals,
                        VariantPredicateOperator::NotEquals,
                        VariantPredicateOperator::Contains,
                        VariantPredicateOperator::GreaterThan,
                        VariantPredicateOperator::GreaterOrEqual,
                        VariantPredicateOperator::LessThan,
                        VariantPredicateOperator::LessOrEqual,
                    ] {
                        ui.selectable_value(
                            predicate_operator,
                            operator,
                            variant_predicate_operator_label(operator),
                        );
                    }
                });
            if *predicate_operator != VariantPredicateOperator::Exists {
                let label = ui.label("Value");
                ui.add(
                    egui::TextEdit::singleline(predicate_value)
                        .desired_width(100.0)
                        .hint_text("Value"),
                )
                .labelled_by(label.id);
            }
        });
        missing_rule_ui(
            ui,
            "Missing field",
            predicate_missing,
            "variant-predicate-missing",
        );
        if ui
            .add_enabled(
                settings.field_predicates.len() < rgv_core::MAX_VARIANT_FIELD_PREDICATES,
                egui::Button::new("Add condition"),
            )
            .clicked()
        {
            match build_variant_predicate(
                *predicate_scope,
                predicate_key,
                *predicate_operator,
                predicate_value,
                *predicate_missing,
            ) {
                Ok(predicate) => {
                    settings.field_predicates.push(predicate);
                    *predicate_error = None;
                }
                Err(error) => *predicate_error = Some(error),
            }
        }
        if let Some(error) = predicate_error {
            ui.colored_label(egui::Color32::DARK_RED, error);
        }
        ui.small(
            "FORMAT conditions match when any selected sample satisfies the condition; missing behavior is always explicit.",
        );
    });

    if let Some(query) = query {
        ui.small(format!(
            "Query examined {} records · filtered {} · retained {} · decoded {}/{} samples",
            query.provenance.records_examined,
            query.provenance.records_filtered,
            query.records.len(),
            query.provenance.selected_samples,
            query.provenance.total_samples,
        ));
    }
}

pub(crate) fn update_string_membership(values: &mut Vec<String>, value: &str, selected: bool) {
    if selected {
        if !values.iter().any(|candidate| candidate == value) {
            values.push(value.to_owned());
        }
    } else {
        values.retain(|candidate| candidate != value);
    }
}

pub(crate) fn variant_type_groups() -> [(VariantTypeGroup, &'static str); 13] {
    [
        (VariantTypeGroup::Snv, "SNV"),
        (VariantTypeGroup::Mnv, "MNV"),
        (VariantTypeGroup::Insertion, "Insertion"),
        (VariantTypeGroup::Deletion, "Deletion"),
        (VariantTypeGroup::Complex, "Complex"),
        (VariantTypeGroup::StructuralDeletion, "SV deletion"),
        (VariantTypeGroup::StructuralInsertion, "SV insertion"),
        (VariantTypeGroup::Inversion, "Inversion"),
        (VariantTypeGroup::Duplication, "Duplication"),
        (VariantTypeGroup::CopyNumber, "Copy number"),
        (VariantTypeGroup::Breakend, "Breakend"),
        (VariantTypeGroup::SpanningDeletion, "Spanning deletion"),
        (VariantTypeGroup::OtherSymbolic, "Other symbolic"),
    ]
}

pub(crate) fn variant_genotype_filter_label(filter: VariantGenotypeFilter) -> &'static str {
    match filter {
        VariantGenotypeFilter::Any => "Any genotype",
        VariantGenotypeFilter::Called => "Called",
        VariantGenotypeFilter::Missing => "Missing",
        VariantGenotypeFilter::HomReference => "Homozygous reference",
        VariantGenotypeFilter::Heterozygous => "Heterozygous",
        VariantGenotypeFilter::HomAlternate => "Homozygous alternate",
        VariantGenotypeFilter::NonReference => "Any non-reference allele",
        VariantGenotypeFilter::Phased => "Fully phased",
        VariantGenotypeFilter::Unphased => "Contains unphased separator",
    }
}

pub(crate) fn missing_rule_ui(
    ui: &mut egui::Ui,
    label: &str,
    rule: &mut VariantMissingValueRule,
    id: &'static str,
) {
    ui.horizontal(|ui| {
        let label_response = ui.label(label);
        egui::ComboBox::from_id_salt(id)
            .selected_text(match rule {
                VariantMissingValueRule::Include => "Include missing",
                VariantMissingValueRule::Exclude => "Exclude missing",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(rule, VariantMissingValueRule::Include, "Include missing");
                ui.selectable_value(rule, VariantMissingValueRule::Exclude, "Exclude missing");
            })
            .response
            .labelled_by(label_response.id);
    });
}

pub(crate) fn variant_predicate_operator_label(operator: VariantPredicateOperator) -> &'static str {
    match operator {
        VariantPredicateOperator::Exists => "exists",
        VariantPredicateOperator::Equals => "=",
        VariantPredicateOperator::NotEquals => "≠",
        VariantPredicateOperator::Contains => "contains",
        VariantPredicateOperator::GreaterThan => ">",
        VariantPredicateOperator::GreaterOrEqual => "≥",
        VariantPredicateOperator::LessThan => "<",
        VariantPredicateOperator::LessOrEqual => "≤",
    }
}

pub(crate) fn build_variant_predicate(
    scope: VariantFieldScope,
    key: &str,
    operator: VariantPredicateOperator,
    value: &str,
    missing: VariantMissingValueRule,
) -> Result<VariantFieldPredicate, String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("Choose an INFO or FORMAT field".to_owned());
    }
    let value = value.trim();
    let numeric = || {
        value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| "A finite numeric comparison value is required".to_owned())
    };
    let comparison = match operator {
        VariantPredicateOperator::Exists => VariantFieldComparison::Exists,
        VariantPredicateOperator::Equals if !value.is_empty() => {
            VariantFieldComparison::Equals(value.to_owned())
        }
        VariantPredicateOperator::NotEquals if !value.is_empty() => {
            VariantFieldComparison::NotEquals(value.to_owned())
        }
        VariantPredicateOperator::Contains if !value.is_empty() => {
            VariantFieldComparison::Contains(value.to_owned())
        }
        VariantPredicateOperator::Equals
        | VariantPredicateOperator::NotEquals
        | VariantPredicateOperator::Contains => {
            return Err("A comparison value is required".to_owned());
        }
        VariantPredicateOperator::GreaterThan => VariantFieldComparison::GreaterThan(numeric()?),
        VariantPredicateOperator::GreaterOrEqual => {
            VariantFieldComparison::GreaterOrEqual(numeric()?)
        }
        VariantPredicateOperator::LessThan => VariantFieldComparison::LessThan(numeric()?),
        VariantPredicateOperator::LessOrEqual => VariantFieldComparison::LessOrEqual(numeric()?),
    };
    Ok(VariantFieldPredicate {
        scope,
        key: key.to_owned(),
        comparison,
        missing,
    })
}

pub(crate) fn format_variant_predicate(predicate: &VariantFieldPredicate) -> String {
    let scope = match predicate.scope {
        VariantFieldScope::Info => "INFO",
        VariantFieldScope::Format => "FORMAT(any selected sample)",
    };
    let comparison = match &predicate.comparison {
        VariantFieldComparison::Exists => "exists".to_owned(),
        VariantFieldComparison::Equals(value) => format!("= {value}"),
        VariantFieldComparison::NotEquals(value) => format!("≠ {value}"),
        VariantFieldComparison::Contains(value) => format!("contains {value}"),
        VariantFieldComparison::GreaterThan(value) => format!("> {value}"),
        VariantFieldComparison::GreaterOrEqual(value) => format!("≥ {value}"),
        VariantFieldComparison::LessThan(value) => format!("< {value}"),
        VariantFieldComparison::LessOrEqual(value) => format!("≤ {value}"),
    };
    let missing = match predicate.missing {
        VariantMissingValueRule::Include => "include missing",
        VariantMissingValueRule::Exclude => "exclude missing",
    };
    format!("{scope}/{} {comparison} · {missing}", predicate.key)
}
