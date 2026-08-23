use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read as _},
    path::{Path, PathBuf},
};

use bigtools::{BBIFileRead, BigBedRead, BigWigRead, utils::reopen::ReopenableFile};
use flate2::read::MultiGzDecoder;
use rgv_core::{
    CopyNumberSortMode, CopyNumberTrackSettings, InteractionTrackSettings, VariantFieldComparison,
    VariantFieldPredicate, VariantFieldScope, VariantGenotypeFilter, VariantMissingValueRule,
    VariantTrackSettings, VariantTypeGroup,
};
use rust_htslib::bcf::{
    self, Read as _,
    header::HeaderRecord,
    record::{GenotypeAllele, Numeric as _},
};
use thiserror::Error;

use crate::{HttpRangeConfig, HttpRangeReader};

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
const MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS: usize = 100_000;
const BIGBED_BACKWARD_NAVIGATION_WINDOW_BASES: u32 = 1_000_000;

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
const MAX_GWAS_SEARCH_SCANNED_ASSOCIATIONS: usize = 100_000;

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

pub struct TextFeatureSource {
    features: HashMap<String, FeatureIndex>,
    search_terms: BTreeMap<String, Vec<(String, usize)>>,
    search_index_truncated: bool,
}

#[derive(Default)]
struct FeatureIndex {
    records: Vec<FeatureRecord>,
    prefix_max_end: Vec<u64>,
}

impl FeatureIndex {
    fn finish(&mut self) {
        self.records
            .sort_by_key(|record| (record.start, record.end));
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.records.len());
        let mut maximum = 0_u64;
        for record in &self.records {
            maximum = maximum.max(record.end);
            self.prefix_max_end.push(maximum);
        }
    }

    fn query(&self, start: u64, end: u64) -> Result<Vec<FeatureRecord>, TrackError> {
        let right = self.records.partition_point(|feature| feature.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        let mut records = Vec::new();
        for feature in self.records[left..right]
            .iter()
            .filter(|feature| feature.end > start)
        {
            if records.len() == MAX_FEATURE_RECORDS_PER_QUERY {
                return Err(TrackError::ResourceLimit(format!(
                    "feature query exceeds the {MAX_FEATURE_RECORDS_PER_QUERY}-record limit"
                )));
            }
            records.push(feature.clone());
        }
        Ok(records)
    }
}

impl TextFeatureSource {
    pub fn open(path: impl AsRef<Path>, format: FeatureFormat) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"))
        {
            Self::from_reader(BufReader::new(MultiGzDecoder::new(file)), format)
        } else {
            Self::from_reader(BufReader::new(file), format)
        }
    }

    pub fn from_reader(
        mut reader: impl BufRead,
        format: FeatureFormat,
    ) -> Result<Self, TrackError> {
        let mut features: HashMap<String, FeatureIndex> = HashMap::new();
        let mut source_records = 0_usize;
        let mut line_number = 0_usize;
        loop {
            let mut line = String::new();
            let bytes = (&mut reader)
                .take((MAX_FEATURE_LINE_BYTES + 1) as u64)
                .read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            line_number = line_number.saturating_add(1);
            if bytes > MAX_FEATURE_LINE_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "feature source line {line_number} exceeds {MAX_FEATURE_LINE_BYTES} bytes"
                )));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with("track ")
                || line.starts_with("browser ")
                || (format == FeatureFormat::Psl && is_psl_header_line(line))
            {
                continue;
            }
            if source_records == MAX_FEATURE_SOURCE_RECORDS {
                return Err(TrackError::ResourceLimit(format!(
                    "feature source exceeds the {MAX_FEATURE_SOURCE_RECORDS}-record limit"
                )));
            }
            let feature = match format {
                FeatureFormat::Bed => parse_bed(line),
                FeatureFormat::Gff3 => parse_gff(line, false),
                FeatureFormat::Gtf => parse_gff(line, true),
                FeatureFormat::GenePred => parse_gene_pred(line, false),
                FeatureFormat::RefFlat => parse_gene_pred(line, true),
                FeatureFormat::NarrowPeak => parse_narrow_peak(line),
                FeatureFormat::BroadPeak => parse_broad_peak(line),
                FeatureFormat::Psl => parse_psl(line),
            }
            .map_err(|message| TrackError::MalformedLine {
                line: line_number,
                message,
            })?;
            source_records = source_records.saturating_add(1);
            features
                .entry(feature.contig.clone())
                .or_default()
                .records
                .push(feature);
        }
        for index in features.values_mut() {
            index.finish();
        }
        let (search_terms, search_index_truncated) = build_feature_search_index(&features);
        Ok(Self {
            features,
            search_terms,
            search_index_truncated,
        })
    }

    pub fn query(
        &self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<FeatureRecord>, TrackError> {
        let Some(features) = self.features.get(contig) else {
            return Ok(Vec::new());
        };
        features.query(start, end)
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.features.keys().map(String::as_str)
    }

    #[must_use]
    pub fn adjacent_feature(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<FeatureRecord> {
        let records = &self.features.get(contig)?.records;
        if forward {
            let index = records.partition_point(|record| record.start < position);
            records.get(index).cloned()
        } else {
            let right = records.partition_point(|record| record.start < position);
            records[..right]
                .iter()
                .rev()
                .find(|record| record.end <= position)
                .cloned()
        }
    }

    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> FeatureSearchResults {
        let query = normalized_feature_search_term(query);
        if query.is_empty() || limit == 0 {
            return FeatureSearchResults {
                index_truncated: self.search_index_truncated,
                ..FeatureSearchResults::default()
            };
        }
        let limit = limit.min(MAX_FEATURE_SEARCH_RESULTS);
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        let mut scanned = 0_usize;
        let mut scan_truncated = false;
        for (term, locations) in self.search_terms.range(query.clone()..) {
            if !term.starts_with(&query) {
                break;
            }
            for (contig, index) in locations {
                if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break;
                }
                scanned = scanned.saturating_add(1);
                if seen.insert((contig.as_str(), *index))
                    && let Some(record) = self
                        .features
                        .get(contig)
                        .and_then(|features| features.records.get(*index))
                {
                    candidates.push((term == &query, term.clone(), record.clone()));
                }
            }
            if scan_truncated {
                break;
            }
        }
        candidates.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.contig.cmp(&right.2.contig))
                .then_with(|| left.2.start.cmp(&right.2.start))
                .then_with(|| left.2.end.cmp(&right.2.end))
                .then_with(|| left.2.name.cmp(&right.2.name))
        });
        candidates.truncate(limit);
        FeatureSearchResults {
            hits: candidates
                .into_iter()
                .map(|(_, matched_term, record)| FeatureSearchHit {
                    matched_term,
                    record,
                })
                .collect(),
            index_truncated: self.search_index_truncated,
            scan_truncated,
        }
    }
}

fn build_feature_search_index(
    features: &HashMap<String, FeatureIndex>,
) -> (BTreeMap<String, Vec<(String, usize)>>, bool) {
    let mut terms = BTreeMap::<String, Vec<(String, usize)>>::new();
    let mut associations = 0_usize;
    let mut contigs = features.keys().collect::<Vec<_>>();
    contigs.sort();
    for contig in contigs {
        let Some(index) = features.get(contig) else {
            continue;
        };
        for (record_index, record) in index.records.iter().enumerate() {
            for term in feature_search_terms(record) {
                if associations == MAX_FEATURE_SEARCH_ASSOCIATIONS {
                    return (terms, true);
                }
                terms
                    .entry(term)
                    .or_default()
                    .push((contig.clone(), record_index));
                associations = associations.saturating_add(1);
            }
        }
    }
    (terms, false)
}

fn feature_search_terms(record: &FeatureRecord) -> Vec<String> {
    let mut record_terms = Vec::new();
    if let Some(name) = &record.name {
        record_terms.push(name.as_str());
    }
    for (key, value) in &record.attributes {
        if matches!(
            key.to_ascii_lowercase().as_str(),
            "id" | "name"
                | "alias"
                | "gene_id"
                | "gene_name"
                | "gene_name2"
                | "gene_synonym"
                | "name2"
                | "transcript_id"
                | "transcript_name"
        ) {
            record_terms.extend(value.split(','));
        }
    }
    let mut normalized = record_terms
        .into_iter()
        .map(normalized_feature_search_term)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

fn normalized_feature_search_term(value: &str) -> String {
    value.trim().to_lowercase().chars().take(256).collect()
}

pub struct TextQuantitativeSource {
    signals: HashMap<String, QuantitativeIndex>,
}

#[derive(Default)]
struct QuantitativeIndex {
    records: Vec<QuantitativeRecord>,
    prefix_max_end: Vec<u64>,
}

impl QuantitativeIndex {
    fn finish(&mut self) {
        self.records
            .sort_by_key(|record| (record.start, record.end));
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.records.len());
        let mut maximum = 0_u64;
        for record in &self.records {
            maximum = maximum.max(record.end);
            self.prefix_max_end.push(maximum);
        }
    }

    fn query(&self, start: u64, end: u64) -> QuantitativeQuery {
        let right = self.records.partition_point(|record| record.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        let mut query = QuantitativeQuery::default();
        for record in self.records[left..right]
            .iter()
            .filter(|record| record.end > start)
        {
            if !retain_quantitative_record(&mut query, *record, MAX_QUANTITATIVE_RECORDS_PER_QUERY)
            {
                break;
            }
        }
        query
    }
}

impl TextQuantitativeSource {
    pub fn open(path: impl AsRef<Path>, format: QuantitativeFormat) -> Result<Self, TrackError> {
        let file = BufReader::new(File::open(path)?);
        Self::from_reader(file, format)
    }

    pub fn from_reader(
        reader: impl BufRead,
        format: QuantitativeFormat,
    ) -> Result<Self, TrackError> {
        let mut signals: HashMap<String, QuantitativeIndex> = HashMap::new();
        let mut wig_state = None;
        for (line_number, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with("track ")
                || line.starts_with("browser ")
            {
                continue;
            }
            let parsed = match format {
                QuantitativeFormat::BedGraph => parse_bedgraph(line),
                QuantitativeFormat::Wig => parse_wig_line(line, &mut wig_state),
            }
            .map_err(|message| TrackError::MalformedLine {
                line: line_number + 1,
                message,
            })?;
            if let Some((contig, record)) = parsed {
                signals.entry(contig).or_default().records.push(record);
            }
        }
        for index in signals.values_mut() {
            index.finish();
        }
        Ok(Self { signals })
    }

    #[must_use]
    pub fn query(&self, contig: &str, start: u64, end: u64) -> QuantitativeQuery {
        self.signals
            .get(contig)
            .map_or_else(QuantitativeQuery::default, |index| index.query(start, end))
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.signals.keys().map(String::as_str)
    }
}

/// Bounded local plain/gzip GWAS source using the flexible header aliases
/// documented by IGV Desktop and PLINK-style one-based BP coordinates.
pub struct TextGwasSource {
    associations: HashMap<String, Vec<GwasRecord>>,
    source_rows_ignored: usize,
    search_terms: BTreeMap<String, Vec<(String, usize)>>,
    search_index_truncated: bool,
}

#[derive(Clone, Copy)]
struct GwasColumns {
    chromosome: usize,
    position: usize,
    identifier: usize,
    p_value: usize,
}

impl TextGwasSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"))
        {
            Self::from_reader(BufReader::new(MultiGzDecoder::new(file)))
        } else {
            Self::from_reader(BufReader::new(file))
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn from_reader(mut reader: impl BufRead) -> Result<Self, TrackError> {
        let mut associations: HashMap<String, Vec<GwasRecord>> = HashMap::new();
        let mut headers = None;
        let mut columns = None;
        let mut source_records = 0_usize;
        let mut source_annotation_bytes = 0_usize;
        let mut source_rows_ignored = 0_usize;
        let mut line_number = 0_usize;
        loop {
            let mut line = String::new();
            let bytes = (&mut reader)
                .take((MAX_GWAS_LINE_BYTES + 1) as u64)
                .read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            line_number = line_number.saturating_add(1);
            if bytes > MAX_GWAS_LINE_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "GWAS source line {line_number} exceeds {MAX_GWAS_LINE_BYTES} bytes"
                )));
            }
            let line = line.trim_end_matches(['\r', '\n']).trim();
            if line.is_empty()
                || line.starts_with("##")
                || line.starts_with("track ")
                || line.starts_with("browser ")
            {
                continue;
            }
            if headers.is_none() {
                let parsed = line
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>();
                let parsed_columns = match gwas_columns(&parsed) {
                    Ok(columns) => columns,
                    Err(_) if line.starts_with('#') => continue,
                    Err(message) => {
                        return Err(TrackError::MalformedLine {
                            line: line_number,
                            message,
                        });
                    }
                };
                headers = Some(parsed);
                columns = Some(parsed_columns);
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            if source_records == MAX_GWAS_SOURCE_RECORDS {
                return Err(TrackError::ResourceLimit(format!(
                    "GWAS source exceeds the {MAX_GWAS_SOURCE_RECORDS}-record limit"
                )));
            }
            let headers = headers.as_ref().expect("header initialized together");
            let columns = columns.expect("columns initialized together");
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != headers.len() {
                return Err(TrackError::MalformedLine {
                    line: line_number,
                    message: format!(
                        "GWAS row has {} columns but its header declares {}",
                        fields.len(),
                        headers.len()
                    ),
                });
            }
            let p_text = fields[columns.p_value];
            if matches!(p_text.to_ascii_lowercase().as_str(), "na" | "nan" | ".") {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            }
            let (record, annotation_bytes) =
                parse_gwas_record(headers, &fields, columns).map_err(|message| {
                    TrackError::MalformedLine {
                        line: line_number,
                        message,
                    }
                })?;
            source_annotation_bytes = source_annotation_bytes
                .checked_add(annotation_bytes)
                .ok_or_else(|| {
                    TrackError::ResourceLimit("GWAS annotation byte count overflowed".to_owned())
                })?;
            if source_annotation_bytes > MAX_GWAS_SOURCE_ANNOTATION_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "GWAS source annotations exceed {MAX_GWAS_SOURCE_ANNOTATION_BYTES} bytes"
                )));
            }
            source_records = source_records.saturating_add(1);
            associations
                .entry(record.contig.clone())
                .or_default()
                .push(record);
        }
        if headers.is_none() {
            return Err(TrackError::InvalidSchema(
                "GWAS source has no required header line".to_owned(),
            ));
        }
        for records in associations.values_mut() {
            records.sort_by(|left, right| {
                left.position
                    .cmp(&right.position)
                    .then_with(|| left.identifier.cmp(&right.identifier))
            });
        }
        let mut search_terms: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
        let mut search_associations = 0_usize;
        let mut search_index_truncated = false;
        let mut contigs = associations.keys().cloned().collect::<Vec<_>>();
        contigs.sort();
        'index: for contig in contigs {
            for (index, record) in associations[&contig].iter().enumerate() {
                if search_associations == MAX_GWAS_SEARCH_ASSOCIATIONS {
                    search_index_truncated = true;
                    break 'index;
                }
                let term = normalized_gwas_search_term(&record.identifier);
                if !term.is_empty() {
                    search_terms
                        .entry(term)
                        .or_default()
                        .push((contig.clone(), index));
                    search_associations = search_associations.saturating_add(1);
                }
            }
        }
        Ok(Self {
            associations,
            source_rows_ignored,
            search_terms,
            search_index_truncated,
        })
    }

    #[must_use]
    pub fn query(&self, contig: &str, start: u64, end: u64) -> GwasQuery {
        let Some(records) = self.associations.get(contig) else {
            return GwasQuery {
                provenance: GwasQueryProvenance {
                    source_rows_ignored: self.source_rows_ignored,
                    ..GwasQueryProvenance::default()
                },
                ..GwasQuery::default()
            };
        };
        let left = records.partition_point(|record| record.position < start);
        let right = records.partition_point(|record| record.position < end);
        let visible = &records[left..right];
        let truncated = visible.len() > MAX_GWAS_RECORDS_PER_QUERY;
        let retained = bounded_gwas_records(visible, MAX_GWAS_RECORDS_PER_QUERY);
        GwasQuery {
            provenance: GwasQueryProvenance {
                records_examined: visible.len(),
                records_retained: retained.len(),
                source_rows_ignored: self.source_rows_ignored,
                records_truncated: truncated,
            },
            records: retained,
        }
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.associations.keys().map(String::as_str)
    }

    #[must_use]
    pub fn adjacent_association(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<GwasRecord> {
        let records = self.associations.get(contig)?;
        if forward {
            records
                .get(records.partition_point(|record| record.position < position))
                .cloned()
        } else {
            let right = records.partition_point(|record| record.position < position);
            records.get(right.checked_sub(1)?).cloned()
        }
    }

    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> GwasSearchResults {
        let query = normalized_gwas_search_term(query);
        if query.is_empty() || limit == 0 {
            return GwasSearchResults {
                index_truncated: self.search_index_truncated,
                ..GwasSearchResults::default()
            };
        }
        let limit = limit.min(MAX_GWAS_SEARCH_RESULTS);
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        let mut scanned = 0_usize;
        let mut scan_truncated = false;
        for (term, locations) in self.search_terms.range(query.clone()..) {
            if !term.starts_with(&query) {
                break;
            }
            for (contig, index) in locations {
                if scanned == MAX_GWAS_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break;
                }
                scanned = scanned.saturating_add(1);
                if seen.insert((contig.as_str(), *index))
                    && let Some(record) = self
                        .associations
                        .get(contig)
                        .and_then(|records| records.get(*index))
                {
                    candidates.push(GwasSearchHit {
                        matched_term: term.clone(),
                        record: record.clone(),
                    });
                }
            }
            if scan_truncated {
                break;
            }
        }
        candidates.sort_by(|left, right| {
            (right.matched_term == query)
                .cmp(&(left.matched_term == query))
                .then_with(|| left.matched_term.cmp(&right.matched_term))
                .then_with(|| left.record.contig.cmp(&right.record.contig))
                .then_with(|| left.record.position.cmp(&right.record.position))
        });
        candidates.truncate(limit);
        GwasSearchResults {
            hits: candidates,
            index_truncated: self.search_index_truncated,
            scan_truncated,
        }
    }
}

fn normalized_gwas_search_term(value: &str) -> String {
    value.trim().to_lowercase().chars().take(256).collect()
}

fn bounded_gwas_records(visible: &[GwasRecord], limit: usize) -> Vec<GwasRecord> {
    let limit = limit.max(1);
    if visible.len() <= limit {
        return visible.to_vec();
    }
    // Retain the strongest association from each stable positional chunk.
    // This keeps coverage across the locus without discarding narrow, highly
    // significant peaks merely because they occur late in a file.
    let chunk_size = visible.len().div_ceil(limit);
    visible
        .chunks(chunk_size)
        .filter_map(|chunk| {
            chunk.iter().max_by(|left, right| {
                left.minus_log10_p
                    .total_cmp(&right.minus_log10_p)
                    .then_with(|| right.position.cmp(&left.position))
            })
        })
        .cloned()
        .collect()
}

fn gwas_columns(headers: &[String]) -> Result<GwasColumns, String> {
    if headers.len() > MAX_GWAS_COLUMNS {
        return Err(format!(
            "GWAS header exceeds the {MAX_GWAS_COLUMNS}-column limit"
        ));
    }
    let find = |aliases: &[&str], label: &str| -> Result<usize, String> {
        let matches = headers
            .iter()
            .enumerate()
            .filter(|(_, header)| {
                let normalized = header.trim_start_matches('#').to_ascii_lowercase();
                aliases.contains(&normalized.as_str())
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [index] => Ok(*index),
            [] => Err(format!("GWAS header is missing required {label} column")),
            _ => Err(format!("GWAS header has multiple {label} columns")),
        }
    };
    Ok(GwasColumns {
        chromosome: find(&["chr", "chrom", "chromosome"], "CHR/chromosome")?,
        position: find(&["bp", "pos", "position"], "BP/position")?,
        identifier: find(
            &["snp", "rs", "rsid", "rsnum", "id", "marker", "markername"],
            "SNP/identifier",
        )?,
        p_value: find(&["p", "pval", "p-value", "pvalue", "p.value"], "P/p-value")?,
    })
}

fn parse_gwas_record(
    headers: &[String],
    values: &[&str],
    columns: GwasColumns,
) -> Result<(GwasRecord, usize), String> {
    let contig = values[columns.chromosome];
    if contig.is_empty() {
        return Err("GWAS chromosome cannot be empty".to_owned());
    }
    let source_position = values[columns.position]
        .parse::<u64>()
        .map_err(|_| "GWAS BP/position must be a positive integer".to_owned())?;
    let position = source_position
        .checked_sub(1)
        .ok_or_else(|| "GWAS BP/position is one-based and cannot be zero".to_owned())?;
    let identifier = values[columns.identifier];
    if identifier.is_empty() || identifier.len() > MAX_GWAS_FIELD_BYTES {
        return Err(format!(
            "GWAS identifier must contain 1 to {MAX_GWAS_FIELD_BYTES} bytes"
        ));
    }
    let p_value = values[columns.p_value]
        .parse::<f64>()
        .map_err(|_| "GWAS p-value must be numeric or NA".to_owned())?;
    if !p_value.is_finite() || !(0.0..=1.0).contains(&p_value) {
        return Err("GWAS p-value must be finite and between 0 and 1".to_owned());
    }
    let minus_log10_p = if p_value == 0.0 {
        f32::INFINITY
    } else {
        (-p_value.log10()) as f32
    };
    let mut annotation_bytes = 0_usize;
    let mut fields = Vec::with_capacity(headers.len());
    for (header, value) in headers.iter().zip(values) {
        if header.len() > MAX_GWAS_FIELD_BYTES || value.len() > MAX_GWAS_FIELD_BYTES {
            return Err(format!(
                "GWAS field exceeds the {MAX_GWAS_FIELD_BYTES}-byte limit"
            ));
        }
        annotation_bytes = annotation_bytes
            .checked_add(header.len().saturating_add(value.len()))
            .ok_or_else(|| "GWAS annotation byte count overflowed".to_owned())?;
        if annotation_bytes > MAX_GWAS_ANNOTATION_BYTES_PER_RECORD {
            return Err(format!(
                "GWAS row annotations exceed {MAX_GWAS_ANNOTATION_BYTES_PER_RECORD} bytes"
            ));
        }
        fields.push((header.clone(), (*value).to_owned()));
    }
    Ok((
        GwasRecord {
            contig: contig.to_owned(),
            position,
            identifier: identifier.to_owned(),
            p_value,
            minus_log10_p,
            fields,
        },
        annotation_bytes,
    ))
}

pub struct TextCopyNumberSource {
    samples: Vec<String>,
    segments: HashMap<String, CopyNumberIndex>,
    source_rows_ignored: usize,
    inferred_value_semantics: CopyNumberValueSemantics,
}

#[derive(Default)]
struct CopyNumberIndex {
    segments: Vec<CopyNumberSegment>,
    prefix_max_end: Vec<u64>,
}

impl CopyNumberIndex {
    fn finish(&mut self) {
        self.segments
            .sort_by_key(|segment| (segment.start, segment.end, segment.sample_index));
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.segments.len());
        let mut maximum = 0_u64;
        for segment in &self.segments {
            maximum = maximum.max(segment.end);
            self.prefix_max_end.push(maximum);
        }
    }

    fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = &CopyNumberSegment> {
        let right = self.segments.partition_point(|segment| segment.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        self.segments[left..right]
            .iter()
            .filter(move |segment| segment.end > start)
    }
}

impl TextCopyNumberSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::from_reader(BufReader::new(File::open(path)?))
    }

    #[allow(clippy::too_many_lines)]
    pub fn from_reader(reader: impl BufRead) -> Result<Self, TrackError> {
        let mut samples = Vec::new();
        let mut sample_indices = HashMap::new();
        let mut segments: HashMap<String, CopyNumberIndex> = HashMap::new();
        let mut source_rows_ignored = 0_usize;
        let mut total_segments = 0_usize;
        let mut header_seen = false;
        let mut zero_based = false;
        let mut saw_negative = false;
        for (line_number, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with("browser ") || line.starts_with("#type=") {
                continue;
            }
            if line.starts_with("#track ") || line.starts_with("track ") {
                if let Some(coords) =
                    track_coordinate_mode(line).map_err(|message| TrackError::MalformedLine {
                        line: line_number + 1,
                        message,
                    })?
                {
                    zero_based = coords == 0;
                }
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            if !header_seen {
                if line.split('\t').count() < 5 {
                    return Err(TrackError::MalformedLine {
                        line: line_number + 1,
                        message: "SEG header requires at least five tab-separated columns"
                            .to_owned(),
                    });
                }
                header_seen = true;
                continue;
            }
            let Some(parsed) =
                parse_seg_line(line, zero_based).map_err(|message| TrackError::MalformedLine {
                    line: line_number + 1,
                    message,
                })?
            else {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            };
            let (sample, contig, start, end, marker_count, value) = parsed;
            if sample.len() > MAX_COPY_NUMBER_SAMPLE_NAME_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "SEG sample name exceeds {MAX_COPY_NUMBER_SAMPLE_NAME_BYTES} bytes"
                )));
            }
            let sample_index = if let Some(index) = sample_indices.get(&sample) {
                *index
            } else {
                if samples.len() == MAX_COPY_NUMBER_SAMPLES {
                    return Err(TrackError::ResourceLimit(format!(
                        "SEG has more than {MAX_COPY_NUMBER_SAMPLES} samples"
                    )));
                }
                let index = samples.len();
                sample_indices.insert(sample.clone(), index);
                samples.push(sample);
                index
            };
            if total_segments == MAX_COPY_NUMBER_SOURCE_SEGMENTS {
                return Err(TrackError::ResourceLimit(format!(
                    "SEG has more than {MAX_COPY_NUMBER_SOURCE_SEGMENTS} segments"
                )));
            }
            saw_negative |= value < 0.0;
            segments
                .entry(contig)
                .or_default()
                .segments
                .push(CopyNumberSegment {
                    sample_index,
                    start,
                    end,
                    marker_count,
                    value,
                });
            total_segments = total_segments.saturating_add(1);
        }
        if !header_seen {
            return Err(TrackError::Backend("SEG file has no header".to_owned()));
        }
        for index in segments.values_mut() {
            index.finish();
        }
        Ok(Self {
            samples,
            segments,
            source_rows_ignored,
            inferred_value_semantics: if saw_negative {
                CopyNumberValueSemantics::LogRatio
            } else {
                CopyNumberValueSemantics::Absolute
            },
        })
    }

    #[must_use]
    pub fn query(
        &self,
        contig: &str,
        start: u64,
        end: u64,
        settings: &CopyNumberTrackSettings,
    ) -> CopyNumberQuery {
        let mut provenance = CopyNumberQueryProvenance {
            source_rows_ignored: self.source_rows_ignored,
            ..CopyNumberQueryProvenance::default()
        };
        let mut result = Vec::new();
        if let Some(index) = self.segments.get(contig) {
            for segment in index.overlapping(start, end) {
                provenance.records_examined = provenance.records_examined.saturating_add(1);
                if result.len() == MAX_COPY_NUMBER_SEGMENTS_PER_QUERY {
                    provenance.records_truncated = true;
                    break;
                }
                result.push(*segment);
            }
        }
        provenance.records_retained = result.len();
        let mut sample_sort_values = vec![None; self.samples.len()];
        if settings.sort_mode != CopyNumberSortMode::FileOrder
            && let Some(locus) = &settings.sort_locus
            && let Some(index) = self.segments.get(&locus.contig)
        {
            let mut weighted_sums = vec![0.0_f64; self.samples.len()];
            let mut weights = vec![0_u64; self.samples.len()];
            for segment in index.overlapping(locus.start, locus.end) {
                if provenance.sort_records_examined == MAX_COPY_NUMBER_SORT_SEGMENTS {
                    provenance.sort_records_truncated = true;
                    break;
                }
                provenance.sort_records_examined =
                    provenance.sort_records_examined.saturating_add(1);
                let overlap_start = segment.start.max(locus.start);
                let overlap_end = segment.end.min(locus.end);
                let weight = overlap_end.saturating_sub(overlap_start);
                if weight > 0 {
                    weighted_sums[segment.sample_index] += f64::from(segment.value) * weight as f64;
                    weights[segment.sample_index] =
                        weights[segment.sample_index].saturating_add(weight);
                }
            }
            for (sample_index, (&sum, &weight)) in weighted_sums.iter().zip(&weights).enumerate() {
                if weight > 0 {
                    sample_sort_values[sample_index] = Some((sum / weight as f64) as f32);
                }
            }
        }
        CopyNumberQuery {
            samples: self.samples.clone(),
            segments: result,
            sample_sort_values,
            inferred_value_semantics: self.inferred_value_semantics,
            provenance,
        }
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.segments.keys().map(String::as_str)
    }
}

type ParsedSegLine = (String, String, u64, u64, Option<u64>, f32);

fn parse_seg_line(line: &str, zero_based: bool) -> Result<Option<ParsedSegLine>, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() < 5 {
        return Err("SEG requires sample, chromosome, start, end, and value columns".to_owned());
    }
    let sample = fields[0].trim();
    let contig = fields[1].trim();
    if sample.is_empty() || contig.is_empty() {
        return Err("SEG sample and chromosome cannot be empty".to_owned());
    }
    let input_start = parse_u64(fields[2].trim(), "SEG start")?;
    let input_end = parse_u64(fields[3].trim(), "SEG end")?;
    let (start, end) = if zero_based {
        (input_start, input_end)
    } else {
        (
            input_start
                .checked_sub(1)
                .ok_or_else(|| "one-based SEG start must be positive".to_owned())?,
            input_end,
        )
    };
    if start >= end {
        return Err("SEG start must be smaller than end".to_owned());
    }
    let Ok(value) = fields[fields.len() - 1].trim().parse::<f32>() else {
        return Ok(None);
    };
    if !value.is_finite() {
        return Ok(None);
    }
    let marker_count = (fields.len() >= 6)
        .then(|| fields[4].trim().parse::<u64>().ok())
        .flatten();
    Ok(Some((
        sample.to_owned(),
        contig.to_owned(),
        start,
        end,
        marker_count,
        value,
    )))
}

pub struct TextMutationSource {
    headers: Vec<String>,
    samples: Vec<String>,
    mutations: HashMap<String, MutationIndex>,
    source_rows_ignored: usize,
    header_columns_omitted: usize,
    source_annotation_bytes: usize,
}

#[derive(Default)]
struct MutationIndex {
    records: Vec<MutationRecord>,
    prefix_max_end: Vec<u64>,
}

impl MutationIndex {
    fn finish(&mut self) {
        self.records
            .sort_by_key(|record| (record.start, record.end, record.sample_index));
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.records.len());
        let mut maximum = 0_u64;
        for record in &self.records {
            maximum = maximum.max(record.end);
            self.prefix_max_end.push(maximum);
        }
    }

    fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = &MutationRecord> {
        let right = self.records.partition_point(|record| record.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        self.records[left..right]
            .iter()
            .filter(move |record| record.end > start)
    }
}

#[derive(Clone, Copy)]
struct MafHeader {
    gene: usize,
    contig: usize,
    start: usize,
    end: usize,
    classification: usize,
    variant_type: usize,
    reference: usize,
    tumor_allele_one: usize,
    tumor_allele_two: usize,
    sample: usize,
    maximum_required_index: usize,
}

impl MafHeader {
    fn parse(line: &str) -> Result<(Self, Vec<String>, usize), TrackError> {
        let mut headers = Vec::new();
        let mut positions = HashMap::new();
        let mut column_count = 0_usize;
        for (index, raw_header) in line.split('\t').enumerate() {
            let header = raw_header.trim();
            if index < MAX_MUTATION_COLUMNS {
                if header.len() > MAX_MUTATION_FIELD_BYTES {
                    return Err(TrackError::ResourceLimit(format!(
                        "MAF header name exceeds {MAX_MUTATION_FIELD_BYTES} bytes"
                    )));
                }
                headers.push(header.to_owned());
                positions.entry(header.to_owned()).or_insert(index);
            }
            column_count = column_count.saturating_add(1);
        }
        let required = |name: &str| {
            positions.get(name).copied().ok_or_else(|| {
                TrackError::Backend(format!("MAF header is missing required column {name}"))
            })
        };
        let sample = positions
            .get("Tumor_Sample_Barcode")
            .or_else(|| positions.get("Tumor_Sample_UUID"))
            .copied()
            .ok_or_else(|| {
                TrackError::Backend(
                    "MAF header requires Tumor_Sample_Barcode or Tumor_Sample_UUID".to_owned(),
                )
            })?;
        let header = Self {
            gene: required("Hugo_Symbol")?,
            contig: required("Chromosome")?,
            start: required("Start_Position")?,
            end: required("End_Position")?,
            classification: required("Variant_Classification")?,
            variant_type: required("Variant_Type")?,
            reference: required("Reference_Allele")?,
            tumor_allele_one: required("Tumor_Seq_Allele1")?,
            tumor_allele_two: required("Tumor_Seq_Allele2")?,
            sample,
            maximum_required_index: 0,
        };
        let maximum_required_index = [
            header.gene,
            header.contig,
            header.start,
            header.end,
            header.classification,
            header.variant_type,
            header.reference,
            header.tumor_allele_one,
            header.tumor_allele_two,
            header.sample,
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        if maximum_required_index >= MAX_MUTATION_COLUMNS {
            return Err(TrackError::ResourceLimit(format!(
                "required MAF columns occur after the {MAX_MUTATION_COLUMNS}-column limit"
            )));
        }
        Ok((
            Self {
                maximum_required_index,
                ..header
            },
            headers,
            column_count.saturating_sub(MAX_MUTATION_COLUMNS),
        ))
    }
}

impl TextMutationSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let file = File::open(path)?;
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"))
        {
            Self::from_reader(BufReader::new(MultiGzDecoder::new(file)))
        } else {
            Self::from_reader(BufReader::new(file))
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn from_reader(mut reader: impl BufRead) -> Result<Self, TrackError> {
        let mut header = None;
        let mut headers = Vec::new();
        let mut header_columns_omitted = 0_usize;
        let mut samples = Vec::new();
        let mut sample_indices = HashMap::new();
        let mut mutations: HashMap<String, MutationIndex> = HashMap::new();
        let mut source_rows_ignored = 0_usize;
        let mut total_records = 0_usize;
        let mut source_annotation_bytes = 0_usize;
        let mut line_number = 0_usize;
        loop {
            let mut line = String::new();
            let bytes = (&mut reader)
                .take(u64::try_from(MAX_MUTATION_LINE_BYTES + 1).unwrap_or(u64::MAX))
                .read_line(&mut line)?;
            if bytes == 0 {
                break;
            }
            line_number = line_number.saturating_add(1);
            if bytes > MAX_MUTATION_LINE_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "MAF line {line_number} exceeds {MAX_MUTATION_LINE_BYTES} bytes"
                )));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let Some(maf_header) = header else {
                let (parsed, parsed_headers, omitted) =
                    MafHeader::parse(line).map_err(|error| match error {
                        TrackError::Backend(message) => TrackError::MalformedLine {
                            line: line_number,
                            message,
                        },
                        other => other,
                    })?;
                header = Some(parsed);
                headers = parsed_headers;
                header_columns_omitted = omitted;
                continue;
            };
            let values = line
                .split('\t')
                .take(MAX_MUTATION_COLUMNS)
                .collect::<Vec<_>>();
            if values.len() <= maf_header.maximum_required_index {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            }
            let contig = values[maf_header.contig].trim();
            let sample = values[maf_header.sample].trim();
            let input_start = values[maf_header.start].trim().parse::<u64>();
            let input_end = values[maf_header.end].trim().parse::<u64>();
            let (Ok(input_start), Ok(input_end)) = (input_start, input_end) else {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            };
            let insertion_boundary = input_end.checked_add(1) == Some(input_start)
                && (values[maf_header.variant_type]
                    .trim()
                    .eq_ignore_ascii_case("INS")
                    || values[maf_header.reference].trim() == "-");
            let coordinates = if insertion_boundary {
                Some((input_end, input_start))
            } else {
                input_start
                    .checked_sub(1)
                    .filter(|_| input_end >= input_start)
                    .map(|start| (start, input_end))
            };
            let Some((start, end)) = coordinates else {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            };
            if contig.is_empty() || sample.is_empty() {
                source_rows_ignored = source_rows_ignored.saturating_add(1);
                continue;
            }
            if sample.len() > MAX_MUTATION_SAMPLE_NAME_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "MAF sample name exceeds {MAX_MUTATION_SAMPLE_NAME_BYTES} bytes"
                )));
            }
            if total_records == MAX_MUTATION_SOURCE_RECORDS {
                return Err(TrackError::ResourceLimit(format!(
                    "MAF has more than {MAX_MUTATION_SOURCE_RECORDS} mutations"
                )));
            }
            let sample_index = if let Some(index) = sample_indices.get(sample) {
                *index
            } else {
                if samples.len() == MAX_MUTATION_SAMPLES {
                    return Err(TrackError::ResourceLimit(format!(
                        "MAF has more than {MAX_MUTATION_SAMPLES} samples"
                    )));
                }
                let index = samples.len();
                let owned = sample.to_owned();
                sample_indices.insert(owned.clone(), index);
                samples.push(owned);
                index
            };
            let mut annotations = Vec::new();
            let mut annotation_bytes = 0_usize;
            let mut annotation_values_omitted = 0_usize;
            for (column_index, value) in values.iter().enumerate().take(headers.len()) {
                let value = value.trim();
                if value.is_empty() {
                    continue;
                }
                let remaining =
                    MAX_MUTATION_ANNOTATION_BYTES_PER_RECORD.saturating_sub(annotation_bytes);
                if remaining == 0 {
                    annotation_values_omitted = annotation_values_omitted.saturating_add(1);
                    continue;
                }
                let maximum = remaining.min(MAX_MUTATION_FIELD_BYTES);
                let bounded = truncate_utf8(value, maximum);
                let truncated = bounded.len() < value.len();
                annotation_bytes = annotation_bytes.saturating_add(bounded.len());
                annotations.push(MutationAnnotation {
                    column_index: u16::try_from(column_index).unwrap_or(u16::MAX),
                    value: bounded,
                    truncated,
                });
            }
            if source_annotation_bytes.saturating_add(annotation_bytes)
                > MAX_MUTATION_SOURCE_ANNOTATION_BYTES
            {
                return Err(TrackError::ResourceLimit(format!(
                    "MAF annotation values exceed {} MiB",
                    MAX_MUTATION_SOURCE_ANNOTATION_BYTES / (1_024 * 1_024)
                )));
            }
            source_annotation_bytes = source_annotation_bytes.saturating_add(annotation_bytes);
            let bounded_core =
                |index: usize| truncate_utf8(values[index].trim(), MAX_MUTATION_FIELD_BYTES);
            mutations
                .entry(contig.to_owned())
                .or_default()
                .records
                .push(MutationRecord {
                    sample_index,
                    start,
                    end,
                    source_start: input_start,
                    source_end: input_end,
                    insertion_boundary,
                    gene: bounded_core(maf_header.gene),
                    classification: bounded_core(maf_header.classification),
                    variant_type: bounded_core(maf_header.variant_type),
                    reference: bounded_core(maf_header.reference),
                    tumor_allele_one: bounded_core(maf_header.tumor_allele_one),
                    tumor_allele_two: bounded_core(maf_header.tumor_allele_two),
                    annotations,
                    annotation_values_omitted,
                });
            total_records = total_records.saturating_add(1);
        }
        if header.is_none() {
            return Err(TrackError::Backend("MAF file has no header".to_owned()));
        }
        for index in mutations.values_mut() {
            index.finish();
        }
        Ok(Self {
            headers,
            samples,
            mutations,
            source_rows_ignored,
            header_columns_omitted,
            source_annotation_bytes,
        })
    }

    #[must_use]
    pub fn query(&self, contig: &str, start: u64, end: u64) -> MutationQuery {
        let mut query = MutationQuery {
            headers: self.headers.clone(),
            samples: self.samples.clone(),
            provenance: MutationQueryProvenance {
                source_rows_ignored: self.source_rows_ignored,
                header_columns_omitted: self.header_columns_omitted,
                source_annotation_bytes: self.source_annotation_bytes,
                ..MutationQueryProvenance::default()
            },
            ..MutationQuery::default()
        };
        if let Some(index) = self.mutations.get(contig) {
            for record in index.overlapping(start, end) {
                query.provenance.records_examined =
                    query.provenance.records_examined.saturating_add(1);
                if query.records.len() == MAX_MUTATION_RECORDS_PER_QUERY {
                    query.provenance.records_truncated = true;
                    break;
                }
                query.provenance.annotation_values_truncated = query
                    .provenance
                    .annotation_values_truncated
                    .saturating_add(record.annotation_values_omitted)
                    .saturating_add(
                        record
                            .annotations
                            .iter()
                            .filter(|annotation| annotation.truncated)
                            .count(),
                    );
                query.records.push(record.clone());
            }
        }
        query.provenance.records_retained = query.records.len();
        query
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.mutations.keys().map(String::as_str)
    }
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn track_coordinate_mode(line: &str) -> Result<Option<u8>, String> {
    let value = line
        .split_whitespace()
        .find_map(|field| field.strip_prefix("coords="));
    match value {
        None => Ok(None),
        Some("0") => Ok(Some(0)),
        Some("1") => Ok(Some(1)),
        Some(value) => Err(format!("track coords must be 0 or 1, not {value}")),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WigState {
    Fixed {
        contig: String,
        next_one_based: u64,
        step: u64,
        span: u64,
    },
    Variable {
        contig: String,
        span: u64,
    },
}

fn parse_bedgraph(line: &str) -> Result<Option<(String, QuantitativeRecord)>, String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 4 {
        return Err("bedGraph requires chromosome, start, end, and value".to_owned());
    }
    let start = parse_u64(fields[1], "bedGraph start")?;
    let end = parse_u64(fields[2], "bedGraph end")?;
    if start >= end {
        return Err("bedGraph start must be smaller than end".to_owned());
    }
    let value = fields[3]
        .parse::<f32>()
        .map_err(|_| format!("invalid bedGraph value: {}", fields[3]))?;
    Ok(Some((
        fields[0].to_owned(),
        QuantitativeRecord { start, end, value },
    )))
}

fn parse_wig_line(
    line: &str,
    state: &mut Option<WigState>,
) -> Result<Option<(String, QuantitativeRecord)>, String> {
    if line.starts_with("fixedStep") {
        let fields = wig_declaration_fields(line, "fixedStep")?;
        let contig = fields
            .get("chrom")
            .ok_or_else(|| "fixedStep requires chrom".to_owned())?
            .clone();
        let start = parse_required_positive(&fields, "start", "fixedStep")?;
        let step = parse_required_positive(&fields, "step", "fixedStep")?;
        let span = parse_optional_positive(&fields, "span", 1, "fixedStep")?;
        *state = Some(WigState::Fixed {
            contig,
            next_one_based: start,
            step,
            span,
        });
        return Ok(None);
    }
    if line.starts_with("variableStep") {
        let fields = wig_declaration_fields(line, "variableStep")?;
        let contig = fields
            .get("chrom")
            .ok_or_else(|| "variableStep requires chrom".to_owned())?
            .clone();
        let span = parse_optional_positive(&fields, "span", 1, "variableStep")?;
        *state = Some(WigState::Variable { contig, span });
        return Ok(None);
    }
    match state {
        Some(WigState::Fixed {
            contig,
            next_one_based,
            step,
            span,
        }) => {
            let value = line
                .parse::<f32>()
                .map_err(|_| format!("invalid fixedStep value: {line}"))?;
            let start = next_one_based
                .checked_sub(1)
                .ok_or_else(|| "fixedStep start must be one or greater".to_owned())?;
            let end = start
                .checked_add(*span)
                .ok_or_else(|| "fixedStep interval overflow".to_owned())?;
            *next_one_based = next_one_based
                .checked_add(*step)
                .ok_or_else(|| "fixedStep coordinate overflow".to_owned())?;
            Ok(Some((
                contig.clone(),
                QuantitativeRecord { start, end, value },
            )))
        }
        Some(WigState::Variable { contig, span }) => {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 {
                return Err("variableStep data requires position and value".to_owned());
            }
            let one_based = parse_u64(fields[0], "variableStep position")?;
            let start = one_based
                .checked_sub(1)
                .ok_or_else(|| "variableStep position must be one or greater".to_owned())?;
            let end = start
                .checked_add(*span)
                .ok_or_else(|| "variableStep interval overflow".to_owned())?;
            let value = fields[1]
                .parse::<f32>()
                .map_err(|_| format!("invalid variableStep value: {}", fields[1]))?;
            Ok(Some((
                contig.clone(),
                QuantitativeRecord { start, end, value },
            )))
        }
        None => Err("WIG data appears before a fixedStep or variableStep declaration".to_owned()),
    }
}

fn wig_declaration_fields(line: &str, kind: &str) -> Result<HashMap<String, String>, String> {
    let mut fields = HashMap::new();
    let mut words = line.split_whitespace();
    if words.next() != Some(kind) {
        return Err(format!("invalid {kind} declaration"));
    }
    for word in words {
        let (key, value) = word
            .split_once('=')
            .ok_or_else(|| format!("invalid {kind} field: {word}"))?;
        if key.is_empty()
            || value.is_empty()
            || fields.insert(key.to_owned(), value.to_owned()).is_some()
        {
            return Err(format!("invalid or duplicate {kind} field: {word}"));
        }
    }
    Ok(fields)
}

fn parse_required_positive(
    fields: &HashMap<String, String>,
    key: &str,
    kind: &str,
) -> Result<u64, String> {
    let value = fields
        .get(key)
        .ok_or_else(|| format!("{kind} requires {key}"))?;
    parse_optional_positive(fields, key, 0, kind)
        .map_err(|_| format!("invalid {kind} {key}: {value}"))
}

fn parse_optional_positive(
    fields: &HashMap<String, String>,
    key: &str,
    default: u64,
    kind: &str,
) -> Result<u64, String> {
    let value = fields.get(key).map_or(Ok(default), |raw| {
        raw.parse::<u64>()
            .map_err(|_| format!("invalid {kind} {key}: {raw}"))
    })?;
    if value == 0 {
        return Err(format!("{kind} {key} must be positive"));
    }
    Ok(value)
}

pub struct TextInteractionSource {
    records: Vec<InteractionRecord>,
    endpoints: HashMap<String, InteractionIndex>,
}

#[derive(Clone, Copy, Debug)]
struct InteractionIndexEntry {
    start: u64,
    end: u64,
    record_index: usize,
}

#[derive(Default)]
struct InteractionIndex {
    entries: Vec<InteractionIndexEntry>,
    prefix_max_end: Vec<u64>,
}

impl InteractionIndex {
    fn finish(&mut self) {
        self.entries
            .sort_by_key(|entry| (entry.start, entry.end, entry.record_index));
        self.prefix_max_end.clear();
        self.prefix_max_end.reserve(self.entries.len());
        let mut maximum = 0_u64;
        for entry in &self.entries {
            maximum = maximum.max(entry.end);
            self.prefix_max_end.push(maximum);
        }
    }

    fn overlapping(&self, start: u64, end: u64) -> &[InteractionIndexEntry] {
        let right = self.entries.partition_point(|entry| entry.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        &self.entries[left..right]
    }
}

impl TextInteractionSource {
    pub fn open(path: impl AsRef<Path>, format: InteractionFormat) -> Result<Self, TrackError> {
        Self::from_reader(BufReader::new(File::open(path)?), format)
    }

    pub fn from_reader(
        reader: impl BufRead,
        format: InteractionFormat,
    ) -> Result<Self, TrackError> {
        let mut records = Vec::new();
        for (line_number, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with("track ")
                || line.starts_with("browser ")
            {
                continue;
            }
            let record = match format {
                InteractionFormat::BedPe => parse_bedpe(line),
                InteractionFormat::Interact => parse_interact(line),
            }
            .map_err(|message| TrackError::MalformedLine {
                line: line_number + 1,
                message,
            })?;
            records.push(record);
        }
        let mut endpoints: HashMap<String, InteractionIndex> = HashMap::new();
        for (record_index, record) in records.iter().enumerate() {
            for endpoint in [&record.source, &record.target] {
                endpoints
                    .entry(endpoint.contig.clone())
                    .or_default()
                    .entries
                    .push(InteractionIndexEntry {
                        start: endpoint.start,
                        end: endpoint.end,
                        record_index,
                    });
            }
            if record.source.contig == record.target.contig {
                endpoints
                    .entry(record.source.contig.clone())
                    .or_default()
                    .entries
                    .push(InteractionIndexEntry {
                        start: record.source.start.min(record.target.start),
                        end: record.source.end.max(record.target.end),
                        record_index,
                    });
            }
        }
        for index in endpoints.values_mut() {
            index.finish();
        }
        Ok(Self { records, endpoints })
    }

    #[must_use]
    pub fn query(
        &self,
        contig: &str,
        start: u64,
        end: u64,
        settings: &InteractionTrackSettings,
    ) -> InteractionQuery {
        let Some(index) = self.endpoints.get(contig) else {
            return InteractionQuery::default();
        };
        let mut provenance = InteractionQueryProvenance::default();
        let mut seen = HashSet::new();
        let mut record_indices = Vec::new();
        for entry in index
            .overlapping(start, end)
            .iter()
            .filter(|entry| entry.end > start)
        {
            provenance.endpoint_entries_examined =
                provenance.endpoint_entries_examined.saturating_add(1);
            if seen.insert(entry.record_index) {
                record_indices.push(entry.record_index);
            } else {
                provenance.duplicate_entries_ignored =
                    provenance.duplicate_entries_ignored.saturating_add(1);
            }
        }
        record_indices.sort_unstable();
        let mut records = Vec::new();
        for record_index in record_indices {
            let Some(record) = self.records.get(record_index) else {
                continue;
            };
            if settings
                .minimum_score
                .is_some_and(|minimum| record.score.is_none_or(|score| score < minimum))
            {
                provenance.records_filtered = provenance.records_filtered.saturating_add(1);
                continue;
            }
            if records.len() == MAX_INTERACTION_RECORDS_PER_QUERY {
                provenance.records_truncated = true;
                break;
            }
            records.push(record.clone());
        }
        InteractionQuery {
            records,
            provenance,
        }
    }
}

fn parse_bedpe(line: &str) -> Result<InteractionRecord, String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return Err("BEDPE requires at least six endpoint columns".to_owned());
    }
    let source = parse_interaction_endpoint(
        fields[0],
        fields[1],
        fields[2],
        None,
        fields.get(8).copied(),
        "BEDPE source",
    )?;
    let target = parse_interaction_endpoint(
        fields[3],
        fields[4],
        fields[5],
        None,
        fields.get(9).copied(),
        "BEDPE target",
    )?;
    Ok(InteractionRecord {
        source,
        target,
        name: optional_track_string(fields.get(6).copied()),
        score: parse_optional_f32(fields.get(7).copied(), "BEDPE score")?,
        value: None,
        experiment: None,
        color: None,
        directional: false,
    })
}

fn parse_interact(line: &str) -> Result<InteractionRecord, String> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 18 {
        return Err("interact requires all 18 BED5+13 columns".to_owned());
    }
    let envelope_start = parse_u64(fields[1], "interact chromStart")?;
    let envelope_end = parse_u64(fields[2], "interact chromEnd")?;
    if envelope_start >= envelope_end {
        return Err("interact chromStart must be smaller than chromEnd".to_owned());
    }
    let source = parse_interaction_endpoint(
        fields[8],
        fields[9],
        fields[10],
        Some(fields[11]),
        Some(fields[12]),
        "interact source",
    )?;
    let target = parse_interaction_endpoint(
        fields[13],
        fields[14],
        fields[15],
        Some(fields[16]),
        Some(fields[17]),
        "interact target",
    )?;
    Ok(InteractionRecord {
        source,
        target,
        name: optional_track_string(Some(fields[3])),
        score: parse_optional_f32(Some(fields[4]), "interact score")?,
        value: parse_optional_f64(Some(fields[5]), "interact value")?,
        experiment: optional_track_string(Some(fields[6])),
        color: parse_interaction_color(fields[7])?,
        directional: true,
    })
}

fn parse_interaction_endpoint(
    contig: &str,
    raw_start: &str,
    raw_end: &str,
    name: Option<&str>,
    strand: Option<&str>,
    label: &str,
) -> Result<InteractionEndpoint, String> {
    let start = parse_u64(raw_start, &format!("{label} start"))?;
    let end = parse_u64(raw_end, &format!("{label} end"))?;
    if start >= end {
        return Err(format!("{label} start must be smaller than end"));
    }
    Ok(InteractionEndpoint {
        contig: contig.to_owned(),
        start,
        end,
        name: optional_track_string(name),
        strand: strand.map_or(Strand::Unstranded, parse_strand),
    })
}

fn optional_track_string(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty() && *value != ".")
        .map(str::to_owned)
}

fn parse_optional_f32(value: Option<&str>, label: &str) -> Result<Option<f32>, String> {
    value
        .filter(|value| !value.is_empty() && *value != ".")
        .map(|value| {
            let parsed = value
                .parse::<f32>()
                .map_err(|_| format!("invalid {label}: {value}"))?;
            parsed
                .is_finite()
                .then_some(parsed)
                .ok_or_else(|| format!("non-finite {label}: {value}"))
        })
        .transpose()
}

fn parse_optional_f64(value: Option<&str>, label: &str) -> Result<Option<f64>, String> {
    value
        .filter(|value| !value.is_empty() && *value != ".")
        .map(|value| {
            let parsed = value
                .parse::<f64>()
                .map_err(|_| format!("invalid {label}: {value}"))?;
            parsed
                .is_finite()
                .then_some(parsed)
                .ok_or_else(|| format!("non-finite {label}: {value}"))
        })
        .transpose()
}

fn parse_interaction_color(value: &str) -> Result<Option<[u8; 4]>, String> {
    if matches!(value, "0" | ".") {
        return Ok(None);
    }
    if let Some(hex) = value.strip_prefix('#')
        && hex.len() == 6
    {
        let red = u8::from_str_radix(&hex[0..2], 16)
            .map_err(|_| format!("invalid interact color: {value}"))?;
        let green = u8::from_str_radix(&hex[2..4], 16)
            .map_err(|_| format!("invalid interact color: {value}"))?;
        let blue = u8::from_str_radix(&hex[4..6], 16)
            .map_err(|_| format!("invalid interact color: {value}"))?;
        return Ok(Some([red, green, blue, 230]));
    }
    let channels = value
        .split(',')
        .map(str::parse::<u8>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid interact color: {value}"))?;
    if let [red, green, blue] = channels.as_slice() {
        Ok(Some([*red, *green, *blue, 230]))
    } else {
        Err(format!("invalid interact color: {value}"))
    }
}

pub struct LocalVariantSource {
    path: PathBuf,
    reader: bcf::IndexedReader,
    header: VariantHeader,
    omitted_info_schemas: usize,
    omitted_format_schemas: usize,
    total_samples: usize,
    omitted_header_samples: usize,
    id_index: Option<VariantIdIndex>,
}

#[derive(Default)]
struct VariantIdIndex {
    ids: BTreeMap<String, Vec<VariantIdSearchHit>>,
    truncated: bool,
}

impl LocalVariantSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        let reader = if is_remote_url(&raw) {
            let url =
                url::Url::parse(&raw).map_err(|error| TrackError::Backend(error.to_string()))?;
            bcf::IndexedReader::from_url(&url)
        } else {
            bcf::IndexedReader::from_path(path)
        }
        .map_err(|error| TrackError::Backend(error.to_string()))?;
        let (header, omitted_info_schemas, omitted_format_schemas, omitted_header_samples) =
            decode_variant_header(reader.header());
        let total_samples = header.samples.len().saturating_add(omitted_header_samples);
        Ok(Self {
            path: path.to_path_buf(),
            reader,
            header,
            omitted_info_schemas,
            omitted_format_schemas,
            total_samples,
            omitted_header_samples,
            id_index: None,
        })
    }

    pub fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        maximum_records: usize,
    ) -> Result<VariantQuery, TrackError> {
        self.query_with_settings(
            contig,
            start,
            end,
            maximum_records,
            &VariantTrackSettings::default(),
        )
    }

    pub fn query_with_settings(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
        maximum_records: usize,
        settings: &VariantTrackSettings,
    ) -> Result<VariantQuery, TrackError> {
        let rid = self
            .reader
            .header()
            .name2rid(contig.as_bytes())
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        self.reader
            .fetch(rid, start, Some(end.saturating_sub(1)))
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        let mut output = Vec::new();
        let contig = contig.to_owned();
        let mut records_truncated = false;
        let mut remaining_genotype_cells = MAX_VARIANT_GENOTYPE_CELLS_PER_QUERY;
        let mut genotype_cells_loaded = 0_usize;
        let mut genotype_cells_omitted = 0_usize;
        let mut records_examined = 0_usize;
        let mut records_filtered = 0_usize;
        let selected_sample_indices = selected_sample_indices(&self.header, settings);
        for result in self.reader.records() {
            let record = result.map_err(|error| TrackError::Backend(error.to_string()))?;
            records_examined = records_examined.saturating_add(1);
            let (decoded, loaded, omitted) = decode_variant_record(
                &record,
                &self.header,
                &contig,
                remaining_genotype_cells,
                &selected_sample_indices,
            )?;
            if !variant_record_matches(&decoded, &self.header, settings) {
                records_filtered = records_filtered.saturating_add(1);
                continue;
            }
            if output.len() >= maximum_records {
                records_truncated = true;
                break;
            }
            remaining_genotype_cells = remaining_genotype_cells.saturating_sub(loaded);
            genotype_cells_loaded = genotype_cells_loaded.saturating_add(loaded);
            genotype_cells_omitted = genotype_cells_omitted.saturating_add(omitted);
            output.push(decoded);
        }
        Ok(VariantQuery {
            header: self.header.clone(),
            records: output,
            provenance: VariantQueryProvenance {
                records_truncated,
                records_examined,
                records_filtered,
                total_samples: self.total_samples,
                header_samples_omitted: self.omitted_header_samples,
                selected_samples: selected_sample_indices.len(),
                samples_omitted_by_subset: if settings.sample_subset_enabled {
                    self.header
                        .samples
                        .len()
                        .saturating_sub(selected_sample_indices.len())
                } else {
                    0
                },
                genotype_cells_loaded,
                genotype_cells_omitted,
                info_fields_truncated: self.omitted_info_schemas,
                format_fields_truncated: self.omitted_format_schemas,
            },
        })
    }

    pub fn search_ids(
        &mut self,
        query: &str,
        limit: usize,
    ) -> Result<VariantIdSearchResults, TrackError> {
        if self.id_index.is_none() {
            self.id_index = Some(build_variant_id_index(&self.path)?);
        }
        let index = self.id_index.as_ref().expect("variant ID index was built");
        let query = query.trim().to_lowercase();
        if query.is_empty() || limit == 0 {
            return Ok(VariantIdSearchResults {
                index_truncated: index.truncated,
                ..VariantIdSearchResults::default()
            });
        }
        let limit = limit.min(MAX_VARIANT_ID_SEARCH_RESULTS);
        let mut hits = Vec::new();
        let mut scanned = 0_usize;
        let mut scan_truncated = false;
        for (id, locations) in index.ids.range(query.clone()..) {
            if !id.starts_with(&query) {
                break;
            }
            for location in locations {
                if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break;
                }
                scanned = scanned.saturating_add(1);
                hits.push(location.clone());
            }
            if scan_truncated {
                break;
            }
        }
        hits.sort_by(|left, right| {
            let left_exact = left.matched_id == query;
            let right_exact = right.matched_id == query;
            right_exact
                .cmp(&left_exact)
                .then_with(|| left.matched_id.cmp(&right.matched_id))
                .then_with(|| left.contig.cmp(&right.contig))
                .then_with(|| left.start.cmp(&right.start))
                .then_with(|| left.end.cmp(&right.end))
        });
        hits.dedup();
        hits.truncate(limit);
        Ok(VariantIdSearchResults {
            hits,
            index_truncated: index.truncated,
            scan_truncated,
        })
    }
}

fn build_variant_id_index(path: &Path) -> Result<VariantIdIndex, TrackError> {
    let raw = path.to_string_lossy();
    let mut reader = if is_remote_url(&raw) {
        let url = url::Url::parse(&raw).map_err(|error| TrackError::Backend(error.to_string()))?;
        bcf::Reader::from_url(&url)
    } else {
        bcf::Reader::from_path(path)
    }
    .map_err(|error| TrackError::Backend(error.to_string()))?;
    let header = reader.header().clone();
    let mut index = VariantIdIndex::default();
    let mut records = 0_usize;
    let mut associations = 0_usize;
    for result in reader.records() {
        if records == MAX_VARIANT_ID_INDEX_RECORDS || associations == MAX_VARIANT_ID_ASSOCIATIONS {
            index.truncated = true;
            break;
        }
        records = records.saturating_add(1);
        let record = result.map_err(|error| TrackError::Backend(error.to_string()))?;
        let Some(rid) = record.rid() else {
            continue;
        };
        let contig = String::from_utf8_lossy(
            header
                .rid2name(rid)
                .map_err(|error| TrackError::Backend(error.to_string()))?,
        )
        .into_owned();
        let start = u64::try_from(record.pos()).map_err(|_| TrackError::Coordinate)?;
        let end = start.saturating_add(u64::try_from(record.rlen()).unwrap_or(1).max(1));
        let raw_ids = record.id();
        let ids = String::from_utf8_lossy(&raw_ids);
        let mut record_ids = ids
            .split(';')
            .map(str::trim)
            .filter(|id| !id.is_empty() && *id != ".")
            .map(|id| (id.to_lowercase(), id.to_owned()))
            .collect::<Vec<_>>();
        record_ids.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        record_ids.dedup_by(|left, right| left.0 == right.0);
        for (normalized_id, id) in record_ids {
            if associations == MAX_VARIANT_ID_ASSOCIATIONS {
                index.truncated = true;
                break;
            }
            index
                .ids
                .entry(normalized_id.clone())
                .or_default()
                .push(VariantIdSearchHit {
                    matched_id: normalized_id,
                    id,
                    contig: contig.clone(),
                    start,
                    end,
                });
            associations = associations.saturating_add(1);
        }
    }
    Ok(index)
}

fn decode_variant_header(header: &bcf::header::HeaderView) -> (VariantHeader, usize, usize, usize) {
    let raw_samples = header.samples();
    let omitted_samples = raw_samples.len().saturating_sub(MAX_VARIANT_HEADER_SAMPLES);
    let samples = raw_samples
        .into_iter()
        .take(MAX_VARIANT_HEADER_SAMPLES)
        .map(|sample| String::from_utf8_lossy(sample).into_owned())
        .collect();
    let mut filters = Vec::new();
    let mut info = Vec::new();
    let mut formats = Vec::new();
    for record in header.header_records() {
        match record {
            HeaderRecord::Filter { values, .. } => {
                let Some(id) = values.get("ID") else {
                    continue;
                };
                filters.push(VariantFilterSchema {
                    id: id.clone(),
                    description: values
                        .get("Description")
                        .map_or_else(String::new, |value| clean_header_value(value)),
                });
            }
            HeaderRecord::Info { values, .. } => {
                if let Some(schema) = variant_field_schema(
                    values.get("ID"),
                    values.get("Number"),
                    values.get("Type"),
                    values.get("Description"),
                ) {
                    info.push(schema);
                }
            }
            HeaderRecord::Format { values, .. } => {
                if let Some(schema) = variant_field_schema(
                    values.get("ID"),
                    values.get("Number"),
                    values.get("Type"),
                    values.get("Description"),
                ) {
                    formats.push(schema);
                }
            }
            HeaderRecord::Contig { .. }
            | HeaderRecord::Structured { .. }
            | HeaderRecord::Generic { .. } => {}
        }
    }
    let omitted_info = bound_variant_schemas(
        &mut info,
        MAX_VARIANT_INFO_FIELDS,
        &["END", "SVTYPE", "SVLEN", "CIPOS", "CIEND", "AF", "AC", "AN"],
    );
    let omitted_formats = bound_variant_schemas(
        &mut formats,
        MAX_VARIANT_FORMAT_FIELDS,
        &["GT", "GQ", "DP", "AD", "PS", "PL"],
    );
    (
        VariantHeader {
            samples,
            filters,
            info,
            formats,
        },
        omitted_info,
        omitted_formats,
        omitted_samples,
    )
}

fn bound_variant_schemas(
    schemas: &mut Vec<VariantFieldSchema>,
    maximum: usize,
    priorities: &[&str],
) -> usize {
    let omitted = schemas.len().saturating_sub(maximum);
    if omitted == 0 {
        return 0;
    }
    let mut bounded = Vec::with_capacity(maximum);
    for priority in priorities {
        if let Some(schema) = schemas.iter().find(|schema| schema.id == *priority) {
            bounded.push(schema.clone());
        }
    }
    for schema in schemas.iter() {
        if bounded.len() == maximum {
            break;
        }
        if !bounded.iter().any(|retained| retained.id == schema.id) {
            bounded.push(schema.clone());
        }
    }
    *schemas = bounded;
    omitted
}

fn variant_field_schema(
    id: Option<&String>,
    number: Option<&String>,
    field_type: Option<&String>,
    description: Option<&String>,
) -> Option<VariantFieldSchema> {
    let id = id?.clone();
    let value_type = match field_type.map(String::as_str) {
        Some("Flag") => VariantValueType::Flag,
        Some("Integer") => VariantValueType::Integer,
        Some("Float") => VariantValueType::Float,
        _ => VariantValueType::String,
    };
    Some(VariantFieldSchema {
        id,
        number: number.cloned().unwrap_or_else(|| ".".to_owned()),
        value_type,
        description: description.map_or_else(String::new, |value| clean_header_value(value)),
    })
}

fn clean_header_value(value: &str) -> String {
    value.trim_matches('"').to_owned()
}

fn decode_variant_record(
    record: &bcf::Record,
    header: &VariantHeader,
    contig: &str,
    remaining_genotype_cells: usize,
    selected_sample_indices: &[usize],
) -> Result<(VariantRecord, usize, usize), TrackError> {
    let start = u64::try_from(record.pos()).map_err(|_| TrackError::Coordinate)?;
    let alleles = record.alleles();
    let reference = alleles
        .first()
        .map_or_else(Vec::new, |value| value.to_vec());
    let alternates = alleles
        .iter()
        .skip(1)
        .map(|value| value.to_vec())
        .collect::<Vec<_>>();
    let info = decode_info_fields(record, &header.info)?;
    let declared_end = integer_info_value(&info, "END")
        .and_then(|value| u64::try_from(value).ok())
        .filter(|end| *end > start);
    let record_end = u64::try_from(record.end()).map_err(|_| TrackError::Coordinate)?;
    let end = declared_end
        .unwrap_or(record_end)
        .max(start.saturating_add(1));
    let filters = record
        .filters()
        .map(|id| String::from_utf8_lossy(&record.header().id_to_name(id)).into_owned())
        .collect();
    let (genotypes, loaded, omitted) = decode_genotypes(
        record,
        header,
        remaining_genotype_cells,
        selected_sample_indices,
    );
    let variant_types = alternates
        .iter()
        .map(|alternate| classify_variant(&reference, alternate))
        .collect();
    let breakends = alternates
        .iter()
        .enumerate()
        .filter_map(|(alternate_index, alternate)| {
            parse_breakend_partner(alternate_index, alternate)
        })
        .collect();
    let id = record.id();
    Ok((
        VariantRecord {
            contig: contig.to_owned(),
            start,
            end,
            id: (id.as_slice() != b".").then(|| String::from_utf8_lossy(&id).into_owned()),
            reference,
            alternates,
            quality: record.qual().is_finite().then_some(record.qual()),
            filters,
            info,
            genotypes,
            variant_types,
            breakends,
        },
        loaded,
        omitted,
    ))
}

fn decode_info_fields(
    record: &bcf::Record,
    schemas: &[VariantFieldSchema],
) -> Result<Vec<VariantField>, TrackError> {
    let mut fields = Vec::new();
    for schema in schemas {
        let decoded = match schema.value_type {
            VariantValueType::Flag => {
                let mut field = record.info(schema.id.as_bytes());
                field
                    .flag()
                    .map_err(|error| TrackError::Backend(error.to_string()))?
                    .then_some(VariantField {
                        key: schema.id.clone(),
                        value: VariantValue::Flag(true),
                        truncated: false,
                    })
            }
            VariantValueType::Integer => record
                .info(schema.id.as_bytes())
                .integer()
                .map_err(|error| TrackError::Backend(error.to_string()))?
                .map(|values| {
                    let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD;
                    VariantField {
                        key: schema.id.clone(),
                        value: VariantValue::Integers(
                            values
                                .iter()
                                .take(MAX_VARIANT_VALUES_PER_FIELD)
                                .map(|value| (!value.is_missing()).then_some(*value))
                                .collect(),
                        ),
                        truncated,
                    }
                }),
            VariantValueType::Float => record
                .info(schema.id.as_bytes())
                .float()
                .map_err(|error| TrackError::Backend(error.to_string()))?
                .map(|values| {
                    let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD;
                    VariantField {
                        key: schema.id.clone(),
                        value: VariantValue::Floats(
                            values
                                .iter()
                                .take(MAX_VARIANT_VALUES_PER_FIELD)
                                .map(|value| (!value.is_missing()).then_some(*value))
                                .collect(),
                        ),
                        truncated,
                    }
                }),
            VariantValueType::String => record
                .info(schema.id.as_bytes())
                .string()
                .map_err(|error| TrackError::Backend(error.to_string()))?
                .map(|values| {
                    let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD;
                    VariantField {
                        key: schema.id.clone(),
                        value: VariantValue::Strings(
                            values
                                .iter()
                                .take(MAX_VARIANT_VALUES_PER_FIELD)
                                .map(|value| bounded_variant_string(value))
                                .collect(),
                        ),
                        truncated,
                    }
                }),
        };
        if let Some(field) = decoded {
            fields.push(field);
        }
    }
    Ok(fields)
}

fn decode_genotypes(
    record: &bcf::Record,
    header: &VariantHeader,
    remaining_cells: usize,
    selected_sample_indices: &[usize],
) -> (Vec<VariantGenotype>, usize, usize) {
    let total = selected_sample_indices.len();
    let loaded = total
        .min(MAX_GENOTYPE_SAMPLES_PER_RECORD)
        .min(remaining_cells);
    let omitted = total.saturating_sub(loaded);
    if loaded == 0 || header.formats.is_empty() {
        return (Vec::new(), 0, omitted);
    }
    let genotype_values = record.genotypes().ok();
    let mut genotypes = selected_sample_indices
        .iter()
        .copied()
        .take(loaded)
        .map(|sample_index| {
            let alleles = genotype_values.as_ref().map_or_else(Vec::new, |values| {
                values
                    .get(sample_index)
                    .iter()
                    .enumerate()
                    .map(|(index, allele)| VariantGenotypeAllele {
                        index: allele.index(),
                        phased: index > 0
                            && matches!(
                                allele,
                                GenotypeAllele::Phased(_) | GenotypeAllele::PhasedMissing
                            ),
                    })
                    .collect()
            });
            VariantGenotype {
                sample_index,
                alleles,
                fields: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    for schema in header.formats.iter().filter(|schema| schema.id != "GT") {
        decode_format_field(record, schema, &mut genotypes);
    }
    (genotypes, loaded, omitted)
}

fn decode_format_field(
    record: &bcf::Record,
    schema: &VariantFieldSchema,
    genotypes: &mut [VariantGenotype],
) {
    match schema.value_type {
        VariantValueType::Flag => {}
        VariantValueType::Integer => {
            let Ok(values) = record.format(schema.id.as_bytes()).integer() else {
                return;
            };
            for genotype in genotypes {
                let Some(values) = values.get(genotype.sample_index) else {
                    continue;
                };
                let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD;
                genotype.fields.push(VariantField {
                    key: schema.id.clone(),
                    value: VariantValue::Integers(
                        values
                            .iter()
                            .take(MAX_VARIANT_VALUES_PER_FIELD)
                            .map(|value| (!value.is_missing()).then_some(*value))
                            .collect(),
                    ),
                    truncated,
                });
            }
        }
        VariantValueType::Float => {
            let Ok(values) = record.format(schema.id.as_bytes()).float() else {
                return;
            };
            for genotype in genotypes {
                let Some(values) = values.get(genotype.sample_index) else {
                    continue;
                };
                let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD;
                genotype.fields.push(VariantField {
                    key: schema.id.clone(),
                    value: VariantValue::Floats(
                        values
                            .iter()
                            .take(MAX_VARIANT_VALUES_PER_FIELD)
                            .map(|value| (!value.is_missing()).then_some(*value))
                            .collect(),
                    ),
                    truncated,
                });
            }
        }
        VariantValueType::String => {
            let Ok(values) = record.format(schema.id.as_bytes()).string() else {
                return;
            };
            for genotype in genotypes {
                let Some(value) = values.get(genotype.sample_index) else {
                    continue;
                };
                genotype.fields.push(VariantField {
                    key: schema.id.clone(),
                    value: VariantValue::Strings(vec![bounded_variant_string(value)]),
                    truncated: value.len() > MAX_VARIANT_STRING_BYTES,
                });
            }
        }
    }
}

fn selected_sample_indices(header: &VariantHeader, settings: &VariantTrackSettings) -> Vec<usize> {
    if !settings.sample_subset_enabled {
        return (0..header.samples.len()).collect();
    }
    let selected = settings
        .sample_subset
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    header
        .samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| selected.contains(sample.as_str()).then_some(index))
        .collect()
}

/// Applies the persisted record/cohort filter using explicit missing-value
/// rules. All configured clauses are combined with AND; multi-valued fields,
/// alternate types, and selected samples use ANY within one clause.
#[must_use]
pub fn variant_record_matches(
    record: &VariantRecord,
    header: &VariantHeader,
    settings: &VariantTrackSettings,
) -> bool {
    if let Some(minimum) = settings.minimum_quality {
        match record.quality {
            Some(quality) if quality < minimum => return false,
            None if settings.quality_missing == VariantMissingValueRule::Exclude => return false,
            Some(_) | None => {}
        }
    }
    let passes = record.filters.is_empty() || record.filters.iter().all(|filter| filter == "PASS");
    if settings.pass_only && !passes {
        return false;
    }
    if !settings.allowed_filters.is_empty() {
        let named_filter_allowed = record.filters.iter().any(|filter| {
            settings
                .allowed_filters
                .iter()
                .any(|allowed| allowed == filter)
        });
        let pass_allowed = passes
            && settings
                .allowed_filters
                .iter()
                .any(|allowed| allowed == "PASS");
        if !named_filter_allowed && !pass_allowed {
            return false;
        }
    }
    if !settings.variant_types.is_empty()
        && !record.variant_types.iter().any(|variant_type| {
            settings
                .variant_types
                .contains(&variant_type_group(variant_type))
        })
    {
        return false;
    }
    if settings.minimum_allele_frequency.is_some() || settings.maximum_allele_frequency.is_some() {
        let frequencies = numeric_field_values(record.info.iter().find(|field| field.key == "AF"));
        if frequencies.is_empty() {
            if settings.allele_frequency_missing == VariantMissingValueRule::Exclude {
                return false;
            }
        } else if !frequencies.into_iter().any(|frequency| {
            settings
                .minimum_allele_frequency
                .is_none_or(|minimum| frequency >= f64::from(minimum))
                && settings
                    .maximum_allele_frequency
                    .is_none_or(|maximum| frequency <= f64::from(maximum))
        }) {
            return false;
        }
    }
    if settings.genotype != VariantGenotypeFilter::Any
        && !record
            .genotypes
            .iter()
            .any(|genotype| genotype_matches(genotype, settings.genotype))
    {
        return false;
    }
    settings
        .field_predicates
        .iter()
        .all(|predicate| match predicate.scope {
            VariantFieldScope::Info => field_predicate_matches(
                record.info.iter().find(|field| field.key == predicate.key),
                predicate,
            ),
            VariantFieldScope::Format => {
                if predicate.key == "GT" {
                    return genotype_predicate_matches(&record.genotypes, predicate);
                }
                let fields = record.genotypes.iter().filter_map(|genotype| {
                    genotype
                        .fields
                        .iter()
                        .find(|field| field.key == predicate.key)
                });
                multiple_fields_match(fields, predicate)
            }
        })
        && selected_genotypes_belong_to_header(record, header)
}

fn genotype_predicate_matches(
    genotypes: &[VariantGenotype],
    predicate: &VariantFieldPredicate,
) -> bool {
    let values = genotypes
        .iter()
        .filter_map(genotype_filter_value)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return predicate.missing == VariantMissingValueRule::Include;
    }
    variant_value_matches(&VariantValue::Strings(values), &predicate.comparison)
}

fn genotype_filter_value(genotype: &VariantGenotype) -> Option<String> {
    if genotype.alleles.is_empty() || genotype.alleles.iter().all(|allele| allele.index.is_none()) {
        return None;
    }
    let mut value = String::new();
    for (position, allele) in genotype.alleles.iter().enumerate() {
        if position > 0 {
            value.push(if allele.phased { '|' } else { '/' });
        }
        if let Some(index) = allele.index {
            let _ = std::fmt::Write::write_fmt(&mut value, format_args!("{index}"));
        } else {
            value.push('.');
        }
    }
    Some(value)
}

fn selected_genotypes_belong_to_header(record: &VariantRecord, header: &VariantHeader) -> bool {
    record
        .genotypes
        .iter()
        .all(|genotype| genotype.sample_index < header.samples.len())
}

fn variant_type_group(variant_type: &VariantType) -> VariantTypeGroup {
    match variant_type {
        VariantType::Snv => VariantTypeGroup::Snv,
        VariantType::Mnv => VariantTypeGroup::Mnv,
        VariantType::Insertion => VariantTypeGroup::Insertion,
        VariantType::Deletion => VariantTypeGroup::Deletion,
        VariantType::Complex => VariantTypeGroup::Complex,
        VariantType::StructuralDeletion => VariantTypeGroup::StructuralDeletion,
        VariantType::StructuralInsertion => VariantTypeGroup::StructuralInsertion,
        VariantType::Inversion => VariantTypeGroup::Inversion,
        VariantType::Duplication => VariantTypeGroup::Duplication,
        VariantType::CopyNumber => VariantTypeGroup::CopyNumber,
        VariantType::Breakend => VariantTypeGroup::Breakend,
        VariantType::SpanningDeletion => VariantTypeGroup::SpanningDeletion,
        VariantType::Symbolic(_) => VariantTypeGroup::OtherSymbolic,
    }
}

fn genotype_matches(genotype: &VariantGenotype, filter: VariantGenotypeFilter) -> bool {
    let called = !genotype.alleles.is_empty()
        && genotype.alleles.iter().all(|allele| allele.index.is_some());
    let indices = genotype
        .alleles
        .iter()
        .filter_map(|allele| allele.index)
        .collect::<Vec<_>>();
    match filter {
        VariantGenotypeFilter::Any => true,
        VariantGenotypeFilter::Called => called,
        VariantGenotypeFilter::Missing => !called,
        VariantGenotypeFilter::HomReference => called && indices.iter().all(|index| *index == 0),
        VariantGenotypeFilter::Heterozygous => {
            called
                && indices
                    .first()
                    .is_some_and(|first| indices.iter().any(|index| index != first))
        }
        VariantGenotypeFilter::HomAlternate => {
            called
                && indices
                    .first()
                    .is_some_and(|first| *first > 0 && indices.iter().all(|index| index == first))
        }
        VariantGenotypeFilter::NonReference => called && indices.iter().any(|index| *index > 0),
        VariantGenotypeFilter::Phased => {
            called
                && genotype.alleles.len() > 1
                && genotype.alleles.iter().skip(1).all(|allele| allele.phased)
        }
        VariantGenotypeFilter::Unphased => {
            called
                && genotype.alleles.len() > 1
                && genotype.alleles.iter().skip(1).any(|allele| !allele.phased)
        }
    }
}

fn field_predicate_matches(
    field: Option<&VariantField>,
    predicate: &VariantFieldPredicate,
) -> bool {
    multiple_fields_match(field.into_iter(), predicate)
}

fn multiple_fields_match<'a>(
    fields: impl Iterator<Item = &'a VariantField>,
    predicate: &VariantFieldPredicate,
) -> bool {
    let fields = fields.collect::<Vec<_>>();
    let present = fields.iter().any(|field| field_has_value(field));
    if !present {
        return predicate.missing == VariantMissingValueRule::Include;
    }
    fields
        .into_iter()
        .any(|field| variant_value_matches(&field.value, &predicate.comparison))
}

fn field_has_value(field: &VariantField) -> bool {
    match &field.value {
        VariantValue::Flag(value) => *value,
        VariantValue::Integers(values) => values.iter().any(Option::is_some),
        VariantValue::Floats(values) => values.iter().any(Option::is_some),
        VariantValue::Strings(values) => !values.is_empty(),
    }
}

fn numeric_field_values(field: Option<&VariantField>) -> Vec<f64> {
    field.map_or_else(Vec::new, |field| match &field.value {
        VariantValue::Integers(values) => values
            .iter()
            .filter_map(|value| value.map(f64::from))
            .collect(),
        VariantValue::Floats(values) => values
            .iter()
            .filter_map(|value| value.map(f64::from))
            .collect(),
        VariantValue::Flag(_) | VariantValue::Strings(_) => Vec::new(),
    })
}

fn variant_value_matches(value: &VariantValue, comparison: &VariantFieldComparison) -> bool {
    match comparison {
        VariantFieldComparison::Exists => true,
        VariantFieldComparison::Equals(expected) => {
            scalar_strings(value).any(|value| value == *expected)
        }
        VariantFieldComparison::NotEquals(expected) => {
            scalar_strings(value).all(|value| value != *expected)
        }
        VariantFieldComparison::Contains(expected) => {
            scalar_strings(value).any(|value| value.contains(expected))
        }
        VariantFieldComparison::GreaterThan(expected) => {
            scalar_numbers(value).any(|value| value > *expected)
        }
        VariantFieldComparison::GreaterOrEqual(expected) => {
            scalar_numbers(value).any(|value| value >= *expected)
        }
        VariantFieldComparison::LessThan(expected) => {
            scalar_numbers(value).any(|value| value < *expected)
        }
        VariantFieldComparison::LessOrEqual(expected) => {
            scalar_numbers(value).any(|value| value <= *expected)
        }
    }
}

fn scalar_strings(value: &VariantValue) -> Box<dyn Iterator<Item = String> + '_> {
    match value {
        VariantValue::Flag(value) => Box::new(std::iter::once(value.to_string())),
        VariantValue::Integers(values) => {
            Box::new(values.iter().flatten().map(ToString::to_string))
        }
        VariantValue::Floats(values) => Box::new(values.iter().flatten().map(ToString::to_string)),
        VariantValue::Strings(values) => Box::new(values.iter().cloned()),
    }
}

fn scalar_numbers(value: &VariantValue) -> Box<dyn Iterator<Item = f64> + '_> {
    match value {
        VariantValue::Integers(values) => {
            Box::new(values.iter().filter_map(|value| value.map(f64::from)))
        }
        VariantValue::Floats(values) => {
            Box::new(values.iter().filter_map(|value| value.map(f64::from)))
        }
        VariantValue::Strings(values) => {
            Box::new(values.iter().filter_map(|value| value.parse().ok()))
        }
        VariantValue::Flag(_) => Box::new(std::iter::empty()),
    }
}

fn bounded_variant_string(value: &[u8]) -> String {
    let end = value.len().min(MAX_VARIANT_STRING_BYTES);
    String::from_utf8_lossy(&value[..end]).into_owned()
}

fn integer_info_value(fields: &[VariantField], key: &str) -> Option<i32> {
    fields.iter().find_map(|field| {
        (field.key == key)
            .then_some(&field.value)
            .and_then(|value| match value {
                VariantValue::Integers(values) => values.first().copied().flatten(),
                VariantValue::Flag(_) | VariantValue::Floats(_) | VariantValue::Strings(_) => None,
            })
    })
}

fn classify_variant(reference: &[u8], alternate: &[u8]) -> VariantType {
    if alternate == b"*" {
        return VariantType::SpanningDeletion;
    }
    if alternate.contains(&b'[') || alternate.contains(&b']') {
        return VariantType::Breakend;
    }
    if alternate.starts_with(b"<") && alternate.ends_with(b">") {
        let symbol = String::from_utf8_lossy(&alternate[1..alternate.len() - 1]).into_owned();
        return match symbol.as_str() {
            "DEL" => VariantType::StructuralDeletion,
            "INS" => VariantType::StructuralInsertion,
            "INV" => VariantType::Inversion,
            "DUP" | "DUP:TANDEM" => VariantType::Duplication,
            "CNV" => VariantType::CopyNumber,
            _ => VariantType::Symbolic(symbol),
        };
    }
    match (reference.len(), alternate.len()) {
        (1, 1) => VariantType::Snv,
        (reference_length, alternate_length) if reference_length == alternate_length => {
            VariantType::Mnv
        }
        (reference_length, alternate_length) if alternate_length > reference_length => {
            VariantType::Insertion
        }
        (reference_length, alternate_length) if alternate_length < reference_length => {
            VariantType::Deletion
        }
        _ => VariantType::Complex,
    }
}

fn parse_breakend_partner(alternate_index: usize, alternate: &[u8]) -> Option<BreakendPartner> {
    let alternate = std::str::from_utf8(alternate).ok()?;
    let (first, bracket) = alternate
        .char_indices()
        .find(|(_, character)| matches!(character, '[' | ']'))?;
    let second_offset = alternate[first + 1..].find(bracket)?;
    let second = first + 1 + second_offset;
    let partner = alternate.get(first + 1..second)?;
    let (contig, one_based_position) = partner.rsplit_once(':')?;
    let position = one_based_position.parse::<u64>().ok()?.checked_sub(1)?;
    Some(BreakendPartner {
        alternate_index,
        contig: contig.to_owned(),
        position,
        local_join_after_position: first > 0,
        partner_join_after_position: bracket == ']',
    })
}

pub struct BigWigSource {
    reader: BigWigBackend,
}

enum BigWigBackend {
    Local(BigWigRead<ReopenableFile>),
    Remote(Box<BigWigRead<HttpRangeReader>>),
}

impl BigWigSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        let reader = if is_remote_url(&raw) {
            let remote = HttpRangeReader::open(&raw, HttpRangeConfig::default())
                .map_err(|error| TrackError::Backend(error.to_string()))?;
            BigWigBackend::Remote(Box::new(
                BigWigRead::open(remote).map_err(|error| TrackError::Backend(error.to_string()))?,
            ))
        } else {
            BigWigBackend::Local(
                BigWigRead::open_file(path)
                    .map_err(|error| TrackError::Backend(error.to_string()))?,
            )
        };
        Ok(Self { reader })
    }

    pub fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<QuantitativeQuery, TrackError> {
        match &mut self.reader {
            BigWigBackend::Local(reader) => query_bigwig(reader, contig, start, end),
            BigWigBackend::Remote(reader) => query_bigwig(reader, contig, start, end),
        }
    }
}

pub struct BigBedSource {
    reader: BigBedBackend,
    format: BigBedFeatureFormat,
}

enum BigBedBackend {
    Local(BigBedRead<ReopenableFile>),
    Remote(Box<BigBedRead<HttpRangeReader>>),
}

impl BigBedSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::open_with_format(path, BigBedFeatureFormat::Bed)
    }

    pub fn open_with_format(
        path: impl AsRef<Path>,
        format: BigBedFeatureFormat,
    ) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        let reader = if is_remote_url(&raw) {
            let remote = HttpRangeReader::open(&raw, HttpRangeConfig::default())
                .map_err(|error| TrackError::Backend(error.to_string()))?;
            BigBedBackend::Remote(Box::new(
                BigBedRead::open(remote).map_err(|error| TrackError::Backend(error.to_string()))?,
            ))
        } else {
            BigBedBackend::Local(
                BigBedRead::open_file(path)
                    .map_err(|error| TrackError::Backend(error.to_string()))?,
            )
        };
        match &reader {
            BigBedBackend::Local(reader) => validate_bigbed_schema(reader, format)?,
            BigBedBackend::Remote(reader) => validate_bigbed_schema(reader, format)?,
        }
        Ok(Self { reader, format })
    }

    pub fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<FeatureRecord>, TrackError> {
        match &mut self.reader {
            BigBedBackend::Local(reader) => query_bigbed(reader, self.format, contig, start, end),
            BigBedBackend::Remote(reader) => query_bigbed(reader, self.format, contig, start, end),
        }
    }

    pub fn search(
        &mut self,
        query: &str,
        limit: usize,
    ) -> Result<FeatureSearchResults, TrackError> {
        match &mut self.reader {
            BigBedBackend::Local(reader) => search_bigbed(reader, self.format, query, limit),
            BigBedBackend::Remote(reader) => search_bigbed(reader, self.format, query, limit),
        }
    }

    pub fn adjacent_feature(
        &mut self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Result<Option<FeatureRecord>, TrackError> {
        match &mut self.reader {
            BigBedBackend::Local(reader) => {
                adjacent_bigbed_feature(reader, self.format, contig, position, forward)
            }
            BigBedBackend::Remote(reader) => {
                adjacent_bigbed_feature(reader, self.format, contig, position, forward)
            }
        }
    }
}

fn validate_bigbed_schema<R>(
    reader: &BigBedRead<R>,
    format: BigBedFeatureFormat,
) -> Result<(), TrackError> {
    let expected = match format {
        BigBedFeatureFormat::Bed => return Ok(()),
        BigBedFeatureFormat::BigGenePred => 20,
        BigBedFeatureFormat::BigNarrowPeak => 10,
    };
    let actual = reader.info().header.field_count;
    if actual != expected {
        return Err(TrackError::InvalidSchema(format!(
            "{} requires exactly {expected} BigBed fields, but the file declares {actual}",
            bigbed_format_name(format)
        )));
    }
    Ok(())
}

const fn bigbed_format_name(format: BigBedFeatureFormat) -> &'static str {
    match format {
        BigBedFeatureFormat::Bed => "BigBed",
        BigBedFeatureFormat::BigGenePred => "bigGenePred",
        BigBedFeatureFormat::BigNarrowPeak => "bigNarrowPeak",
    }
}

fn query_bigwig<R: BBIFileRead>(
    reader: &mut BigWigRead<R>,
    contig: &str,
    start: u64,
    end: u64,
) -> Result<QuantitativeQuery, TrackError> {
    let start = u32::try_from(start).map_err(|_| TrackError::Coordinate)?;
    let end = u32::try_from(end).map_err(|_| TrackError::Coordinate)?;
    let mut query = QuantitativeQuery::default();
    let intervals = reader
        .get_interval(contig, start, end)
        .map_err(|error| TrackError::Backend(error.to_string()))?;
    for result in intervals {
        let value = result.map_err(|error| TrackError::Backend(error.to_string()))?;
        if !retain_quantitative_record(
            &mut query,
            QuantitativeRecord {
                start: u64::from(value.start),
                end: u64::from(value.end),
                value: value.value,
            },
            MAX_QUANTITATIVE_RECORDS_PER_QUERY,
        ) {
            break;
        }
    }
    query.provenance.records_retained = query.records.len();
    Ok(query)
}

fn retain_quantitative_record(
    query: &mut QuantitativeQuery,
    record: QuantitativeRecord,
    maximum_records: usize,
) -> bool {
    query.provenance.records_examined = query.provenance.records_examined.saturating_add(1);
    if query.records.len() == maximum_records {
        query.provenance.records_truncated = true;
        return false;
    }
    if !record.value.is_finite() {
        query.provenance.non_finite_records = query.provenance.non_finite_records.saturating_add(1);
    }
    query.records.push(record);
    query.provenance.records_retained = query.records.len();
    true
}

fn query_bigbed<R: BBIFileRead>(
    reader: &mut BigBedRead<R>,
    format: BigBedFeatureFormat,
    contig: &str,
    start: u64,
    end: u64,
) -> Result<Vec<FeatureRecord>, TrackError> {
    let start = u32::try_from(start).map_err(|_| TrackError::Coordinate)?;
    let end = u32::try_from(end).map_err(|_| TrackError::Coordinate)?;
    let intervals = reader
        .get_interval(contig, start, end)
        .map_err(|error| TrackError::Backend(error.to_string()))?;
    let mut records = Vec::new();
    for result in intervals {
        ensure_bigbed_query_capacity(records.len())?;
        let entry = result.map_err(|error| TrackError::Backend(error.to_string()))?;
        records.push(parse_bigbed_record(contig, &entry, format)?);
    }
    Ok(records)
}

fn ensure_bigbed_query_capacity(current_records: usize) -> Result<(), TrackError> {
    if current_records >= MAX_FEATURE_RECORDS_PER_QUERY {
        return Err(TrackError::ResourceLimit(format!(
            "BigBed query exceeds the {MAX_FEATURE_RECORDS_PER_QUERY}-record limit"
        )));
    }
    Ok(())
}

fn search_bigbed<R: BBIFileRead>(
    reader: &mut BigBedRead<R>,
    format: BigBedFeatureFormat,
    query: &str,
    limit: usize,
) -> Result<FeatureSearchResults, TrackError> {
    let query = normalized_feature_search_term(query);
    let limit = limit.min(MAX_FEATURE_SEARCH_RESULTS);
    if query.is_empty() || limit == 0 {
        return Ok(FeatureSearchResults::default());
    }
    let chromosomes = reader
        .chroms()
        .iter()
        .map(|chromosome| (chromosome.name.clone(), chromosome.length))
        .collect::<Vec<_>>();
    let mut candidates = Vec::<(bool, String, FeatureRecord)>::new();
    let mut scanned = 0_usize;
    let mut scan_truncated = false;
    'chromosomes: for (contig, length) in chromosomes {
        let intervals = reader
            .get_interval(&contig, 0, length)
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        for result in intervals {
            if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                scan_truncated = true;
                break 'chromosomes;
            }
            scanned = scanned.saturating_add(1);
            let entry = result.map_err(|error| TrackError::Backend(error.to_string()))?;
            let record = parse_bigbed_record(&contig, &entry, format)?;
            let Some(matched_term) = feature_search_terms(&record)
                .into_iter()
                .filter(|term| term.starts_with(&query))
                .min_by(|left, right| {
                    (left != &query)
                        .cmp(&(right != &query))
                        .then_with(|| left.cmp(right))
                })
            else {
                continue;
            };
            candidates.push((matched_term == query, matched_term, record));
            candidates.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.cmp(&right.1))
                    .then_with(|| left.2.contig.cmp(&right.2.contig))
                    .then_with(|| left.2.start.cmp(&right.2.start))
                    .then_with(|| left.2.end.cmp(&right.2.end))
                    .then_with(|| left.2.name.cmp(&right.2.name))
            });
            candidates.truncate(limit);
        }
    }
    Ok(FeatureSearchResults {
        hits: candidates
            .into_iter()
            .map(|(_, matched_term, record)| FeatureSearchHit {
                matched_term,
                record,
            })
            .collect(),
        index_truncated: false,
        scan_truncated,
    })
}

fn adjacent_bigbed_feature<R: BBIFileRead>(
    reader: &mut BigBedRead<R>,
    format: BigBedFeatureFormat,
    contig: &str,
    position: u64,
    forward: bool,
) -> Result<Option<FeatureRecord>, TrackError> {
    let Some(chromosome_length) = reader
        .chroms()
        .iter()
        .find(|chromosome| chromosome.name == contig)
        .map(|chromosome| chromosome.length)
    else {
        return Ok(None);
    };
    let position = u32::try_from(position)
        .unwrap_or(u32::MAX)
        .min(chromosome_length);
    if forward {
        if position == chromosome_length {
            return Ok(None);
        }
        let intervals = reader
            .get_interval(contig, position, chromosome_length)
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        for (scanned, result) in intervals.enumerate() {
            if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                return Err(TrackError::ResourceLimit(format!(
                    "BigBed adjacent-feature scan exceeds {MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS} records"
                )));
            }
            let entry = result.map_err(|error| TrackError::Backend(error.to_string()))?;
            if entry.start >= position {
                return parse_bigbed_record(contig, &entry, format).map(Some);
            }
        }
        return Ok(None);
    }

    let mut chunk_end = position;
    let mut scanned = 0_usize;
    while chunk_end > 0 {
        let chunk_start = chunk_end.saturating_sub(BIGBED_BACKWARD_NAVIGATION_WINDOW_BASES);
        let intervals = reader
            .get_interval(contig, chunk_start, chunk_end)
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        let mut nearest = None;
        for result in intervals {
            if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                return Err(TrackError::ResourceLimit(format!(
                    "BigBed adjacent-feature scan exceeds {MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS} records"
                )));
            }
            scanned = scanned.saturating_add(1);
            let entry = result.map_err(|error| TrackError::Backend(error.to_string()))?;
            if entry.end <= position {
                nearest = Some(parse_bigbed_record(contig, &entry, format)?);
            }
        }
        if nearest.is_some() {
            return Ok(nearest);
        }
        chunk_end = chunk_start;
    }
    Ok(None)
}

fn parse_bigbed_record(
    contig: &str,
    entry: &bigtools::BedEntry,
    format: BigBedFeatureFormat,
) -> Result<FeatureRecord, TrackError> {
    if entry.rest.len() > MAX_FEATURE_LINE_BYTES {
        return Err(TrackError::ResourceLimit(format!(
            "BigBed record at {contig}:{} exceeds {MAX_FEATURE_LINE_BYTES} bytes",
            entry.start
        )));
    }
    let start = u64::from(entry.start);
    let end = u64::from(entry.end);
    if start >= end {
        return Err(TrackError::MalformedRecord {
            contig: contig.to_owned(),
            start,
            message: "feature start must be less than end".to_owned(),
        });
    }
    let parsed = match format {
        BigBedFeatureFormat::Bed => {
            Ok(parse_generic_bigbed_record(contig, start, end, &entry.rest))
        }
        BigBedFeatureFormat::BigGenePred => {
            parse_big_gene_pred_record(contig, start, end, &entry.rest)
        }
        BigBedFeatureFormat::BigNarrowPeak => {
            parse_big_narrow_peak_record(contig, start, end, &entry.rest)
        }
    };
    parsed.map_err(|message| TrackError::MalformedRecord {
        contig: contig.to_owned(),
        start,
        message,
    })
}

fn parse_generic_bigbed_record(contig: &str, start: u64, end: u64, rest: &str) -> FeatureRecord {
    let fields = rest.split('\t').collect::<Vec<_>>();
    FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: fields
            .first()
            .filter(|value| !value.is_empty() && **value != ".")
            .map(ToString::to_string),
        feature_type: None,
        score: fields.get(1).and_then(|value| value.parse().ok()),
        strand: fields
            .get(2)
            .map_or(Strand::Unstranded, |value| parse_strand(value)),
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn parse_big_gene_pred_record(
    contig: &str,
    start: u64,
    end: u64,
    rest: &str,
) -> Result<FeatureRecord, String> {
    let fields = rest.split('\t').collect::<Vec<_>>();
    if fields.len() != 17 {
        return Err(format!(
            "bigGenePred requires 17 non-coordinate fields, found {}",
            fields.len()
        ));
    }
    let transcript_name = fields[0];
    if transcript_name.is_empty() || transcript_name == "." {
        return Err("bigGenePred transcript name must not be empty".to_owned());
    }
    let score = fields[1]
        .parse::<u16>()
        .map_err(|_| format!("invalid bigGenePred integer score: {}", fields[1]))?;
    if score > 1000 {
        return Err("bigGenePred score must be an integer between 0 and 1000".to_owned());
    }
    let strand = parse_strand(fields[2]);
    if strand == Strand::Unstranded {
        return Err("bigGenePred strand must be + or -".to_owned());
    }
    let coding_start = parse_u64(fields[3], "bigGenePred thick start")?;
    let coding_end = parse_u64(fields[4], "bigGenePred thick end")?;
    if coding_start < start || coding_end > end || coding_start > coding_end {
        return Err(
            "bigGenePred thick interval must be contained within the transcript".to_owned(),
        );
    }
    let item_rgb = fields[5]
        .parse::<u32>()
        .map_err(|_| format!("invalid bigGenePred reserved/itemRgb value: {}", fields[5]))?;
    let block_count = fields[6]
        .parse::<usize>()
        .map_err(|_| format!("invalid bigGenePred block count: {}", fields[6]))?;
    if block_count == 0 || block_count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "bigGenePred block count must be between 1 and {MAX_FEATURE_BLOCKS_PER_RECORD}"
        ));
    }
    let block_sizes = comma_numbers_exact(fields[7], "bigGenePred block size", block_count)?;
    let block_starts = comma_numbers_exact(fields[8], "bigGenePred block start", block_count)?;
    let span = end - start;
    let mut blocks = Vec::with_capacity(block_count);
    for (size, relative_start) in block_sizes.into_iter().zip(block_starts) {
        if size == 0 || relative_start >= span || size > span.saturating_sub(relative_start) {
            return Err(
                "bigGenePred blocks must be non-empty and contained within the transcript"
                    .to_owned(),
            );
        }
        let block_start = start + relative_start;
        let block_end = block_start + size;
        if blocks
            .last()
            .is_some_and(|(_, previous_end)| block_start < *previous_end)
        {
            return Err("bigGenePred blocks must be sorted and non-overlapping".to_owned());
        }
        blocks.push((block_start, block_end));
    }
    for (label, value) in [
        ("CDS start status", fields[10]),
        ("CDS end status", fields[11]),
    ] {
        if !matches!(value, "none" | "unk" | "incmpl" | "cmpl") {
            return Err(format!("invalid bigGenePred {label}: {value}"));
        }
    }
    let mut exon_frames = Vec::with_capacity(block_count);
    for value in fields[12].trim_end_matches(',').split(',') {
        if exon_frames.len() == block_count {
            return Err("bigGenePred exon frame count does not match block count".to_owned());
        }
        let value = value
            .parse::<i8>()
            .map_err(|_| format!("invalid bigGenePred exon frame: {value}"))?;
        if !matches!(value, -1..=2) {
            return Err(format!("invalid bigGenePred exon frame: {value}"));
        }
        exon_frames.push(value);
    }
    if exon_frames.len() != block_count {
        return Err("bigGenePred exon frame count does not match block count".to_owned());
    }
    let name2 = fields[9];
    let transcript_type = fields[13];
    let gene_name = fields[14];
    let gene_name2 = fields[15];
    let gene_type = fields[16];
    let display_name = [gene_name, name2, transcript_name]
        .into_iter()
        .find(|value| !value.is_empty() && *value != ".")
        .unwrap_or(transcript_name);
    let mut attributes = vec![
        ("transcript_id".to_owned(), transcript_name.to_owned()),
        ("source_format".to_owned(), "bigGenePred".to_owned()),
        ("item_rgb".to_owned(), item_rgb.to_string()),
        ("cds_start_status".to_owned(), fields[10].to_owned()),
        ("cds_end_status".to_owned(), fields[11].to_owned()),
        ("exon_frames".to_owned(), fields[12].to_owned()),
    ];
    for (key, value) in [
        ("name2", name2),
        ("transcript_type", transcript_type),
        ("gene_name", gene_name),
        ("gene_name2", gene_name2),
        ("gene_type", gene_type),
    ] {
        if !value.is_empty() && value != "." {
            attributes.push((key.to_owned(), value.to_owned()));
        }
    }
    Ok(FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: Some(display_name.to_owned()),
        feature_type: Some("transcript".to_owned()),
        score: Some(f32::from(score)),
        strand,
        blocks,
        coding_interval: (coding_start < coding_end).then_some((coding_start, coding_end)),
        peak: None,
        attributes,
    })
}

fn parse_big_narrow_peak_record(
    contig: &str,
    start: u64,
    end: u64,
    rest: &str,
) -> Result<FeatureRecord, String> {
    let fields = rest.split('\t').collect::<Vec<_>>();
    if fields.len() != 7 {
        return Err(format!(
            "bigNarrowPeak requires 7 non-coordinate fields, found {}",
            fields.len()
        ));
    }
    fields[1]
        .parse::<u16>()
        .ok()
        .filter(|score| *score <= 1000)
        .ok_or_else(|| "bigNarrowPeak score must be an integer between 0 and 1000".to_owned())?;
    if !matches!(fields[2], "+" | "-" | ".") {
        return Err("bigNarrowPeak strand must be +, -, or .".to_owned());
    }
    parse_narrow_peak_fields(contig, start, end, &fields, "bigNarrowPeak")
}

fn is_remote_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn parse_bed(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() < 3 {
        return Err("BED requires at least three tab-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "BED start")?;
    let end = parse_u64(fields[2], "BED end")?;
    if start >= end {
        return Err("BED start must be less than end".to_owned());
    }
    let blocks = parse_bed_blocks(&fields, start)?;
    let coding_interval = if fields.len() >= 8 {
        let coding_start = parse_u64(fields[6], "BED thick start")?;
        let coding_end = parse_u64(fields[7], "BED thick end")?;
        if coding_start < start || coding_end > end || coding_start > coding_end {
            return Err("BED thick interval must be contained within the feature".to_owned());
        }
        (coding_start < coding_end).then_some((coding_start, coding_end))
    } else {
        None
    };
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start,
        end,
        name: fields
            .get(3)
            .filter(|value| **value != ".")
            .map(ToString::to_string),
        score: fields.get(4).and_then(|value| value.parse().ok()),
        strand: fields
            .get(5)
            .map_or(Strand::Unstranded, |value| parse_strand(value)),
        feature_type: None,
        blocks,
        coding_interval,
        peak: None,
        attributes: Vec::new(),
    })
}

fn parse_bed_blocks(fields: &[&str], feature_start: u64) -> Result<Vec<(u64, u64)>, String> {
    if fields.len() < 12 {
        return Ok(Vec::new());
    }
    let count = fields[9]
        .parse::<usize>()
        .map_err(|_| "invalid BED block count".to_owned())?;
    if count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "BED block count exceeds the {MAX_FEATURE_BLOCKS_PER_RECORD}-block limit"
        ));
    }
    let sizes = comma_numbers(fields[10], "BED block size")?;
    let starts = comma_numbers(fields[11], "BED block start")?;
    if count != sizes.len() || count != starts.len() {
        return Err("BED block count does not match block arrays".to_owned());
    }
    Ok(sizes
        .into_iter()
        .zip(starts)
        .map(|(size, start)| {
            let start = feature_start.saturating_add(start);
            (start, start.saturating_add(size))
        })
        .collect())
}

fn parse_gff(line: &str, gtf: bool) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 9 {
        return Err("GFF/GTF requires nine tab-separated columns".to_owned());
    }
    let one_based_start = parse_u64(fields[3], "GFF start")?;
    let end = parse_u64(fields[4], "GFF end")?;
    if one_based_start == 0 || one_based_start > end {
        return Err("GFF coordinates must be one-based and forward".to_owned());
    }
    let attributes = parse_attributes(fields[8], gtf);
    let name = attribute_value(&attributes, &["Name", "gene_name", "gene_id", "ID"]);
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start: one_based_start - 1,
        end,
        name,
        feature_type: Some(fields[2].to_owned()),
        score: (fields[5] != ".").then(|| fields[5].parse().ok()).flatten(),
        strand: parse_strand(fields[6]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes,
    })
}

#[allow(clippy::too_many_lines)]
fn parse_gene_pred(line: &str, ref_flat: bool) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    let minimum_fields = if ref_flat { 11 } else { 10 };
    let extended_fields = if ref_flat { None } else { Some(15) };
    let has_bin = fields.len() == minimum_fields + 1
        || extended_fields.is_some_and(|count| fields.len() == count + 1);
    let offset = usize::from(has_bin);
    let available = fields.len().saturating_sub(offset);
    if available != minimum_fields && extended_fields != Some(available) {
        return Err(if ref_flat {
            "refFlat requires 11 tab-separated columns, with an optional leading bin".to_owned()
        } else {
            "genePred requires 10 or 15 tab-separated columns, with an optional leading bin"
                .to_owned()
        });
    }
    if has_bin {
        parse_u64(fields[0], "genePred bin")?;
    }
    let gene_name = ref_flat.then_some(fields[offset]);
    let transcript_column = offset + usize::from(ref_flat);
    let transcript_name = fields[transcript_column];
    let contig = fields[transcript_column + 1];
    let strand = parse_strand(fields[transcript_column + 2]);
    if strand == Strand::Unstranded {
        return Err("genePred strand must be + or -".to_owned());
    }
    let start = parse_u64(fields[transcript_column + 3], "genePred transcript start")?;
    let end = parse_u64(fields[transcript_column + 4], "genePred transcript end")?;
    let coding_start = parse_u64(fields[transcript_column + 5], "genePred CDS start")?;
    let coding_end = parse_u64(fields[transcript_column + 6], "genePred CDS end")?;
    if start >= end {
        return Err("genePred transcript start must be less than end".to_owned());
    }
    if coding_start < start || coding_end > end || coding_start > coding_end {
        return Err("genePred CDS interval must be contained within the transcript".to_owned());
    }
    let exon_count = fields[transcript_column + 7]
        .parse::<usize>()
        .map_err(|_| "invalid genePred exon count".to_owned())?;
    if exon_count == 0 || exon_count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "genePred exon count must be between 1 and {MAX_FEATURE_BLOCKS_PER_RECORD}"
        ));
    }
    let exon_starts = comma_numbers(fields[transcript_column + 8], "genePred exon start")?;
    let exon_ends = comma_numbers(fields[transcript_column + 9], "genePred exon end")?;
    if exon_starts.len() != exon_count || exon_ends.len() != exon_count {
        return Err("genePred exon count does not match exon arrays".to_owned());
    }
    let mut blocks = Vec::with_capacity(exon_count);
    for (exon_start, exon_end) in exon_starts.into_iter().zip(exon_ends) {
        if exon_start < start || exon_start >= exon_end || exon_end > end {
            return Err(
                "genePred exon must be non-empty and contained within the transcript".to_owned(),
            );
        }
        if blocks
            .last()
            .is_some_and(|(_, previous_end)| exon_start < *previous_end)
        {
            return Err("genePred exons must be sorted and non-overlapping".to_owned());
        }
        blocks.push((exon_start, exon_end));
    }
    let mut attributes = vec![
        ("transcript_id".to_owned(), transcript_name.to_owned()),
        (
            "source_format".to_owned(),
            if ref_flat { "refFlat" } else { "genePred" }.to_owned(),
        ),
    ];
    if let Some(gene_name) = gene_name {
        attributes.push(("gene_name".to_owned(), gene_name.to_owned()));
    }
    let score = if !ref_flat && available == 15 {
        let score_column = transcript_column + 10;
        let score = fields[score_column]
            .parse::<i32>()
            .map_err(|_| "invalid extended genePred score".to_owned())?;
        for (label, value) in [
            ("CDS start status", fields[score_column + 2]),
            ("CDS end status", fields[score_column + 3]),
        ] {
            if !matches!(value, "none" | "unk" | "incmpl" | "cmpl") {
                return Err(format!("invalid extended genePred {label}: {value}"));
            }
        }
        let exon_frames = fields[score_column + 4]
            .trim_end_matches(',')
            .split(',')
            .map(|value| {
                value
                    .parse::<i8>()
                    .map_err(|_| format!("invalid extended genePred exon frame: {value}"))
                    .and_then(|value| {
                        matches!(value, -1..=2)
                            .then_some(value)
                            .ok_or_else(|| format!("invalid extended genePred exon frame: {value}"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if exon_frames.len() != exon_count {
            return Err("extended genePred exon frame count does not match exon count".to_owned());
        }
        attributes.extend([
            ("gene_name".to_owned(), fields[score_column + 1].to_owned()),
            (
                "cds_start_status".to_owned(),
                fields[score_column + 2].to_owned(),
            ),
            (
                "cds_end_status".to_owned(),
                fields[score_column + 3].to_owned(),
            ),
            (
                "exon_frames".to_owned(),
                fields[score_column + 4].to_owned(),
            ),
        ]);
        Some(score as f32)
    } else {
        None
    };
    Ok(FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: Some(gene_name.unwrap_or(transcript_name).to_owned()),
        feature_type: Some("transcript".to_owned()),
        score,
        strand,
        blocks,
        coding_interval: (coding_start < coding_end).then_some((coding_start, coding_end)),
        peak: None,
        attributes,
    })
}

fn parse_narrow_peak(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 10 {
        return Err("narrowPeak requires exactly 10 tab-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "narrowPeak start")?;
    let end = parse_u64(fields[2], "narrowPeak end")?;
    if start >= end {
        return Err("narrowPeak start must be less than end".to_owned());
    }
    parse_narrow_peak_fields(fields[0], start, end, &fields[3..], "narrowPeak")
}

fn parse_broad_peak(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 9 {
        return Err("broadPeak requires exactly 9 tab-separated columns".to_owned());
    }
    let start = parse_u64(fields[1], "broadPeak start")?;
    let end = parse_u64(fields[2], "broadPeak end")?;
    if start >= end {
        return Err("broadPeak start must be less than end".to_owned());
    }
    let score = fields[4]
        .parse::<u16>()
        .ok()
        .filter(|score| *score <= 1000)
        .ok_or_else(|| "broadPeak score must be an integer between 0 and 1000".to_owned())?;
    if !matches!(fields[5], "+" | "-" | ".") {
        return Err("broadPeak strand must be +, -, or .".to_owned());
    }
    let signal_value = parse_finite_f32(fields[6], "broadPeak signal value")?;
    let p_value = parse_optional_peak_value(fields[7], "broadPeak p-value")?;
    let q_value = parse_optional_peak_value(fields[8], "broadPeak q-value")?;
    Ok(FeatureRecord {
        contig: fields[0].to_owned(),
        start,
        end,
        name: (fields[3] != ".").then(|| fields[3].to_owned()),
        feature_type: Some("broadPeak".to_owned()),
        score: Some(f32::from(score)),
        strand: parse_strand(fields[5]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: Some(PeakMetrics {
            signal_value,
            p_value,
            q_value,
            summit: None,
        }),
        attributes: vec![("source_format".to_owned(), "broadPeak".to_owned())],
    })
}

fn is_psl_header_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("psLayout ")
        || trimmed.starts_with("match ")
        || trimmed.chars().all(|character| {
            character.is_ascii_whitespace() || matches!(character, '-' | '+' | '|')
        })
}

#[allow(clippy::too_many_lines)]
fn parse_psl(line: &str) -> Result<FeatureRecord, String> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let has_bin = fields.len() == 22;
    if fields.len() != 21 && !has_bin {
        return Err(
            "PSL requires exactly 21 whitespace-separated columns, with an optional leading bin"
                .to_owned(),
        );
    }
    let offset = usize::from(has_bin);
    if has_bin {
        parse_u64(fields[0], "PSL bin")?;
    }
    let number = |column: usize, label: &str| parse_u64(fields[offset + column], label);
    let matches = number(0, "PSL matches")?;
    let mismatches = number(1, "PSL mismatches")?;
    let repeat_matches = number(2, "PSL repeat matches")?;
    let n_count = number(3, "PSL N count")?;
    let query_insertions = number(4, "PSL query insertion count")?;
    let query_inserted_bases = number(5, "PSL query inserted bases")?;
    let target_insertions = number(6, "PSL target insertion count")?;
    let target_inserted_bases = number(7, "PSL target inserted bases")?;
    let raw_strand = fields[offset + 8];
    if raw_strand.is_empty()
        || raw_strand.len() > 2
        || !raw_strand.bytes().all(|value| matches!(value, b'+' | b'-'))
    {
        return Err("PSL strand must contain one or two + or - characters".to_owned());
    }
    let query_reverse = raw_strand.as_bytes()[0] == b'-';
    let target_reverse = raw_strand.as_bytes().get(1) == Some(&b'-');
    let query_name = fields[offset + 9];
    if query_name.is_empty() || query_name == "." {
        return Err("PSL query name must not be empty".to_owned());
    }
    let query_size = number(10, "PSL query size")?;
    let query_start = number(11, "PSL query start")?;
    let query_end = number(12, "PSL query end")?;
    let target_name = fields[offset + 13];
    let target_size = number(14, "PSL target size")?;
    let target_start = number(15, "PSL target start")?;
    let target_end = number(16, "PSL target end")?;
    if query_start >= query_end || query_end > query_size {
        return Err("PSL query interval must be non-empty and contained by qSize".to_owned());
    }
    if target_start >= target_end || target_end > target_size {
        return Err("PSL target interval must be non-empty and contained by tSize".to_owned());
    }
    let block_count = usize::try_from(number(17, "PSL block count")?)
        .map_err(|_| "PSL block count cannot be represented".to_owned())?;
    if block_count == 0 || block_count > MAX_FEATURE_BLOCKS_PER_RECORD {
        return Err(format!(
            "PSL block count must be between 1 and {MAX_FEATURE_BLOCKS_PER_RECORD}"
        ));
    }
    let block_sizes = comma_numbers_exact(fields[offset + 18], "PSL block size", block_count)?;
    let query_starts =
        comma_numbers_exact(fields[offset + 19], "PSL query block start", block_count)?;
    let target_starts =
        comma_numbers_exact(fields[offset + 20], "PSL target block start", block_count)?;
    let block_bases = block_sizes.iter().try_fold(0_u64, |total, size| {
        total
            .checked_add(*size)
            .ok_or_else(|| "PSL block bases overflow their coordinate range".to_owned())
    })?;
    let query_span = query_end - query_start;
    let target_span = target_end - target_start;
    let nucleotide_query_span = block_bases
        .checked_add(query_inserted_bases)
        .ok_or_else(|| "PSL query span overflows its coordinate range".to_owned())?;
    let nucleotide_target_span = block_bases
        .checked_add(target_inserted_bases)
        .ok_or_else(|| "PSL target span overflows its coordinate range".to_owned())?;
    if query_span != nucleotide_query_span || target_span != nucleotide_target_span {
        let translated_block_bases = block_bases.checked_mul(3).ok_or_else(|| {
            "PSL translated block bases overflow their coordinate range".to_owned()
        })?;
        let translated_query_span = translated_block_bases
            .checked_add(query_inserted_bases)
            .ok_or_else(|| "PSL translated query span overflows".to_owned())?;
        let translated_target_span = translated_block_bases
            .checked_add(target_inserted_bases)
            .ok_or_else(|| "PSL translated target span overflows".to_owned())?;
        if query_span == translated_query_span || target_span == translated_target_span {
            return Err(
                "translated protein PSL is not supported by the nucleotide block model".to_owned(),
            );
        }
        return Err("PSL block and insertion sizes do not match the declared spans".to_owned());
    }
    let mut blocks = Vec::with_capacity(block_count);
    for ((size, query_block_start), target_block_start) in block_sizes
        .iter()
        .copied()
        .zip(query_starts.iter().copied())
        .zip(target_starts.iter().copied())
    {
        if size == 0 {
            return Err("PSL blocks must be non-empty".to_owned());
        }
        let query_block_end = query_block_start
            .checked_add(size)
            .ok_or_else(|| "PSL query block overflows its coordinate range".to_owned())?;
        let positive_query_start = if query_reverse {
            query_size.checked_sub(query_block_end).ok_or_else(|| {
                "PSL reverse query block falls outside the query sequence".to_owned()
            })?
        } else {
            query_block_start
        };
        if positive_query_start < query_start
            || positive_query_start
                .checked_add(size)
                .is_none_or(|end| end > query_end)
        {
            return Err("PSL query block must be contained by the query interval".to_owned());
        }
        let target_block_end = target_block_start
            .checked_add(size)
            .ok_or_else(|| "PSL target block overflows its coordinate range".to_owned())?;
        let positive_target_start = if target_reverse {
            target_size.checked_sub(target_block_end).ok_or_else(|| {
                "PSL reverse target block falls outside the target sequence".to_owned()
            })?
        } else {
            target_block_start
        };
        let positive_target_end = positive_target_start
            .checked_add(size)
            .ok_or_else(|| "PSL target block overflows its coordinate range".to_owned())?;
        if positive_target_start < target_start || positive_target_end > target_end {
            return Err(
                "PSL target block must be contained by the declared target interval; translated protein PSL is not supported"
                    .to_owned(),
            );
        }
        blocks.push((positive_target_start, positive_target_end));
    }
    blocks.sort_unstable();
    if blocks
        .windows(2)
        .any(|adjacent| adjacent[1].0 < adjacent[0].1)
    {
        return Err("PSL target blocks must not overlap".to_owned());
    }
    let total_compared = matches
        .saturating_add(mismatches)
        .saturating_add(repeat_matches)
        .saturating_add(n_count);
    let score = (total_compared > 0).then(|| {
        (matches.saturating_add(repeat_matches) as f64 * 1000.0 / total_compared as f64) as f32
    });
    let strand = if query_reverse ^ target_reverse {
        Strand::Reverse
    } else {
        Strand::Forward
    };
    let attributes = [
        ("source_format", "PSL".to_owned()),
        ("matches", matches.to_string()),
        ("mismatches", mismatches.to_string()),
        ("repeat_matches", repeat_matches.to_string()),
        ("n_count", n_count.to_string()),
        ("query_insertions", query_insertions.to_string()),
        ("query_inserted_bases", query_inserted_bases.to_string()),
        ("target_insertions", target_insertions.to_string()),
        ("target_inserted_bases", target_inserted_bases.to_string()),
        ("query_name", query_name.to_owned()),
        ("query_size", query_size.to_string()),
        ("query_start", query_start.to_string()),
        ("query_end", query_end.to_string()),
        ("target_size", target_size.to_string()),
        ("raw_strand", raw_strand.to_owned()),
        ("block_sizes", fields[offset + 18].to_owned()),
        ("query_starts", fields[offset + 19].to_owned()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value))
    .collect();
    Ok(FeatureRecord {
        contig: target_name.to_owned(),
        start: target_start,
        end: target_end,
        name: Some(query_name.to_owned()),
        feature_type: Some("PSL alignment".to_owned()),
        score,
        strand,
        blocks,
        coding_interval: None,
        peak: None,
        attributes,
    })
}

fn parse_narrow_peak_fields(
    contig: &str,
    start: u64,
    end: u64,
    fields: &[&str],
    source_format: &str,
) -> Result<FeatureRecord, String> {
    if fields.len() != 7 {
        return Err(format!(
            "{source_format} requires exactly seven fields after its coordinates"
        ));
    }
    let score = fields[1]
        .parse::<f32>()
        .map_err(|_| format!("invalid {source_format} score"))?;
    if !score.is_finite() || !(0.0..=1000.0).contains(&score) {
        return Err(format!(
            "{source_format} score must be finite and between 0 and 1000"
        ));
    }
    let signal_value = parse_finite_f32(fields[3], &format!("{source_format} signal value"))?;
    let p_value = parse_optional_peak_value(fields[4], &format!("{source_format} p-value"))?;
    let q_value = parse_optional_peak_value(fields[5], &format!("{source_format} q-value"))?;
    let peak_offset = fields[6]
        .parse::<i64>()
        .map_err(|_| format!("invalid {source_format} summit offset"))?;
    let summit = if peak_offset == -1 {
        None
    } else {
        let peak_offset = u64::try_from(peak_offset)
            .map_err(|_| format!("{source_format} summit offset must be -1 or non-negative"))?;
        if peak_offset >= end - start {
            return Err(format!(
                "{source_format} summit offset must fall inside the peak interval"
            ));
        }
        Some(start + peak_offset)
    };
    Ok(FeatureRecord {
        contig: contig.to_owned(),
        start,
        end,
        name: (fields[0] != ".").then(|| fields[0].to_owned()),
        feature_type: Some(source_format.to_owned()),
        score: Some(score),
        strand: parse_strand(fields[2]),
        blocks: Vec::new(),
        coding_interval: None,
        peak: Some(PeakMetrics {
            signal_value,
            p_value,
            q_value,
            summit,
        }),
        attributes: if source_format == "narrowPeak" {
            Vec::new()
        } else {
            vec![("source_format".to_owned(), source_format.to_owned())]
        },
    })
}

fn parse_finite_f32(raw: &str, label: &str) -> Result<f32, String> {
    let value = raw
        .parse::<f32>()
        .map_err(|_| format!("invalid {label}: {raw}"))?;
    if !value.is_finite() {
        return Err(format!("{label} must be finite"));
    }
    Ok(value)
}

fn parse_optional_peak_value(raw: &str, label: &str) -> Result<Option<f32>, String> {
    let value = parse_finite_f32(raw, label)?;
    if value.to_bits() == (-1.0_f32).to_bits() {
        Ok(None)
    } else if value >= 0.0 {
        Ok(Some(value))
    } else {
        Err(format!("{label} must be -1 or non-negative"))
    }
}

fn parse_attributes(raw: &str, gtf: bool) -> Vec<(String, String)> {
    raw.split(';')
        .filter_map(|field| {
            let field = field.trim();
            if field.is_empty() {
                return None;
            }
            let pair = if gtf {
                field.split_once(char::is_whitespace)
            } else {
                field.split_once('=')
            }?;
            Some((
                pair.0.trim().to_owned(),
                pair.1.trim().trim_matches('"').to_owned(),
            ))
        })
        .collect()
}

fn attribute_value(attributes: &[(String, String)], keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        attributes
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    })
}

fn comma_numbers(raw: &str, label: &str) -> Result<Vec<u64>, String> {
    raw.trim_end_matches(',')
        .split(',')
        .map(|value| parse_u64(value, label))
        .collect()
}

fn comma_numbers_exact(raw: &str, label: &str, expected: usize) -> Result<Vec<u64>, String> {
    let mut values = Vec::with_capacity(expected);
    for raw_value in raw.trim_end_matches(',').split(',') {
        if values.len() == expected {
            return Err(format!("{label} count does not match the declared count"));
        }
        values.push(parse_u64(raw_value, label)?);
    }
    if values.len() != expected {
        return Err(format!("{label} count does not match the declared count"));
    }
    Ok(values)
}

fn parse_u64(raw: &str, label: &str) -> Result<u64, String> {
    raw.parse().map_err(|_| format!("invalid {label}: {raw}"))
}

fn parse_strand(raw: &str) -> Strand {
    match raw {
        "+" => Strand::Forward,
        "-" => Strand::Reverse,
        _ => Strand::Unstranded,
    }
}

#[derive(Debug, Error)]
pub enum TrackError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("malformed track line {line}: {message}")]
    MalformedLine { line: usize, message: String },
    #[error("malformed indexed track record at {contig}:{start}: {message}")]
    MalformedRecord {
        contig: String,
        start: u64,
        message: String,
    },
    #[error("invalid track schema: {0}")]
    InvalidSchema(String),
    #[error("track coordinate cannot be represented by the backend")]
    Coordinate,
    #[error("track backend error: {0}")]
    Backend(String),
    #[error("track resource limit: {0}")]
    ResourceLimit(String),
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        io::Cursor,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    use bigtools::{BigBedWrite, beddata::BedParserStreamingIterator};
    use rust_htslib::bcf::{Format, Header, Writer, index};
    use tiny_http::{Header as HttpHeader, Method, Response, Server, StatusCode};

    use super::*;

    const BIG_GENE_PRED_AUTOSQL: &str = r#"table bigGenePred
"bigGenePred gene models"
(
string chrom;
uint chromStart;
uint chromEnd;
string name;
uint score;
char[1] strand;
uint thickStart;
uint thickEnd;
uint reserved;
int blockCount;
int[blockCount] blockSizes;
int[blockCount] chromStarts;
string name2;
string cdsStartStat;
string cdsEndStat;
int[blockCount] exonFrames;
string type;
string geneName;
string geneName2;
string geneType;
)
"#;

    const BIG_NARROW_PEAK_AUTOSQL: &str = r#"table bigNarrowPeak
"bigNarrowPeak peaks"
(
string chrom;
uint chromStart;
uint chromEnd;
string name;
uint score;
char[1] strand;
float signalValue;
float pValue;
float qValue;
int peak;
)
"#;

    struct RemoteVariantFixture {
        url: String,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl RemoteVariantFixture {
        fn start() -> Self {
            let server = Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let objects = HashMap::from([
                (
                    "/variants.bcf".to_owned(),
                    include_bytes!("../../../benchmark-data/demo-tracks/variants.bcf").to_vec(),
                ),
                (
                    "/variants.bcf.csi".to_owned(),
                    include_bytes!("../../../benchmark-data/demo-tracks/variants.bcf.csi").to_vec(),
                ),
            ]);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                        continue;
                    };
                    let path = request.url().split('?').next().unwrap_or_default();
                    let Some(data) = objects.get(path) else {
                        request.respond(Response::empty(StatusCode(404))).unwrap();
                        continue;
                    };
                    let response = if request.method() == &Method::Head {
                        Response::from_data(data.clone())
                    } else if let Some((start, end)) = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| parse_http_range(header.value.as_str(), data.len()))
                    {
                        Response::from_data(data[start..=end].to_vec())
                            .with_status_code(StatusCode(206))
                            .with_header(http_header(
                                "Content-Range",
                                &format!("bytes {start}-{end}/{}", data.len()),
                            ))
                    } else {
                        Response::from_data(data.clone())
                    }
                    .with_header(http_header("Accept-Ranges", "bytes"));
                    request.respond(response).unwrap();
                }
            });
            Self {
                url: format!("http://{address}/variants.bcf"),
                stop,
                worker: Some(worker),
            }
        }
    }

    impl Drop for RemoteVariantFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn http_header(name: &str, value: &str) -> HttpHeader {
        HttpHeader::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    fn parse_http_range(value: &str, length: usize) -> Option<(usize, usize)> {
        let value = value.strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        let start = start.parse::<usize>().ok()?;
        let end = if end.is_empty() {
            length.checked_sub(1)?
        } else {
            end.parse::<usize>().ok()?.min(length.checked_sub(1)?)
        };
        (start <= end).then_some((start, end))
    }

    fn write_test_bigbed(path: &Path, autosql: &str, contents: &str) {
        let mut chromosome_sizes = HashMap::new();
        chromosome_sizes.insert("chr1".to_owned(), 1_000_000);
        chromosome_sizes.insert("chr2".to_owned(), 1_000_000);
        let mut writer = BigBedWrite::create_file(path, chromosome_sizes).unwrap();
        writer.autosql = Some(autosql.to_owned());
        writer.options.compress = false;
        let data = BedParserStreamingIterator::from_bed_file(Cursor::new(contents), false);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();
        writer.write(data, runtime).unwrap();
    }

    #[test]
    fn bedgraph_queries_are_zero_based_half_open_and_overlap_aware() {
        let source = TextQuantitativeSource::from_reader(
            std::io::Cursor::new(
                "track type=bedGraph\nchr1\t10\t20\t2.5\nchr1\t15\t40\t-1\nchr2\t0\t5\tNaN\n",
            ),
            QuantitativeFormat::BedGraph,
        )
        .unwrap();
        let query = source.query("chr1", 19, 21);
        assert_eq!(
            query.records,
            vec![
                QuantitativeRecord {
                    start: 10,
                    end: 20,
                    value: 2.5,
                },
                QuantitativeRecord {
                    start: 15,
                    end: 40,
                    value: -1.0,
                },
            ]
        );
        assert_eq!(query.provenance.records_examined, 2);
        assert!(!query.provenance.records_truncated);
        let non_finite = source.query("chr2", 0, 10);
        assert_eq!(non_finite.provenance.non_finite_records, 1);
    }

    #[test]
    fn wig_fixed_and_variable_steps_convert_one_based_coordinates_exactly() {
        let source = TextQuantitativeSource::from_reader(
            std::io::Cursor::new(
                "fixedStep chrom=chr1 start=11 step=5 span=3\n1.5\n-2\nvariableStep chrom=chr1 span=2\n21 4\n31 8\n",
            ),
            QuantitativeFormat::Wig,
        )
        .unwrap();
        assert_eq!(
            source.query("chr1", 0, 100).records,
            vec![
                QuantitativeRecord {
                    start: 10,
                    end: 13,
                    value: 1.5,
                },
                QuantitativeRecord {
                    start: 15,
                    end: 18,
                    value: -2.0,
                },
                QuantitativeRecord {
                    start: 20,
                    end: 22,
                    value: 4.0,
                },
                QuantitativeRecord {
                    start: 30,
                    end: 32,
                    value: 8.0,
                },
            ]
        );
    }

    #[test]
    fn wig_rejects_data_without_a_declaration_and_zero_coordinates() {
        assert!(
            TextQuantitativeSource::from_reader(
                std::io::Cursor::new("1.0\n"),
                QuantitativeFormat::Wig,
            )
            .is_err()
        );
        assert!(
            TextQuantitativeSource::from_reader(
                std::io::Cursor::new("variableStep chrom=chr1\n0 1.0\n"),
                QuantitativeFormat::Wig,
            )
            .is_err()
        );
    }

    #[test]
    fn seg_preserves_samples_markers_and_one_based_inclusive_coordinates() {
        let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
            "ID\tchrom\tloc.start\tloc.end\tnum.mark\tseg.mean\n\
             tumour\tchr1\t11\t20\t7\t-0.5\n\
             normal\tchr1\t21\t30\t9\t2.0\n\
             ignored\tchr1\t31\t40\t3\tnot-a-number\n",
        ))
        .unwrap();
        let query = source.query("chr1", 0, 100, &CopyNumberTrackSettings::default());
        assert_eq!(query.samples, vec!["tumour", "normal"]);
        assert_eq!(query.segments.len(), 2);
        assert_eq!((query.segments[0].start, query.segments[0].end), (10, 20));
        assert_eq!(query.segments[0].marker_count, Some(7));
        assert_eq!(
            query.inferred_value_semantics,
            CopyNumberValueSemantics::LogRatio
        );
        assert_eq!(query.provenance.source_rows_ignored, 1);
    }

    #[test]
    fn seg_track_coords_zero_is_honoured() {
        let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
            "#track type=seg coords=0\nSample\tChromosome\tStart\tEnd\tValue\ns1\tchr1\t0\t10\t2\n",
        ))
        .unwrap();
        let query = source.query("chr1", 0, 1, &CopyNumberTrackSettings::default());
        assert_eq!((query.segments[0].start, query.segments[0].end), (0, 10));
        assert_eq!(
            query.inferred_value_semantics,
            CopyNumberValueSemantics::Absolute
        );
    }

    #[test]
    fn seg_sort_values_are_base_weighted_at_the_persisted_locus() {
        let source = TextCopyNumberSource::from_reader(std::io::Cursor::new(
            "sample\tchrom\tstart\tend\tmarkers\tvalue\n\
             a\tchr1\t1\t10\t2\t1\n\
             a\tchr1\t11\t30\t4\t3\n\
             b\tchr1\t1\t30\t6\t2\n",
        ))
        .unwrap();
        let query = source.query(
            "chr1",
            0,
            5,
            &CopyNumberTrackSettings {
                sort_mode: CopyNumberSortMode::Amplification,
                sort_locus: Some(rgv_core::CopyNumberSortLocus {
                    contig: "chr1".to_owned(),
                    start: 0,
                    end: 30,
                }),
                ..CopyNumberTrackSettings::default()
            },
        );
        assert_eq!(query.segments.len(), 2);
        assert!((query.sample_sort_values[0].unwrap() - (70.0 / 30.0)).abs() < 1.0e-6);
        assert_eq!(query.sample_sort_values[1], Some(2.0));
        assert_eq!(query.provenance.sort_records_examined, 3);
    }

    #[test]
    fn mutation_maf_preserves_samples_coordinates_and_ordered_annotations() {
        let source = TextMutationSource::from_reader(std::io::Cursor::new(
            "#version 2.4\n\
             Hugo_Symbol\tEntrez_Gene_Id\tCenter\tNCBI_Build\tChromosome\tStart_Position\tEnd_Position\tStrand\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tdbSNP_RS\tValidation_Status\tTumor_Sample_Barcode\tHGVSp_Short\n\
             TP53\t7157\tGDC\tGRCh38\tchr17\t101\t101\t+\tMissense_Mutation\tSNP\tC\tC\tT\tnovel\tUnknown\tTUMOUR-A\tp.R1W\n\
             EGFR\t1956\tGDC\tGRCh38\tchr17\t201\t202\t+\tFrame_Shift_Del\tDEL\tAC\t-\t-\tnovel\tUnknown\tTUMOUR-B\tp.E2fs\n\
             ALK\t238\tGDC\tGRCh38\tchr17\t301\t300\t+\tFrame_Shift_Ins\tINS\t-\t-\tA\tnovel\tUnknown\tTUMOUR-A\tp.A3fs\n\
             BAD\t0\tGDC\tGRCh38\tchr17\tnot-a-position\t300\t+\tSilent\tSNP\tA\tA\tG\tnovel\tUnknown\tTUMOUR-A\tp.=\n",
        ))
        .unwrap();

        let query = source.query("chr17", 100, 202);

        assert_eq!(query.samples, vec!["TUMOUR-A", "TUMOUR-B"]);
        assert_eq!(query.records.len(), 2);
        assert_eq!((query.records[0].start, query.records[0].end), (100, 101));
        assert_eq!(query.records[0].gene, "TP53");
        assert_eq!(query.records[0].classification, "Missense_Mutation");
        assert_eq!(query.records[0].variant_type, "SNP");
        assert_eq!(query.records[0].tumor_allele_two, "T");
        assert_eq!(
            query.headers[usize::from(query.records[0].annotations[0].column_index)],
            "Hugo_Symbol"
        );
        assert_eq!(query.provenance.source_rows_ignored, 1);
        assert_eq!(source.query("chr17", 101, 200).records.len(), 0);
        let insertion = source.query("chr17", 300, 301);
        assert_eq!(insertion.records.len(), 1);
        assert_eq!(
            (insertion.records[0].start, insertion.records[0].end),
            (300, 301)
        );
        assert_eq!(
            (
                insertion.records[0].source_start,
                insertion.records[0].source_end,
                insertion.records[0].insertion_boundary,
            ),
            (301, 300, true)
        );
    }

    #[test]
    fn mutation_maf_accepts_uuid_sample_identity_and_requires_named_columns() {
        let header = "Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_UUID\n";
        let source = TextMutationSource::from_reader(std::io::Cursor::new(format!(
            "{header}KRAS\t12\t25398285\t25398285\tMissense_Mutation\tSNP\tC\tC\tA\tcase-uuid\n"
        )))
        .unwrap();
        assert_eq!(
            source.query("12", 25_398_284, 25_398_285).samples,
            ["case-uuid"]
        );
        assert!(
            TextMutationSource::from_reader(std::io::Cursor::new(
                "Chromosome\tStart_Position\tEnd_Position\nchr1\t1\t1\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn mutation_maf_rejects_an_unbounded_source_line() {
        let oversized = format!("{}\n", "x".repeat(MAX_MUTATION_LINE_BYTES));
        assert!(matches!(
            TextMutationSource::from_reader(std::io::Cursor::new(oversized)),
            Err(TrackError::ResourceLimit(_))
        ));
    }

    #[test]
    fn mutation_maf_opens_gzip_sources() {
        use std::io::Write as _;

        let file = tempfile::Builder::new()
            .suffix(".maf.gz")
            .tempfile()
            .unwrap();
        let mut encoder =
            flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
        encoder
            .write_all(
                b"Hugo_Symbol\tChromosome\tStart_Position\tEnd_Position\tVariant_Classification\tVariant_Type\tReference_Allele\tTumor_Seq_Allele1\tTumor_Seq_Allele2\tTumor_Sample_Barcode\nTP53\tchr17\t101\t101\tMissense_Mutation\tSNP\tC\tC\tT\tTUMOUR-A\n",
            )
            .unwrap();
        encoder.finish().unwrap();

        let source = TextMutationSource::open(file.path()).unwrap();

        assert_eq!(source.query("chr17", 100, 101).records.len(), 1);
    }

    #[test]
    fn bedpe_query_indexes_both_endpoints_and_deduplicates_intrachromosomal_spans() {
        let source = TextInteractionSource::from_reader(
            std::io::Cursor::new(
                "chr1\t10\t20\tchr1\t90\t100\tintra\t25\t+\t-\nchr1\t30\t40\tchr2\t50\t60\tcross\t5\n",
            ),
            InteractionFormat::BedPe,
        )
        .unwrap();
        let query = source.query(
            "chr1",
            15,
            95,
            &InteractionTrackSettings {
                minimum_score: Some(10.0),
                ..InteractionTrackSettings::default()
            },
        );
        assert_eq!(query.records.len(), 1);
        assert_eq!(query.records[0].name.as_deref(), Some("intra"));
        assert_eq!(query.records[0].source.strand, Strand::Forward);
        assert_eq!(query.records[0].target.strand, Strand::Reverse);
        assert!(!query.records[0].directional);
        assert!(query.provenance.duplicate_entries_ignored >= 1);
        assert_eq!(query.provenance.records_filtered, 1);

        let partner = source.query("chr2", 55, 56, &InteractionTrackSettings::default());
        assert_eq!(partner.records[0].name.as_deref(), Some("cross"));
    }

    #[test]
    fn interact_parser_preserves_bed5_plus_13_semantics() {
        let source = TextInteractionSource::from_reader(
            std::io::Cursor::new(
                "chr1\t10\t100\tsnp/gene/liver\t700\t0.625\tliver\t#7A67EE\tchr1\t10\t11\trs1\t.\tchr1\t99\t100\tGENE1\t+\n",
            ),
            InteractionFormat::Interact,
        )
        .unwrap();
        let query = source.query("chr1", 50, 51, &InteractionTrackSettings::default());
        let record = &query.records[0];
        assert_eq!(record.source.start, 10);
        assert_eq!(record.target.end, 100);
        assert_eq!(record.source.name.as_deref(), Some("rs1"));
        assert_eq!(record.target.name.as_deref(), Some("GENE1"));
        assert_eq!(record.value, Some(0.625));
        assert_eq!(record.experiment.as_deref(), Some("liver"));
        assert_eq!(record.color, Some([0x7a, 0x67, 0xee, 230]));
        assert!(record.directional);
    }

    #[test]
    fn bed12_blocks_use_zero_based_offsets() {
        let record = parse_bed("chr1\t100\t200\ttx\t42\t+\t100\t200\t0\t2\t20,30\t0,70").unwrap();
        assert_eq!(record.blocks, vec![(100, 120), (170, 200)]);
        assert_eq!(record.coding_interval, Some((100, 200)));
        assert_eq!(record.strand, Strand::Forward);
    }

    #[test]
    fn gene_pred_preserves_transcript_cds_and_exons() {
        let record = parse_gene_pred(
            "tx1\tchr2\t-\t100\t400\t150\t350\t3\t100,200,320,\t180,260,400,",
            false,
        )
        .unwrap();
        assert_eq!((record.start, record.end), (100, 400));
        assert_eq!(record.name.as_deref(), Some("tx1"));
        assert_eq!(record.strand, Strand::Reverse);
        assert_eq!(record.blocks, [(100, 180), (200, 260), (320, 400)]);
        assert_eq!(record.coding_interval, Some((150, 350)));
        assert_eq!(record.feature_type.as_deref(), Some("transcript"));
        assert_eq!(
            attribute_value(&record.attributes, &["transcript_id"]).as_deref(),
            Some("tx1")
        );
    }

    #[test]
    fn extended_gene_pred_and_ref_flat_keep_gene_and_transcript_names() {
        let extended = parse_gene_pred(
            "585\ttx2\tchr3\t+\t10\t80\t20\t70\t2\t10,50,\t30,80,\t900\tGENE2\tcmpl\tincmpl\t0,2,",
            false,
        )
        .unwrap();
        assert_eq!(extended.name.as_deref(), Some("tx2"));
        assert_eq!(extended.score, Some(900.0));
        assert_eq!(
            attribute_value(&extended.attributes, &["gene_name"]).as_deref(),
            Some("GENE2")
        );

        let ref_flat =
            parse_gene_pred("GENE3\ttx3\tchr4\t+\t0\t100\t0\t0\t1\t0,\t100,", true).unwrap();
        assert_eq!(ref_flat.name.as_deref(), Some("GENE3"));
        assert_eq!(ref_flat.coding_interval, None);
        assert_eq!(
            attribute_value(&ref_flat.attributes, &["transcript_id"]).as_deref(),
            Some("tx3")
        );
    }

    #[test]
    fn gene_pred_rejects_mismatched_or_overlapping_exons() {
        assert!(parse_gene_pred("tx\tchr1\t+\t0\t100\t10\t90\t2\t0,50,\t25,", false,).is_err());
        assert!(parse_gene_pred("tx\tchr1\t+\t0\t100\t10\t90\t2\t0,20,\t30,40,", false,).is_err());
    }

    #[test]
    fn big_gene_pred_preserves_binary_schema_transcript_semantics() {
        let record = parse_bigbed_record(
            "chr1",
            &bigtools::BedEntry {
                start: 100,
                end: 220,
                rest: "tx1\t900\t+\t120\t200\t0\t3\t20,30,20,\t0,60,100,\ttx1-alt\tcmpl\tincmpl\t0,1,2,\tcoding\tGENE1\tGENE1-alt\tprotein_coding".to_owned(),
            },
            BigBedFeatureFormat::BigGenePred,
        )
        .unwrap();

        assert_eq!(record.name.as_deref(), Some("GENE1"));
        assert_eq!(record.blocks, [(100, 120), (160, 190), (200, 220)]);
        assert_eq!(record.coding_interval, Some((120, 200)));
        assert_eq!(record.strand, Strand::Forward);
        assert_eq!(record.score, Some(900.0));
        assert_eq!(
            attribute_value(&record.attributes, &["transcript_id"]).as_deref(),
            Some("tx1")
        );
        assert_eq!(
            attribute_value(&record.attributes, &["gene_type"]).as_deref(),
            Some("protein_coding")
        );
        assert!(feature_search_terms(&record).contains(&"tx1-alt".to_owned()));
    }

    #[test]
    fn big_gene_pred_rejects_malformed_blocks_frames_status_and_bounds() {
        let parse = |rest: &str| {
            parse_big_gene_pred_record("chr1", 100, 220, rest).map(|record| record.blocks)
        };
        assert!(parse("tx\t0\t+\t100\t200\t0\t2\t50,50,\t0,25,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
        assert!(parse("tx\t0\t+\t100\t200\t0\t2\t20,20,\t0,50,\ttx\tbad\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
        assert!(parse("tx\t0\t+\t100\t200\t0\t2\t20,20,\t0,50,\ttx\tcmpl\tcmpl\t0,\tcoding\tGENE\t.\tprotein_coding").is_err());
        assert!(parse("tx\t0\t+\t90\t200\t0\t2\t20,20,\t0,50,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding").is_err());
    }

    #[test]
    fn narrow_peak_preserves_statistics_and_absolute_summit() {
        let record = parse_narrow_peak("chr1\t100\t200\tp1\t750\t.\t18.25\t4.5\t-1\t37").unwrap();
        assert_eq!((record.start, record.end), (100, 200));
        assert_eq!(record.score, Some(750.0));
        assert_eq!(
            record.peak,
            Some(PeakMetrics {
                signal_value: 18.25,
                p_value: Some(4.5),
                q_value: None,
                summit: Some(137),
            })
        );
        assert_eq!(record.feature_type.as_deref(), Some("narrowPeak"));
    }

    #[test]
    fn narrow_peak_rejects_out_of_interval_summits_and_invalid_missing_values() {
        assert!(parse_narrow_peak("chr1\t100\t200\tp1\t0\t.\t1\t-1\t-1\t100").is_err());
        assert!(parse_narrow_peak("chr1\t100\t200\tp1\t0\t.\t1\t-2\t-1\t-1").is_err());
    }

    #[test]
    fn broad_peak_preserves_statistics_without_inventing_a_summit() {
        let record =
            parse_broad_peak("chr1\t100\t250\tregion-a\t116\t.\t4.89716\t3.70716\t-1").unwrap();
        assert_eq!((record.start, record.end), (100, 250));
        assert_eq!(record.feature_type.as_deref(), Some("broadPeak"));
        assert_eq!(record.score, Some(116.0));
        assert_eq!(record.peak.unwrap().summit, None);
        assert_eq!(record.peak.unwrap().q_value, None);
        assert!(parse_broad_peak("chr1\t100\t250\tbad\t1001\t.\t1\t-1\t-1").is_err());
        assert!(parse_broad_peak("chr1\t100\t250\tbad\t1\tx\t1\t-1\t-1").is_err());
    }

    #[test]
    fn psl_preserves_target_blocks_identity_counts_and_query_name() {
        let record = parse_psl(
            "70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,",
        )
        .unwrap();
        assert_eq!((record.start, record.end), (100, 190));
        assert_eq!(record.contig, "chr1");
        assert_eq!(record.name.as_deref(), Some("query-1"));
        assert_eq!(record.feature_type.as_deref(), Some("PSL alignment"));
        assert_eq!(record.blocks, [(100, 130), (140, 190)]);
        assert_eq!(record.strand, Strand::Forward);
        assert_eq!(record.score, Some(937.5));
        assert_eq!(
            attribute_value(&record.attributes, &["mismatches"]).as_deref(),
            Some("5")
        );
    }

    #[test]
    fn psl_maps_reverse_query_and_target_block_coordinates() {
        let query_reverse = parse_psl(
            "38\t0\t0\t0\t1\t14\t1\t32\t-\tquery-minus\t61\t4\t56\tchr1\t1000\t110\t180\t2\t20,18,\t5,39,\t110,162,",
        )
        .unwrap();
        assert_eq!(query_reverse.strand, Strand::Reverse);
        assert_eq!(query_reverse.blocks, [(110, 130), (162, 180)]);

        let target_reverse = parse_psl(
            "59\t9\t0\t0\t1\t823\t1\t96\t+-\tFS_CONTIG_48080_1\t1955\t171\t1062\tchr22\t47748585\t13073589\t13073753\t2\t48,20,\t171,1042,\t34674832,34674976,",
        )
        .unwrap();
        assert_eq!(target_reverse.strand, Strand::Reverse);
        assert_eq!(
            target_reverse.blocks,
            [(13_073_589, 13_073_609), (13_073_705, 13_073_753)]
        );
    }

    #[test]
    fn psl_source_skips_standard_header_and_rejects_malformed_block_arrays() {
        let source = TextFeatureSource::from_reader(
            std::io::Cursor::new(
                "psLayout version 3\n\nmatch mis- rep. N's Q gap count Q gap bases T gap count T gap bases strand Q name Q size Q start Q end T name T size T start T end block count blockSizes qStarts tStarts\n---------------------------------------------------------------------------------------------------------------------------------------------------------------\n70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,\n",
            ),
            FeatureFormat::Psl,
        )
        .unwrap();
        assert_eq!(source.query("chr1", 120, 121).unwrap().len(), 1);
        assert_eq!(source.search("query-1", 10).hits.len(), 1);
        assert!(
            parse_psl("70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t80\tchr1\t1000\t100\t190\t2\t30,\t0,40,\t100,150,")
                .is_err()
        );
        assert!(
            parse_psl("110\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t200\t0\t123\tchr1\t1000\t100\t230\t2\t60,60,\t0,63,\t100,120,")
                .is_err()
        );
        assert!(
            parse_psl("30\t0\t0\t0\t0\t0\t0\t0\t+\tprotein\t30\t0\t30\tchr1\t1000\t100\t190\t1\t30,\t0,\t100,")
                .unwrap_err()
                .contains("translated protein PSL")
        );
    }

    #[test]
    fn big_narrow_peak_preserves_binary_statistics_and_source() {
        let record = parse_bigbed_record(
            "chr2",
            &bigtools::BedEntry {
                start: 500,
                end: 600,
                rest: "peak-a\t750\t.\t18.25\t4.5\t-1\t37".to_owned(),
            },
            BigBedFeatureFormat::BigNarrowPeak,
        )
        .unwrap();
        assert_eq!(record.feature_type.as_deref(), Some("bigNarrowPeak"));
        assert_eq!(record.peak.unwrap().summit, Some(537));
        assert_eq!(
            attribute_value(&record.attributes, &["source_format"]).as_deref(),
            Some("bigNarrowPeak")
        );
    }

    #[test]
    fn generated_bigbed_truth_queries_typed_models_and_supports_bounded_search() {
        let directory = tempfile::tempdir().unwrap();
        let gene_path = directory.path().join("genes.bb");
        write_test_bigbed(
            &gene_path,
            BIG_GENE_PRED_AUTOSQL,
            "chr1\t100\t220\ttx1\t900\t+\t120\t200\t0\t3\t20,30,20,\t0,60,100,\ttx1-alt\tcmpl\tincmpl\t0,1,2,\tcoding\tGENE1\tGENE1-alt\tprotein_coding\n\
             chr2\t300\t380\ttx2\t500\t-\t300\t300\t0\t2\t20,20,\t0,60,\ttx2-alt\tnone\tnone\t-1,-1,\tnoncoding\tGENE2\t.\tlncRNA\n",
        );
        let peak_path = directory.path().join("peaks.bb");
        write_test_bigbed(
            &peak_path,
            BIG_NARROW_PEAK_AUTOSQL,
            "chr1\t150\t250\tpeak-a\t750\t.\t18.25\t4.5\t-1\t37\n\
             chr2\t500\t600\tpeak-b\t200\t-\t2.5\t-1\t3.25\t-1\n",
        );

        let mut genes =
            BigBedSource::open_with_format(&gene_path, BigBedFeatureFormat::BigGenePred).unwrap();
        let queried = genes.query("chr1", 110, 210).unwrap();
        assert_eq!(queried.len(), 1);
        assert_eq!(queried[0].name.as_deref(), Some("GENE1"));
        assert_eq!(queried[0].blocks, [(100, 120), (160, 190), (200, 220)]);
        let search = genes.search("tx1-alt", 10).unwrap();
        assert_eq!(search.hits.len(), 1);
        assert_eq!(search.hits[0].record.name.as_deref(), Some("GENE1"));
        assert!(!search.scan_truncated);
        assert_eq!(
            genes
                .adjacent_feature("chr1", 0, true)
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("GENE1")
        );
        assert_eq!(
            genes
                .adjacent_feature("chr1", 221, false)
                .unwrap()
                .unwrap()
                .name
                .as_deref(),
            Some("GENE1")
        );

        let mut peaks =
            BigBedSource::open_with_format(&peak_path, BigBedFeatureFormat::BigNarrowPeak).unwrap();
        let queried = peaks.query("chr1", 0, 1_000).unwrap();
        assert_eq!(queried[0].peak.unwrap().summit, Some(187));
        assert_eq!(peaks.search("peak-b", 10).unwrap().hits.len(), 1);

        assert!(matches!(
            BigBedSource::open_with_format(&gene_path, BigBedFeatureFormat::BigNarrowPeak),
            Err(TrackError::InvalidSchema(_))
        ));
        assert!(matches!(
            ensure_bigbed_query_capacity(MAX_FEATURE_RECORDS_PER_QUERY),
            Err(TrackError::ResourceLimit(_))
        ));
    }

    #[test]
    fn malformed_binary_gene_record_fails_with_its_indexed_location() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("malformed.bb");
        write_test_bigbed(
            &path,
            BIG_GENE_PRED_AUTOSQL,
            "chr1\t100\t220\ttx\t0\t+\t100\t200\t0\t2\t50,50,\t0,25,\ttx\tcmpl\tcmpl\t0,0,\tcoding\tGENE\t.\tprotein_coding\n",
        );
        let mut source =
            BigBedSource::open_with_format(&path, BigBedFeatureFormat::BigGenePred).unwrap();
        assert!(matches!(
            source.query("chr1", 0, 1_000),
            Err(TrackError::MalformedRecord { ref contig, start: 100, .. }) if contig == "chr1"
        ));
    }

    #[test]
    fn text_feature_sources_reject_unbounded_lines() {
        let oversized = format!("{}\n", "x".repeat(MAX_FEATURE_LINE_BYTES));
        assert!(matches!(
            TextFeatureSource::from_reader(
                std::io::Cursor::new(oversized),
                FeatureFormat::GenePred
            ),
            Err(TrackError::ResourceLimit(_))
        ));
    }

    #[test]
    fn feature_search_indexes_names_aliases_and_transcripts_deterministically() {
        let source = TextFeatureSource::from_reader(
            std::io::Cursor::new(
                "chr2\tsrc\tgene\t101\t200\t.\t+\t.\tID=g2;Name=BRCA2;Alias=FANCD1\n\
                 chr1\tsrc\ttranscript\t11\t50\t.\t+\t.\tID=tx1;gene_name=BRCA1;transcript_id=BRCA1-201\n",
            ),
            FeatureFormat::Gff3,
        )
        .unwrap();

        let prefix = source.search("brca", 10);
        assert_eq!(prefix.hits.len(), 2);
        assert_eq!(prefix.hits[0].record.contig, "chr1");
        assert_eq!(prefix.hits[1].record.contig, "chr2");
        assert!(!prefix.index_truncated);
        assert!(!prefix.scan_truncated);

        let alias = source.search("fancd1", 10);
        assert_eq!(alias.hits[0].record.name.as_deref(), Some("BRCA2"));
        let transcript = source.search("brca1-201", 10);
        assert_eq!(transcript.hits[0].record.start, 10);
        assert_eq!(
            source
                .adjacent_feature("chr1", 0, true)
                .and_then(|record| record.name),
            Some("BRCA1".to_owned())
        );
        assert_eq!(
            source
                .adjacent_feature("chr2", 500, false)
                .and_then(|record| record.name),
            Some("BRCA2".to_owned())
        );
    }

    #[test]
    fn compressed_gene_pred_sources_open_and_query() {
        use std::io::Write as _;

        let file = tempfile::Builder::new()
            .suffix(".genePred.gz")
            .tempfile()
            .unwrap();
        let mut encoder =
            flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
        encoder
            .write_all(b"tx1\tchr1\t+\t0\t100\t10\t90\t1\t0,\t100,\n")
            .unwrap();
        encoder.finish().unwrap();

        let source = TextFeatureSource::open(file.path(), FeatureFormat::GenePred).unwrap();

        assert_eq!(source.query("chr1", 0, 1).unwrap().len(), 1);
    }

    #[test]
    fn compressed_psl_sources_open_query_and_search() {
        use std::io::Write as _;

        let file = tempfile::Builder::new()
            .suffix(".psl.gz")
            .tempfile()
            .unwrap();
        let mut encoder =
            flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
        encoder
            .write_all(b"70\t5\t5\t0\t1\t3\t1\t10\t+\tquery-1\t100\t0\t83\tchr1\t1000\t100\t190\t2\t30,50,\t0,33,\t100,140,\n")
            .unwrap();
        encoder.finish().unwrap();

        let source = TextFeatureSource::open(file.path(), FeatureFormat::Psl).unwrap();

        assert_eq!(source.query("chr1", 120, 121).unwrap().len(), 1);
        assert_eq!(source.search("query-1", 10).hits.len(), 1);
    }

    #[test]
    fn gff_coordinates_become_zero_based_half_open() {
        let record = parse_gff(
            "chr1\tsrc\tgene\t101\t200\t.\t-\t.\tID=g1;Name=GENE1",
            false,
        )
        .unwrap();
        assert_eq!((record.start, record.end), (100, 200));
        assert_eq!(record.name.as_deref(), Some("GENE1"));
        assert_eq!(record.strand, Strand::Reverse);
    }

    #[test]
    fn gtf_names_are_decoded() {
        let record = parse_gff(
            "chr1\tsrc\texon\t1\t20\t1.5\t+\t.\tgene_id \"g1\"; gene_name \"ABC\";",
            true,
        )
        .unwrap();
        assert_eq!(record.name.as_deref(), Some("ABC"));
        assert_eq!(record.score, Some(1.5));
    }

    #[test]
    fn nested_features_do_not_break_overlap_queries() {
        let source = TextFeatureSource {
            features: HashMap::from([(
                "chr1".to_owned(),
                FeatureIndex {
                    records: vec![
                        FeatureRecord {
                            contig: "chr1".to_owned(),
                            start: 0,
                            end: 1_000,
                            name: Some("outer".to_owned()),
                            feature_type: None,
                            score: None,
                            strand: Strand::Unstranded,
                            blocks: Vec::new(),
                            coding_interval: None,
                            peak: None,
                            attributes: Vec::new(),
                        },
                        FeatureRecord {
                            contig: "chr1".to_owned(),
                            start: 10,
                            end: 20,
                            name: Some("inner".to_owned()),
                            feature_type: None,
                            score: None,
                            strand: Strand::Unstranded,
                            blocks: Vec::new(),
                            coding_interval: None,
                            peak: None,
                            attributes: Vec::new(),
                        },
                    ],
                    prefix_max_end: vec![1_000, 1_000],
                },
            )]),
            search_terms: BTreeMap::default(),
            search_index_truncated: false,
        };
        let records = source.query("chr1", 500, 600).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name.as_deref(), Some("outer"));
    }

    #[test]
    fn interval_index_matches_brute_force_queries() {
        let mut state = 0xa076_1d64_78bd_642f_u64;
        let mut index = FeatureIndex::default();
        for record_number in 0..5_000_u64 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let start = state % 1_000_000;
            let length = 1 + state.rotate_left(29) % 50_000;
            index.records.push(FeatureRecord {
                contig: "chr1".to_owned(),
                start,
                end: start + length,
                name: Some(record_number.to_string()),
                feature_type: None,
                score: None,
                strand: Strand::Unstranded,
                blocks: Vec::new(),
                coding_interval: None,
                peak: None,
                attributes: Vec::new(),
            });
        }
        index.finish();
        for query_number in 0..1_000_u64 {
            let start = query_number * 997 % 1_000_000;
            let end = start + 10_000;
            let expected = index
                .records
                .iter()
                .filter(|record| record.start < end && record.end > start)
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(index.query(start, end).unwrap(), expected);
        }
    }

    #[test]
    fn variant_classification_and_breakend_parsing_are_typed() {
        assert_eq!(classify_variant(b"A", b"T"), VariantType::Snv);
        assert_eq!(classify_variant(b"AC", b"GT"), VariantType::Mnv);
        assert_eq!(classify_variant(b"A", b"AT"), VariantType::Insertion);
        assert_eq!(classify_variant(b"AT", b"A"), VariantType::Deletion);
        assert_eq!(classify_variant(b"N", b"<INV>"), VariantType::Inversion);
        assert_eq!(classify_variant(b"N", b"*"), VariantType::SpanningDeletion);

        let partner = parse_breakend_partner(2, b"A]chr2:501]").unwrap();
        assert_eq!(partner.alternate_index, 2);
        assert_eq!(partner.contig, "chr2");
        assert_eq!(partner.position, 500);
        assert!(partner.local_join_after_position);
        assert!(partner.partner_join_after_position);
        let leading = parse_breakend_partner(0, b"[chr3:7[G").unwrap();
        assert!(!leading.local_join_after_position);
        assert!(!leading.partner_join_after_position);
    }

    #[test]
    fn large_cohort_subset_selection_is_bounded_and_preserves_header_indices() {
        let header = VariantHeader {
            samples: (0..25_000).map(|index| format!("sample-{index}")).collect(),
            ..VariantHeader::default()
        };
        let settings = VariantTrackSettings {
            sample_subset_enabled: true,
            sample_subset: (0..10_000)
                .map(|index| format!("sample-{}", index * 2))
                .collect(),
            ..VariantTrackSettings::default()
        };
        assert!(settings.is_valid());
        let selected = selected_sample_indices(&header, &settings);
        assert_eq!(selected.len(), 10_000);
        assert_eq!(selected[0], 0);
        assert_eq!(selected[9_999], 19_998);
        assert_eq!(
            selected_sample_indices(&header, &VariantTrackSettings::default()).len(),
            25_000
        );
    }

    #[test]
    fn indexed_remote_bcf_opens_and_queries_through_http_ranges() {
        let fixture = RemoteVariantFixture::start();
        let mut source = LocalVariantSource::open(&fixture.url).unwrap();
        let query = source.query("chrVariants", 8_500, 16_000, 100).unwrap();

        assert_eq!(query.header.samples, ["tumour", "normal"]);
        assert_eq!(query.records.len(), 6);
        assert_eq!(query.records[0].id.as_deref(), Some("demo-snv"));
        assert!(
            query
                .records
                .iter()
                .any(|record| record.variant_types.contains(&VariantType::Breakend))
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn indexed_bcf_query_preserves_typed_headers_info_filters_and_genotypes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("typed.bcf");
        let mut header = Header::new();
        header.push_record(br"##contig=<ID=chr1,length=1000000>");
        header.push_record(br"##contig=<ID=chr2,length=1000000>");
        header.push_record(br#"##FILTER=<ID=q10,Description="Quality below ten">"#);
        header.push_record(
            br#"##INFO=<ID=END,Number=1,Type=Integer,Description="Structural endpoint">"#,
        );
        header.push_record(br#"##INFO=<ID=AF,Number=A,Type=Float,Description="Allele frequency">"#);
        header
            .push_record(br#"##INFO=<ID=SOMATIC,Number=0,Type=Flag,Description="Somatic event">"#);
        header.push_record(br#"##INFO=<ID=NOTE,Number=1,Type=String,Description="Review note">"#);
        header.push_record(br#"##FORMAT=<ID=GT,Number=1,Type=String,Description="Genotype">"#);
        header.push_record(br#"##FORMAT=<ID=DP,Number=1,Type=Integer,Description="Read depth">"#);
        header.push_sample(b"tumour");
        header.push_sample(b"normal");
        {
            let mut writer = Writer::from_path(&path, &header, false, Format::Bcf).unwrap();
            let mut record = writer.empty_record();
            record.set_rid(Some(0));
            record.set_pos(99);
            record.set_id(b"sv1").unwrap();
            record.set_alleles(&[b"N", b"<DEL>"]).unwrap();
            record.set_qual(9.5);
            record.set_filters(&[b"q10".as_slice()]).unwrap();
            record.push_info_integer(b"END", &[300]).unwrap();
            record.push_info_float(b"AF", &[0.25]).unwrap();
            record.push_info_flag(b"SOMATIC").unwrap();
            record.push_info_string(b"NOTE", &[b"review"]).unwrap();
            record
                .push_genotypes(&[
                    GenotypeAllele::Unphased(0),
                    GenotypeAllele::Unphased(1),
                    GenotypeAllele::Unphased(1),
                    GenotypeAllele::Phased(1),
                ])
                .unwrap();
            record.push_format_integer(b"DP", &[42, 31]).unwrap();
            writer.write(&record).unwrap();

            for (position, alleles) in [
                (399, vec![b"A".as_slice(), b"T".as_slice()]),
                (499, vec![b"AC".as_slice(), b"GT".as_slice()]),
                (599, vec![b"A".as_slice(), b"AT".as_slice()]),
                (699, vec![b"AT".as_slice(), b"A".as_slice()]),
                (799, vec![b"N".as_slice(), b"N]chr2:501]".as_slice()]),
                (
                    899,
                    vec![b"A".as_slice(), b"C".as_slice(), b"<INV>".as_slice()],
                ),
            ] {
                let mut record = writer.empty_record();
                record.set_rid(Some(0));
                record.set_pos(position);
                record.set_alleles(&alleles).unwrap();
                record
                    .push_genotypes(&[
                        GenotypeAllele::UnphasedMissing,
                        GenotypeAllele::UnphasedMissing,
                    ])
                    .unwrap();
                writer.write(&record).unwrap();
            }
        }
        index::build(&path, None, 1, index::Type::Csi(14)).unwrap();

        let mut source = LocalVariantSource::open(&path).unwrap();
        let id_results = source.search_ids("SV", 10).unwrap();
        assert_eq!(id_results.hits.len(), 1);
        assert_eq!(id_results.hits[0].matched_id, "sv1");
        assert_eq!(id_results.hits[0].id, "sv1");
        assert_eq!(id_results.hits[0].contig, "chr1");
        assert_eq!(id_results.hits[0].start, 99);
        let query = source.query("chr1", 0, 1_000, 10).unwrap();
        assert_eq!(query.header.samples, vec!["tumour", "normal"]);
        assert_eq!(query.header.info[0].id, "END");
        assert_eq!(query.header.info[0].description, "Structural endpoint");
        assert_eq!(query.records.len(), 7);
        let record = &query.records[0];
        assert_eq!((record.start, record.end), (99, 300));
        assert_eq!(record.filters, vec!["q10"]);
        assert_eq!(record.variant_types, vec![VariantType::StructuralDeletion]);
        assert!(
            record
                .info
                .iter()
                .any(|field| { field.key == "SOMATIC" && field.value == VariantValue::Flag(true) })
        );
        assert_eq!(record.genotypes.len(), 2);
        assert_eq!(
            record.genotypes[0].alleles,
            vec![
                VariantGenotypeAllele {
                    index: Some(0),
                    phased: false,
                },
                VariantGenotypeAllele {
                    index: Some(1),
                    phased: false,
                },
            ]
        );
        assert!(record.genotypes[1].alleles[1].phased);
        assert_eq!(
            record.genotypes[0].fields[0].value,
            VariantValue::Integers(vec![Some(42)])
        );
        assert_eq!(query.provenance.genotype_cells_loaded, 14);
        assert_eq!(query.provenance.genotype_cells_omitted, 0);
        assert_eq!(query.records[1].variant_types, vec![VariantType::Snv]);
        assert_eq!(query.records[2].variant_types, vec![VariantType::Mnv]);
        assert_eq!(query.records[3].variant_types, vec![VariantType::Insertion]);
        assert_eq!(query.records[4].variant_types, vec![VariantType::Deletion]);
        assert_eq!(query.records[5].variant_types, vec![VariantType::Breakend]);
        assert_eq!(query.records[5].breakends[0].contig, "chr2");
        assert_eq!(query.records[5].breakends[0].position, 500);
        assert_eq!(
            query.records[6].variant_types,
            vec![VariantType::Snv, VariantType::Inversion]
        );

        let filtered = source
            .query_with_settings(
                "chr1",
                0,
                1_000,
                10,
                &VariantTrackSettings {
                    minimum_quality: Some(9.0),
                    allowed_filters: vec!["q10".to_owned()],
                    variant_types: vec![VariantTypeGroup::StructuralDeletion],
                    minimum_allele_frequency: Some(0.20),
                    maximum_allele_frequency: Some(0.30),
                    genotype: VariantGenotypeFilter::Heterozygous,
                    field_predicates: vec![
                        VariantFieldPredicate {
                            scope: VariantFieldScope::Info,
                            key: "NOTE".to_owned(),
                            comparison: VariantFieldComparison::Contains("view".to_owned()),
                            missing: VariantMissingValueRule::Exclude,
                        },
                        VariantFieldPredicate {
                            scope: VariantFieldScope::Format,
                            key: "DP".to_owned(),
                            comparison: VariantFieldComparison::GreaterOrEqual(40.0),
                            missing: VariantMissingValueRule::Exclude,
                        },
                        VariantFieldPredicate {
                            scope: VariantFieldScope::Format,
                            key: "GT".to_owned(),
                            comparison: VariantFieldComparison::Equals("0/1".to_owned()),
                            missing: VariantMissingValueRule::Exclude,
                        },
                    ],
                    sample_subset_enabled: true,
                    sample_subset: vec!["tumour".to_owned()],
                    ..VariantTrackSettings::default()
                },
            )
            .unwrap();
        assert_eq!(filtered.records.len(), 1);
        assert_eq!(filtered.records[0].id.as_deref(), Some("sv1"));
        assert_eq!(filtered.records[0].genotypes.len(), 1);
        assert_eq!(filtered.records[0].genotypes[0].sample_index, 0);
        assert_eq!(filtered.provenance.records_examined, 7);
        assert_eq!(filtered.provenance.records_filtered, 6);
        assert_eq!(filtered.provenance.selected_samples, 1);
        assert_eq!(filtered.provenance.samples_omitted_by_subset, 1);
        assert_eq!(filtered.provenance.genotype_cells_loaded, 1);

        let missing_af_is_explicit = source
            .query_with_settings(
                "chr1",
                0,
                1_000,
                10,
                &VariantTrackSettings {
                    variant_types: vec![VariantTypeGroup::Snv],
                    minimum_allele_frequency: Some(0.20),
                    allele_frequency_missing: VariantMissingValueRule::Include,
                    ..VariantTrackSettings::default()
                },
            )
            .unwrap();
        assert_eq!(
            missing_af_is_explicit
                .records
                .iter()
                .map(|record| record.start)
                .collect::<Vec<_>>(),
            vec![399, 899]
        );

        let pass_deletion = source
            .query_with_settings(
                "chr1",
                0,
                1_000,
                10,
                &VariantTrackSettings {
                    pass_only: true,
                    variant_types: vec![VariantTypeGroup::Deletion],
                    ..VariantTrackSettings::default()
                },
            )
            .unwrap();
        assert_eq!(
            pass_deletion
                .records
                .iter()
                .map(|record| record.start)
                .collect::<Vec<_>>(),
            vec![699]
        );
    }

    #[test]
    fn quantitative_query_cap_and_non_finite_provenance_are_exact() {
        let mut query = QuantitativeQuery::default();
        assert!(retain_quantitative_record(
            &mut query,
            QuantitativeRecord {
                start: 0,
                end: 1,
                value: 2.0,
            },
            2,
        ));
        assert!(retain_quantitative_record(
            &mut query,
            QuantitativeRecord {
                start: 1,
                end: 2,
                value: f32::NAN,
            },
            2,
        ));
        assert!(!retain_quantitative_record(
            &mut query,
            QuantitativeRecord {
                start: 2,
                end: 3,
                value: 4.0,
            },
            2,
        ));
        assert_eq!(
            query.provenance,
            QuantitativeQueryProvenance {
                records_examined: 3,
                records_retained: 2,
                non_finite_records: 1,
                records_truncated: true,
            }
        );
    }

    #[test]
    fn gwas_header_aliases_coordinates_values_and_missing_rows_are_exact() {
        let source = TextGwasSource::from_reader(Cursor::new(
            b"# generated by the association pipeline\n\
              #CHROM POS ID BETA P EXTRA\n\
              chr1 101 rs-one 0.25 1e-8 retained\n\
              chr1 151 rs-missing -0.5 NA ignored\n\
              chr2 201 rs-zero 1.0 0 underflow\n",
        ))
        .unwrap();
        let query = source.query("chr1", 100, 200);
        assert_eq!(query.records.len(), 1);
        let record = &query.records[0];
        assert_eq!(record.contig, "chr1");
        assert_eq!(record.position, 100);
        assert_eq!(record.identifier, "rs-one");
        assert!((record.p_value - 1e-8).abs() < f64::EPSILON);
        assert!((record.minus_log10_p - 8.0).abs() < 1e-5);
        assert_eq!(record.fields[4], ("P".to_owned(), "1e-8".to_owned()));
        assert_eq!(query.provenance.source_rows_ignored, 1);
        assert!(!query.provenance.records_truncated);
        let zero = source.query("chr2", 0, 1_000);
        assert_eq!(zero.records[0].position, 200);
        assert!(zero.records[0].minus_log10_p.is_infinite());
    }

    #[test]
    fn gwas_schema_and_malformed_rows_fail_explicitly() {
        let missing = TextGwasSource::from_reader(Cursor::new(b"CHR BP P\nchr1 1 0.5\n"));
        assert!(matches!(
            missing,
            Err(TrackError::MalformedLine { line: 1, .. })
        ));

        for contents in [
            "CHR BP SNP P\nchr1 0 rs1 0.5\n",
            "CHR BP SNP P\nchr1 1 rs1 1.5\n",
            "CHR BP SNP P EXTRA\nchr1 1 rs1 0.5\n",
        ] {
            assert!(matches!(
                TextGwasSource::from_reader(Cursor::new(contents.as_bytes())),
                Err(TrackError::MalformedLine { line: 2, .. })
            ));
        }
        assert!(matches!(
            TextGwasSource::from_reader(Cursor::new(b"## comment only\n")),
            Err(TrackError::InvalidSchema(_))
        ));
    }

    #[test]
    fn gwas_query_cap_keeps_the_strongest_point_in_each_positional_chunk() {
        let records = [(0, 1.0), (1, 3.0), (2, 2.0), (3, 4.0), (4, 5.0)]
            .into_iter()
            .map(|(position, score)| GwasRecord {
                contig: "chr1".to_owned(),
                position,
                identifier: format!("rs{position}"),
                p_value: 10_f64.powf(-f64::from(score)),
                minus_log10_p: score,
                fields: Vec::new(),
            })
            .collect::<Vec<_>>();
        let retained = bounded_gwas_records(&records, 2);
        assert_eq!(
            retained
                .iter()
                .map(|record| record.position)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
    }

    #[test]
    fn gzip_gwas_opens_through_the_production_path() {
        use std::io::Write as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("associations.gwas.gz");
        let file = File::create(&path).unwrap();
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        encoder
            .write_all(b"chromosome position markername p.value note\nchr7 42 rs7 0.001 exact\n")
            .unwrap();
        encoder.finish().unwrap();
        let source = TextGwasSource::open(path).unwrap();
        let query = source.query("chr7", 0, 100);
        assert_eq!(query.records.len(), 1);
        assert_eq!(query.records[0].identifier, "rs7");
        assert!((query.records[0].minus_log10_p - 3.0).abs() < 1e-5);
    }
}
