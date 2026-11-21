//! The "Track settings" window for reference, alignment and auxiliary tracks.

use crate::app::alignment_context::{
    AlignmentQuickPredicate, alignment_predicate_label, quick_alignment_predicate,
};
use crate::display::auxiliary::alignment_sort_position;
use crate::ui::alignment_settings::{
    alignment_style_controls, modification_code_label, modified_base_color_mode_label,
    read_base_display_mode_label, rgba_to_color32,
};
use crate::ui::frame::ShellFrame;
use crate::ui::track_settings::{
    copy_number_track_settings_ui, gwas_track_settings_ui, interaction_track_settings_ui,
    mutation_track_settings_ui, quantitative_track_settings_ui, variant_track_settings_ui,
};
use crate::ui::{TrackFrameKey, TrackUiAction};
use crate::workspace::lists::sample_association_ui;
use crate::{MIN_VERTICAL_DENSITY, Viewer, ui};
use rgv_core::{
    AlignmentReadSampling, CoverageScaleTransform, FeatureDisplayMode, GeneticCode,
    InspectorActivation, JunctionDisplayMode, LandmarkAssetRole, ModifiedBaseColorMode,
    OverlappingMatePolicy, ReadBaseDisplayMode, ReadStackingMode, ReferenceTranslationFrame,
    SashimiCountMode, SashimiScaleMode, TrackId,
};
use rgv_layout::{AlignmentSortMode, MAX_HIDDEN_MODIFICATION_CODES, modification_code_color};

impl Viewer {
    pub(super) fn show_track_settings_window(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        if let Some(key) = st.tracks.rendered_track_settings_panel {
            let mut open = true;
            egui::Window::new("Track settings")
                .open(&mut open)
                .default_width(460.0)
                .vscroll(true)
                .show(ui.ctx(), |ui| match key {
                    TrackFrameKey::Group { channel, id } => {
                        Self::track_group_settings_ui(ui, channel, id);
                    }
                    TrackFrameKey::Reference => Self::reference_track_settings_ui(ui, st),
                    TrackFrameKey::Alignment(id) if id == st.align.settings_alignment_id => {
                        self.alignment_track_settings_ui(ui, st);
                    }
                    TrackFrameKey::Alignment(_) => {}
                    TrackFrameKey::Auxiliary(id) => Self::auxiliary_track_settings_ui(ui, st, id),
                });
            if !open {
                st.tracks.track_settings_panel = None;
            }
        }
    }

    pub(super) fn track_group_settings_ui(ui: &mut egui::Ui, channel: ui::TrackPane, id: u32) {
        ui.heading(format!("{channel:?} track group {id}"));
        ui.label("Use the group eye to minimize or expand it, or right-click the group header to ungroup its tracks.");
    }

    pub(super) fn reference_track_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.heading("Reference sequence");
        ui.checkbox(&mut st.align.show_reference, "Show reference track");
        ui.checkbox(
            &mut st.align.reference_reverse_complement,
            "Display reverse strand",
        );
        ui.separator();
        ui.label("Translation frames");
        ui.horizontal_wrapped(|ui| {
            for frame in ReferenceTranslationFrame::ALL {
                let mut enabled = st.align.reference_translation_frames.contains(&frame);
                if ui.checkbox(&mut enabled, frame.label()).changed() {
                    if enabled {
                        st.align.reference_translation_frames.push(frame);
                    } else {
                        st.align
                            .reference_translation_frames
                            .retain(|candidate| *candidate != frame);
                    }
                }
            }
        });
        egui::ComboBox::from_label("Genetic code")
            .selected_text(format!("{:?}", st.align.reference_genetic_code))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut st.align.reference_genetic_code,
                    GeneticCode::Standard,
                    "Standard",
                );
                ui.selectable_value(
                    &mut st.align.reference_genetic_code,
                    GeneticCode::VertebrateMitochondrial,
                    "Vertebrate mitochondrial",
                );
            });
        ui.label(
            "The FASTA is loaded independently of alignment files and stays pinned above the scrollable read stack.",
        );
    }

    /// Settings for the alignment track being edited; the sections run in display order.
    pub(super) fn alignment_track_settings_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        Self::alignment_identity_settings_ui(ui, st);
        Self::alignment_subtrack_settings_ui(ui, st);
        self.alignment_display_settings_ui(ui, st);
        Self::alignment_read_layout_settings_ui(ui, st);
        Self::alignment_base_settings_ui(ui, st);
        self.alignment_filter_settings_ui(ui, st);
    }

    /// Sample association, bulk-selection actions, name, read lookup, visibility and height.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_identity_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.heading("Alignment track");
        sample_association_ui(
            ui,
            &mut st.align.edited_track_sample_key,
            st.lists.sample_metadata.as_ref(),
        );
        if st.align.bulk_alignment_ids.len() > 1 {
            ui.label(format!(
                "{} alignment tracks selected",
                st.align.bulk_alignment_ids.len()
            ));
            ui.horizontal_wrapped(|ui| {
                if ui.button("Group selected tracks").clicked() {
                    if let Some(pane) = st.align.bulk_alignment_pane {
                        st.tracks
                            .track_ui_actions
                            .push(TrackUiAction::GroupSelectedTracks(pane));
                    }
                }
                if let Some(group) = &st.layout.bulk_alignment_group {
                    if ui
                        .button(if group.collapsed {
                            "Expand group"
                        } else {
                            "Minimize group"
                        })
                        .clicked()
                    {
                        st.tracks
                            .track_ui_actions
                            .push(TrackUiAction::ToggleTrackGroup(group.pane, group.id));
                    }
                    if ui.button("Ungroup").clicked() {
                        st.tracks
                            .track_ui_actions
                            .push(TrackUiAction::UngroupTracks(group.pane, group.id));
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                if ui.button("Show all").clicked() {
                    st.align.bulk_alignment_visibility = Some(true);
                }
                if ui.button("Hide all").clicked() {
                    st.align.bulk_alignment_visibility = Some(false);
                }
                if ui.button("Expand all").clicked() {
                    st.align.bulk_alignment_collapsed = Some(false);
                }
                if ui.button("Collapse all").clicked() {
                    st.align.bulk_alignment_collapsed = Some(true);
                }
            });
            if ui
                .button("Apply compatible appearance to selected tracks")
                .on_hover_text(
                    "Copies display, grouping, pairing, and modification appearance. Sample filters and record caps stay independent.",
                )
                .clicked()
            {
                st.align.apply_bulk_alignment_appearance = true;
            }
            ui.separator();
        }
        ui.horizontal(|ui| {
            let label = ui.label("Name");
            ui.text_edit_singleline(&mut st.align.alignment_name)
                .labelled_by(label.id);
        });
        ui.horizontal(|ui| {
            let label = ui.label("Read/template");
            let response = ui
                .add(
                    egui::TextEdit::singleline(&mut st.align.read_name_editor)
                        .desired_width(240.0)
                        .hint_text("Exact read name"),
                )
                .labelled_by(label.id);
            if ui.button("Select").clicked()
                || (response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)))
            {
                st.align.select_read_name_requested = true;
            }
        });
        ui.small(
            "Searches the complete individual-read population admitted for this panel's current viewport; names are not saved in sessions.",
        );
        if ui
            .checkbox(&mut st.align.alignment_visible, "Show annotation content")
            .changed()
        {
            st.align.alignment_collapsed = !st.align.alignment_visible;
        }
        ui.add(egui::Slider::new(&mut st.align.alignment_height, 40.0..=1_200.0).text("Height"));
    }

    /// Subtrack visibility and junction/sashimi appearance.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_subtrack_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.separator();
        ui.label("Appearance");
        ui.horizontal_wrapped(|ui| {
            ui.label("Subtracks");
            ui.checkbox(&mut st.align.show_reference, "Reference");
            ui.checkbox(&mut st.align.show_coverage, "Coverage");
            ui.checkbox(&mut st.align.show_junctions, "Junctions");
            ui.checkbox(&mut st.align.show_modification_frequency, "Modifications");
            ui.checkbox(&mut st.align.show_reads, "Reads");
        });
        if st.align.show_junctions {
            ui.horizontal(|ui| {
                ui.label("Junction depth ≥");
                ui.add(
                    egui::DragValue::new(&mut st.align.minimum_junction_depth)
                        .range(1..=1_000_000)
                        .suffix(" reads"),
                );
            });
            ui.horizontal(|ui| {
                ui.label("Aligned flank ≥");
                ui.add(
                    egui::DragValue::new(&mut st.align.minimum_junction_flank)
                        .range(0..=1_000_000)
                        .suffix(" bp each side"),
                );
            });
            ui.small(
                "Counts are recomputed from reads with this much aligned sequence on both sides of the splice.",
            );
            ui.horizontal(|ui| {
                let label = ui.label("Sashimi layout");
                egui::ComboBox::from_id_salt("junction-display-mode")
                    .selected_text(match st.align.junction_display_mode {
                        JunctionDisplayMode::Collapsed => "Collapsed",
                        JunctionDisplayMode::Expanded => "Expanded",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut st.align.junction_display_mode,
                            JunctionDisplayMode::Collapsed,
                            "Collapsed",
                        );
                        ui.selectable_value(
                            &mut st.align.junction_display_mode,
                            JunctionDisplayMode::Expanded,
                            "Expanded",
                        );
                    })
                    .response
                    .labelled_by(label.id);
            });
            ui.horizontal(|ui| {
                let label = ui.label("Count units");
                egui::ComboBox::from_id_salt("sashimi-count-mode")
                    .selected_text(match st.align.sashimi_count_mode {
                        SashimiCountMode::Raw => "Raw reads",
                        SashimiCountMode::CountsPerMillion => "CPM",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut st.align.sashimi_count_mode,
                            SashimiCountMode::Raw,
                            "Raw reads",
                        );
                        ui.selectable_value(
                            &mut st.align.sashimi_count_mode,
                            SashimiCountMode::CountsPerMillion,
                            "Counts per million mapped reads",
                        );
                    })
                    .response
                    .labelled_by(label.id);
            });
            ui.horizontal(|ui| {
                let label = ui.label("Arc scale");
                egui::ComboBox::from_id_salt("sashimi-scale-mode")
                    .selected_text(match st.align.sashimi_scale_mode {
                        SashimiScaleMode::Automatic => "Per-track auto",
                        SashimiScaleMode::Fixed => "Fixed",
                        SashimiScaleMode::Shared => "Shared visible tracks",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut st.align.sashimi_scale_mode,
                            SashimiScaleMode::Automatic,
                            "Per-track automatic",
                        );
                        ui.selectable_value(
                            &mut st.align.sashimi_scale_mode,
                            SashimiScaleMode::Fixed,
                            "Fixed maximum",
                        );
                        ui.selectable_value(
                            &mut st.align.sashimi_scale_mode,
                            SashimiScaleMode::Shared,
                            "Shared across visible tracks",
                        );
                    })
                    .response
                    .labelled_by(label.id);
                if st.align.sashimi_scale_mode == SashimiScaleMode::Fixed {
                    ui.add(
                        egui::DragValue::new(&mut st.align.sashimi_fixed_scale_maximum)
                            .range(1..=1_000_000)
                            .suffix(match st.align.sashimi_count_mode {
                                SashimiCountMode::Raw => " reads",
                                SashimiCountMode::CountsPerMillion => " CPM",
                            }),
                    );
                }
            });
            ui.checkbox(&mut st.align.show_junction_labels, "Show arc count labels");
            if st.align.sashimi_count_mode == SashimiCountMode::CountsPerMillion {
                ui.small(
                    "CPM uses the whole-file mapped-read count stored in the BAM index; unavailable metadata is shown explicitly and falls back to raw labels.",
                );
            }
        }
    }

    /// Read density, inspector activation, coverage scale and colour/group/sort style.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_display_settings_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.horizontal(|ui| {
            ui.label("Read density");
            if ui.button("Expanded").clicked() {
                st.align.alignment_density = 1.0;
            }
            if ui.button("Squished").clicked() {
                st.align.alignment_density = 0.55;
            }
            if ui.button("Collapsed").clicked() {
                st.align.alignment_density = MIN_VERTICAL_DENSITY;
            }
            ui.small(format!("{:.2}×", st.align.alignment_density));
        });
        ui.horizontal(|ui| {
            let label = ui.label("Open inspectors on");
            egui::ComboBox::from_id_salt("inspector-activation")
                .selected_text(match st.align.inspector_activation {
                    InspectorActivation::Click => "Click",
                    InspectorActivation::Hover => "Hover",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut st.align.inspector_activation,
                        InspectorActivation::Click,
                        "Click",
                    );
                    ui.selectable_value(
                        &mut st.align.inspector_activation,
                        InspectorActivation::Hover,
                        "Hover",
                    );
                })
                .response
                .labelled_by(label.id);
        });
        ui.checkbox(
            &mut st.align.use_fixed_coverage_scale,
            "Fixed coverage scale",
        );
        ui.horizontal(|ui| {
            let label = ui.label("Coverage transform");
            egui::ComboBox::from_id_salt("coverage-scale-transform")
                .selected_text(match st.align.coverage_scale_transform {
                    CoverageScaleTransform::Linear => "Linear",
                    CoverageScaleTransform::SquareRoot => "Square root",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut st.align.coverage_scale_transform,
                        CoverageScaleTransform::Linear,
                        "Linear",
                    );
                    ui.selectable_value(
                        &mut st.align.coverage_scale_transform,
                        CoverageScaleTransform::SquareRoot,
                        "Square root",
                    );
                })
                .response
                .labelled_by(label.id);
        });
        if st.align.use_fixed_coverage_scale {
            ui.horizontal(|ui| {
                ui.label("Coverage maximum");
                ui.add(
                    egui::DragValue::new(&mut st.align.coverage_scale_maximum)
                        .range(1..=10_000_000)
                        .suffix("×"),
                );
            });
        } else if let Some(maximum) = self
            .session
            .alignment_tracks
            .first()
            .and_then(|track| {
                self.presented_alignment_lane(st.snapshot.active_panel_id, track.config.id)
            })
            .and_then(|presented| presented.lane.snapshot.as_ref())
            .and_then(|snapshot| {
                snapshot
                    .summaries()
                    .maximum_depth(self.session.panels.active().viewport.interval())
                    .ok()
            })
        {
            ui.small(format!("Coverage autoscale: current maximum {}×", maximum));
        }
        alignment_style_controls(
            ui,
            &mut st.align.color_mode,
            &mut st.align.group_mode,
            &mut st.align.group_order,
            &mut st.align.sort_mode,
            &mut st.align.sort_order,
            &mut st.align.style_tag_editor,
        );
        if st.align.sort_mode == AlignmentSortMode::Base {
            let anchor = st.align.settings_filters.alignment_sort_anchor.or_else(|| {
                alignment_sort_position(
                    AlignmentSortMode::Base,
                    None,
                    self.session.panels.active().viewport,
                )
            });
            if let Some(anchor) = anchor {
                ui.small(format!(
                    "Base-sort anchor: {}:{}",
                    self.session.panels.active().contig_name,
                    anchor.saturating_add(1)
                ));
            }
        }
    }

    /// Read stacking, sampling, pairing and overlapping-mate policy.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_read_layout_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.horizontal(|ui| {
            let label = ui.label("Read stacking");
            egui::ComboBox::from_id_salt("read-stacking-mode")
                .selected_text(match st.align.read_stacking_mode {
                    ReadStackingMode::Stable => "Stable across navigation",
                    ReadStackingMode::Compact => "Compact each view",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut st.align.read_stacking_mode,
                        ReadStackingMode::Stable,
                        "Stable across navigation",
                    )
                    .on_hover_text(
                        "Builds an organised row architecture and keeps each read on its row",
                    );
                    ui.selectable_value(
                        &mut st.align.read_stacking_mode,
                        ReadStackingMode::Compact,
                        "Compact each view",
                    )
                    .on_hover_text(
                        "Rebuilds the current view for density; reads may move while navigating",
                    );
                })
                .response
                .labelled_by(label.id);
            if st.align.read_stacking_mode == ReadStackingMode::Stable
                && ui
                    .button("Rebuild stable architecture")
                    .on_hover_text(
                        "Use the current known neighbourhood as a new organised stable baseline",
                    )
                    .clicked()
            {
                st.align.compact_repack_requested = true;
            }
        });
        ui.small(match st.align.read_stacking_mode {
            ReadStackingMode::Stable => {
                "Builds an organised row architecture and keeps each read on its row."
            }
            ReadStackingMode::Compact => {
                "Rebuilds the current view for density; reads may move while navigating."
            }
        });
        ui.horizontal(|ui| {
            let label = ui.label("Read display");
            let mut downsampled = st.align.read_sampling.is_downsampled();
            egui::ComboBox::from_id_salt("read-sampling-mode")
                .selected_text(if downsampled {
                    "Downsampled"
                } else {
                    "Complete"
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut downsampled, true, "Downsampled")
                        .on_hover_text(
                            "Shows a stable subset of reads in deep regions; coverage always counts every read",
                        );
                    ui.selectable_value(&mut downsampled, false, "Complete")
                        .on_hover_text(
                            "Shows every read, or none above the complete-read limit",
                        );
                })
                .response
                .labelled_by(label.id);
            if downsampled != st.align.read_sampling.is_downsampled() {
                st.align.read_sampling = if downsampled {
                    AlignmentReadSampling::downsampled_default()
                } else {
                    AlignmentReadSampling::Complete
                };
            }
            if let AlignmentReadSampling::Downsampled {
                window_bases,
                maximum_reads,
            } = &mut st.align.read_sampling
            {
                ui.add(
                    egui::DragValue::new(maximum_reads)
                        .range(
                            1..=AlignmentReadSampling::MAXIMUM_READS_PER_WINDOW,
                        )
                        .suffix(" reads"),
                );
                ui.label("per");
                ui.add(
                    egui::DragValue::new(window_bases)
                        .range(1..=AlignmentReadSampling::MAXIMUM_WINDOW_BASES)
                        .suffix(" bp"),
                );
            }
        });
        ui.small(if st.align.read_sampling.is_downsampled() {
            "Deep regions show a stable subset of reads. Coverage, alleles and junctions count every read."
        } else {
            "Every read is shown; regions above the complete-read limit show coverage only."
        });
        ui.checkbox(&mut st.align.view_as_pairs, "View as pairs")
            .on_hover_text(
                "Pack primary ends with the same read name on one row and select them as one template",
            );
        ui.checkbox(
            &mut st.align.override_pair_bounds,
            "Override inferred template-length bounds",
        );
        if st.align.override_pair_bounds {
            ui.horizontal(|ui| {
                ui.label("Expected template length");
                ui.add(
                    egui::DragValue::new(&mut st.align.minimum_template_length)
                        .range(0..=10_000_000)
                        .suffix(" bp min"),
                );
                ui.add(
                    egui::DragValue::new(&mut st.align.maximum_template_length)
                        .range(0..=10_000_000)
                        .suffix(" bp max"),
                );
            });
        }
        if let Some(bounds) = st.align.pair_summary.inferred_bounds {
            ui.small(format!(
                "Inferred from {} of {} proper templates: median {} bp, expected {}–{} bp{}",
                st.align.pair_summary.sampled_templates,
                st.align.pair_summary.eligible_proper_templates,
                st.align
                    .pair_summary
                    .median_template_length
                    .unwrap_or_default(),
                bounds.minimum.unwrap_or_default(),
                bounds.maximum.unwrap_or_default(),
                if st.align.pair_summary.all_eligible_records {
                    ""
                } else {
                    " (incomplete query)"
                },
            ));
        } else {
            ui.small(format!(
                "Template-length inference unavailable: {} proper templates ({} required)",
                st.align.pair_summary.sampled_templates,
                rgv_hts::MIN_TEMPLATE_LENGTH_SAMPLES,
            ));
        }
        ui.horizontal(|ui| {
            let label = ui.label("Overlapping mates in pileup");
            egui::ComboBox::from_id_salt("overlapping-mate-policy")
                .selected_text(match st.align.overlapping_mate_policy {
                    OverlappingMatePolicy::CountOnce => "Count once",
                    OverlappingMatePolicy::CountBoth => "Count both",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut st.align.overlapping_mate_policy,
                        OverlappingMatePolicy::CountOnce,
                        "Count once (leftmost end)",
                    );
                    ui.selectable_value(
                        &mut st.align.overlapping_mate_policy,
                        OverlappingMatePolicy::CountBoth,
                        "Count both ends",
                    );
                })
                .response
                .labelled_by(label.id);
        });
    }

    /// Read base display, indel thresholds and base-modification appearance.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_base_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.horizontal(|ui| {
            let label = ui.label("Read bases");
            egui::ComboBox::from_id_salt("read-base-display-mode")
                .selected_text(read_base_display_mode_label(
                    st.align.read_base_display_mode,
                ))
                .show_ui(ui, |ui| {
                    for mode in [
                        ReadBaseDisplayMode::Mismatches,
                        ReadBaseDisplayMode::All,
                        ReadBaseDisplayMode::Hidden,
                    ] {
                        ui.selectable_value(
                            &mut st.align.read_base_display_mode,
                            mode,
                            read_base_display_mode_label(mode),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
        });
        ui.checkbox(
            &mut st.align.shade_read_bases_by_quality,
            "Shade read bases by quality",
        );
        ui.horizontal(|ui| {
            ui.label("Show indels ≥");
            ui.add(
                egui::Slider::new(&mut st.align.minimum_indel_size, 1..=10_000)
                    .logarithmic(true)
                    .suffix(" bp"),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Label events ≥");
            ui.add(
                egui::Slider::new(&mut st.align.minimum_event_label_size, 1..=10_000)
                    .logarithmic(true)
                    .suffix(" bp"),
            );
        });
        ui.checkbox(&mut st.align.show_modified_bases, "Show modified bases");
        ui.horizontal(|ui| {
            ui.label("Modification probability ≥");
            ui.add(
                egui::Slider::new(&mut st.align.minimum_modification_probability, 0..=255)
                    .suffix(" / 255"),
            );
        });
        ui.horizontal(|ui| {
            let label = ui.label("Modification colours");
            egui::ComboBox::from_id_salt("modified-base-color-mode")
                .selected_text(modified_base_color_mode_label(
                    st.align.modified_base_color_mode,
                ))
                .show_ui(ui, |ui| {
                    for mode in [
                        ModifiedBaseColorMode::ByType,
                        ModifiedBaseColorMode::Mono,
                        ModifiedBaseColorMode::ModifiedVsUnmodified,
                    ] {
                        ui.selectable_value(
                            &mut st.align.modified_base_color_mode,
                            mode,
                            modified_base_color_mode_label(mode),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
        });
        if !st.align.available_modification_codes.is_empty() {
            ui.label("Modification legend and visibility");
            for code in &st.align.available_modification_codes {
                let mut visible = !st.align.hidden_modification_codes.contains(code);
                let label = modification_code_label(*code);
                let color = rgba_to_color32(modification_code_color(*code));
                if ui
                    .checkbox(&mut visible, egui::RichText::new(label).color(color))
                    .changed()
                {
                    if visible {
                        st.align
                            .hidden_modification_codes
                            .retain(|candidate| candidate != code);
                    } else if st.align.hidden_modification_codes.len()
                        < MAX_HIDDEN_MODIFICATION_CODES
                    {
                        st.align.hidden_modification_codes.push(*code);
                    }
                }
            }
        }
    }

    /// Quality thresholds, SAM tag expression, biological predicates and the complete-read limit.
    #[allow(clippy::too_many_lines)]
    pub(super) fn alignment_filter_settings_ui(&self, ui: &mut egui::Ui, st: &mut ShellFrame) {
        ui.separator();
        ui.label("Filters");
        ui.horizontal(|ui| {
            ui.label("Minimum MAPQ");
            ui.add(egui::Slider::new(&mut st.align.minimum_mapq, 0..=60));
        });
        ui.horizontal(|ui| {
            ui.label("Minimum base quality");
            ui.add(egui::Slider::new(
                &mut st.align.minimum_base_quality,
                0..=60,
            ));
        });
        ui.horizontal(|ui| {
            ui.label("Minimum allele fraction");
            ui.add(
                egui::Slider::new(&mut st.align.minimum_allele_fraction, 0.0..=1.0)
                    .fixed_decimals(2),
            );
        });
        ui.checkbox(
            &mut st.align.quality_weighted_alleles,
            "Weight allele colours by base quality",
        );
        ui.checkbox(
            &mut st.align.show_coverage_center_line,
            "Show coverage centre line",
        );
        ui.checkbox(
            &mut st.align.include_secondary,
            "Include secondary alignments",
        );
        ui.checkbox(
            &mut st.align.include_supplementary,
            "Include supplementary alignments",
        );
        ui.horizontal(|ui| {
            ui.label("SAM tags");
            ui.add(
                egui::TextEdit::singleline(&mut st.align.filter_expression)
                    .hint_text("HP=1;RG=tumour;MM"),
            );
        });
        if !st
            .align
            .settings_filters
            .alignment_view_filter
            .all
            .is_empty()
        {
            ui.label("Active read filters");
            let mut remove = None;
            for (index, predicate) in st
                .align
                .settings_filters
                .alignment_view_filter
                .all
                .iter()
                .enumerate()
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label(alignment_predicate_label(predicate));
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove filter")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                st.align
                    .settings_filters
                    .alignment_view_filter
                    .all
                    .remove(index);
                st.align.typed_alignment_predicates_changed = true;
            }
        }
        ui.menu_button("Add biological filter…", |ui| {
            for (label, quick) in [
                (
                    "Contains substitution",
                    AlignmentQuickPredicate::Substitution,
                ),
                ("Contains insertion", AlignmentQuickPredicate::Insertion),
                ("Contains deletion", AlignmentQuickPredicate::Deletion),
                ("Contains any indel", AlignmentQuickPredicate::Indel),
                ("Contains soft clipping", AlignmentQuickPredicate::SoftClip),
                ("Contains splice", AlignmentQuickPredicate::Splice),
                ("Duplicate", AlignmentQuickPredicate::Duplicate),
                ("Secondary", AlignmentQuickPredicate::Secondary),
                ("Supplementary", AlignmentQuickPredicate::Supplementary),
            ] {
                let enabled = !quick.requires_reference() || st.align.reference_configured;
                let response = ui.add_enabled(enabled, egui::Button::new(label));
                let response = if enabled {
                    response
                } else {
                    response.on_disabled_hover_text("Requires a matching reference FASTA")
                };
                if response.clicked() {
                    let predicate = quick_alignment_predicate(
                        quick,
                        &self.session.panels.active().contig_name,
                        self.session
                            .panels
                            .active()
                            .viewport
                            .interval()
                            .start()
                            .saturating_add(
                                self.session.panels.active().viewport.interval().len() / 2,
                            ),
                    );
                    if !st
                        .align
                        .settings_filters
                        .alignment_view_filter
                        .all
                        .contains(&predicate)
                    {
                        st.align
                            .settings_filters
                            .alignment_view_filter
                            .all
                            .push(predicate);
                        st.align.typed_alignment_predicates_changed = true;
                    }
                    ui.close();
                }
            }
        });
        if !st
            .align
            .settings_filters
            .alignment_highlight_filter
            .all
            .is_empty()
        {
            ui.label("Highlighted read predicates");
            let mut remove = None;
            for (index, predicate) in st
                .align
                .settings_filters
                .alignment_highlight_filter
                .all
                .iter()
                .enumerate()
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label(alignment_predicate_label(predicate));
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove highlight")
                        .clicked()
                    {
                        remove = Some(index);
                    }
                });
            }
            if let Some(index) = remove {
                st.align
                    .settings_filters
                    .alignment_highlight_filter
                    .all
                    .remove(index);
                st.align.typed_alignment_predicates_changed = true;
                st.align.alignment_highlight_predicates_changed = true;
            }
        }
        ui.horizontal(|ui| {
            ui.label("Complete read limit").on_hover_text(
                "At or below the effective limit, every eligible viewport read is shown. Above it, RGV shows no individual reads and asks you to zoom in or raise the limit. The default is 50,000; raising it can increase memory, query/layout time, and GPU work. Values in the millions are experimental and can exhaust available memory. Multi-panel or multi-track workspace allocation can make the effective limit lower than this requested value.",
            );
            ui.add(
                egui::Slider::new(
                    &mut st.align.complete_read_limit,
                    1_000
                        ..=rgv_core::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT
                            as u32,
                )
                .logarithmic(true),
            );
        });
        if ui.button("Apply filters").clicked() {
            st.align.apply_filter = true;
        }
        if let Some(error) = &st.align.filter_error {
            ui.colored_label(crate::theme::adapt(egui::Color32::DARK_RED), error);
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn auxiliary_track_settings_ui(ui: &mut egui::Ui, st: &mut ShellFrame, id: TrackId) {
        let track_control_count = st.tracks.track_controls.len();
        if let Some((index, control)) = st
            .tracks
            .track_controls
            .iter_mut()
            .enumerate()
            .find(|(_, control)| control.id == id)
        {
            ui.heading("Track");
            sample_association_ui(
                ui,
                &mut st.align.edited_track_sample_key,
                st.lists.sample_metadata.as_ref(),
            );
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut control.name);
            });
            if ui
                .checkbox(&mut control.visible, "Show annotation content")
                .changed()
            {
                control.collapsed = !control.visible;
            }
            ui.add_enabled(
                !st.tracks.natural_row_height_tracks.contains(&id),
                egui::Slider::new(&mut control.height, 40.0..=600.0).text("Height"),
            )
            .on_disabled_hover_text("This matrix height follows its sample count and row height");
            let count = st
                .tracks
                .track_counts
                .iter()
                .find(|(candidate, _)| *candidate == id)
                .map_or(0, |(_, count)| *count);
            ui.label(format!("{count} records at the current locus"));
            if let Some(identity) = st.tracks.builtin_track_identities.get(&id) {
                ui.separator();
                match identity.asset_role {
                    LandmarkAssetRole::RepresentativeGenes => {
                        if ui.button("Show all transcripts").clicked() {
                            st.inspect.show_all_transcripts_requested =
                                Some(identity.bundle_id.clone());
                        }
                    }
                    LandmarkAssetRole::DetailedTranscripts => {
                        if ui.button("Return to representative genes").clicked() {
                            st.inspect.return_to_representative_genes_requested =
                                Some(identity.bundle_id.clone());
                        }
                    }
                    _ => {}
                }
                ui.small(format!(
                    "Built-in bundle {} · {} · {}",
                    identity.bundle_id,
                    identity.bundle_version,
                    &identity.sha256[..12]
                ));
            }
            if let Some((_, settings)) = st
                .tracks
                .feature_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                ui.separator();
                let label = ui.label("Feature display");
                egui::ComboBox::from_id_salt(("feature-display-mode", id.get()))
                    .selected_text(match settings.display_mode {
                        FeatureDisplayMode::Collapsed => "Collapsed",
                        FeatureDisplayMode::Squished => "Squished",
                        FeatureDisplayMode::Expanded => "Expanded",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut settings.display_mode,
                            FeatureDisplayMode::Collapsed,
                            "Collapsed",
                        );
                        ui.selectable_value(
                            &mut settings.display_mode,
                            FeatureDisplayMode::Squished,
                            "Squished",
                        );
                        ui.selectable_value(
                            &mut settings.display_mode,
                            FeatureDisplayMode::Expanded,
                            "Expanded",
                        );
                    })
                    .response
                    .labelled_by(label.id);
            }
            if let Some((_, settings)) = st
                .tracks
                .variant_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                let query = st
                    .tracks
                    .variant_settings_queries
                    .iter()
                    .find(|(track_id, _)| *track_id == id)
                    .map(|(_, query)| query.as_ref());
                ui.separator();
                variant_track_settings_ui(
                    ui,
                    settings,
                    query,
                    &mut st.tracks.variant_settings_sample_search,
                    &mut st.tracks.variant_predicate_scope,
                    &mut st.tracks.variant_predicate_key,
                    &mut st.tracks.variant_predicate_operator,
                    &mut st.tracks.variant_predicate_value,
                    &mut st.tracks.variant_predicate_missing,
                    &mut st.tracks.variant_predicate_error,
                );
            }
            if let Some((_, settings)) = st
                .tracks
                .gwas_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                ui.separator();
                gwas_track_settings_ui(ui, settings);
            }
            if let Some((_, copy_settings)) = st
                .tracks
                .copy_number_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                let (_, quantitative_settings) = st
                    .tracks
                    .quantitative_settings_controls
                    .iter_mut()
                    .find(|(track_id, _)| *track_id == id)
                    .expect("copy-number tracks have quantitative settings");
                ui.separator();
                copy_number_track_settings_ui(
                    ui,
                    copy_settings,
                    quantitative_settings,
                    &st.tracks.copy_number_sort_locus,
                );
            } else if let Some((_, settings)) = st
                .tracks
                .mutation_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                ui.separator();
                mutation_track_settings_ui(ui, settings);
            } else if let Some((_, settings)) = st
                .tracks
                .quantitative_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                ui.separator();
                quantitative_track_settings_ui(
                    ui,
                    settings,
                    id,
                    &st.tracks.quantitative_track_choices,
                );
            }
            if let Some((_, settings)) = st
                .tracks
                .interaction_settings_controls
                .iter_mut()
                .find(|(track_id, _)| *track_id == id)
            {
                ui.separator();
                interaction_track_settings_ui(ui, settings);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(index > 0, egui::Button::new("Move up"))
                    .clicked()
                {
                    st.tracks.track_move = Some((id, index - 1));
                }
                if ui
                    .add_enabled(
                        index + 1 < track_control_count,
                        egui::Button::new("Move down"),
                    )
                    .clicked()
                {
                    st.tracks.track_move = Some((id, index + 1));
                }
            });
        }
    }
}
