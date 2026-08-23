#[allow(clippy::wildcard_imports)]
use super::*;

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
