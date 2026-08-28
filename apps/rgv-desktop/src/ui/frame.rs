//! One borrow-scoped egui frame transaction.
//!
//! Widget families and their typed drafts/actions live in sibling modules. This
//! module deliberately keeps the remaining shell composition and action commit
//! in one transaction so egui closures never retain application borrows across
//! engine polling or GPU submission.

use super::super::*;

fn alignment_subtrack_visible(filters: &SessionFilters, subtrack: AlignmentSubtrack) -> bool {
    match subtrack {
        AlignmentSubtrack::Reference => filters.show_reference,
        AlignmentSubtrack::Coverage => filters.show_coverage,
        AlignmentSubtrack::Sashimi => filters.show_junctions,
        AlignmentSubtrack::Modifications => filters.show_modification_frequency,
        AlignmentSubtrack::Reads => filters.show_reads,
    }
}

fn alignment_subtrack_menu(
    ui: &mut egui::Ui,
    track_id: TrackId,
    filters: &SessionFilters,
    read_content_visible: bool,
    include_reference: bool,
    actions: &mut Vec<TrackUiAction>,
) {
    ui.label("Subtracks");
    for subtrack in AlignmentSubtrack::ALL {
        if subtrack == AlignmentSubtrack::Reference && !include_reference {
            continue;
        }
        let mut visible = if subtrack == AlignmentSubtrack::Reads {
            read_content_visible
        } else {
            alignment_subtrack_visible(filters, subtrack)
        };
        if ui.checkbox(&mut visible, subtrack.label()).changed() {
            if subtrack == AlignmentSubtrack::Reads {
                actions.push(TrackUiAction::SetNodeVisibility(
                    PresentationNodeId::Track {
                        channel: ui::TrackPane::Reads,
                        track_id,
                    },
                    visible,
                ));
            } else {
                actions.push(TrackUiAction::ToggleAlignmentSubtrack(track_id, subtrack));
            }
        }
    }
}

#[derive(Clone)]
struct ReadPanelScrollbarDraft {
    track_id: TrackId,
    top: f32,
    bottom: f32,
    maximum: f32,
    offset: f32,
    density: f32,
    preview_fraction: Option<f32>,
    overview: ui::ScrollbarOverview,
    interaction: ui::ScrollbarInteraction,
}

#[derive(Clone)]
struct WorkspaceScrollbarsDraft {
    index: usize,
    panel_id: PanelId,
    annotation_maximum: f32,
    annotation_offset: f32,
    annotation_overview: Option<ui::ScrollbarOverview>,
    annotation_clip_top: f32,
    annotation_clip_bottom: f32,
    annotation_interaction: ui::ScrollbarInteraction,
    read_stack_maximum: f32,
    read_stack_offset: f32,
    read_stack_interaction: ui::ScrollbarInteraction,
    read_clip_top: f32,
    read_panels: Vec<ReadPanelScrollbarDraft>,
}

fn pane_vertical_bounds(layout: &ui::PaneLayoutSnapshot, fallback: f32) -> (f32, f32) {
    let top = layout.pane_rect.top();
    let bottom = layout.pane_rect.bottom();
    if top.is_finite() && bottom.is_finite() && bottom >= top {
        (top, bottom)
    } else {
        (fallback, fallback)
    }
}

/// Rebase semantic genomic text from the display list that was committed at
/// the start of an egui pass onto the shell geometry computed during that
/// pass. GPU rectangles are rebuilt before submission; this affine rebase
/// keeps egui-rendered text on that same geometry revision without replaying
/// pointer input through a second UI pass. Text belonging to a removed panel
/// is deliberately omitted, and a newly added panel receives text on the next
/// display-list rebuild.
fn remap_genomic_texts_for_layout(
    texts: &[rgv_layout::TextPrimitive],
    previous: &ui::WorkspaceLayoutSnapshot,
    current: &ui::WorkspaceLayoutSnapshot,
) -> Vec<rgv_layout::TextPrimitive> {
    if previous == current {
        return texts.to_vec();
    }
    let vertical_delta = current.track_viewport.top() - previous.track_viewport.top();
    texts
        .iter()
        .filter_map(|text| {
            let previous_panel = previous
                .panels
                .iter()
                .find(|panel| text.x >= panel.canvas.left() && text.x <= panel.canvas.right())?;
            let current_panel = current.panel(previous_panel.panel_id)?;
            let fraction = if previous_panel.canvas.width() > f32::EPSILON {
                ((text.x - previous_panel.canvas.left()) / previous_panel.canvas.width())
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut remapped = text.clone();
            remapped.x = current_panel.canvas.left() + fraction * current_panel.canvas.width();
            remapped.y += vertical_delta;
            Some(remapped)
        })
        .collect()
}

impl Viewer {
    #[allow(clippy::too_many_lines)]
    pub(super) fn render_shell_frame(
        &mut self,
        raw_input: egui::RawInput,
    ) -> (egui::FullOutput, Duration) {
        let interval = self.panels.active().viewport.interval();
        let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
        let detail = self.panels.active().viewport.detail_level();
        let contig_name = self.panels.active().contig_name.clone();
        let navigation = &self.panels.active().navigation_presentation;
        let data_status = if navigation.is_unsettled() {
            if navigation.notice_visible {
                "Updating view…".to_owned()
            } else if navigation.source_status.is_empty() {
                self.data_status.clone()
            } else {
                navigation.source_status.clone()
            }
        } else {
            self.data_status.clone()
        };
        let announce_status = self.announced_status != data_status;
        let active_panel_id = self.panels.active().id;
        let diagnostics = self.diagnostics_snapshot();
        let inspector = self.inspector_snapshot();
        let pileup_inspector = self.pileup_inspector_snapshot();
        let modification_inspector = self.modification_inspector_snapshot();
        let junction_inspector = self.junction_inspector_snapshot();
        let feature_inspector = self.feature_inspector_snapshot();
        let variant_inspector = self.variant_inspector_snapshot();
        let variant_alignment_evidence = variant_inspector
            .as_ref()
            .map_or_else(Vec::new, |inspector| {
                self.variant_alignment_evidence(inspector)
            });
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
        let mut show_all_transcripts_requested = None;
        let mut return_to_representative_genes_requested = None;
        let builtin_track_identities = self
            .tracks
            .tracks()
            .iter()
            .filter_map(|track| track.built_in.clone().map(|identity| (track.id, identity)))
            .collect::<BTreeMap<_, _>>();
        let mut variant_sample_search = self.variant_sample_search.clone();
        let mut variant_settings_sample_search = self.variant_settings_sample_search.clone();
        let mut variant_predicate_scope = self.variant_predicate_scope;
        let mut variant_predicate_key = self.variant_predicate_key.clone();
        let mut variant_predicate_operator = self.variant_predicate_operator;
        let mut variant_predicate_value = self.variant_predicate_value.clone();
        let mut variant_predicate_missing = self.variant_predicate_missing;
        let mut variant_predicate_error = None;
        let primary_alignment_id = self.primary_alignment_track_id();
        let editing_alignment_id = match self.open_panel {
            Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(id))) => Some(id),
            _ => None,
        };
        let editing_auxiliary_id = match self.open_panel {
            Some(UiPanel::TrackSettings(TrackFrameKey::Auxiliary(id))) => Some(id),
            _ => None,
        };
        let mut edited_track_sample_key = editing_alignment_id
            .and_then(|id| {
                self.alignment_tracks
                    .iter()
                    .find(|track| track.config.id == id)
                    .and_then(|track| track.config.sample_key.clone())
            })
            .or_else(|| {
                editing_auxiliary_id.and_then(|id| {
                    self.tracks
                        .tracks()
                        .iter()
                        .find(|track| track.id == id)
                        .and_then(|track| track.sample_key.clone())
                })
            });
        let settings_alignment_id = editing_alignment_id.unwrap_or(primary_alignment_id);
        let settings_query = self
            .alignment_query_for_track(settings_alignment_id)
            .cloned();
        let available_modification_codes = editing_alignment_id.map_or_else(Vec::new, |track_id| {
            self.available_modification_codes(track_id)
        });
        let settings_controller = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == settings_alignment_id);
        let settings_filters = if settings_alignment_id == primary_alignment_id {
            self.live_session_filters()
        } else {
            settings_controller.map_or_else(SessionFilters::default, |track| {
                track.config.filters.clone()
            })
        };
        let settings_filter_editor = if settings_alignment_id == primary_alignment_id {
            self.primary_filter_editor().clone()
        } else {
            settings_controller.map_or_else(
                || self.primary_filter_editor().clone(),
                |track| track.filter_editor.clone(),
            )
        };
        let mut minimum_mapq = settings_filter_editor.minimum_mapq;
        let mut minimum_base_quality = settings_filters.minimum_base_quality;
        let mut minimum_allele_fraction = settings_filters.minimum_allele_fraction;
        let mut quality_weighted_alleles = settings_filters.quality_weighted_alleles;
        let mut show_reference = settings_filters.show_reference;
        let mut reference_reverse_complement = self.reference_reverse_complement;
        let mut reference_translation_frames = self.reference_translation_frames.clone();
        let mut reference_genetic_code = self.reference_genetic_code;
        let mut show_coverage = settings_filters.show_coverage;
        let mut show_junctions = settings_filters.show_junctions;
        let mut minimum_junction_depth = settings_filters.minimum_junction_depth;
        let mut minimum_junction_flank = settings_filters.minimum_junction_flank;
        let mut junction_display_mode = settings_filters.junction_display_mode;
        let mut sashimi_scale_mode = settings_filters.sashimi_scale_mode;
        let mut sashimi_fixed_scale_maximum = settings_filters.sashimi_fixed_scale_maximum;
        let mut sashimi_count_mode = settings_filters.sashimi_count_mode;
        let mut show_junction_labels = settings_filters.show_junction_labels;
        let initial_show_reads = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Reads, settings_alignment_id)
            .is_none_or(|node| node.content_visible);
        let mut show_reads = initial_show_reads;
        let mut use_fixed_coverage_scale = settings_filters.coverage_scale_maximum.is_some();
        let mut coverage_scale_maximum = settings_filters.coverage_scale_maximum.unwrap_or(100);
        let mut coverage_scale_transform = settings_filters.coverage_scale_transform;
        let mut show_coverage_center_line = settings_filters.show_coverage_center_line;
        let mut alignment_density = settings_filters.alignment_density;
        let mut inspector_activation = settings_filters.inspector_activation;
        let mut read_base_display_mode = settings_filters.read_base_display_mode;
        let mut shade_read_bases_by_quality = settings_filters.shade_read_bases_by_quality;
        let mut minimum_indel_size = settings_filters.minimum_indel_size;
        let mut minimum_event_label_size = settings_filters.minimum_event_label_size;
        let mut show_modified_bases = settings_filters.show_modified_bases;
        let mut show_modification_frequency = settings_filters.show_modification_frequency;
        let mut minimum_modification_probability =
            settings_filters.minimum_modification_probability;
        let mut modified_base_color_mode = settings_filters.modified_base_color_mode;
        let mut hidden_modification_codes = settings_filters
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value))
            .collect::<Vec<_>>();
        let mut include_secondary = settings_filter_editor.include_secondary;
        let mut include_supplementary = settings_filter_editor.include_supplementary;
        let mut filter_expression = settings_filter_editor.expression.clone();
        let filter_error = settings_filter_editor.error.clone();
        let mut apply_filter = false;
        let (bulk_alignment_ids, bulk_alignment_pane) = editing_alignment_id.map_or_else(
            || (Vec::new(), None),
            |id| {
                let panel = self.panels.active();
                if panel.selected_alignment_tracks.len() > 1
                    && panel.selected_alignment_tracks.contains(&id)
                {
                    (
                        panel.selected_alignment_tracks.clone(),
                        Some(ui::TrackPane::Annotation),
                    )
                } else if panel.selected_read_tracks.len() > 1
                    && panel.selected_read_tracks.contains(&id)
                {
                    (
                        panel.selected_read_tracks.clone(),
                        Some(ui::TrackPane::Reads),
                    )
                } else {
                    (Vec::new(), None)
                }
            },
        );
        let mut apply_bulk_alignment_appearance = false;
        let mut bulk_alignment_visibility = None;
        let mut bulk_alignment_collapsed = None;
        let mut complete_read_limit = u32::try_from(
            settings_controller.map_or(self.primary_alignment_limit(), |track| {
                track.config.maximum_alignments
            }),
        )
        .unwrap_or(u32::MAX)
        .clamp(1, MAX_CONFIGURABLE_COMPLETE_READ_LIMIT as u32);
        let mut alignment_track_controls = self
            .alignment_tracks
            .iter()
            .map(|track| {
                let state = self
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track.config.id);
                let visible = state.is_none_or(|state| state.content_visible);
                let height = state
                    .and_then(|state| state.preferred_height)
                    .or(track.config.annotation_height)
                    .unwrap_or(120.0);
                TrackControl {
                    id: track.config.id,
                    name: if track.config.id == primary_alignment_id {
                        self.primary_alignment_name().to_owned()
                    } else {
                        track.config.name.clone()
                    },
                    visible,
                    collapsed: !visible,
                    height,
                    initial_visible: visible,
                    initial_height: height,
                }
            })
            .collect::<Vec<_>>();
        let alignment_subtrack_controls = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.config.id,
                    self.alignment_filters_for_track(track.config.id),
                )
            })
            .collect::<Vec<_>>();
        let mut track_controls = self
            .tracks
            .tracks()
            .iter()
            .map(|track| {
                let state = self
                    .track_presentation
                    .pane_track_state(ui::TrackPane::Annotation, track.id);
                let visible = state.is_none_or(|state| state.content_visible);
                let height = state
                    .and_then(|state| state.preferred_height)
                    .unwrap_or(track.height);
                TrackControl {
                    id: track.id,
                    name: track.name.clone(),
                    visible,
                    collapsed: !visible,
                    height,
                    initial_visible: visible,
                    initial_height: height,
                }
            })
            .collect::<Vec<_>>();
        let mut feature_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::Feature | TrackKind::GeneModel))
            .map(|track| (track.id, track.feature.clone()))
            .collect::<Vec<_>>();
        let natural_row_height_tracks = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::CopyNumber | TrackKind::Mutation))
            .map(|track| track.id)
            .collect::<BTreeSet<_>>();
        let mut variant_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant)
            .map(|track| (track.id, track.variant.clone()))
            .collect::<Vec<_>>();
        let mut gwas_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Gwas)
            .map(|track| (track.id, track.gwas.clone()))
            .collect::<Vec<_>>();
        let mut quantitative_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::Quantitative | TrackKind::CopyNumber))
            .map(|track| (track.id, track.quantitative.clone()))
            .collect::<Vec<_>>();
        let mut copy_number_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber)
            .map(|track| (track.id, track.copy_number.clone()))
            .collect::<Vec<_>>();
        let mut mutation_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Mutation)
            .map(|track| (track.id, track.mutation.clone()))
            .collect::<Vec<_>>();
        let copy_number_sort_locus = CopyNumberSortLocus {
            contig: self.panels.active().contig_name.clone(),
            start: self.panels.active().viewport.interval().start(),
            end: self.panels.active().viewport.interval().end(),
        };
        let quantitative_track_choices = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Quantitative)
            .map(|track| (track.id, track.name.clone()))
            .collect::<Vec<_>>();
        let mut interaction_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Interaction)
            .map(|track| (track.id, track.interaction.clone()))
            .collect::<Vec<_>>();
        let variant_settings_queries = self
            .panels
            .active()
            .track_snapshots
            .iter()
            .filter_map(|snapshot| match &snapshot.data {
                TrackSnapshotData::Variants(query) => Some((snapshot.id, Arc::clone(query))),
                TrackSnapshotData::Features(_)
                | TrackSnapshotData::Quantitative(_)
                | TrackSnapshotData::CopyNumber(_)
                | TrackSnapshotData::Mutations(_)
                | TrackSnapshotData::Interactions(_)
                | TrackSnapshotData::Gwas(_) => None,
            })
            .collect::<Vec<_>>();
        let track_counts = self
            .panels
            .active()
            .track_snapshots
            .iter()
            .map(|snapshot| (snapshot.id, snapshot_record_count(snapshot)))
            .collect::<Vec<_>>();
        let mut track_move = None;
        let mut color_mode = settings_filters.alignment_color_mode;
        let mut group_mode = settings_filters.alignment_group_mode;
        let mut group_order = settings_filters.alignment_group_order;
        let mut sort_mode = settings_filters.alignment_sort_mode;
        let mut sort_order = settings_filters.alignment_sort_order;
        let mut read_stacking_mode = settings_filters.read_stacking_mode;
        let mut compact_repack_requested = false;
        let mut view_as_pairs = settings_filters.view_as_pairs;
        let pair_summary = settings_query
            .as_deref()
            .map_or_else(Default::default, |query| query.pairs);
        let inferred_pair_bounds = pair_summary.inferred_bounds.unwrap_or_default();
        let mut override_pair_bounds = settings_filters.minimum_template_length.is_some()
            || settings_filters.maximum_template_length.is_some();
        let mut minimum_template_length = settings_filters
            .minimum_template_length
            .or(inferred_pair_bounds.minimum)
            .unwrap_or_default();
        let mut maximum_template_length = settings_filters
            .maximum_template_length
            .or(inferred_pair_bounds.maximum)
            .unwrap_or(1_000)
            .max(minimum_template_length);
        let mut overlapping_mate_policy = settings_filters.overlapping_mate_policy;
        let mut style_tag_editor = persisted_style_tag(color_mode, group_mode, sort_mode);
        let mut read_name_editor = self.read_name_editor.clone();
        let mut select_read_name_requested = false;
        let pending_command = self.pending_command.take();
        let mut open_alignment_requested = pending_command == Some(UiCommand::OpenAlignment);
        let mut open_reference_requested = false;
        let mut open_genome_config_requested = false;
        let mut restore_default_genes_requested = false;
        let mut open_landmark_folder_requested = false;
        let mut add_track_requested = pending_command == Some(UiCommand::AddTrack);
        let mut open_session_requested = pending_command == Some(UiCommand::OpenSession);
        let mut save_session_requested = pending_command == Some(UiCommand::SaveSession);
        let mut save_session_as_requested = false;
        let mut export_png_requested = pending_command == Some(UiCommand::ExportPng);
        let mut clear_cache_requested = pending_command == Some(UiCommand::ClearCache);
        let mut settings_requested = pending_command == Some(UiCommand::Settings);
        let focus_locus_requested = pending_command == Some(UiCommand::FocusLocus);
        let mut locus_editor = self.locus_editor.clone();
        let mut navigate_requested = false;
        let mut command_palette_open = self.open_panel == Some(UiPanel::CommandPalette);
        let mut exit_confirmation_open = self.exit_confirmation_open;
        let mut user_exit_requested = false;
        let mut command_search = self.command_search.clone();
        let mut palette_command = None;
        let mut remote_source_open = self.open_panel == Some(UiPanel::RemoteSource);
        let mut remote_source_editor = self.remote_source_editor.clone();
        let mut remote_source_request = None;
        let mut settings_open = self.open_panel == Some(UiPanel::Settings);
        let mut project_issues_open = self.open_panel == Some(UiPanel::ProjectIssues);
        let unresolved_session_sources = self.unresolved_session_sources.clone();
        let session_migration_notices = self.session_migration_notices.clone();
        let mut igv_report_open = self.open_panel == Some(UiPanel::IgvImportReport);
        let igv_import_report = self.igv_import_report.clone();
        let mut recovery_open = self.recovery_open;
        let mut recovery_requested = None;
        let mut dismiss_recovery = false;
        let mut cache_budget_mib = self.settings.cache_budget_mib;
        let mut diagnostics_opt_in = self.settings.diagnostics_opt_in;
        let mut autosave_enabled = self.settings.autosave_enabled;
        let mut autosave_interval_seconds = self.settings.autosave_interval_seconds;
        let mut autosave_retention = self.settings.autosave_retention;
        let mut interface_scale_percent = self.settings.interface_scale_percent;
        let mut high_contrast = self.settings.high_contrast;
        let mut reduced_motion = self.settings.reduced_motion;
        let recent_loci = self.settings.recent_loci.clone();
        let current_session_path = self.current_session_path.clone();
        let mut clear_recent_requested = false;
        let mut recent_alignment_requested = None;
        let mut recent_session_requested = None;
        let mut recent_locus_requested = None;
        let committed_canvas_geometry = self.canvas_geometry();
        let mut new_canvas_geometry = committed_canvas_geometry;
        let previous_workspace_layout = self.workspace_layout.clone();
        let mut new_workspace_layout = previous_workspace_layout.clone();
        let mut native_properties_resize_active = false;
        let pane_layout = self.panels.active().pane_layout.clone();
        let track_frames = pane_layout.frames().cloned().collect::<Vec<_>>();
        let annotation_track_count = self
            .track_presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == ui::TrackPane::Annotation)
            .count()
            + usize::from(self.reference_engine.is_some());
        let read_track_count = self
            .track_presentation
            .pane_tracks
            .iter()
            .filter(|node| node.channel == ui::TrackPane::Reads)
            .count();
        let shared_annotation_maximum = self.panels.maximum_annotation_scroll_points();
        let shared_annotation_offset = if self.track_presentation.annotation_section_expanded {
            self.panels
                .annotation_scroll_points
                .clamp(0.0, shared_annotation_maximum)
        } else {
            self.panels.annotation_scroll_points
        };
        let selected_annotation_track_ids = self
            .panels
            .active()
            .selected_presentation_nodes
            .iter()
            .filter_map(|node| match node {
                PresentationNodeId::Track {
                    channel: ui::TrackPane::Annotation,
                    track_id,
                } => Some(*track_id),
                PresentationNodeId::Reference
                | PresentationNodeId::Track { .. }
                | PresentationNodeId::Group { .. } => None,
            })
            .collect::<Vec<_>>();
        let track_groups = self.alignment_track_group_chrome();
        let bulk_alignment_group = track_groups.iter().find(|group| {
            bulk_alignment_pane == Some(group.pane)
                && bulk_alignment_ids.len() > 1
                && bulk_alignment_ids.len() == group.members.len()
                && bulk_alignment_ids
                    .iter()
                    .all(|id| group.members.contains(id))
        });
        let mut workspace_scrollbars = self
            .panels
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let annotation_maximum = shared_annotation_maximum;
                let (pane_clip_top, pane_clip_bottom) = pane_vertical_bounds(
                    &panel.pane_layout.annotation,
                    committed_canvas_geometry.viewport.top(),
                );
                let annotation_clip_top = panel
                    .scroll_display_cache
                    .as_ref()
                    .map_or(pane_clip_top, |cache| cache.annotation_clip_top);
                let annotation_clip_bottom = panel
                    .scroll_display_cache
                    .as_ref()
                    .map_or(pane_clip_bottom, |cache| cache.annotation_clip_bottom);
                let read_panels = panel
                    .scroll_display_cache
                    .as_ref()
                    .map(|cache| {
                        cache
                            .read_panels
                            .iter()
                            .map(|read_panel| {
                                let state = panel.read_panel_scroll_state(read_panel.track_id);
                                let maximum =
                                    (state.content_bottom_points - read_panel.clip_bottom).max(0.0);
                                let preview_fraction = self
                                    .vertical_overview_drag
                                    .filter(|drag| {
                                        drag.density_dragging
                                            && drag.panel_id == panel.id
                                            && drag.track_id == read_panel.track_id
                                    })
                                    .map(|drag| drag.virtual_scroll_fraction)
                                    .or_else(|| {
                                        (maximum <= f32::EPSILON)
                                            .then_some(state.latent_scroll_fraction)
                                            .flatten()
                                    });
                                ReadPanelScrollbarDraft {
                                    track_id: read_panel.track_id,
                                    top: read_panel.clip_top,
                                    bottom: read_panel.clip_bottom,
                                    maximum,
                                    offset: state.scroll_points.clamp(0.0, maximum),
                                    density: self.alignment_density_for_panel_track(
                                        panel.id,
                                        read_panel.track_id,
                                    ),
                                    preview_fraction,
                                    overview: read_panel.overview.clone(),
                                    interaction: ui::ScrollbarInteraction::default(),
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                WorkspaceScrollbarsDraft {
                    index,
                    panel_id: panel.id,
                    annotation_maximum,
                    annotation_offset: shared_annotation_offset,
                    annotation_overview: panel
                        .scroll_display_cache
                        .as_ref()
                        .map(|cache| cache.annotation_overview.clone()),
                    annotation_clip_top,
                    annotation_clip_bottom,
                    annotation_interaction: ui::ScrollbarInteraction::default(),
                    read_stack_maximum: (panel.read_stack_content_bottom_points
                        - committed_canvas_geometry.viewport.bottom())
                    .max(0.0),
                    read_stack_offset: panel.read_stack_scroll_points,
                    read_stack_interaction: ui::ScrollbarInteraction::default(),
                    read_clip_top: panel.pane_layout.reads.pane_rect.top(),
                    read_panels,
                }
            })
            .collect::<Vec<_>>();
        let (pane_annotation_top, pane_annotation_bottom) = pane_vertical_bounds(
            &pane_layout.annotation,
            committed_canvas_geometry.viewport.top(),
        );
        let genomic_texts = self
            .composite_display_list
            .texts
            .iter()
            .filter(|text| {
                self.workspace_layout
                    .panels
                    .iter()
                    .find(|layout| {
                        text.x >= layout.canvas.left() && text.x <= layout.canvas.right()
                    })
                    .and_then(|layout| self.panels.get(layout.panel_id))
                    .is_some_and(|panel| panel.navigation_presentation.permits_staged_display())
            })
            .cloned()
            .collect::<Vec<_>>();
        let selected_track = self.panels.active().selected_track;
        let assembly = self.assembly.clone();
        let assembly_label = assembly.context_label();
        let contigs = assembly
            .dictionary
            .iter()
            .map(|contig| (contig.id(), contig.name().to_owned()))
            .collect::<Vec<_>>();
        let mut requested_contig = None;
        let mut previous_feature_requested = false;
        let mut next_feature_requested = false;
        let annotation_search_results = self.annotation_search_results.clone();
        let mut locus_search_changed = false;
        let mut search_popup_position = None;
        let mut annotation_hit_requested = None;
        let mut navigate_back_requested = false;
        let mut navigate_forward_requested = false;
        let mut navigate_home_requested = false;
        let mut zoom_in_requested = false;
        let mut zoom_out_requested = false;
        let mut requested_span = None;
        let panel_controls = self
            .panels
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                (
                    index,
                    panel.id,
                    format!(
                        "{}:{}-{}",
                        panel.contig_name,
                        panel.viewport.interval().start().saturating_add(1),
                        panel.viewport.interval().end()
                    ),
                )
            })
            .collect::<Vec<_>>();
        let panel_contexts = self
            .panels
            .iter()
            .map(|panel| {
                let mut genome_overview = ui::GenomeOverviewData::default();
                let mut query_fallback_overview = ui::GenomeOverviewData::default();
                for track in &self.alignment_tracks {
                    let index_bins = track
                        .dictionary
                        .as_ref()
                        .and_then(|dictionary| dictionary.id_by_name(&panel.contig_name))
                        .and_then(|contig| {
                            track
                                .index_overview
                                .as_ref()
                                .and_then(|overview| overview.contig_bins(contig))
                        });
                    if let Some(index_bins) = index_bins {
                        genome_overview.add_index_density(index_bins);
                    }
                    let query = track
                        .projection(panel.id)
                        .and_then(|projection| projection.current_query.as_ref());
                    let query_matches_contig = track
                        .dictionary
                        .as_ref()
                        .and_then(|dictionary| dictionary.id_by_name(&panel.contig_name))
                        .zip(query.map(|query| query.interval.contig()))
                        .is_some_and(|(expected, actual)| expected == actual);
                    if query_matches_contig && let Some(query) = query {
                        query_fallback_overview.add_depths(
                            panel.contig_length,
                            query.interval,
                            query.pileup.bins.iter().map(|bin| bin.depth),
                        );
                    }
                }
                query_fallback_overview.normalize_logarithmically();
                genome_overview.merge_max(&query_fallback_overview);
                (
                    panel.id,
                    panel.contig_name.clone(),
                    panel.contig_length,
                    panel.viewport,
                    genome_overview,
                )
            })
            .collect::<Vec<_>>();
        let mut panel_to_activate = None;
        let mut panel_to_close = None;
        let mut panel_to_move = None;
        let mut overview_interaction = ui::OverviewInteraction::default();
        let mut overview_panel_id = active_panel_id;
        let mut genomic_canvas_layer_id = None;
        let mut pane_divider_rect = None;
        let mut diagnostics_open = self.diagnostics_open;
        let mut metadata_open = self.open_panel == Some(UiPanel::Metadata);
        let mut regions_open = self.open_panel == Some(UiPanel::Regions);
        let mut review_open = self.open_panel == Some(UiPanel::Review);
        let mut regions = self.regions_of_interest.clone();
        let mut review_items = self.review_items.clone();
        let mut review_status_filter = self.review_status_filter;
        let mut add_current_region = false;
        let mut add_current_review = false;
        let mut add_selected_feature_to_review = false;
        let mut region_import_requested = false;
        let mut region_export_requested = false;
        let mut review_import_requested = false;
        let mut review_export_requested = false;
        let mut previous_review_requested = false;
        let mut next_review_requested = false;
        let mut managed_locus_requested = None;
        let mut region_to_delete = None;
        let mut review_to_delete = None;
        let mut region_to_relocate = None;
        let mut review_to_relocate = None;
        let mut sample_metadata = self.sample_metadata.clone();
        let mut metadata_filter_column = self.metadata_filter_column;
        let mut metadata_filter_value = self.metadata_filter_value.clone();
        let mut metadata_selected_tracks = self.metadata_selected_tracks.clone();
        let mut metadata_bulk_height = self.metadata_bulk_height;
        let mut metadata_remove_armed = self.metadata_remove_armed;
        let mut metadata_import_requested = false;
        let mut metadata_remove_requested = false;
        let mut metadata_select_requested = false;
        let mut metadata_bulk_visibility = None;
        let mut metadata_bulk_collapsed = None;
        let mut metadata_bulk_height_requested = false;
        let mut metadata_bulk_rename_requested = false;
        let mut metadata_bulk_alignment_settings_requested = false;
        let mut metadata_bulk_auxiliary_appearance_requested = false;
        let mut metadata_bulk_quantitative_autoscale_requested = false;
        let mut metadata_bulk_quantitative_overlay_requested = false;
        let mut metadata_bulk_quantitative_separate_requested = false;
        let mut metadata_bulk_remove_requested = false;
        let mut metadata_track_rows = self
            .alignment_tracks
            .iter()
            .map(|track| {
                (
                    track.config.id,
                    track.config.name.clone(),
                    track.config.sample_key.clone(),
                    TrackKind::Alignment,
                )
            })
            .chain(self.tracks.tracks().iter().map(|track| {
                (
                    track.id,
                    track.name.clone(),
                    track.sample_key.clone(),
                    track.kind,
                )
            }))
            .collect::<Vec<_>>();
        let annotation_presentation_state = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Annotation, settings_alignment_id);
        let mut alignment_visible =
            annotation_presentation_state.is_none_or(|state| state.content_visible);
        let mut alignment_collapsed = !alignment_visible;
        let mut alignment_name = settings_controller.map_or_else(
            || self.primary_alignment_name().to_owned(),
            |track| track.config.name.clone(),
        );
        let mut alignment_height = annotation_presentation_state
            .and_then(|state| state.preferred_height)
            .unwrap_or(120.0);
        let mut track_ui_actions = Vec::new();
        let rendered_track_settings_panel = match self.open_panel {
            Some(UiPanel::TrackSettings(key)) => Some(key),
            _ => None,
        };
        let mut track_settings_panel = match self.open_panel {
            Some(UiPanel::TrackSettings(key)) => Some(key),
            _ => None,
        };
        let status_locus = format!("{contig_name}:{}-{}", interval.start() + 1, interval.end());
        let selection_summary = inspector
            .as_ref()
            .or(pileup_inspector.as_ref())
            .or(modification_inspector.as_ref())
            .or(junction_inspector.as_ref())
            .and_then(|snapshot| snapshot.fields.first())
            .map(|(_, value)| format!("Selected: {value}"));
        let properties_expanded_id = egui::Id::new("properties-panel-expanded");
        let properties_revision_id = egui::Id::new("properties-panel-evidence-revision");
        let mut properties_expanded = self
            .egui_context
            .data(|data| data.get_temp::<bool>(properties_expanded_id))
            .unwrap_or(true);
        let previous_evidence_revision = self
            .egui_context
            .data(|data| data.get_temp::<u64>(properties_revision_id));
        if previous_evidence_revision
            .is_some_and(|revision| revision != self.next_evidence_revision)
        {
            properties_expanded = true;
        }
        self.egui_context.data_mut(|data| {
            data.insert_temp(properties_revision_id, self.next_evidence_revision);
        });
        let command_key = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        let full_output = self.egui_context.run_ui(raw_input, |ui| {
            egui::Panel::top("application-menu")
                .exact_size(MENU_HEIGHT)
                .show(ui, |ui| {
                    egui::MenuBar::new().ui(ui, |ui| {
                        ui.menu_button("File", |ui| {
                            if ui
                                .button(format!("Open BAM/CRAM…  {command_key}+O"))
                                .clicked()
                            {
                                open_alignment_requested = true;
                                ui.close();
                            }
                            if ui.button("Open URL…").clicked() {
                                remote_source_open = true;
                                ui.close();
                            }
                            if ui.button("Open session…").clicked() {
                                open_session_requested = true;
                                ui.close();
                            }
                            let save_label = if current_session_path.is_some() {
                                format!("Save project  {command_key}+S")
                            } else {
                                format!("Save project as…  {command_key}+S")
                            };
                            if ui.button(save_label).clicked() {
                                save_session_requested = true;
                                ui.close();
                            }
                            if current_session_path.is_some()
                                && ui.button("Save project as…").clicked()
                            {
                                save_session_as_requested = true;
                                ui.close();
                            }
                            if !unresolved_session_sources.is_empty()
                                || !session_migration_notices.is_empty()
                            {
                                ui.separator();
                                if ui
                                    .button(format!(
                                        "Project issues ({})…",
                                        unresolved_session_sources
                                            .len()
                                            .saturating_add(session_migration_notices.len())
                                    ))
                                    .clicked()
                                {
                                    project_issues_open = true;
                                    ui.close();
                                }
                            }
                            if igv_import_report.is_some()
                                && ui.button("Last IGV import report…").clicked()
                            {
                                igv_report_open = true;
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .button(format!("Export PNG…  {command_key}+E"))
                                .clicked()
                            {
                                export_png_requested = true;
                                ui.close();
                            }
                        });
                        ui.menu_button("Genomes", |ui| {
                            ui.label(&assembly_label);
                            if let Some(identity) = &assembly.identity {
                                ui.small(format!(
                                    "{} · {:?} · {} fingerprint matches",
                                    identity.stable_id,
                                    identity.confidence,
                                    identity.matched_fingerprints
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
                                assembly.reference_contig_count,
                                if assembly.has_cytobands() { "loaded" } else { "missing" },
                                assembly.default_annotation_count,
                            ));
                            if !assembly.has_cytobands()
                                || assembly.default_annotation_count == 0
                            {
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
                                ui.colored_label(
                                    egui::Color32::from_rgb(170, 45, 38),
                                    error,
                                );
                            }
                            if ui.button("Open FASTA…").clicked() {
                                open_reference_requested = true;
                                ui.close();
                            }
                            if ui.button("Open genome configuration…").clicked() {
                                open_genome_config_requested = true;
                                ui.close();
                            }
                            if self
                                .landmark_resolution
                                .as_ref()
                                .and_then(|resolution| resolution.bundle_id.as_ref())
                                .is_some_and(|bundle_id| {
                                    self.suppressed_builtin_gene_bundles.contains(bundle_id)
                                })
                                && ui.button("Restore default genes").clicked()
                            {
                                restore_default_genes_requested = true;
                                ui.close();
                            }
                            if self.landmark_resource_root.is_some()
                                && ui.button("Open landmark resource folder").clicked()
                            {
                                open_landmark_folder_requested = true;
                                ui.close();
                            }
                            ui.separator();
                            for (id, name) in &contigs {
                                if ui.button(name).clicked() {
                                    requested_contig = Some(*id);
                                    ui.close();
                                }
                            }
                        });
                        ui.menu_button("View", |ui| {
                            ui.checkbox(&mut diagnostics_open, "Diagnostics");
                            ui.checkbox(&mut metadata_open, "Sample metadata");
                            ui.checkbox(&mut properties_expanded, "Properties panel");
                            if ui
                                .button(format!("Settings…  {command_key}+,"))
                                .clicked()
                            {
                                settings_requested = true;
                                ui.close();
                            }
                        });
                        ui.menu_button("Tracks", |ui| {
                            if ui.button("Add track…").clicked() {
                                add_track_requested = true;
                                ui.close();
                            }
                            if ui.button("Import sample attributes…").clicked() {
                                metadata_import_requested = true;
                                metadata_open = true;
                                ui.close();
                            }
                            ui.separator();
                            if self.alignment_tracks.is_empty() && self.synthetic {
                                let id = self.primary_alignment_track_id();
                                let filters = self.live_session_filters();
                                ui.menu_button(self.alignment_track_name(), |ui| {
                                    if ui
                                        .checkbox(
                                            &mut alignment_visible,
                                            "Show annotation content",
                                        )
                                        .changed()
                                    {
                                        alignment_collapsed = !alignment_visible;
                                    }
                                    ui.separator();
                                    alignment_subtrack_menu(
                                        ui,
                                        id,
                                        &filters,
                                        show_reads,
                                        true,
                                        &mut track_ui_actions,
                                    );
                                    ui.separator();
                                    if ui.button("Alignment settings…").clicked() {
                                        track_settings_panel =
                                            Some(TrackFrameKey::Alignment(id));
                                        ui.close();
                                    }
                                });
                            }
                            for control in &mut alignment_track_controls {
                                let filters = alignment_subtrack_controls
                                    .iter()
                                    .find(|(candidate, _)| *candidate == control.id)
                                    .map(|(_, filters)| filters)
                                    .cloned()
                                    .unwrap_or_default();
                                ui.menu_button(control.name.as_str(), |ui| {
                                    if ui
                                        .checkbox(
                                            &mut control.visible,
                                            "Show annotation content",
                                        )
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
                                            .pane_track_state(
                                                ui::TrackPane::Reads,
                                                control.id,
                                            )
                                            .is_none_or(|node| node.content_visible),
                                        control.id == primary_alignment_id,
                                        &mut track_ui_actions,
                                    );
                                    ui.separator();
                                    if ui.button("Alignment settings…").clicked() {
                                        track_settings_panel =
                                            Some(TrackFrameKey::Alignment(control.id));
                                        ui.close();
                                    }
                                });
                            }
                            for control in &mut track_controls {
                                if ui
                                    .checkbox(&mut control.visible, control.name.as_str())
                                    .changed()
                                {
                                    control.collapsed = !control.visible;
                                }
                            }
                        });
                        ui.menu_button("Regions", |ui| {
                            if ui.button("Add current view as region").clicked() {
                                add_current_region = true;
                                ui.close();
                            }
                            if ui.button("Add current view to review list").clicked() {
                                add_current_review = true;
                                ui.close();
                            }
                            if ui.button("Manage regions…").clicked() {
                                regions_open = true;
                                ui.close();
                            }
                            if ui.button("Review queue…").clicked() {
                                review_open = true;
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Previous annotation feature").clicked() {
                                previous_feature_requested = true;
                                ui.close();
                            }
                            if ui.button("Next annotation feature").clicked() {
                                next_feature_requested = true;
                                ui.close();
                            }
                            ui.separator();
                            for locus in &recent_loci {
                                if ui.button(locus).clicked() {
                                    recent_locus_requested = Some(locus.clone());
                                    ui.close();
                                }
                            }
                            if recent_loci.is_empty() {
                                ui.label("No recent loci");
                            }
                        });
                        ui.menu_button("Tools", |ui| {
                            if ui
                                .button(format!("Command search…  {command_key}+K"))
                                .clicked()
                            {
                                command_palette_open = true;
                                ui.close();
                            }
                            if ui.button("Clear caches").clicked() {
                                clear_cache_requested = true;
                                ui.close();
                            }
                        });
                        ui.menu_button("Help", |ui| {
                            ui.label("Drag to pan; wheel/trackpad to zoom");
                            ui.label("Shift+wheel or Page Up/Down scrolls tracks");
                            ui.label(
                                "Chromosome overview: drag horizontally to pan and vertically to zoom",
                            );
                            ui.label(
                                "Right overview: drag vertically to scroll and horizontally for density",
                            );
                            ui.label("N/P selects the next/previous visible read");
                            ui.label("]/[ selects the next/previous loaded item in the selected track");
                            ui.label("Enter opens settings for the selected track");
                            ui.label("V/C toggles a focused track's visibility/collapse");
                            ui.label(
                                "Alt+Up/Down reorders a focused track; Shift+Delete removes it",
                            );
                        });
                    });
                });
            egui::Panel::bottom("application-status")
                .exact_size(STATUS_HEIGHT)
                .show(ui, |ui| {
                    let status_response = ui::status_bar(
                        ui,
                        track_frames.len(),
                        &status_locus,
                        &data_status,
                        selection_summary.as_deref(),
                    );
                    if announce_status {
                        status_response.output_event(egui::output::OutputEvent::ValueChanged(
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Label,
                                true,
                                format!("Application status: {data_status}"),
                            ),
                        ));
                    }
                });
            egui::Panel::top("navigation-toolbar")
                .exact_size(NAVIGATION_HEIGHT)
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        let assembly_response = egui::ComboBox::from_id_salt("assembly-selector")
                            .selected_text(&assembly_label)
                            .width(145.0)
                            .show_ui(ui, |ui| {
                                let _ = ui.selectable_label(true, &assembly_label);
                            });
                        assembly_response.response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                true,
                                format!("Genome assembly; {assembly_label}"),
                            )
                        });
                        let contig_response = egui::ComboBox::from_id_salt("contig-selector")
                            .selected_text(&contig_name)
                            .width(90.0)
                            .show_ui(ui, |ui| {
                                for (id, name) in &contigs {
                                    if ui.selectable_label(*name == contig_name, name).clicked() {
                                        requested_contig = Some(*id);
                                    }
                                }
                            });
                        contig_response.response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                true,
                                format!("Chromosome or contig; {contig_name}"),
                            )
                        });
                        let locus_label = ui.label("Locus");
                        let locus_response = ui.add(
                            egui::TextEdit::singleline(&mut locus_editor)
                                .id(egui::Id::new(LOCUS_EDITOR_ID))
                                .desired_width(190.0)
                                .hint_text("chr1:1-100000 chr2:1-100000"),
                        ).labelled_by(locus_label.id);
                        locus_search_changed = locus_response.changed();
                        search_popup_position = Some(locus_response.rect.left_bottom());
                        if focus_locus_requested {
                            locus_response.request_focus();
                        }
                        if locus_response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter))
                        {
                            navigate_requested = true;
                        }
                        if ui.button("Go").clicked() {
                            navigate_requested = true;
                        }
                        let panel_response = egui::ComboBox::from_id_salt("view-panel-selector")
                            .selected_text(format!("Panels {}", panel_controls.len()))
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                for (index, id, label) in &panel_controls {
                                    ui.horizontal(|ui| {
                                        if ui
                                            .selectable_label(*id == active_panel_id, label)
                                            .clicked()
                                        {
                                            panel_to_activate = Some(*id);
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
                                            panel_to_move = Some((*id, index - 1));
                                        }
                                        let move_right = ui
                                            .add_enabled(
                                                index + 1 < panel_controls.len(),
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
                                                index + 1 < panel_controls.len(),
                                                format!("Move panel {} right", id.get()),
                                            )
                                        });
                                        if move_right.clicked() {
                                            panel_to_move = Some((*id, index + 1));
                                        }
                                        let close = ui
                                            .add_enabled(
                                                panel_controls.len() > 1,
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
                                                panel_controls.len() > 1,
                                                format!("Close panel {}", id.get()),
                                            )
                                        });
                                        if close.clicked() {
                                            panel_to_close = Some(*id);
                                        }
                                    });
                                }
                            });
                        panel_response.response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::ComboBox,
                                true,
                                format!("View panels; {} open", panel_controls.len()),
                            )
                        });
                        let previous_locus = ui
                            .add_enabled(
                                self.panels.active().navigation_history.can_go_back(),
                                ui::icons::IconButton::new(
                                    ui::icons::AppIcon::Previous,
                                    "Previous locus",
                                ),
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
                            navigate_back_requested = true;
                        }
                        let next_locus = ui
                            .add_enabled(
                                self.panels.active().navigation_history.can_go_forward(),
                                ui::icons::IconButton::new(
                                    ui::icons::AppIcon::Next,
                                    "Next locus",
                                ),
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
                            navigate_forward_requested = true;
                        }
                        let home_locus = ui
                            .add(ui::icons::IconButton::new(
                                ui::icons::AppIcon::Home,
                                "Home locus",
                            ))
                            .on_hover_text("Home locus");
                        home_locus.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                "Home locus",
                            )
                        });
                        if home_locus.clicked() {
                            navigate_home_requested = true;
                        }
                        let zoom_out = ui
                            .add(ui::icons::IconButton::new(
                                ui::icons::AppIcon::ZoomOut,
                                "Zoom out",
                            ))
                            .on_hover_text("Zoom out");
                        zoom_out.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Zoom out")
                        });
                        if zoom_out.clicked() {
                            zoom_out_requested = true;
                        }
                        let maximum_zoom = (self.panels.active().contig_length.max(1) as f64).log10().max(1.0);
                        let mut zoom_level =
                            (self.panels.active().contig_length as f64 / interval.len() as f64).log10();
                        let zoom_response = ui
                            .add(
                                egui::Slider::new(&mut zoom_level, 0.0..=maximum_zoom)
                                    .show_value(false),
                            )
                            .on_hover_text("Genome zoom");
                        zoom_response.widget_info(|| {
                            egui::WidgetInfo::slider(true, zoom_level, "Genome zoom level")
                        });
                        if zoom_response.changed() {
                            requested_span = Some(
                                (self.panels.active().contig_length as f64 / 10_f64.powf(zoom_level))
                                    .round()
                                    .clamp(1.0, self.panels.active().contig_length as f64)
                                    as u64,
                            );
                        }
                        let zoom_in = ui
                            .add(ui::icons::IconButton::new(
                                ui::icons::AppIcon::ZoomIn,
                                "Zoom in",
                            ))
                            .on_hover_text("Zoom in");
                        zoom_in.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Zoom in")
                        });
                        if zoom_in.clicked() {
                            zoom_in_requested = true;
                        }
                        ui.separator();
                        ui.small(format!("{bases_per_pixel:.3} bp/px · {detail:?}"));
                        ui.allocate_ui_with_layout(
                            egui::vec2(18.0, 18.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                if !reduced_motion
                                    && (data_status.starts_with("Loading")
                                    || data_status.contains("queue busy")
                                    )
                                {
                                    ui.spinner();
                                }
                            },
                        );
                    });
                });
            if !locus_editor.contains(':')
                && !locus_editor.trim().is_empty()
                && annotation_search_results
                    .query
                    .trim()
                    .eq_ignore_ascii_case(locus_editor.trim())
                && !annotation_search_results.hits.is_empty()
                && let Some(position) = search_popup_position
            {
                egui::Window::new("Annotation search results")
                    .id(egui::Id::new("annotation-search-results"))
                    .fixed_pos(position)
                    .fixed_size(egui::vec2(460.0, 260.0))
                    .collapsible(false)
                    .resizable(false)
                    .title_bar(false)
                    .show(ui.ctx(), |ui| {
                        for hit in annotation_search_results.hits.iter().take(12) {
                            let name = hit
                                .record
                                .name
                                .as_deref()
                                .unwrap_or(hit.matched_term.as_str());
                            let label = format!(
                                "{name}  ·  {}:{}-{}  ·  {}",
                                hit.record.contig,
                                hit.record.start.saturating_add(1),
                                hit.record.end,
                                hit.track_name
                            );
                            if ui.button(label).clicked() {
                                annotation_hit_requested = Some(hit.clone());
                            }
                        }
                        if annotation_search_results.hits.len() > 12 {
                            ui.label(format!(
                                "{} more matches; continue typing to narrow the list",
                                annotation_search_results.hits.len() - 12
                            ));
                        }
                        if annotation_search_results.index_truncated
                            || annotation_search_results.scan_truncated
                        {
                            ui.label("Search was bounded; a more specific prefix may find omitted matches");
                        }
                    });
            }
            let displayed_properties_expanded = properties_expanded;
            let properties_rect = if displayed_properties_expanded {
                egui::Panel::right("properties-panel")
                    .default_size(340.0)
                    .size_range(280.0..=520.0)
                    .resizable(true)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.heading("Properties");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(ui::icons::IconButton::new(
                                            ui::icons::AppIcon::ChevronRight,
                                            "Collapse Properties panel",
                                        ))
                                        .on_hover_text("Collapse Properties panel")
                                        .clicked()
                                    {
                                        properties_expanded = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        if let Some(panel_id) =
                            ui::inspectors::show_inspected_item_switcher(self, ui)
                        {
                            panel_to_activate = Some(panel_id);
                        }
                        egui::ScrollArea::vertical()
                            .id_salt("properties-panel-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let inspector_actions = ui::inspectors::show_properties(
                                    self,
                                    ui,
                                    variant_sample_search.clone(),
                                );
                                clear_selection |= inspector_actions.clear_selection;
                                selected_to_top_requested |=
                                    inspector_actions.selected_to_top_requested;
                                chain_navigation_requested =
                                    inspector_actions.chain_navigation_requested;
                                mate_panel_requested |= inspector_actions.mate_panel_requested;
                                chain_panel_requested = inspector_actions.chain_panel_requested;
                                chain_all_panels_requested |=
                                    inspector_actions.chain_all_panels_requested;
                                variant_partner_requested =
                                    inspector_actions.variant_partner_requested;
                                variant_endpoint_requested =
                                    inspector_actions.variant_endpoint_requested;
                                interaction_endpoint_requested =
                                    inspector_actions.interaction_endpoint_requested;
                                variant_pileup_requested =
                                    inspector_actions.variant_pileup_requested;
                                variant_modification_requested =
                                    inspector_actions.variant_modification_requested;
                                variant_genotypes_export_requested |=
                                    inspector_actions.variant_genotypes_export_requested;
                                variant_evidence_export_requested |=
                                    inspector_actions.variant_evidence_export_requested;
                                junction_bed_export_requested |=
                                    inspector_actions.junction_bed_export_requested;
                                sashimi_png_export_requested |=
                                    inspector_actions.sashimi_png_export_requested;
                                sashimi_svg_export_requested |=
                                    inspector_actions.sashimi_svg_export_requested;
                                add_selected_feature_to_review |=
                                    inspector_actions.add_selected_feature_to_review;
                                if inspector_actions.show_all_transcripts_requested.is_some() {
                                    show_all_transcripts_requested =
                                        inspector_actions.show_all_transcripts_requested;
                                }
                                review_open |= inspector_actions.review_open;
                                variant_sample_search = inspector_actions.variant_sample_search;
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
                                properties_expanded = true;
                            }
                        });
                    })
                    .response
                    .rect
            };
            native_properties_resize_active = displayed_properties_expanded
                && ui
                    .ctx()
                    .read_response(egui::Id::new("properties-panel").with("__resize"))
                    .is_some_and(|response| response.dragged());
            egui::CentralPanel::no_frame().show(ui, |ui| {
                let panel_inputs = panel_contexts
                    .iter()
                    .map(|(id, name, length, viewport, _)| {
                        let cytobands = if assembly
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
                    previous_workspace_layout.revision,
                );
                let workspace_layout = if candidate_layout == previous_workspace_layout {
                    candidate_layout
                } else {
                    ui::WorkspaceLayoutSnapshot::compute(
                        layout_input,
                        previous_workspace_layout.revision.saturating_add(1),
                    )
                };
                let geometry = workspace_layout.canvas_geometry();
                new_canvas_geometry = geometry;
                ui.painter().rect_filled(
                    workspace_layout.genome_context,
                    0.0,
                    egui::Color32::from_gray(250),
                );
                ui.painter().line_segment(
                    [
                        workspace_layout.context_gutter.right_top(),
                        workspace_layout.context_gutter.right_bottom(),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_gray(190)),
                );
                for (index, ((id, name, length, viewport, genome_overview), panel_layout)) in
                    panel_contexts.iter().zip(&workspace_layout.panels).enumerate()
                {
                    let interaction = ui::paint_genome_context(
                        ui,
                        &assembly,
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
                        overview_interaction = interaction;
                        overview_panel_id = *id;
                    }
                    if index > 0 {
                        ui.painter().line_segment(
                            [
                                panel_layout.header.left_top(),
                                panel_layout.header.left_bottom(),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(180)),
                        );
                    }
                }
                new_workspace_layout = workspace_layout.clone();
                let gutter = ui::paint_track_viewport_chrome_with_sections(
                    ui,
                    geometry,
                    &track_frames,
                    self.panels.annotation_scroll_points,
                    0.0,
                    // Section chrome belongs to the current pane-layout
                    // revision. A scroll display cache can legitimately lag
                    // by one frame during a window resize; mixing its old
                    // annotation edge with the new read-pane edge stretches
                    // the Read data header across the intervening space.
                    pane_annotation_top,
                    pane_annotation_bottom,
                    selected_track,
                    &selected_annotation_track_ids,
                    &self.panels.active().selected_read_tracks,
                    &track_groups,
                    ui::TrackSectionChrome {
                        annotation_expanded: self
                            .track_presentation
                            .annotation_section_expanded,
                        reads_expanded: self.track_presentation.read_section_expanded,
                        annotation_track_count,
                        read_track_count,
                        annotation_content_top: pane_layout.annotation.pane_rect.top(),
                        read_content_top: pane_layout.reads.pane_rect.top(),
                    },
                );
                pane_divider_rect = gutter.pane_divider_rect;
                track_ui_actions.extend(gutter.actions);
                let frame_genomic_texts = remap_genomic_texts_for_layout(
                    &genomic_texts,
                    &previous_workspace_layout,
                    &workspace_layout,
                );
                ui::paint_genomic_text(ui, geometry.genomic, &frame_genomic_texts);
                for ((_, panel_id, locus), panel_layout) in
                    panel_controls.iter().zip(&workspace_layout.panels)
                {
                    let canvas_accessibility = ui.interact(
                        panel_layout.canvas,
                        egui::Id::new(("genomic-canvas-accessibility", panel_id)),
                        egui::Sense::focusable_noninteractive(),
                    );
                    genomic_canvas_layer_id = Some(canvas_accessibility.layer_id);
                    let canvas_label = if *panel_id == active_panel_id {
                        selection_summary.as_ref().map_or_else(
                            || format!("Genome tracks at {locus}; no evidence selected"),
                            |selection| format!("Genome tracks at {locus}; {selection}"),
                        )
                    } else {
                        format!("Genome tracks at {locus}; inactive locus panel")
                    };
                    canvas_accessibility.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Panel,
                            true,
                            &canvas_label,
                        )
                    });
                }
                if panel_controls.len() > 1 {
                    for index in 1..panel_controls.len() {
                        let x = workspace_layout.panels[index].track_column.left();
                        ui.painter().line_segment(
                            [
                                egui::pos2(x, geometry.viewport.top()),
                                egui::pos2(x, geometry.viewport.bottom()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(175)),
                        );
                    }
                    if let Some(active_index) = panel_controls
                        .iter()
                        .position(|(_, id, _)| *id == active_panel_id)
                    {
                        ui.painter().rect_stroke(
                            workspace_layout.panels[active_index].track_column,
                            0.0,
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(45, 92, 175)),
                            egui::StrokeKind::Inside,
                        );
                    }
                }
                for workspace in &mut workspace_scrollbars {
                    let full = workspace_layout.panels[workspace.index].scrollbar;
                    let split_y = workspace
                        .annotation_clip_bottom
                        .clamp(full.top(), full.bottom());
                    let annotation_top = workspace
                        .annotation_clip_top
                        .clamp(full.top(), split_y);
                    let annotation_rect = egui::Rect::from_min_max(
                        egui::pos2(full.left(), annotation_top),
                        egui::pos2(full.right(), split_y),
                    );
                    if annotation_rect.height() > 1.0 {
                        workspace.annotation_interaction =
                            ui::paint_workspace_annotation_scrollbar(
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
                        egui::Stroke::new(1.0, egui::Color32::from_gray(125)),
                    );
                    let read_top = workspace.read_clip_top.clamp(split_y, full.bottom());
                    let read_scrollbar_left = if workspace.read_stack_maximum > f32::EPSILON
                        && full.bottom() - read_top > 1.0
                    {
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
                            egui::Stroke::new(0.75, egui::Color32::from_gray(150)),
                        );
                        full.center().x
                    } else {
                        full.left()
                    };
                    for scrollbar in &mut workspace.read_panels {
                        let top = scrollbar.top.clamp(full.top(), full.bottom());
                        let bottom = scrollbar.bottom.clamp(top, full.bottom());
                        let rect = egui::Rect::from_min_max(
                            egui::pos2(read_scrollbar_left, top),
                            egui::pos2(full.right(), bottom),
                        );
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
                        );
                        ui.painter().line_segment(
                            [
                                egui::pos2(rect.left(), rect.top()),
                                egui::pos2(rect.right(), rect.top()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_gray(125)),
                        );
                    }
                }
            });
            if let Some(key) = rendered_track_settings_panel {
                let mut open = true;
                egui::Window::new("Track settings")
                    .open(&mut open)
                    .default_width(460.0)
                    .vscroll(true)
                    .show(ui.ctx(), |ui| match key {
                        TrackFrameKey::Group { channel, id } => {
                            ui.heading(format!("{channel:?} track group {id}"));
                            ui.label("Use the group eye to minimize or expand it, or right-click the group header to ungroup its tracks.");
                        }
                        TrackFrameKey::Reference => {
                            ui.heading("Reference sequence");
                            ui.checkbox(&mut show_reference, "Show reference track");
                            ui.checkbox(
                                &mut reference_reverse_complement,
                                "Display reverse strand",
                            );
                            ui.separator();
                            ui.label("Translation frames");
                            ui.horizontal_wrapped(|ui| {
                                for frame in ReferenceTranslationFrame::ALL {
                                    let mut enabled =
                                        reference_translation_frames.contains(&frame);
                                    if ui.checkbox(&mut enabled, frame.label()).changed() {
                                        if enabled {
                                            reference_translation_frames.push(frame);
                                        } else {
                                            reference_translation_frames
                                                .retain(|candidate| *candidate != frame);
                                        }
                                    }
                                }
                            });
                            egui::ComboBox::from_label("Genetic code")
                                .selected_text(format!("{reference_genetic_code:?}"))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut reference_genetic_code,
                                        GeneticCode::Standard,
                                        "Standard",
                                    );
                                    ui.selectable_value(
                                        &mut reference_genetic_code,
                                        GeneticCode::VertebrateMitochondrial,
                                        "Vertebrate mitochondrial",
                                    );
                                });
                            ui.label(
                                "The FASTA is loaded independently of alignment files and stays pinned above the scrollable read stack.",
                            );
                        }
                        TrackFrameKey::Alignment(id) if id == settings_alignment_id => {
                            ui.heading("Alignment track");
                            sample_association_ui(
                                ui,
                                &mut edited_track_sample_key,
                                sample_metadata.as_ref(),
                            );
                            if bulk_alignment_ids.len() > 1 {
                                ui.label(format!(
                                    "{} alignment tracks selected",
                                    bulk_alignment_ids.len()
                                ));
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Group selected tracks").clicked() {
                                        if let Some(pane) = bulk_alignment_pane {
                                            track_ui_actions
                                                .push(TrackUiAction::GroupSelectedTracks(pane));
                                        }
                                    }
                                    if let Some(group) = bulk_alignment_group {
                                        if ui
                                            .button(if group.collapsed {
                                                "Expand group"
                                            } else {
                                                "Minimize group"
                                            })
                                            .clicked()
                                        {
                                            track_ui_actions
                                                .push(TrackUiAction::ToggleTrackGroup(
                                                    group.pane,
                                                    group.id,
                                                ));
                                        }
                                        if ui.button("Ungroup").clicked() {
                                            track_ui_actions
                                                .push(TrackUiAction::UngroupTracks(
                                                    group.pane,
                                                    group.id,
                                                ));
                                        }
                                    }
                                });
                                ui.horizontal_wrapped(|ui| {
                                    if ui.button("Show all").clicked() {
                                        bulk_alignment_visibility = Some(true);
                                    }
                                    if ui.button("Hide all").clicked() {
                                        bulk_alignment_visibility = Some(false);
                                    }
                                    if ui.button("Expand all").clicked() {
                                        bulk_alignment_collapsed = Some(false);
                                    }
                                    if ui.button("Collapse all").clicked() {
                                        bulk_alignment_collapsed = Some(true);
                                    }
                                });
                                if ui
                                    .button("Apply compatible appearance to selected tracks")
                                    .on_hover_text(
                                        "Copies display, grouping, pairing, and modification appearance. Sample filters and record caps stay independent.",
                                    )
                                    .clicked()
                                {
                                    apply_bulk_alignment_appearance = true;
                                }
                                ui.separator();
                            }
                            ui.horizontal(|ui| {
                                let label = ui.label("Name");
                                ui.text_edit_singleline(&mut alignment_name)
                                    .labelled_by(label.id);
                            });
                            ui.horizontal(|ui| {
                                let label = ui.label("Read/template");
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut read_name_editor)
                                        .desired_width(240.0)
                                        .hint_text("Exact read name"),
                                ).labelled_by(label.id);
                                if ui.button("Select").clicked()
                                    || (response.lost_focus()
                                        && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                                {
                                    select_read_name_requested = true;
                                }
                            });
                            ui.small(
                                "Searches the complete individual-read population admitted for this panel's current viewport; names are not saved in sessions.",
                            );
                            if ui
                                .checkbox(&mut alignment_visible, "Show annotation content")
                                .changed()
                            {
                                alignment_collapsed = !alignment_visible;
                            }
                            ui.add(
                                egui::Slider::new(&mut alignment_height, 40.0..=1_200.0)
                                    .text("Height"),
                            );
                            ui.separator();
                            ui.label("Appearance");
                            ui.horizontal_wrapped(|ui| {
                                ui.label("Subtracks");
                                ui.checkbox(&mut show_reference, "Reference");
                                ui.checkbox(&mut show_coverage, "Coverage");
                                ui.checkbox(&mut show_junctions, "Junctions");
                                ui.checkbox(&mut show_modification_frequency, "Modifications");
                                ui.checkbox(&mut show_reads, "Reads");
                            });
                            if show_junctions {
                                ui.horizontal(|ui| {
                                    ui.label("Junction depth ≥");
                                    ui.add(
                                        egui::DragValue::new(&mut minimum_junction_depth)
                                            .range(1..=1_000_000)
                                            .suffix(" reads"),
                                    );
                                });
                                ui.horizontal(|ui| {
                                    ui.label("Aligned flank ≥");
                                    ui.add(
                                        egui::DragValue::new(&mut minimum_junction_flank)
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
                                        .selected_text(match junction_display_mode {
                                            JunctionDisplayMode::Collapsed => "Collapsed",
                                            JunctionDisplayMode::Expanded => "Expanded",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut junction_display_mode,
                                                JunctionDisplayMode::Collapsed,
                                                "Collapsed",
                                            );
                                            ui.selectable_value(
                                                &mut junction_display_mode,
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
                                        .selected_text(match sashimi_count_mode {
                                            SashimiCountMode::Raw => "Raw reads",
                                            SashimiCountMode::CountsPerMillion => "CPM",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut sashimi_count_mode,
                                                SashimiCountMode::Raw,
                                                "Raw reads",
                                            );
                                            ui.selectable_value(
                                                &mut sashimi_count_mode,
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
                                        .selected_text(match sashimi_scale_mode {
                                            SashimiScaleMode::Automatic => "Per-track auto",
                                            SashimiScaleMode::Fixed => "Fixed",
                                            SashimiScaleMode::Shared => "Shared visible tracks",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut sashimi_scale_mode,
                                                SashimiScaleMode::Automatic,
                                                "Per-track automatic",
                                            );
                                            ui.selectable_value(
                                                &mut sashimi_scale_mode,
                                                SashimiScaleMode::Fixed,
                                                "Fixed maximum",
                                            );
                                            ui.selectable_value(
                                                &mut sashimi_scale_mode,
                                                SashimiScaleMode::Shared,
                                                "Shared across visible tracks",
                                            );
                                        })
                                        .response
                                        .labelled_by(label.id);
                                    if sashimi_scale_mode == SashimiScaleMode::Fixed {
                                        ui.add(
                                            egui::DragValue::new(
                                                &mut sashimi_fixed_scale_maximum,
                                            )
                                            .range(1..=1_000_000)
                                            .suffix(match sashimi_count_mode {
                                                SashimiCountMode::Raw => " reads",
                                                SashimiCountMode::CountsPerMillion => " CPM",
                                            }),
                                        );
                                    }
                                });
                                ui.checkbox(&mut show_junction_labels, "Show arc count labels");
                                if sashimi_count_mode == SashimiCountMode::CountsPerMillion {
                                    ui.small(
                                        "CPM uses the whole-file mapped-read count stored in the BAM index; unavailable metadata is shown explicitly and falls back to raw labels.",
                                    );
                                }
                            }
                            ui.horizontal(|ui| {
                                ui.label("Read density");
                                if ui.button("Expanded").clicked() {
                                    alignment_density = 1.0;
                                }
                                if ui.button("Squished").clicked() {
                                    alignment_density = 0.55;
                                }
                                if ui.button("Collapsed").clicked() {
                                    alignment_density = MIN_VERTICAL_DENSITY;
                                }
                                ui.small(format!("{alignment_density:.2}×"));
                            });
                            ui.horizontal(|ui| {
                                let label = ui.label("Open inspectors on");
                                egui::ComboBox::from_id_salt("inspector-activation")
                                    .selected_text(match inspector_activation {
                                        InspectorActivation::Click => "Click",
                                        InspectorActivation::Hover => "Hover",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut inspector_activation,
                                            InspectorActivation::Click,
                                            "Click",
                                        );
                                        ui.selectable_value(
                                            &mut inspector_activation,
                                            InspectorActivation::Hover,
                                            "Hover",
                                        );
                                    })
                                    .response
                                    .labelled_by(label.id);
                            });
                            ui.checkbox(&mut use_fixed_coverage_scale, "Fixed coverage scale");
                            ui.horizontal(|ui| {
                                let label = ui.label("Coverage transform");
                                egui::ComboBox::from_id_salt("coverage-scale-transform")
                                    .selected_text(match coverage_scale_transform {
                                        CoverageScaleTransform::Linear => "Linear",
                                        CoverageScaleTransform::SquareRoot => "Square root",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut coverage_scale_transform,
                                            CoverageScaleTransform::Linear,
                                            "Linear",
                                        );
                                        ui.selectable_value(
                                            &mut coverage_scale_transform,
                                            CoverageScaleTransform::SquareRoot,
                                            "Square root",
                                        );
                                    })
                                    .response
                                    .labelled_by(label.id);
                            });
                            if use_fixed_coverage_scale {
                                ui.horizontal(|ui| {
                                    ui.label("Coverage maximum");
                                    ui.add(
                                        egui::DragValue::new(&mut coverage_scale_maximum)
                                            .range(1..=10_000_000)
                                            .suffix("×"),
                                    );
                                });
                            } else if let Some(stats) = self
                                .alignment_tracks
                                .first()
                                .and_then(|track| track.projection(active_panel_id))
                                .and_then(|projection| projection.cached_layout.as_ref())
                                .map(|(_, layout)| layout.stats)
                            {
                                ui.small(format!(
                                    "Coverage autoscale: current maximum {}×",
                                    stats.maximum_coverage
                                ));
                            }
                            alignment_style_controls(
                                ui,
                                &mut color_mode,
                                &mut group_mode,
                                &mut group_order,
                                &mut sort_mode,
                                &mut sort_order,
                                &mut style_tag_editor,
                            );
                            if sort_mode == AlignmentSortMode::Base {
                                let anchor = settings_filters.alignment_sort_anchor.or_else(|| {
                                    alignment_sort_position(
                                        AlignmentSortMode::Base,
                                        None,
                                        self.panels.active().viewport,
                                    )
                                });
                                if let Some(anchor) = anchor {
                                    ui.small(format!(
                                        "Base-sort anchor: {}:{}",
                                        self.panels.active().contig_name,
                                        anchor.saturating_add(1)
                                    ));
                                }
                            }
                            ui.horizontal(|ui| {
                                let label = ui.label("Read stacking");
                                egui::ComboBox::from_id_salt("read-stacking-mode")
                                    .selected_text(match read_stacking_mode {
                                        ReadStackingMode::Stable => "Stable across navigation",
                                        ReadStackingMode::Compact => "Compact each view",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut read_stacking_mode,
                                            ReadStackingMode::Stable,
                                            "Stable across navigation",
                                        )
                                        .on_hover_text(
                                            "Builds an organised row architecture and keeps each read on its row",
                                        );
                                        ui.selectable_value(
                                            &mut read_stacking_mode,
                                            ReadStackingMode::Compact,
                                            "Compact each view",
                                        )
                                        .on_hover_text(
                                            "Rebuilds the current view for density; reads may move while navigating",
                                        );
                                    })
                                    .response
                                    .labelled_by(label.id);
                                if read_stacking_mode == ReadStackingMode::Stable
                                    && ui
                                        .button("Rebuild stable architecture")
                                        .on_hover_text(
                                            "Use the current known neighbourhood as a new organised stable baseline",
                                        )
                                        .clicked()
                                {
                                    compact_repack_requested = true;
                                }
                            });
                            ui.small(match read_stacking_mode {
                                ReadStackingMode::Stable => {
                                    "Builds an organised row architecture and keeps each read on its row."
                                }
                                ReadStackingMode::Compact => {
                                    "Rebuilds the current view for density; reads may move while navigating."
                                }
                            });
                            ui.checkbox(&mut view_as_pairs, "View as pairs")
                                .on_hover_text(
                                    "Pack primary ends with the same read name on one row and select them as one template",
                                );
                            ui.checkbox(
                                &mut override_pair_bounds,
                                "Override inferred template-length bounds",
                            );
                            if override_pair_bounds {
                                ui.horizontal(|ui| {
                                    ui.label("Expected template length");
                                    ui.add(
                                        egui::DragValue::new(&mut minimum_template_length)
                                            .range(0..=10_000_000)
                                            .suffix(" bp min"),
                                    );
                                    ui.add(
                                        egui::DragValue::new(&mut maximum_template_length)
                                            .range(0..=10_000_000)
                                            .suffix(" bp max"),
                                    );
                                });
                            }
                            if let Some(bounds) = pair_summary.inferred_bounds {
                                ui.small(format!(
                                    "Inferred from {} of {} proper templates: median {} bp, expected {}–{} bp{}",
                                    pair_summary.sampled_templates,
                                    pair_summary.eligible_proper_templates,
                                    pair_summary.median_template_length.unwrap_or_default(),
                                    bounds.minimum.unwrap_or_default(),
                                    bounds.maximum.unwrap_or_default(),
                                    if pair_summary.all_eligible_records { "" } else { " (incomplete query)" },
                                ));
                            } else {
                                ui.small(format!(
                                    "Template-length inference unavailable: {} proper templates ({} required)",
                                    pair_summary.sampled_templates,
                                    rgv_hts::MIN_TEMPLATE_LENGTH_SAMPLES,
                                ));
                            }
                            ui.horizontal(|ui| {
                                let label = ui.label("Overlapping mates in pileup");
                                egui::ComboBox::from_id_salt("overlapping-mate-policy")
                                    .selected_text(match overlapping_mate_policy {
                                        OverlappingMatePolicy::CountOnce => "Count once",
                                        OverlappingMatePolicy::CountBoth => "Count both",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut overlapping_mate_policy,
                                            OverlappingMatePolicy::CountOnce,
                                            "Count once (leftmost end)",
                                        );
                                        ui.selectable_value(
                                            &mut overlapping_mate_policy,
                                            OverlappingMatePolicy::CountBoth,
                                            "Count both ends",
                                        );
                                    })
                                    .response
                                    .labelled_by(label.id);
                            });
                            ui.horizontal(|ui| {
                                let label = ui.label("Read bases");
                                egui::ComboBox::from_id_salt("read-base-display-mode")
                                    .selected_text(read_base_display_mode_label(
                                        read_base_display_mode,
                                    ))
                                    .show_ui(ui, |ui| {
                                        for mode in [
                                            ReadBaseDisplayMode::Mismatches,
                                            ReadBaseDisplayMode::All,
                                            ReadBaseDisplayMode::Hidden,
                                        ] {
                                            ui.selectable_value(
                                                &mut read_base_display_mode,
                                                mode,
                                                read_base_display_mode_label(mode),
                                            );
                                        }
                                    })
                                    .response
                                    .labelled_by(label.id);
                            });
                            ui.checkbox(
                                &mut shade_read_bases_by_quality,
                                "Shade read bases by quality",
                            );
                            ui.horizontal(|ui| {
                                ui.label("Show indels ≥");
                                ui.add(
                                    egui::Slider::new(&mut minimum_indel_size, 1..=10_000)
                                        .logarithmic(true)
                                        .suffix(" bp"),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("Label events ≥");
                                ui.add(
                                    egui::Slider::new(&mut minimum_event_label_size, 1..=10_000)
                                        .logarithmic(true)
                                        .suffix(" bp"),
                                );
                            });
                            ui.checkbox(&mut show_modified_bases, "Show modified bases");
                            ui.horizontal(|ui| {
                                ui.label("Modification probability ≥");
                                ui.add(
                                    egui::Slider::new(
                                        &mut minimum_modification_probability,
                                        0..=255,
                                    )
                                    .suffix(" / 255"),
                                );
                            });
                            ui.horizontal(|ui| {
                                let label = ui.label("Modification colours");
                                egui::ComboBox::from_id_salt("modified-base-color-mode")
                                    .selected_text(modified_base_color_mode_label(
                                        modified_base_color_mode,
                                    ))
                                    .show_ui(ui, |ui| {
                                        for mode in [
                                            ModifiedBaseColorMode::ByType,
                                            ModifiedBaseColorMode::Mono,
                                            ModifiedBaseColorMode::ModifiedVsUnmodified,
                                        ] {
                                            ui.selectable_value(
                                                &mut modified_base_color_mode,
                                                mode,
                                                modified_base_color_mode_label(mode),
                                            );
                                        }
                                    })
                                    .response
                                    .labelled_by(label.id);
                            });
                            if !available_modification_codes.is_empty() {
                                ui.label("Modification legend and visibility");
                                for code in &available_modification_codes {
                                    let mut visible = !hidden_modification_codes.contains(code);
                                    let label = modification_code_label(*code);
                                    let color = rgba_to_color32(modification_code_color(*code));
                                    if ui
                                        .checkbox(
                                            &mut visible,
                                            egui::RichText::new(label).color(color),
                                        )
                                        .changed()
                                    {
                                        if visible {
                                            hidden_modification_codes
                                                .retain(|candidate| candidate != code);
                                        } else if hidden_modification_codes.len()
                                            < MAX_HIDDEN_MODIFICATION_CODES
                                        {
                                            hidden_modification_codes.push(*code);
                                        }
                                    }
                                }
                            }
                            ui.separator();
                            ui.label("Filters");
                            ui.horizontal(|ui| {
                                ui.label("Minimum MAPQ");
                                ui.add(egui::Slider::new(&mut minimum_mapq, 0..=60));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Minimum base quality");
                                ui.add(egui::Slider::new(&mut minimum_base_quality, 0..=60));
                            });
                            ui.horizontal(|ui| {
                                ui.label("Minimum allele fraction");
                                ui.add(
                                    egui::Slider::new(&mut minimum_allele_fraction, 0.0..=1.0)
                                        .fixed_decimals(2),
                                );
                            });
                            ui.checkbox(
                                &mut quality_weighted_alleles,
                                "Weight allele colours by base quality",
                            );
                            ui.checkbox(
                                &mut show_coverage_center_line,
                                "Show coverage centre line",
                            );
                            ui.checkbox(&mut include_secondary, "Include secondary alignments");
                            ui.checkbox(
                                &mut include_supplementary,
                                "Include supplementary alignments",
                            );
                            ui.horizontal(|ui| {
                                ui.label("SAM tags");
                                ui.add(
                                    egui::TextEdit::singleline(&mut filter_expression)
                                        .hint_text("HP=1;RG=tumour;MM"),
                                );
                            });
                            ui.horizontal(|ui| {
                                ui.label("Complete read limit").on_hover_text(
                                    "At or below the effective limit, every eligible viewport read is shown. Above it, RGV shows no individual reads and asks you to zoom in or raise the limit. The default is 50,000; raising it can increase memory, query/layout time, and GPU work. Multi-panel or multi-track workspace allocation can make the effective limit lower than this requested value.",
                                );
                                ui.add(
                                    egui::Slider::new(
                                        &mut complete_read_limit,
                                        1_000..=MAX_CONFIGURABLE_COMPLETE_READ_LIMIT as u32,
                                    )
                                    .logarithmic(true),
                                );
                            });
                            if ui.button("Apply filters").clicked() {
                                apply_filter = true;
                            }
                            if let Some(error) = &filter_error {
                                ui.colored_label(egui::Color32::DARK_RED, error);
                            }
                        }
                        TrackFrameKey::Alignment(_) => {}
                        TrackFrameKey::Auxiliary(id) => {
                            let track_control_count = track_controls.len();
                            if let Some((index, control)) = track_controls
                                .iter_mut()
                                .enumerate()
                                .find(|(_, control)| control.id == id)
                            {
                                ui.heading("Track");
                                sample_association_ui(
                                    ui,
                                    &mut edited_track_sample_key,
                                    sample_metadata.as_ref(),
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
                                    !natural_row_height_tracks.contains(&id),
                                    egui::Slider::new(&mut control.height, 40.0..=600.0)
                                        .text("Height"),
                                )
                                .on_disabled_hover_text(
                                    "This matrix height follows its sample count and row height",
                                );
                                let count = track_counts
                                    .iter()
                                    .find(|(candidate, _)| *candidate == id)
                                    .map_or(0, |(_, count)| *count);
                                ui.label(format!("{count} records at the current locus"));
                                if let Some(identity) = builtin_track_identities.get(&id) {
                                    ui.separator();
                                    match identity.asset_role {
                                        LandmarkAssetRole::RepresentativeGenes => {
                                            if ui.button("Show all transcripts").clicked() {
                                                show_all_transcripts_requested =
                                                    Some(identity.bundle_id.clone());
                                            }
                                        }
                                        LandmarkAssetRole::DetailedTranscripts => {
                                            if ui.button("Return to representative genes").clicked()
                                            {
                                                return_to_representative_genes_requested =
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
                                if let Some((_, settings)) = feature_settings_controls
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
                                if let Some((_, settings)) = variant_settings_controls
                                    .iter_mut()
                                    .find(|(track_id, _)| *track_id == id)
                                {
                                    let query = variant_settings_queries
                                        .iter()
                                        .find(|(track_id, _)| *track_id == id)
                                        .map(|(_, query)| query.as_ref());
                                    ui.separator();
                                    variant_track_settings_ui(
                                        ui,
                                        settings,
                                        query,
                                        &mut variant_settings_sample_search,
                                        &mut variant_predicate_scope,
                                        &mut variant_predicate_key,
                                        &mut variant_predicate_operator,
                                        &mut variant_predicate_value,
                                        &mut variant_predicate_missing,
                                        &mut variant_predicate_error,
                                    );
                                }
                                if let Some((_, settings)) = gwas_settings_controls
                                    .iter_mut()
                                    .find(|(track_id, _)| *track_id == id)
                                {
                                    ui.separator();
                                    gwas_track_settings_ui(ui, settings);
                                }
                                if let Some((_, copy_settings)) = copy_number_settings_controls
                                    .iter_mut()
                                    .find(|(track_id, _)| *track_id == id)
                                {
                                    let (_, quantitative_settings) =
                                        quantitative_settings_controls
                                            .iter_mut()
                                            .find(|(track_id, _)| *track_id == id)
                                            .expect("copy-number tracks have quantitative settings");
                                    ui.separator();
                                    copy_number_track_settings_ui(
                                        ui,
                                        copy_settings,
                                        quantitative_settings,
                                        &copy_number_sort_locus,
                                    );
                                } else if let Some((_, settings)) = mutation_settings_controls
                                    .iter_mut()
                                    .find(|(track_id, _)| *track_id == id)
                                {
                                    ui.separator();
                                    mutation_track_settings_ui(ui, settings);
                                } else if let Some((_, settings)) =
                                    quantitative_settings_controls
                                        .iter_mut()
                                        .find(|(track_id, _)| *track_id == id)
                                {
                                    ui.separator();
                                    quantitative_track_settings_ui(
                                        ui,
                                        settings,
                                        id,
                                        &quantitative_track_choices,
                                    );
                                }
                                if let Some((_, settings)) = interaction_settings_controls
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
                                        track_move = Some((id, index - 1));
                                    }
                                    if ui
                                        .add_enabled(
                                            index + 1 < track_control_count,
                                            egui::Button::new("Move down"),
                                        )
                                        .clicked()
                                    {
                                        track_move = Some((id, index + 1));
                                    }
                                });
                            }
                        }
                    });
                if !open {
                    track_settings_panel = None;
                }
            }
            let mut workspace_dialogs = ui::workspace_dialogs::WorkspaceDialogDraft {
                metadata_open,
                regions_open,
                review_open,
                regions: regions.clone(),
                review_items: review_items.clone(),
                review_status_filter,
                add_current_region,
                add_current_review,
                region_import_requested,
                region_export_requested,
                review_import_requested,
                review_export_requested,
                previous_review_requested,
                next_review_requested,
                managed_locus_requested: managed_locus_requested.clone(),
                region_to_delete,
                review_to_delete,
                region_to_relocate,
                review_to_relocate,
                sample_metadata: sample_metadata.clone(),
                metadata_filter_column,
                metadata_filter_value: metadata_filter_value.clone(),
                metadata_selected_tracks: metadata_selected_tracks.clone(),
                metadata_bulk_height,
                metadata_remove_armed,
                metadata_import_requested,
                metadata_remove_requested,
                metadata_select_requested,
                metadata_bulk_visibility,
                metadata_bulk_collapsed,
                metadata_bulk_height_requested,
                metadata_bulk_rename_requested,
                metadata_bulk_alignment_settings_requested,
                metadata_bulk_auxiliary_appearance_requested,
                metadata_bulk_quantitative_autoscale_requested,
                metadata_bulk_quantitative_overlay_requested,
                metadata_bulk_quantitative_separate_requested,
                metadata_bulk_remove_requested,
                metadata_track_rows: metadata_track_rows.clone(),
            };
            ui::workspace_dialogs::show_workspace_dialogs(ui, &mut workspace_dialogs);
            metadata_open = workspace_dialogs.metadata_open;
            regions_open = workspace_dialogs.regions_open;
            review_open = workspace_dialogs.review_open;
            regions = workspace_dialogs.regions;
            review_items = workspace_dialogs.review_items;
            review_status_filter = workspace_dialogs.review_status_filter;
            add_current_region = workspace_dialogs.add_current_region;
            add_current_review = workspace_dialogs.add_current_review;
            region_import_requested = workspace_dialogs.region_import_requested;
            region_export_requested = workspace_dialogs.region_export_requested;
            review_import_requested = workspace_dialogs.review_import_requested;
            review_export_requested = workspace_dialogs.review_export_requested;
            previous_review_requested = workspace_dialogs.previous_review_requested;
            next_review_requested = workspace_dialogs.next_review_requested;
            managed_locus_requested = workspace_dialogs.managed_locus_requested;
            region_to_delete = workspace_dialogs.region_to_delete;
            review_to_delete = workspace_dialogs.review_to_delete;
            region_to_relocate = workspace_dialogs.region_to_relocate;
            review_to_relocate = workspace_dialogs.review_to_relocate;
            sample_metadata = workspace_dialogs.sample_metadata;
            metadata_filter_column = workspace_dialogs.metadata_filter_column;
            metadata_filter_value = workspace_dialogs.metadata_filter_value;
            metadata_selected_tracks = workspace_dialogs.metadata_selected_tracks;
            metadata_bulk_height = workspace_dialogs.metadata_bulk_height;
            metadata_remove_armed = workspace_dialogs.metadata_remove_armed;
            metadata_import_requested = workspace_dialogs.metadata_import_requested;
            metadata_remove_requested = workspace_dialogs.metadata_remove_requested;
            metadata_select_requested = workspace_dialogs.metadata_select_requested;
            metadata_bulk_visibility = workspace_dialogs.metadata_bulk_visibility;
            metadata_bulk_collapsed = workspace_dialogs.metadata_bulk_collapsed;
            metadata_bulk_height_requested = workspace_dialogs.metadata_bulk_height_requested;
            metadata_bulk_rename_requested = workspace_dialogs.metadata_bulk_rename_requested;
            metadata_bulk_alignment_settings_requested = workspace_dialogs.metadata_bulk_alignment_settings_requested;
            metadata_bulk_auxiliary_appearance_requested = workspace_dialogs.metadata_bulk_auxiliary_appearance_requested;
            metadata_bulk_quantitative_autoscale_requested = workspace_dialogs.metadata_bulk_quantitative_autoscale_requested;
            metadata_bulk_quantitative_overlay_requested = workspace_dialogs.metadata_bulk_quantitative_overlay_requested;
            metadata_bulk_quantitative_separate_requested = workspace_dialogs.metadata_bulk_quantitative_separate_requested;
            metadata_bulk_remove_requested = workspace_dialogs.metadata_bulk_remove_requested;
            metadata_track_rows = workspace_dialogs.metadata_track_rows;
            ui::show_diagnostics(ui.ctx(), &mut diagnostics_open, &diagnostics);
            let mut system_dialogs = ui::system_dialogs::SystemDialogDraft {
                exit_confirmation_open,
                exit_requested: false,
                command_palette_open,
                command_search: command_search.clone(),
                palette_command,
                remote_source_open,
                remote_source_editor: remote_source_editor.clone(),
                remote_source_request: remote_source_request.clone(),
                settings_requested,
                settings_open,
                cache_budget_mib,
                diagnostics_opt_in,
                autosave_enabled,
                autosave_interval_seconds,
                autosave_retention,
                interface_scale_percent,
                high_contrast,
                reduced_motion,
                clear_recent_requested,
                recent_alignment_requested: recent_alignment_requested.clone(),
                recent_session_requested: recent_session_requested.clone(),
                recent_locus_requested: recent_locus_requested.clone(),
                project_issues_open,
                igv_report_open,
                recovery_open,
                recovery_requested: recovery_requested.clone(),
                dismiss_recovery,
            };
            ui::system_dialogs::show_system_dialogs(self, ui, &mut system_dialogs);
            exit_confirmation_open = system_dialogs.exit_confirmation_open;
            user_exit_requested |= system_dialogs.exit_requested;
            command_palette_open = system_dialogs.command_palette_open;
            command_search = system_dialogs.command_search;
            palette_command = system_dialogs.palette_command;
            remote_source_open = system_dialogs.remote_source_open;
            remote_source_editor = system_dialogs.remote_source_editor;
            remote_source_request = system_dialogs.remote_source_request;
            settings_requested = system_dialogs.settings_requested;
            settings_open = system_dialogs.settings_open;
            cache_budget_mib = system_dialogs.cache_budget_mib;
            diagnostics_opt_in = system_dialogs.diagnostics_opt_in;
            autosave_enabled = system_dialogs.autosave_enabled;
            autosave_interval_seconds = system_dialogs.autosave_interval_seconds;
            autosave_retention = system_dialogs.autosave_retention;
            interface_scale_percent = system_dialogs.interface_scale_percent;
            high_contrast = system_dialogs.high_contrast;
            reduced_motion = system_dialogs.reduced_motion;
            clear_recent_requested = system_dialogs.clear_recent_requested;
            recent_alignment_requested = system_dialogs.recent_alignment_requested;
            recent_session_requested = system_dialogs.recent_session_requested;
            recent_locus_requested = system_dialogs.recent_locus_requested;
            project_issues_open = system_dialogs.project_issues_open;
            igv_report_open = system_dialogs.igv_report_open;
            recovery_open = system_dialogs.recovery_open;
            recovery_requested = system_dialogs.recovery_requested;
            dismiss_recovery = system_dialogs.dismiss_recovery;
        });
        self.egui_context.data_mut(|data| {
            data.insert_temp(properties_expanded_id, properties_expanded);
        });
        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |output| output.repaint_delay);
        self.announced_status.clone_from(&data_status);
        self.genomic_canvas_layer_id = genomic_canvas_layer_id;
        self.pane_divider_rect = pane_divider_rect;
        self.command_search = command_search;
        self.exit_confirmation_open = exit_confirmation_open;
        self.user_exit_requested |= user_exit_requested;
        self.diagnostics_open = diagnostics_open;
        self.recovery_open = recovery_open && !dismiss_recovery && recovery_requested.is_none();
        let properties_resize_active =
            native_properties_resize_active || self.automation_properties_resize_active;
        let properties_resize_ended =
            self.properties_resize_gesture_active && !properties_resize_active;
        self.properties_resize_gesture_active = properties_resize_active;
        if properties_resize_active {
            self.pending_resolution_query_refresh = None;
        }
        let workspace_layout_changed =
            self.workspace_layout.revision != new_workspace_layout.revision;
        if workspace_layout_changed {
            self.workspace_layout_recomputations =
                self.workspace_layout_recomputations.saturating_add(1);
        }
        self.workspace_layout = new_workspace_layout;
        if workspace_layout_changed {
            for panel in self.panels.iter_mut() {
                panel
                    .navigation_presentation
                    .invalidate_preview("workspace layout changed");
                panel.navigation_provisional_display = None;
            }
            self.discard_navigation_preview_texture();
        }
        if !committed_canvas_geometry.approximately_equals(new_canvas_geometry) {
            let width_changed = (committed_canvas_geometry.genomic.width()
                - new_canvas_geometry.genomic.width())
            .abs()
                >= 0.5;
            if width_changed {
                self.synchronize_panel_widths();
                if !properties_resize_active {
                    self.schedule_resolution_query_refresh();
                }
            } else if self.synthetic {
                self.display_dirty = true;
            } else {
                self.rebuild_all_panel_displays();
                self.display_dirty = true;
            }
        }
        if properties_resize_ended {
            self.schedule_resolution_query_refresh();
        }
        if overview_panel_id != self.panels.active().id {
            self.activate_view_panel(overview_panel_id);
        }
        self.handle_overview_interaction(overview_interaction);
        self.active_stack_scrollbar_gesture = None;
        for workspace in &mut workspace_scrollbars {
            if workspace.annotation_interaction.pointer_down
                || (workspace.annotation_offset - shared_annotation_offset).abs() > f32::EPSILON
            {
                if workspace.annotation_interaction.pointer_down {
                    self.active_stack_scrollbar_gesture =
                        Some((workspace.panel_id, ui::TrackPane::Annotation));
                }
                if !self.refresh_shared_annotation_scroll_display(workspace.annotation_offset) {
                    self.display_dirty = true;
                }
                self.window.request_redraw();
            }
            let current_read_stack_offset = self
                .panels
                .get(workspace.panel_id)
                .map_or(0.0, |panel| panel.read_stack_scroll_points);
            if workspace.read_stack_interaction.pointer_down
                || (workspace.read_stack_offset - current_read_stack_offset).abs() > f32::EPSILON
            {
                if workspace.read_stack_interaction.pointer_down {
                    self.active_stack_scrollbar_gesture =
                        Some((workspace.panel_id, ui::TrackPane::Reads));
                }
                self.activate_view_panel(workspace.panel_id);
                self.panels.active_mut().read_stack_scroll_points = workspace.read_stack_offset;
                self.rebuild_genomic_display();
                self.display_dirty = true;
                self.window.request_redraw();
            }
            for scrollbar in &mut workspace.read_panels {
                let current_offset = self
                    .panels
                    .get(workspace.panel_id)
                    .map(|panel| {
                        panel
                            .read_panel_scroll_state(scrollbar.track_id)
                            .scroll_points
                    })
                    .unwrap_or_default();
                if scrollbar.interaction.pointer_down
                    || scrollbar.interaction.keyboard_density_pixels.abs() > f32::EPSILON
                    || (scrollbar.offset - current_offset).abs() > f32::EPSILON
                {
                    self.activate_view_panel(workspace.panel_id);
                    self.apply_read_panel_overview_interaction(
                        scrollbar.track_id,
                        scrollbar.interaction,
                        &mut scrollbar.offset,
                    );
                }
            }
        }
        if let Some(id) = editing_alignment_id
            && let Some(control) = alignment_track_controls
                .iter_mut()
                .find(|control| control.id == id)
        {
            if !alignment_name.trim().is_empty() {
                control.name.clone_from(&alignment_name);
            }
            control.visible = alignment_visible;
            control.collapsed = alignment_collapsed;
            control.height = alignment_height;
        }
        if let Some(command) = palette_command {
            match command {
                UiCommand::OpenAlignment => open_alignment_requested = true,
                UiCommand::OpenUrl => remote_source_open = true,
                UiCommand::AddTrack => add_track_requested = true,
                UiCommand::OpenSession => open_session_requested = true,
                UiCommand::SaveSession => save_session_requested = true,
                UiCommand::ExportPng => export_png_requested = true,
                UiCommand::ClearCache => clear_cache_requested = true,
                UiCommand::Settings => settings_open = true,
                UiCommand::FocusLocus => self
                    .egui_context
                    .memory_mut(|memory| memory.request_focus(egui::Id::new(LOCUS_EDITOR_ID))),
                UiCommand::OpenTrackSettings => {
                    if let Some(track) = selected_track {
                        track_settings_panel = Some(track);
                    } else {
                        "Select a track before opening track settings"
                            .clone_into(&mut self.data_status);
                    }
                }
                UiCommand::ToggleTrackVisibility => {
                    if let Some(track) = selected_track {
                        track_ui_actions.push(TrackUiAction::ToggleVisibility(track));
                    }
                }
                UiCommand::ToggleTrackCollapsed => {
                    if let Some(track) = selected_track {
                        track_ui_actions.push(TrackUiAction::ToggleCollapsed(track));
                    }
                }
                UiCommand::PreviousEvidence => self.select_loaded_track_evidence(false),
                UiCommand::NextEvidence => self.select_loaded_track_evidence(true),
                UiCommand::PreviousPanel | UiCommand::NextPanel => {
                    if let Some(current) = panel_controls
                        .iter()
                        .position(|(_, id, _)| *id == active_panel_id)
                    {
                        let forward = command == UiCommand::NextPanel;
                        let target =
                            next_evidence_index(Some(current), panel_controls.len(), forward)
                                .unwrap_or(current);
                        panel_to_activate = panel_controls.get(target).map(|(_, id, _)| *id);
                    }
                }
                UiCommand::Metadata => metadata_open = true,
                UiCommand::Regions => regions_open = true,
                UiCommand::ReviewQueue => review_open = true,
            }
        }
        if show_reads != initial_show_reads {
            track_ui_actions.push(TrackUiAction::SetNodeVisibility(
                PresentationNodeId::Track {
                    channel: ui::TrackPane::Reads,
                    track_id: settings_alignment_id,
                },
                show_reads,
            ));
        }
        if !bulk_alignment_ids.is_empty()
            && (bulk_alignment_visibility.is_some() || bulk_alignment_collapsed.is_some())
        {
            let visible = bulk_alignment_visibility
                .or_else(|| bulk_alignment_collapsed.map(|collapsed| !collapsed))
                .expect("bulk presentation action has a requested state");
            let channel = bulk_alignment_pane.unwrap_or(ui::TrackPane::Annotation);
            track_ui_actions.extend(bulk_alignment_ids.iter().copied().map(|track_id| {
                TrackUiAction::SetNodeVisibility(
                    PresentationNodeId::Track { channel, track_id },
                    visible,
                )
            }));
        }
        self.append_pointer_release_resize_actions(
            self.egui_context
                .input(|input| input.pointer.primary_down()),
            &mut track_ui_actions,
        );
        let track_effects = self.dispatch_track_actions(
            track_ui_actions,
            TrackActionDraft {
                pane_layout: &pane_layout,
                alignment_controls: &mut alignment_track_controls,
                auxiliary_controls: &mut track_controls,
                primary_alignment_id,
                primary_visible: &mut alignment_visible,
                primary_collapsed: &mut alignment_collapsed,
                show_reference: &mut show_reference,
                settings_panel: track_settings_panel,
                metadata_selection: &mut metadata_selected_tracks,
            },
        );
        track_settings_panel = track_effects.settings_panel;
        export_png_requested |= track_effects.export_requested;
        let alignment_track_to_remove = track_effects.alignment_to_remove;
        let alignment_subtracks_to_toggle = track_effects.alignment_subtracks_to_toggle;
        let mut alignment_configuration_changed =
            track_effects.annotation_geometry_changed || track_effects.read_geometry_changed;
        for control in alignment_track_controls {
            if let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == control.id)
            {
                alignment_configuration_changed |= track.config.name != control.name;
                track.config.name = control.name;
            }
        }
        normalize_sample_association(&mut edited_track_sample_key);
        if let Some(id) = editing_alignment_id
            && let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == id)
            && track.config.sample_key != edited_track_sample_key
        {
            track.config.sample_key.clone_from(&edited_track_sample_key);
            alignment_configuration_changed = true;
        }
        if alignment_configuration_changed {
            self.invalidate_navigation_preview_contract("alignment track layout changed");
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if self.alignment_tracks.is_empty() && alignment_visible != self.primary_alignment_visible()
        {
            self.set_primary_alignment_visible(alignment_visible);
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if self.alignment_tracks.is_empty()
            && alignment_collapsed != self.primary_alignment_collapsed()
        {
            self.set_primary_alignment_collapsed(alignment_collapsed);
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if self.alignment_tracks.is_empty()
            && !alignment_name.trim().is_empty()
            && alignment_name != self.primary_alignment_name()
        {
            self.set_primary_alignment_name(alignment_name);
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        self.locus_editor = locus_editor;
        if locus_search_changed
            && !navigate_requested
            && !self.locus_editor.contains(':')
            && self.locus_editor.trim().chars().count() >= 2
        {
            let query = self.locus_editor.clone();
            self.request_annotation_search(&query, false, false);
        }
        if let Some((id, index)) = panel_to_move {
            self.move_view_panel(id, index);
        }
        if let Some(id) = panel_to_close {
            self.close_view_panel(id);
        } else if let Some(id) = panel_to_activate {
            self.activate_view_panel(id);
        }
        if variant_genotypes_export_requested && let Some(inspector) = &variant_inspector {
            let record = &inspector.query.records[inspector.record_index];
            let file_name = variant_export_file_name(record, "variant-genotypes");
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(file_name)
                .add_filter("Tab-separated values", &["tsv"])
                .save_file()
            {
                self.start_export(path, variant_genotype_export_tsv(inspector));
            }
        }
        if variant_evidence_export_requested && let Some(inspector) = &variant_inspector {
            let record = &inspector.query.records[inspector.record_index];
            let file_name = variant_export_file_name(record, "alignment-evidence");
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(file_name)
                .add_filter("Tab-separated values", &["tsv"])
                .save_file()
            {
                self.start_export(
                    path,
                    variant_evidence_export_tsv(inspector, &variant_alignment_evidence),
                );
            }
        }
        if junction_bed_export_requested {
            let bundle = self.sashimi_export_bundle();
            let selected_id = self
                .panels
                .active()
                .selected_alignment_track
                .unwrap_or_else(|| self.primary_alignment_track_id());
            if let Some(track) = bundle
                .tracks
                .iter()
                .find(|track| track.id == selected_id)
                .cloned()
                .or_else(|| bundle.tracks.first().cloned())
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name(rna_export_file_name(&track.name, "junctions", "bed"))
                    .add_filter("BED", &["bed"])
                    .save_file()
            {
                let contig = bundle.contig_name;
                let viewport = bundle.viewport.interval();
                let annotations = bundle.annotations;
                self.start_export_job(path, move |path| {
                    std::fs::write(
                        path,
                        junction_bed_export(&track, &contig, viewport, &annotations),
                    )
                });
            }
        }
        if sashimi_png_export_requested {
            let bundle = self.sashimi_export_bundle();
            if !bundle.tracks.is_empty()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("rgv-sashimi.png")
                    .add_filter("PNG image", &["png"])
                    .save_file()
            {
                self.start_export_job(path, move |path| {
                    let (display, width, height) = sashimi_export_display(&bundle);
                    export_png(&display, width, height, path)
                        .map_err(|error| std::io::Error::other(error.to_string()))
                });
            }
        }
        if sashimi_svg_export_requested {
            let bundle = self.sashimi_export_bundle();
            if !bundle.tracks.is_empty()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("rgv-sashimi.svg")
                    .add_filter("SVG image", &["svg"])
                    .save_file()
            {
                self.start_export_job(path, move |path| {
                    let (display, width, height) = sashimi_export_display(&bundle);
                    std::fs::write(path, sashimi_svg_export(&display, width, height, &bundle))
                });
            }
        }
        if let Some((track_id, position)) = variant_pileup_requested {
            self.select_variant_alignment_evidence(track_id, position, false);
        } else if let Some((track_id, position)) = variant_modification_requested {
            self.select_variant_alignment_evidence(track_id, position, true);
        }
        if let Some((contig, position)) = variant_partner_requested
            .or(variant_endpoint_requested)
            .or(interaction_endpoint_requested)
        {
            if let Err(error) = self.open_genomic_position_in_new_panel(&contig, position) {
                self.data_status = error.to_string();
            }
        } else if mate_panel_requested {
            if let Err(error) = self.open_selected_mate_in_new_panel() {
                self.data_status = error.to_string();
            }
        } else if chain_all_panels_requested {
            match self.open_all_chain_segments_in_panels() {
                Ok((opened, requested)) if opened < requested => {
                    self.data_status = format!(
                        "Opened {opened} of {requested} supplementary segments; the {MAXIMUM_VIEW_PANELS}-panel workspace cap was reached"
                    );
                }
                Ok((opened, _)) => {
                    self.data_status = format!("Opened {opened} supplementary segment panels");
                }
                Err(error) => self.data_status = error.to_string(),
            }
        } else if let Some(index) = chain_panel_requested {
            if let Err(error) = self.open_chain_segment_in_new_panel(index) {
                self.data_status = error.to_string();
            }
        } else if let Some(index) = chain_navigation_requested {
            if let Err(error) = self.navigate_to_chain_segment(index) {
                self.data_status = error.to_string();
            }
        } else if let Some(contig) = requested_contig {
            self.navigate_to_contig(contig);
        } else if navigate_back_requested {
            self.navigate_history_back();
        } else if navigate_forward_requested {
            self.navigate_history_forward();
        } else if navigate_home_requested {
            self.navigate_home();
        } else if zoom_in_requested {
            self.keyboard_zoom(0.8);
        } else if zoom_out_requested {
            self.keyboard_zoom(1.25);
        } else if let Some(span) = requested_span {
            let factor = span as f64 / self.panels.active().viewport.span();
            if let Ok(viewport) = self.panels.active_mut().viewport.zoom_at(
                self.panels.active_mut().viewport.pixel_width() * 0.5,
                factor,
                self.panels.active_mut().contig_length,
            ) {
                self.panels.active_mut().viewport = viewport;
                self.panels
                    .active_mut()
                    .navigation_history
                    .visit(viewport.interval());
                self.viewport_changed();
            }
        }
        if let Some(hit) = annotation_hit_requested {
            self.navigate_to_annotation_hit(&hit);
        }
        if previous_feature_requested {
            self.request_adjacent_feature(false);
        } else if next_feature_requested {
            self.request_adjacent_feature(true);
        }
        if navigate_requested {
            let locus = self.locus_editor.clone();
            if locus.contains(':') {
                if let Err(error) = self.navigate_to_loci(&locus) {
                    self.data_status = error.to_string();
                } else {
                    self.persist_settings();
                }
            } else if self
                .annotation_search_results
                .query
                .trim()
                .eq_ignore_ascii_case(locus.trim())
                && self.annotation_search_results.variant_ids_included
            {
                self.submit_annotation_search_results(&locus);
            } else {
                self.request_annotation_search(&locus, true, true);
            }
        }
        if add_current_region {
            if regions.len() >= MAXIMUM_REGIONS_OF_INTEREST {
                self.data_status = format!(
                    "Cannot add another region: the {MAXIMUM_REGIONS_OF_INTEREST}-region workspace cap was reached"
                );
            } else if let Some(id) = next_available_item_id(regions.iter().map(|region| region.id))
            {
                regions.push(RegionOfInterest {
                    id,
                    name: format!("Region {id}"),
                    locus: SessionLocus {
                        contig: contig_name.clone(),
                        start: interval.start(),
                        end: interval.end(),
                    },
                    color: [45, 92, 175, 48],
                });
                regions_open = true;
            } else {
                "Cannot add another region: no stable identifier is available"
                    .clone_into(&mut self.data_status);
            }
        }
        if add_current_review {
            if review_items.len() >= MAXIMUM_REVIEW_ITEMS {
                self.data_status = format!(
                    "Cannot add another review item: the {MAXIMUM_REVIEW_ITEMS}-item workspace cap was reached"
                );
            } else if let Some(id) = next_available_item_id(review_items.iter().map(|item| item.id))
            {
                review_items.push(ReviewItem {
                    id,
                    label: status_locus.clone(),
                    locus: SessionLocus {
                        contig: contig_name.clone(),
                        start: interval.start(),
                        end: interval.end(),
                    },
                    status: ReviewStatus::Pending,
                    notes: String::new(),
                    assignment: None,
                });
                review_open = true;
            } else {
                "Cannot add another review item: no stable identifier is available"
                    .clone_into(&mut self.data_status);
            }
        }
        if add_selected_feature_to_review && let Some(inspector) = &feature_inspector {
            if review_items.len() >= MAXIMUM_REVIEW_ITEMS {
                self.data_status = format!(
                    "Cannot add another review item: the {MAXIMUM_REVIEW_ITEMS}-item workspace cap was reached"
                );
            } else if let Some(id) = next_available_item_id(review_items.iter().map(|item| item.id))
            {
                review_items.push(ReviewItem {
                    id,
                    label: inspector
                        .record
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("{} feature", inspector.track_name)),
                    locus: SessionLocus {
                        contig: inspector.record.contig.clone(),
                        start: inspector.record.start,
                        end: inspector.record.end,
                    },
                    status: ReviewStatus::Pending,
                    notes: String::new(),
                    assignment: None,
                });
            }
        }
        if let Some(id) = region_to_delete {
            regions.retain(|region| region.id != id);
        }
        if let Some(id) = review_to_delete {
            review_items.retain(|item| item.id != id);
        }
        if let Some(id) = region_to_relocate
            && let Some(region) = regions.iter_mut().find(|region| region.id == id)
        {
            region.locus = SessionLocus {
                contig: contig_name.clone(),
                start: interval.start(),
                end: interval.end(),
            };
        }
        if let Some(id) = review_to_relocate
            && let Some(item) = review_items.iter_mut().find(|item| item.id == id)
        {
            item.locus = SessionLocus {
                contig: contig_name.clone(),
                start: interval.start(),
                end: interval.end(),
            };
        }
        normalize_regions_and_review_items(&mut regions, &mut review_items);
        if previous_review_requested || next_review_requested {
            managed_locus_requested = adjacent_review_locus(
                &review_items,
                review_status_filter,
                &contig_name,
                interval.start(),
                interval.end(),
                next_review_requested,
            )
            .map(|locus| session_locus_text(&locus));
        }
        let regions_changed = regions != self.regions_of_interest;
        let review_changed = review_items != self.review_items;
        self.regions_of_interest = regions;
        self.review_items = review_items;
        self.review_status_filter = review_status_filter;
        if regions_changed {
            self.invalidate_navigation_preview_contract("region overlays changed");
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        } else if review_changed {
            self.display_dirty = true;
        }
        if metadata_remove_requested {
            self.sample_metadata = None;
            metadata_filter_value = None;
            metadata_selected_tracks.clear();
            "Removed sample metadata; stable track/sample assignments were retained"
                .clone_into(&mut self.data_status);
        }
        if metadata_select_requested
            && let (Some(table), Some(value)) = (&sample_metadata, metadata_filter_value.as_deref())
        {
            metadata_selected_tracks = metadata_matching_track_ids(
                &metadata_track_rows,
                table,
                metadata_filter_column,
                value,
            );
            self.data_status = format!(
                "Selected {} tracks whose {} is {value:?}",
                metadata_selected_tracks.len(),
                table.columns[metadata_filter_column]
            );
        }
        let live_track_ids = metadata_track_rows
            .iter()
            .map(|(id, _, _, _)| *id)
            .collect::<BTreeSet<_>>();
        metadata_selected_tracks.retain(|id| live_track_ids.contains(id));
        let metadata_selection_changed = metadata_selected_tracks != self.metadata_selected_tracks;
        self.metadata_filter_column = metadata_filter_column;
        self.metadata_filter_value = metadata_filter_value;
        self.metadata_selected_tracks = metadata_selected_tracks;
        if metadata_selection_changed {
            let selected_alignment_tracks = self
                .alignment_tracks
                .iter()
                .filter(|track| self.metadata_selected_tracks.contains(&track.config.id))
                .map(|track| track.config.id)
                .collect::<Vec<_>>();
            let selected_auxiliary_tracks = self
                .tracks
                .tracks()
                .iter()
                .filter(|track| self.metadata_selected_tracks.contains(&track.id))
                .map(|track| track.id)
                .collect::<BTreeSet<_>>();
            let selected_nodes = self
                .track_presentation
                .pane_root_nodes(ui::TrackPane::Annotation)
                .into_iter()
                .filter(|node| match node {
                    PresentationNodeId::Track { track_id, .. } => {
                        selected_alignment_tracks.contains(track_id)
                            || selected_auxiliary_tracks.contains(track_id)
                    }
                    PresentationNodeId::Reference | PresentationNodeId::Group { .. } => false,
                })
                .collect::<Vec<_>>();
            let panel = self.panels.active_mut();
            panel.selected_alignment_tracks = selected_alignment_tracks;
            panel.selected_read_tracks.clear();
            panel.read_selection_anchor = None;
            panel.selected_presentation_nodes = selected_nodes;
            panel.presentation_selection_anchor = panel.selected_presentation_nodes.last().copied();
            panel.selected_track =
                panel
                    .selected_presentation_nodes
                    .last()
                    .and_then(|node| match node {
                        PresentationNodeId::Track { track_id, .. } => {
                            if selected_auxiliary_tracks.contains(track_id) {
                                Some(TrackFrameKey::Auxiliary(*track_id))
                            } else {
                                Some(TrackFrameKey::Alignment(*track_id))
                            }
                        }
                        PresentationNodeId::Reference | PresentationNodeId::Group { .. } => None,
                    });
            panel.alignment_selection_anchor = panel.selected_alignment_tracks.last().copied();
        }
        if metadata_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Sample attributes", &["tsv", "txt"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::SampleMetadata);
        }
        if region_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("BED regions", &["bed"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::Regions);
        }
        if region_export_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-regions.bed")
                .add_filter("BED regions", &["bed"])
                .save_file()
        {
            self.start_export(path, region_bed_export(&self.regions_of_interest));
        }
        if review_import_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("RGV review queue", &["tsv"])
                .pick_file()
        {
            self.start_workspace_list_import(path, WorkspaceListKind::Review);
        }
        if review_export_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-review-queue.tsv")
                .add_filter("RGV review queue", &["tsv"])
                .save_file()
        {
            self.start_export(path, review_tsv_export(&self.review_items));
        }
        if let Some(locus) = managed_locus_requested
            && let Err(error) = self.navigate_to_locus(&locus)
        {
            self.data_status = error.to_string();
        }
        self.remote_source_editor = remote_source_editor;
        if let Some(request) = remote_source_request {
            match self.open_remote_source(request) {
                Ok(()) => self.remote_source_editor = RemoteSourceEditor::default(),
                Err(error) => {
                    self.remote_source_editor.error = Some(error.to_string());
                    remote_source_open = true;
                }
            }
        }
        self.open_panel = if command_palette_open {
            Some(UiPanel::CommandPalette)
        } else if remote_source_open {
            Some(UiPanel::RemoteSource)
        } else if settings_open {
            Some(UiPanel::Settings)
        } else if project_issues_open {
            Some(UiPanel::ProjectIssues)
        } else if igv_report_open {
            Some(UiPanel::IgvImportReport)
        } else if metadata_open {
            Some(UiPanel::Metadata)
        } else if regions_open {
            Some(UiPanel::Regions)
        } else if review_open {
            Some(UiPanel::Review)
        } else {
            track_settings_panel.map(UiPanel::TrackSettings)
        };
        let autosave_schedule_changed = self.settings.autosave_enabled != autosave_enabled
            || self.settings.autosave_interval_seconds != autosave_interval_seconds;
        let accessibility_changed = self.settings.interface_scale_percent
            != interface_scale_percent
            || self.settings.high_contrast != high_contrast
            || self.settings.reduced_motion != reduced_motion;
        let settings_changed = self.settings.cache_budget_mib != cache_budget_mib
            || self.settings.diagnostics_opt_in != diagnostics_opt_in
            || autosave_schedule_changed
            || self.settings.autosave_retention != autosave_retention
            || accessibility_changed;
        self.settings.cache_budget_mib = cache_budget_mib;
        self.settings.diagnostics_opt_in = diagnostics_opt_in;
        self.settings.autosave_enabled = autosave_enabled;
        self.settings.autosave_interval_seconds = autosave_interval_seconds;
        self.settings.autosave_retention = autosave_retention;
        self.settings.interface_scale_percent = interface_scale_percent;
        self.settings.high_contrast = high_contrast;
        self.settings.reduced_motion = reduced_motion;
        if accessibility_changed {
            apply_accessibility_preferences(&self.egui_context, &self.settings);
            self.display_dirty = true;
        }
        if autosave_schedule_changed {
            self.schedule_next_autosave();
        }
        if clear_recent_requested {
            self.settings.clear_recent();
        }
        if settings_changed || clear_recent_requested {
            self.persist_settings();
        }
        if let Some(source) = recent_alignment_requested {
            self.open_alignment_source(&source_path(&source));
        }
        if let Some(path) = recent_session_requested {
            self.handle_dropped_file(Path::new(&path));
        }
        if let Some(locus) = recent_locus_requested
            && let Err(error) = self.navigate_to_locus(&locus)
        {
            self.data_status = error.to_string();
        }
        if open_alignment_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Alignment", &["bam", "cram"])
                .pick_file()
        {
            self.open_alignment_source(&path);
        }
        if open_reference_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Reference FASTA", &["fa", "fasta", "fna"])
                .pick_file()
        {
            self.open_reference_source(&path);
        }
        if open_genome_config_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("RGV genome configuration", &["json", "rgvgenome"])
                .pick_file()
            && let Err(error) = self.open_genome_configuration(&path)
        {
            self.data_status = error.to_string();
        }
        if restore_default_genes_requested && let Err(error) = self.restore_default_genes() {
            self.data_status = error.to_string();
        }
        if open_landmark_folder_requested && let Err(error) = self.open_landmark_resource_folder() {
            self.data_status = error.to_string();
        }
        if let Some(bundle_id) = show_all_transcripts_requested
            && let Err(error) = self.show_all_transcripts(&bundle_id)
        {
            self.data_status = error.to_string();
        }
        if let Some(bundle_id) = return_to_representative_genes_requested
            && let Err(error) = self.return_to_representative_genes(&bundle_id)
        {
            self.data_status = error.to_string();
        }
        if add_track_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter(
                    "Genomic track",
                    &[
                        "bed",
                        "gff",
                        "gff3",
                        "gtf",
                        "vcf",
                        "bcf",
                        "bw",
                        "bigwig",
                        "tdf",
                        "bedgraph",
                        "bdg",
                        "wig",
                        "wiggle",
                        "bedpe",
                        "interact",
                        "bb",
                        "bigbed",
                        "biggenepred",
                        "bignarrowpeak",
                        "seg",
                        "cbs",
                        "maf",
                        "genepred",
                        "gp",
                        "refflat",
                        "narrowpeak",
                        "broadpeak",
                        "psl",
                        "linear",
                        "logistic",
                        "assoc",
                        "qassoc",
                        "gwas",
                        "gz",
                    ],
                )
                .pick_file()
            && let Err(error) = self.add_track_source(&path)
        {
            self.data_status = error.to_string();
        }
        if let Some(path) = recovery_requested {
            self.start_session_load(path, SessionLoadKind::Recovery);
        }
        if open_session_requested
            && let Some(path) = rfd::FileDialog::new()
                .add_filter("Genome viewer session", &["json", "xml"])
                .pick_file()
        {
            self.handle_dropped_file(&path);
        }
        if save_session_requested {
            if let Some(path) = self.current_session_path.clone() {
                self.start_session_save(path);
            } else {
                save_session_as_requested = true;
            }
        }
        if save_session_as_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-project.json")
                .add_filter("RGV project", &["json"])
                .save_file()
        {
            self.start_session_save(path);
        }
        if export_png_requested
            && self
                .panels
                .iter()
                .any(|panel| panel.navigation_presentation.is_unsettled())
        {
            "The target viewport is still being checked; export is available after it settles"
                .clone_into(&mut self.data_status);
        } else if export_png_requested
            && let Some(path) = rfd::FileDialog::new()
                .set_file_name("rgv-view.png")
                .add_filter("PNG image", &["png"])
                .save_file()
        {
            let width = (f64::from(self.surface_config.width) / self.window.scale_factor())
                .round()
                .clamp(1.0, f64::from(u32::MAX)) as u32;
            let height = (f64::from(self.surface_config.height) / self.window.scale_factor())
                .round()
                .clamp(1.0, f64::from(u32::MAX)) as u32;
            let export_display = semantic_export_display_list(&self.composite_display_list);
            match export_png(&export_display, width, height, &path) {
                Ok(()) => self.data_status = format!("Exported image to {}", path.display()),
                Err(error) => self.data_status = error.to_string(),
            }
        }
        if clear_cache_requested {
            for track in &self.alignment_tracks {
                if let Err(error) = track.engine.try_clear_cache() {
                    self.data_status = error.to_string();
                }
            }
        }
        if clear_cache_requested {
            self.layout_cache.clear();
        }
        truncate_string_to_bytes(&mut read_name_editor, 1_024);
        self.read_name_editor.clone_from(&read_name_editor);
        truncate_string_to_bytes(&mut variant_sample_search, 256);
        self.variant_sample_search
            .clone_from(&variant_sample_search);
        truncate_string_to_bytes(&mut variant_settings_sample_search, 256);
        truncate_string_to_bytes(&mut variant_predicate_key, 256);
        truncate_string_to_bytes(&mut variant_predicate_value, 256);
        self.variant_settings_sample_search
            .clone_from(&variant_settings_sample_search);
        self.variant_predicate_scope = variant_predicate_scope;
        self.variant_predicate_key
            .clone_from(&variant_predicate_key);
        self.variant_predicate_operator = variant_predicate_operator;
        self.variant_predicate_value
            .clone_from(&variant_predicate_value);
        self.variant_predicate_missing = variant_predicate_missing;
        if select_read_name_requested {
            let name = read_name_editor.trim().as_bytes().to_vec();
            if self.select_read_or_template_by_name(settings_alignment_id, &name) {
                "Selected the exact read/template name from the complete view"
                    .clone_into(&mut self.data_status);
            } else {
                "No exact read/template name is loaded in this track at the current locus"
                    .clone_into(&mut self.data_status);
            }
        }
        if selected_to_top_requested && self.promote_selected_read_to_top() {
            self.rebuild_genomic_display();
        }
        if clear_selection {
            self.clear_promoted_read();
            self.panels.active_mut().clear_selection();
            self.display_dirty = true;
        }
        let editing_primary_settings = settings_alignment_id == primary_alignment_id;
        if editing_primary_settings {
            self.primary_filter_editor_mut().minimum_mapq = minimum_mapq;
            self.primary_filter_editor_mut().include_secondary = include_secondary;
            self.primary_filter_editor_mut().include_supplementary = include_supplementary;
            self.primary_filter_editor_mut()
                .expression
                .clone_from(&filter_expression);
        }
        if editing_primary_settings
            && minimum_base_quality != self.primary_alignment_filters().minimum_base_quality
        {
            self.primary_alignment_filters_mut().minimum_base_quality = minimum_base_quality;
            self.query_options_changed();
        }
        if editing_primary_settings
            && minimum_junction_flank != self.primary_alignment_filters().minimum_junction_flank
        {
            self.primary_alignment_filters_mut().minimum_junction_flank = minimum_junction_flank;
            self.query_options_changed();
        }
        if editing_primary_settings
            && overlapping_mate_policy != self.primary_alignment_filters().overlapping_mate_policy
        {
            self.primary_alignment_filters_mut().overlapping_mate_policy = overlapping_mate_policy;
            self.query_options_changed();
        }
        if editing_primary_settings
            && show_modification_frequency
                != self.primary_alignment_filters().show_modification_frequency
        {
            self.primary_alignment_filters_mut()
                .show_modification_frequency = show_modification_frequency;
            self.panels.active_mut().selected_modification_position = None;
            self.query_options_changed();
        }
        reference_translation_frames.sort();
        reference_translation_frames.dedup();
        if reference_reverse_complement != self.reference_reverse_complement
            || reference_translation_frames != self.reference_translation_frames
            || reference_genetic_code != self.reference_genetic_code
        {
            self.reference_reverse_complement = reference_reverse_complement;
            self.reference_translation_frames = reference_translation_frames;
            self.reference_genetic_code = reference_genetic_code;
            self.invalidate_navigation_preview_contract("reference presentation changed");
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        }
        let requested_coverage_scale =
            use_fixed_coverage_scale.then_some(coverage_scale_maximum.max(1));
        if editing_primary_settings
            && inspector_activation != self.primary_alignment_filters().inspector_activation
        {
            self.primary_alignment_filters_mut().inspector_activation = inspector_activation;
            self.panels.active_mut().hover_inspector_target = None;
        }
        if editing_primary_settings
            && ((minimum_allele_fraction
                - self.primary_alignment_filters().minimum_allele_fraction)
                .abs()
                > f32::EPSILON
                || quality_weighted_alleles
                    != self.primary_alignment_filters().quality_weighted_alleles
                || show_reference != self.primary_alignment_filters().show_reference
                || show_coverage != self.primary_alignment_filters().show_coverage
                || show_junctions != self.primary_alignment_filters().show_junctions
                || minimum_junction_depth
                    != self.primary_alignment_filters().minimum_junction_depth
                || junction_display_mode != self.primary_alignment_filters().junction_display_mode
                || sashimi_scale_mode != self.primary_alignment_filters().sashimi_scale_mode
                || sashimi_fixed_scale_maximum
                    != self.primary_alignment_filters().sashimi_fixed_scale_maximum
                || sashimi_count_mode != self.primary_alignment_filters().sashimi_count_mode
                || show_junction_labels != self.primary_alignment_filters().show_junction_labels
                || requested_coverage_scale
                    != self.primary_alignment_filters().coverage_scale_maximum
                || coverage_scale_transform
                    != self.primary_alignment_filters().coverage_scale_transform
                || show_coverage_center_line
                    != self.primary_alignment_filters().show_coverage_center_line
                || (alignment_density - self.primary_alignment_filters().alignment_density).abs()
                    > f32::EPSILON
                || read_base_display_mode
                    != self.primary_alignment_filters().read_base_display_mode
                || shade_read_bases_by_quality
                    != self.primary_alignment_filters().shade_read_bases_by_quality
                || minimum_indel_size != self.primary_alignment_filters().minimum_indel_size
                || minimum_event_label_size
                    != self.primary_alignment_filters().minimum_event_label_size
                || show_modified_bases != self.primary_alignment_filters().show_modified_bases
                || minimum_modification_probability
                    != self
                        .primary_alignment_filters()
                        .minimum_modification_probability
                || modified_base_color_mode
                    != self.primary_alignment_filters().modified_base_color_mode
                || hidden_modification_codes != self.primary_hidden_modification_codes())
        {
            self.primary_alignment_filters_mut().minimum_allele_fraction = minimum_allele_fraction;
            self.primary_alignment_filters_mut()
                .quality_weighted_alleles = quality_weighted_alleles;
            self.primary_alignment_filters_mut().show_reference = show_reference;
            if !show_coverage {
                self.panels.active_mut().selected_pileup_position = None;
            }
            self.primary_alignment_filters_mut().show_coverage = show_coverage;
            if !show_junctions {
                self.panels.active_mut().selected_junction = None;
                self.panels.active_mut().selected_junction_component = None;
            }
            self.primary_alignment_filters_mut().show_junctions = show_junctions;
            self.primary_alignment_filters_mut().minimum_junction_depth =
                minimum_junction_depth.max(1);
            self.primary_alignment_filters_mut().junction_display_mode = junction_display_mode;
            self.primary_alignment_filters_mut().sashimi_scale_mode = sashimi_scale_mode;
            self.primary_alignment_filters_mut()
                .sashimi_fixed_scale_maximum = sashimi_fixed_scale_maximum.max(1);
            self.primary_alignment_filters_mut().sashimi_count_mode = sashimi_count_mode;
            self.primary_alignment_filters_mut().show_junction_labels = show_junction_labels;
            self.primary_alignment_filters_mut().coverage_scale_maximum = requested_coverage_scale;
            self.primary_alignment_filters_mut()
                .coverage_scale_transform = coverage_scale_transform;
            self.primary_alignment_filters_mut()
                .show_coverage_center_line = show_coverage_center_line;
            self.set_alignment_density_for_track(primary_alignment_id, alignment_density);
            self.primary_alignment_filters_mut().read_base_display_mode = read_base_display_mode;
            self.primary_alignment_filters_mut()
                .shade_read_bases_by_quality = shade_read_bases_by_quality;
            self.primary_alignment_filters_mut().minimum_indel_size = minimum_indel_size;
            self.primary_alignment_filters_mut()
                .minimum_event_label_size = minimum_event_label_size;
            self.primary_alignment_filters_mut().show_modified_bases = show_modified_bases;
            self.primary_alignment_filters_mut()
                .minimum_modification_probability = minimum_modification_probability;
            self.primary_alignment_filters_mut()
                .modified_base_color_mode = modified_base_color_mode;
            self.primary_alignment_filters_mut()
                .hidden_modification_codes = hidden_modification_codes
                .iter()
                .copied()
                .map(modification_code_label)
                .collect();
            self.invalidate_navigation_preview_contract("alignment presentation changed");
            self.layout_cache.clear();
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        let previous_auxiliary_query_membership = auxiliary_query_track_ids(self.tracks.tracks())
            .into_iter()
            .collect::<HashSet<_>>();
        let mut track_configuration_changed = false;
        if let Some(id) = editing_auxiliary_id
            && let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
            && track.sample_key != edited_track_sample_key
        {
            track.sample_key.clone_from(&edited_track_sample_key);
            track_configuration_changed = true;
        }
        for control in track_controls {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == control.id)
                && track.name != control.name
            {
                track.name = control.name;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in feature_settings_controls {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.feature != settings
            {
                track.feature = settings;
                track_configuration_changed = true;
            }
        }
        let mut variant_query_requested = false;
        for (id, settings) in variant_settings_controls {
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.variant != settings
            {
                track.variant = settings;
                variant_query_requested = true;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in gwas_settings_controls {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.gwas != settings
            {
                track.gwas = settings;
                track_configuration_changed = true;
            }
        }
        normalize_quantitative_overlay_settings(
            &mut quantitative_settings_controls,
            editing_auxiliary_id,
        );
        for (id, settings) in quantitative_settings_controls {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.quantitative != settings
            {
                track.quantitative = settings;
                track_configuration_changed = true;
            }
        }
        let mut copy_number_query_requested = false;
        for (id, settings) in copy_number_settings_controls {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.copy_number != settings
            {
                copy_number_query_requested |= track.copy_number.sort_mode != settings.sort_mode
                    || track.copy_number.sort_locus != settings.sort_locus;
                track.copy_number = settings;
                track_configuration_changed = true;
            }
        }
        for (id, settings) in mutation_settings_controls {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.mutation != settings
            {
                track.mutation = settings;
                track_configuration_changed = true;
            }
        }
        let mut interaction_query_requested = false;
        for (id, settings) in interaction_settings_controls {
            if !settings.is_valid() {
                continue;
            }
            if let Some(track) = self
                .tracks
                .tracks_mut()
                .iter_mut()
                .find(|track| track.id == id)
                && track.interaction != settings
            {
                interaction_query_requested |=
                    track.interaction.minimum_score != settings.minimum_score;
                track.interaction = settings;
                track_configuration_changed = true;
            }
        }
        if let Some((id, new_index)) = track_move {
            track_configuration_changed |= self.tracks.move_track(id, new_index);
        }
        let auxiliary_query_membership_changed = previous_auxiliary_query_membership
            != auxiliary_query_track_ids(self.tracks.tracks())
                .into_iter()
                .collect::<HashSet<_>>();
        if track_configuration_changed {
            self.invalidate_navigation_preview_contract("auxiliary track presentation changed");
            self.reconcile_track_presentation();
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if auxiliary_query_membership_changed {
            self.invalidate_and_request_all_auxiliary_track_queries();
        } else if variant_query_requested
            || interaction_query_requested
            || copy_number_query_requested
        {
            self.request_all_auxiliary_track_queries();
        }
        if let Some(tag) = parse_style_tag(&style_tag_editor) {
            if matches!(color_mode, AlignmentColorMode::Tag(_)) {
                color_mode = AlignmentColorMode::Tag(tag);
            }
            if matches!(group_mode, AlignmentGroupMode::Tag(_)) {
                group_mode = AlignmentGroupMode::Tag(tag);
            }
            if matches!(sort_mode, AlignmentSortMode::Tag(_)) {
                sort_mode = AlignmentSortMode::Tag(tag);
            }
        }
        let alignment_sort_anchor = (sort_mode == AlignmentSortMode::Base)
            .then(|| {
                (settings_filters.alignment_sort_mode == AlignmentSortMode::Base)
                    .then_some(settings_filters.alignment_sort_anchor)
                    .flatten()
                    .or_else(|| {
                        alignment_sort_position(
                            AlignmentSortMode::Base,
                            None,
                            self.panels.active().viewport,
                        )
                    })
            })
            .flatten();
        if editing_primary_settings
            && (color_mode != self.primary_alignment_filters().alignment_color_mode
                || group_mode != self.primary_alignment_filters().alignment_group_mode
                || group_order != self.primary_alignment_filters().alignment_group_order
                || sort_mode != self.primary_alignment_filters().alignment_sort_mode
                || sort_order != self.primary_alignment_filters().alignment_sort_order
                || alignment_sort_anchor != self.primary_alignment_filters().alignment_sort_anchor
                || read_stacking_mode != self.primary_alignment_filters().read_stacking_mode
                || view_as_pairs != self.primary_alignment_filters().view_as_pairs
                || (override_pair_bounds.then_some(minimum_template_length)
                    != self.primary_alignment_filters().minimum_template_length)
                || (override_pair_bounds.then_some(maximum_template_length)
                    != self.primary_alignment_filters().maximum_template_length))
        {
            let reference_query_changed = alignment_color_mode_needs_reference(
                self.primary_alignment_filters().alignment_color_mode,
            ) != alignment_color_mode_needs_reference(color_mode);
            self.primary_alignment_filters_mut().alignment_color_mode = color_mode;
            self.primary_alignment_filters_mut().alignment_group_mode = group_mode;
            self.primary_alignment_filters_mut().alignment_group_order = group_order;
            self.primary_alignment_filters_mut().alignment_sort_mode = sort_mode;
            self.primary_alignment_filters_mut().alignment_sort_order = sort_order;
            self.primary_alignment_filters_mut().alignment_sort_anchor = alignment_sort_anchor;
            self.primary_alignment_filters_mut().read_stacking_mode = read_stacking_mode;
            self.primary_alignment_filters_mut().view_as_pairs = view_as_pairs;
            if override_pair_bounds {
                maximum_template_length = maximum_template_length.max(minimum_template_length);
                self.primary_alignment_filters_mut().minimum_template_length =
                    Some(minimum_template_length);
                self.primary_alignment_filters_mut().maximum_template_length =
                    Some(maximum_template_length);
            } else {
                self.primary_alignment_filters_mut().minimum_template_length = None;
                self.primary_alignment_filters_mut().maximum_template_length = None;
            }
            if reference_query_changed {
                self.query_options_changed();
            } else {
                if let Some(track) = self.alignment_tracks.first_mut() {
                    track.projection_mut(active_panel_id).alignment_rows = None;
                }
                self.layout_cache.clear();
                self.rebuild_genomic_display();
                self.display_dirty = true;
            }
        }
        if editing_primary_settings {
            self.style_tag_editor.clone_from(&style_tag_editor);
        }
        if editing_primary_settings && apply_filter {
            let editor = self.primary_filter_editor().clone();
            let expressions = editor
                .expression
                .split(';')
                .map(str::trim)
                .filter(|expression| !expression.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            let parsed = expressions
                .iter()
                .map(|expression| expression.parse::<AuxPredicate>())
                .collect::<Result<Vec<_>, _>>();
            match parsed {
                Ok(auxiliary) => {
                    let active = ActiveFilters {
                        alignment: AlignmentFilter {
                            minimum_mapping_quality: (editor.minimum_mapq > 0)
                                .then_some(editor.minimum_mapq),
                            auxiliary,
                            ..AlignmentFilter::default()
                        },
                        include_secondary: editor.include_secondary,
                        include_supplementary: editor.include_supplementary,
                    };
                    let filters = self.primary_alignment_filters_mut();
                    filters.minimum_mapping_quality = active.alignment.minimum_mapping_quality;
                    filters.include_secondary = active.include_secondary;
                    filters.include_supplementary = active.include_supplementary;
                    filters.auxiliary_predicates = expressions;
                    *self.primary_active_filters_mut() = active;
                    self.primary_filter_editor_mut().error = None;
                    self.query_options_changed();
                }
                Err(error) => self.primary_filter_editor_mut().error = Some(error.to_string()),
            }
        }
        if editing_primary_settings
            && complete_read_limit
                != u32::try_from(self.primary_alignment_limit()).unwrap_or(u32::MAX)
        {
            self.set_primary_alignment_limit(complete_read_limit as usize);
            self.query_options_changed();
        }
        if !editing_primary_settings && editing_alignment_id.is_some() {
            let mut filters = settings_filters;
            filters.alignment_color_mode = color_mode;
            filters.alignment_group_mode = group_mode;
            filters.alignment_group_order = group_order;
            filters.alignment_sort_mode = sort_mode;
            filters.alignment_sort_order = sort_order;
            filters.alignment_sort_anchor = alignment_sort_anchor;
            filters.read_stacking_mode = read_stacking_mode;
            filters.view_as_pairs = view_as_pairs;
            filters.minimum_template_length =
                override_pair_bounds.then_some(minimum_template_length);
            filters.maximum_template_length = override_pair_bounds
                .then_some(maximum_template_length.max(minimum_template_length));
            filters.overlapping_mate_policy = overlapping_mate_policy;
            filters.minimum_base_quality = minimum_base_quality;
            filters.minimum_allele_fraction = minimum_allele_fraction;
            filters.quality_weighted_alleles = quality_weighted_alleles;
            filters.show_reference = show_reference;
            filters.show_coverage = show_coverage;
            filters.show_junctions = show_junctions;
            filters.minimum_junction_depth = minimum_junction_depth.max(1);
            filters.minimum_junction_flank = minimum_junction_flank;
            filters.junction_display_mode = junction_display_mode;
            filters.sashimi_scale_mode = sashimi_scale_mode;
            filters.sashimi_fixed_scale_maximum = sashimi_fixed_scale_maximum.max(1);
            filters.sashimi_count_mode = sashimi_count_mode;
            filters.show_junction_labels = show_junction_labels;
            filters.coverage_scale_maximum = requested_coverage_scale;
            filters.coverage_scale_transform = coverage_scale_transform;
            filters.show_coverage_center_line = show_coverage_center_line;
            filters.alignment_density =
                alignment_density.clamp(MIN_VERTICAL_DENSITY, MAX_VERTICAL_DENSITY);
            filters.inspector_activation = inspector_activation;
            filters.read_base_display_mode = read_base_display_mode;
            filters.shade_read_bases_by_quality = shade_read_bases_by_quality;
            filters.minimum_indel_size = minimum_indel_size;
            filters.minimum_event_label_size = minimum_event_label_size;
            filters.show_modified_bases = show_modified_bases;
            filters.show_modification_frequency = show_modification_frequency;
            filters.minimum_modification_probability = minimum_modification_probability;
            filters.modified_base_color_mode = modified_base_color_mode;
            filters.hidden_modification_codes = hidden_modification_codes
                .iter()
                .copied()
                .map(modification_code_label)
                .collect();
            self.apply_secondary_alignment_settings(
                settings_alignment_id,
                AlignmentSettingsDraft {
                    filters,
                    filter_editor: FilterEditor {
                        minimum_mapq,
                        include_secondary,
                        include_supplementary,
                        expression: filter_expression,
                        error: filter_error,
                    },
                    maximum_alignments: complete_read_limit as usize,
                    apply_filter,
                },
            );
        }
        if compact_repack_requested
            && let Some(track) = self
                .alignment_tracks
                .iter_mut()
                .find(|track| track.config.id == settings_alignment_id)
        {
            let projection = track.projection_mut(active_panel_id);
            projection.pending_row_repack = true;
            projection.alignment_rows = None;
            projection.cached_layout = None;
            self.rebuild_genomic_display();
            self.display_dirty = true;
        }
        if apply_bulk_alignment_appearance
            && let Err(error) =
                self.apply_bulk_alignment_appearance(settings_alignment_id, &bulk_alignment_ids)
        {
            self.data_status = error.to_string();
        }
        for (track_id, subtrack) in alignment_subtracks_to_toggle {
            self.toggle_alignment_subtrack(track_id, subtrack);
        }
        self.metadata_bulk_height = metadata_bulk_height;
        self.metadata_remove_armed = metadata_remove_armed && !metadata_selection_changed;
        let metadata_selected = self.metadata_selected_tracks.clone();
        if metadata_bulk_visibility.is_some()
            || metadata_bulk_collapsed.is_some()
            || metadata_bulk_height_requested
            || metadata_bulk_rename_requested
        {
            self.apply_metadata_bulk_state(
                &metadata_selected,
                metadata_bulk_visibility,
                metadata_bulk_collapsed,
                metadata_bulk_height_requested.then_some(metadata_bulk_height),
                metadata_bulk_rename_requested,
            );
        }
        if metadata_bulk_alignment_settings_requested
            && let Err(error) = self.apply_metadata_bulk_alignment_settings(&metadata_selected)
        {
            self.data_status = error.to_string();
        }
        if metadata_bulk_auxiliary_appearance_requested
            && apply_compatible_auxiliary_appearance(self.tracks.tracks_mut(), &metadata_selected)
        {
            self.rebuild_all_panel_displays();
            self.display_dirty = true;
        }
        if metadata_bulk_quantitative_autoscale_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Autoscale,
            );
        } else if metadata_bulk_quantitative_overlay_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Overlay,
            );
        } else if metadata_bulk_quantitative_separate_requested {
            self.apply_metadata_quantitative_action(
                &metadata_selected,
                QuantitativeBulkAction::Separate,
            );
        }
        if metadata_bulk_remove_requested {
            match self.remove_metadata_selected_tracks(&metadata_selected) {
                Ok(removed) => {
                    self.data_status = format!("Removed {removed} selected tracks");
                }
                Err(error) => self.data_status = error.to_string(),
            }
        }
        if let Some(id) = alignment_track_to_remove
            && let Err(error) = self.remove_alignment_track(id)
        {
            self.data_status = error.to_string();
        }
        (full_output, repaint_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_layout(
        workspace_right: f32,
        cytobands: ui::CytobandAvailability,
        revision: u64,
    ) -> ui::WorkspaceLayoutSnapshot {
        let workspace =
            egui::Rect::from_min_max(egui::pos2(0.0, 62.0), egui::pos2(workspace_right, 776.0));
        ui::WorkspaceLayoutSnapshot::compute(
            ui::WorkspaceLayoutInput::new(
                workspace,
                egui::Rect::from_min_max(
                    egui::pos2(workspace_right, 62.0),
                    egui::pos2(1_280.0, 776.0),
                ),
                true,
                vec![ui::WorkspacePanelLayoutInput {
                    panel_id: PanelId::new(1),
                    contig_name: "chr1".to_owned(),
                    contig_length: 3_000_000,
                    cytobands,
                }],
                ui::GenomeContextMetrics::default(),
                2.0,
                1.0,
            ),
            revision,
        )
    }

    #[test]
    fn same_frame_genomic_text_rebases_to_the_new_committed_layout() {
        let previous = text_layout(1_250.0, ui::CytobandAvailability::Absent, 1);
        let current = text_layout(940.0, ui::CytobandAvailability::Present, 2);
        let old_canvas = previous.panels[0].canvas;
        let new_canvas = current.panels[0].canvas;
        let original = rgv_layout::TextPrimitive {
            x: old_canvas.left() + old_canvas.width() * 0.25,
            y: previous.track_viewport.top() + 40.0,
            text: "A".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        };

        let remapped = remap_genomic_texts_for_layout(&[original], &previous, &current);

        assert_eq!(remapped.len(), 1);
        assert!((remapped[0].x - (new_canvas.left() + new_canvas.width() * 0.25)).abs() < 0.01);
        assert!((remapped[0].y - (current.track_viewport.top() + 40.0)).abs() < 0.01);
    }

    #[test]
    fn same_frame_text_for_a_non_panel_region_is_not_left_interactive_or_visible() {
        let previous = text_layout(1_250.0, ui::CytobandAvailability::Absent, 1);
        let current = text_layout(940.0, ui::CytobandAvailability::Absent, 2);
        let outside = rgv_layout::TextPrimitive {
            x: previous.track_gutter.center().x,
            y: previous.track_viewport.top() + 20.0,
            text: "stale".to_owned(),
            font_size: 12.0,
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
        };

        assert!(remap_genomic_texts_for_layout(&[outside], &previous, &current).is_empty());
    }

    #[test]
    fn pane_snapshot_supplies_section_chrome_bounds_without_a_scroll_cache() {
        let mut layout = ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation);
        layout.pane_rect =
            egui::Rect::from_min_max(egui::pos2(0.0, 220.0), egui::pos2(800.0, 540.0));

        assert_eq!(pane_vertical_bounds(&layout, 100.0), (220.0, 540.0));
        assert_eq!(
            pane_vertical_bounds(
                &ui::PaneLayoutSnapshot::empty(ui::TrackPane::Annotation),
                100.0,
            ),
            (100.0, 100.0)
        );
    }
}
