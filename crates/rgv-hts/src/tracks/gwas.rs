#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(super) fn bounded_gwas_records(visible: &[GwasRecord], limit: usize) -> Vec<GwasRecord> {
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
