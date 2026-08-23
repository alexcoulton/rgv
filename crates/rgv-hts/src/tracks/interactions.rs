#[allow(clippy::wildcard_imports)]
use super::*;

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
