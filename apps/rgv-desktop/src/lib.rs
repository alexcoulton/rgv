#![allow(clippy::wildcard_imports)]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::Write as _,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::Arc,
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod app;
mod automation;
mod control_service;
mod display;
mod inspection;
mod interaction_trace;
mod launch;
mod runtime;
mod sources;
mod ui;
mod workspace;
mod workspace_lists;

use anyhow::{Context as _, Result};
use app::{actions::*, tasks::*};
use automation::*;
use clap::Parser;
use control_service::{ControlRequest, ControlService, ControlStatusMetadata, read_control_secret};
#[cfg(test)]
use display::reference_bases_label;
use display::{alignment::*, auxiliary::*, hit_test::*};
use egui_wgpu::{RendererOptions, ScreenDescriptor};
use inspection::{auxiliary::*, model::*, read::*, rna::*, variant::*};
use interaction_trace::{InteractionTraceRecorder, InteractionTraceStage};
use launch::*;
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AutomationAlignmentRelationship, AutomationAlignmentReveal,
    AutomationAlignmentSelector, AutomationAlignmentState, AutomationAlignmentTrackSnapshot,
    AutomationBatch, AutomationCommand, AutomationCommandResult, AutomationCommandStatus,
    AutomationEnvironmentTelemetry, AutomationErrorCode, AutomationEvidenceField,
    AutomationExportKind, AutomationFrameTelemetry, AutomationGenomeSnapshot,
    AutomationGpuTelemetry, AutomationHistoryNavigation, AutomationNavigationDestination,
    AutomationOptionScope, AutomationPanelSnapshot, AutomationRenderLaneSnapshot,
    AutomationRenderOmissionReason, AutomationReviewNavigation, AutomationRunReport,
    AutomationRunTelemetry, AutomationSelection, AutomationSelectionGranularity,
    AutomationSelectionKind, AutomationSelectionSnapshot, AutomationSetting,
    AutomationSnapshotFormat, AutomationSnapshotScope, AutomationTrackLifecycle,
    AutomationTrackSnapshot, AutomationUnsavedChangesPolicy, AutomationViewportOperation,
    AutomationWorkspaceSnapshot, BisulfiteContext, ContigAliases, ContigId, CopyNumberSortLocus,
    CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode, CoverageScaleTransform,
    DesktopSettings, DetailLevel, FeatureDisplayMode, GenerationClock, GeneticCode, GenomeAssembly,
    GenomeAssemblyIdentity, GenomeConfiguration, GenomicInterval, GenomicSourceFormat,
    GwasColorMode, GwasTrackSettings, IgvImportReport, InspectorActivation, InteractionDisplayMode,
    InteractionTrackSettings, JunctionDisplayMode, MAXIMUM_ALIGNMENT_TRACKS,
    MAXIMUM_AUTOMATION_EVIDENCE_FIELDS, MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
    MAXIMUM_AUTOMATION_LOCI, MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS, MAXIMUM_AUTOSAVE_INTERVAL_SECONDS,
    MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, MAXIMUM_CACHE_BUDGET_MIB,
    MAXIMUM_INTERFACE_SCALE_PERCENT, MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS,
    MAXIMUM_REVIEW_NOTE_BYTES, MAXIMUM_VIEW_PANELS, MINIMUM_AUTOSAVE_INTERVAL_SECONDS,
    MINIMUM_AUTOSAVE_RETENTION, MINIMUM_CACHE_BUDGET_MIB, MINIMUM_INTERFACE_SCALE_PERCENT,
    ModifiedBaseColorMode, MutationColorMode, MutationTrackSettings, NavigationHistory,
    OverlappingMatePolicy, PanelGeneration, PanelId, PanelTrackId, QuantitativeArithmetic,
    QuantitativeDisplayMode, QuantitativeMissingMode, QuantitativeScaleMode,
    QuantitativeScaleTransform, QuantitativeTrackSettings, QuantitativeWindowFunction,
    ReadBaseDisplayMode, ReferenceDictionary, ReferenceTranslationFrame, RegionOfInterest,
    ReviewItem, ReviewStatus, SampleMetadataTable, SashimiCountMode, SashimiScaleMode,
    SbxDisplayOptions, SbxSimplexTailMode, SessionAlignmentTrack, SessionDocument, SessionFilters,
    SessionLocus, SessionViewPanel, SmrtKineticsMode, TrackCollection, TrackConfig, TrackFormat,
    TrackId, TrackKind, TrackSource, UltimaDisplayOptions, UltimaQualityMode,
    UnresolvedSessionSource, VariantColorMode, VariantFieldComparison, VariantFieldPredicate,
    VariantFieldScope, VariantGenotypeFilter, VariantGenotypeGrouping, VariantMissingValueRule,
    VariantTrackSettings, VariantTypeGroup, Viewport, classify_genomic_source,
    parse_ucsc_cytobands, resolve_contig_alias,
};
use rgv_engine::{
    AlignmentEngine, AlignmentEngineEvent, CacheBudgets, CacheStats, EngineQueueError,
    LayoutTileKey, ReferenceEngine, ReferenceEngineEvent, SourceFingerprint, TrackEngine,
    TrackEngineEvent, TrackSearchHit, TrackSearchResults, TrackSnapshot, TrackSnapshotData,
    TrackSpec, TrackSpecKind, WeightedLruCache,
};
use rgv_hts::{
    AlignmentFilter, AlignmentOpenOptions, AlignmentQuery, AlignmentRecord, AuxPredicate, AuxValue,
    CigarOpKind, FastaOpenOptions, HttpRangeConfig, ModificationCode, QUALITY_WEIGHT_SCALE,
    QueryDetail, QueryOptions, QueryStats, ReferenceSlice, RemoteAuthentication, RemoteIoStats,
    SequenceData, SupplementarySegment, TemplateLengthBounds, UltimaEventUnavailable,
    UltimaIndelKind, UltimaQualitySource, UltimaTagOrientation,
    analyze_ultima_record_with_tag_orientation, bisulfite_strand_is_reverse, decode_modified_bases,
    decode_supplementary_chain, pair_evidence, smrt_kinetics_series, summarize_bisulfite_record,
    summarize_sbx_record,
};
use rgv_layout::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentLayout, AlignmentLayoutOptions,
    AlignmentLayoutStats, AlignmentOrderDirection, AlignmentRowLayout, AlignmentSortMode,
    COPY_NUMBER_HEADER_HEIGHT, DisplayList, JunctionAnnotationLookup, JunctionAnnotationStatus,
    MAX_HIDDEN_MODIFICATION_CODES, MINIMUM_BASE_GLYPH_PIXEL_WIDTH, MUTATION_HEADER_HEIGHT,
    QuantitativeRange, RectPrimitive, Rgba, TrackLayoutOptions, alignment_read_name_hash,
    assign_alignment_rows, compose_quantitative_records, copy_number_sample_order,
    copy_number_track_height, gwas_point_y, junction_count_value,
    layout_alignment_query_with_rows_and_annotations, layout_copy_number, layout_features,
    layout_gwas, layout_interactions, layout_mutations, layout_quantitative,
    layout_reference_slice_oriented, layout_reference_translation, layout_sashimi, layout_variants,
    modification_code_color, mutation_track_height, quantitative_data_range, record_object_id,
    ultima_quality_color,
};
use rgv_render::{PrepareStats, RectangleRenderer, export_png};
use runtime::*;
use sha2::{Digest as _, Sha256};
use sources::*;
use ui::{
    AlignmentSubtrack, CanvasGeometry, GENOME_CONTEXT_HEIGHT, MENU_HEIGHT, NAVIGATION_HEIGHT,
    SEARCHABLE_COMMANDS, STATUS_HEIGHT, TrackFrame, TrackFrameKey, TrackSubtrack, TrackUiAction,
    UiCommand,
};
use ui::{alignment_settings::*, gestures::*, track_settings::*};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorGrabMode, Theme, Window, WindowAttributes, WindowId},
};
use workspace::{alignments::*, lists::*, navigation::*, panels::*};
use workspace_lists::{
    WorkspaceListImport, WorkspaceListKind, import_workspace_list, region_bed_export,
    review_tsv_export,
};

const SYNTHETIC_CONTIG_LENGTH: u64 = 250_000_000;
const INITIAL_INTERVAL_START: u64 = 1_000_000;
const INITIAL_INTERVAL_END: u64 = 1_100_000;
const LOCUS_EDITOR_ID: &str = "rgv-locus-editor";
const COMMAND_SEARCH_ID: &str = "rgv-command-search";
// The vertical navigator is a logarithmic camera over read-row density. Keep a
// finite range so malformed input cannot create unbounded geometry, but allow
// enough expansion for close inspection rather than stopping at the old 21 px
// row height.
const MIN_VERTICAL_DENSITY: f32 = 0.35;
const MAX_VERTICAL_DENSITY: f32 = 9.0;
const MAX_ALIGNMENT_ROW_HEIGHT: f32 = 64.0;
const EVENT_SEQUENCE_PREVIEW_BASES: usize = 120;
const EVENT_SEQUENCE_COPY_BASES: usize = 1_000_000;
const SEQUENCE_COPY_BASES: usize = 1_000_000;
const AUXILIARY_VALUE_DISPLAY_BYTES: usize = 4_096;
const AUXILIARY_TAGS_COPY_BYTES: usize = 1_000_000;
const VARIANT_ALLELE_PREVIEW_BASES: usize = 4_096;
const VARIANT_DETAILS_COPY_BYTES: usize = 1_000_000;
const MAX_VARIANT_EXPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_VARIANT_EXPORT_FIELD_BYTES: usize = 64 * 1024;
const MAX_RNA_EXPORT_BYTES: usize = 16 * 1024 * 1024;
const MAX_SASHIMI_EXPORT_TRACKS: usize = 16;
const MAX_OVERLAY_GUTTER_MEMBERS: usize = 64;
const SYNTHETIC_ALIGNMENT_TRACK_ID: TrackId = TrackId::new(0);
const MAX_RENDERED_REGIONS_OF_INTEREST: usize = 10_000;
const MAX_METADATA_FILTER_VALUES: usize = 4_096;
const MAX_METADATA_VISIBLE_COLUMNS: usize = 12;
const MAX_AUTOMATION_EXPORT_BYTES: usize = 64 * 1024 * 1024;
const AUTOMATION_SOURCE_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_AUTOMATION_FULL_PANEL_HEIGHT: u32 = 16_384;
const MAX_AUTOMATION_FRAME_SAMPLES: usize = 4_096;

#[allow(clippy::too_many_lines)]
fn apply_accessibility_preferences(context: &egui::Context, settings: &DesktopSettings) {
    context.set_theme(egui::Theme::Light);
    context.set_zoom_factor(f32::from(settings.interface_scale_percent) / 100.0);
    let mut visuals = egui::Visuals::light();
    if settings.high_contrast {
        visuals.override_text_color = Some(egui::Color32::BLACK);
        visuals.faint_bg_color = egui::Color32::WHITE;
        visuals.extreme_bg_color = egui::Color32::WHITE;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.25, egui::Color32::BLACK);
        visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.5, egui::Color32::BLACK);
        visuals.widgets.hovered.bg_stroke = egui::Stroke::new(2.0, egui::Color32::BLACK);
        visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0, egui::Color32::BLACK);
        visuals.widgets.open.bg_stroke = egui::Stroke::new(2.0, egui::Color32::BLACK);
        visuals.selection.bg_fill = egui::Color32::from_rgb(0, 64, 160);
        visuals.selection.stroke = egui::Stroke::new(2.0, egui::Color32::WHITE);
    }
    context.set_visuals(visuals);
    context.style_mut_of(egui::Theme::Light, |style| {
        style.animation_time = if settings.reduced_motion { 0.0 } else { 0.2 };
    });
    if settings.reduced_motion {
        context.clear_animations();
    }
}

pub fn run() -> Result<()> {
    let process_started = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rgv=info".into()),
        )
        .init();

    let mut options = LaunchOptions::parse();
    options.load_session()?;
    let mut run_marker = RunMarker::start(
        options
            .settings_path
            .as_deref()
            .unwrap_or_else(|| Path::new("rgv-settings.json")),
    );
    options.recovered_unclean_exit = run_marker.previous_unclean_exit();
    if options.recovered_unclean_exit {
        options.recovery_candidates = discover_recovery_candidates(
            options
                .settings_path
                .as_deref()
                .unwrap_or_else(|| Path::new("rgv-settings.json")),
        );
    }
    let event_loop = match EventLoop::new().context("failed to create native event loop") {
        Ok(event_loop) => event_loop,
        Err(error) => {
            run_marker.finish();
            return Err(error);
        }
    };
    event_loop.set_control_flow(ControlFlow::Wait);
    let result = event_loop
        .run_app(&mut Application {
            viewer: None,
            options,
            process_started,
        })
        .context("RGV event loop failed");
    run_marker.finish();
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UiPanel {
    CommandPalette,
    RemoteSource,
    Settings,
    ProjectIssues,
    IgvImportReport,
    Metadata,
    Regions,
    Review,
    TrackSettings(TrackFrameKey),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum RemoteSourceKind {
    #[default]
    Alignment,
    Reference,
    IndexedTrack,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RemoteSourceEditor {
    kind: RemoteSourceKind,
    source_url: String,
    index_url: String,
    reference_url: String,
    reference_index_url: String,
    error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteSourceRequest {
    kind: RemoteSourceKind,
    source_url: String,
    index_url: Option<String>,
    reference_url: Option<String>,
    reference_index_url: Option<String>,
}

struct CachedModificationCodes {
    panel_id: PanelId,
    track_id: TrackId,
    query: Arc<AlignmentQuery>,
    codes: Vec<rgv_hts::ModificationCode>,
}

fn normalized_remote_url(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if !(value.starts_with("https://") || value.starts_with("http://")) {
        anyhow::bail!("{label} must start with https:// or http://");
    }
    if value.chars().any(char::is_whitespace) {
        anyhow::bail!("{label} cannot contain whitespace");
    }
    Ok(value.to_owned())
}

fn optional_remote_url(value: &str, label: &str) -> Result<Option<String>> {
    (!value.trim().is_empty())
        .then(|| normalized_remote_url(value, label))
        .transpose()
}

fn validate_remote_source_editor(editor: &RemoteSourceEditor) -> Result<RemoteSourceRequest> {
    let source_url = normalized_remote_url(&editor.source_url, "Source URL")?;
    let classified_source = classify_genomic_source(&source_url);
    let index_url = (editor.kind != RemoteSourceKind::IndexedTrack)
        .then(|| optional_remote_url(&editor.index_url, "Index URL"))
        .transpose()?
        .flatten();
    let (reference_url, reference_index_url) = if editor.kind == RemoteSourceKind::Alignment {
        (
            optional_remote_url(&editor.reference_url, "Reference FASTA URL")?,
            optional_remote_url(&editor.reference_index_url, "Reference FASTA index URL")?,
        )
    } else {
        (None, None)
    };
    match editor.kind {
        RemoteSourceKind::Alignment
            if !matches!(
                classified_source.map(|source| source.format),
                Some(GenomicSourceFormat::Bam | GenomicSourceFormat::Cram)
            ) =>
        {
            anyhow::bail!("an alignment URL must end in .bam or .cram")
        }
        RemoteSourceKind::Reference
            if classified_source.map(|source| source.format)
                != Some(GenomicSourceFormat::Fasta) =>
        {
            anyhow::bail!("a reference URL must end in .fa, .fasta, or .fna")
        }
        RemoteSourceKind::IndexedTrack => {
            anyhow::ensure!(
                classified_source.is_some_and(|source| source.format.supports_remote()),
                "this track format is not supported for remote sources"
            );
            let (specification, _) = inferred_desktop_track_kind(Path::new(&source_url))?;
            if !matches!(
                specification,
                TrackSpecKind::VcfOrBcf
                    | TrackSpecKind::BigWig
                    | TrackSpecKind::Tdf
                    | TrackSpecKind::BigBed
                    | TrackSpecKind::BigGenePred
                    | TrackSpecKind::BigNarrowPeak
            ) {
                anyhow::bail!(
                    "remote desktop tracks currently require indexed VCF/BCF, BigWig, TDF, BigBed, bigGenePred, or bigNarrowPeak data"
                );
            }
        }
        RemoteSourceKind::Alignment | RemoteSourceKind::Reference => {}
    }
    if reference_index_url.is_some() && reference_url.is_none() {
        anyhow::bail!("a reference FAI URL requires a reference FASTA URL")
    }
    Ok(RemoteSourceRequest {
        kind: editor.kind,
        source_url,
        index_url,
        reference_url,
        reference_index_url,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuantitativeBulkAction {
    Autoscale,
    Overlay,
    Separate,
}

type MetadataTrackRow = (TrackId, String, Option<String>, TrackKind);

#[derive(Clone, Copy)]
struct HorizontalOverviewDrag {
    anchor_fraction: f64,
    last_pointer_x: f64,
    last_pointer_y: f64,
}

#[derive(Clone, Copy)]
struct VerticalOverviewDrag {
    panel_id: PanelId,
    grab_offset: f32,
    press_x: f32,
    press_y: f32,
    last_pointer_x: f32,
    thumb_grab_fraction: f32,
    virtual_pointer_y: f32,
    virtual_scroll_fraction: f32,
    density_dragging: bool,
    pending_motion_x: f64,
    pending_motion_y: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverviewCapture {
    Horizontal,
    VerticalDensity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HoverInspectorTarget {
    Modification(TrackId, u64),
    Pileup(TrackId, u64),
    Junction(TrackId, u64, u64),
    Record {
        track_id: TrackId,
        index: usize,
        operation_index: Option<usize>,
    },
}

impl HoverInspectorTarget {
    const fn track_id(self) -> TrackId {
        match self {
            Self::Modification(track_id, _)
            | Self::Pileup(track_id, _)
            | Self::Junction(track_id, _, _)
            | Self::Record { track_id, .. } => track_id,
        }
    }
}

fn clear_removed_alignment_panel_state(
    panel: &mut ViewPanelController,
    removed: TrackId,
    removed_was_primary: bool,
) {
    panel
        .track_frames
        .retain(|frame| frame.key != TrackFrameKey::Alignment(removed));
    panel
        .selected_alignment_tracks
        .retain(|candidate| *candidate != removed);
    let removed_owned_evidence = panel.selected_alignment_track == Some(removed)
        || (removed_was_primary
            && panel.selected_alignment_track.is_none()
            && panel.selected_record.is_some());
    if removed_owned_evidence {
        panel.selected_record = None;
        panel.selected_alignment_track = None;
        panel.selected_read_event = None;
        panel.selected_pileup_position = None;
        panel.selected_modification_position = None;
        panel.selected_junction = None;
        panel.selected_chain = None;
    }
    if panel
        .pending_read_selection
        .as_ref()
        .is_some_and(|(track_id, _)| *track_id == removed)
    {
        panel.pending_read_selection = None;
    }
    panel.hovered_record = panel
        .hovered_record
        .filter(|(track_id, _)| *track_id != removed);
    panel.hover_inspector_target = panel
        .hover_inspector_target
        .filter(|target| target.track_id() != removed);
    if panel.selected_track == Some(TrackFrameKey::Alignment(removed)) {
        panel.selected_track = panel
            .selected_alignment_tracks
            .last()
            .copied()
            .map(TrackFrameKey::Alignment);
    }
}

fn clear_removed_auxiliary_panel_state(
    panel: &mut ViewPanelController,
    removed: &HashSet<TrackId>,
) {
    panel
        .track_snapshots
        .retain(|snapshot| !removed.contains(&snapshot.id));
    panel.track_frames.retain(
        |frame| !matches!(frame.key, TrackFrameKey::Auxiliary(id) if removed.contains(&id)),
    );
    panel.selected_feature = panel
        .selected_feature
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_variant = panel
        .selected_variant
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_gwas = panel
        .selected_gwas
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_quantitative = panel
        .selected_quantitative
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_copy_number = panel
        .selected_copy_number
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_mutation = panel
        .selected_mutation
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.selected_interaction = panel
        .selected_interaction
        .filter(|selection| !removed.contains(&selection.track_id));
    panel.hovered_record = panel
        .hovered_record
        .filter(|(track_id, _)| !removed.contains(track_id));
    panel.hover_inspector_target = panel
        .hover_inspector_target
        .filter(|target| !removed.contains(&target.track_id()));
    if matches!(
        panel.selected_track,
        Some(TrackFrameKey::Auxiliary(id)) if removed.contains(&id)
    ) {
        panel.selected_track = None;
    }
    panel.track_settled_generation = None;
}

fn clear_removed_quantitative_dependencies(
    tracks: &mut [TrackConfig],
    removed: &HashSet<TrackId>,
) -> usize {
    let mut cleared = 0;
    for track in tracks {
        if track
            .quantitative
            .operand_track
            .is_some_and(|id| removed.contains(&id))
        {
            track.quantitative.operand_track = None;
            track.quantitative.arithmetic = QuantitativeArithmetic::None;
            cleared += 1;
        }
    }
    cleared
}

fn auxiliary_query_track_ids(tracks: &[TrackConfig]) -> Vec<TrackId> {
    let quantitative_ids = tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Quantitative)
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    let mut required = tracks
        .iter()
        .filter(|track| track.visible)
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    for track in tracks.iter().filter(|track| {
        track.visible
            && track.kind == TrackKind::Quantitative
            && track.quantitative.arithmetic != QuantitativeArithmetic::None
    }) {
        if let Some(operand) = track.quantitative.operand_track
            && quantitative_ids.contains(&operand)
        {
            required.insert(operand);
        }
    }
    tracks
        .iter()
        .filter(|track| required.contains(&track.id))
        .map(|track| track.id)
        .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum VariantPredicateOperator {
    #[default]
    Exists,
    Equals,
    NotEquals,
    Contains,
    GreaterThan,
    GreaterOrEqual,
    LessThan,
    LessOrEqual,
}

#[derive(Clone, Copy)]
struct AutomationAlignmentHandleTarget {
    panel_id: PanelId,
    track_id: TrackId,
    query_generation: u64,
    record_index: usize,
}

#[allow(clippy::struct_excessive_bools)]
struct Viewer {
    application_instance_id: String,
    workspace_revision: u64,
    last_saved_workspace_revision: u64,
    next_evidence_revision: u64,
    automation_alignment_handles: HashMap<String, AutomationAlignmentHandleTarget>,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    rectangle_renderer: RectangleRenderer,
    egui_context: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    panels: PanelWorkspace,
    canvas_geometry: CanvasGeometry,
    genomic_canvas_layer_id: Option<egui::LayerId>,
    composite_display_list: DisplayList,
    shared_primary_alignment_height: Option<f32>,
    alignment_tracks: Vec<AlignmentTrackController>,
    empty_alignment: EmptyAlignmentState,
    alignment_retirement_tasks: Vec<JoinHandle<()>>,
    reference_engine: Option<ReferenceEngine>,
    reference_open_error: Option<String>,
    track_engine: Option<TrackEngine>,
    automation_auxiliary_open_state: HashMap<TrackId, AutomationAuxiliaryOpenState>,
    tracks: TrackCollection,
    reference_source: Option<TrackSource>,
    remote_reference_index_url: Option<String>,
    reference_remote_io_stats: Option<RemoteIoStats>,
    reference_dictionary: Option<ReferenceDictionary>,
    contig_aliases: ContigAliases,
    assembly: GenomeAssembly,
    assembly_identity: Option<GenomeAssemblyIdentity>,
    suggested_assembly_identity: Option<GenomeAssemblyIdentity>,
    assembly_name: String,
    cytoband_path: Option<PathBuf>,
    data_status: String,
    announced_status: String,
    layout_cache: WeightedLruCache<LayoutTileKey, Arc<AlignmentLayout>>,
    reference_reverse_complement: bool,
    reference_translation_frames: Vec<ReferenceTranslationFrame>,
    reference_genetic_code: GeneticCode,
    regions_of_interest: Vec<RegionOfInterest>,
    review_items: Vec<ReviewItem>,
    sample_metadata: Option<SampleMetadataTable>,
    metadata_filter_column: usize,
    metadata_filter_value: Option<String>,
    metadata_selected_tracks: BTreeSet<TrackId>,
    metadata_bulk_height: f32,
    metadata_remove_armed: bool,
    review_status_filter: Option<ReviewStatus>,
    style_tag_editor: String,
    read_name_editor: String,
    variant_sample_search: String,
    variant_settings_sample_search: String,
    variant_predicate_scope: VariantFieldScope,
    variant_predicate_key: String,
    variant_predicate_operator: VariantPredicateOperator,
    variant_predicate_value: String,
    variant_predicate_missing: VariantMissingValueRule,
    modification_code_cache: Option<CachedModificationCodes>,
    export_tasks: Vec<ExportTask>,
    pending_native_window_capture: Option<NativeWindowCaptureTask>,
    session_save_tasks: Vec<SessionSaveTask>,
    session_load_tasks: Vec<SessionLoadTask>,
    workspace_list_import_tasks: Vec<WorkspaceListImportTask>,
    /// Ephemeral selection ordering; never written to sessions or diagnostics.
    promoted_read: Option<(TrackId, u64)>,
    synthetic: bool,
    workspace_alignment_limit: usize,
    display_dirty: bool,
    display_upload_pending_metrics: bool,
    dragging: bool,
    drag_origin_interval: Option<GenomicInterval>,
    cursor: PhysicalPosition<f64>,
    last_cursor: Option<PhysicalPosition<f64>>,
    press_cursor: Option<PhysicalPosition<f64>>,
    smoothed_frame_ms: f64,
    next_egui_repaint: Option<Instant>,
    modifiers: ModifiersState,
    pending_command: Option<UiCommand>,
    open_panel: Option<UiPanel>,
    remote_source_editor: RemoteSourceEditor,
    diagnostics_open: bool,
    interaction_trace: InteractionTraceRecorder,
    command_search: String,
    annotation_search_generation: u64,
    annotation_navigation_generation: u64,
    annotation_search_results: TrackSearchResults,
    pending_annotation_navigation: Option<String>,
    settings: DesktopSettings,
    settings_path: PathBuf,
    current_session_path: Option<PathBuf>,
    unresolved_session_sources: Vec<UnresolvedSessionSource>,
    igv_import_report: Option<IgvImportReport>,
    next_autosave: Instant,
    recovery_candidates: Vec<PathBuf>,
    recovery_open: bool,
    automation: Option<AutomationRunner>,
    control_service: Option<ControlService>,
    automation_exit_requested: bool,
    cache_budgets: CacheBudgets,
    locus_editor: String,
    resizing_track: Option<(TrackId, f32)>,
    horizontal_overview_drag: Option<HorizontalOverviewDrag>,
    vertical_overview_drag: Option<VerticalOverviewDrag>,
    overview_capture: Option<OverviewCapture>,
    captured_motion: (f64, f64),
    captured_motion_distance: f64,
    suppress_horizontal_overview_click: bool,
}

impl Viewer {}

#[cfg(test)]
mod tests;
