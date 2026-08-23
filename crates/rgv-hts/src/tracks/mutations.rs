#[allow(clippy::wildcard_imports)]
use super::*;

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
