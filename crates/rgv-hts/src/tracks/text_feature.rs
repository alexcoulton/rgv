use super::feature_search::{
    FeatureSearchIndex, Retarget, StreamingFeatureSearch, container_search_record,
};
use super::gene_model::{Assembly, Placement};
#[allow(clippy::wildcard_imports)]
use super::*;

pub struct TextFeatureSource {
    pub(super) features: HashMap<String, FeatureIndex>,
    /// Name index of a fully loaded source; empty for an indexed one.
    pub(super) search: FeatureSearchIndex,
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
    search: OnceLock<StreamingFeatureSearch>,
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

/// Initial distance searched backwards for the previous feature; quadrupled until one is found.
const INDEXED_PREVIOUS_FEATURE_WINDOW: u64 = 16_384;

/// Furthest a windowed read of a GTF or GFF3 file is widened beyond the window to reach the
/// ends of the genes and transcripts it overlaps. The longest human genes span about 2.5 Mb.
const MAX_GENE_MODEL_READ_REACH: u64 = 4_000_000;

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
        // It also unwraps the UTF-8 conversion of every leading comment line and of every
        // sequence name in the index, so both calls are contained here: a file with a
        // non-UTF-8 comment is a clean error for this track, whoever the caller is.
        let not_text = |_| {
            TrackError::Backend(format!(
                "{} has a comment line or an indexed sequence name that is not valid UTF-8",
                path.display()
            ))
        };
        let reader =
            crate::htslib_guard::contain_panic(|| rust_htslib::tbx::Reader::from_path(path))
                .map_err(not_text)?
                .map_err(|error| TrackError::Backend(error.to_string()))?;
        let contigs = crate::htslib_guard::contain_panic(|| reader.seqnames()).map_err(not_text)?;
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
                // `tbx::Reader::read` unwraps the UTF-8 conversion of the line it has just
                // read; the reader itself stays usable for the next query.
                while crate::htslib_guard::contain_panic(|| reader.read(&mut line))
                    .map_err(|_| {
                        TrackError::Backend("indexed feature line is not UTF-8".to_owned())
                    })?
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

    /// Every line overlapping `[start, end)`.
    fn read(&self, contig: &str, start: u64, end: u64) -> Result<Vec<FeatureRecord>, TrackError> {
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
        Ok(records)
    }

    /// Records overlapping `[start, end)`. GTF and GFF3 lines are assembled into transcript
    /// models: the read is first widened to the full extent of every gene and transcript line
    /// it meets (up to [`MAX_GENE_MODEL_READ_REACH`] beyond the window), so a model has all its
    /// exons, the same span and the same identity in every window.
    fn query(&self, contig: &str, start: u64, end: u64) -> Result<Vec<FeatureRecord>, TrackError> {
        self.query_within_reach(contig, start, end, MAX_GENE_MODEL_READ_REACH)
    }

    /// [`Self::query`], widening a GTF or GFF3 read by at most `reach` bases on either side.
    /// A model reaching further keeps its transcript line's span and identity, but holds only
    /// the exons the read met.
    fn query_within_reach(
        &self,
        contig: &str,
        start: u64,
        end: u64,
        reach: u64,
    ) -> Result<Vec<FeatureRecord>, TrackError> {
        let mut records = self.read(contig, start, end)?;
        let (mut low, mut high) = (start, end);
        if let Some(gtf) = gene_model_dialect(self.format) {
            let (wider_low, wider_high) =
                super::gene_model::assembly_read_extent(&records, start, end, reach, gtf);
            if wider_low < start || wider_high > end {
                match self.read(contig, wider_low, wider_high) {
                    Ok(wider) => (records, low, high) = (wider, wider_low, wider_high),
                    // The window alone stays within the limit: its models may be partial.
                    Err(TrackError::ResourceLimit(_)) => {}
                    Err(error) => return Err(error),
                }
            }
            records = super::gene_model::assemble(records, gtf).records;
        }
        if is_gene_model_format(self.format) {
            // Every transcript of a gene in the window is read before its representative is
            // chosen, so the choice is the same in every window. A transcript beyond all the
            // others of its gene is met by reading again, twice at most.
            for _ in 0..2 {
                let (wider_low, wider_high) =
                    transcript_group_read_extent(&records, start, end, reach);
                if wider_low >= low && wider_high <= high {
                    break;
                }
                let (wider_low, wider_high) = (wider_low.min(low), wider_high.max(high));
                match self.read(contig, wider_low, wider_high) {
                    Ok(wider) => {
                        records = match gene_model_dialect(self.format) {
                            Some(gtf) => super::gene_model::assemble(wider, gtf).records,
                            None => wider,
                        };
                        (low, high) = (wider_low, wider_high);
                    }
                    Err(TrackError::ResourceLimit(_)) => break,
                    Err(error) => return Err(error),
                }
            }
            annotate_transcript_choices(&mut records);
            records.retain(|record| record.start < end && record.end > start);
        }
        records.sort_by_key(|record| (record.start, record.end));
        Ok(records)
    }

    fn adjacent_feature(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<FeatureRecord> {
        if gene_model_dialect(self.format).is_some() {
            return self.adjacent_gene_model(contig, position, forward);
        }
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

    /// [`Self::adjacent_feature`] over transcript models: windows of growing size are
    /// assembled until one holds a model starting at or after `position` (forward) or ending
    /// at or before it (backward). The nearest such model in a window is the nearest overall.
    fn adjacent_gene_model(
        &self,
        contig: &str,
        position: u64,
        forward: bool,
    ) -> Option<FeatureRecord> {
        let limit = u64::from(u32::MAX);
        let mut window = INDEXED_PREVIOUS_FEATURE_WINDOW;
        loop {
            let (start, end) = if forward {
                (position, position.saturating_add(window).min(limit))
            } else {
                (position.saturating_sub(window), position)
            };
            if start >= end {
                return None;
            }
            let records = self.query(contig, start, end).ok()?;
            let found = if forward {
                // Start-sorted, shortest first among equal starts.
                records.into_iter().find(|record| record.start >= position)
            } else {
                records
                    .into_iter()
                    .filter(|record| record.end <= position)
                    .max_by_key(|record| record.start)
            };
            if found.is_some() || (forward && end == limit) || (!forward && start == 0) {
                return found;
            }
            window = window.saturating_mul(4);
        }
    }

    /// Streams the whole file once, retaining one record per search term and contig (see
    /// [`StreamingFeatureSearch`]).
    /// A remote file is streamed through the range reader until
    /// [`MAX_REMOTE_FEATURE_SEARCH_SCAN_BYTES`] have been transferred (or its transfer budget
    /// or cancellation stops it); the index is then reported as truncated.
    fn build_search(&self) -> StreamingFeatureSearch {
        let mut search = StreamingFeatureSearch::default();
        // Whether to go on to the next line.
        let add = |search: &mut StreamingFeatureSearch, line: &str| -> bool {
            let Ok(Some(record)) = parse_feature_line(self.format, line) else {
                return true;
            };
            search.add(&record, MAX_FEATURE_SEARCH_ASSOCIATIONS)
        };
        match &self.backend {
            IndexedTextBackend::Local { path, .. } => {
                let Ok(file) = File::open(path) else {
                    search.truncate();
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
                            search.truncate();
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
                    search.truncate();
                    return search;
                };
                let Ok(baseline) = remote.start_scan() else {
                    search.truncate();
                    return search;
                };
                let mut line = Vec::new();
                loop {
                    if remote.response_bytes().saturating_sub(baseline)
                        > MAX_REMOTE_FEATURE_SEARCH_SCAN_BYTES
                    {
                        search.truncate();
                        break;
                    }
                    match remote.next_scan_line(&mut line) {
                        Ok(true) if line.len() <= MAX_FEATURE_LINE_BYTES => {}
                        Ok(false) => break,
                        _ => {
                            search.truncate();
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
        self.search
            .get_or_init(|| self.build_search())
            .search(query, limit)
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

/// `Some(true)` for GTF and `Some(false)` for GFF3, whose lines are assembled into transcript
/// models (see [`super::gene_model`]); `None` for formats whose lines are whole features.
pub(super) const fn gene_model_dialect(format: FeatureFormat) -> Option<bool> {
    match format {
        FeatureFormat::Gtf => Some(true),
        FeatureFormat::Gff3 => Some(false),
        _ => None,
    }
}

/// Whether a format's records are transcript models, of which one per gene can be drawn: they
/// carry a representative-transcript choice (see [`annotate_transcript_choices`]).
pub(super) const fn is_gene_model_format(format: FeatureFormat) -> bool {
    matches!(
        format,
        FeatureFormat::Gtf | FeatureFormat::Gff3 | FeatureFormat::GenePred | FeatureFormat::RefFlat
    )
}

/// Replaces the lines of a fully loaded GTF or GFF3 source by its transcript models, and builds
/// the name index.
///
/// The index is built over the lines, exactly as for any other source, so gene names still
/// find gene spans and the association count does not change; its targets are then re-pointed
/// at what each line became: a transcript line at its model, a gene line whose transcripts are
/// drawn at a copy of its span kept by the index.
fn assemble_loaded_gene_models(
    features: &mut HashMap<String, FeatureIndex>,
    gtf: bool,
    search_cap: usize,
) -> FeatureSearchIndex {
    struct Assembled {
        placements: Vec<Placement>,
        /// Sorted position of each assembled record.
        positions: Vec<usize>,
        hidden: Vec<Option<FeatureRecord>>,
        hidden_spans: Vec<Option<usize>>,
    }
    let mut search = FeatureSearchIndex::build(features, search_cap);
    let mut assembled = HashMap::with_capacity(features.len());
    for (contig, index) in features.iter_mut() {
        let lines = std::mem::take(&mut index.records);
        let Assembly {
            records,
            hidden,
            placements,
        } = super::gene_model::assemble(lines, gtf);
        let mut order = (0..records.len()).collect::<Vec<_>>();
        order.sort_by_key(|&record| (records[record].start, records[record].end));
        let mut positions = vec![0; records.len()];
        for (position, &record) in order.iter().enumerate() {
            positions[record] = position;
        }
        let mut slots = records.into_iter().map(Some).collect::<Vec<_>>();
        index.records = order
            .into_iter()
            .filter_map(|record| slots[record].take())
            .collect();
        index.finish();
        let hidden_spans = vec![None; hidden.len()];
        assembled.insert(
            contig.clone(),
            Assembled {
                placements,
                positions,
                hidden: hidden.into_iter().map(Some).collect(),
                hidden_spans,
            },
        );
    }
    search.retarget_records(|contig, line, spans| {
        let assembled = assembled.get_mut(contig)?;
        match *assembled.placements.get(line)? {
            Placement::Drawn(record) => Some(Retarget::Record(assembled.positions[record])),
            Placement::Hidden(container) => {
                // Several terms of one gene share one copy.
                if let Some(span) = assembled.hidden_spans[container] {
                    return Some(Retarget::Span(span));
                }
                let record = assembled.hidden[container].take()?;
                spans.push(container_search_record(record));
                assembled.hidden_spans[container] = Some(spans.len() - 1);
                Some(Retarget::Span(spans.len() - 1))
            }
            // Parts are found through spans of their own, never as records.
            Placement::Absorbed => None,
        }
    });
    search
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
                    search: FeatureSearchIndex::default(),
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
                search: FeatureSearchIndex::default(),
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

    pub fn from_reader(reader: impl BufRead, format: FeatureFormat) -> Result<Self, TrackError> {
        Self::from_reader_with_search_cap(reader, format, MAX_FEATURE_SEARCH_ASSOCIATIONS)
    }

    /// As [`Self::from_reader`], with the name index bounded at `search_cap` associations.
    pub(super) fn from_reader_with_search_cap(
        mut reader: impl BufRead,
        format: FeatureFormat,
        search_cap: usize,
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
        let search = if let Some(gtf) = gene_model_dialect(format) {
            assemble_loaded_gene_models(&mut features, gtf, search_cap)
        } else {
            for index in features.values_mut() {
                index.finish();
            }
            FeatureSearchIndex::build(&features, search_cap)
        };
        if is_gene_model_format(format) {
            // A whole contig is at hand: every gene's representative is chosen from all its
            // transcripts. Records keep their positions, so the name index stays valid.
            for index in features.values_mut() {
                annotate_transcript_choices(&mut index.records);
            }
        }
        Ok(Self {
            features,
            search,
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
                index_truncated: self
                    .search_index_coverage()
                    .is_some_and(|coverage| coverage.truncated),
                ..FeatureSearchResults::default()
            };
        }
        if let Some(indexed) = &self.indexed {
            return indexed.search(&query, limit);
        }
        self.search.search(&self.features, &query, limit)
    }

    /// How much of the source the name index covers. `None` for an indexed source whose
    /// index has not been built yet (it is built by the first name search).
    #[must_use]
    pub fn search_index_coverage(&self) -> Option<FeatureSearchIndexCoverage> {
        match &self.indexed {
            Some(indexed) => indexed.search.get().map(|search| search.coverage().clone()),
            None => Some(self.search.coverage().clone()),
        }
    }
}

#[cfg(test)]
#[path = "text_feature_remote_tests.rs"]
mod remote_tests;

#[cfg(test)]
#[path = "text_feature_search_tests.rs"]
mod search_tests;
