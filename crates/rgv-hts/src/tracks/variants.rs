#[allow(clippy::wildcard_imports)]
use super::*;

mod filtering;
mod remote;
#[cfg(test)]
mod remote_tests;

pub use remote::MAX_REMOTE_VARIANT_ID_SCAN_BYTES;
pub(super) use remote::{
    MAX_REMOTE_TEXT_INDEX_BYTES, RemoteVariantIndex as RemoteTabixIndex, TABIX_FLAG_ZERO_BASED,
    TabixColumns, combine_stats as combine_remote_stats, parse_text_index,
    read_line as read_bgzf_line, strtoll,
};
pub(crate) use remote::{fuzz_variant_index, fuzz_variant_line_interval};

/// Indexed VCF/BCF source. A local file is read by `HTSlib`; a remote bgzipped VCF or BCF is
/// read over [`crate::HttpRangeReader`] (see [`Self::open_with_remote_config`]) and only
/// decoded by `HTSlib`.
pub struct LocalVariantSource {
    path: PathBuf,
    backend: VariantBackend,
    header: VariantHeader,
    omitted_info_schemas: usize,
    omitted_format_schemas: usize,
    total_samples: usize,
    omitted_header_samples: usize,
    id_index: Option<VariantIdIndex>,
}

enum VariantBackend {
    Htslib(bcf::IndexedReader),
    Remote(Box<remote::RemoteVariantFile>),
}

#[derive(Default)]
struct VariantIdIndex {
    ids: BTreeMap<String, Vec<VariantIdSearchHit>>,
    truncated: bool,
}

impl LocalVariantSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::open_with_remote_config(path, HttpRangeConfig::default())
    }

    /// Opens an indexed VCF/BCF. An `http(s)` URL (bgzipped VCF with an adjacent `.csi` or
    /// `.tbi`, or BCF with an adjacent `.csi`) is read with `remote` (authentication, timeouts,
    /// retries and cache geometry) through RGV's range reader; nothing is written outside the
    /// system temporary directory. `remote` is unused for a local file.
    pub fn open_with_remote_config(
        path: impl AsRef<Path>,
        remote: HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        Self::open_with_remote_index_and_config(path, None, remote)
    }

    /// As [`Self::open_with_remote_config`], with an explicit index URL for a remote file
    /// (needed when the index is signed separately or not adjacent to the data).
    pub fn open_with_remote_index_and_config(
        path: impl AsRef<Path>,
        remote_index_url: Option<&str>,
        remote: HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        if let Some(scheme) = crate::local::unsupported_url_scheme(&raw) {
            // Refused by name: `HTSlib` would hand `s3://`, `gs://` or `ftp://` to libcurl.
            return Err(TrackError::Backend(format!(
                "unsupported URL scheme {scheme}: a variant file is opened from a local path \
                 or an http(s) URL"
            )));
        }
        let backend = if is_remote_url(&raw) {
            VariantBackend::Remote(Box::new(remote::RemoteVariantFile::open(
                &raw,
                remote_index_url,
                remote,
            )?))
        } else {
            VariantBackend::Htslib(
                bcf::IndexedReader::from_path(path)
                    .map_err(|error| TrackError::Backend(error.to_string()))?,
            )
        };
        let (header, omitted_info_schemas, omitted_format_schemas, omitted_header_samples) =
            decode_variant_header(match &backend {
                VariantBackend::Htslib(reader) => reader.header(),
                VariantBackend::Remote(remote) => remote.raw_header(),
            });
        let total_samples = header.samples.len().saturating_add(omitted_header_samples);
        Ok(Self {
            path: path.to_path_buf(),
            backend,
            header,
            omitted_info_schemas,
            omitted_format_schemas,
            total_samples,
            omitted_header_samples,
            id_index: None,
        })
    }

    /// Installs (or clears) the check that abandons remote reads once the view has moved on.
    /// No effect on a local file.
    pub fn set_remote_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        if let VariantBackend::Remote(remote) = &mut self.backend {
            remote.set_cancellation(cancellation);
        }
    }

    /// Bounds the requests and bytes spent from now on; `None` lifts the bound. No effect on a
    /// local file.
    pub fn set_remote_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        if let VariantBackend::Remote(remote) = &mut self.backend {
            remote.set_transfer_budget(budget);
        }
    }

    /// Transfer statistics (index and data) of a remote source; `None` for a local file.
    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        match &self.backend {
            VariantBackend::Htslib(_) => None,
            VariantBackend::Remote(remote) => Some(remote.stats()),
        }
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
        let mut collector = VariantQueryCollector {
            header: &self.header,
            contig,
            settings,
            maximum_records,
            selected_sample_indices: selected_sample_indices(&self.header, settings),
            output: Vec::new(),
            records_truncated: false,
            remaining_genotype_cells: MAX_VARIANT_GENOTYPE_CELLS_PER_QUERY,
            genotype_cells_loaded: 0,
            genotype_cells_omitted: 0,
            records_examined: 0,
            records_filtered: 0,
        };
        match &mut self.backend {
            VariantBackend::Htslib(reader) => {
                let rid = reader
                    .header()
                    .name2rid(contig.as_bytes())
                    .map_err(|error| TrackError::Backend(error.to_string()))?;
                reader
                    .fetch(rid, start, Some(end.saturating_sub(1)))
                    .map_err(|error| TrackError::Backend(error.to_string()))?;
                filtering::validate_filter_authority(
                    reader.header(),
                    &self.header,
                    self.omitted_header_samples,
                    settings,
                )?;
                for result in reader.records() {
                    let record = result.map_err(|error| TrackError::Backend(error.to_string()))?;
                    if !collector.push(&record)? {
                        break;
                    }
                }
            }
            VariantBackend::Remote(remote) => {
                let mut region = remote.region(contig, start, end)?;
                filtering::validate_filter_authority(
                    remote.raw_header(),
                    &self.header,
                    self.omitted_header_samples,
                    settings,
                )?;
                'batches: loop {
                    let lines = remote
                        .next_region_batch(&mut region, remote::REMOTE_VARIANT_BATCH_BYTES)?;
                    if lines.is_empty() {
                        break;
                    }
                    let mut batch = remote.parse(&lines)?;
                    for result in batch.reader.records() {
                        let record =
                            result.map_err(|error| TrackError::Backend(error.to_string()))?;
                        if !collector.push(&record)? {
                            break 'batches;
                        }
                    }
                }
            }
        }
        let VariantQueryCollector {
            selected_sample_indices,
            output,
            records_truncated,
            genotype_cells_loaded,
            genotype_cells_omitted,
            records_examined,
            records_filtered,
            ..
        } = collector;
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

    /// Prefix search over record IDs. The first call builds an in-memory ID index by reading
    /// the file once, bounded by [`MAX_VARIANT_ID_INDEX_RECORDS`] and
    /// [`MAX_VARIANT_ID_ASSOCIATIONS`]. For a remote file that read is additionally
    /// bounded by [`MAX_REMOTE_VARIANT_ID_SCAN_BYTES`] of transfer (reported through
    /// `index_truncated`), honours the installed cancellation and transfer budget, and is
    /// retried by the next call if it fails.
    pub fn search_ids(
        &mut self,
        query: &str,
        limit: usize,
    ) -> Result<VariantIdSearchResults, TrackError> {
        if self.id_index.is_none() {
            self.id_index = Some(match &mut self.backend {
                VariantBackend::Htslib(_) => build_variant_id_index(&self.path)?,
                VariantBackend::Remote(remote) => build_remote_variant_id_index(remote)?,
            });
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

    #[cfg(test)]
    pub(super) fn set_remote_id_scan_byte_limit(&mut self, limit: u64) {
        if let VariantBackend::Remote(remote) = &mut self.backend {
            remote.set_id_scan_byte_limit(limit);
        }
    }
}

/// Accumulates one region query record by record; shared by every backend so the record
/// limit, filters and genotype-cell budget behave identically.
struct VariantQueryCollector<'a> {
    header: &'a VariantHeader,
    contig: &'a str,
    settings: &'a VariantTrackSettings,
    maximum_records: usize,
    selected_sample_indices: Vec<usize>,
    output: Vec<VariantRecord>,
    records_truncated: bool,
    remaining_genotype_cells: usize,
    genotype_cells_loaded: usize,
    genotype_cells_omitted: usize,
    records_examined: usize,
    records_filtered: usize,
}

impl VariantQueryCollector<'_> {
    /// Returns `false` once the record limit is reached and reading should stop.
    fn push(&mut self, record: &bcf::Record) -> Result<bool, TrackError> {
        self.records_examined = self.records_examined.saturating_add(1);
        let (mut decoded, _, _) = decode_variant_record(
            record,
            self.header,
            self.contig,
            0,
            &self.selected_sample_indices,
        )?;
        if !filtering::source_record_matches(
            record,
            &decoded,
            self.header,
            self.settings,
            &self.selected_sample_indices,
        )? {
            self.records_filtered = self.records_filtered.saturating_add(1);
            return Ok(true);
        }
        if self.output.len() >= self.maximum_records {
            self.records_truncated = true;
            return Ok(false);
        }
        let (genotypes, loaded, omitted) = decode_genotypes(
            record,
            self.header,
            self.remaining_genotype_cells,
            &self.selected_sample_indices,
        );
        decoded.genotypes = genotypes;
        self.remaining_genotype_cells = self.remaining_genotype_cells.saturating_sub(loaded);
        self.genotype_cells_loaded = self.genotype_cells_loaded.saturating_add(loaded);
        self.genotype_cells_omitted = self.genotype_cells_omitted.saturating_add(omitted);
        self.output.push(decoded);
        Ok(true)
    }
}

fn build_variant_id_index(path: &Path) -> Result<VariantIdIndex, TrackError> {
    let mut reader =
        bcf::Reader::from_path(path).map_err(|error| TrackError::Backend(error.to_string()))?;
    let mut builder = VariantIdIndexBuilder::default();
    builder.extend(&mut reader)?;
    Ok(builder.index)
}

/// Builds the ID index of a remote VCF/BCF from a sequential range-reader scan. The scan
/// stops, marking the index truncated, at the record/association limits or after
/// [`MAX_REMOTE_VARIANT_ID_SCAN_BYTES`] of transfer; cancellation and an exhausted caller
/// budget surface as errors and leave no partial index behind.
fn build_remote_variant_id_index(
    remote: &mut remote::RemoteVariantFile,
) -> Result<VariantIdIndex, TrackError> {
    let mut builder = VariantIdIndexBuilder::default();
    let mut scan = remote.scan()?;
    while !builder.index.truncated {
        // One line beyond the record limit lets the builder observe that more records exist.
        let lines = remote.next_scan_batch(
            &mut scan,
            remote::REMOTE_VARIANT_BATCH_BYTES,
            MAX_VARIANT_ID_INDEX_RECORDS
                .saturating_sub(builder.records)
                .saturating_add(1),
        )?;
        if lines.is_empty() {
            break;
        }
        let mut batch = remote.parse(&lines)?;
        builder.extend(&mut batch.reader)?;
    }
    builder.index.truncated |= scan.budget_exhausted;
    Ok(builder.index)
}

#[derive(Default)]
struct VariantIdIndexBuilder {
    index: VariantIdIndex,
    records: usize,
    associations: usize,
}

impl VariantIdIndexBuilder {
    /// Indexes every record `reader` yields until a limit truncates the index.
    fn extend(&mut self, reader: &mut bcf::Reader) -> Result<(), TrackError> {
        let header = reader.header().clone();
        for result in reader.records() {
            if self.records == MAX_VARIANT_ID_INDEX_RECORDS
                || self.associations == MAX_VARIANT_ID_ASSOCIATIONS
            {
                self.index.truncated = true;
                break;
            }
            self.records = self.records.saturating_add(1);
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
            record_ids
                .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
            record_ids.dedup_by(|left, right| left.0 == right.0);
            for (normalized_id, id) in record_ids {
                if self.associations == MAX_VARIANT_ID_ASSOCIATIONS {
                    self.index.truncated = true;
                    break;
                }
                self.index
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
                self.associations = self.associations.saturating_add(1);
            }
        }
        Ok(())
    }
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
        &[
            "END", "SVTYPE", "SVLEN", "STRANDS", "CIPOS", "CIEND", "AF", "AC", "AN",
        ],
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
    let breakpoint_orientations = parse_breakpoint_orientations(&info);
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
            breakpoint_orientations,
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
                    let truncated = values.len() > MAX_VARIANT_VALUES_PER_FIELD
                        || values
                            .iter()
                            .any(|value| value.len() > MAX_VARIANT_STRING_BYTES);
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

pub(super) fn selected_sample_indices(
    header: &VariantHeader,
    settings: &VariantTrackSettings,
) -> Vec<usize> {
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

pub(super) fn parse_breakpoint_orientations(fields: &[VariantField]) -> Vec<BreakpointOrientation> {
    const MAX_ORIENTATIONS: usize = 16;
    let Some(VariantValue::Strings(values)) = fields
        .iter()
        .find(|field| field.key == "STRANDS")
        .map(|field| &field.value)
    else {
        return Vec::new();
    };
    values
        .iter()
        .flat_map(|value| value.split(','))
        .filter_map(|value| {
            let (orientation, support) = value
                .split_once(':')
                .map_or((value, None), |(orientation, support)| {
                    (orientation, support.parse::<u32>().ok())
                });
            let mut characters = orientation.chars();
            let local = characters.next()?;
            let partner = characters.next()?;
            if characters.next().is_some()
                || !matches!(local, '+' | '-')
                || !matches!(partner, '+' | '-')
            {
                return None;
            }
            Some(BreakpointOrientation {
                local_join_after_position: local == '+',
                partner_join_after_position: partner == '+',
                support,
            })
        })
        .take(MAX_ORIENTATIONS)
        .collect()
}

pub(super) fn classify_variant(reference: &[u8], alternate: &[u8]) -> VariantType {
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

pub(super) fn parse_breakend_partner(
    alternate_index: usize,
    alternate: &[u8],
) -> Option<BreakendPartner> {
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
