//! Domain types shared by the RGV application, data engine, and renderer.

mod automation;
mod coordinates;
mod generation;
mod genome;
mod lod;
mod navigation;
mod panel;
mod sample_metadata;
mod session;
mod settings;
mod source;
mod track;
mod viewport;

pub use automation::{
    AUTOMATION_SCHEMA_VERSION, AutomationAlignmentHit, AutomationAlignmentRelationship,
    AutomationAlignmentReveal, AutomationAlignmentSearchResult, AutomationAlignmentSelector,
    AutomationAlignmentState, AutomationAlignmentTrackSnapshot, AutomationBatch, AutomationCommand,
    AutomationCommandResult, AutomationCommandStatus, AutomationCytobandAvailability,
    AutomationEnvironmentTelemetry, AutomationError, AutomationErrorCode, AutomationEvidenceField,
    AutomationExportKind, AutomationFrameTelemetry, AutomationGenomeSnapshot,
    AutomationGpuTelemetry, AutomationHistoryEntrySnapshot, AutomationHistoryNavigation,
    AutomationInteractionTrace, AutomationInteractionTraceEvent, AutomationNavigationDestination,
    AutomationNavigationHistorySnapshot, AutomationOptionScope, AutomationPaneItemLayoutSnapshot,
    AutomationPaneLayoutSnapshot, AutomationPanelSnapshot, AutomationPresentationCompositeSnapshot,
    AutomationPresentationGroupSnapshot, AutomationPresentationNodeSnapshot,
    AutomationReadPanelStateSnapshot, AutomationRectSnapshot, AutomationRenderLaneSnapshot,
    AutomationRenderOmissionReason, AutomationReviewItemSnapshot, AutomationReviewNavigation,
    AutomationReviewQueueSnapshot, AutomationRunReport, AutomationRunTelemetry,
    AutomationSelection, AutomationSelectionGranularity, AutomationSelectionKind,
    AutomationSelectionSnapshot, AutomationSetting, AutomationSnapshotFormat,
    AutomationSnapshotScope, AutomationTrackGestureKind, AutomationTrackGestureSnapshot,
    AutomationTrackLifecycle, AutomationTrackSnapshot, AutomationUnsavedChangesPolicy,
    AutomationViewportOperation, AutomationWorkspaceLayoutSnapshot,
    AutomationWorkspacePanelLayoutSnapshot, AutomationWorkspaceSnapshot,
    MAXIMUM_AUTOMATION_ALIGNMENT_RESULTS, MAXIMUM_AUTOMATION_COMMANDS,
    MAXIMUM_AUTOMATION_EVIDENCE_FIELDS, MAXIMUM_AUTOMATION_HISTORY_ENTRIES,
    MAXIMUM_AUTOMATION_LOCI, MAXIMUM_AUTOMATION_OPTION_BYTES,
    MAXIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS, MAXIMUM_AUTOMATION_SNAPSHOT_TRACKS,
    MAXIMUM_AUTOMATION_STRING_BYTES, MAXIMUM_AUTOMATION_TRACK_SELECTION,
    MINIMUM_AUTOMATION_SCHEMA_VERSION, MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS,
};
pub use coordinates::{
    Contig, ContigAliasError, ContigAliases, ContigId, GenomicInterval, IntervalError,
    ReferenceDictionary, resolve_contig_alias,
};
pub use generation::{GenerationClock, ViewportGeneration};
pub use genome::{
    AssemblyIdentityConfidence, Cytoband, CytobandParseError, CytobandStain,
    GENOME_CONFIGURATION_SCHEMA_VERSION, GeneticCode, GenomeAnnotation, GenomeAssembly,
    GenomeAssemblyIdentity, GenomeAssetProvenance, GenomeConfiguration, GenomeConfigurationError,
    MAXIMUM_DEFAULT_ANNOTATIONS, MAXIMUM_GENOME_ASSET_PROVENANCE, ReferenceTranslationFrame,
    parse_ucsc_cytobands,
};
pub use lod::{DetailLevel, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, RenderBudget};
pub use navigation::NavigationHistory;
pub use panel::{PanelGeneration, PanelId, PanelTrackId};
pub use sample_metadata::{
    MAXIMUM_SAMPLE_KEY_BYTES, MAXIMUM_SAMPLE_METADATA_BYTES, MAXIMUM_SAMPLE_METADATA_CELLS,
    MAXIMUM_SAMPLE_METADATA_COLUMNS, MAXIMUM_SAMPLE_METADATA_FIELD_BYTES,
    MAXIMUM_SAMPLE_METADATA_ROWS, SampleMetadataError, SampleMetadataRow, SampleMetadataTable,
};
pub use session::{
    AlignmentColorMode, AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode,
    BisulfiteContext, CoverageScaleTransform, IgvImportReport, IgvSessionImport,
    InspectorActivation, JunctionDisplayMode, MAXIMUM_ALIGNMENT_TRACKS, MAXIMUM_CONTIG_ALIASES,
    MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS, MAXIMUM_REVIEW_NOTE_BYTES,
    MAXIMUM_VIEW_PANELS, ModifiedBaseColorMode, OverlappingMatePolicy, PresentationNodeId,
    ReadBaseDisplayMode, RegionOfInterest, ReviewItem, ReviewStatus, SESSION_SCHEMA_VERSION,
    SashimiCountMode, SashimiScaleMode, SbxDisplayOptions, SbxSimplexTailMode,
    SessionAlignmentTrack, SessionDocument, SessionError, SessionFilters, SessionLocus,
    SessionPaneTrackState, SessionPresentationComposite, SessionPresentationGroup,
    SessionReadPanelState, SessionViewPanel, SmrtKineticsMode, TrackChangeSet, TrackChannel,
    TrackIntent, TrackIntentError, UltimaDisplayOptions, UltimaQualityMode,
    UnresolvedSessionSource, WorkspacePresentation,
};
pub use settings::{
    DEFAULT_AUTOSAVE_INTERVAL_SECONDS, DEFAULT_AUTOSAVE_RETENTION, DEFAULT_CACHE_BUDGET_MIB,
    DEFAULT_INTERFACE_SCALE_PERCENT, DesktopSettings, MAXIMUM_AUTOSAVE_INTERVAL_SECONDS,
    MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_CACHE_BUDGET_MIB, MAXIMUM_INTERFACE_SCALE_PERCENT,
    MINIMUM_AUTOSAVE_INTERVAL_SECONDS, MINIMUM_AUTOSAVE_RETENTION, MINIMUM_CACHE_BUDGET_MIB,
    MINIMUM_INTERFACE_SCALE_PERCENT, SETTINGS_SCHEMA_VERSION, SettingsError,
};
pub use source::{
    ClassifiedGenomicSource, GenomicSourceFormat, classify_genomic_source,
    default_genomic_source_label,
};
pub use track::{
    CopyNumberSortLocus, CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode,
    FeatureDisplayMode, FeatureTrackSettings, GwasColorMode, GwasTrackSettings,
    InteractionDisplayMode, InteractionTrackSettings, MAX_VARIANT_FIELD_PREDICATES,
    MAX_VARIANT_FILTER_VALUES, MAX_VARIANT_SAMPLE_SUBSET, MAX_VARIANT_SETTING_STRING_BYTES,
    MAXIMUM_TDF_CHANNELS, MutationColorMode, MutationTrackSettings, QuantitativeArithmetic,
    QuantitativeDisplayMode, QuantitativeMissingMode, QuantitativeScaleMode,
    QuantitativeScaleTransform, QuantitativeTrackSettings, QuantitativeWindowFunction,
    TrackCollection, TrackConfig, TrackFormat, TrackId, TrackKind, TrackSource, VariantColorMode,
    VariantFieldComparison, VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter,
    VariantGenotypeGrouping, VariantMissingValueRule, VariantTrackSettings, VariantTypeGroup,
};
pub use viewport::{Viewport, ViewportError};
