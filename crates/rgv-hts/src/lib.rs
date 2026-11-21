//! Alignment and reference I/O boundary.

mod backend_record;
mod coverage_sidecar;
mod coverage_stream;
mod htslib_guard;
mod igv_genome;
mod index_overview;
mod local;
mod positioned_resume;
mod reference;
mod remote;
mod remote_alignment;
mod source_revision;
mod summary_index;
mod tdf;
mod tracks;
mod two_bit;

use rgv_model::{
    alignment, bisulfite, exact_depth, filter, junction, modification_summary, modified_bases,
    pair, pileup, sbx, smrt_kinetics, source_position, supplementary, ultima,
};

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
pub use coverage_sidecar::{
    COVERAGE_SIDECAR_DEFAULT_MAXIMUM_BYTES, CoverageSidecarCache, CoverageSidecarKey,
    CoverageSidecarStats, default_coverage_cache_directory,
};
pub use coverage_stream::{CoverageTileOutcome, CoverageTileProduction, CoverageTileRequest};
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
    AlignmentBackendPreference, AlignmentDerivationMetrics, AlignmentDerivationPhase,
    AlignmentError, AlignmentOpenOptions, AlignmentQuery, AlignmentSource, IndividualReadStatus,
    LocalAlignmentSource, PositionedAlignmentBlock, PositionedAlignmentRecord,
    PositionedAlignmentStreamCheckpoint, PositionedAlignmentStreamCompletion, QueryDetail,
    QueryOptions, QueryStats, derive_alignment_query, derive_alignment_query_with_metrics,
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
    LocalBamAlignmentSource, LocalCramAlignmentSource, MAX_IN_MEMORY_ALIGNMENT_BYTES,
    ProductionAlignmentSource, RemoteBamAlignmentSource, RemoteCramAlignmentSource,
};
pub use rgv_model::coverage_pyramid::{
    COVERAGE_PYRAMID_BASE_BIN_BASES, COVERAGE_PYRAMID_LEVEL_FACTOR, COVERAGE_PYRAMID_MAX_LEVELS,
    COVERAGE_PYRAMID_TILE_BINS, CoverageAccumulation, CoverageAccumulator,
    CoverageAccumulatorTelemetry, CoverageBin, CoverageEligibility, CoveragePyramidError,
    CoveragePyramidParameters, CoverageTile, CoverageTileId, add_counted_depth_segments,
    derive_coarser_tiles,
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
pub use tdf::{TdfMetadata, TdfSource, TdfSummaryQuery};
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
    MAX_GWAS_SOURCE_RECORDS, MAX_IN_MEMORY_VARIANT_BYTES, MAX_IN_MEMORY_VARIANT_RECORDS,
    MAX_INTERACTION_RECORDS_PER_QUERY, MAX_MUTATION_ANNOTATION_BYTES_PER_RECORD,
    MAX_MUTATION_COLUMNS, MAX_MUTATION_FIELD_BYTES, MAX_MUTATION_LINE_BYTES,
    MAX_MUTATION_RECORDS_PER_QUERY, MAX_MUTATION_SAMPLE_NAME_BYTES, MAX_MUTATION_SAMPLES,
    MAX_MUTATION_SOURCE_ANNOTATION_BYTES, MAX_MUTATION_SOURCE_RECORDS,
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

    /// Parses the uncompressed content of a `.tbi` with the tabix parser written for the
    /// remote VCF and annotation paths, and queries it.
    pub fn parse_inflated_tabix_index(inflated: &[u8]) -> bool {
        crate::tracks::fuzz_inflated_tabix_index(inflated)
    }

    /// Decodes a coverage sidecar tile file (`.rgvcov`) as the sidecar cache does, with and
    /// without a recomputed checksum.
    pub fn decode_coverage_sidecar_tile(bytes: &[u8]) -> bool {
        crate::coverage_sidecar::fuzz_decode_tile(bytes)
    }

    /// Computes the tabix interval of one VCF data line as the remote VCF path does.
    pub fn variant_line_interval(line: &[u8]) -> Option<(i64, i64)> {
        crate::tracks::fuzz_variant_line_interval(line)
    }
}

/// A deterministic mutation sweep over the entry points of the `tabix_index`,
/// `coverage_sidecar` and `vcf_in_memory` fuzz targets. It is no substitute for running the
/// targets under libFuzzer; it keeps the entry points honest in an ordinary test run, where no
/// fuzzing engine is installed.
#[cfg(test)]
mod fuzzing_smoke_tests {
    use std::io::Read as _;

    /// xorshift64*: deterministic, so a failure reproduces.
    struct Mutator(u64);

    impl Mutator {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound.max(1) as u64) as usize
        }

        /// One to four edits: a flipped bit, an interesting integer, a truncation, a removed
        /// or duplicated run.
        fn mutate(&mut self, seed: &[u8]) -> Vec<u8> {
            let mut bytes = seed.to_vec();
            for _ in 0..=self.below(4) {
                if bytes.is_empty() {
                    bytes.push(self.next() as u8);
                    continue;
                }
                let at = self.below(bytes.len());
                match self.below(6) {
                    0 => bytes[at] ^= 1 << self.below(8),
                    1 => bytes[at] = self.next() as u8,
                    2 => {
                        let value = [0_u32, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff, 0x0001_0000]
                            [self.below(6)];
                        let end = (at + 4).min(bytes.len());
                        bytes[at..end].copy_from_slice(&value.to_le_bytes()[..end - at]);
                    }
                    3 => bytes.truncate(at),
                    4 => {
                        let end = (at + 1 + self.below(16)).min(bytes.len());
                        bytes.drain(at..end);
                    }
                    _ => {
                        let end = (at + 1 + self.below(16)).min(bytes.len());
                        let run = bytes[at..end].to_vec();
                        bytes.splice(at..at, run);
                    }
                }
            }
            bytes
        }
    }

    #[test]
    fn mutated_tabix_indexes_are_parsed_or_refused() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/manual-suite/data/variants/cohort.vcf.gz.tbi");
        let Ok(stored) = std::fs::read(path) else {
            return;
        };
        let mut seed = Vec::new();
        flate2::read::MultiGzDecoder::new(stored.as_slice())
            .read_to_end(&mut seed)
            .unwrap();
        assert!(crate::fuzzing::parse_inflated_tabix_index(&seed));
        assert!(crate::fuzzing::parse_variant_index(&stored));
        let mut mutator = Mutator(0x9E37_79B9_7F4A_7C15);
        let mut accepted = 0_usize;
        for _ in 0..20_000 {
            let input = mutator.mutate(&seed);
            accepted += usize::from(crate::fuzzing::parse_inflated_tabix_index(&input));
            let _ = crate::fuzzing::parse_variant_index(&input);
        }
        // Both outcomes occur: the sweep reaches past the first rejection.
        assert!(accepted > 0 && accepted < 20_000, "{accepted}");
    }

    #[test]
    fn mutated_coverage_sidecar_tiles_are_decoded_or_refused() {
        use rgv_model::{CoverageBin, CoveragePyramidParameters, CoverageTile, CoverageTileId};
        let parameters = CoveragePyramidParameters::DEFAULT;
        let id = CoverageTileId::new(rgv_core::ContigId::new(3), 0, 0);
        let contig_length = 1_000_000;
        let count = parameters.tile_bin_count(id, contig_length).unwrap();
        let bases = parameters.bin_bases(0).unwrap();
        let tile = CoverageTile::from_bins(
            id,
            parameters,
            contig_length,
            (0..count as u64)
                .map(|bin| CoverageBin {
                    depth_sum: (bin % 7) * bases,
                    minimum_depth: (bin % 7) as u32,
                    maximum_depth: (bin % 7) as u32,
                })
                .collect(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let cache = crate::CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap();
        let key = crate::CoverageSidecarKey::new(
            rgv_model::AlignmentSourceStamp::new([1; 16], [2; 16]),
            &rgv_model::CoverageEligibility::default(),
            parameters,
        );
        cache.store(key, &tile).unwrap();
        let mut stored = None;
        for entry in walk(directory.path()) {
            if entry
                .extension()
                .is_some_and(|extension| extension == "rgvcov")
            {
                stored = Some(std::fs::read(entry).unwrap());
            }
        }
        let stored = stored.expect("the cache wrote a tile file");
        // The file without its checksum is the form the resealing entry point accepts.
        let seed = &stored[..stored.len() - 32];
        assert!(crate::fuzzing::decode_coverage_sidecar_tile(seed));
        assert!(!crate::fuzzing::decode_coverage_sidecar_tile(&stored));
        let mut mutator = Mutator(0xD1B5_4A32_D192_ED03);
        let mut accepted = 0_usize;
        for _ in 0..20_000 {
            accepted += usize::from(crate::fuzzing::decode_coverage_sidecar_tile(
                &mutator.mutate(seed),
            ));
        }
        assert!(accepted > 0 && accepted < 20_000, "{accepted}");
    }

    fn walk(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn mutated_unindexed_vcf_text_is_loaded_or_refused() {
        const SEED: &[u8] = b"##fileformat=VCFv4.2\n\
            ##contig=<ID=chr1,length=1000000>\n\
            ##INFO=<ID=END,Number=1,Type=Integer,Description=\"End\">\n\
            ##INFO=<ID=AF,Number=A,Type=Float,Description=\"Frequency\">\n\
            ##FORMAT=<ID=GT,Number=1,Type=String,Description=\"Genotype\">\n\
            ##FORMAT=<ID=DP,Number=1,Type=Integer,Description=\"Depth\">\n\
            #CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\tFORMAT\ts1\ts2\n\
            chr1\t100\trs1\tA\tT\t50\tPASS\tAF=0.5\tGT:DP\t0/1:10\t1|1:20\n\
            chr1\t200\trs2\tN\t<DEL>\t.\t.\tEND=900\tGT:DP\t./.:.\t0/0:3\n\
            chr1\t300\t.\tAC\tA,ACC\t9.5\tq10\tAF=0.1,0.2\tGT\t1/2\t0/1\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.vcf");
        let mut mutator = Mutator(0xA076_1D64_78BD_642F);
        let mut opened = 0_usize;
        for round in 0..1_500 {
            let input = if round == 0 {
                SEED.to_vec()
            } else {
                mutator.mutate(SEED)
            };
            std::fs::write(&path, &input).unwrap();
            let Ok(mut source) = crate::LocalVariantSource::open(&path) else {
                continue;
            };
            opened += 1;
            let _ = source.query("chr1", 0, 1 << 28, 256);
            let _ = source.query("chr1", 100, 101, 256);
            let _ = source.search_ids("rs", 16);
        }
        assert!(opened > 0 && opened < 1_500, "{opened}");
    }
}
