//! Alignment and reference I/O boundary.

mod alignment;
mod backend_record;
mod bisulfite;
mod exact_depth;
mod filter;
mod igv_genome;
mod index_overview;
mod junction;
mod local;
mod modification_summary;
mod modified_bases;
mod pair;
mod pileup;
mod positioned_resume;
mod reference;
mod remote;
mod remote_alignment;
mod sbx;
mod smrt_kinetics;
mod source_position;
mod source_revision;
mod summary_index;
mod supplementary;
mod tdf;
mod tracks;
mod two_bit;
mod ultima;

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
pub use exact_depth::{ExactDepthRun, ExactDepthTile, ExactDepthTileBuilder};
pub use filter::{AlignmentFilter, AuxComparison, AuxPredicate, FilterParseError, FilterValue};
pub use igv_genome::{
    IgvGenomeImport, IgvGenomeLoadError, load_hosted_igv_genome, load_igv_genome,
    load_igv_genome_list,
};
pub use index_overview::{ALIGNMENT_INDEX_OVERVIEW_BINS, AlignmentIndexOverview};
pub use junction::{
    JunctionEvidence, JunctionObservation, JunctionProvenance, JunctionStrand, JunctionStrandInput,
    JunctionSummary, JunctionSummaryBuilder, MAX_JUNCTIONS, MAX_JUNCTIONS_PER_RECORD,
    RecordJunctionCollector,
};
pub use local::{
    AlignmentDerivationMetrics, AlignmentDerivationPhase, AlignmentError, AlignmentOpenOptions,
    AlignmentQuery, AlignmentSource, IndividualReadStatus, LocalAlignmentSource,
    PositionedAlignmentBlock, PositionedAlignmentRecord, PositionedAlignmentStreamCheckpoint,
    PositionedAlignmentStreamCompletion, QueryDetail, QueryOptions, QueryStats,
    derive_alignment_query, derive_alignment_query_with_metrics,
    derive_alignment_query_with_metrics_and_phase,
    derive_alignment_query_with_summary_index_and_phase,
};
pub use modification_summary::{
    MAX_MODIFICATION_SERIES, ModificationBin, ModificationProvenance, ModificationSeries,
    ModificationSeriesKey, ModificationSummary, ModificationSummaryBuilder,
};
pub use modified_bases::{
    BaseModificationStrand, ImplicitModificationMode, ModificationCode, ModifiedBaseCall,
    ModifiedBaseError, aligned_position_for_sequencer_position, decode_modified_bases,
    decode_modified_bases_parts, decode_modified_bases_parts_checked, modified_bases_resolvable,
    sequencer_base_matches,
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
pub use positioned_resume::PositionedChunkReceipt;
pub use reference::{FastaOpenOptions, FastaReference, ReferenceSlice};
pub use remote::{
    HttpRangeConfig, HttpRangeReader, RemoteAuthentication, RemoteCancellation, RemoteError,
    RemoteHttpError, RemoteIoStats, RemoteTransferBudget, associated_remote_url,
    read_remote_object_bounded, redact_url, remote_url_with_extension,
};
pub use remote_alignment::{
    LocalCramAlignmentSource, ProductionAlignmentSource, RemoteBamAlignmentSource,
    RemoteCramAlignmentSource,
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
pub use summary_index::{ALIGNMENT_SUMMARY_TILE_BASES, AlignmentSummaryIndex};
pub use supplementary::{
    MAX_SUPPLEMENTARY_CIGAR_OPERATIONS, MAX_SUPPLEMENTARY_SEGMENTS, SupplementaryChain,
    SupplementarySegment, decode_supplementary_chain,
};
pub use tdf::{TdfMetadata, TdfSource};
pub use tracks::{
    BigBedFeatureFormat, BigBedSource, BigWigSource, BreakendPartner, BreakpointOrientation,
    CopyNumberQuery, CopyNumberQueryProvenance, CopyNumberSegment, CopyNumberValueSemantics,
    FeatureFormat, FeaturePresentationIdentity, FeatureRecord, FeatureSearchHit,
    FeatureSearchResults, GwasQuery, GwasQueryProvenance, GwasRecord, GwasSearchHit,
    GwasSearchResults, InteractionEndpoint, InteractionFormat, InteractionQuery,
    InteractionQueryProvenance, InteractionRecord, LocalVariantSource,
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
    MAX_QUANTITATIVE_RECORDS_PER_QUERY, MAX_REMOTE_VARIANT_ID_SCAN_BYTES,
    MAX_VARIANT_ID_ASSOCIATIONS, MAX_VARIANT_ID_INDEX_RECORDS, MAX_VARIANT_ID_SEARCH_RESULTS,
    MutationAnnotation, MutationQuery, MutationQueryProvenance, MutationRecord, PeakMetrics,
    QuantitativeFormat, QuantitativeQuery, QuantitativeQueryProvenance, QuantitativeRecord,
    QuantitativeSourceMetadata, QuantitativeSummaryQuery, QuantitativeSummaryRecord, Strand,
    TdfQueryMetadata, TextCopyNumberSource, TextFeatureSource, TextGwasSource,
    TextInteractionSource, TextMutationSource, TextQuantitativeSource, TrackError, VariantField,
    VariantFieldSchema, VariantFilterSchema, VariantGenotype, VariantGenotypeAllele, VariantHeader,
    VariantIdSearchHit, VariantIdSearchResults, VariantQuery, VariantQueryProvenance,
    VariantRecord, VariantType, VariantValue, VariantValueType, variant_record_matches,
};
pub use ultima::{
    MAX_ULTIMA_VALUES_PER_RECORD, UltimaEventUnavailable, UltimaEvidenceError, UltimaIndelEvidence,
    UltimaIndelKind, UltimaQualitySource, UltimaRecordAnalysis, UltimaSummary,
    analyze_ultima_record, analyze_ultima_record_with_tag_orientation,
};

/// Entry points for the fuzz targets in `fuzz/`: RGV's own parsers of untrusted index bytes,
/// which are otherwise reachable only through a network fetch. Not a supported API.
#[doc(hidden)]
pub mod fuzzing {
    /// Parses a gzip-compressed CRAI as the remote CRAM path does, against a CRAM of
    /// `source_length` bytes.
    pub fn parse_cram_index(compressed: &[u8], source_length: u64) -> bool {
        crate::remote_alignment::parse_cram_index(compressed, source_length).is_ok()
    }

    /// Parses a `.fai` as the remote FASTA path does.
    pub fn parse_fasta_index(bytes: &[u8]) -> bool {
        crate::reference::parse_fasta_index(bytes).is_ok()
    }

    /// Parses a bgzip-compressed `.tbi` or `.csi` as the remote VCF path does, and queries it.
    pub fn parse_variant_index(bytes: &[u8]) -> bool {
        crate::tracks::fuzz_variant_index(bytes)
    }

    /// Computes the tabix interval of one VCF data line as the remote VCF path does.
    pub fn variant_line_interval(line: &[u8]) -> Option<(i64, i64)> {
        crate::tracks::fuzz_variant_line_interval(line)
    }
}
