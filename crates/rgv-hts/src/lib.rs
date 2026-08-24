//! Alignment and reference I/O boundary.

mod alignment;
mod bisulfite;
mod filter;
mod index_overview;
mod junction;
mod local;
mod modification_summary;
mod modified_bases;
mod pair;
mod pileup;
mod reference;
mod remote;
mod remote_alignment;
mod sampling;
mod sbx;
mod smrt_kinetics;
mod supplementary;
mod tdf;
mod tracks;
mod ultima;

pub use alignment::{
    AlignmentBatch, AlignmentFlags, AlignmentHeaderMetadata, AlignmentReadGroup, AlignmentRecord,
    AlignmentReferenceSequence, AuxField, AuxValue, CigarOp, CigarOpKind,
    MAX_ALIGNMENT_READ_GROUPS, PackedRecordError, SequenceData, SequenceView,
    recognize_assembly_from_header,
};
pub use bisulfite::{
    BisulfiteObservation, BisulfiteSummary, bisulfite_context_matches, bisulfite_strand_is_reverse,
    classify_bisulfite_observation, summarize_bisulfite_record,
};
pub use filter::{AlignmentFilter, AuxComparison, AuxPredicate, FilterParseError, FilterValue};
pub use index_overview::{ALIGNMENT_INDEX_OVERVIEW_BINS, AlignmentIndexOverview};
pub use junction::{
    JunctionEvidence, JunctionProvenance, JunctionStrand, JunctionSummary, MAX_JUNCTIONS,
    MAX_JUNCTIONS_PER_RECORD,
};
pub use local::{
    AlignmentError, AlignmentOpenOptions, AlignmentQuery, AlignmentSource, LocalAlignmentSource,
    QueryDetail, QueryOptions, QueryStats,
};
pub use modification_summary::{
    MAX_MODIFICATION_SERIES, ModificationBin, ModificationProvenance, ModificationSeries,
    ModificationSeriesKey, ModificationSummary,
};
pub use modified_bases::{
    BaseModificationStrand, ImplicitModificationMode, ModificationCode, ModifiedBaseCall,
    ModifiedBaseError, decode_modified_bases, decode_modified_bases_parts,
};
pub use pair::{
    MAX_TEMPLATE_LENGTH_SAMPLES, MIN_TEMPLATE_LENGTH_SAMPLES, MateVisibility, PairClassification,
    PairEvidence, PairOrientation, PairSummary, ReadOrder, TemplateId, TemplateLengthBounds,
    pair_evidence, pair_evidence_at, template_id,
};
pub use pileup::{
    AlleleSupport, PileupBin, PileupProvenance, PileupSummary, QUALITY_WEIGHT_SCALE,
    QualityWeightedAlleleSupport,
};
pub use reference::{FastaOpenOptions, FastaReference, ReferenceSlice};
pub use remote::{
    HttpRangeConfig, HttpRangeReader, RemoteAuthentication, RemoteCancellation, RemoteError,
    RemoteHttpError, RemoteIoStats, associated_remote_url, read_remote_object_bounded, redact_url,
    remote_url_with_extension,
};
pub use remote_alignment::{
    ProductionAlignmentSource, RemoteBamAlignmentSource, RemoteCramAlignmentSource,
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
pub use supplementary::{
    MAX_SUPPLEMENTARY_CIGAR_OPERATIONS, MAX_SUPPLEMENTARY_SEGMENTS, SupplementaryChain,
    SupplementarySegment, decode_supplementary_chain,
};
pub use tdf::{TdfMetadata, TdfSource};
pub use tracks::{
    BigBedFeatureFormat, BigBedSource, BigWigSource, BreakendPartner, CopyNumberQuery,
    CopyNumberQueryProvenance, CopyNumberSegment, CopyNumberValueSemantics, FeatureFormat,
    FeatureRecord, FeatureSearchHit, FeatureSearchResults, GwasQuery, GwasQueryProvenance,
    GwasRecord, GwasSearchHit, GwasSearchResults, InteractionEndpoint, InteractionFormat,
    InteractionQuery, InteractionQueryProvenance, InteractionRecord, LocalVariantSource,
    MAX_COPY_NUMBER_SAMPLE_NAME_BYTES, MAX_COPY_NUMBER_SAMPLES, MAX_COPY_NUMBER_SEGMENTS_PER_QUERY,
    MAX_COPY_NUMBER_SORT_SEGMENTS, MAX_COPY_NUMBER_SOURCE_SEGMENTS, MAX_FEATURE_BLOCKS_PER_RECORD,
    MAX_FEATURE_LINE_BYTES, MAX_FEATURE_RECORDS_PER_QUERY, MAX_FEATURE_SEARCH_ASSOCIATIONS,
    MAX_FEATURE_SEARCH_RESULTS, MAX_FEATURE_SOURCE_RECORDS, MAX_GWAS_ANNOTATION_BYTES_PER_RECORD,
    MAX_GWAS_COLUMNS, MAX_GWAS_FIELD_BYTES, MAX_GWAS_LINE_BYTES, MAX_GWAS_RECORDS_PER_QUERY,
    MAX_GWAS_SEARCH_ASSOCIATIONS, MAX_GWAS_SEARCH_RESULTS, MAX_GWAS_SOURCE_ANNOTATION_BYTES,
    MAX_GWAS_SOURCE_RECORDS, MAX_INTERACTION_RECORDS_PER_QUERY,
    MAX_MUTATION_ANNOTATION_BYTES_PER_RECORD, MAX_MUTATION_COLUMNS, MAX_MUTATION_FIELD_BYTES,
    MAX_MUTATION_LINE_BYTES, MAX_MUTATION_RECORDS_PER_QUERY, MAX_MUTATION_SAMPLE_NAME_BYTES,
    MAX_MUTATION_SAMPLES, MAX_MUTATION_SOURCE_ANNOTATION_BYTES, MAX_MUTATION_SOURCE_RECORDS,
    MAX_QUANTITATIVE_RECORDS_PER_QUERY, MAX_VARIANT_ID_ASSOCIATIONS, MAX_VARIANT_ID_INDEX_RECORDS,
    MAX_VARIANT_ID_SEARCH_RESULTS, MutationAnnotation, MutationQuery, MutationQueryProvenance,
    MutationRecord, PeakMetrics, QuantitativeFormat, QuantitativeQuery,
    QuantitativeQueryProvenance, QuantitativeRecord, QuantitativeSourceMetadata, Strand,
    TdfQueryMetadata, TextCopyNumberSource, TextFeatureSource, TextGwasSource,
    TextInteractionSource, TextMutationSource, TextQuantitativeSource, TrackError, VariantField,
    VariantFieldSchema, VariantFilterSchema, VariantGenotype, VariantGenotypeAllele, VariantHeader,
    VariantIdSearchHit, VariantIdSearchResults, VariantQuery, VariantQueryProvenance,
    VariantRecord, VariantType, VariantValue, VariantValueType, variant_record_matches,
};
pub use ultima::{
    MAX_ULTIMA_VALUES_PER_RECORD, UltimaEventUnavailable, UltimaEvidenceError, UltimaIndelEvidence,
    UltimaIndelKind, UltimaQualitySource, UltimaRecordAnalysis, UltimaSummary,
    analyze_ultima_record,
};
