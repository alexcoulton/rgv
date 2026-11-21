#[allow(clippy::wildcard_imports)]
use super::*;

pub struct BigWigSource {
    reader: BigWigBackend,
    remote: Option<RemoteReaderControl>,
}

enum BigWigBackend {
    Local(BigWigRead<ReopenableFile>),
    Remote(Box<BigWigRead<ControlledRangeReader>>),
}

impl BigWigSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::open_with_remote_config(path, HttpRangeConfig::default())
    }

    /// Opens a bigWig; a remote URL is read with `remote` (authentication, timeouts, retries
    /// and cache geometry) instead of the anonymous defaults. `remote` is unused for a file.
    pub fn open_with_remote_config(
        path: impl AsRef<Path>,
        remote: HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        if is_remote_url(&raw) {
            let (reader, control) = ControlledRangeReader::open(&raw, remote)
                .map_err(|error| TrackError::Backend(error.to_string()))?;
            Ok(Self {
                reader: BigWigBackend::Remote(Box::new(
                    BigWigRead::open(reader)
                        .map_err(|error| TrackError::Backend(error.to_string()))?,
                )),
                remote: Some(control),
            })
        } else {
            Ok(Self {
                reader: BigWigBackend::Local(
                    BigWigRead::open_file(path)
                        .map_err(|error| TrackError::Backend(error.to_string()))?,
                ),
                remote: None,
            })
        }
    }

    /// Installs (or clears) the check that abandons remote reads once the view has moved on.
    /// No effect on a local file.
    pub fn set_remote_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        if let Some(remote) = &self.remote {
            remote.set_cancellation(cancellation);
        }
    }

    /// Bounds the requests and bytes spent from now on; `None` lifts the bound.
    pub fn set_remote_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        if let Some(remote) = &self.remote {
            remote.set_transfer_budget(budget);
        }
    }

    /// Transfer statistics of a remote source; `None` for a local file.
    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        self.remote.as_ref().map(RemoteReaderControl::stats)
    }

    pub fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<QuantitativeQuery, TrackError> {
        match &mut self.reader {
            BigWigBackend::Local(reader) => query_bigwig(reader, contig, start, end, None),
            BigWigBackend::Remote(reader) => query_bigwig(reader, contig, start, end, None),
        }
    }

    /// As [`Self::query`], with the minimum and maximum of the values behind every record.
    ///
    /// The records are those `query` returns for the same arguments, in the same order
    /// (`mean` is its `value`), and the provenance is the same. When a precomputed zoom level
    /// is read, `minimum` and `maximum` come from the file's own zoom summaries, so a peak
    /// narrower than one summary bin stays visible in `maximum` although it barely moves the
    /// mean. A record of raw values has `minimum == mean == maximum`.
    pub fn query_with_extremes(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<QuantitativeSummaryQuery, TrackError> {
        let mut extremes = Vec::new();
        let query = match &mut self.reader {
            BigWigBackend::Local(reader) => {
                query_bigwig(reader, contig, start, end, Some(&mut extremes))
            }
            BigWigBackend::Remote(reader) => {
                query_bigwig(reader, contig, start, end, Some(&mut extremes))
            }
        }?;
        debug_assert_eq!(query.records.len(), extremes.len());
        Ok(QuantitativeSummaryQuery {
            records: query
                .records
                .iter()
                .zip(extremes)
                .map(|(record, (minimum, maximum))| QuantitativeSummaryRecord {
                    start: record.start,
                    end: record.end,
                    minimum,
                    mean: record.value,
                    maximum,
                })
                .collect(),
            provenance: query.provenance,
        })
    }
}

/// One quantitative record with the range of the values it stands for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QuantitativeSummaryRecord {
    pub start: u64,
    pub end: u64,
    /// Smallest value over the covered bases.
    pub minimum: f32,
    /// Mean over the covered bases; the `value` of the matching [`QuantitativeRecord`].
    pub mean: f32,
    /// Largest value over the covered bases.
    pub maximum: f32,
}

/// Result of [`BigWigSource::query_with_extremes`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuantitativeSummaryQuery {
    pub records: Vec<QuantitativeSummaryRecord>,
    pub provenance: QuantitativeQueryProvenance,
}

pub struct BigBedSource {
    reader: BigBedBackend,
    format: BigBedFeatureFormat,
    remote: Option<RemoteReaderControl>,
}

enum BigBedBackend {
    Local(BigBedRead<ReopenableFile>),
    Remote(Box<BigBedRead<ControlledRangeReader>>),
}

impl BigBedSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::open_with_format(path, BigBedFeatureFormat::Bed)
    }

    pub fn open_with_format(
        path: impl AsRef<Path>,
        format: BigBedFeatureFormat,
    ) -> Result<Self, TrackError> {
        Self::open_with_format_and_remote_config(path, format, HttpRangeConfig::default())
    }

    /// Opens a bigBed; a remote URL is read with `remote` (authentication, timeouts, retries
    /// and cache geometry) instead of the anonymous defaults. `remote` is unused for a file.
    pub fn open_with_format_and_remote_config(
        path: impl AsRef<Path>,
        format: BigBedFeatureFormat,
        remote: HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        let (reader, remote) = if is_remote_url(&raw) {
            let (reader, control) = ControlledRangeReader::open(&raw, remote)
                .map_err(|error| TrackError::Backend(error.to_string()))?;
            (
                BigBedBackend::Remote(Box::new(
                    BigBedRead::open(reader)
                        .map_err(|error| TrackError::Backend(error.to_string()))?,
                )),
                Some(control),
            )
        } else {
            (
                BigBedBackend::Local(
                    BigBedRead::open_file(path)
                        .map_err(|error| TrackError::Backend(error.to_string()))?,
                ),
                None,
            )
        };
        match &reader {
            BigBedBackend::Local(reader) => validate_bigbed_schema(reader, format)?,
            BigBedBackend::Remote(reader) => validate_bigbed_schema(reader, format)?,
        }
        Ok(Self {
            reader,
            format,
            remote,
        })
    }

    /// Installs (or clears) the check that abandons remote reads once the view has moved on.
    /// No effect on a local file.
    pub fn set_remote_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        if let Some(remote) = &self.remote {
            remote.set_cancellation(cancellation);
        }
    }

    /// Bounds the requests and bytes spent from now on; `None` lifts the bound.
    pub fn set_remote_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        if let Some(remote) = &self.remote {
            remote.set_transfer_budget(budget);
        }
    }

    /// Transfer statistics of a remote source; `None` for a local file.
    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        self.remote.as_ref().map(RemoteReaderControl::stats)
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
    let (expected, extended) = match format {
        BigBedFeatureFormat::Bed => return Ok(()),
        BigBedFeatureFormat::BigGenePred => (20, true),
        BigBedFeatureFormat::BigNarrowPeak => (10, false),
    };
    let actual = reader.info().header.field_count;
    if (extended && actual < expected) || (!extended && actual != expected) {
        let qualifier = if extended { "at least" } else { "exactly" };
        return Err(TrackError::InvalidSchema(format!(
            "{} requires {qualifier} {expected} BigBed fields, but the file declares {actual}",
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

/// Minimum summary bins across one query for a precomputed zoom level to be used. Zoom levels
/// are typically spaced by a factor of four, so the level chosen yields between this many and
/// four times this many bins: at least one bin per pixel on a 4K-wide canvas.
const BIGWIG_ZOOM_TARGET_BINS: u64 = 4_096;

/// The coarsest precomputed zoom level that still yields at least the target number of bins
/// across `span` bases. `None` means the raw values should be read.
fn bigwig_zoom_reduction(reductions: &[u32], span: u64) -> Option<u32> {
    reductions
        .iter()
        .copied()
        .filter(|reduction| {
            *reduction > 0 && span / u64::from(*reduction) >= BIGWIG_ZOOM_TARGET_BINS
        })
        .max()
}

/// `extremes`, when given, receives the `(minimum, maximum)` behind each returned record.
fn query_bigwig<R: BBIFileRead>(
    reader: &mut BigWigRead<R>,
    contig: &str,
    start: u64,
    end: u64,
    mut extremes: Option<&mut Vec<(f32, f32)>>,
) -> Result<QuantitativeQuery, TrackError> {
    let start = u32::try_from(start).map_err(|_| TrackError::Coordinate)?;
    let end = u32::try_from(end).map_err(|_| TrackError::Coordinate)?;
    let reductions = reader
        .info()
        .zoom_headers
        .iter()
        .map(|header| header.reduction_level)
        .collect::<Vec<_>>();
    let span = u64::from(end.saturating_sub(start));
    if let Some(reduction) = bigwig_zoom_reduction(&reductions, span) {
        return query_bigwig_zoom(reader, contig, start, end, reduction, extremes);
    }
    let raw = query_bigwig_raw(reader, contig, start, end)?;
    if let Some(extremes) = extremes.as_deref_mut() {
        extremes.extend(
            raw.records
                .iter()
                .map(|record| (record.value, record.value)),
        );
    }
    if !raw.provenance.records_truncated {
        return Ok(raw);
    }
    // Dense raw data exceeded the record cap. A truncated signal would be blank on the right of
    // the view, so prefer the finest zoom level that covers the whole query.
    let Some(finest) = reductions.iter().copied().filter(|level| *level > 0).min() else {
        return Ok(raw);
    };
    let mut summarized_extremes = Vec::new();
    let summarized = query_bigwig_zoom(
        reader,
        contig,
        start,
        end,
        finest,
        extremes.is_some().then_some(&mut summarized_extremes),
    )?;
    Ok(if summarized.provenance.records_truncated {
        raw
    } else {
        if let Some(extremes) = extremes {
            *extremes = summarized_extremes;
        }
        summarized
    })
}

fn query_bigwig_raw<R: BBIFileRead>(
    reader: &mut BigWigRead<R>,
    contig: &str,
    start: u32,
    end: u32,
) -> Result<QuantitativeQuery, TrackError> {
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

/// Reads one precomputed zoom level. Each summary becomes one record carrying the mean of the
/// covered bases, which is what the file's own reductions are designed to display; the
/// summary's minimum and maximum go to `extremes` when it is given.
fn query_bigwig_zoom<R: BBIFileRead>(
    reader: &mut BigWigRead<R>,
    contig: &str,
    start: u32,
    end: u32,
    reduction: u32,
    mut extremes: Option<&mut Vec<(f32, f32)>>,
) -> Result<QuantitativeQuery, TrackError> {
    let mut query = QuantitativeQuery::default();
    query.provenance.summary_reduction_bases = Some(reduction);
    let summaries = reader
        .get_zoom_interval(contig, start, end, reduction)
        .map_err(|error| TrackError::Backend(error.to_string()))?;
    for result in summaries {
        let summary = result.map_err(|error| TrackError::Backend(error.to_string()))?;
        if summary.summary.bases_covered == 0 {
            continue;
        }
        #[allow(clippy::cast_possible_truncation)]
        let mean = (summary.summary.sum / summary.summary.bases_covered as f64) as f32;
        if !retain_quantitative_record(
            &mut query,
            QuantitativeRecord {
                start: u64::from(summary.start),
                end: u64::from(summary.end),
                value: mean,
            },
            MAX_QUANTITATIVE_RECORDS_PER_QUERY,
        ) {
            break;
        }
        if let Some(extremes) = extremes.as_deref_mut() {
            #[allow(clippy::cast_possible_truncation)]
            extremes.push((
                summary.summary.min_val as f32,
                summary.summary.max_val as f32,
            ));
        }
    }
    query.provenance.records_retained = query.records.len();
    Ok(query)
}

pub(super) fn retain_quantitative_record(
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

pub(super) fn ensure_bigbed_query_capacity(current_records: usize) -> Result<(), TrackError> {
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

pub(super) fn parse_bigbed_record(
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
pub(super) fn parse_big_gene_pred_record(
    contig: &str,
    start: u64,
    end: u64,
    rest: &str,
) -> Result<FeatureRecord, String> {
    let fields = rest.split('\t').collect::<Vec<_>>();
    if fields.len() < 17 {
        return Err(format!(
            "bigGenePred requires at least 17 non-coordinate fields, found {}",
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
    let item_rgb = fields[5];
    let numeric_item_rgb = item_rgb.parse::<u32>().is_ok();
    let channel_item_rgb = {
        let channels = item_rgb.split(',').collect::<Vec<_>>();
        channels.len() == 3 && channels.iter().all(|channel| channel.parse::<u8>().is_ok())
    };
    if !numeric_item_rgb && !channel_item_rgb {
        return Err(format!(
            "invalid bigGenePred reserved/itemRgb value: {item_rgb}"
        ));
    }
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
    // bigGenePred's geneName is commonly a stable database identifier, while
    // geneName2 is the human-readable gene symbol intended for display.
    let display_name = [gene_name2, gene_name, name2, transcript_name]
        .into_iter()
        .find(|value| !value.is_empty() && *value != ".")
        .unwrap_or(transcript_name);
    let mut attributes = vec![
        ("transcript_id".to_owned(), transcript_name.to_owned()),
        ("source_format".to_owned(), "bigGenePred".to_owned()),
        ("item_rgb".to_owned(), item_rgb.to_owned()),
        ("cds_start_status".to_owned(), fields[10].to_owned()),
        ("cds_end_status".to_owned(), fields[11].to_owned()),
        ("exon_frames".to_owned(), fields[12].to_owned()),
    ];
    if !gene_name.is_empty() && gene_name != "." {
        attributes.push(("gene_id".to_owned(), gene_name.to_owned()));
    }
    if !display_name.is_empty() && display_name != "." {
        attributes.push(("gene_name".to_owned(), display_name.to_owned()));
    }
    for (key, value) in [
        ("name2", name2),
        ("transcript_type", transcript_type),
        ("big_gene_pred_gene_name", gene_name),
        ("gene_name2", gene_name2),
        ("gene_type", gene_type),
    ] {
        if !value.is_empty() && value != "." {
            attributes.push((key.to_owned(), value.to_owned()));
        }
    }
    attributes.extend(
        fields[17..]
            .iter()
            .enumerate()
            .filter(|(_, value)| !value.is_empty() && **value != ".")
            .map(|(index, value)| {
                (
                    format!("extension_field_{}", index + 1),
                    (*value).to_owned(),
                )
            }),
    );
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
