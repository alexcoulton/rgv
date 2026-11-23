//! Name search over annotation features: which terms a record contributes, and the two
//! indexes built from them (one over a fully loaded source, one streamed from an indexed file).
//!
//! Names are indexed once per gene and per transcript, not once per row. A GTF or GFF3 file
//! repeats `gene_id`, `gene_name`, `transcript_id` and `transcript_name` on every exon, CDS,
//! UTR and codon row; indexing each of those rows made a GENCODE-sized file exhaust the
//! association limit before reaching chromosome 3. Now:
//!
//! * gene rows (any type ending in `gene`) contribute all their terms;
//! * other rows (transcripts, and every record of a format without types) contribute the
//!   terms no gene row on the same contig already carries;
//! * sub-feature rows (exon, CDS, UTR, start/stop codon, Selenocysteine, …) contribute only
//!   gene and transcript identity terms that no gene or transcript row on the same contig
//!   carries, each once per gene: the term then points at the span of all such rows of that
//!   gene (or transcript), so a file of only exon rows still navigates to the whole gene.
//!   Their own row identifiers (`ID=exon:…`, `exon_id`) are not indexed. A sub-feature row
//!   with no identity attribute and no `Parent` is indexed by its own names, once per name.
//!
//! A fully loaded GTF or GFF3 source draws assembled transcript models, not lines (see
//! `gene_model`). Its index is still built over the lines, by the rules above, and then
//! re-pointed: a transcript line's terms at its model, a gene line's at a copy of the gene kept
//! by the index (the gene line itself is not drawn).
//!
//! An identifier with a version suffix (`ENSG00000163939.18`, `NM_000546.6`: letters, digits,
//! a dot and digits) is also indexed without it, so an unversioned query matches exactly.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet, hash_map::Entry};

use super::text_feature::FeatureIndex;
use super::{
    FeatureRecord, FeatureSearchHit, FeatureSearchIndexCoverage, FeatureSearchResults,
    MAX_FEATURE_SEARCH_RESULTS, MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS,
};

/// Attributes naming the gene a row belongs to.
const GENE_IDENTITY_KEYS: [&str; 5] = [
    "gene_id",
    "gene_name",
    "gene_name2",
    "gene_synonym",
    "name2",
];
/// Attributes naming the transcript a row belongs to.
const TRANSCRIPT_IDENTITY_KEYS: [&str; 2] = ["transcript_id", "transcript_name"];
/// Row types that describe part of a transcript and repeat its names.
const SUB_FEATURE_TYPES: [&str; 13] = [
    "exon",
    "cds",
    "utr",
    "five_prime_utr",
    "three_prime_utr",
    "5utr",
    "3utr",
    "start_codon",
    "stop_codon",
    "selenocysteine",
    "intron",
    "coding_exon",
    "noncoding_exon",
];

/// Where a row sits in the gene → transcript → sub-feature hierarchy. Lower sorts first and
/// wins a term both carry.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum SearchLevel {
    Gene,
    Other,
    SubFeature,
}

pub(super) fn search_level(record: &FeatureRecord) -> SearchLevel {
    let Some(feature_type) = record.feature_type.as_deref() else {
        return SearchLevel::Other;
    };
    // Called for every line of a large file: compared without allocating.
    if SUB_FEATURE_TYPES
        .iter()
        .any(|candidate| feature_type.eq_ignore_ascii_case(candidate))
    {
        SearchLevel::SubFeature
    } else if feature_type.len() >= 4
        && feature_type.as_bytes()[feature_type.len() - 4..].eq_ignore_ascii_case(b"gene")
    {
        SearchLevel::Gene
    } else {
        SearchLevel::Other
    }
}

/// What a span built from sub-feature rows stands for.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum SpanKind {
    Gene,
    Transcript,
    /// The rows' own names (no gene or transcript identity).
    Own,
}

/// Every term a gene or transcript row is found by.
pub(super) fn feature_search_terms(record: &FeatureRecord) -> Vec<String> {
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
    let mut normalized = Vec::new();
    for term in record_terms {
        push_term_with_unversioned(&mut normalized, normalized_feature_search_term(term));
    }
    normalized.sort();
    normalized.dedup();
    normalized
}

/// The gene and transcript identity terms of a sub-feature row (see the module notes).
pub(super) fn sub_feature_search_terms(record: &FeatureRecord) -> Vec<(String, SpanKind)> {
    let mut terms = Vec::new();
    for (key, value) in &record.attributes {
        let key = key.to_ascii_lowercase();
        let kind = if GENE_IDENTITY_KEYS.contains(&key.as_str()) {
            SpanKind::Gene
        } else if TRANSCRIPT_IDENTITY_KEYS.contains(&key.as_str()) {
            SpanKind::Transcript
        } else {
            continue;
        };
        for part in value.split(',') {
            let mut expanded = Vec::new();
            push_term_with_unversioned(&mut expanded, normalized_feature_search_term(part));
            terms.extend(expanded.into_iter().map(|term| (term, kind)));
        }
    }
    let has_parent = record
        .attributes
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case("parent"));
    if terms.is_empty() && !has_parent {
        terms = feature_search_terms(record)
            .into_iter()
            .map(|term| (term, SpanKind::Own))
            .collect();
    }
    // A term named both ways (gene and transcript) stands for the gene.
    terms.sort();
    terms.dedup_by(|later, earlier| later.0 == earlier.0);
    terms
}

fn push_term_with_unversioned(terms: &mut Vec<String>, term: String) {
    if term.is_empty() {
        return;
    }
    if let Some(unversioned) = unversioned_identifier(&term) {
        terms.push(unversioned.to_owned());
    }
    terms.push(term);
}

/// `ENSG00000163939.18` → `ENSG00000163939`, `NM_000546.6` → `NM_000546`: an identifier of
/// letters (and underscores), digits, a dot and a numeric version, without the version.
pub(super) fn unversioned_identifier(term: &str) -> Option<&str> {
    let (base, version) = term.rsplit_once('.')?;
    if version.is_empty() || !version.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let digits_start = base.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (prefix, digits) = base.split_at(digits_start);
    let identifier_prefix = !prefix.is_empty()
        && prefix.bytes().any(|byte| byte.is_ascii_alphabetic())
        && prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'_');
    (identifier_prefix && !digits.is_empty()).then_some(base)
}

pub(super) fn normalized_feature_search_term(value: &str) -> String {
    value.trim().to_lowercase().chars().take(256).collect()
}

/// Groups the sub-feature rows of one gene: rows sharing a name but belonging to different
/// genes (paralogs, repeated RNA gene names) stay separate spans.
fn span_group(record: &FeatureRecord) -> String {
    ["gene_id", "transcript_id"]
        .into_iter()
        .find_map(|key| {
            record
                .attributes
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.clone())
        })
        .unwrap_or_default()
}

/// The record a search term built from sub-feature rows points at: the first row, labelled as
/// the gene or transcript it stands for, widened by [`widen_span`] to every row of it.
fn search_span_record(record: &FeatureRecord, kind: SpanKind) -> FeatureRecord {
    let keeps = |key: &str| {
        let key = key.to_ascii_lowercase();
        match kind {
            SpanKind::Gene => {
                GENE_IDENTITY_KEYS.contains(&key.as_str())
                    || key == "gene_type"
                    || key == "gene_biotype"
            }
            SpanKind::Transcript => {
                GENE_IDENTITY_KEYS.contains(&key.as_str())
                    || TRANSCRIPT_IDENTITY_KEYS.contains(&key.as_str())
                    || key.starts_with("gene_")
                    || key.starts_with("transcript_")
            }
            SpanKind::Own => true,
        }
    };
    FeatureRecord {
        contig: record.contig.clone(),
        start: record.start,
        end: record.end,
        name: record.name.clone(),
        feature_type: match kind {
            SpanKind::Gene => Some("gene".to_owned()),
            SpanKind::Transcript => Some("transcript".to_owned()),
            SpanKind::Own => record.feature_type.clone(),
        },
        score: None,
        strand: record.strand,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: record
            .attributes
            .iter()
            .filter(|(key, _)| keeps(key))
            .cloned()
            .collect(),
    }
}

fn widen_span(span: &mut FeatureRecord, record: &FeatureRecord) {
    span.start = span.start.min(record.start);
    span.end = span.end.max(record.end);
}

/// What a search for a gene whose line is not drawn (its transcripts are) finds: the gene's
/// span, type and identity attributes.
pub(super) fn container_search_record(record: FeatureRecord) -> FeatureRecord {
    let FeatureRecord {
        contig,
        start,
        end,
        name,
        feature_type,
        strand,
        attributes,
        ..
    } = record;
    FeatureRecord {
        contig,
        start,
        end,
        name,
        feature_type,
        score: None,
        strand,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: attributes
            .into_iter()
            .filter(|(key, _)| {
                let key = key.to_ascii_lowercase();
                GENE_IDENTITY_KEYS.contains(&key.as_str())
                    || matches!(
                        key.as_str(),
                        "id" | "name" | "alias" | "gene_type" | "gene_biotype" | "biotype"
                    )
            })
            .collect(),
    }
}

/// A search hit needs identity and location, not exon structure or the full attribute list.
pub(super) fn slim_search_record(record: &FeatureRecord) -> FeatureRecord {
    FeatureRecord {
        contig: record.contig.clone(),
        start: record.start,
        end: record.end,
        name: record.name.clone(),
        feature_type: record.feature_type.clone(),
        score: None,
        strand: record.strand,
        blocks: Vec::new(),
        coding_interval: None,
        peak: None,
        attributes: Vec::new(),
    }
}

/// Chromosome order for indexing: numbered contigs by number (`chr2` before `chr10`), then X,
/// Y and the mitochondrion, then everything else by name. A fully loaded source is opened
/// without the reference, so its dictionary order is not available here; this order matches it
/// for the usual assemblies and makes any truncation fall on the last contigs, not on `chr3`.
pub(super) fn natural_contig_order(left: &str, right: &str) -> Ordering {
    fn key(name: &str) -> (u8, u64, String) {
        let lower = name.to_ascii_lowercase();
        let bare = lower.strip_prefix("chr").unwrap_or(&lower);
        let rank = match bare.parse::<u64>() {
            Ok(number) => return (0, number, lower),
            Err(_) => match bare {
                "x" => 1,
                "y" => 2,
                "m" | "mt" => 3,
                _ => 4,
            },
        };
        (rank, 0, lower)
    }
    key(left).cmp(&key(right)).then_with(|| left.cmp(right))
}

/// Orders candidates (exact matches first, then by term and position), drops repeats of the
/// same feature found through several terms, and keeps `limit`.
fn rank_candidates(
    mut candidates: Vec<(bool, String, FeatureRecord)>,
    limit: usize,
) -> Vec<FeatureSearchHit> {
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
    let mut seen = HashSet::new();
    candidates.retain(|(_, _, record)| {
        seen.insert((
            record.contig.clone(),
            record.start,
            record.end,
            record.name.clone(),
            record.feature_type.clone(),
        ))
    });
    candidates.truncate(limit);
    candidates
        .into_iter()
        .map(|(_, matched_term, record)| FeatureSearchHit {
            matched_term,
            record,
        })
        .collect()
}

/// Where a term of a fully loaded source points.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum SearchTarget {
    /// Record `index` of the contig's feature index.
    Record { contig: String, index: usize },
    /// A gene or transcript span built from its sub-feature rows.
    Span(usize),
}

/// Where [`FeatureSearchIndex::retarget_records`] moves a record target.
pub(super) enum Retarget {
    /// Record `index` of the same contig.
    Record(usize),
    /// One of the index's own records.
    Span(usize),
}

/// The name index of a fully loaded source.
#[derive(Debug, Default)]
pub(super) struct FeatureSearchIndex {
    terms: BTreeMap<String, Vec<SearchTarget>>,
    spans: Vec<FeatureRecord>,
    coverage: FeatureSearchIndexCoverage,
}

impl FeatureSearchIndex {
    /// Indexes contigs in [`natural_contig_order`] until `cap` associations are held.
    pub(super) fn build(features: &HashMap<String, FeatureIndex>, cap: usize) -> Self {
        let mut index = Self::default();
        let mut contigs = features.keys().collect::<Vec<_>>();
        contigs.sort_by(|left, right| natural_contig_order(left, right));
        let mut previous: Option<&String> = None;
        for contig in contigs {
            if !index.index_contig(contig, &features[contig].records, cap) {
                index.coverage.truncated = true;
                index.coverage.complete_through = previous.cloned();
                return index;
            }
            previous = Some(contig);
        }
        index
    }

    pub(super) const fn coverage(&self) -> &FeatureSearchIndexCoverage {
        &self.coverage
    }

    /// Re-points the record targets of an index built over a file's lines at what those lines
    /// became once assembled into transcript models (see `gene_model`). `resolve` is given a
    /// line's contig and index and the index's own records, to which it may add one; it
    /// returns the new target, or `None` to drop the association.
    pub(super) fn retarget_records(
        &mut self,
        mut resolve: impl FnMut(&str, usize, &mut Vec<FeatureRecord>) -> Option<Retarget>,
    ) {
        let Self { terms, spans, .. } = self;
        for targets in terms.values_mut() {
            targets.retain_mut(|target| {
                let SearchTarget::Record { contig, index } = target else {
                    return true;
                };
                match resolve(contig, *index, spans) {
                    Some(Retarget::Record(record)) => *index = record,
                    Some(Retarget::Span(span)) => *target = SearchTarget::Span(span),
                    None => return false,
                }
                true
            });
        }
    }

    fn push(&mut self, term: String, target: SearchTarget, cap: usize) -> bool {
        if self.coverage.associations >= cap {
            return false;
        }
        self.terms.entry(term).or_default().push(target);
        self.coverage.associations += 1;
        true
    }

    /// Returns false when the cap stopped it part-way.
    fn index_contig(&mut self, contig: &str, records: &[FeatureRecord], cap: usize) -> bool {
        let mut order = (0..records.len()).collect::<Vec<_>>();
        order.sort_by_key(|&index| search_level(&records[index]));
        let mut gene_terms = HashSet::<String>::new();
        let mut primary_terms = HashSet::<String>::new();
        let mut spans = HashMap::<(String, String), usize>::new();
        for index in order {
            let record = &records[index];
            let level = search_level(record);
            if level == SearchLevel::SubFeature {
                let group = span_group(record);
                for (term, kind) in sub_feature_search_terms(record) {
                    if primary_terms.contains(&term) {
                        continue;
                    }
                    match spans.entry((term, group.clone())) {
                        Entry::Occupied(entry) => widen_span(&mut self.spans[*entry.get()], record),
                        Entry::Vacant(entry) => {
                            let span = self.spans.len();
                            let term = entry.key().0.clone();
                            if !self.push(term, SearchTarget::Span(span), cap) {
                                return false;
                            }
                            self.spans.push(search_span_record(record, kind));
                            entry.insert(span);
                        }
                    }
                }
                continue;
            }
            for term in feature_search_terms(record) {
                if level == SearchLevel::Other && gene_terms.contains(&term) {
                    continue;
                }
                if level == SearchLevel::Gene {
                    gene_terms.insert(term.clone());
                }
                primary_terms.insert(term.clone());
                let target = SearchTarget::Record {
                    contig: contig.to_owned(),
                    index,
                };
                if !self.push(term, target, cap) {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn search(
        &self,
        features: &HashMap<String, FeatureIndex>,
        query: &str,
        limit: usize,
    ) -> FeatureSearchResults {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        let mut scanned = 0_usize;
        let mut scan_truncated = false;
        'terms: for (term, targets) in self.terms.range(query.to_owned()..) {
            if !term.starts_with(query) {
                break;
            }
            for target in targets {
                if scanned == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break 'terms;
                }
                scanned = scanned.saturating_add(1);
                if !seen.insert(target) {
                    continue;
                }
                let record = match target {
                    SearchTarget::Record { contig, index } => features
                        .get(contig)
                        .and_then(|features| features.records.get(*index)),
                    SearchTarget::Span(span) => self.spans.get(*span),
                };
                if let Some(record) = record {
                    candidates.push((term == query, term.clone(), record.clone()));
                }
            }
        }
        FeatureSearchResults {
            hits: rank_candidates(candidates, limit.min(MAX_FEATURE_SEARCH_RESULTS)),
            index_truncated: self.coverage.truncated,
            scan_truncated,
        }
    }
}

/// One term's feature on one contig in a streamed index.
#[derive(Debug)]
struct StreamedHit {
    level: SearchLevel,
    /// For a span of sub-feature rows, the gene it belongs to.
    group: String,
    record: FeatureRecord,
}

/// The name index of an indexed (tabix or CSI) source, built by one streaming pass: for every
/// term, one feature per contig — a gene row before a transcript row, a transcript row before
/// sub-feature rows, the widest row among equals.
#[derive(Debug, Default)]
pub(super) struct StreamingFeatureSearch {
    terms: BTreeMap<String, Vec<StreamedHit>>,
    coverage: FeatureSearchIndexCoverage,
    current_contig: Option<String>,
    last_left_contig: Option<String>,
}

impl StreamingFeatureSearch {
    /// Adds one record read in file order. Returns false (and marks the index truncated) once
    /// `cap` associations are held.
    pub(super) fn add(&mut self, record: &FeatureRecord, cap: usize) -> bool {
        if self.current_contig.as_deref() != Some(record.contig.as_str()) {
            self.last_left_contig = self.current_contig.replace(record.contig.clone());
        }
        let level = search_level(record);
        let (terms, group) = if level == SearchLevel::SubFeature {
            let terms = sub_feature_search_terms(record)
                .into_iter()
                .map(|(term, kind)| (term, Some(kind)))
                .collect::<Vec<_>>();
            (terms, span_group(record))
        } else {
            let terms = feature_search_terms(record)
                .into_iter()
                .map(|term| (term, None))
                .collect::<Vec<_>>();
            (terms, String::new())
        };
        for (term, kind) in terms {
            let represent = || {
                kind.map_or_else(
                    || slim_search_record(record),
                    |kind| search_span_record(record, kind),
                )
            };
            let hits = self.terms.entry(term).or_default();
            if let Some(existing) = hits
                .iter_mut()
                .find(|hit| hit.record.contig == record.contig)
            {
                match level.cmp(&existing.level) {
                    Ordering::Less => {
                        *existing = StreamedHit {
                            level,
                            group: group.clone(),
                            record: represent(),
                        };
                    }
                    Ordering::Greater => {}
                    Ordering::Equal if level == SearchLevel::SubFeature => {
                        if existing.group == group {
                            widen_span(&mut existing.record, record);
                        }
                    }
                    Ordering::Equal => {
                        if record.end - record.start > existing.record.end - existing.record.start {
                            existing.record = represent();
                        }
                    }
                }
                continue;
            }
            if self.coverage.associations >= cap {
                self.truncate();
                return false;
            }
            self.coverage.associations += 1;
            hits.push(StreamedHit {
                level,
                group: group.clone(),
                record: represent(),
            });
        }
        true
    }

    /// Marks the index as stopped before the end of the file.
    pub(super) fn truncate(&mut self) {
        self.coverage.truncated = true;
        self.coverage.complete_through = self.last_left_contig.clone();
    }

    pub(super) const fn coverage(&self) -> &FeatureSearchIndexCoverage {
        &self.coverage
    }

    pub(super) fn search(&self, query: &str, limit: usize) -> FeatureSearchResults {
        let mut candidates = Vec::new();
        let mut scan_truncated = false;
        'terms: for (term, hits) in self.terms.range(query.to_owned()..) {
            if !term.starts_with(query) {
                break;
            }
            for hit in hits {
                if candidates.len() == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break 'terms;
                }
                candidates.push((term == query, term.clone(), hit.record.clone()));
            }
        }
        FeatureSearchResults {
            hits: rank_candidates(candidates, limit.min(MAX_FEATURE_SEARCH_RESULTS)),
            index_truncated: self.coverage.truncated,
            scan_truncated,
        }
    }
}
