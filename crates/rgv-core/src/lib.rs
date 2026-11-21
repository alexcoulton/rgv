//! Domain types shared by the RGV application, data engine, and renderer.

mod automation;
mod coordinates;
mod generation;
mod genome;
mod igv_genome;
mod interaction_stress;
mod landmarks;
mod locus;
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
    AutomationAlignmentReveal, AutomationAlignmentSceneDiagnostics,
    AutomationAlignmentSearchResult, AutomationAlignmentSelector, AutomationAlignmentState,
    AutomationAlignmentTrackSnapshot, AutomationBatch, AutomationCommand, AutomationCommandResult,
    AutomationCommandStatus, AutomationContextLaneSnapshot, AutomationCytobandAvailability,
    AutomationEnvironmentTelemetry, AutomationError, AutomationErrorCode, AutomationEvidenceField,
    AutomationExportKind, AutomationFrameTelemetry, AutomationGenomeSnapshot,
    AutomationGpuTelemetry, AutomationHistoryEntrySnapshot, AutomationHistoryNavigation,
    AutomationInteractionTrace, AutomationInteractionTraceEvent, AutomationLandmarkIntegrityState,
    AutomationLandmarkSnapshot, AutomationLandmarkTrackState, AutomationNavigationDestination,
    AutomationNavigationHistorySnapshot, AutomationNavigationTelemetry, AutomationOptionScope,
    AutomationPaneItemLayoutSnapshot, AutomationPaneLayoutSnapshot, AutomationPanelSnapshot,
    AutomationPresentationCompositeSnapshot, AutomationPresentationGroupSnapshot,
    AutomationPresentationNodeSnapshot, AutomationReadPanelStateSnapshot,
    AutomationReadPresentationState, AutomationRectSnapshot, AutomationRenderLaneSnapshot,
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
    MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT, MINIMUM_AUTOMATION_SCHEMA_VERSION,
    MINIMUM_AUTOMATION_SETTLE_TIMEOUT_MILLIS, check_automation_remote_url,
    check_session_sources_within,
};
pub use coordinates::{
    Contig, ContigAliasError, ContigAliases, ContigId, DuplicateContigError, GenomicInterval,
    IntervalError, ReferenceDictionary, resolve_contig_alias,
};
pub use generation::{GenerationClock, ViewportGeneration};
pub use genome::{
    AssemblyIdentityConfidence, Cytoband, CytobandParseError, CytobandStain,
    GENOME_CONFIGURATION_SCHEMA_VERSION, GeneticCode, GenomeAnnotation, GenomeAssembly,
    GenomeAssemblyIdentity, GenomeAssetProvenance, GenomeConfiguration, GenomeConfigurationError,
    MAXIMUM_DEFAULT_ANNOTATIONS, MAXIMUM_GENOME_ASSET_PROVENANCE, ReferenceTranslationFrame,
    parse_ucsc_cytobands, parse_ucsc_cytobands_with_aliases,
};
pub use igv_genome::{
    IGV_HOSTED_GENOME_LIST_URL, IgvGenomeDescription, IgvGenomeError, IgvGenomeListEntry,
    IgvGenomeTrack, parse_igv_alias_table, parse_igv_genome_list,
};
pub use interaction_stress::{
    INTERACTION_RECORDING_SCHEMA_NAME, INTERACTION_RECORDING_SCHEMA_VERSION,
    INTERACTION_STRESS_SCHEMA_NAME, INTERACTION_STRESS_SCHEMA_VERSION, InteractionLifecycle,
    InteractionRecordingCheckpoint, InteractionRecordingEntry, InteractionRecordingFooter,
    InteractionRecordingHeader, InteractionRecordingState, InteractionRecordingStatus,
    InteractionStressError, InteractionStressRunManifest, InteractionStressScenario,
    InteractionStressStep, MAXIMUM_INTERACTION_STRESS_DOCUMENT_BYTES,
    MAXIMUM_STRESS_COORDINATE_FRACTION, MAXIMUM_STRESS_NATIVE_EVENTS, MAXIMUM_STRESS_NESTING,
    MAXIMUM_STRESS_REPEAT_COUNT, MAXIMUM_STRESS_RUN_MILLIS, MAXIMUM_STRESS_STEPS,
    MAXIMUM_STRESS_STRING_BYTES, MAXIMUM_STRESS_TIMING_SCALE, NativeEventShape,
    NativeInputObservation, NativeObservationSummary, RecordedReadScroll,
    STRESS_HEALTH_SCHEMA_VERSION, StressActionKind, StressArtifactReference, StressFailureClass,
    StressFailureSignature, StressGeneratorProfile, StressGeneratorSpecification,
    StressHealthSnapshot, StressInteractionState, StressModifiers, StressNativeEvent, StressPoint,
    StressPredicate, StressRequirements, StressRunEnvironment, StressRunLimits, StressRunState,
    StressScrollUnit, StressStepResult, StressTarget, StressTiming, StressTouchPhase,
};
pub use landmarks::{
    AssemblyMatchEvidence, AssemblyResolution, AssemblyResolutionInput, AssemblyResolutionStatus,
    BuiltInLandmarkContext, BuiltInTrackIdentity, GenomeLandmarkAsset, GenomeLandmarkBundle,
    GenomeLandmarkCatalogue, GenomeLandmarkCatalogueError, LANDMARK_CATALOGUE_SCHEMA_VERSION,
    LANDMARK_SEARCH_SCHEMA_VERSION, LandmarkAssemblyIdentity, LandmarkAssetFormat,
    LandmarkAssetRole, LandmarkContigFingerprint, LandmarkSearchEntry, LandmarkSearchIndex,
    LandmarkSearchIndexError, MAXIMUM_LANDMARK_ASSETS, MAXIMUM_LANDMARK_BUNDLES, dictionary_sha256,
    parse_landmark_aliases, parse_landmark_search_index,
};
pub use locus::{Locus, LocusError, LocusRange, ResolvedLocus, resolve_contig_name, split_loci};
pub use lod::{DetailLevel, MAXIMUM_BASE_MARK_BASES_PER_PIXEL, RenderBudget};
pub use navigation::{NamedNavigationHistory, NavigationEntry, NavigationHistory};
pub use panel::{PanelGeneration, PanelId, PanelTrackId};
pub use sample_metadata::{
    MAXIMUM_SAMPLE_KEY_BYTES, MAXIMUM_SAMPLE_METADATA_BYTES, MAXIMUM_SAMPLE_METADATA_CELLS,
    MAXIMUM_SAMPLE_METADATA_COLUMNS, MAXIMUM_SAMPLE_METADATA_FIELD_BYTES,
    MAXIMUM_SAMPLE_METADATA_ROWS, SampleMetadataError, SampleMetadataRow, SampleMetadataTable,
};
pub use session::newer_writer_notice;
pub use session::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentCigarEventPredicate,
    AlignmentColorMode, AlignmentFlagPredicate, AlignmentGroupMode, AlignmentOrderDirection,
    AlignmentPredicateExpression, AlignmentPredicateScope, AlignmentReadSampling,
    AlignmentSortMode, AlignmentViewPredicate, BisulfiteContext, CoverageScaleTransform,
    IgvImportReport, IgvLocusResolution, IgvSessionImport, InspectorActivation,
    JunctionDisplayMode, MAXIMUM_ALIGNMENT_TRACKS, MAXIMUM_CONTIG_ALIASES,
    MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS, MAXIMUM_REVIEW_NOTE_BYTES,
    MAXIMUM_SESSION_TRACKS, MAXIMUM_VIEW_PANELS, ModifiedBaseColorMode, OverlappingMatePolicy,
    PresentationNodeId, ReadBaseDisplayMode, ReadStackingMode, ReadStackingTransitionReason,
    RegionOfInterest, ReviewItem, ReviewStatus, SESSION_SCHEMA_MINOR_VERSION,
    SESSION_SCHEMA_VERSION, SashimiCountMode, SashimiScaleMode, SbxDisplayOptions,
    SbxSimplexTailMode, SessionAlignmentTrack, SessionDocument, SessionError, SessionFilters,
    SessionLocus, SessionPaneTrackState, SessionPresentationComposite, SessionPresentationGroup,
    SessionReadPanelState, SessionViewPanel, SmrtKineticsMode, TrackChangeSet, TrackChannel,
    TrackIntent, TrackIntentError, UltimaDisplayOptions, UltimaQualityMode,
    UnresolvedSessionSource, WorkspacePresentation,
};
pub use settings::{
    DEFAULT_AUTOSAVE_INTERVAL_SECONDS, DEFAULT_AUTOSAVE_RETENTION, DEFAULT_CACHE_BUDGET_MIB,
    DEFAULT_INTERFACE_SCALE_PERCENT, DEFAULT_READ_BROWSING_BUFFER_PERCENT, DesktopSettings,
    MAXIMUM_AUTOSAVE_INTERVAL_SECONDS, MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_CACHE_BUDGET_MIB,
    MAXIMUM_INTERFACE_SCALE_PERCENT, MAXIMUM_READ_BROWSING_BUFFER_PERCENT,
    MINIMUM_AUTOSAVE_INTERVAL_SECONDS, MINIMUM_AUTOSAVE_RETENTION, MINIMUM_CACHE_BUDGET_MIB,
    MINIMUM_INTERFACE_SCALE_PERCENT, MINIMUM_READ_BROWSING_BUFFER_PERCENT, SETTINGS_SCHEMA_VERSION,
    SettingsError, SettingsRecovery,
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
