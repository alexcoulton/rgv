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

    // ---------------------------------------------------------------------------------------
    // Field-corruption sweep over the `tabix_index`, `coverage_sidecar`, `fai`, `crai` and
    // `two_bit` fuzz targets.
    //
    // Every call runs under `catch_unwind`, so a panic is reported with the parser, the seed,
    // the iteration and the hex-encoded input. Allocation is not measured directly: a counting
    // allocator needs a `GlobalAlloc` implementation, which is `unsafe` and therefore forbidden
    // in this workspace. Instead each sweep asserts that what a parser returns is bounded by
    // its input (entries no more numerous than input bytes, sequences no longer than the bases
    // asked for) and that no single call exceeds `CALL_TIME_LIMIT` of wall-clock time, which
    // is what a multi-gigabyte reservation or a count-driven loop would cost.
    // ---------------------------------------------------------------------------------------

    /// Generous: the test binary shares the machine with other tests and builds.
    const CALL_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);
    /// Mutations per parser in the field-corruption sweeps.
    const FIELD_SWEEP_ROUNDS: usize = 4_000;

    const INTERESTING_U32: [u32; 11] = [
        0,
        1,
        2,
        0xff,
        0xffff,
        0x0001_0000,
        0x00ff_ffff,
        0x7fff_ffff,
        // `i32::MIN`.
        0x8000_0000,
        0xffff_fffe,
        0xffff_ffff,
    ];
    const INTERESTING_U64: [u64; 9] = [
        0,
        1,
        0x7fff_ffff,
        0xffff_ffff,
        0x0001_0000_0000,
        // `i64::MAX` and `i64::MIN`.
        0x7fff_ffff_ffff_ffff,
        0x8000_0000_0000_0000,
        u64::MAX - 1,
        u64::MAX,
    ];
    /// Replacements for a decimal field of a text index (`.fai`, `.crai`).
    const INTERESTING_DECIMAL: [&str; 12] = [
        "",
        "0",
        "1",
        "-1",
        "2147483647",
        "-2147483648",
        "4294967295",
        "4294967296",
        "9223372036854775807",
        "18446744073709551615",
        "18446744073709551616",
        "00000000000000000000000000000000000000001",
    ];

    fn overwrite(bytes: &mut [u8], at: usize, value: &[u8]) {
        let end = (at + value.len()).min(bytes.len());
        bytes[at..end].copy_from_slice(&value[..end - at]);
    }

    impl Mutator {
        /// One to three edits weighted towards length and count fields: a flipped bit, a
        /// truncation, a 4- or 8-byte little-endian field overwritten with a boundary value
        /// (at any offset and at a 4-byte-aligned one), a decimal field replaced with a
        /// boundary value, or one round of [`Mutator::mutate`].
        fn corrupt(&mut self, seed: &[u8]) -> Vec<u8> {
            let mut bytes = seed.to_vec();
            for _ in 0..=self.below(3) {
                if bytes.is_empty() {
                    break;
                }
                let at = self.below(bytes.len());
                match self.below(8) {
                    0 => bytes[at] ^= 1 << self.below(8),
                    1 => bytes.truncate(at),
                    2 | 3 => {
                        let value = INTERESTING_U32[self.below(INTERESTING_U32.len())];
                        let at = if self.below(2) == 0 { at } else { at & !3 };
                        overwrite(&mut bytes, at, &value.to_le_bytes());
                    }
                    4 | 5 => {
                        let value = INTERESTING_U64[self.below(INTERESTING_U64.len())];
                        let at = if self.below(2) == 0 { at } else { at & !3 };
                        overwrite(&mut bytes, at, &value.to_le_bytes());
                    }
                    6 => self.replace_decimal_field(&mut bytes, at),
                    _ => bytes = self.mutate(&bytes),
                }
            }
            bytes
        }

        /// Replaces one run of ASCII digits (with its sign) by a boundary value; flips a bit
        /// at `fallback` when there is none.
        fn replace_decimal_field(&mut self, bytes: &mut Vec<u8>, fallback: usize) {
            let mut fields = Vec::new();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index].is_ascii_digit() {
                    let start = if index > 0 && bytes[index - 1] == b'-' {
                        index - 1
                    } else {
                        index
                    };
                    while index < bytes.len() && bytes[index].is_ascii_digit() {
                        index += 1;
                    }
                    fields.push((start, index));
                } else {
                    index += 1;
                }
            }
            if fields.is_empty() {
                bytes[fallback] ^= 1 << self.below(8);
                return;
            }
            let (start, end) = fields[self.below(fields.len())];
            let value = INTERESTING_DECIMAL[self.below(INTERESTING_DECIMAL.len())];
            bytes.splice(start..end, value.bytes());
        }
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    /// Where a sweep is: enough to reproduce the input without the hex.
    #[derive(Clone, Copy)]
    struct SweepCase<'a> {
        parser: &'static str,
        seed: u64,
        iteration: usize,
        input: &'a [u8],
    }

    impl SweepCase<'_> {
        /// Runs one parser call, failing with the offending input if it panics or takes
        /// longer than [`CALL_TIME_LIMIT`].
        fn run<T>(self, call: impl FnOnce() -> T) -> T {
            let started = std::time::Instant::now();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(call));
            let elapsed = started.elapsed();
            let Self {
                parser,
                seed,
                iteration,
                input,
            } = self;
            match outcome {
                Ok(value) => {
                    assert!(
                        elapsed < CALL_TIME_LIMIT,
                        "{parser} took {elapsed:?} (seed {seed:#x}, iteration {iteration}) on \
                         input {}",
                        hex(input)
                    );
                    value
                }
                Err(payload) => {
                    let message = payload
                        .downcast_ref::<&str>()
                        .map(|message| (*message).to_owned())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic".to_owned());
                    panic!(
                        "{parser} panicked with '{message}' (seed {seed:#x}, iteration \
                         {iteration}) on input {}",
                        hex(input)
                    );
                }
            }
        }
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// An uncompressed two-sequence tabix index built for a VCF: one data bin and the
    /// metadata pseudo-bin per sequence, and a two-entry linear index.
    fn synthetic_tabix_index() -> Vec<u8> {
        let mut index = b"TBI\x01".to_vec();
        let names = b"chr1\0chr2\0";
        // n_ref, format (VCF), col_seq, col_beg, col_end, meta ('#'), skip, l_nm.
        let names_length = i32::try_from(names.len()).unwrap();
        for value in [2_i32, 2, 1, 2, 0, 35, 0, names_length] {
            index.extend(value.to_le_bytes());
        }
        index.extend(names);
        for sequence in 0..2_u64 {
            index.extend(2_i32.to_le_bytes());
            index.extend(4_681_u32.to_le_bytes());
            index.extend(1_i32.to_le_bytes());
            index.extend(((sequence * 0x100) << 16).to_le_bytes());
            index.extend(((sequence * 0x100 + 0x80) << 16).to_le_bytes());
            index.extend(37_450_u32.to_le_bytes());
            index.extend(2_i32.to_le_bytes());
            for value in [0_u64, 0x8000, 3, 0] {
                index.extend(value.to_le_bytes());
            }
            index.extend(2_i32.to_le_bytes());
            index.extend(((sequence * 0x100) << 16).to_le_bytes());
            index.extend(((sequence * 0x100 + 0x40) << 16).to_le_bytes());
        }
        index
    }

    /// The tabix parser checks every count against the bytes that remain before reserving
    /// (`IndexBytes::count`) and returns only whether the index parsed, so its bound here is
    /// the per-call time limit; the entry point itself asserts that the parsed sequence count
    /// equals the number of names, which the input's length bounds.
    #[test]
    fn mutated_tabix_index_fields_are_parsed_or_refused_within_bounds() {
        const SEED: u64 = 0x2545_F491_4F6C_DD1D;
        let mut seeds = vec![synthetic_tabix_index()];
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/manual-suite/data/variants/cohort.vcf.gz.tbi");
        if let Ok(stored) = std::fs::read(path) {
            let mut inflated = Vec::new();
            flate2::read::MultiGzDecoder::new(stored.as_slice())
                .read_to_end(&mut inflated)
                .unwrap();
            seeds.push(inflated);
        }
        for seed in &seeds {
            assert!(crate::fuzzing::parse_inflated_tabix_index(seed));
            assert!(crate::fuzzing::parse_variant_index(&gzip(seed)));
        }
        let mut mutator = Mutator(SEED);
        let (mut accepted, mut framed) = (0_usize, 0_usize);
        for iteration in 0..FIELD_SWEEP_ROUNDS {
            let input = mutator.corrupt(&seeds[iteration % seeds.len()]);
            let case = SweepCase {
                parser: "tabix index",
                seed: SEED,
                iteration,
                input: &input,
            };
            // As the fuzz target does: raw, raw behind the magic, and each gzip-framed.
            accepted +=
                usize::from(case.run(|| crate::fuzzing::parse_inflated_tabix_index(&input)));
            let mut prefixed = b"TBI\x01".to_vec();
            prefixed.extend_from_slice(&input);
            case.run(|| crate::fuzzing::parse_inflated_tabix_index(&prefixed));
            case.run(|| crate::fuzzing::parse_variant_index(&input));
            framed +=
                usize::from(case.run(|| crate::fuzzing::parse_variant_index(&gzip(&input))));
            case.run(|| crate::fuzzing::parse_variant_index(&gzip(&prefixed)));
        }
        assert!(accepted > 0 && accepted < FIELD_SWEEP_ROUNDS, "{accepted}");
        assert!(framed > 0 && framed < FIELD_SWEEP_ROUNDS, "{framed}");
    }

    /// A stored tile of a short contig, without its checksum: small, so that most edits land
    /// on the header and the bin count rather than on bin payload.
    fn small_sidecar_tile() -> Vec<u8> {
        use rgv_model::{CoverageBin, CoveragePyramidParameters, CoverageTile, CoverageTileId};
        let parameters = CoveragePyramidParameters::DEFAULT;
        let id = CoverageTileId::new(rgv_core::ContigId::new(5), 0, 0);
        let bases = parameters.bin_bases(0).unwrap();
        let contig_length = bases * 6 + 1;
        let count = parameters.tile_bin_count(id, contig_length).unwrap();
        let tile = CoverageTile::from_bins(
            id,
            parameters,
            contig_length,
            (0..count as u64)
                .map(|bin| CoverageBin {
                    depth_sum: (bin % 3) * bases.min(contig_length - bin * bases),
                    minimum_depth: (bin % 3) as u32,
                    maximum_depth: (bin % 3) as u32,
                })
                .collect(),
        )
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let cache = crate::CoverageSidecarCache::open(directory.path(), 1 << 30).unwrap();
        let key = crate::CoverageSidecarKey::new(
            rgv_model::AlignmentSourceStamp::new([3; 16], [4; 16]),
            &rgv_model::CoverageEligibility::default(),
            parameters,
        );
        cache.store(key, &tile).unwrap();
        let stored = walk(directory.path())
            .into_iter()
            .find(|entry| {
                entry
                    .extension()
                    .is_some_and(|extension| extension == "rgvcov")
            })
            .map(|entry| std::fs::read(entry).unwrap())
            .expect("the cache wrote a tile file");
        stored[..stored.len() - 32].to_vec()
    }

    /// The entry point asserts that a tile which decodes encodes back to exactly the input,
    /// so a decoded tile is never larger than its file; the bin count is checked against the
    /// remaining bytes before the bins are reserved.
    #[test]
    fn mutated_coverage_sidecar_fields_are_decoded_or_refused_within_bounds() {
        const SEED: u64 = 0x94D0_49BB_1331_11EB;
        let seed = small_sidecar_tile();
        assert!(crate::fuzzing::decode_coverage_sidecar_tile(&seed));
        let mut mutator = Mutator(SEED);
        let mut accepted = 0_usize;
        for iteration in 0..FIELD_SWEEP_ROUNDS {
            let input = mutator.corrupt(&seed);
            let case = SweepCase {
                parser: "coverage sidecar tile",
                seed: SEED,
                iteration,
                input: &input,
            };
            accepted +=
                usize::from(case.run(|| crate::fuzzing::decode_coverage_sidecar_tile(&input)));
        }
        assert!(accepted > 0 && accepted < FIELD_SWEEP_ROUNDS, "{accepted}");
    }

    /// Contigs fetched per input, and bases fetched per contig, whatever the index declares
    /// (the limits of the `fai` and `two_bit` fuzz targets).
    const MAXIMUM_FETCHED_CONTIGS: usize = 4;
    const MAXIMUM_FETCHED_BASES: u64 = 4_096;

    /// Fetches the windows the `fai` and `two_bit` fuzz targets fetch (the start and the end
    /// of each declared length) and two interior ones, asserting that no fetch returns more
    /// than it was asked for. Returns the number of fetches that succeeded.
    fn bounded_fetches(case: SweepCase<'_>, reference: &crate::FastaReference) -> usize {
        let lengths = reference
            .reference_dictionary()
            .iter()
            .map(rgv_core::Contig::length)
            .take(MAXIMUM_FETCHED_CONTIGS)
            .collect::<Vec<_>>();
        let mut fetched = 0;
        for (index, length) in lengths.into_iter().enumerate() {
            let windows = [
                (0, length.clamp(1, MAXIMUM_FETCHED_BASES)),
                (length.saturating_sub(MAXIMUM_FETCHED_BASES), length.max(1)),
                (length / 2, (length / 2 + MAXIMUM_FETCHED_BASES).min(length)),
                (length - length / 4, length),
            ];
            for (start, end) in windows {
                let Ok(interval) = rgv_core::GenomicInterval::new(
                    rgv_core::ContigId::new(index as u32),
                    start,
                    end,
                ) else {
                    continue;
                };
                if let Ok(bases) = case.run(|| reference.fetch(interval)) {
                    assert!(
                        bases.len() as u64 <= end - start,
                        "{} returned {} bases for {start}-{end} (seed {:#x}, iteration {}) \
                         on input {}",
                        case.parser,
                        bases.len(),
                        case.seed,
                        case.iteration,
                        hex(case.input)
                    );
                    fetched += 1;
                }
            }
        }
        fetched
    }

    #[test]
    fn mutated_fasta_indexes_are_parsed_or_refused_within_bounds() {
        const SEED: u64 = 0xBF58_476D_1CE4_E5B9;
        /// The FASTA of the `fai` fuzz target and the index `samtools faidx` writes for it.
        const FASTA: &[u8] =
            b">chr1 first\nACGTACGTACGTACGTACGT\nACGTACGTAC\n>chr2\nNNNNNNNNNN\nacgtacgt\n";
        const INDEX: &[u8] = b"chr1\t30\t12\t20\t21\nchr2\t18\t50\t10\t11\n";
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reference.fa");
        let index_path = directory.path().join("reference.fa.fai");
        std::fs::write(&path, FASTA).unwrap();
        let mut mutator = Mutator(SEED);
        let (mut parsed, mut opened, mut fetched) = (0_usize, 0_usize, 0_usize);
        for iteration in 0..FIELD_SWEEP_ROUNDS {
            let input = if iteration == 0 {
                INDEX.to_vec()
            } else {
                mutator.corrupt(INDEX)
            };
            let case = SweepCase {
                parser: "FASTA index",
                seed: SEED,
                iteration,
                input: &input,
            };
            // RGV's own `.fai` parser (the remote FASTA path).
            assert_eq!(
                case.run(|| crate::fuzzing::parse_fasta_index(&input)),
                match case.run(|| crate::reference::parse_fasta_index(&input)) {
                    Ok(entries) => {
                        // Every entry is a line of at least nine bytes.
                        assert!(entries.len() <= input.len(), "{}", hex(&input));
                        parsed += 1;
                        true
                    }
                    Err(_) => false,
                }
            );
            // The same bytes as the index of a local FASTA (`HTSlib`'s reader).
            std::fs::write(&index_path, &input).unwrap();
            let Ok(reference) = case.run(|| crate::FastaReference::open(&path)) else {
                continue;
            };
            assert!(
                reference.reference_dictionary().len() <= input.len(),
                "{}",
                hex(&input)
            );
            opened += 1;
            let succeeded = bounded_fetches(case, &reference);
            if iteration == 0 {
                assert_eq!(succeeded, 8, "the intact index serves every window");
            }
            fetched += succeeded;
        }
        assert!(parsed > 0 && parsed < FIELD_SWEEP_ROUNDS, "{parsed}");
        assert!(opened > 0 && opened < FIELD_SWEEP_ROUNDS, "{opened}");
        assert!(fetched > 0, "{fetched}");
    }

    #[test]
    fn mutated_cram_indexes_are_parsed_or_refused_within_bounds() {
        const SEED: u64 = 0xC2B2_AE3D_27D4_EB4F;
        /// CRAM object lengths of the `crai` fuzz target: one that holds any offset the seed
        /// names, and a tiny one that makes the "points outside the CRAM" checks reachable.
        const SOURCE_LENGTHS: [u64; 2] = [1 << 40, 4_096];
        const TEXT: &[u8] = b"0\t1\t1000\t100\t20\t500\n0\t1001\t1000\t700\t20\t400\n\
            1\t1\t500\t1200\t25\t300\n-1\t0\t0\t1600\t20\t200\n";
        let stored = gzip(TEXT);
        for source_length in SOURCE_LENGTHS {
            assert!(crate::fuzzing::parse_cram_index(&stored, source_length));
        }
        let mut mutator = Mutator(SEED);
        let mut accepted = [0_usize; 2];
        // Twice the rounds: the index text (framed here, so edits reach the line parser) and
        // the stored gzip bytes (so edits reach the framing and its rejection).
        for iteration in 0..2 * FIELD_SWEEP_ROUNDS {
            let mutate_text = iteration % 2 == 0;
            let input = mutator.corrupt(if mutate_text { TEXT } else { &stored });
            let case = SweepCase {
                parser: "CRAM index",
                seed: SEED,
                iteration,
                input: &input,
            };
            let framed = gzip(&input);
            for source_length in SOURCE_LENGTHS {
                for (bytes, text_length) in [(&input, None), (&framed, Some(input.len()))] {
                    let Ok(index) = case.run(|| {
                        crate::remote_alignment::parse_cram_index(bytes, source_length)
                    }) else {
                        continue;
                    };
                    // A record is a line of at least eleven bytes. Input that is itself
                    // gzip may inflate, so only the parser's own record limit bounds it.
                    assert!(
                        index.len() <= text_length.unwrap_or(1_000_000),
                        "{} records from {}",
                        index.len(),
                        hex(&input)
                    );
                    accepted[usize::from(text_length.is_none())] += 1;
                }
            }
        }
        // Both forms reach past the first rejection, and neither is always accepted.
        for count in accepted {
            assert!(count > 0 && count < 2 * FIELD_SWEEP_ROUNDS, "{accepted:?}");
        }
    }

    #[test]
    fn mutated_two_bit_files_are_opened_or_refused_within_bounds() {
        const SEED: u64 = 0x1656_67B1_9E37_79F9;
        // Several unknown and soft-masked runs, and a second sequence, so the run tables and
        // the sequence index are all there to corrupt. Both byte orders are swept.
        let sequences = [
            ("chrA", "NANACNACGTACGTACnnnnACGTacgtACGTNNACGTTTGACCAGATTACAGATTACAnnC"),
            ("chrB", "GGGCCCTTTAAACnnGATTACA"),
        ];
        let seeds = [false, true].map(|big_endian| {
            crate::two_bit::tests::encode_two_bit(&sequences, big_endian)
        });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.2bit");
        let soft_masked = crate::FastaOpenOptions {
            two_bit_soft_masking: true,
            ..crate::FastaOpenOptions::default()
        };
        let mut mutator = Mutator(SEED);
        let (mut opened, mut fetched) = (0_usize, 0_usize);
        for iteration in 0..FIELD_SWEEP_ROUNDS {
            let seed = &seeds[iteration % 2];
            let input = if iteration < 2 {
                seed.clone()
            } else {
                mutator.corrupt(seed)
            };
            let case = SweepCase {
                parser: "2bit",
                seed: SEED,
                iteration,
                input: &input,
            };
            std::fs::write(&path, &input).unwrap();
            // Header, sequence index and every sequence record's length are parsed on open;
            // the run tables on the first fetch. Masked and unmasked: the mask tables are
            // read only when soft-masking is reported.
            for options in [&crate::FastaOpenOptions::default(), &soft_masked] {
                let Ok(reference) =
                    case.run(|| crate::FastaReference::open_with_options(&path, options))
                else {
                    continue;
                };
                // Every indexed sequence is at least six bytes of the file.
                assert!(
                    reference.reference_dictionary().len() <= input.len(),
                    "{}",
                    hex(&input)
                );
                opened += 1;
                let succeeded = bounded_fetches(case, &reference);
                if iteration < 2 {
                    assert_eq!(succeeded, 8, "the intact file serves every window");
                }
                fetched += succeeded;
            }
        }
        assert!(opened > 0 && opened < 2 * FIELD_SWEEP_ROUNDS, "{opened}");
        assert!(fetched > 0, "{fetched}");
    }
}
