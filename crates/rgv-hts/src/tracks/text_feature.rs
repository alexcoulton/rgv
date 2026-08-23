#[allow(clippy::wildcard_imports)]
use super::*;

pub struct TextFeatureSource {
    pub(super) features: HashMap<String, FeatureIndex>,
    pub(super) search_terms: BTreeMap<String, Vec<(String, usize)>>,
    pub(super) search_index_truncated: bool,
}

#[derive(Default)]
pub(super) struct FeatureIndex {
    pub(super) records: Vec<FeatureRecord>,
    pub(super) prefix_max_end: Vec<u64>,
}

impl FeatureIndex {
    pub(super) fn finish(&mut self) {
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

    pub(super) fn query(&self, start: u64, end: u64) -> Result<Vec<FeatureRecord>, TrackError> {
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
    let mut normalized = record_terms
        .into_iter()
        .map(normalized_feature_search_term)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

pub(super) fn normalized_feature_search_term(value: &str) -> String {
    value.trim().to_lowercase().chars().take(256).collect()
}
