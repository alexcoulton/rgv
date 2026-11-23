//! Per-frame drafts shared by frame preparation, the egui pass and the commit step.

use crate::app::actions::TrackControl;
use crate::app::alignment_context::{
    AlignmentContextAction, AlignmentContextMenuState, AlignmentContextMenuView,
    AlignmentContextTarget,
};
use crate::app::status_activity::StatusActivity;
use crate::ui::frame::WorkspaceScrollbarsDraft;
use crate::ui::{TrackFrameKey, TrackUiAction, UiCommand};
use crate::{
    MetadataTrackRow, RemoteSourceEditor, RemoteSourceRequest, VariantPredicateOperator, ui,
};
use rgv_core::{
    AlignmentReadSampling, ContigId, CopyNumberSortLocus, CopyNumberTrackSettings,
    CoverageScaleTransform, DetailLevel, GeneticCode, GenomicInterval, GwasTrackSettings,
    IgvImportReport, InspectorActivation, InteractionTrackSettings, JunctionDisplayMode,
    ModifiedBaseColorMode, MutationTrackSettings, OverlappingMatePolicy, PanelId,
    QuantitativeTrackSettings, ReadBaseDisplayMode, ReadStackingMode, ReferenceTranslationFrame,
    RegionOfInterest, ReviewItem, ReviewStatus, SampleMetadataTable, SashimiCountMode,
    SashimiScaleMode, SessionFilters, TrackId, TrackSource, UnresolvedSessionSource,
    VariantFieldScope, VariantMissingValueRule, VariantTrackSettings, Viewport,
};
use rgv_engine::{TrackSearchHit, TrackSearchResults};
use rgv_hts::ModificationCode;
use rgv_layout::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

/// `(panel, contig name, contig length, viewport, genome overview)` for one locus panel.
pub(super) type PanelContext = (PanelId, String, u64, Viewport, ui::GenomeOverviewData);

/// Read-only view of the active panel, status line and inspector state for one frame.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct FrameSnapshot {
    pub(super) interval: GenomicInterval,
    pub(super) bases_per_pixel: f64,
    pub(super) detail: DetailLevel,
    pub(super) contig_name: String,
    pub(super) data_status: String,
    /// Whether `data_status` describes background work; drives the toolbar spinner.
    pub(super) status_activity: StatusActivity,
    pub(super) announce_status: bool,
    pub(super) active_panel_id: PanelId,
    pub(super) diagnostics: ui::diagnostics::DiagnosticsSnapshot,
    pub(super) manual_trace: ui::ManualInteractionTraceSnapshot,
    pub(super) inspectors: ui::inspectors::InspectorFrame,
    pub(super) status_locus: String,
    pub(super) selection_summary: Option<String>,
}

/// Requests raised by the Properties panel inspectors.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct InspectorDraft {
    pub(super) clear_selection: bool,
    pub(super) selected_to_top_requested: bool,
    pub(super) chain_navigation_requested: Option<usize>,
    pub(super) mate_panel_requested: bool,
    pub(super) chain_panel_requested: Option<usize>,
    pub(super) chain_all_panels_requested: bool,
    pub(super) variant_partner_requested: Option<(String, u64)>,
    pub(super) variant_endpoint_requested: Option<(String, u64)>,
    pub(super) interaction_endpoint_requested: Option<(String, u64)>,
    pub(super) variant_pileup_requested: Option<(TrackId, u64)>,
    pub(super) variant_modification_requested: Option<(TrackId, u64)>,
    pub(super) variant_genotypes_export_requested: bool,
    pub(super) variant_evidence_export_requested: bool,
    pub(super) junction_bed_export_requested: bool,
    pub(super) sashimi_png_export_requested: bool,
    pub(super) sashimi_svg_export_requested: bool,
    pub(super) show_all_transcripts_requested: Option<String>,
    pub(super) return_to_representative_genes_requested: Option<String>,
    pub(super) variant_sample_search: String,
}

/// Editable copy of the alignment (and reference) track settings shown in "Track settings".
#[allow(clippy::struct_excessive_bools)]
pub(super) struct AlignmentSettingsFrame {
    pub(super) primary_alignment_id: TrackId,
    pub(super) editing_alignment_id: Option<TrackId>,
    pub(super) editing_auxiliary_id: Option<TrackId>,
    pub(super) edited_track_sample_key: Option<String>,
    pub(super) settings_alignment_id: TrackId,
    pub(super) available_modification_codes: Vec<ModificationCode>,
    pub(super) settings_filters: SessionFilters,
    pub(super) minimum_mapq: u8,
    pub(super) minimum_base_quality: u8,
    pub(super) minimum_allele_fraction: f32,
    pub(super) quality_weighted_alleles: bool,
    pub(super) show_reference: bool,
    pub(super) reference_reverse_complement: bool,
    pub(super) reference_translation_frames: Vec<ReferenceTranslationFrame>,
    pub(super) reference_genetic_code: GeneticCode,
    pub(super) show_coverage: bool,
    pub(super) show_junctions: bool,
    pub(super) minimum_junction_depth: u32,
    pub(super) minimum_junction_flank: u32,
    pub(super) junction_display_mode: JunctionDisplayMode,
    pub(super) sashimi_scale_mode: SashimiScaleMode,
    pub(super) sashimi_fixed_scale_maximum: u32,
    pub(super) sashimi_count_mode: SashimiCountMode,
    pub(super) show_junction_labels: bool,
    pub(super) initial_show_reads: bool,
    pub(super) show_reads: bool,
    pub(super) use_fixed_coverage_scale: bool,
    pub(super) coverage_scale_maximum: u32,
    pub(super) coverage_scale_transform: CoverageScaleTransform,
    pub(super) show_coverage_center_line: bool,
    pub(super) alignment_density: f32,
    pub(super) inspector_activation: InspectorActivation,
    pub(super) read_base_display_mode: ReadBaseDisplayMode,
    pub(super) shade_read_bases_by_quality: bool,
    pub(super) minimum_indel_size: u32,
    pub(super) minimum_event_label_size: u32,
    pub(super) show_modified_bases: bool,
    pub(super) show_modification_frequency: bool,
    pub(super) minimum_modification_probability: u8,
    pub(super) modified_base_color_mode: ModifiedBaseColorMode,
    /// The 2-colour display: unmodified calls drawn too.
    pub(super) show_unmodified_calls: bool,
    /// The code the Modifications lane plots; `None` is automatic (5mC).
    pub(super) modification_lane_code: Option<ModificationCode>,
    pub(super) hidden_modification_codes: Vec<ModificationCode>,
    pub(super) include_secondary: bool,
    pub(super) include_supplementary: bool,
    pub(super) filter_expression: String,
    pub(super) filter_error: Option<String>,
    pub(super) apply_filter: bool,
    pub(super) typed_alignment_predicates_changed: bool,
    pub(super) alignment_highlight_predicates_changed: bool,
    pub(super) bulk_alignment_ids: Vec<TrackId>,
    pub(super) bulk_alignment_pane: Option<ui::TrackPane>,
    pub(super) apply_bulk_alignment_appearance: bool,
    pub(super) bulk_alignment_visibility: Option<bool>,
    pub(super) bulk_alignment_collapsed: Option<bool>,
    pub(super) complete_read_limit: u32,
    pub(super) color_mode: AlignmentColorMode,
    pub(super) group_mode: AlignmentGroupMode,
    pub(super) group_order: AlignmentOrderDirection,
    pub(super) sort_mode: AlignmentSortMode,
    pub(super) sort_order: AlignmentOrderDirection,
    pub(super) read_stacking_mode: ReadStackingMode,
    pub(super) read_sampling: AlignmentReadSampling,
    pub(super) compact_repack_requested: bool,
    pub(super) view_as_pairs: bool,
    pub(super) pair_summary: rgv_hts::PairSummary,
    pub(super) override_pair_bounds: bool,
    pub(super) minimum_template_length: u64,
    pub(super) maximum_template_length: u64,
    pub(super) overlapping_mate_policy: OverlappingMatePolicy,
    pub(super) style_tag_editor: String,
    pub(super) read_name_editor: String,
    pub(super) select_read_name_requested: bool,
    pub(super) alignment_visible: bool,
    pub(super) alignment_collapsed: bool,
    pub(super) alignment_name: String,
    pub(super) alignment_height: f32,
    pub(super) reference_configured: bool,
}

/// Per-track controls, auxiliary track settings and queued track actions.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct TrackControlsDraft {
    pub(super) builtin_track_identities: BTreeMap<TrackId, rgv_core::BuiltInTrackIdentity>,
    pub(super) variant_settings_sample_search: String,
    pub(super) variant_predicate_scope: VariantFieldScope,
    pub(super) variant_predicate_key: String,
    pub(super) variant_predicate_operator: VariantPredicateOperator,
    pub(super) variant_predicate_value: String,
    pub(super) variant_predicate_missing: VariantMissingValueRule,
    pub(super) variant_predicate_error: Option<String>,
    pub(super) alignment_track_controls: Vec<TrackControl>,
    pub(super) alignment_subtrack_controls: Vec<(TrackId, SessionFilters)>,
    pub(super) track_controls: Vec<TrackControl>,
    pub(super) feature_settings_controls: Vec<(TrackId, rgv_core::FeatureTrackSettings)>,
    /// Feature tracks of gene models, which offer the transcript settings.
    pub(super) gene_model_tracks: BTreeSet<TrackId>,
    pub(super) natural_row_height_tracks: BTreeSet<TrackId>,
    pub(super) variant_settings_controls: Vec<(TrackId, VariantTrackSettings)>,
    pub(super) gwas_settings_controls: Vec<(TrackId, GwasTrackSettings)>,
    pub(super) quantitative_settings_controls: Vec<(TrackId, QuantitativeTrackSettings)>,
    pub(super) copy_number_settings_controls: Vec<(TrackId, CopyNumberTrackSettings)>,
    pub(super) mutation_settings_controls: Vec<(TrackId, MutationTrackSettings)>,
    pub(super) copy_number_sort_locus: CopyNumberSortLocus,
    pub(super) quantitative_track_choices: Vec<(TrackId, String)>,
    pub(super) interaction_settings_controls: Vec<(TrackId, InteractionTrackSettings)>,
    pub(super) variant_settings_queries: Vec<(TrackId, Arc<rgv_hts::VariantQuery>)>,
    pub(super) track_counts: Vec<(TrackId, usize)>,
    pub(super) track_move: Option<(TrackId, usize)>,
    pub(super) track_ui_actions: Vec<TrackUiAction>,
    pub(super) rendered_track_settings_panel: Option<TrackFrameKey>,
    pub(super) track_settings_panel: Option<TrackFrameKey>,
}

/// Menu, palette and system-dialog commands plus the application settings being edited.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct CommandDraft {
    pub(super) open_alignment_requested: bool,
    pub(super) open_reference_requested: bool,
    pub(super) open_genome_config_requested: bool,
    pub(super) hosted_genome_filter: String,
    pub(super) hosted_genome_requested: Option<String>,
    pub(super) refresh_hosted_genomes_requested: bool,
    pub(super) restore_default_genes_requested: bool,
    pub(super) open_landmark_folder_requested: bool,
    pub(super) choose_landmark_folder_requested: bool,
    pub(super) add_track_requested: bool,
    pub(super) open_session_requested: bool,
    pub(super) save_session_requested: bool,
    pub(super) save_session_as_requested: bool,
    pub(super) export_png_requested: bool,
    pub(super) clear_cache_requested: bool,
    pub(super) settings_requested: bool,
    pub(super) focus_locus_requested: bool,
    pub(super) command_palette_open: bool,
    pub(super) exit_confirmation_open: bool,
    pub(super) user_exit_requested: bool,
    pub(super) command_search: String,
    pub(super) palette_command: Option<UiCommand>,
    pub(super) undo_requested: bool,
    pub(super) redo_requested: bool,
    /// What Undo / Redo would act on; `None` disables the menu item.
    pub(super) undo_label: Option<&'static str>,
    pub(super) redo_label: Option<&'static str>,
    pub(super) remote_source_open: bool,
    pub(super) remote_source_editor: RemoteSourceEditor,
    pub(super) remote_source_request: Option<RemoteSourceRequest>,
    pub(super) settings_open: bool,
    pub(super) project_issues_open: bool,
    pub(super) unresolved_session_sources: Vec<UnresolvedSessionSource>,
    pub(super) session_migration_notices: Vec<String>,
    pub(super) igv_report_open: bool,
    pub(super) igv_import_report: Option<IgvImportReport>,
    pub(super) recovery_open: bool,
    pub(super) recovery_requested: Option<PathBuf>,
    pub(super) dismiss_recovery: bool,
    pub(super) cache_budget_mib: usize,
    pub(super) read_browsing_buffer_percent: u16,
    pub(super) diagnostics_opt_in: bool,
    pub(super) autosave_enabled: bool,
    pub(super) autosave_interval_seconds: u64,
    pub(super) autosave_retention: usize,
    pub(super) interface_scale_percent: u16,
    pub(super) high_contrast: bool,
    pub(super) interface_theme: rgv_core::InterfaceTheme,
    pub(super) reduced_motion: bool,
    pub(super) coverage_cache_enabled: bool,
    pub(super) track_pipeline: rgv_core::TrackPipeline,
    pub(super) recent_loci: Vec<String>,
    pub(super) current_session_path: Option<PathBuf>,
    pub(super) clear_recent_requested: bool,
    pub(super) recent_alignment_requested: Option<TrackSource>,
    pub(super) recent_session_requested: Option<String>,
    pub(super) recent_locus_requested: Option<String>,
    pub(super) diagnostics_open: bool,
    pub(super) manual_trace_action: Option<ui::ManualInteractionTraceAction>,
    pub(super) command_key: &'static str,
    /// Unsaved-changes prompt and notification log.
    pub(super) shell: super::shell_dialogs::ShellUiDraft,
}

/// Workspace geometry, track chrome inputs and scrollbar drafts.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct LayoutFrame {
    pub(super) committed_canvas_geometry: ui::CanvasGeometry,
    pub(super) new_canvas_geometry: ui::CanvasGeometry,
    pub(super) previous_workspace_layout: ui::WorkspaceLayoutSnapshot,
    pub(super) new_workspace_layout: ui::WorkspaceLayoutSnapshot,
    pub(super) native_properties_resize_active: bool,
    pub(super) pane_layout: ui::WorkspacePaneLayout,
    pub(super) track_frames: Vec<ui::TrackFrame>,
    pub(super) annotation_track_count: usize,
    pub(super) read_track_count: usize,
    pub(super) shared_annotation_offset: f32,
    pub(super) selected_annotation_track_ids: Vec<TrackId>,
    pub(super) track_groups: Vec<ui::TrackGroupChrome>,
    pub(super) bulk_alignment_group: Option<ui::TrackGroupChrome>,
    pub(super) workspace_scrollbars: Vec<WorkspaceScrollbarsDraft>,
    pub(super) pane_annotation_top: f32,
    pub(super) pane_annotation_bottom: f32,
    pub(super) selected_track: Option<TrackFrameKey>,
    pub(super) overview_interaction: ui::OverviewInteraction,
    pub(super) overview_panel_id: PanelId,
    pub(super) genomic_canvas_layer_id: Option<egui::LayerId>,
    pub(super) pane_divider_rect: Option<egui::Rect>,
}

/// Locus bar, assembly/contig choices, panel selector and navigation requests.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct NavigationDraft {
    pub(super) locus_editor: String,
    pub(super) navigate_requested: bool,
    pub(super) assembly_label: String,
    pub(super) requested_contig: Option<ContigId>,
    pub(super) previous_feature_requested: bool,
    pub(super) next_feature_requested: bool,
    pub(super) annotation_search_results: TrackSearchResults,
    pub(super) locus_search_changed: bool,
    /// The note shown beside the locus box about the name it last searched for.
    pub(super) locus_search_note: Option<crate::workspace::navigation_controller::LocusSearchNote>,
    pub(super) search_popup_position: Option<egui::Pos2>,
    pub(super) annotation_hit_requested: Option<TrackSearchHit>,
    pub(super) navigate_back_requested: bool,
    pub(super) navigate_forward_requested: bool,
    pub(super) navigate_home_requested: bool,
    pub(super) zoom_in_requested: bool,
    pub(super) zoom_out_requested: bool,
    pub(super) requested_span: Option<u64>,
    pub(super) panel_controls: Vec<(usize, PanelId, String)>,
    pub(super) panel_to_activate: Option<PanelId>,
    pub(super) panel_to_close: Option<PanelId>,
    pub(super) panel_to_move: Option<(PanelId, usize)>,
}

/// Regions of interest, review queue and sample metadata dialogs.
#[allow(clippy::struct_excessive_bools)]
pub(super) struct WorkspaceListsDraft {
    /// Whether `regions` and `review_items` hold the workspace lists (copied only while a
    /// window that edits them is open).
    pub(super) lists_loaded: bool,
    pub(super) metadata_open: bool,
    pub(super) regions_open: bool,
    pub(super) review_open: bool,
    pub(super) regions: Vec<RegionOfInterest>,
    pub(super) review_items: Vec<ReviewItem>,
    pub(super) review_status_filter: Option<ReviewStatus>,
    pub(super) add_current_region: bool,
    pub(super) add_current_review: bool,
    pub(super) add_selected_feature_to_review: bool,
    pub(super) region_import_requested: bool,
    pub(super) region_export_requested: bool,
    pub(super) review_import_requested: bool,
    pub(super) review_export_requested: bool,
    pub(super) previous_review_requested: bool,
    pub(super) next_review_requested: bool,
    pub(super) managed_locus_requested: Option<String>,
    pub(super) region_to_delete: Option<u64>,
    pub(super) review_to_delete: Option<u64>,
    pub(super) region_to_relocate: Option<u64>,
    pub(super) review_to_relocate: Option<u64>,
    pub(super) sample_metadata: Option<SampleMetadataTable>,
    pub(super) metadata_filter_column: usize,
    pub(super) metadata_filter_value: Option<String>,
    pub(super) metadata_selected_tracks: BTreeSet<TrackId>,
    pub(super) metadata_bulk_height: f32,
    pub(super) metadata_remove_armed: bool,
    pub(super) metadata_import_requested: bool,
    pub(super) metadata_remove_requested: bool,
    pub(super) metadata_select_requested: bool,
    pub(super) metadata_bulk_visibility: Option<bool>,
    pub(super) metadata_bulk_collapsed: Option<bool>,
    pub(super) metadata_bulk_height_requested: bool,
    pub(super) metadata_bulk_rename_requested: bool,
    pub(super) metadata_bulk_alignment_settings_requested: bool,
    pub(super) metadata_bulk_auxiliary_appearance_requested: bool,
    pub(super) metadata_bulk_quantitative_autoscale_requested: bool,
    pub(super) metadata_bulk_quantitative_overlay_requested: bool,
    pub(super) metadata_bulk_quantitative_separate_requested: bool,
    pub(super) metadata_bulk_remove_requested: bool,
    pub(super) metadata_track_rows: Vec<MetadataTrackRow>,
}

/// Everything one shell frame reads and edits between preparation and commit.
///
/// The egui pass borrows the application immutably and mutates only this value; the commit step
/// then applies it to the application.
pub(super) struct ShellFrame {
    pub(super) snapshot: FrameSnapshot,
    pub(super) inspect: InspectorDraft,
    pub(super) align: AlignmentSettingsFrame,
    pub(super) tracks: TrackControlsDraft,
    pub(super) commands: CommandDraft,
    pub(super) layout: LayoutFrame,
    pub(super) nav: NavigationDraft,
    pub(super) lists: WorkspaceListsDraft,
    pub(super) panel_contexts: Vec<PanelContext>,
    pub(super) properties_expanded_id: egui::Id,
    pub(super) properties_expanded: bool,
    pub(super) alignment_context_menu: Option<AlignmentContextMenuState>,
    pub(super) alignment_context_menu_view: Option<AlignmentContextMenuView>,
    pub(super) alignment_context_action: Option<(AlignmentContextTarget, AlignmentContextAction)>,
    pub(super) feature_context_menu:
        Option<crate::inspection::gene_transcripts::FeatureContextMenuState>,
    /// The gene whose transcripts the feature menu or the properties panel toggled.
    pub(super) gene_transcripts_action:
        Option<crate::inspection::gene_transcripts::GeneTranscriptsTarget>,
    /// A Sashimi filter chip was clicked: clear that panel's junction filters.
    pub(super) sashimi_clear_filters: Option<PanelId>,
    /// Retry was clicked on a read panel's failure banner: request that view again.
    pub(super) read_notice_retry: Option<(PanelId, TrackId)>,
}
