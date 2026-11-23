#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeatureFormat {
    Bed,
    Gff3,
    Gtf,
    GenePred,
    RefFlat,
    NarrowPeak,
    BroadPeak,
    Psl,
}

/// The schema carried by the non-coordinate columns of an indexed `BigBed` file.
///
/// A plain `BigBed` remains deliberately permissive because `BigBed` can contain any
/// BED3+ schema. The two typed variants require the canonical UCSC/ENCODE field
/// counts and preserve their analytical fields in [`FeatureRecord`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BigBedFeatureFormat {
    #[default]
    Bed,
    BigGenePred,
    BigNarrowPeak,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuantitativeFormat {
    BedGraph,
    Wig,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractionFormat {
    BedPe,
    Interact,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CopyNumberValueSemantics {
    #[default]
    LogRatio,
    Absolute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Strand {
    Forward,
    Reverse,
    Unstranded,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FeatureRecord {
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub name: Option<String>,
    pub feature_type: Option<String>,
    pub score: Option<f32>,
    pub strand: Strand,
    pub blocks: Vec<(u64, u64)>,
    /// Zero-based, half-open coding interval. Transcript blocks outside this interval are UTRs.
    pub coding_interval: Option<(u64, u64)>,
    /// Typed ENCODE narrowPeak measurements and absolute summit position.
    pub peak: Option<PeakMetrics>,
    pub attributes: Vec<(String, String)>,
}

/// Presentation-oriented feature identity resolved without changing the
/// record's stable source identifiers or genomic coordinates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeaturePresentationIdentity<'a> {
    pub display_symbol: Option<&'a str>,
    pub gene_id: Option<&'a str>,
    pub transcript_id: Option<&'a str>,
    pub record_name: Option<&'a str>,
}

impl<'a> FeaturePresentationIdentity<'a> {
    /// Human-readable label used by tracks, search results, and inspectors.
    #[must_use]
    pub fn label(self) -> Option<&'a str> {
        self.display_symbol
            .or(self.record_name)
            .or(self.gene_id)
            .or(self.transcript_id)
    }
}

impl FeatureRecord {
    #[must_use]
    pub fn presentation_identity(&self) -> FeaturePresentationIdentity<'_> {
        let attribute = |keys: &[&str]| {
            self.attributes.iter().find_map(|(key, value)| {
                (keys
                    .iter()
                    .any(|candidate| key.eq_ignore_ascii_case(candidate))
                    && !value.trim().is_empty()
                    && value != ".")
                    .then_some(value.as_str())
            })
        };
        FeaturePresentationIdentity {
            display_symbol: attribute(&[
                "gene_name",
                "gene_name2",
                "gene_symbol",
                "symbol",
                "Name",
            ]),
            gene_id: attribute(&["gene_id"]),
            transcript_id: attribute(&["transcript_id"]),
            record_name: self
                .name
                .as_deref()
                .filter(|value| !value.trim().is_empty() && *value != "."),
        }
    }

    #[must_use]
    pub fn display_label(&self) -> Option<&str> {
        self.presentation_identity().label()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakMetrics {
    pub signal_value: f32,
    pub p_value: Option<f32>,
    pub q_value: Option<f32>,
    pub summit: Option<u64>,
}

pub const MAX_FEATURE_BLOCKS_PER_RECORD: usize = 100_000;
pub const MAX_FEATURE_SOURCE_RECORDS: usize = 2_000_000;
pub const MAX_FEATURE_RECORDS_PER_QUERY: usize = 250_000;
pub const MAX_FEATURE_LINE_BYTES: usize = 1_048_576;
pub const MAX_FEATURE_SEARCH_ASSOCIATIONS: usize = 1_000_000;
pub const MAX_FEATURE_SEARCH_RESULTS: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct FeatureSearchHit {
    pub matched_term: String,
    pub record: FeatureRecord,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureSearchResults {
    pub hits: Vec<FeatureSearchHit>,
    pub index_truncated: bool,
    pub scan_truncated: bool,
}

/// How much of an annotation source its name-search index covers.
///
/// Names are indexed once per gene and per transcript: exon, CDS, UTR and codon rows add no
/// term their gene or transcript row already carries, so the index grows with the number of
/// genes and transcripts, not with the number of rows. When it still reaches
/// [`MAX_FEATURE_SEARCH_ASSOCIATIONS`] (or a remote scan its transfer limit) it is truncated,
/// and `complete_through` names the last contig whose names were all indexed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FeatureSearchIndexCoverage {
    /// Term-to-feature associations held by the index.
    pub associations: usize,
    /// Whether the index stopped before the whole source was read.
    pub truncated: bool,
    /// For a truncated index, the last contig whose names were all indexed; `None` when the
    /// index stopped within the first contig it read.
    pub complete_through: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantRecord {
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub id: Option<String>,
    pub reference: Vec<u8>,
    pub alternates: Vec<Vec<u8>>,
    pub quality: Option<f32>,
    pub filters: Vec<String>,
    pub info: Vec<VariantField>,
    pub genotypes: Vec<VariantGenotype>,
    pub variant_types: Vec<VariantType>,
    pub breakends: Vec<BreakendPartner>,
    /// Breakpoint join orientations declared by symbolic-SV `STRANDS` INFO values.
    pub breakpoint_orientations: Vec<BreakpointOrientation>,
}

pub const MAX_VARIANT_ID_INDEX_RECORDS: usize = 2_000_000;
pub const MAX_VARIANT_ID_ASSOCIATIONS: usize = 1_000_000;
pub const MAX_VARIANT_ID_SEARCH_RESULTS: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantIdSearchHit {
    pub matched_id: String,
    pub id: String,
    pub contig: String,
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VariantIdSearchResults {
    pub hits: Vec<VariantIdSearchHit>,
    pub index_truncated: bool,
    pub scan_truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VariantValueType {
    Flag,
    Integer,
    Float,
    String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VariantValue {
    Flag(bool),
    Integers(Vec<Option<i32>>),
    Floats(Vec<Option<f32>>),
    Strings(Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantField {
    pub key: String,
    pub value: VariantValue,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantFieldSchema {
    pub id: String,
    pub number: String,
    pub value_type: VariantValueType,
    pub description: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantFilterSchema {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VariantHeader {
    pub samples: Vec<String>,
    pub filters: Vec<VariantFilterSchema>,
    pub info: Vec<VariantFieldSchema>,
    pub formats: Vec<VariantFieldSchema>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VariantGenotypeAllele {
    pub index: Option<u32>,
    /// Whether the separator before this allele is phased. Always false for the first allele.
    pub phased: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VariantGenotype {
    pub sample_index: usize,
    pub alleles: Vec<VariantGenotypeAllele>,
    pub fields: Vec<VariantField>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VariantType {
    Snv,
    Mnv,
    Insertion,
    Deletion,
    Complex,
    StructuralDeletion,
    StructuralInsertion,
    Inversion,
    Duplication,
    CopyNumber,
    Breakend,
    SpanningDeletion,
    Symbolic(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BreakendPartner {
    pub alternate_index: usize,
    pub contig: String,
    /// Zero-based partner coordinate.
    pub position: u64,
    pub local_join_after_position: bool,
    pub partner_join_after_position: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BreakpointOrientation {
    pub local_join_after_position: bool,
    pub partner_join_after_position: bool,
    pub support: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VariantQueryProvenance {
    pub records_truncated: bool,
    pub records_examined: usize,
    pub records_filtered: usize,
    pub total_samples: usize,
    pub header_samples_omitted: usize,
    pub selected_samples: usize,
    pub samples_omitted_by_subset: usize,
    pub genotype_cells_loaded: usize,
    pub genotype_cells_omitted: usize,
    pub info_fields_truncated: usize,
    pub format_fields_truncated: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VariantQuery {
    pub header: VariantHeader,
    pub records: Vec<VariantRecord>,
    pub provenance: VariantQueryProvenance,
}

pub const MAX_VARIANT_INFO_FIELDS: usize = 256;
pub const MAX_VARIANT_FORMAT_FIELDS: usize = 32;
pub const MAX_VARIANT_VALUES_PER_FIELD: usize = 256;
pub const MAX_VARIANT_STRING_BYTES: usize = 4_096;
pub const MAX_VARIANT_HEADER_SAMPLES: usize = 100_000;
pub const MAX_GENOTYPE_SAMPLES_PER_RECORD: usize = 10_000;
pub const MAX_VARIANT_GENOTYPE_CELLS_PER_QUERY: usize = 250_000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantitativeRecord {
    pub start: u64,
    pub end: u64,
    pub value: f32,
}

pub const MAX_QUANTITATIVE_RECORDS_PER_QUERY: usize = 250_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QuantitativeQueryProvenance {
    pub records_examined: usize,
    pub records_retained: usize,
    pub non_finite_records: usize,
    pub records_truncated: bool,
    /// Bases per record when the source's precomputed zoom summaries were read instead of its
    /// raw values; each record then holds the mean over the bases it covers.
    pub summary_reduction_bases: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuantitativeQuery {
    pub records: Vec<QuantitativeRecord>,
    pub provenance: QuantitativeQueryProvenance,
    pub source_metadata: Option<QuantitativeSourceMetadata>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuantitativeSourceMetadata {
    Tdf(TdfQueryMetadata),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TdfQueryMetadata {
    pub version: u32,
    pub genome_id: Option<String>,
    pub data_type: String,
    pub selected_track_name: String,
    pub selected_track_index: usize,
    pub track_count: usize,
    pub dataset: String,
    pub zoom: Option<u32>,
    pub window_function: String,
    pub compressed: bool,
}

/// One association from an IGV-compatible GWAS summary file. `position` is
/// zero-based internally; the source BP value and p-value spelling are retained
/// in `fields` for exact inspection.
#[derive(Clone, Debug, PartialEq)]
pub struct GwasRecord {
    pub contig: String,
    pub position: u64,
    pub identifier: String,
    pub p_value: f64,
    pub minus_log10_p: f32,
    pub fields: Vec<(String, String)>,
}

pub const MAX_GWAS_SOURCE_RECORDS: usize = 5_000_000;
pub const MAX_GWAS_RECORDS_PER_QUERY: usize = 250_000;
pub const MAX_GWAS_COLUMNS: usize = 256;
pub const MAX_GWAS_FIELD_BYTES: usize = 4_096;
pub const MAX_GWAS_LINE_BYTES: usize = 1_048_576;
pub const MAX_GWAS_ANNOTATION_BYTES_PER_RECORD: usize = 64 * 1_024;
pub const MAX_GWAS_SOURCE_ANNOTATION_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_GWAS_SEARCH_ASSOCIATIONS: usize = 1_000_000;
pub const MAX_GWAS_SEARCH_RESULTS: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct GwasSearchHit {
    pub matched_term: String,
    pub record: GwasRecord,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GwasSearchResults {
    pub hits: Vec<GwasSearchHit>,
    pub index_truncated: bool,
    pub scan_truncated: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GwasQueryProvenance {
    pub records_examined: usize,
    pub records_retained: usize,
    pub source_rows_ignored: usize,
    pub records_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GwasQuery {
    pub records: Vec<GwasRecord>,
    pub provenance: GwasQueryProvenance,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CopyNumberSegment {
    pub sample_index: usize,
    pub start: u64,
    pub end: u64,
    pub marker_count: Option<u64>,
    pub value: f32,
}

pub const MAX_COPY_NUMBER_SAMPLES: usize = 100_000;
pub const MAX_COPY_NUMBER_SOURCE_SEGMENTS: usize = 2_000_000;
pub const MAX_COPY_NUMBER_SEGMENTS_PER_QUERY: usize = 250_000;
pub const MAX_COPY_NUMBER_SORT_SEGMENTS: usize = 500_000;
pub const MAX_COPY_NUMBER_SAMPLE_NAME_BYTES: usize = 1_024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CopyNumberQueryProvenance {
    pub records_examined: usize,
    pub records_retained: usize,
    pub source_rows_ignored: usize,
    pub records_truncated: bool,
    pub sort_records_examined: usize,
    pub sort_records_truncated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CopyNumberQuery {
    pub samples: Vec<String>,
    pub segments: Vec<CopyNumberSegment>,
    pub sample_sort_values: Vec<Option<f32>>,
    pub inferred_value_semantics: CopyNumberValueSemantics,
    pub provenance: CopyNumberQueryProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationAnnotation {
    pub column_index: u16,
    pub value: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationRecord {
    pub sample_index: usize,
    pub start: u64,
    pub end: u64,
    pub source_start: u64,
    pub source_end: u64,
    pub insertion_boundary: bool,
    pub gene: String,
    pub classification: String,
    pub variant_type: String,
    pub reference: String,
    pub tumor_allele_one: String,
    pub tumor_allele_two: String,
    pub annotations: Vec<MutationAnnotation>,
    pub annotation_values_omitted: usize,
}

pub const MAX_MUTATION_SAMPLES: usize = 100_000;
pub const MAX_MUTATION_SOURCE_RECORDS: usize = 1_000_000;
pub const MAX_MUTATION_RECORDS_PER_QUERY: usize = 100_000;
pub const MAX_MUTATION_COLUMNS: usize = 256;
pub const MAX_MUTATION_FIELD_BYTES: usize = 4_096;
pub const MAX_MUTATION_ANNOTATION_BYTES_PER_RECORD: usize = 64 * 1_024;
pub const MAX_MUTATION_SOURCE_ANNOTATION_BYTES: usize = 512 * 1_024 * 1_024;
pub const MAX_MUTATION_SAMPLE_NAME_BYTES: usize = 1_024;
pub const MAX_MUTATION_LINE_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MutationQueryProvenance {
    pub records_examined: usize,
    pub records_retained: usize,
    pub source_rows_ignored: usize,
    pub records_truncated: bool,
    pub header_columns_omitted: usize,
    pub annotation_values_truncated: usize,
    pub source_annotation_bytes: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MutationQuery {
    pub headers: Vec<String>,
    pub samples: Vec<String>,
    pub records: Vec<MutationRecord>,
    pub provenance: MutationQueryProvenance,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractionEndpoint {
    pub contig: String,
    pub start: u64,
    pub end: u64,
    pub name: Option<String>,
    pub strand: Strand,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InteractionRecord {
    pub source: InteractionEndpoint,
    pub target: InteractionEndpoint,
    pub name: Option<String>,
    pub score: Option<f32>,
    pub value: Option<f64>,
    pub experiment: Option<String>,
    pub color: Option<[u8; 4]>,
    pub directional: bool,
}

pub const MAX_INTERACTION_RECORDS_PER_QUERY: usize = 100_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct InteractionQueryProvenance {
    pub endpoint_entries_examined: usize,
    pub duplicate_entries_ignored: usize,
    pub records_filtered: usize,
    pub records_truncated: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct InteractionQuery {
    pub records: Vec<InteractionRecord>,
    pub provenance: InteractionQueryProvenance,
}
