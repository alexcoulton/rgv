#![allow(clippy::wildcard_imports)]

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::Write as _,
    num::{NonZeroU32, NonZeroU64, NonZeroUsize},
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
use display::{
    alignment_row_activity_overview, alignment_scene_read_extent,
    reconcile_read_panel_scroll_for_authority,
};
use egui_wgpu::{RendererOptions, ScreenDescriptor};
use inspection::{auxiliary::*, document::*, model::*, read::*, rna::*, variant::*};
use interaction_trace::{InteractionTraceRecorder, InteractionTraceStage};
use launch::*;
use rgv_alignment_scene::{
    AlignmentCamera, AlignmentCoherenceGroupId, AlignmentCoherenceGroupKind,
    AlignmentCoherenceGroupSpec, AlignmentComponent, AlignmentCoverageLaneOptions,
    AlignmentDerivedLaneCapture, AlignmentDerivedLaneKind, AlignmentDerivedLaneOptions,
    AlignmentHit, AlignmentInteractionState, AlignmentModificationLaneOptions, AlignmentPageKey,
    AlignmentPanelLaneCapture, AlignmentPanelLanePublication, AlignmentPanelSceneAdmission,
    AlignmentPanelSceneError, AlignmentPanelSceneFrame, AlignmentPanelSceneStore,
    AlignmentPresentationScene, AlignmentReadLabelMode, AlignmentRowActivityProfile,
    AlignmentSashimiLaneOptions, AlignmentSashimiScalePolicy, AlignmentSceneCamera,
    AlignmentSceneSnapshot, AlignmentStyleContract, AlleleSummaryDetail, AllocationBundleActual,
    AllocationBundleCommit, AllocationBundleError, AllocationBundleLease, AllocationBundleSlot,
    AllocationBundleSpec, AllocationLedger, AllocationRetirementClaimSet,
    AllocationRetirementToken, CapabilityRequirement, CoverageAlleleInterpretation,
    CpuAlignmentProjector, CpuAlleleProjectionOptions, CpuClipRect, CpuDepthProjectionOptions,
    CpuJunctionProjectionOptions, CpuModificationProjectionOptions, CpuProjectionError,
    CpuSummaryCamera, GeometryLod, LayoutContract, LedgerError, LedgerProgress,
    LedgerProgressEpoch, LedgerProgressSubscription, LedgerSnapshot,
    MAXIMUM_HIDDEN_MODIFICATION_CODES, PayloadContract, PresentationContract, ReferenceRevision,
    RowAtlasRevision, RowCoordinateSpaceRevision, SummaryContract,
};
use rgv_core::{
    AUTOMATION_SCHEMA_VERSION, AutomationAlignmentRelationship, AutomationAlignmentReveal,
    AutomationAlignmentSelector, AutomationAlignmentState, AutomationAlignmentTrackSnapshot,
    AutomationBatch, AutomationCommand, AutomationCommandResult, AutomationCommandStatus,
    AutomationCytobandAvailability, AutomationEnvironmentTelemetry, AutomationErrorCode,
    AutomationEvidenceField, AutomationExportKind, AutomationFrameTelemetry,
    AutomationGenomeSnapshot, AutomationGpuTelemetry, AutomationHistoryNavigation,
    AutomationLandmarkIntegrityState, AutomationLandmarkSnapshot, AutomationLandmarkTrackState,
    AutomationNavigationDestination, AutomationNavigationTelemetry, AutomationOptionScope,
    AutomationPaneItemLayoutSnapshot, AutomationPaneLayoutSnapshot, AutomationPanelSnapshot,
    AutomationPresentationCompositeSnapshot, AutomationPresentationGroupSnapshot,
    AutomationPresentationNodeSnapshot, AutomationReadPanelStateSnapshot,
    AutomationReadPresentationState, AutomationRectSnapshot, AutomationRenderLaneSnapshot,
    AutomationRenderOmissionReason, AutomationReviewNavigation, AutomationRunReport,
    AutomationRunTelemetry, AutomationSelection, AutomationSelectionGranularity,
    AutomationSelectionKind, AutomationSelectionSnapshot, AutomationSetting,
    AutomationSnapshotFormat, AutomationSnapshotScope, AutomationTrackGestureKind,
    AutomationTrackGestureSnapshot, AutomationTrackLifecycle, AutomationTrackSnapshot,
    AutomationUnsavedChangesPolicy, AutomationViewportOperation, AutomationWorkspaceLayoutSnapshot,
    AutomationWorkspacePanelLayoutSnapshot, AutomationWorkspaceSnapshot, BisulfiteContext,
    ContigAliases, ContigId, CopyNumberSortLocus, CopyNumberSortMode, CopyNumberTrackSettings,
    CopyNumberValueMode, CoverageScaleTransform, DesktopSettings, DetailLevel, FeatureDisplayMode,
    GenerationClock, GeneticCode, GenomeAssembly, GenomeAssemblyIdentity, GenomeConfiguration,
    GenomeLandmarkCatalogue, GenomicInterval, GenomicSourceFormat, GwasColorMode,
    GwasTrackSettings, IgvImportReport, InspectorActivation, InteractionDisplayMode,
    InteractionTrackSettings, JunctionDisplayMode, LandmarkAssetRole, LandmarkSearchIndex,
    MAXIMUM_ALIGNMENT_TRACKS, MAXIMUM_AUTOMATION_EVIDENCE_FIELDS,
    MAXIMUM_AUTOMATION_HISTORY_ENTRIES, MAXIMUM_AUTOMATION_LOCI,
    MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS, MAXIMUM_AUTOSAVE_INTERVAL_SECONDS,
    MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, MAXIMUM_CACHE_BUDGET_MIB,
    MAXIMUM_INTERFACE_SCALE_PERCENT, MAXIMUM_READ_BROWSING_BUFFER_PERCENT,
    MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS, MAXIMUM_REVIEW_NOTE_BYTES,
    MAXIMUM_VIEW_PANELS, MINIMUM_AUTOSAVE_INTERVAL_SECONDS, MINIMUM_AUTOSAVE_RETENTION,
    MINIMUM_CACHE_BUDGET_MIB, MINIMUM_INTERFACE_SCALE_PERCENT,
    MINIMUM_READ_BROWSING_BUFFER_PERCENT, ModifiedBaseColorMode, MutationColorMode,
    MutationTrackSettings, NavigationHistory, OverlappingMatePolicy, PanelGeneration, PanelId,
    PanelTrackId, PresentationNodeId, QuantitativeArithmetic, QuantitativeDisplayMode,
    QuantitativeMissingMode, QuantitativeScaleMode, QuantitativeScaleTransform,
    QuantitativeTrackSettings, QuantitativeWindowFunction, ReadBaseDisplayMode, ReadStackingMode,
    ReferenceDictionary, ReferenceTranslationFrame, RegionOfInterest, ReviewItem, ReviewStatus,
    SampleMetadataTable, SashimiCountMode, SashimiScaleMode, SbxDisplayOptions, SbxSimplexTailMode,
    SessionAlignmentTrack, SessionDocument, SessionFilters, SessionLocus, SessionPaneTrackState,
    SessionReadPanelState, SessionViewPanel, SmrtKineticsMode, TrackChangeSet, TrackCollection,
    TrackConfig, TrackFormat, TrackId, TrackIntent, TrackKind, TrackSource, UltimaDisplayOptions,
    UltimaQualityMode, UnresolvedSessionSource, VariantColorMode, VariantFieldComparison,
    VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter, VariantGenotypeGrouping,
    VariantMissingValueRule, VariantTrackSettings, VariantTypeGroup, Viewport, ViewportGeneration,
    WorkspacePresentation, classify_genomic_source, parse_ucsc_cytobands, resolve_contig_alias,
};
use rgv_engine::{
    AlignmentBufferClampReason, AlignmentCacheOutcome, AlignmentCoordinatorFrame,
    AlignmentIndividualReadCapability, AlignmentQueryPhase, AlignmentReferenceAuthority,
    AlignmentReferenceRequirement, AlignmentSceneBuildContract, AlignmentSceneEngine,
    AlignmentSceneEngineEvent, AlignmentSceneEngineOperation, AlignmentSceneFrameCause,
    AlignmentScenePreparationPolicy, AlignmentSceneRequestToken,
    AlignmentSceneResourcePressureDisposition, AlignmentSceneSourceMetadata,
    AlignmentVerticalResidency, AlignmentWorkingSetStats, CacheBudgets, CacheStats,
    EngineQueueError, LayoutTileKey, LocalAlignmentSceneEngineConfig, LocalBamCanonicalBackend,
    LocalBamSourceControl, MAX_REFERENCE_QUERY_BASES, ReferenceEngine, ReferenceEngineEvent,
    ReferenceQueryIdentity, SourceFingerprint, SourceIntentKind, SourceOwnerId, TrackEngine,
    TrackEngineEvent, TrackSearchHit, TrackSearchResults, TrackSnapshot, TrackSnapshotData,
    TrackSpec, TrackSpecKind, WeightedLruCache, alignment_row_activity_satisfies_contract,
};
use rgv_hts::{
    AlignedBaseClassification, AlignmentFilter, AlignmentOpenOptions, AlignmentQuery,
    AlignmentRecord, AuxPredicate, AuxValue, CigarOpKind, FastaOpenOptions, HttpRangeConfig,
    IndividualReadStatus, ModificationCode, QUALITY_WEIGHT_SCALE, QueryDetail, QueryOptions,
    QueryStats, ReferenceSlice, RemoteAuthentication, RemoteIoStats, SequenceData,
    SupplementarySegment, TemplateLengthBounds, UltimaEventUnavailable, UltimaIndelKind,
    UltimaQualitySource, UltimaTagOrientation, analyze_ultima_record_with_tag_orientation,
    bisulfite_strand_is_reverse, classify_aligned_base, decode_modified_bases,
    decode_supplementary_chain, pair_evidence, smrt_kinetics_series, summarize_bisulfite_record,
    summarize_sbx_record,
};
#[cfg(test)]
use rgv_layout::layout_features;
use rgv_layout::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentLayout, AlignmentLayoutOptions,
    AlignmentLayoutStats, AlignmentOrderDirection, AlignmentRowHistory, AlignmentRowLayout,
    AlignmentRowReconciliationStats, AlignmentRowScene, AlignmentSortMode,
    COPY_NUMBER_HEADER_HEIGHT, DisplayList, IndividualReadRefusal, IndividualReadRefusalScope,
    JunctionAnnotationLookup, JunctionAnnotationStatus, JunctionArcComponent,
    MAX_HIDDEN_MODIFICATION_CODES, MINIMUM_BASE_GLYPH_PIXEL_WIDTH, MUTATION_HEADER_HEIGHT,
    QuantitativeRange, RectPrimitive, Rgba, TrackLayoutOptions, alignment_read_name_hash,
    compose_quantitative_records, copy_number_sample_order, gwas_point_y,
    junction_arc_distance_to_point, junction_arc_stroke_width, junction_count_value,
    layout_alignment_query_with_rows, layout_alignment_query_with_rows_and_annotations,
    layout_copy_number, layout_features_with_label_widths, layout_gwas, layout_interactions,
    layout_mutations, layout_quantitative, layout_reference_slice_oriented,
    layout_reference_translation, layout_sashimi, layout_variants, modification_code_color,
    quantitative_data_range, reconcile_alignment_rows_with_horizon, record_object_id,
    ultima_quality_color,
};
use rgv_render::{
    AlignmentGpuCameraCaptureError, AlignmentGpuFrame, AlignmentGpuPageAdmissionError,
    AlignmentGpuPrefetchCursor, AlignmentGpuPrepareError, AlignmentGpuRenderer,
    AlignmentGpuReplacementError, AlignmentGpuResidencyLease, AlignmentGpuRetirement,
    AlignmentGpuSceneRequest, AlignmentGpuScissor, AlignmentSummaryFrameOverlays,
    AlignmentSummaryGpuCameraCaptureError, AlignmentSummaryGpuFrame,
    AlignmentSummaryGpuPageAdmissionError, AlignmentSummaryGpuPageKey,
    AlignmentSummaryGpuPrefetchCursor, AlignmentSummaryGpuPrepareError,
    AlignmentSummaryGpuRenderer, AlignmentSummaryGpuReplacementError,
    AlignmentSummaryGpuResidencyLease, AlignmentSummaryGpuRetirement,
    AlignmentSummaryGpuSceneRequest, GpuBudgetPressure, GpuReclaimGoal, GpuReclaimReport,
    NavigationPreviewInstance, NavigationPreviewRenderer, PrepareStats, RectangleRenderer,
    export_png, navigation_preview_source_uv,
};
use runtime::*;
use sha2::{Digest as _, Sha256};
use sources::*;
use ui::{
    AlignmentSubtrack, CytobandAvailability, GenomeContextMetrics, MENU_HEIGHT, NAVIGATION_HEIGHT,
    SEARCHABLE_COMMANDS, STATUS_HEIGHT, TrackFrame, TrackFrameKey, TrackSubtrack, TrackUiAction,
    UiCommand, WorkspaceLayoutInput, WorkspaceLayoutSnapshot, WorkspacePanelLayoutInput,
};
use ui::{alignment_settings::*, gestures::*, track_settings::*};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{DeviceEvent, ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorGrabMode, CursorIcon, Theme, Window, WindowAttributes, WindowId},
};
use workspace::{
    alignment_navigation::*, alignments::*, lists::*, navigation::*, navigation_preview::*,
    panels::*, tracks::replace_panel_track_selection,
};
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
    track_id: TrackId,
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
    Junction(TrackId, u64, u64, JunctionArcComponent),
    Record {
        track_id: TrackId,
        index: usize,
        operation_index: Option<usize>,
        operation_offset: Option<u32>,
    },
}

impl HoverInspectorTarget {
    const fn track_id(self) -> TrackId {
        match self {
            Self::Modification(track_id, _)
            | Self::Pileup(track_id, _)
            | Self::Junction(track_id, _, _, _)
            | Self::Record { track_id, .. } => track_id,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct JunctionArcKey {
    track_id: TrackId,
    start: u64,
    end: u64,
    component: JunctionArcComponent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ComposedJunctionArcTarget {
    key: JunctionArcKey,
    primitive: RectPrimitive,
    stroke_width: f32,
    draw_order: u32,
    interaction_token: u32,
    content_revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct JunctionArcHover {
    key: JunctionArcKey,
    interaction_token: u32,
    distance_points: f32,
    content_revision: u64,
}

const SASHIMI_HIT_DURATION_SAMPLE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SashimiHitTestGate {
    #[default]
    NotObserved,
    PointerConsumed,
    Dragging,
    OutsideCanvas,
    PresentationUnsettled,
    OutsidePanel,
    OutsideSashimiLane,
    Tested,
}

impl SashimiHitTestGate {
    const fn label(self) -> &'static str {
        match self {
            Self::NotObserved => "not observed",
            Self::PointerConsumed => "pointer consumed",
            Self::Dragging => "dragging",
            Self::OutsideCanvas => "outside canvas",
            Self::PresentationUnsettled => "presentation unsettled",
            Self::OutsidePanel => "outside active panel",
            Self::OutsideSashimiLane => "outside Sashimi lane",
            Self::Tested => "curve tested",
        }
    }
}

#[derive(Debug)]
struct SashimiInteractionDiagnostics {
    hit_tests: u64,
    bounds_candidates: u64,
    curve_evaluations: u64,
    acquisitions: u64,
    transfers: u64,
    clears: u64,
    hysteresis_retentions: u64,
    stale_revision_rejections: u64,
    hover_redraws: u64,
    hover_layout_rebuilds: u64,
    hit_duration_micros: [u64; SASHIMI_HIT_DURATION_SAMPLE_CAPACITY],
    hit_duration_count: usize,
    next_hit_duration: usize,
    latest_pointer: Option<egui::Pos2>,
    latest_phase: Option<NavigationPresentationPhase>,
    latest_result: JunctionArcHitTestResult,
    latest_hover_overlay_count: usize,
}

impl Default for SashimiInteractionDiagnostics {
    fn default() -> Self {
        Self {
            hit_tests: 0,
            bounds_candidates: 0,
            curve_evaluations: 0,
            acquisitions: 0,
            transfers: 0,
            clears: 0,
            hysteresis_retentions: 0,
            stale_revision_rejections: 0,
            hover_redraws: 0,
            hover_layout_rebuilds: 0,
            hit_duration_micros: [0; SASHIMI_HIT_DURATION_SAMPLE_CAPACITY],
            hit_duration_count: 0,
            next_hit_duration: 0,
            latest_pointer: None,
            latest_phase: None,
            latest_result: JunctionArcHitTestResult::default(),
            latest_hover_overlay_count: 0,
        }
    }
}

impl SashimiInteractionDiagnostics {
    fn record_latest_probe(
        &mut self,
        pointer: egui::Pos2,
        phase: NavigationPresentationPhase,
        result: JunctionArcHitTestResult,
    ) {
        self.latest_pointer = Some(pointer);
        self.latest_phase = Some(phase);
        self.latest_result = result;
        if result.hover.is_none() {
            self.latest_hover_overlay_count = 0;
        }
    }

    fn record_hit_test(
        &mut self,
        result: JunctionArcHitTestResult,
        previous: Option<JunctionArcHover>,
        current: Option<JunctionArcHover>,
        duration: Option<Duration>,
        hover_redraw: bool,
    ) {
        self.hit_tests = self.hit_tests.saturating_add(1);
        self.bounds_candidates = self
            .bounds_candidates
            .saturating_add(result.bounds_candidates as u64);
        self.curve_evaluations = self
            .curve_evaluations
            .saturating_add(result.curve_evaluations as u64);
        self.stale_revision_rejections = self
            .stale_revision_rejections
            .saturating_add(result.stale_revision_rejections as u64);
        self.hysteresis_retentions = self
            .hysteresis_retentions
            .saturating_add(u64::from(result.hysteresis_retained));
        match (previous, current) {
            (None, Some(_)) => self.acquisitions = self.acquisitions.saturating_add(1),
            (Some(previous), Some(current)) if previous.key != current.key => {
                self.transfers = self.transfers.saturating_add(1);
            }
            (Some(_), None) => self.clears = self.clears.saturating_add(1),
            (None, None) | (Some(_), Some(_)) => {}
        }
        self.hover_redraws = self.hover_redraws.saturating_add(u64::from(hover_redraw));
        if let Some(duration) = duration {
            self.hit_duration_micros[self.next_hit_duration] =
                u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
            self.next_hit_duration =
                (self.next_hit_duration + 1) % SASHIMI_HIT_DURATION_SAMPLE_CAPACITY;
            self.hit_duration_count = self
                .hit_duration_count
                .saturating_add(1)
                .min(SASHIMI_HIT_DURATION_SAMPLE_CAPACITY);
        }
    }

    fn duration_summary_micros(&self) -> (u64, u64, u64) {
        if self.hit_duration_count == 0 {
            return (0, 0, 0);
        }
        let mut samples = self.hit_duration_micros;
        let samples = &mut samples[..self.hit_duration_count];
        samples.sort_unstable();
        let median = samples[(samples.len() - 1) / 2];
        let p95 = samples[((samples.len() - 1) * 95) / 100];
        let maximum = samples[samples.len() - 1];
        (median, p95, maximum)
    }
}

fn clear_removed_alignment_panel_state(
    panel: &mut ViewPanelController,
    removed: TrackId,
    removed_was_primary: bool,
) {
    panel.selected_presentation_nodes.retain(
        |node| !matches!(node, PresentationNodeId::Track { track_id, .. } if *track_id == removed),
    );
    panel.presentation_selection_anchor = panel.presentation_selection_anchor.filter(
        |node| !matches!(node, PresentationNodeId::Track { track_id, .. } if *track_id == removed),
    );
    panel.read_panel_scrolls.remove(&removed);
    if let Some(cache) = panel.scroll_display_cache.as_mut() {
        cache
            .read_panels
            .retain(|read_panel| read_panel.track_id != removed);
    }
    if removed_was_primary {
        panel.vertical_scroll_points = 0.0;
        panel.vertical_content_origin_points = 0.0;
        panel.latent_vertical_scroll_fraction = None;
    }
    panel
        .track_frames
        .retain(|frame| frame.key != TrackFrameKey::Alignment(removed));
    panel
        .selected_alignment_tracks
        .retain(|candidate| *candidate != removed);
    panel
        .selected_read_tracks
        .retain(|candidate| *candidate != removed);
    if panel.read_selection_anchor == Some(removed) {
        panel.read_selection_anchor = panel.selected_read_tracks.last().copied();
    }
    if panel.alignment_selection_anchor == Some(removed) {
        panel.alignment_selection_anchor = panel.selected_alignment_tracks.last().copied();
    }
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
        panel.selected_junction_component = None;
        panel.selected_chain = None;
    }
    if panel
        .pending_read_selection
        .as_ref()
        .is_some_and(|pending| pending.track_id == removed)
    {
        panel.pending_read_selection = None;
    }
    if panel
        .pending_read_reveal
        .is_some_and(|(track_id, _)| track_id == removed)
    {
        panel.pending_read_reveal = None;
    }
    panel.hovered_record = panel
        .hovered_record
        .filter(|(track_id, _)| *track_id != removed);
    panel.hovered_alignment_hit = panel
        .hovered_alignment_hit
        .filter(|hit| hit.track != removed);
    panel.selected_alignment_hit = panel
        .selected_alignment_hit
        .filter(|hit| hit.track != removed);
    panel.hover_inspector_target = panel
        .hover_inspector_target
        .filter(|target| target.track_id() != removed);
    panel.hovered_junction_arc = panel
        .hovered_junction_arc
        .filter(|hover| hover.key.track_id != removed);
    panel
        .junction_arc_targets
        .retain(|target| target.key.track_id != removed);
    if let Some(cache) = panel.scroll_display_cache.as_mut() {
        cache
            .junction_arc_keys
            .retain(|_, key| key.track_id != removed);
    }
    if panel.selected_track == Some(TrackFrameKey::Alignment(removed)) {
        panel.selected_track = panel
            .selected_alignment_tracks
            .last()
            .copied()
            .or_else(|| panel.selected_read_tracks.last().copied())
            .map(TrackFrameKey::Alignment);
    }
}

fn clear_removed_auxiliary_panel_state(
    panel: &mut ViewPanelController,
    removed: &HashSet<TrackId>,
) {
    panel.selected_presentation_nodes.retain(|node| {
        !matches!(node, PresentationNodeId::Track { track_id, .. } if removed.contains(track_id))
    });
    panel.presentation_selection_anchor = panel.presentation_selection_anchor.filter(|node| {
        !matches!(node, PresentationNodeId::Track { track_id, .. } if removed.contains(track_id))
    });
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
    // Query demand is source lifecycle state, not presentation state. Keeping
    // loaded auxiliary sources warm makes either pane eye instant to restore
    // and prevents a hidden arithmetic operand from being dropped.
    tracks.iter().map(|track| track.id).collect()
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

/// Exact camera-and-layout destination for one native alignment publication. This deliberately
/// excludes scene content, styling, hover, and selection: those can require a newer publication
/// attempt without making pixels from the last successful attempt geometrically untruthful.
#[derive(Clone, Debug, PartialEq)]
struct AlignmentSceneTargetIdentity {
    workspace_layout_revision: u64,
    surface_size: [u32; 2],
    scale_factor: f64,
    panels: Vec<AlignmentPanelTargetIdentity>,
}

#[derive(Clone, Debug, PartialEq)]
struct AlignmentPanelTargetIdentity {
    panel_id: PanelId,
    viewport: Viewport,
    lanes: Vec<AlignmentLaneTargetIdentity>,
}

#[derive(Clone, Debug, PartialEq)]
struct AlignmentLaneTargetIdentity {
    track_id: TrackId,
    preparation_camera: AlignmentSceneCamera,
    read_camera: Option<AlignmentCamera>,
    derived_lanes: Vec<AlignmentDerivedLaneTargetIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AlignmentDerivedLaneTargetIdentity {
    kind: AlignmentDerivedLaneKind,
    clip: CpuClipRect,
}

impl AlignmentSceneTargetIdentity {
    fn from_panel_frames(
        workspace_layout_revision: u64,
        surface_size: [u32; 2],
        scale_factor: f64,
        frames: &HashMap<PanelId, AlignmentPanelSceneFrame>,
    ) -> Self {
        let mut panels = frames
            .values()
            .map(|frame| {
                let mut lanes = frame
                    .lanes
                    .iter()
                    .map(|lane| AlignmentLaneTargetIdentity {
                        track_id: lane.track_id,
                        preparation_camera: lane.preparation_camera,
                        read_camera: lane.read_camera,
                        derived_lanes: lane
                            .derived_lanes
                            .iter()
                            .map(|derived| AlignmentDerivedLaneTargetIdentity {
                                kind: derived.kind(),
                                clip: derived.clip,
                            })
                            .collect(),
                    })
                    .collect::<Vec<_>>();
                lanes.sort_by_key(|lane| lane.track_id);
                AlignmentPanelTargetIdentity {
                    panel_id: frame.panel_id,
                    viewport: frame.viewport,
                    lanes,
                }
            })
            .collect::<Vec<_>>();
        panels.sort_by_key(|panel| panel.panel_id);
        Self {
            workspace_layout_revision,
            surface_size,
            scale_factor,
            panels,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AlignmentScenePublicationAttempt {
    target: AlignmentSceneTargetIdentity,
    target_revision: u64,
    sequence: u64,
    target_changed: bool,
}

/// Separates live camera/layout identity from attempts to publish content for that target. A
/// recoverable failure therefore cannot invalidate a still-exact prior frame merely because a
/// hover, selection, or same-target scene refresh tried to replace it.
#[derive(Clone, Debug, Default, PartialEq)]
struct AlignmentScenePublicationAuthority {
    /// Last target whose semantic/native epoch committed successfully. It continues to own
    /// rollback resources even while a newer desired target is being prepared.
    target: Option<AlignmentSceneTargetIdentity>,
    target_revision: u64,
    target_revision_clock: u64,
    /// Current live camera/layout target. Render, hit, live-region, and scroll exactness gate on
    /// this identity immediately; a failed changed-target attempt cannot present old pixels as
    /// though they described the new destination.
    desired_target: Option<AlignmentSceneTargetIdentity>,
    desired_target_revision: u64,
    attempt_sequence: u64,
}

impl AlignmentScenePublicationAuthority {
    fn begin_attempt(
        &mut self,
        target: &AlignmentSceneTargetIdentity,
    ) -> AlignmentScenePublicationAttempt {
        self.attempt_sequence = self
            .attempt_sequence
            .checked_add(1)
            .expect("alignment scene publication attempt space was exhausted");
        let target_changed = self.target.as_ref() != Some(target);
        let target_revision = if !target_changed {
            self.target_revision
        } else if self.desired_target.as_ref() == Some(target) {
            self.desired_target_revision
        } else {
            self.target_revision_clock
                .checked_add(1)
                .expect("alignment scene target revision space was exhausted")
        };
        self.target_revision_clock = self.target_revision_clock.max(target_revision);
        self.desired_target = Some(target.clone());
        self.desired_target_revision = target_revision;
        AlignmentScenePublicationAttempt {
            target: target.clone(),
            target_revision,
            sequence: self.attempt_sequence,
            target_changed,
        }
    }

    /// Transfers committed ownership only after semantic and native publication succeeded.
    /// Same-target failure therefore retains exact pixels/hits; changed-target failure leaves the
    /// old resources privately owned for rollback while desired-target gating keeps them off the
    /// live render and interaction paths.
    fn commit_attempt(&mut self, attempt: &AlignmentScenePublicationAttempt) -> bool {
        if attempt.sequence != self.attempt_sequence {
            return false;
        }
        if self.desired_target.as_ref() != Some(&attempt.target)
            || self.desired_target_revision != attempt.target_revision
        {
            return false;
        }
        self.target = Some(attempt.target.clone());
        self.target_revision = attempt.target_revision;
        true
    }

    const fn desired_target_revision(&self) -> u64 {
        self.desired_target_revision
    }

    fn desired_target_matches(&self, target: &AlignmentSceneTargetIdentity) -> bool {
        self.desired_target.as_ref() == Some(target)
    }

    /// Whether the committed row profile remains an exact semantic authority for one lane even
    /// though the complete pixel target changed. Row activity depends on the lane's genomic
    /// interval and atlas lineage, not its visible-row camera, clip, scale factor, or surface
    /// allocation. Atlas-lineage changes are reconciled separately when a semantic epoch commits.
    fn committed_row_target_matches_desired(&self, panel_id: PanelId, track_id: TrackId) -> bool {
        fn lane(
            target: &AlignmentSceneTargetIdentity,
            panel_id: PanelId,
            track_id: TrackId,
        ) -> Option<&AlignmentLaneTargetIdentity> {
            target
                .panels
                .iter()
                .find(|panel| panel.panel_id == panel_id)
                .and_then(|panel| panel.lanes.iter().find(|lane| lane.track_id == track_id))
        }
        let (Some(committed), Some(desired)) = (
            self.target
                .as_ref()
                .and_then(|target| lane(target, panel_id, track_id)),
            self.desired_target
                .as_ref()
                .and_then(|target| lane(target, panel_id, track_id)),
        ) else {
            return false;
        };
        committed.read_camera.is_some()
            && desired.read_camera.is_some()
            && committed.preparation_camera.interval() == desired.preparation_camera.interval()
    }

    const fn attempt_sequence(&self) -> u64 {
        self.attempt_sequence
    }

    const fn authorizes_target(&self, revision: u64) -> bool {
        revision != 0 && revision == self.desired_target_revision
    }

    fn reset(&mut self) {
        *self = Self::default();
    }
}

/// One logical rectangle that a successfully prepared native alignment presentation can paint for
/// a specific panel-camera capture. Navigation preview composition consumes this authority rather
/// than inferring readiness from the existence of a semantic lane or snapshot.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AlignmentNativeLiveRegion {
    panel_id: PanelId,
    target_revision: u64,
    clip: CpuClipRect,
}

/// Exact read-lane authority published with the native frame. Hit testing consumes this record
/// rather than inferring visibility from a semantic snapshot that may have survived a failed or
/// superseded GPU replacement.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AlignmentNativeReadAuthority {
    panel_id: PanelId,
    track_id: TrackId,
    target_revision: u64,
    source_request_token: AlignmentSceneRequestToken,
    desired_request_token: AlignmentSceneRequestToken,
    source_camera: AlignmentSceneCamera,
    scene_revision: rgv_alignment_scene::SceneRevision,
    clip: CpuClipRect,
}

/// Source provenance committed beside one panel lane, including lanes whose exact pixels or
/// vertical activity are still pending. This is presentation authority, not worker staging state.
#[derive(Clone, Copy, Debug, PartialEq)]
struct AlignmentCommittedLaneAuthority {
    panel_id: PanelId,
    track_id: TrackId,
    target_revision: u64,
    source_request_token: AlignmentSceneRequestToken,
    desired_request_token: AlignmentSceneRequestToken,
    source_camera: AlignmentSceneCamera,
    individual_reads: AlignmentIndividualReadCapability,
    semantic_target_complete: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AlignmentGpuPreparedAuthority {
    read_pages: Arc<[AlignmentPageKey]>,
    summary_pages: Arc<[AlignmentSummaryGpuPageKey]>,
}

/// The only published native alignment presentation. Reads and every supported derived lane are
/// assigned together after one shared replacement bundle settles, so the render loop cannot
/// observe a mixture of camera epochs. A rolled-back older bundle may remain owned for the next
/// retry. It remains visible after a failed same-target attempt, but is stale-gated as soon as the
/// exact camera/layout target changes. Renderer-local sequences must match the successful attempt.
struct AlignmentGpuFrameBundle {
    target_revision: u64,
    publication_attempt_sequence: u64,
    /// Exact immutable prepared-page authority minted by the content replacement. Camera-only
    /// frames and redraw-driven prefetch clone this authority instead of rebuilding complete
    /// scene manifests on the UI thread.
    prepared_authority: AlignmentGpuPreparedAuthority,
    reads: Option<AlignmentGpuFrame>,
    summaries: Option<AlignmentSummaryGpuFrame>,
    live_regions: Vec<AlignmentNativeLiveRegion>,
    lane_authorities: Vec<AlignmentCommittedLaneAuthority>,
    read_authorities: Vec<AlignmentNativeReadAuthority>,
}

/// The single presentation commit point for semantic panel captures and their native derivative.
/// `gpu_bundle` is `None` only synchronously while a replacement transaction has detached the old
/// resources; no render, display, or hit-test code runs in that interval.
struct PresentedAlignmentEpoch {
    panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
    gpu_bundle: Option<AlignmentGpuFrameBundle>,
}

impl PresentedAlignmentEpoch {
    fn new(
        panel_frames: HashMap<PanelId, AlignmentPanelSceneFrame>,
        gpu_bundle: AlignmentGpuFrameBundle,
    ) -> Self {
        Self {
            panel_frames,
            gpu_bundle: Some(gpu_bundle),
        }
    }
}

#[derive(Clone, Copy)]
struct PresentedAlignmentLane<'a> {
    target_revision: u64,
    lane: &'a rgv_alignment_scene::AlignmentPanelLaneFrame,
    source: &'a AlignmentCommittedLaneAuthority,
    read_authority: Option<&'a AlignmentNativeReadAuthority>,
}

fn alignment_gpu_sequences_match_attempt(
    publication_attempt_sequence: u64,
    read_sequences: Option<&[u64]>,
    summary_sequences: Option<&[u64]>,
) -> bool {
    read_sequences.is_none_or(|sequences| {
        sequences
            .iter()
            .all(|sequence| *sequence == publication_attempt_sequence)
    }) && summary_sequences.is_none_or(|sequences| {
        sequences
            .iter()
            .all(|sequence| *sequence == publication_attempt_sequence)
    })
}

fn alignment_gpu_bundle_is_current(
    bundle: &AlignmentGpuFrameBundle,
    authority: &AlignmentScenePublicationAuthority,
) -> bool {
    // Desired, rather than merely committed, target authority is intentional: an old rollback
    // epoch must never render or answer hits as if it described a newer camera/layout target.
    authority.authorizes_target(bundle.target_revision)
        && alignment_gpu_sequences_match_attempt(
            bundle.publication_attempt_sequence,
            bundle
                .reads
                .as_ref()
                .map(|frame| frame.camera_sequences.as_ref()),
            bundle
                .summaries
                .as_ref()
                .map(|frame| frame.camera_sequences.as_ref()),
        )
}

/// Speculative complete-scene pins are one unit because either renderer may need their shared
/// budget for the next foreground bundle. Partial acquisition is never published here.
struct AlignmentGpuResidencyBundle {
    authority: AlignmentGpuPreparedAuthority,
    _reads: Option<AlignmentGpuResidencyLease>,
    _summaries: Option<AlignmentSummaryGpuResidencyLease>,
}

#[derive(Default)]
struct AlignmentGpuPrefetchState {
    reads: bool,
    summaries: bool,
    read_cursor: Option<AlignmentGpuPrefetchCursor>,
    summary_cursor: Option<AlignmentSummaryGpuPrefetchCursor>,
    read_residency: Option<AlignmentGpuResidencyLease>,
    summary_residency: Option<AlignmentSummaryGpuResidencyLease>,
}

/// A recoverable foreground GPU publication refusal. Keeping the original ledger error typed
/// lets the scheduler determine whether the exact obstruction still exists without parsing a
/// user-facing diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlignmentGpuPublicationBlocker {
    Ledger(LedgerError),
    /// A shared-ledger policy edit raced a replacement plan. The mismatch itself proves that the
    /// refused plan is obsolete, so the next attempt must re-snapshot and re-plan exactly once.
    PolicyRevisionChanged {
        planned: u64,
        current: u64,
    },
}

/// Progress gate for one exact desired-target publication attempt. `Ready` is used only when the
/// refusal itself proves that the obstruction settled before it could be recorded (for example,
/// the reported replacement bundle is no longer active in the first post-error snapshot).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AlignmentGpuPublicationRetryGate {
    AwaitingLedgerProgress(LedgerProgressEpoch),
    ReadyAfterObservedProgress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AlignmentGpuPublicationRetry {
    target_revision: u64,
    attempt_sequence: u64,
    blocker: AlignmentGpuPublicationBlocker,
    gate: AlignmentGpuPublicationRetryGate,
}

impl AlignmentGpuPrefetchState {
    const fn pending(&self) -> bool {
        self.reads || self.summaries
    }

    fn reset(&mut self, reads: bool, summaries: bool) {
        *self = Self {
            reads,
            summaries,
            ..Self::default()
        };
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum AlignmentGpuSpeculationPhase {
    #[default]
    Eligible,
    Warming,
    Resident,
    Suppressed,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct AlignmentGpuSpeculationGate {
    authority: Option<AlignmentGpuPreparedAuthority>,
    budget_bytes: usize,
    phase: AlignmentGpuSpeculationPhase,
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
    alignment_scene_ledger: AllocationLedger,
    alignment_ledger_progress: LedgerProgressSubscription,
    alignment_gpu_renderer: AlignmentGpuRenderer,
    alignment_summary_gpu_renderer: AlignmentSummaryGpuRenderer,
    alignment_presented_epoch: Option<PresentedAlignmentEpoch>,
    alignment_gpu_residency_bundle: Option<AlignmentGpuResidencyBundle>,
    alignment_gpu_prefetch: AlignmentGpuPrefetchState,
    alignment_gpu_publication_retry: Option<AlignmentGpuPublicationRetry>,
    alignment_gpu_speculation_gate: AlignmentGpuSpeculationGate,
    alignment_panel_scene_stores: HashMap<PanelId, AlignmentPanelSceneStore>,
    alignment_scene_owner_clock: u64,
    alignment_scene_request_clock: u64,
    alignment_scene_publication_authority: AlignmentScenePublicationAuthority,
    navigation_preview_renderer: NavigationPreviewRenderer,
    navigation_texture_revisions: HashMap<PanelId, u64>,
    navigation_horizon_preview_stats: NavigationHorizonPreviewStats,
    navigation_stale_result_rejections: u64,
    egui_context: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    panels: PanelWorkspace,
    workspace_layout: WorkspaceLayoutSnapshot,
    genomic_canvas_layer_id: Option<egui::LayerId>,
    pane_divider_rect: Option<egui::Rect>,
    composite_display_list: DisplayList,
    /// Canonical workspace annotation row heights, keyed by presentation
    /// identity. Every locus uses this table before its display is clipped.
    shared_annotation_frame_heights: BTreeMap<PresentationNodeId, f32>,
    alignment_tracks: Vec<AlignmentTrackController>,
    empty_alignment: EmptyAlignmentState,
    alignment_retirement_tasks: Vec<JoinHandle<()>>,
    reference_engine: Option<ReferenceEngine>,
    /// Identity of the currently open FASTA handle. A panel slice is usable only when it carries
    /// this same revision; reopening an equivalent path still creates a distinct authority.
    reference_revision: Option<ReferenceRevision>,
    reference_open_error: Option<String>,
    track_engine: Option<TrackEngine>,
    automation_auxiliary_open_state: HashMap<TrackId, AutomationAuxiliaryOpenState>,
    tracks: TrackCollection,
    track_presentation: WorkspacePresentation,
    reference_source: Option<TrackSource>,
    remote_reference_index_url: Option<String>,
    reference_remote_io_stats: Option<RemoteIoStats>,
    reference_dictionary: Option<ReferenceDictionary>,
    contig_aliases: ContigAliases,
    assembly: GenomeAssembly,
    assembly_identity: Option<GenomeAssemblyIdentity>,
    suggested_assembly_identity: Option<GenomeAssemblyIdentity>,
    landmark_resource_root: Option<PathBuf>,
    landmark_catalogue: Option<GenomeLandmarkCatalogue>,
    landmark_catalogue_error: Option<String>,
    landmark_resolution: Option<rgv_core::AssemblyResolution>,
    landmark_search_index: Option<LandmarkSearchIndex>,
    suppressed_builtin_gene_bundles: BTreeSet<String>,
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
    navigation_contract_revision: u64,
    display_dirty: bool,
    display_upload_pending_metrics: bool,
    pending_resolution_query_refresh: Option<Instant>,
    automation_properties_resize_active: bool,
    properties_resize_gesture_active: bool,
    workspace_layout_recomputations: u64,
    resize_query_refreshes: u64,
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
    sashimi_interaction_diagnostics: SashimiInteractionDiagnostics,
    command_search: String,
    annotation_search_generation: u64,
    annotation_navigation_generation: u64,
    annotation_search_results: TrackSearchResults,
    pending_annotation_navigation: Option<String>,
    settings: DesktopSettings,
    settings_path: PathBuf,
    current_session_path: Option<PathBuf>,
    unresolved_session_sources: Vec<UnresolvedSessionSource>,
    session_migration_notices: Vec<String>,
    igv_import_report: Option<IgvImportReport>,
    next_autosave: Instant,
    recovery_candidates: Vec<PathBuf>,
    recovery_open: bool,
    automation: Option<AutomationRunner>,
    control_service: Option<ControlService>,
    automation_exit_requested: bool,
    exit_confirmation_open: bool,
    user_exit_requested: bool,
    cache_budgets: CacheBudgets,
    cache_coordinator: LiveCacheCoordinatorSnapshot,
    row_scene_residency_cache: Option<(Vec<(usize, u64)>, usize)>,
    locus_editor: String,
    resizing_annotation_track: Option<(TrackFrameKey, f32)>,
    resizing_read_track: Option<(TrackId, f32)>,
    resizing_read_pane: Option<f32>,
    active_stack_scrollbar_gesture: Option<(PanelId, ui::TrackPane)>,
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
