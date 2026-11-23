//! Backend-neutral genomic data model.
//!
//! The record, batch, summary and query value types that layout, the alignment scene, the
//! renderer and the engine consume, plus the pure functions over them. This crate performs no
//! I/O and links no C library: decoders, indexes and HTTP live in `rgv-hts`, which re-exports
//! every item here from its original path.
//!
//! The modules are public so `rgv-hts` can reach the decoder-facing hooks (`#[doc(hidden)]`
//! items) its backends drive. Everything supported is re-exported from the crate root.

pub mod alignment;
pub mod bisulfite;
pub mod coverage_pyramid;
pub mod exact_depth;
pub mod filter;
pub mod junction;
pub mod modification_summary;
pub mod modified_bases;
pub mod pair;
pub mod pileup;
pub mod query;
pub mod reference;
pub mod sashimi;
pub mod sbx;
pub mod smrt_kinetics;
pub mod source_position;
pub mod source_revision;
pub mod supplementary;
pub mod track_tile;
pub mod tracks;
pub mod ultima;

pub use alignment::{
    AlignedBaseClassification, AlignmentBatch, AlignmentFlags, AlignmentHeaderMetadata,
    AlignmentOverlapIndex, AlignmentPayloadBatch, AlignmentPayloadError, AlignmentReadGroup,
    AlignmentRecord, AlignmentRecordView, AlignmentReferenceSequence, AlignmentStableId,
    AlignmentStorageRecordId, AuxField, AuxValue, CigarOp, CigarOpKind, MAX_ALIGNMENT_READ_GROUPS,
    PackedRecordError, SequenceData, SequenceView, UltimaTagOrientation, classify_aligned_base,
    resolve_assembly_from_header,
};
pub use bisulfite::{
    BisulfiteObservation, BisulfiteSummary, bisulfite_context_matches, bisulfite_strand_is_reverse,
    classify_bisulfite_observation, summarize_bisulfite_record,
};
pub use coverage_pyramid::{
    COVERAGE_PYRAMID_BASE_BIN_BASES, COVERAGE_PYRAMID_LEVEL_FACTOR, COVERAGE_PYRAMID_MAX_LEVELS,
    COVERAGE_PYRAMID_TILE_BINS, CoverageAccumulation, CoverageAccumulator,
    CoverageAccumulatorTelemetry, CoverageBin, CoverageEligibility, CoveragePyramidError,
    CoveragePyramidParameters, CoverageTile, CoverageTileId, add_counted_depth_segments,
    derive_coarser_tiles,
};
pub use exact_depth::{ExactDepthRun, ExactDepthTile, ExactDepthTileBuilder};
pub use filter::{AlignmentFilter, AuxComparison, AuxPredicate, FilterParseError, FilterValue};
pub use junction::{
    JunctionEvidence, JunctionObservation, JunctionProvenance, JunctionStrand, JunctionStrandInput,
    JunctionSummary, JunctionSummaryBuilder, MAX_JUNCTIONS, MAX_JUNCTIONS_PER_RECORD,
    RecordJunctionCollector,
};
pub use modification_summary::{
    MAX_MODIFICATION_SERIES, ModificationBin, ModificationProvenance, ModificationSeries,
    ModificationSeriesKey, ModificationSummary, ModificationSummaryBuilder,
};
pub use modified_bases::{
    BaseModificationStrand, ImplicitModificationMode, ModificationCode, ModifiedBaseCall,
    ModifiedBaseError, aligned_position_for_sequencer_position, decode_modified_bases,
    decode_modified_bases_parts, decode_modified_bases_parts_checked, modification_codes_in_tag,
    modified_bases_resolvable, sequencer_base_matches,
};
pub use pair::{
    MAX_TEMPLATE_LENGTH_SAMPLES, MIN_TEMPLATE_LENGTH_SAMPLES, MateVisibility, PairClassification,
    PairEvidence, PairOrientation, PairSummary, PairSummaryBuilder, ReadOrder, TemplateId,
    TemplateLengthBounds, pair_evidence, pair_evidence_at, pair_evidence_view, template_id,
};
pub use pileup::{
    AlleleSupport, MateAlignment as AlignmentMateAlignment,
    MateOverlapMask as AlignmentMateCoverageMask,
    OverlapCollapser as AlignmentMateOverlapCollapser, OverlapMask as AlignmentMateOverlapMask,
    PileupBin, PileupBuilder, PileupProvenance, PileupSummary, QUALITY_WEIGHT_SCALE,
    QualityWeightedAlleleSupport, add_unmasked_range, aligned_observation,
    base_observation_is_counted,
};
pub use query::{AlignmentQuery, IndividualReadStatus, QueryDetail, QueryOptions, QueryStats};
pub use reference::ReferenceSlice;
pub use sashimi::{
    JunctionPresentation, JunctionStrandSource, ResolvedJunctionStrand, SASHIMI_HOVER_FADE_ALPHA,
    SASHIMI_LABEL_CLEARANCE, SASHIMI_MAXIMUM_STROKE, SASHIMI_MINIMUM_ARC_HEIGHT,
    SASHIMI_MINIMUM_STROKE, SASHIMI_UNLABELLED_CLEARANCE, SashimiPresentationOptions,
    SashimiPresentationTable, SpliceMotif, SpliceSite, SpliceSiteSide, format_psi,
    place_labels_by_priority, psi_label, resolve_junction_strand, sashimi_arc_height,
    sashimi_available_height, sashimi_stroke_width,
};
pub use sbx::{
    SbxEvidenceError, SbxInsertionState, SbxSummary, SbxTailBounds, classify_sbx_insertion,
    sbx_tail_bounds, summarize_sbx_record,
};
pub use smrt_kinetics::{
    MAX_SMRT_KINETICS_VALUES_PER_RECORD, SmrtKineticsError, SmrtKineticsSeries,
    SmrtKineticsSummary, SmrtKineticsValues, decode_smrt_kinetics, smrt_kinetics_series,
    smrt_kinetics_tag,
};
pub use source_position::AlignmentSourcePosition;
pub use source_revision::{AlignmentSourceFormat, AlignmentSourceStamp};
pub use supplementary::{
    MAX_SUPPLEMENTARY_CIGAR_OPERATIONS, MAX_SUPPLEMENTARY_SEGMENTS, SupplementaryChain,
    SupplementarySegment, decode_supplementary_chain,
};
pub use track_tile::*;
pub use tracks::*;
pub use ultima::{
    MAX_ULTIMA_VALUES_PER_RECORD, UltimaEventUnavailable, UltimaEvidenceError, UltimaIndelEvidence,
    UltimaIndelKind, UltimaQualitySource, UltimaRecordAnalysis, UltimaSummary,
    analyze_ultima_record, analyze_ultima_record_with_tag_orientation,
};
