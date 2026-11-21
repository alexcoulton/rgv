#[allow(clippy::wildcard_imports)]
use super::*;

pub struct TextFeatureSource {
    pub(super) features: HashMap<String, FeatureIndex>,
    pub(super) search_terms: BTreeMap<String, Vec<(String, usize)>>,
    pub(super) search_index_truncated: bool,
    /// Present when the source is a bgzip-compressed file with a tabix or CSI index. Records are
    /// then read on demand for each query and `features` stays empty.
    pub(super) indexed: Option<IndexedTextFeatures>,
    /// Lines skipped because they could not be parsed, and the first such line.
    pub(super) malformed_lines: usize,
    pub(super) first_malformed_line: Option<(usize, String)>,
}

/// On-demand access to a tabix-indexed annotation file of any size.
pub(super) struct IndexedTextFeatures {
    format: FeatureFormat,
    contigs: Vec<String>,
    backend: IndexedTextBackend,
    /// Unparseable lines skipped by region queries (a line re-read by a later query counts
    /// again).
    malformed_lines: std::sync::atomic::AtomicUsize,
    /// Built by one bounded streaming pass the first time a name search is submitted.
    search: OnceLock<IndexedFeatureSearch>,
}

/// Where the lines of an indexed annotation come from.
enum IndexedTextBackend {
    /// A local file, read by `HTSlib`.
    Local {
        path: PathBuf,
        reader: Mutex<rust_htslib::tbx::Reader>,
    },
    /// A remote file, read over the HTTP range reader.
    Remote(Box<Mutex<super::remote_tabix::RemoteTabixText>>),
}

/// Compressed bytes one remote name-search index build may transfer before the index is
/// reported as truncated. A region query never needs the whole file; name search does.
const MAX_REMOTE_FEATURE_SEARCH_SCAN_BYTES: u64 = 64 * 1024 * 1024;

fn poisoned() -> TrackError {
    TrackError::Backend("indexed annotation reader is poisoned".to_owned())
}

#[derive(Default)]
struct IndexedFeatureSearch {
    /// For every search term, the widest feature carrying it on each contig.
    terms: BTreeMap<String, Vec<FeatureRecord>>,
    truncated: bool,
}

/// Initial distance searched backwards for the previous feature; quadrupled until one is found.
const INDEXED_PREVIOUS_FEATURE_WINDOW: u64 = 16_384;

fn parse_feature_line(format: FeatureFormat, line: &str) -> Result<Option<FeatureRecord>, String> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty()
        || line.starts_with('#')
        || line.starts_with("track ")
        || line.starts_with("browser ")
        || (format == FeatureFormat::Psl && is_psl_header_line(line))
    {
        return Ok(None);
    }
    match format {
        FeatureFormat::Bed => parse_bed(line),
        FeatureFormat::Gff3 => parse_gff(line, false),
        FeatureFormat::Gtf => parse_gff(line, true),
        FeatureFormat::GenePred => parse_gene_pred(line, false),
        FeatureFormat::RefFlat => parse_gene_pred(line, true),
        FeatureFormat::NarrowPeak => parse_narrow_peak(line),
        FeatureFormat::BroadPeak => parse_broad_peak(line),
        FeatureFormat::Psl => parse_psl(line),
    }
    .map(Some)
}

/// Whether a GFF3 line begins the embedded sequence section: the `##FASTA` directive, or a bare
/// FASTA header where the directive was omitted.
fn starts_gff3_fasta_section(line: &str) -> bool {
    line.trim_end().eq_ignore_ascii_case("##FASTA") || line.starts_with('>')
}

/// The tabix or CSI index beside a bgzip-compressed annotation file, if one exists.
fn adjacent_text_index(path: &Path) -> Option<PathBuf> {
    ["tbi", "csi"].into_iter().find_map(|extension| {
        let mut candidate = path.as_os_str().to_owned();
        candidate.push(".");
        candidate.push(extension);
        let candidate = PathBuf::from(candidate);
        candidate.is_file().then_some(candidate)
    })
}

impl IndexedTextFeatures {
    fn open(path: &Path, format: FeatureFormat) -> Result<Self, TrackError> {
        // `tbx::Reader` dereferences the null handle `HTSlib` returns for a path it cannot
        // read (a directory, a file without read permission).
        crate::htslib_guard::ensure_readable_file(path)?;
        let reader = rust_htslib::tbx::Reader::from_path(path)
            .map_err(|error| TrackError::Backend(error.to_string()))?;
        let contigs = reader.seqnames();
        Ok(Self {
            format,
            contigs,
            backend: IndexedTextBackend::Local {
                path: path.to_owned(),
                reader: Mutex::new(reader),
            },
            malformed_lines: std::sync::atomic::AtomicUsize::new(0),
            search: OnceLock::new(),
        })
    }

    fn remote(remote: super::remote_tabix::RemoteTabixText, format: FeatureFormat) -> Self {
        Self {
            format,
            contigs: remote.sequence_names(),
            backend: IndexedTextBackend::Remote(Box::new(Mutex::new(remote))),
            malformed_lines: std::sync::atomic::AtomicUsize::new(0),
            search: OnceLock::new(),
        }
    }

    /// Visits every record overlapping `[start, end)` in file order until `visit` returns false.
    fn visit(
        &self,
        contig: &str,
        start: u64,
        end: u64,
        mut visit: impl FnMut(FeatureRecord) -> Result<bool, TrackError>,
    ) -> Result<(), TrackError> {
        use rust_htslib::tbx::Read as _;
        if start >= end || !self.contigs.iter().any(|name| name == contig) {
            return Ok(());
        }
        // Whether to go on to the next line.
        let mut accept = |line: &[u8]| -> Result<bool, TrackError> {
            if line.len() > MAX_FEATURE_LINE_BYTES {
                return Err(TrackError::ResourceLimit(format!(
                    "indexed feature line exceeds {MAX_FEATURE_LINE_BYTES} bytes"
                )));
            }
            let text = std::str::from_utf8(line)
                .map_err(|_| TrackError::Backend("indexed feature line is not UTF-8".to_owned()))?;
            // One unparseable line must not blank the whole region; it is skipped and counted.
            let feature = match parse_feature_line(self.format, text) {
                Ok(Some(feature)) => feature,
                Ok(None) => return Ok(true),
                Err(_) => {
                    self.malformed_lines
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(true);
                }
            };
            if feature.start < end && feature.end > start {
                return visit(feature);
            }
            Ok(true)
        };
        let mut line = Vec::new();
        match &self.backend {
            IndexedTextBackend::Local { reader, .. } => {
                let mut reader = reader.lock().map_err(|_| poisoned())?;
                let tid = reader
                    .tid(contig)
                    .map_err(|error| TrackError::Backend(error.to_string()))?;
                reader
                    .fetch(tid, start, end)
                    .map_err(|error| TrackError::Backend(error.to_string()))?;
                while reader
                    .read(&mut line)
                    .map_err(|error| TrackError::Backend(error.to_string()))?
                {
                    if !accept(&line)? {
                        break;
                    }
                }
            }
            IndexedTextBackend::Remote(remote) => {
                let mut remote = remote.lock().map_err(|_| poisoned())?;
                let mut cursor = remote.region(contig, start, end)?;
                while remote.next_region_line(&mut cursor, &mut line)? {
                    if !accept(&line)? {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn query(&self, contig: &str, start: u64, end: u64) -> Result<Vec<FeatureRecord>, TrackError> {
        let mut records = Vec::new();
        self.visit(contig, start, end, |feature| {
            if records.len() == MAX_FEATURE_RECORDS_PER_QUERY {
                return Err(TrackError::ResourceLimit(format!(
                    "feature query exceeds the {MAX_FEATURE_RECORDS_PER_QUERY}-record limit"
                )));
            }
            records.push(feature);
            Ok(true)
        })?;
        records.sort_by_key(|record| (record.start, record.end));
        Ok(records)
    }

    fn adjacent_feature(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<FeatureRecord> {
        if forward {
            let mut next: Option<FeatureRecord> = None;
            // Records are start-sorted, so reading stops at the first later start.
            self.visit(contig, position, u64::from(u32::MAX), |feature| {
                if feature.start < position {
                    return Ok(true);
                }
                match &next {
                    // Later starts cannot improve the answer.
                    Some(current) if feature.start > current.start => return Ok(false),
                    // Among equal starts prefer the shortest, as the in-memory order does.
                    Some(current) if feature.end >= current.end => {}
                    _ => next = Some(feature),
                }
                Ok(true)
            })
            .ok()?;
            return next;
        }
        let mut window = INDEXED_PREVIOUS_FEATURE_WINDOW;
        loop {
            let start = position.saturating_sub(window);
            let mut previous: Option<FeatureRecord> = None;
            self.visit(contig, start, position, |feature| {
                if feature.end <= position
                    && previous
                        .as_ref()
                        .is_none_or(|current| feature.start >= current.start)
                {
                    previous = Some(feature);
                }
                Ok(true)
            })
            .ok()?;
            if previous.is_some() || start == 0 {
                return previous;
            }
            window = window.saturating_mul(4);
        }
    }

    /// Streams the whole file once, retaining only one slim record per search term and contig.
    /// A remote file is streamed through the range reader until
    /// [`MAX_REMOTE_FEATURE_SEARCH_SCAN_BYTES`] have been transferred (or its transfer budget
    /// or cancellation stops it); the index is then reported as truncated.
    fn build_search(&self) -> IndexedFeatureSearch {
        let mut search = IndexedFeatureSearch::default();
        let mut associations = 0_usize;
        // Whether to go on to the next line.
        let mut add = |search: &mut IndexedFeatureSearch, line: &str| -> bool {
            let Ok(Some(record)) = parse_feature_line(self.format, line) else {
                return true;
            };
            for term in feature_search_terms(&record) {
                let hits = search.terms.entry(term).or_default();
                if let Some(existing) = hits.iter_mut().find(|hit| hit.contig == record.contig) {
                    if record.end - record.start > existing.end - existing.start {
                        *existing = slim_search_record(&record);
                    }
                    continue;
                }
                if associations == MAX_FEATURE_SEARCH_ASSOCIATIONS {
                    search.truncated = true;
                    return false;
                }
                associations += 1;
                hits.push(slim_search_record(&record));
            }
            true
        };
        match &self.backend {
            IndexedTextBackend::Local { path, .. } => {
                let Ok(file) = File::open(path) else {
                    search.truncated = true;
                    return search;
                };
                let mut reader = BufReader::new(MultiGzDecoder::new(file));
                let mut line = String::new();
                loop {
                    line.clear();
                    match (&mut reader)
                        .take((MAX_FEATURE_LINE_BYTES + 1) as u64)
                        .read_line(&mut line)
                    {
                        Ok(0) => break,
                        Ok(bytes) if bytes <= MAX_FEATURE_LINE_BYTES => {}
                        _ => {
                            search.truncated = true;
                            break;
                        }
                    }
                    if !add(&mut search, &line) {
                        break;
                    }
                }
            }
            IndexedTextBackend::Remote(remote) => {
                let Ok(mut remote) = remote.lock() else {
                    search.truncated = true;
                    return search;
                };
                let Ok(baseline) = remote.start_scan() else {
                    search.truncated = true;
                    return search;
                };
                let mut line = Vec::new();
                loop {
                    if remote.response_bytes().saturating_sub(baseline)
                        > MAX_REMOTE_FEATURE_SEARCH_SCAN_BYTES
                    {
                        search.truncated = true;
                        break;
                    }
                    match remote.next_scan_line(&mut line) {
                        Ok(true) if line.len() <= MAX_FEATURE_LINE_BYTES => {}
                        Ok(false) => break,
                        _ => {
                            search.truncated = true;
                            break;
                        }
                    }
                    let Ok(text) = std::str::from_utf8(&line) else {
                        continue;
                    };
                    if !add(&mut search, text) {
                        break;
                    }
                }
            }
        }
        search
    }

    fn search(&self, query: &str, limit: usize) -> FeatureSearchResults {
        let search = self.search.get_or_init(|| self.build_search());
        let limit = limit.min(MAX_FEATURE_SEARCH_RESULTS);
        let mut candidates = Vec::new();
        let mut scan_truncated = false;
        'terms: for (term, hits) in search.terms.range(query.to_owned()..) {
            if !term.starts_with(query) {
                break;
            }
            for hit in hits {
                if candidates.len() == MAX_FEATURE_SEARCH_SCANNED_ASSOCIATIONS {
                    scan_truncated = true;
                    break 'terms;
                }
                candidates.push((term == query, term.clone(), hit.clone()));
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
            index_truncated: search.truncated,
            scan_truncated,
        }
    }
}

/// A search hit needs identity and location, not exon structure or the full attribute list.
fn slim_search_record(record: &FeatureRecord) -> FeatureRecord {
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

/// Largest remote text annotation which is downloaded whole.
const MAX_REMOTE_TEXT_TRACK_BYTES: usize = 256 * 1024 * 1024;

/// Where downloaded remote text tracks are kept. `RGV_REMOTE_TRACK_CACHE_DIR` overrides the
/// default under the system temporary directory.
fn remote_text_track_directory() -> std::path::PathBuf {
    std::env::var_os("RGV_REMOTE_TRACK_CACHE_DIR").map_or_else(
        || std::env::temp_dir().join("rgv-remote-tracks"),
        std::path::PathBuf::from,
    )
}

fn rgv_core_file_name(location: &str) -> &str {
    let path = location.split(['?', '#']).next().unwrap_or(location);
    path.rsplit('/').next().unwrap_or(path)
}

impl TextFeatureSource {
    pub fn open(path: impl AsRef<Path>, format: FeatureFormat) -> Result<Self, TrackError> {
        Self::open_with_remote_config(path, format, &crate::HttpRangeConfig::default())
    }

    /// Opens a text annotation; a remote URL is read with `remote` (authentication, timeouts
    /// and retries) instead of the anonymous defaults. `remote` is unused for a file.
    ///
    /// A remote bgzip-compressed file (`.gz` or `.bgz`) with a tabix or CSI index beside it
    /// (`<url>.csi` or `<url>.tbi`) is read on demand over HTTP range requests, like a local
    /// indexed file: only the index and the blocks a query needs are transferred. A remote
    /// file without an index is downloaded whole (up to 256 MiB) and read like a local file.
    pub fn open_with_remote_config(
        path: impl AsRef<Path>,
        format: FeatureFormat,
        remote: &crate::HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        Self::open_with_remote_index_and_config(path, format, None, remote)
    }

    /// As [`Self::open_with_remote_config`], with an explicit tabix or CSI index URL for a
    /// remote file whose index is not at `<url>.csi` or `<url>.tbi`. With an explicit index
    /// the file is never downloaded whole: a missing or unreadable index is an error.
    pub fn open_with_remote_index_and_config(
        path: impl AsRef<Path>,
        format: FeatureFormat,
        remote_index_url: Option<&str>,
        remote: &crate::HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let location = path.to_string_lossy();
        if location.starts_with("http://") || location.starts_with("https://") {
            let name = rgv_core_file_name(&location);
            let block_compressed = Path::new(name).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("gz") || extension.eq_ignore_ascii_case("bgz")
            });
            if (block_compressed || remote_index_url.is_some())
                && let Some(indexed) =
                    super::remote_tabix::RemoteTabixText::open(&location, remote_index_url, remote)?
            {
                return Ok(Self {
                    features: HashMap::new(),
                    search_terms: BTreeMap::new(),
                    search_index_truncated: false,
                    indexed: Some(IndexedTextFeatures::remote(indexed, format)),
                    malformed_lines: 0,
                    first_malformed_line: None,
                });
            }
            // No index: a bounded copy is downloaded and read like a local file. The server is
            // asked first so an updated file is seen; the previous copy is used when the
            // server cannot be reached.
            let local = crate::igv_genome::local_copy(
                &location,
                name,
                &remote_text_track_directory(),
                remote,
                MAX_REMOTE_TEXT_TRACK_BYTES,
                crate::igv_genome::Freshness::PreferNetwork,
            )
            .map_err(|error| {
                TrackError::Io(std::io::Error::other(format!(
                    "could not download {name}: {error}"
                )))
            })?;
            return Self::open(local, format);
        }
        let compressed = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"));
        if compressed && adjacent_text_index(path).is_some() {
            return Ok(Self {
                features: HashMap::new(),
                search_terms: BTreeMap::new(),
                search_index_truncated: false,
                indexed: Some(IndexedTextFeatures::open(path, format)?),
                malformed_lines: 0,
                first_malformed_line: None,
            });
        }
        let file = File::open(path)?;
        if compressed {
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
        let mut malformed_lines = 0_usize;
        let mut first_malformed_line = None;
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
            if format == FeatureFormat::Gff3 && starts_gff3_fasta_section(&line) {
                // Everything after `##FASTA` is embedded sequence, not annotation.
                break;
            }
            let feature = match parse_feature_line(format, &line) {
                Ok(Some(feature)) => feature,
                Ok(None) => continue,
                Err(message) => {
                    // A bad line costs that one feature, not the track.
                    malformed_lines = malformed_lines.saturating_add(1);
                    first_malformed_line.get_or_insert((line_number, message));
                    continue;
                }
            };
            if source_records == MAX_FEATURE_SOURCE_RECORDS {
                return Err(TrackError::ResourceLimit(format!(
                    "feature source exceeds the {MAX_FEATURE_SOURCE_RECORDS}-record limit; \
                     compress it with bgzip and index it with tabix to open it on demand"
                )));
            }
            source_records = source_records.saturating_add(1);
            features
                .entry(feature.contig.clone())
                .or_default()
                .records
                .push(feature);
        }
        // Nothing parsed and something failed: this is the wrong format, not a blemished file.
        if source_records == 0
            && let Some((line, message)) = first_malformed_line
        {
            return Err(TrackError::MalformedLine { line, message });
        }
        for index in features.values_mut() {
            index.finish();
        }
        let (search_terms, search_index_truncated) = build_feature_search_index(&features);
        Ok(Self {
            features,
            search_terms,
            search_index_truncated,
            indexed: None,
            malformed_lines,
            first_malformed_line,
        })
    }

    /// Number of lines skipped because they could not be parsed. For an indexed source this
    /// counts lines met by the queries run so far.
    #[must_use]
    pub fn malformed_line_count(&self) -> usize {
        self.indexed
            .as_ref()
            .map_or(self.malformed_lines, |indexed| {
                indexed
                    .malformed_lines
                    .load(std::sync::atomic::Ordering::Relaxed)
            })
    }

    /// One-based line number and parse error of the first skipped line of a fully loaded
    /// source.
    #[must_use]
    pub fn first_malformed_line(&self) -> Option<(usize, &str)> {
        self.first_malformed_line
            .as_ref()
            .map(|(line, message)| (*line, message.as_str()))
    }

    pub fn query(
        &self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<FeatureRecord>, TrackError> {
        if let Some(indexed) = &self.indexed {
            return indexed.query(contig, start, end);
        }
        let Some(features) = self.features.get(contig) else {
            return Ok(Vec::new());
        };
        features.query(start, end)
    }

    /// Whether records are read on demand through a tabix or CSI index.
    #[must_use]
    pub const fn is_indexed(&self) -> bool {
        self.indexed.is_some()
    }

    fn remote_backend(&self) -> Option<&Mutex<super::remote_tabix::RemoteTabixText>> {
        match &self.indexed.as_ref()?.backend {
            IndexedTextBackend::Remote(remote) => Some(remote.as_ref()),
            IndexedTextBackend::Local { .. } => None,
        }
    }

    /// Installs (or clears) the check that abandons remote reads once the view has moved on.
    /// It has no effect on a local file or on a remote file that was downloaded whole.
    pub fn set_remote_cancellation(&mut self, cancellation: Option<crate::RemoteCancellation>) {
        if let Some(Ok(mut remote)) = self.remote_backend().map(Mutex::lock) {
            remote.set_cancellation(cancellation);
        }
    }

    /// Bounds the requests and bytes the following remote reads may spend, or lifts the bound.
    pub fn set_remote_transfer_budget(&mut self, budget: Option<crate::RemoteTransferBudget>) {
        if let Some(Ok(mut remote)) = self.remote_backend().map(Mutex::lock) {
            remote.set_transfer_budget(budget);
        }
    }

    /// Transfer statistics (index and data) of a remote indexed source; `None` for a local
    /// file and for a remote file that was downloaded whole.
    #[must_use]
    pub fn remote_io_stats(&self) -> Option<crate::RemoteIoStats> {
        let remote = self.remote_backend()?.lock().ok()?;
        Some(remote.stats())
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.features.keys().map(String::as_str).chain(
            self.indexed
                .iter()
                .flat_map(|indexed| indexed.contigs.iter().map(String::as_str)),
        )
    }

    #[must_use]
    pub fn adjacent_feature(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<FeatureRecord> {
        if let Some(indexed) = &self.indexed {
            return indexed.adjacent_feature(contig, position, forward);
        }
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
        if let Some(indexed) = &self.indexed {
            return indexed.search(&query, limit);
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

#[cfg(test)]
#[path = "text_feature_remote_tests.rs"]
mod remote_tests;
