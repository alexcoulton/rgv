#[allow(clippy::wildcard_imports)]
use super::*;

pub struct TextQuantitativeSource {
    signals: HashMap<String, QuantitativeIndex>,
}

#[derive(Default)]
struct QuantitativeIndex {
    records: Vec<QuantitativeRecord>,
    prefix_max_end: Vec<u64>,
}

impl QuantitativeIndex {
    fn finish(&mut self) {
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

    fn query(&self, start: u64, end: u64) -> QuantitativeQuery {
        let right = self.records.partition_point(|record| record.start < end);
        let left = self.prefix_max_end[..right].partition_point(|maximum| *maximum <= start);
        let mut query = QuantitativeQuery::default();
        for record in self.records[left..right]
            .iter()
            .filter(|record| record.end > start)
        {
            if !retain_quantitative_record(&mut query, *record, MAX_QUANTITATIVE_RECORDS_PER_QUERY)
            {
                break;
            }
        }
        query
    }
}

impl TextQuantitativeSource {
    pub fn open(path: impl AsRef<Path>, format: QuantitativeFormat) -> Result<Self, TrackError> {
        let file = BufReader::new(File::open(path)?);
        Self::from_reader(file, format)
    }

    pub fn from_reader(
        reader: impl BufRead,
        format: QuantitativeFormat,
    ) -> Result<Self, TrackError> {
        let mut signals: HashMap<String, QuantitativeIndex> = HashMap::new();
        let mut wig_state = None;
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
            let parsed = match format {
                QuantitativeFormat::BedGraph => parse_bedgraph(line),
                QuantitativeFormat::Wig => parse_wig_line(line, &mut wig_state),
            }
            .map_err(|message| TrackError::MalformedLine {
                line: line_number + 1,
                message,
            })?;
            if let Some((contig, record)) = parsed {
                signals.entry(contig).or_default().records.push(record);
            }
        }
        for index in signals.values_mut() {
            index.finish();
        }
        Ok(Self { signals })
    }

    #[must_use]
    pub fn query(&self, contig: &str, start: u64, end: u64) -> QuantitativeQuery {
        self.signals
            .get(contig)
            .map_or_else(QuantitativeQuery::default, |index| index.query(start, end))
    }

    pub fn contigs(&self) -> impl Iterator<Item = &str> {
        self.signals.keys().map(String::as_str)
    }
}
