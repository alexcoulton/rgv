//! Domain types shared by the RGV application, data engine, and renderer.

mod coordinates;
mod generation;
mod genome;
mod igv_genome;
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
pub use landmarks::{
    AssemblyMatchEvidence, AssemblyResolution, AssemblyResolutionInput, AssemblyResolutionStatus,
    BuiltInLandmarkContext, BuiltInTrackIdentity, GenomeLandmarkAsset, GenomeLandmarkBundle,
    GenomeLandmarkCatalogue, GenomeLandmarkCatalogueError, LANDMARK_CATALOGUE_SCHEMA_VERSION,
    LANDMARK_SEARCH_SCHEMA_VERSION, LandmarkAssemblyIdentity, LandmarkAssetFormat,
    LandmarkAssetRole, LandmarkContigFingerprint, LandmarkRootCandidate, LandmarkRootDiscovery,
    LandmarkRootSource, LandmarkSearchEntry, LandmarkSearchIndex, LandmarkSearchIndexError,
    MAXIMUM_LANDMARK_ASSETS, MAXIMUM_LANDMARK_BUNDLES, dictionary_sha256, discover_landmark_root,
    landmark_catalogue_directory, parse_landmark_aliases, parse_landmark_search_index,
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
/// Validation shared with the automation protocol in `rgv-automation`. Not a supported API.
#[doc(hidden)]
pub use session::validate_alignment_predicate_expression;
pub use session::{
    AlignmentAuxComparison, AlignmentAuxFilterValue, AlignmentCigarEventPredicate,
    AlignmentColorMode, AlignmentFlagPredicate, AlignmentGroupMode, AlignmentOrderDirection,
    AlignmentPredicateExpression, AlignmentPredicateScope, AlignmentReadSampling,
    AlignmentSortMode, AlignmentViewPredicate, BisulfiteContext, CoverageScaleTransform,
    IgvImportReport, IgvLocusResolution, IgvSessionImport, InspectorActivation,
    JunctionDisplayMode, MAXIMUM_ALIGNMENT_TRACKS, MAXIMUM_CONFIGURABLE_COMPLETE_READ_LIMIT,
    MAXIMUM_CONTIG_ALIASES, MAXIMUM_REGIONS_OF_INTEREST, MAXIMUM_REVIEW_ITEMS,
    MAXIMUM_REVIEW_NOTE_BYTES, MAXIMUM_SESSION_TRACKS, MAXIMUM_VIEW_PANELS, ModifiedBaseColorMode,
    OverlappingMatePolicy, PresentationNodeId, ReadBaseDisplayMode, ReadStackingMode,
    ReadStackingTransitionReason, RegionOfInterest, ReviewItem, ReviewStatus,
    SESSION_SCHEMA_MINOR_VERSION, SESSION_SCHEMA_VERSION, SashimiCountMode, SashimiScaleMode,
    SbxDisplayOptions, SbxSimplexTailMode, SessionAlignmentTrack, SessionDocument, SessionError,
    SessionFilters, SessionLocus, SessionPaneTrackState, SessionPresentationComposite,
    SessionPresentationGroup, SessionReadPanelState, SessionViewPanel, SmrtKineticsMode,
    TrackChangeSet, TrackChannel, TrackIntent, TrackIntentError, UltimaDisplayOptions,
    UltimaQualityMode, UnresolvedSessionSource, WorkspacePresentation,
};
pub use settings::{
    AlignmentMemoryPolicy, DEFAULT_AUTOSAVE_INTERVAL_SECONDS, DEFAULT_AUTOSAVE_RETENTION,
    DEFAULT_CACHE_BUDGET_MIB, DEFAULT_INTERFACE_SCALE_PERCENT,
    DEFAULT_READ_BROWSING_BUFFER_PERCENT, DesktopSettings, InterfaceTheme,
    MAXIMUM_AUTOSAVE_INTERVAL_SECONDS, MAXIMUM_AUTOSAVE_RETENTION, MAXIMUM_CACHE_BUDGET_MIB,
    MAXIMUM_INTERFACE_SCALE_PERCENT, MAXIMUM_READ_BROWSING_BUFFER_PERCENT,
    MINIMUM_AUTOSAVE_INTERVAL_SECONDS, MINIMUM_AUTOSAVE_RETENTION, MINIMUM_CACHE_BUDGET_MIB,
    MINIMUM_INTERFACE_SCALE_PERCENT, MINIMUM_READ_BROWSING_BUFFER_PERCENT, SETTINGS_SCHEMA_VERSION,
    SettingsError, SettingsRecovery, TrackPipeline,
};
pub use source::{
    ClassifiedGenomicSource, GenomicSourceFormat, classify_genomic_source,
    default_genomic_source_label,
};
pub use track::{
    CopyNumberSortLocus, CopyNumberSortMode, CopyNumberTrackSettings, CopyNumberValueMode,
    FeatureDisplayMode, FeatureTrackSettings, GwasColorMode, GwasTrackSettings,
    InteractionDisplayMode, InteractionTrackSettings, MAX_EXPANDED_GENES,
    MAX_VARIANT_FIELD_PREDICATES, MAX_VARIANT_FILTER_VALUES, MAX_VARIANT_SAMPLE_SUBSET,
    MAX_VARIANT_SETTING_STRING_BYTES, MAXIMUM_TDF_CHANNELS, MutationColorMode,
    MutationTrackSettings, QuantitativeArithmetic, QuantitativeDisplayMode,
    QuantitativeMissingMode, QuantitativeScaleMode, QuantitativeScaleTransform,
    QuantitativeTrackSettings, QuantitativeWindowFunction, TrackCollection, TrackConfig,
    TrackFormat, TrackId, TrackKind, TrackSource, TranscriptDisplay, VariantColorMode,
    VariantFieldComparison, VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter,
    VariantGenotypeGrouping, VariantMissingValueRule, VariantTrackSettings, VariantTypeGroup,
};
pub use viewport::{MIN_VISIBLE_BASES, Viewport, ViewportError, minimum_visible_span};
