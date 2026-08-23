use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use flate2::read::ZlibDecoder;
use rgv_core::MAXIMUM_TDF_CHANNELS;

use crate::{HttpRangeConfig, HttpRangeReader, tracks::TrackError};

use crate::tracks::{
    MAX_QUANTITATIVE_RECORDS_PER_QUERY, QuantitativeQuery, QuantitativeRecord,
    QuantitativeSourceMetadata, TdfQueryMetadata,
};

const FIXED_HEADER_BYTES: usize = 24;
const GZIP_FLAG: u32 = 0x1;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
const MAX_MASTER_INDEX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTITY_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPRESSED_TILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_UNCOMPRESSED_TILE_BYTES: usize = 256 * 1024 * 1024;
const MAX_TILE_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_INDEX_ENTRIES: usize = 1_000_000;
const MAX_WINDOW_FUNCTIONS: usize = 128;
const MAX_ATTRIBUTES: usize = 65_536;
const MAX_STRING_BYTES: usize = 64 * 1024;
const TARGET_QUERY_RECORDS: u64 = 8_192;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TdfMetadata {
    pub version: u32,
    pub data_type: String,
    pub track_line: String,
    pub track_names: Vec<String>,
    pub genome_id: Option<String>,
    pub window_functions: Vec<String>,
    pub compressed: bool,
    pub contigs: Vec<String>,
}

#[derive(Clone, Debug)]
struct IndexEntry {
    position: u64,
    size: usize,
}

#[derive(Clone, Debug)]
struct Dataset {
    width: u64,
    positions: Vec<Option<u64>>,
    sizes: Vec<usize>,
}

enum RandomAccess {
    Local { file: File, length: u64 },
    Remote(Box<HttpRangeReader>),
}

impl RandomAccess {
    fn open(path: &Path) -> Result<Self, TrackError> {
        let raw = path.to_string_lossy();
        if raw.starts_with("http://") || raw.starts_with("https://") {
            let reader = HttpRangeReader::open(&raw, HttpRangeConfig::default())
                .map_err(|error| TrackError::Backend(error.to_string()))?;
            Ok(Self::Remote(Box::new(reader)))
        } else {
            let file = File::open(path)?;
            let length = file.metadata()?.len();
            Ok(Self::Local { file, length })
        }
    }

    fn length(&self) -> u64 {
        match self {
            Self::Local { length, .. } => *length,
            Self::Remote(reader) => reader.len(),
        }
    }

    fn read_range(&mut self, position: u64, size: usize) -> Result<Vec<u8>, TrackError> {
        let size_u64 = u64::try_from(size)
            .map_err(|_| TrackError::ResourceLimit("TDF range size overflowed".to_owned()))?;
        let end = position
            .checked_add(size_u64)
            .ok_or_else(|| TrackError::InvalidSchema("TDF range overflowed".to_owned()))?;
        if end > self.length() {
            return Err(TrackError::InvalidSchema(format!(
                "TDF range {position}..{end} exceeds the {}-byte source",
                self.length()
            )));
        }
        let mut bytes = vec![0; size];
        match self {
            Self::Local { file, .. } => {
                file.seek(SeekFrom::Start(position))?;
                file.read_exact(&mut bytes)?;
            }
            Self::Remote(reader) => {
                reader.seek(SeekFrom::Start(position))?;
                reader.read_exact(&mut bytes)?;
            }
        }
        Ok(bytes)
    }
}

pub struct TdfSource {
    reader: RandomAccess,
    metadata: TdfMetadata,
    datasets: BTreeMap<String, IndexEntry>,
    dataset_cache: HashMap<String, Dataset>,
    tile_cache: HashMap<u64, Arc<Vec<u8>>>,
    tile_cache_bytes: usize,
    selected_track: usize,
}

impl TdfSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, TrackError> {
        Self::open_with_track_name(path, None)
    }

    #[allow(clippy::too_many_lines)]
    pub fn open_with_track_name(
        path: impl AsRef<Path>,
        preferred_track_name: Option<&str>,
    ) -> Result<Self, TrackError> {
        let mut reader = RandomAccess::open(path.as_ref())?;
        if reader.length() < FIXED_HEADER_BYTES as u64 {
            return Err(TrackError::InvalidSchema(
                "TDF source is shorter than its fixed header".to_owned(),
            ));
        }
        let fixed = reader.read_range(0, FIXED_HEADER_BYTES)?;
        if !matches!(&fixed[0..3], b"TDF" | b"IBF") {
            return Err(TrackError::InvalidSchema(
                "TDF magic must begin with TDF".to_owned(),
            ));
        }
        let mut fixed = ByteCursor::new(&fixed);
        fixed.skip(4)?;
        let version = fixed.u32()?;
        if !(1..=4).contains(&version) {
            return Err(TrackError::InvalidSchema(format!(
                "unsupported TDF version {version}; versions 1 through 4 are supported"
            )));
        }
        let index_position = fixed.u64()?;
        let index_size = bounded_size(fixed.u32()?, MAX_MASTER_INDEX_BYTES, "master index")?;
        let header_size = bounded_size(fixed.u32()?, MAX_HEADER_BYTES, "header")?;

        let header_bytes = reader.read_range(FIXED_HEADER_BYTES as u64, header_size)?;
        let mut header = ByteCursor::new(&header_bytes);
        let window_functions = if version >= 2 {
            let count = header.count(MAX_WINDOW_FUNCTIONS, "window functions")?;
            (0..count)
                .map(|_| header.string())
                .collect::<Result<Vec<_>, _>>()?
        } else {
            vec!["mean".to_owned()]
        };
        let data_type = header.string()?;
        let track_line = header.string()?.trim().to_owned();
        let track_count = header.count(MAXIMUM_TDF_CHANNELS, "tracks")?;
        if track_count == 0 {
            return Err(TrackError::InvalidSchema(
                "TDF header does not contain a quantitative track".to_owned(),
            ));
        }
        let track_names = (0..track_count)
            .map(|_| header.string())
            .collect::<Result<Vec<_>, _>>()?;
        let (genome_id, compressed) = if version > 2 {
            let genome_id = header.string()?;
            let flags = header.u32()?;
            (Some(genome_id), flags & GZIP_FLAG != 0)
        } else {
            (None, false)
        };

        let index_bytes = reader.read_range(index_position, index_size)?;
        let mut index = ByteCursor::new(&index_bytes);
        let dataset_count = index.count(MAX_INDEX_ENTRIES, "datasets")?;
        let mut datasets = BTreeMap::new();
        for _ in 0..dataset_count {
            let name = index.string()?;
            let entry = read_index_entry(&mut index, reader.length(), "dataset")?;
            if datasets.insert(name.clone(), entry).is_some() {
                return Err(TrackError::InvalidSchema(format!(
                    "duplicate TDF dataset name {name:?}"
                )));
            }
        }
        let group_count = index.count(MAX_INDEX_ENTRIES, "groups")?;
        for _ in 0..group_count {
            let _name = index.string()?;
            let _entry = read_index_entry(&mut index, reader.length(), "group")?;
        }

        let mut contigs = datasets
            .keys()
            .filter_map(|name| dataset_contig(name).map(str::to_owned))
            .collect::<Vec<_>>();
        contigs.sort();
        contigs.dedup();
        let selected_track = preferred_track_name
            .and_then(|preferred| {
                track_names
                    .iter()
                    .position(|name| name == preferred)
                    .or_else(|| {
                        track_names
                            .iter()
                            .position(|name| name.eq_ignore_ascii_case(preferred))
                    })
            })
            .unwrap_or(0);
        let metadata = TdfMetadata {
            version,
            data_type,
            track_line,
            track_names,
            genome_id,
            window_functions,
            compressed,
            contigs,
        };
        Ok(Self {
            reader,
            metadata,
            datasets,
            dataset_cache: HashMap::new(),
            tile_cache: HashMap::new(),
            tile_cache_bytes: 0,
            selected_track,
        })
    }

    pub fn open_channel(path: impl AsRef<Path>, track_index: usize) -> Result<Self, TrackError> {
        let mut source = Self::open(path)?;
        if track_index >= source.metadata.track_names.len() {
            return Err(TrackError::InvalidSchema(format!(
                "TDF channel index {track_index} is outside the {} embedded channels",
                source.metadata.track_names.len()
            )));
        }
        source.selected_track = track_index;
        Ok(source)
    }

    pub const fn metadata(&self) -> &TdfMetadata {
        &self.metadata
    }

    pub const fn selected_track_index(&self) -> usize {
        self.selected_track
    }

    pub fn track_index_by_name(&self, name: &str) -> Option<usize> {
        self.metadata
            .track_names
            .iter()
            .position(|candidate| candidate == name)
            .or_else(|| {
                self.metadata
                    .track_names
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(name))
            })
    }

    pub fn query(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<QuantitativeQuery, TrackError> {
        self.query_channel(self.selected_track, contig, start, end)
    }

    pub fn query_channel(
        &mut self,
        track_index: usize,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<QuantitativeQuery, TrackError> {
        if track_index >= self.metadata.track_names.len() {
            return Err(TrackError::InvalidSchema(format!(
                "TDF channel index {track_index} is outside the {} embedded channels",
                self.metadata.track_names.len()
            )));
        }
        if start >= end {
            return Ok(QuantitativeQuery::default());
        }
        let Some(source_contig) = self.resolve_contig(contig) else {
            return Ok(QuantitativeQuery::default());
        };
        let Some(dataset_name) = self.choose_dataset(&source_contig, start, end)? else {
            return Ok(QuantitativeQuery::default());
        };
        let dataset = self.load_dataset(&dataset_name)?.clone();
        let start_tile = usize::try_from(start / dataset.width).unwrap_or(usize::MAX);
        let end_tile = usize::try_from((end - 1) / dataset.width).unwrap_or(usize::MAX);
        let final_tile = end_tile.min(dataset.positions.len().saturating_sub(1));
        let mut query = QuantitativeQuery::default();
        if start_tile <= final_tile {
            for tile_index in start_tile..=final_tile {
                let Some(position) = dataset.positions[tile_index] else {
                    continue;
                };
                let bytes = self.tile_bytes(position, dataset.sizes[tile_index])?;
                let records = parse_tile(&bytes, self.metadata.track_names.len(), track_index)?;
                for record in records {
                    if record.end <= start || record.start >= end {
                        continue;
                    }
                    query.provenance.records_examined =
                        query.provenance.records_examined.saturating_add(1);
                    if !record.value.is_finite() {
                        query.provenance.non_finite_records =
                            query.provenance.non_finite_records.saturating_add(1);
                        continue;
                    }
                    if query.records.len() == MAX_QUANTITATIVE_RECORDS_PER_QUERY {
                        query.provenance.records_truncated = true;
                        continue;
                    }
                    query.records.push(record);
                }
            }
        }
        query.provenance.records_retained = query.records.len();
        let (zoom, window_function) = dataset_description(&dataset_name);
        query.source_metadata = Some(QuantitativeSourceMetadata::Tdf(TdfQueryMetadata {
            version: self.metadata.version,
            genome_id: self.metadata.genome_id.clone(),
            data_type: self.metadata.data_type.clone(),
            selected_track_name: self.metadata.track_names[track_index].clone(),
            selected_track_index: track_index,
            track_count: self.metadata.track_names.len(),
            dataset: dataset_name,
            zoom,
            window_function,
            compressed: self.metadata.compressed,
        }));
        Ok(query)
    }

    fn resolve_contig(&self, contig: &str) -> Option<String> {
        if self.metadata.contigs.iter().any(|value| value == contig) {
            return Some(contig.to_owned());
        }
        let alternate = contig
            .strip_prefix("chr")
            .map_or_else(|| format!("chr{contig}"), str::to_owned);
        self.metadata
            .contigs
            .iter()
            .find(|value| value.as_str() == alternate)
            .cloned()
    }

    fn choose_dataset(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<Option<String>, TrackError> {
        let span = end.saturating_sub(start);
        let prefix = format!("/{contig}/z");
        let mut candidates = self
            .datasets
            .keys()
            .filter_map(|name| {
                let suffix = name.strip_prefix(&prefix)?;
                let (zoom_text, function) = suffix.split_once('/').unwrap_or((suffix, "mean"));
                let zoom = zoom_text.parse::<u32>().ok()?;
                let function_rank = usize::from(function != "mean");
                Some((zoom, function_rank, name.clone()))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(zoom, function_rank, _)| (*zoom, *function_rank));
        candidates.dedup_by_key(|(zoom, _, _)| *zoom);

        let mut chosen = None;
        for (_, _, name) in candidates {
            let dataset = self.load_dataset(&name)?;
            let tiles = span.saturating_add(dataset.width.saturating_sub(1)) / dataset.width;
            let estimate = tiles.saturating_add(1).saturating_mul(700);
            if estimate <= TARGET_QUERY_RECORDS || chosen.is_none() {
                chosen = Some(name);
            }
        }

        let raw = format!("/{contig}/raw");
        if self.datasets.contains_key(&raw) {
            if let Some(summary_name) = &chosen {
                let summary = self.load_dataset(summary_name)?;
                let summary_tiles =
                    span.saturating_add(summary.width.saturating_sub(1)) / summary.width;
                if summary_tiles.saturating_add(1).saturating_mul(700) <= 2_048 {
                    return Ok(Some(raw));
                }
            } else {
                return Ok(Some(raw));
            }
        }
        Ok(chosen)
    }

    fn load_dataset(&mut self, name: &str) -> Result<&Dataset, TrackError> {
        if !self.dataset_cache.contains_key(name) {
            let entry = self.datasets.get(name).ok_or_else(|| {
                TrackError::InvalidSchema(format!("TDF dataset {name:?} is not indexed"))
            })?;
            let bytes = self.reader.read_range(entry.position, entry.size)?;
            let dataset = parse_dataset(&bytes, self.reader.length())?;
            self.dataset_cache.insert(name.to_owned(), dataset);
        }
        Ok(self
            .dataset_cache
            .get(name)
            .expect("TDF dataset was inserted"))
    }

    fn tile_bytes(
        &mut self,
        position: u64,
        compressed_size: usize,
    ) -> Result<Arc<Vec<u8>>, TrackError> {
        if let Some(bytes) = self.tile_cache.get(&position) {
            return Ok(Arc::clone(bytes));
        }
        let source_bytes = self.reader.read_range(position, compressed_size)?;
        let bytes = if self.metadata.compressed {
            decompress_tile(&source_bytes)?
        } else {
            source_bytes
        };
        let bytes = Arc::new(bytes);
        if bytes.len() <= MAX_TILE_CACHE_BYTES {
            if self.tile_cache_bytes.saturating_add(bytes.len()) > MAX_TILE_CACHE_BYTES {
                self.tile_cache.clear();
                self.tile_cache_bytes = 0;
            }
            self.tile_cache_bytes = self.tile_cache_bytes.saturating_add(bytes.len());
            self.tile_cache.insert(position, Arc::clone(&bytes));
        }
        Ok(bytes)
    }
}

fn read_index_entry(
    cursor: &mut ByteCursor<'_>,
    source_length: u64,
    kind: &str,
) -> Result<IndexEntry, TrackError> {
    let position = cursor.u64()?;
    let size = bounded_size(cursor.u32()?, MAX_ENTITY_BYTES, kind)?;
    let end = position
        .checked_add(size as u64)
        .ok_or_else(|| TrackError::InvalidSchema(format!("TDF {kind} range overflowed")))?;
    if end > source_length {
        return Err(TrackError::InvalidSchema(format!(
            "TDF {kind} range exceeds the source"
        )));
    }
    Ok(IndexEntry { position, size })
}

fn parse_dataset(bytes: &[u8], source_length: u64) -> Result<Dataset, TrackError> {
    let mut cursor = ByteCursor::new(bytes);
    let attribute_count = cursor.count(MAX_ATTRIBUTES, "dataset attributes")?;
    for _ in 0..attribute_count {
        let _key = cursor.string()?;
        let _value = cursor.string()?;
    }
    let data_type = cursor.string()?;
    if !matches!(
        data_type.as_str(),
        "BYTE" | "SHORT" | "INT" | "FLOAT" | "DOUBLE" | "STRING"
    ) {
        return Err(TrackError::InvalidSchema(format!(
            "unsupported TDF dataset data type {data_type:?}"
        )));
    }
    let tile_width_float = cursor.f32()?;
    if !tile_width_float.is_finite() || tile_width_float < 1.0 {
        return Err(TrackError::InvalidSchema(
            "TDF dataset tile width must be finite and at least one".to_owned(),
        ));
    }
    let tile_width = tile_width_float as u64;
    let tile_count = cursor.count(MAX_INDEX_ENTRIES, "dataset tiles")?;
    let mut tile_positions = Vec::with_capacity(tile_count);
    let mut tile_sizes = Vec::with_capacity(tile_count);
    for _ in 0..tile_count {
        let raw_position = cursor.i64()?;
        let size = bounded_size(cursor.u32()?, MAX_COMPRESSED_TILE_BYTES, "tile")?;
        if raw_position < 0 {
            tile_positions.push(None);
            tile_sizes.push(0);
            continue;
        }
        let position = raw_position as u64;
        let end = position
            .checked_add(size as u64)
            .ok_or_else(|| TrackError::InvalidSchema("TDF tile range overflowed".to_owned()))?;
        if end > source_length {
            return Err(TrackError::InvalidSchema(
                "TDF tile range exceeds the source".to_owned(),
            ));
        }
        tile_positions.push(Some(position));
        tile_sizes.push(size);
    }
    Ok(Dataset {
        width: tile_width,
        positions: tile_positions,
        sizes: tile_sizes,
    })
}

fn decompress_tile(bytes: &[u8]) -> Result<Vec<u8>, TrackError> {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut output = Vec::with_capacity(bytes.len().saturating_mul(4).min(1024 * 1024));
    decoder
        .by_ref()
        .take(MAX_UNCOMPRESSED_TILE_BYTES as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|error| {
            TrackError::InvalidSchema(format!("invalid compressed TDF tile: {error}"))
        })?;
    if output.len() > MAX_UNCOMPRESSED_TILE_BYTES {
        return Err(TrackError::ResourceLimit(format!(
            "decompressed TDF tile exceeds {MAX_UNCOMPRESSED_TILE_BYTES} bytes"
        )));
    }
    Ok(output)
}

fn parse_tile(
    bytes: &[u8],
    track_count: usize,
    selected_track: usize,
) -> Result<Vec<QuantitativeRecord>, TrackError> {
    let mut cursor = ByteCursor::new(bytes);
    let tile_type = cursor.string()?;
    match tile_type.as_str() {
        "fixedStep" => parse_fixed_tile(&mut cursor, track_count, selected_track),
        "variableStep" => parse_variable_tile(&mut cursor, track_count, selected_track),
        "bed" => parse_bed_tile(&mut cursor, track_count, selected_track, false),
        "bedWithName" => parse_bed_tile(&mut cursor, track_count, selected_track, true),
        _ => Err(TrackError::InvalidSchema(format!(
            "unsupported TDF tile type {tile_type:?}"
        ))),
    }
}

fn parse_fixed_tile(
    cursor: &mut ByteCursor<'_>,
    track_count: usize,
    selected_track: usize,
) -> Result<Vec<QuantitativeRecord>, TrackError> {
    let count = cursor.count(MAX_QUANTITATIVE_RECORDS_PER_QUERY, "fixed-step positions")?;
    let start = nonnegative_coordinate(cursor.i32()?, "fixed-step start")?;
    let span = positive_span(cursor.f32()?)?;
    let selected_values = read_selected_values(cursor, track_count, selected_track, count)?;
    selected_values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            let offset = (index as f64 * span).floor() as u64;
            let next = (((index + 1) as f64) * span).floor() as u64;
            let record_start = start.saturating_add(offset);
            Ok(QuantitativeRecord {
                start: record_start,
                end: start
                    .saturating_add(next)
                    .max(record_start.saturating_add(1)),
                value,
            })
        })
        .collect()
}

fn parse_variable_tile(
    cursor: &mut ByteCursor<'_>,
    track_count: usize,
    selected_track: usize,
) -> Result<Vec<QuantitativeRecord>, TrackError> {
    let _tile_start = nonnegative_coordinate(cursor.i32()?, "variable-step tile start")?;
    let span = positive_span(cursor.f32()?)?;
    let count = cursor.count(
        MAX_QUANTITATIVE_RECORDS_PER_QUERY,
        "variable-step positions",
    )?;
    let starts = (0..count)
        .map(|_| nonnegative_coordinate(cursor.i32()?, "variable-step start"))
        .collect::<Result<Vec<_>, _>>()?;
    let encoded_tracks = cursor.count(MAXIMUM_TDF_CHANNELS, "variable-step tracks")?;
    if encoded_tracks != track_count {
        return Err(TrackError::InvalidSchema(format!(
            "TDF variable-step tile declares {encoded_tracks} tracks but the header declares {track_count}"
        )));
    }
    let selected_values = read_selected_values(cursor, track_count, selected_track, count)?;
    Ok(starts
        .into_iter()
        .zip(selected_values)
        .map(|(start, value)| QuantitativeRecord {
            start,
            end: start
                .saturating_add(span.ceil() as u64)
                .max(start.saturating_add(1)),
            value,
        })
        .collect::<Vec<_>>())
}

fn parse_bed_tile(
    cursor: &mut ByteCursor<'_>,
    track_count: usize,
    selected_track: usize,
    named: bool,
) -> Result<Vec<QuantitativeRecord>, TrackError> {
    let count = cursor.count(MAX_QUANTITATIVE_RECORDS_PER_QUERY, "BED positions")?;
    let starts = (0..count)
        .map(|_| nonnegative_coordinate(cursor.i32()?, "BED start"))
        .collect::<Result<Vec<_>, _>>()?;
    let ends = (0..count)
        .map(|_| nonnegative_coordinate(cursor.i32()?, "BED end"))
        .collect::<Result<Vec<_>, _>>()?;
    let encoded_tracks = cursor.count(MAXIMUM_TDF_CHANNELS, "BED tracks")?;
    if encoded_tracks != track_count {
        return Err(TrackError::InvalidSchema(format!(
            "TDF BED tile declares {encoded_tracks} tracks but the header declares {track_count}"
        )));
    }
    let selected_values = read_selected_values(cursor, track_count, selected_track, count)?;
    if named {
        for _ in 0..count {
            let _name = cursor.string()?;
        }
    }
    starts
        .into_iter()
        .zip(ends)
        .zip(selected_values)
        .map(|((start, end), value)| {
            if start >= end {
                return Err(TrackError::InvalidSchema(
                    "TDF BED tile start must be less than end".to_owned(),
                ));
            }
            Ok(QuantitativeRecord { start, end, value })
        })
        .collect()
}

fn read_selected_values(
    cursor: &mut ByteCursor<'_>,
    track_count: usize,
    selected_track: usize,
    position_count: usize,
) -> Result<Vec<f32>, TrackError> {
    let values = track_count
        .checked_mul(position_count)
        .ok_or_else(|| TrackError::ResourceLimit("TDF tile value count overflowed".to_owned()))?;
    let bytes = values
        .checked_mul(4)
        .ok_or_else(|| TrackError::ResourceLimit("TDF tile value bytes overflowed".to_owned()))?;
    cursor.require(bytes)?;
    let mut selected = Vec::with_capacity(position_count);
    for track in 0..track_count {
        for _ in 0..position_count {
            let value = cursor.f32()?;
            if track == selected_track {
                selected.push(value);
            }
        }
    }
    Ok(selected)
}

fn dataset_contig(name: &str) -> Option<&str> {
    let remainder = name.strip_prefix('/')?;
    let (contig, suffix) = remainder.split_once('/')?;
    (suffix == "raw" || suffix.starts_with('z')).then_some(contig)
}

fn dataset_description(name: &str) -> (Option<u32>, String) {
    let suffix = name.rsplit_once('/').map_or(name, |(_, suffix)| suffix);
    if suffix == "raw" {
        return (None, "raw".to_owned());
    }
    let components = name.trim_start_matches('/').split('/').collect::<Vec<_>>();
    let zoom = components
        .get(1)
        .and_then(|value| value.strip_prefix('z'))
        .and_then(|value| value.parse().ok());
    let function = components.get(2).copied().unwrap_or("mean").to_owned();
    (zoom, function)
}

fn bounded_size(value: u32, maximum: usize, label: &str) -> Result<usize, TrackError> {
    let value = value as usize;
    if value > maximum {
        return Err(TrackError::ResourceLimit(format!(
            "TDF {label} size {value} exceeds {maximum}"
        )));
    }
    Ok(value)
}

fn nonnegative_coordinate(value: i32, label: &str) -> Result<u64, TrackError> {
    u64::try_from(value)
        .map_err(|_| TrackError::InvalidSchema(format!("TDF {label} cannot be negative")))
}

fn positive_span(value: f32) -> Result<f64, TrackError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(TrackError::InvalidSchema(
            "TDF tile span must be finite and positive".to_owned(),
        ));
    }
    Ok(f64::from(value))
}

struct ByteCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn require(&self, count: usize) -> Result<(), TrackError> {
        if self.position.saturating_add(count) > self.bytes.len() {
            return Err(TrackError::InvalidSchema(
                "TDF binary section ended unexpectedly".to_owned(),
            ));
        }
        Ok(())
    }

    fn skip(&mut self, count: usize) -> Result<(), TrackError> {
        self.require(count)?;
        self.position += count;
        Ok(())
    }

    fn u32(&mut self) -> Result<u32, TrackError> {
        self.require(4)?;
        let bytes = self.bytes[self.position..self.position + 4]
            .try_into()
            .expect("four bytes were checked");
        self.position += 4;
        Ok(u32::from_le_bytes(bytes))
    }

    fn i32(&mut self) -> Result<i32, TrackError> {
        self.u32()
            .map(|value| i32::from_le_bytes(value.to_le_bytes()))
    }

    fn u64(&mut self) -> Result<u64, TrackError> {
        self.require(8)?;
        let bytes = self.bytes[self.position..self.position + 8]
            .try_into()
            .expect("eight bytes were checked");
        self.position += 8;
        Ok(u64::from_le_bytes(bytes))
    }

    fn i64(&mut self) -> Result<i64, TrackError> {
        self.u64()
            .map(|value| i64::from_le_bytes(value.to_le_bytes()))
    }

    fn f32(&mut self) -> Result<f32, TrackError> {
        self.u32().map(f32::from_bits)
    }

    fn count(&mut self, maximum: usize, label: &str) -> Result<usize, TrackError> {
        let count = self.u32()? as usize;
        if count > maximum {
            return Err(TrackError::ResourceLimit(format!(
                "TDF {label} count {count} exceeds {maximum}"
            )));
        }
        Ok(count)
    }

    fn string(&mut self) -> Result<String, TrackError> {
        let remainder = &self.bytes[self.position..];
        let Some(length) = remainder.iter().position(|byte| *byte == 0) else {
            return Err(TrackError::InvalidSchema(
                "TDF string is not null terminated".to_owned(),
            ));
        };
        if length > MAX_STRING_BYTES {
            return Err(TrackError::ResourceLimit(format!(
                "TDF string exceeds {MAX_STRING_BYTES} bytes"
            )));
        }
        let value = std::str::from_utf8(&remainder[..length]).map_err(|_| {
            TrackError::InvalidSchema("TDF string is not valid UTF-8/ASCII".to_owned())
        })?;
        self.position += length + 1;
        Ok(value.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::Write,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
        time::Duration,
    };

    use flate2::{Compression, write::ZlibEncoder};
    use tempfile::NamedTempFile;
    use tiny_http::{Header, Response, Server, StatusCode};

    use super::*;

    fn put_string(bytes: &mut Vec<u8>, value: &str) {
        bytes.extend_from_slice(value.as_bytes());
        bytes.push(0);
    }

    struct RangeServer {
        url: String,
        stop: Arc<AtomicBool>,
        requests: Arc<AtomicUsize>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl RangeServer {
        fn start(data: Vec<u8>) -> Self {
            let server = Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(AtomicUsize::new(0));
            let worker_stop = Arc::clone(&stop);
            let worker_requests = Arc::clone(&requests);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap()
                    else {
                        continue;
                    };
                    worker_requests.fetch_add(1, Ordering::Relaxed);
                    let range = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| parse_range(header.value.as_str(), data.len()))
                        .unwrap();
                    let response = Response::from_data(data[range.0..=range.1].to_vec())
                        .with_status_code(StatusCode(206))
                        .with_header(header(
                            "Content-Range",
                            &format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                        ))
                        .with_header(header("ETag", "\"tdf-fixture\""));
                    request.respond(response).unwrap();
                }
            });
            Self {
                url: format!("http://{address}/archive.tdf"),
                stop,
                requests,
                worker: Some(worker),
            }
        }
    }

    impl Drop for RangeServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn header(name: &str, value: &str) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
    }

    fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
        let value = value.strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        let start = start.parse::<usize>().ok()?;
        let end = if end.is_empty() {
            length.checked_sub(1)?
        } else {
            end.parse::<usize>().ok()?.min(length.checked_sub(1)?)
        };
        (start <= end).then_some((start, end))
    }

    fn build_tdf(compressed: bool) -> Vec<u8> {
        build_tdf_version(4, compressed)
    }

    fn build_tdf_version(version: u32, compressed: bool) -> Vec<u8> {
        let mut header = Vec::new();
        if version >= 2 {
            header.extend_from_slice(&1_u32.to_le_bytes());
            put_string(&mut header, "mean");
        }
        put_string(&mut header, "OTHER");
        put_string(&mut header, "track type=wiggle_0");
        header.extend_from_slice(&2_u32.to_le_bytes());
        put_string(&mut header, "sample one");
        put_string(&mut header, "sample two");
        if version > 2 {
            put_string(&mut header, "hg38");
            header.extend_from_slice(&(u32::from(compressed)).to_le_bytes());
        }

        let mut tile = Vec::new();
        put_string(&mut tile, "fixedStep");
        tile.extend_from_slice(&3_u32.to_le_bytes());
        tile.extend_from_slice(&100_i32.to_le_bytes());
        tile.extend_from_slice(&10_f32.to_le_bytes());
        for value in [1.0_f32, f32::NAN, 3.0, 10.0, 20.0, 30.0] {
            tile.extend_from_slice(&value.to_le_bytes());
        }
        let tile = if compressed {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&tile).unwrap();
            encoder.finish().unwrap()
        } else {
            tile
        };

        let tile_position = (FIXED_HEADER_BYTES + header.len()) as u64;
        let mut dataset = Vec::new();
        dataset.extend_from_slice(&0_u32.to_le_bytes());
        put_string(&mut dataset, "FLOAT");
        dataset.extend_from_slice(&1_000_f32.to_le_bytes());
        dataset.extend_from_slice(&1_u32.to_le_bytes());
        dataset.extend_from_slice(&i64::try_from(tile_position).unwrap().to_le_bytes());
        dataset.extend_from_slice(&(tile.len() as u32).to_le_bytes());
        let dataset_position = tile_position + tile.len() as u64;

        let mut index = Vec::new();
        index.extend_from_slice(&1_u32.to_le_bytes());
        put_string(
            &mut index,
            if version >= 2 {
                "/chr1/z0/mean"
            } else {
                "/chr1/z0"
            },
        );
        index.extend_from_slice(&dataset_position.to_le_bytes());
        index.extend_from_slice(&(dataset.len() as u32).to_le_bytes());
        index.extend_from_slice(&0_u32.to_le_bytes());
        let index_position = dataset_position + dataset.len() as u64;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"TDF4");
        bytes.extend_from_slice(&version.to_le_bytes());
        bytes.extend_from_slice(&index_position.to_le_bytes());
        bytes.extend_from_slice(&(index.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&tile);
        bytes.extend_from_slice(&dataset);
        bytes.extend_from_slice(&index);
        bytes
    }

    #[test]
    fn reads_indexed_compressed_tdf_and_selected_channel() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&build_tdf(true)).unwrap();
        let mut source = TdfSource::open_with_track_name(file.path(), Some("sample two")).unwrap();
        assert_eq!(source.metadata().version, 4);
        assert_eq!(source.metadata().contigs, ["chr1"]);
        assert!(source.metadata().compressed);
        assert_eq!(source.selected_track_index(), 1);

        let query = source.query("1", 100, 130).unwrap();
        assert_eq!(query.records.len(), 3);
        assert!((query.records[0].value - 10.0).abs() < f32::EPSILON);
        assert_eq!(query.records[2].end, 130);
        let Some(QuantitativeSourceMetadata::Tdf(metadata)) = query.source_metadata else {
            panic!("expected TDF query metadata");
        };
        assert_eq!(metadata.selected_track_name, "sample two");
        assert_eq!(metadata.track_count, 2);
        assert_eq!(metadata.zoom, Some(0));
        assert_eq!(metadata.window_function, "mean");
    }

    #[test]
    fn counts_non_finite_values_without_returning_them() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&build_tdf(false)).unwrap();
        let mut source = TdfSource::open(file.path()).unwrap();
        let query = source.query("chr1", 100, 130).unwrap();
        assert_eq!(query.records.len(), 2);
        assert_eq!(query.provenance.records_examined, 3);
        assert_eq!(query.provenance.non_finite_records, 1);
    }

    #[test]
    fn reads_legacy_tdf_header_versions() {
        for version in 1..=3 {
            let mut file = NamedTempFile::new().unwrap();
            file.write_all(&build_tdf_version(version, false)).unwrap();
            let mut source = TdfSource::open(file.path()).unwrap();
            assert_eq!(source.metadata().version, version);
            assert_eq!(
                source.metadata().genome_id.as_deref(),
                (version > 2).then_some("hg38")
            );
            assert_eq!(source.query("chr1", 100, 130).unwrap().records.len(), 2);
        }
    }

    #[test]
    fn remote_tdf_uses_bounded_http_ranges() {
        let bytes = build_tdf(true);
        let server = RangeServer::start(bytes);
        let mut source = TdfSource::open(Path::new(&server.url)).unwrap();
        let query = source.query("chr1", 100, 130).unwrap();
        assert_eq!(query.records.len(), 2);
        assert!(server.requests.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn parses_variable_step_bed_and_named_bed_tiles() {
        let mut variable = Vec::new();
        put_string(&mut variable, "variableStep");
        variable.extend_from_slice(&0_i32.to_le_bytes());
        variable.extend_from_slice(&5_f32.to_le_bytes());
        variable.extend_from_slice(&2_u32.to_le_bytes());
        variable.extend_from_slice(&10_i32.to_le_bytes());
        variable.extend_from_slice(&20_i32.to_le_bytes());
        variable.extend_from_slice(&1_u32.to_le_bytes());
        variable.extend_from_slice(&1.5_f32.to_le_bytes());
        variable.extend_from_slice(&2.5_f32.to_le_bytes());
        let records = parse_tile(&variable, 1, 0).unwrap();
        assert_eq!((records[0].start, records[0].end), (10, 15));
        assert!((records[1].value - 2.5).abs() < f32::EPSILON);

        for tile_type in ["bed", "bedWithName"] {
            let mut bed = Vec::new();
            put_string(&mut bed, tile_type);
            bed.extend_from_slice(&2_u32.to_le_bytes());
            bed.extend_from_slice(&30_i32.to_le_bytes());
            bed.extend_from_slice(&50_i32.to_le_bytes());
            bed.extend_from_slice(&40_i32.to_le_bytes());
            bed.extend_from_slice(&70_i32.to_le_bytes());
            bed.extend_from_slice(&1_u32.to_le_bytes());
            bed.extend_from_slice(&3.5_f32.to_le_bytes());
            bed.extend_from_slice(&4.5_f32.to_le_bytes());
            if tile_type == "bedWithName" {
                put_string(&mut bed, "first");
                put_string(&mut bed, "second");
            }
            let records = parse_tile(&bed, 1, 0).unwrap();
            assert_eq!((records[1].start, records[1].end), (50, 70));
            assert!((records[0].value - 3.5).abs() < f32::EPSILON);
        }
    }

    #[test]
    #[ignore = "requires the public IGV data server"]
    fn reads_official_igv_version_four_fixture() {
        let url = "https://data.broadinstitute.org/igvdata/tutorials/hg18/\
                   wgEncodeBroadChipSeqSignalK562H3k4me3.wig.gz.tdf";
        let mut source = TdfSource::open(Path::new(url)).unwrap();
        assert_eq!(source.metadata().version, 4);
        assert_eq!(source.metadata().track_names.len(), 1);
        assert!(source.metadata().compressed);
        let query = source.query("chr1", 0, 1_000_000).unwrap();
        assert!(!query.records.is_empty());
    }

    #[test]
    fn rejects_bad_magic_and_out_of_bounds_index() {
        let mut bad_magic = NamedTempFile::new().unwrap();
        let mut bytes = build_tdf(false);
        bytes[0] = b'X';
        bad_magic.write_all(&bytes).unwrap();
        assert!(matches!(
            TdfSource::open(bad_magic.path()),
            Err(TrackError::InvalidSchema(_))
        ));

        let mut bad_index = NamedTempFile::new().unwrap();
        let mut bytes = build_tdf(false);
        bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        bad_index.write_all(&bytes).unwrap();
        assert!(matches!(
            TdfSource::open(bad_index.path()),
            Err(TrackError::InvalidSchema(_))
        ));

        let mut oversized_header = NamedTempFile::new().unwrap();
        let mut bytes = build_tdf(false);
        bytes[20..24].copy_from_slice(&u32::try_from(MAX_HEADER_BYTES + 1).unwrap().to_le_bytes());
        oversized_header.write_all(&bytes).unwrap();
        assert!(matches!(
            TdfSource::open(oversized_header.path()),
            Err(TrackError::ResourceLimit(_))
        ));
    }
}
