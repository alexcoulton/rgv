//! Frame preparation: snapshot application state into the per-frame drafts.
//!
//! Each function owns one draft from `state.rs`. The side-effecting steps (diagnostic phase
//! marks, the pending-command take, overview caching and egui temp data) keep the order
//! `render_shell_frame` calls them in.

use crate::app::actions::TrackControl;
use crate::app::alignment_context::{AlignmentContextMenuState, AlignmentContextMenuView};
use crate::app::status_activity::StatusActivity;
use crate::diagnostics::ManualDiagnosticFramePhase;
use crate::display::alignment::persisted_style_tag;
use crate::display::auxiliary::snapshot_record_count;
use crate::inspection::rna::selected_supplementary_chain;
use crate::ui::alignment_settings::parse_modification_code;
use crate::ui::frame::{
    AlignmentSettingsFrame, CommandDraft, FrameSnapshot, InspectorDraft, LayoutFrame,
    NavigationDraft, PanelContext, TrackControlsDraft, WorkspaceListsDraft,
    WorkspaceScrollbarsDraft, pane_vertical_bounds, read_panel_scrollbar_draft,
};
use crate::ui::{TrackFrameKey, UiCommand};
use crate::{UiPanel, Viewer, ui};
use rgv_core::{
    CopyNumberSortLocus, PanelId, PresentationNodeId, SessionFilters, TrackId, TrackKind,
};
use rgv_engine::{TrackSearchResults, TrackSnapshotData};
use rgv_hts::SequenceData;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

impl Viewer {
    /// Active-panel status, diagnostics and inspector snapshots.
    pub(super) fn prepare_frame_snapshot(&mut self) -> FrameSnapshot {
        let interval = self.panels.active().viewport.interval();
        let bases_per_pixel = self.panels.active().viewport.bases_per_pixel();
        let detail = self.panels.active().viewport.detail_level();
        let contig_name = self.panels.active().contig_name.clone();
        let navigation = &self.panels.active().navigation_presentation;
        let alignment_navigation_status = self.alignment_tracks.iter().find_map(|track| {
            let projection = track.projection(self.panels.active().id)?;
            (!projection
                .navigation
                .is_current_settled(projection.clock.current().get()))
            .then(|| {
                let diagnostic = projection.navigation.diagnostic();
                diagnostic.invariant_failure.as_deref().map_or_else(
                    || {
                        (
                            diagnostic.phase.status().to_owned(),
                            diagnostic.phase.activity(),
                        )
                    },
                    |failure| {
                        (
                            format!("Alignment navigation stopped: {failure}"),
                            StatusActivity::Idle,
                        )
                    },
                )
            })
        });
        let (data_status, status_activity) = if navigation.is_unsettled() {
            if navigation.notice_visible {
                alignment_navigation_status.unwrap_or_else(|| {
                    (
                        "Updating contextual tracks…".to_owned(),
                        StatusActivity::Idle,
                    )
                })
            } else if navigation.source_status.is_empty() {
                (self.data_status.clone(), self.status_activity())
            } else {
                (navigation.source_status.clone(), navigation.source_activity)
            }
        } else {
            (self.data_status.clone(), self.status_activity())
        };
        let announce_status = self.announced_status != data_status;
        let active_panel_id = self.panels.active().id;
        let diagnostics_started = Instant::now();
        // Building the diagnostics report walks every engine and formats dozens of strings;
        // it is only shown in the Diagnostics window.
        let diagnostics = if self.diagnostics_open {
            self.diagnostics_snapshot()
        } else {
            ui::diagnostics::DiagnosticsSnapshot::default()
        };
        let manual_trace = self.manual_diagnostic_trace_snapshot();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellDiagnostics,
            diagnostics_started.elapsed(),
        );
        let inspectors_started = Instant::now();
        // Built once per frame and shared with the Properties panel, which used to build every
        // one of these a second time.
        let inspectors = ui::inspectors::InspectorFrame::capture(self);
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellInspectors,
            inspectors_started.elapsed(),
        );
        let status_locus = format!("{contig_name}:{}-{}", interval.start() + 1, interval.end());
        let selection_summary = inspectors
            .inspector
            .as_ref()
            .or(inspectors.pileup_inspector.as_ref())
            .or(inspectors.modification_inspector.as_ref())
            .or(inspectors.junction_inspector.as_ref())
            .and_then(|snapshot| snapshot.fields.first())
            .map(|(_, value)| format!("Selected: {value}"));
        FrameSnapshot {
            interval,
            bases_per_pixel,
            detail,
            contig_name,
            data_status,
            status_activity,
            announce_status,
            active_panel_id,
            diagnostics,
            manual_trace,
            inspectors,
            status_locus,
            selection_summary,
        }
    }

    pub(super) fn prepare_inspector_draft(&self) -> InspectorDraft {
        let clear_selection = false;
        let selected_to_top_requested = false;
        let chain_navigation_requested = None;
        let mate_panel_requested = false;
        let chain_panel_requested = None;
        let chain_all_panels_requested = false;
        let variant_partner_requested = None;
        let variant_endpoint_requested = None;
        let interaction_endpoint_requested = None;
        let variant_pileup_requested = None;
        let variant_modification_requested = None;
        let variant_genotypes_export_requested = false;
        let variant_evidence_export_requested = false;
        let junction_bed_export_requested = false;
        let sashimi_png_export_requested = false;
        let sashimi_svg_export_requested = false;
        let show_all_transcripts_requested = None;
        let return_to_representative_genes_requested = None;
        let variant_sample_search = self.variant_sample_search.clone();
        InspectorDraft {
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
            show_all_transcripts_requested,
            return_to_representative_genes_requested,
            variant_sample_search,
        }
    }

    /// Editable copy of the settings of the alignment track open in "Track settings" (the
    /// primary track when none is open).
    #[allow(clippy::too_many_lines)]
    pub(super) fn prepare_alignment_settings(&mut self) -> AlignmentSettingsFrame {
        let primary_alignment_id = self.primary_alignment_track_id();
        let editing_alignment_id = match self.open_panel {
            Some(UiPanel::TrackSettings(TrackFrameKey::Alignment(id))) => Some(id),
            _ => None,
        };
        let editing_auxiliary_id = match self.open_panel {
            Some(UiPanel::TrackSettings(TrackFrameKey::Auxiliary(id))) => Some(id),
            _ => None,
        };
        let edited_track_sample_key = editing_alignment_id
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
        let pair_summary_started = Instant::now();
        let settings_pair_summary = self.alignment_pair_summary_for_track(settings_alignment_id);
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellPairSummary,
            pair_summary_started.elapsed(),
        );
        let available_modification_codes = editing_alignment_id.map_or_else(Vec::new, |track_id| {
            self.available_modification_codes(track_id)
        });
        let settings_controller = self
            .alignment_tracks
            .iter()
            .find(|track| track.config.id == settings_alignment_id)
            .map(|track| {
                (
                    track.config.filters.clone(),
                    track.filter_editor.clone(),
                    track.config.maximum_alignments,
                    track.config.name.clone(),
                )
            });
        let settings_filters = if settings_alignment_id == primary_alignment_id {
            self.live_session_filters()
        } else {
            settings_controller
                .as_ref()
                .map_or_else(SessionFilters::default, |track| track.0.clone())
        };
        let settings_filter_editor = if settings_alignment_id == primary_alignment_id {
            self.primary_filter_editor().clone()
        } else {
            settings_controller.as_ref().map_or_else(
                || self.primary_filter_editor().clone(),
                |track| track.1.clone(),
            )
        };
        let minimum_mapq = settings_filter_editor.minimum_mapq;
        let minimum_base_quality = settings_filters.minimum_base_quality;
        let minimum_allele_fraction = settings_filters.minimum_allele_fraction;
        let quality_weighted_alleles = settings_filters.quality_weighted_alleles;
        let show_reference = settings_filters.show_reference;
        let reference_reverse_complement = self.reference_reverse_complement;
        let reference_translation_frames = self.reference_translation_frames.clone();
        let reference_genetic_code = self.reference_genetic_code;
        let show_coverage = settings_filters.show_coverage;
        let show_junctions = settings_filters.show_junctions;
        let minimum_junction_depth = settings_filters.minimum_junction_depth;
        let minimum_junction_flank = settings_filters.minimum_junction_flank;
        let junction_display_mode = settings_filters.junction_display_mode;
        let sashimi_scale_mode = settings_filters.sashimi_scale_mode;
        let sashimi_fixed_scale_maximum = settings_filters.sashimi_fixed_scale_maximum;
        let sashimi_count_mode = settings_filters.sashimi_count_mode;
        let show_junction_labels = settings_filters.show_junction_labels;
        let initial_show_reads = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Reads, settings_alignment_id)
            .is_none_or(|node| node.content_visible);
        let show_reads = initial_show_reads;
        let use_fixed_coverage_scale = settings_filters.coverage_scale_maximum.is_some();
        let coverage_scale_maximum = settings_filters.coverage_scale_maximum.unwrap_or(100);
        let coverage_scale_transform = settings_filters.coverage_scale_transform;
        let show_coverage_center_line = settings_filters.show_coverage_center_line;
        let alignment_density = settings_filters.alignment_density;
        let inspector_activation = settings_filters.inspector_activation;
        let read_base_display_mode = settings_filters.read_base_display_mode;
        let shade_read_bases_by_quality = settings_filters.shade_read_bases_by_quality;
        let minimum_indel_size = settings_filters.minimum_indel_size;
        let minimum_event_label_size = settings_filters.minimum_event_label_size;
        let show_modified_bases = settings_filters.show_modified_bases;
        let show_modification_frequency = settings_filters.show_modification_frequency;
        let minimum_modification_probability = settings_filters.minimum_modification_probability;
        let modified_base_color_mode = settings_filters.modified_base_color_mode;
        let hidden_modification_codes = settings_filters
            .hidden_modification_codes
            .iter()
            .filter_map(|value| parse_modification_code(value))
            .collect::<Vec<_>>();
        let include_secondary = settings_filter_editor.include_secondary;
        let include_supplementary = settings_filter_editor.include_supplementary;
        let filter_expression = settings_filter_editor.expression.clone();
        let filter_error = settings_filter_editor.error.clone();
        let apply_filter = false;
        let typed_alignment_predicates_changed = false;
        let alignment_highlight_predicates_changed = false;
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
        let apply_bulk_alignment_appearance = false;
        let bulk_alignment_visibility = None;
        let bulk_alignment_collapsed = None;
        let complete_read_limit = u32::try_from(
            settings_controller
                .as_ref()
                .map_or(self.primary_alignment_limit(), |track| track.2),
        )
        .unwrap_or(u32::MAX)
        .clamp(1, rgv_core::MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT as u32);
        let color_mode = settings_filters.alignment_color_mode;
        let group_mode = settings_filters.alignment_group_mode;
        let group_order = settings_filters.alignment_group_order;
        let sort_mode = settings_filters.alignment_sort_mode;
        let sort_order = settings_filters.alignment_sort_order;
        let read_stacking_mode = settings_filters.read_stacking_mode;
        let read_sampling = settings_filters.read_sampling;
        let compact_repack_requested = false;
        let view_as_pairs = settings_filters.view_as_pairs;
        let pair_summary = settings_pair_summary.unwrap_or_default();
        let inferred_pair_bounds = pair_summary.inferred_bounds.unwrap_or_default();
        let override_pair_bounds = settings_filters.minimum_template_length.is_some()
            || settings_filters.maximum_template_length.is_some();
        let minimum_template_length = settings_filters
            .minimum_template_length
            .or(inferred_pair_bounds.minimum)
            .unwrap_or_default();
        let maximum_template_length = settings_filters
            .maximum_template_length
            .or(inferred_pair_bounds.maximum)
            .unwrap_or(1_000)
            .max(minimum_template_length);
        let overlapping_mate_policy = settings_filters.overlapping_mate_policy;
        let style_tag_editor = persisted_style_tag(color_mode, group_mode, sort_mode);
        let read_name_editor = self.read_name_editor.clone();
        let select_read_name_requested = false;
        let annotation_presentation_state = self
            .track_presentation
            .pane_track_state(ui::TrackPane::Annotation, settings_alignment_id);
        let alignment_visible =
            annotation_presentation_state.is_none_or(|state| state.content_visible);
        let alignment_collapsed = !alignment_visible;
        let alignment_name = settings_controller.as_ref().map_or_else(
            || self.primary_alignment_name().to_owned(),
            |track| track.3.clone(),
        );
        let alignment_height = annotation_presentation_state
            .and_then(|state| state.preferred_height)
            .unwrap_or(120.0);
        let reference_configured = self.primary_alignment_dictionary().map_or_else(
            || self.reference_revision.is_some(),
            |dictionary| {
                self.compatible_alignment_reference_revision(dictionary)
                    .is_some()
            },
        );
        AlignmentSettingsFrame {
            primary_alignment_id,
            editing_alignment_id,
            editing_auxiliary_id,
            edited_track_sample_key,
            settings_alignment_id,
            available_modification_codes,
            settings_filters,
            minimum_mapq,
            minimum_base_quality,
            minimum_allele_fraction,
            quality_weighted_alleles,
            show_reference,
            reference_reverse_complement,
            reference_translation_frames,
            reference_genetic_code,
            show_coverage,
            show_junctions,
            minimum_junction_depth,
            minimum_junction_flank,
            junction_display_mode,
            sashimi_scale_mode,
            sashimi_fixed_scale_maximum,
            sashimi_count_mode,
            show_junction_labels,
            initial_show_reads,
            show_reads,
            use_fixed_coverage_scale,
            coverage_scale_maximum,
            coverage_scale_transform,
            show_coverage_center_line,
            alignment_density,
            inspector_activation,
            read_base_display_mode,
            shade_read_bases_by_quality,
            minimum_indel_size,
            minimum_event_label_size,
            show_modified_bases,
            show_modification_frequency,
            minimum_modification_probability,
            modified_base_color_mode,
            hidden_modification_codes,
            include_secondary,
            include_supplementary,
            filter_expression,
            filter_error,
            apply_filter,
            typed_alignment_predicates_changed,
            alignment_highlight_predicates_changed,
            bulk_alignment_ids,
            bulk_alignment_pane,
            apply_bulk_alignment_appearance,
            bulk_alignment_visibility,
            bulk_alignment_collapsed,
            complete_read_limit,
            color_mode,
            group_mode,
            group_order,
            sort_mode,
            sort_order,
            read_stacking_mode,
            read_sampling,
            compact_repack_requested,
            view_as_pairs,
            pair_summary,
            override_pair_bounds,
            minimum_template_length,
            maximum_template_length,
            overlapping_mate_policy,
            style_tag_editor,
            read_name_editor,
            select_read_name_requested,
            alignment_visible,
            alignment_collapsed,
            alignment_name,
            alignment_height,
            reference_configured,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn prepare_track_controls(
        &self,
        primary_alignment_id: TrackId,
    ) -> TrackControlsDraft {
        let builtin_track_identities = self
            .tracks
            .tracks()
            .iter()
            .filter_map(|track| track.built_in.clone().map(|identity| (track.id, identity)))
            .collect::<BTreeMap<_, _>>();
        let variant_settings_sample_search = self.variant_settings_sample_search.clone();
        let variant_predicate_scope = self.variant_predicate_scope;
        let variant_predicate_key = self.variant_predicate_key.clone();
        let variant_predicate_operator = self.variant_predicate_operator;
        let variant_predicate_value = self.variant_predicate_value.clone();
        let variant_predicate_missing = self.variant_predicate_missing;
        let variant_predicate_error = None;
        let alignment_track_controls = self
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
        let track_controls = self
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
        let feature_settings_controls = self
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
        let variant_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Variant)
            .map(|track| (track.id, track.variant.clone()))
            .collect::<Vec<_>>();
        let gwas_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Gwas)
            .map(|track| (track.id, track.gwas.clone()))
            .collect::<Vec<_>>();
        let quantitative_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| matches!(track.kind, TrackKind::Quantitative | TrackKind::CopyNumber))
            .map(|track| (track.id, track.quantitative.clone()))
            .collect::<Vec<_>>();
        let copy_number_settings_controls = self
            .tracks
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::CopyNumber)
            .map(|track| (track.id, track.copy_number.clone()))
            .collect::<Vec<_>>();
        let mutation_settings_controls = self
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
        let interaction_settings_controls = self
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
        let track_move = None;
        let track_ui_actions = Vec::new();
        let rendered_track_settings_panel = match self.open_panel {
            Some(UiPanel::TrackSettings(key)) => Some(key),
            _ => None,
        };
        let track_settings_panel = match self.open_panel {
            Some(UiPanel::TrackSettings(key)) => Some(key),
            _ => None,
        };
        TrackControlsDraft {
            builtin_track_identities,
            variant_settings_sample_search,
            variant_predicate_scope,
            variant_predicate_key,
            variant_predicate_operator,
            variant_predicate_value,
            variant_predicate_missing,
            variant_predicate_error,
            alignment_track_controls,
            alignment_subtrack_controls,
            track_controls,
            feature_settings_controls,
            natural_row_height_tracks,
            variant_settings_controls,
            gwas_settings_controls,
            quantitative_settings_controls,
            copy_number_settings_controls,
            mutation_settings_controls,
            copy_number_sort_locus,
            quantitative_track_choices,
            interaction_settings_controls,
            variant_settings_queries,
            track_counts,
            track_move,
            track_ui_actions,
            rendered_track_settings_panel,
            track_settings_panel,
        }
    }

    fn prepare_shell_ui_draft(&mut self) -> super::shell_dialogs::ShellUiDraft {
        let shell = &mut self.shell;
        if shell.notifications_open {
            // Everything listed in the open window has been seen.
            shell.notifications.mark_seen();
        }
        super::shell_dialogs::ShellUiDraft {
            unsaved_prompt: shell
                .unsaved_gate
                .prompt()
                .map(crate::app::shell_state::GuardedAction::prompt),
            unsaved_choice: None,
            notifications_open: shell.notifications_open,
            error_count: shell.notifications.error_count(),
            unseen_error_count: shell.notifications.unseen_error_count(),
            // The log is bounded and copied only while its window is open.
            notifications: if shell.notifications_open {
                shell.notifications.entries().rev().cloned().collect()
            } else {
                Vec::new()
            },
            dismiss_notification: None,
            clear_notifications: false,
        }
    }

    /// Takes the pending UI command and seeds the menu/dialog request flags from it.
    #[allow(clippy::too_many_lines)]
    pub(super) fn prepare_command_draft(&mut self) -> CommandDraft {
        let pending_command = self.pending_command.take();
        let open_alignment_requested = pending_command == Some(UiCommand::OpenAlignment);
        let open_reference_requested = false;
        let open_genome_config_requested = false;
        let hosted_genome_filter = self.hosted_genomes.filter.clone();
        let hosted_genome_requested = None::<String>;
        let refresh_hosted_genomes_requested = false;
        let restore_default_genes_requested = false;
        let open_landmark_folder_requested = false;
        let add_track_requested = pending_command == Some(UiCommand::AddTrack);
        let open_session_requested = pending_command == Some(UiCommand::OpenSession);
        let save_session_requested = pending_command == Some(UiCommand::SaveSession);
        let save_session_as_requested = false;
        let export_png_requested = pending_command == Some(UiCommand::ExportPng);
        let clear_cache_requested = pending_command == Some(UiCommand::ClearCache);
        let settings_requested = pending_command == Some(UiCommand::Settings);
        let focus_locus_requested = pending_command == Some(UiCommand::FocusLocus);
        let command_palette_open = self.open_panel == Some(UiPanel::CommandPalette);
        let exit_confirmation_open = self.exit_confirmation_open;
        let user_exit_requested = false;
        let command_search = self.command_search.clone();
        let palette_command = None;
        let remote_source_open = self.open_panel == Some(UiPanel::RemoteSource);
        let remote_source_editor = self.remote_source_editor.clone();
        let remote_source_request = None;
        let settings_open = self.open_panel == Some(UiPanel::Settings);
        let project_issues_open = self.open_panel == Some(UiPanel::ProjectIssues);
        let unresolved_session_sources = self.unresolved_session_sources.clone();
        let session_migration_notices = self.session_migration_notices.clone();
        let igv_report_open = self.open_panel == Some(UiPanel::IgvImportReport);
        let igv_import_report = self.igv_import_report.clone();
        let recovery_open = self.recovery_open;
        let recovery_requested = None;
        let dismiss_recovery = false;
        let cache_budget_mib = self.settings.cache_budget_mib;
        let read_browsing_buffer_percent = self.settings.read_browsing_buffer_percent;
        let diagnostics_opt_in = self.settings.diagnostics_opt_in;
        let autosave_enabled = self.settings.autosave_enabled;
        let autosave_interval_seconds = self.settings.autosave_interval_seconds;
        let autosave_retention = self.settings.autosave_retention;
        let interface_scale_percent = self.settings.interface_scale_percent;
        let high_contrast = self.settings.high_contrast;
        let interface_theme = self.settings.theme;
        let reduced_motion = self.settings.reduced_motion;
        let recent_loci = self.settings.recent_loci.clone();
        let current_session_path = self.current_session_path.clone();
        let clear_recent_requested = false;
        let recent_alignment_requested = None;
        let recent_session_requested = None;
        let recent_locus_requested = None;
        let diagnostics_open = self.diagnostics_open;
        let manual_trace_action = None;
        let command_key = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        CommandDraft {
            open_alignment_requested,
            open_reference_requested,
            open_genome_config_requested,
            hosted_genome_filter,
            hosted_genome_requested,
            refresh_hosted_genomes_requested,
            restore_default_genes_requested,
            open_landmark_folder_requested,
            add_track_requested,
            open_session_requested,
            save_session_requested,
            save_session_as_requested,
            export_png_requested,
            clear_cache_requested,
            settings_requested,
            focus_locus_requested,
            command_palette_open,
            exit_confirmation_open,
            user_exit_requested,
            command_search,
            palette_command,
            remote_source_open,
            remote_source_editor,
            remote_source_request,
            settings_open,
            project_issues_open,
            unresolved_session_sources,
            session_migration_notices,
            igv_report_open,
            igv_import_report,
            recovery_open,
            recovery_requested,
            dismiss_recovery,
            cache_budget_mib,
            read_browsing_buffer_percent,
            diagnostics_opt_in,
            autosave_enabled,
            autosave_interval_seconds,
            autosave_retention,
            interface_scale_percent,
            high_contrast,
            interface_theme,
            reduced_motion,
            recent_loci,
            current_session_path,
            clear_recent_requested,
            recent_alignment_requested,
            recent_session_requested,
            recent_locus_requested,
            diagnostics_open,
            manual_trace_action,
            command_key,
            shell: self.prepare_shell_ui_draft(),
        }
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn prepare_layout(
        &mut self,
        active_panel_id: PanelId,
        bulk_alignment_ids: &[TrackId],
        bulk_alignment_pane: Option<ui::TrackPane>,
    ) -> LayoutFrame {
        let committed_canvas_geometry = self.canvas_geometry();
        let new_canvas_geometry = committed_canvas_geometry;
        let previous_workspace_layout = self.workspace_layout.clone();
        let new_workspace_layout = previous_workspace_layout.clone();
        let native_properties_resize_active = false;
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
        let bulk_alignment_group = track_groups
            .iter()
            .find(|group| {
                bulk_alignment_pane == Some(group.pane)
                    && bulk_alignment_ids.len() > 1
                    && bulk_alignment_ids.len() == group.members.len()
                    && bulk_alignment_ids
                        .iter()
                        .all(|id| group.members.contains(id))
            })
            .cloned();
        let scrollbars_started = Instant::now();
        let current_alignment_target_revision = self
            .alignment_scene_publication_authority
            .desired_target_revision();
        let workspace_scrollbars = self
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
                    .live_read_panel_allocations()
                    .map(|(track_id, allocation)| {
                        let cached = panel.scroll_display_cache.as_ref().and_then(|cache| {
                            cache
                                .read_panels
                                .iter()
                                .find(|read_panel| read_panel.track_id == track_id)
                        });
                        let state = panel.read_panel_scroll_state(track_id);
                        let density_drag_fraction = self
                            .vertical_overview_drag
                            .filter(|drag| {
                                drag.density_dragging
                                    && drag.panel_id == panel.id
                                    && drag.track_id == track_id
                            })
                            .map(|drag| drag.virtual_scroll_fraction);
                        read_panel_scrollbar_draft(
                            track_id,
                            allocation,
                            state,
                            self.alignment_density_for_panel_track(panel.id, track_id),
                            density_drag_fraction,
                            cached,
                            current_alignment_target_revision,
                            self.alignment_scene_publication_authority
                                .committed_row_target_matches_desired(panel.id, track_id),
                        )
                    })
                    .collect::<Vec<_>>();
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
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellScrollbars,
            scrollbars_started.elapsed(),
        );
        let (pane_annotation_top, pane_annotation_bottom) = pane_vertical_bounds(
            &pane_layout.annotation,
            committed_canvas_geometry.viewport.top(),
        );
        // Display text is now borrowed by the canvas pass instead of copied here. The phase is
        // still reported so recorded traces keep their shape.
        self.diagnostics
            .frame_phase(ManualDiagnosticFramePhase::ShellTextCopy, Duration::ZERO);
        let selected_track = self.panels.active().selected_track;
        let overview_interaction = ui::OverviewInteraction::default();
        let overview_panel_id = active_panel_id;
        let genomic_canvas_layer_id = None;
        let pane_divider_rect = None;
        LayoutFrame {
            committed_canvas_geometry,
            new_canvas_geometry,
            previous_workspace_layout,
            new_workspace_layout,
            native_properties_resize_active,
            pane_layout,
            track_frames,
            annotation_track_count,
            read_track_count,
            shared_annotation_offset,
            selected_annotation_track_ids,
            track_groups,
            bulk_alignment_group,
            workspace_scrollbars,
            pane_annotation_top,
            pane_annotation_bottom,
            selected_track,
            overview_interaction,
            overview_panel_id,
            genomic_canvas_layer_id,
            pane_divider_rect,
        }
    }

    pub(super) fn prepare_navigation_draft(&self) -> NavigationDraft {
        let locus_editor = self.locus_editor.clone();
        let navigate_requested = false;
        let assembly_label = self.assembly.context_label();
        let requested_contig = None;
        let previous_feature_requested = false;
        let next_feature_requested = false;
        // The hits are only read by the popup under the locus field.
        let annotation_search_results = if crate::app::shell_controller::search_popup_visible(
            &self.locus_editor,
            &self.annotation_search_results.query,
            !self.annotation_search_results.hits.is_empty(),
        ) {
            self.annotation_search_results.clone()
        } else {
            TrackSearchResults::default()
        };
        let locus_search_changed = false;
        let search_popup_position = None;
        let annotation_hit_requested = None;
        let navigate_back_requested = false;
        let navigate_forward_requested = false;
        let navigate_home_requested = false;
        let zoom_in_requested = false;
        let zoom_out_requested = false;
        let requested_span = None;
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
        let panel_to_activate = None;
        let panel_to_close = None;
        let panel_to_move = None;
        NavigationDraft {
            locus_editor,
            navigate_requested,
            assembly_label,
            requested_contig,
            previous_feature_requested,
            next_feature_requested,
            annotation_search_results,
            locus_search_changed,
            search_popup_position,
            annotation_hit_requested,
            navigate_back_requested,
            navigate_forward_requested,
            navigate_home_requested,
            zoom_in_requested,
            zoom_out_requested,
            requested_span,
            panel_controls,
            panel_to_activate,
            panel_to_close,
            panel_to_move,
        }
    }

    /// Per-panel genome overview projection (cached per panel).
    pub(super) fn prepare_panel_contexts(&mut self) -> Vec<PanelContext> {
        let overview_projection_started = Instant::now();
        let panel_overview_inputs = self
            .panels
            .iter()
            .map(|panel| {
                (
                    panel.id,
                    panel.contig_name.clone(),
                    panel.contig_length,
                    panel.viewport,
                )
            })
            .collect::<Vec<_>>();
        let panel_contexts = panel_overview_inputs
            .into_iter()
            .map(|(panel_id, contig_name, contig_length, viewport)| {
                let genome_overview =
                    self.genome_overview_for_panel(panel_id, &contig_name, contig_length);
                (
                    panel_id,
                    contig_name,
                    contig_length,
                    viewport,
                    genome_overview,
                )
            })
            .collect::<Vec<_>>();
        self.diagnostics.frame_phase(
            ManualDiagnosticFramePhase::ShellOverviewProjection,
            overview_projection_started.elapsed(),
        );
        panel_contexts
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn prepare_workspace_lists_draft(&self) -> WorkspaceListsDraft {
        let metadata_open = self.open_panel == Some(UiPanel::Metadata);
        let regions_open = self.open_panel == Some(UiPanel::Regions);
        let review_open = self.open_panel == Some(UiPanel::Review);
        // Up to 100,000 entries each: copied only while a window that edits them is open. A
        // menu or inspector request made while they are closed loads them at commit.
        let lists_loaded = regions_open || review_open;
        let (regions, review_items) = if lists_loaded {
            (self.regions_of_interest.clone(), self.review_items.clone())
        } else {
            (Vec::new(), Vec::new())
        };
        let review_status_filter = self.review_status_filter;
        let add_current_region = false;
        let add_current_review = false;
        let add_selected_feature_to_review = false;
        let region_import_requested = false;
        let region_export_requested = false;
        let review_import_requested = false;
        let review_export_requested = false;
        let previous_review_requested = false;
        let next_review_requested = false;
        let managed_locus_requested = None;
        let region_to_delete = None;
        let review_to_delete = None;
        let region_to_relocate = None;
        let review_to_relocate = None;
        // Read by the metadata window and by the sample pickers in track settings.
        let sample_metadata =
            if metadata_open || matches!(self.open_panel, Some(UiPanel::TrackSettings(_))) {
                self.sample_metadata.clone()
            } else {
                None
            };
        let metadata_filter_column = self.metadata_filter_column;
        let metadata_filter_value = self.metadata_filter_value.clone();
        let metadata_selected_tracks = self.metadata_selected_tracks.clone();
        let metadata_bulk_height = self.metadata_bulk_height;
        let metadata_remove_armed = self.metadata_remove_armed;
        let metadata_import_requested = false;
        let metadata_remove_requested = false;
        let metadata_select_requested = false;
        let metadata_bulk_visibility = None;
        let metadata_bulk_collapsed = None;
        let metadata_bulk_height_requested = false;
        let metadata_bulk_rename_requested = false;
        let metadata_bulk_alignment_settings_requested = false;
        let metadata_bulk_auxiliary_appearance_requested = false;
        let metadata_bulk_quantitative_autoscale_requested = false;
        let metadata_bulk_quantitative_overlay_requested = false;
        let metadata_bulk_quantitative_separate_requested = false;
        let metadata_bulk_remove_requested = false;
        let metadata_track_rows = self
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
        WorkspaceListsDraft {
            lists_loaded,
            metadata_open,
            regions_open,
            review_open,
            regions,
            review_items,
            review_status_filter,
            add_current_region,
            add_current_review,
            add_selected_feature_to_review,
            region_import_requested,
            region_export_requested,
            review_import_requested,
            review_export_requested,
            previous_review_requested,
            next_review_requested,
            managed_locus_requested,
            region_to_delete,
            review_to_delete,
            region_to_relocate,
            review_to_relocate,
            sample_metadata,
            metadata_filter_column,
            metadata_filter_value,
            metadata_selected_tracks,
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
            metadata_track_rows,
        }
    }

    /// Properties panel expansion, re-expanded whenever new evidence is selected.
    pub(super) fn prepare_properties_panel_state(&self) -> (egui::Id, bool) {
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
        (properties_expanded_id, properties_expanded)
    }

    pub(super) fn prepare_alignment_context_menu(
        &self,
    ) -> (
        Option<AlignmentContextMenuState>,
        Option<AlignmentContextMenuView>,
    ) {
        let alignment_context_menu = self.alignment_context_menu.clone();
        let alignment_context_menu_view = alignment_context_menu.as_ref().and_then(|menu| {
            let track = self
                .alignment_tracks
                .iter()
                .find(|track| track.config.id == menu.target.track_id)?;
            let record = menu
                .target
                .hit
                .and_then(|hit| self.alignment_scene_record_for_hit(menu.target.panel_id, hit));
            let read_name = record
                .as_ref()
                .map(|record| String::from_utf8_lossy(&record.read_name).into_owned());
            let mate_available = record.as_ref().is_some_and(|record| {
                record.flags.is_paired()
                    && !record.flags.is_mate_unmapped()
                    && record.mate_contig.is_some()
                    && record.mate_start.is_some()
            });
            let supplementary_segments_available = record.as_ref().is_some_and(|record| {
                selected_supplementary_chain(record, &self.panels.active().contig_name)
                    .is_some_and(|chain| chain.segments.len() > 1)
            });
            let sequence_available = record
                .as_ref()
                .is_some_and(|record| matches!(&record.sequence, SequenceData::Present { .. }));
            let view_filter = self
                .presented_alignment_lane(menu.target.panel_id, menu.target.track_id)
                .filter(|presented| presented.lane.scene_contracts_current)
                .and_then(|presented| presented.lane.snapshot.as_ref())
                .map(|snapshot| snapshot.reads().evidence().view_filter());
            Some(AlignmentContextMenuView {
                filters: track.config.filters.clone(),
                contig_name: self
                    .panels
                    .get(menu.target.panel_id)
                    .map(|panel| panel.contig_name.clone())
                    .unwrap_or_default(),
                read_name,
                mate_available,
                supplementary_segments_available,
                sequence_available,
                eligible_records: view_filter.map(|filter| filter.eligible_records()),
                matching_records: view_filter.map(|filter| filter.matching_records()),
                reference_configured: self
                    .compatible_alignment_reference_revision(track.dictionary.as_ref()?)
                    .is_some(),
            })
        });
        (alignment_context_menu, alignment_context_menu_view)
    }
}
