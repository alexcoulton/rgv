//! Remote bgzipped VCF and BCF over [`HttpRangeReader`].
//!
//! Bytes are fetched with RGV's observable range reader (authentication, redirect policy,
//! cancellation, transfer budgets, byte accounting) and the tabix/CSI index is held in memory, so
//! nothing is written next to the process. Region selection mirrors `HTSlib`'s index iterator
//! exactly; the selected records (VCF text lines or BCF binary records, byte for byte) are then
//! parsed by `HTSlib` itself from a private temporary file, so a remote record decodes through
//! the very code path a local record does.

use std::io::{BufRead as _, Read as _, Write as _};

use noodles::{
    bgzf::{self, VirtualPosition},
    core::Position,
    csi::{
        self, BinningIndex,
        binning_index::index::{
            ReferenceSequence,
            header::Format as TabixFormat,
            reference_sequence::{Bin, bin::Chunk, index::LinearIndex},
        },
    },
};

use rust_htslib::bcf::Read as _;

use super::{TrackError, bcf};
use crate::{
    HttpRangeConfig, HttpRangeReader, RemoteCancellation, RemoteError, RemoteIoStats,
    RemoteTransferBudget, associated_remote_url, read_remote_object_bounded,
};

/// Largest compressed tabix/CSI index downloaded for a remote VCF.
const MAX_REMOTE_VARIANT_INDEX_BYTES: usize = 256 * 1024 * 1024;
/// Largest inflated tabix index held in memory.
const MAX_REMOTE_VARIANT_INDEX_INFLATED_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest VCF header text accepted from a remote file.
const MAX_REMOTE_VARIANT_HEADER_BYTES: usize = 64 * 1024 * 1024;
/// Largest single VCF line or BCF record accepted from a remote file.
const MAX_REMOTE_VARIANT_LINE_BYTES: usize = 256 * 1024 * 1024;
/// Text handed to `HTSlib` per parse; a query larger than this is parsed in several rounds so a
/// record limit stops the transfer early and memory stays bounded.
pub(super) const REMOTE_VARIANT_BATCH_BYTES: usize = 4 * 1024 * 1024;
/// Compressed bytes one remote ID-index build may transfer before it is reported as truncated.
pub const MAX_REMOTE_VARIANT_ID_SCAN_BYTES: u64 = 64 * 1024 * 1024;

const TABIX_MAGIC: &[u8; 4] = b"TBI\x01";
const TABIX_MIN_SHIFT: u8 = 14;
const TABIX_DEPTH: u8 = 5;
const TABIX_FORMAT_VCF: i32 = 2;
/// Flag of a tabix format: begin coordinates are 0-based and ends exclusive (BED).
pub(in crate::tracks) const TABIX_FLAG_ZERO_BASED: i32 = 0x10000;
const BCF_MAGIC: &[u8; 5] = b"BCF\x02\x02";
/// `l_shared`, `l_indiv`, then the fixed `CHROM`/`POS`/`rlen`/... fields of a BCF record.
const BCF_RECORD_PREFIX_BYTES: usize = 8;
const BCF_MINIMUM_SHARED_BYTES: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VariantFileFormat {
    Vcf,
    Bcf,
}

type TabixIndex = csi::binning_index::Index<LinearIndex>;

pub(in crate::tracks) enum RemoteVariantIndex {
    Tabix(TabixIndex),
    Csi(csi::Index),
}

/// What a tabix index (or the tabix header of a CSI index) says about the indexed text: the
/// `tbx_conf_t` of `HTSlib`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tracks) struct TabixColumns {
    /// Format (0 generic, 1 SAM, 2 VCF) with its flags.
    pub(in crate::tracks) preset: i32,
    /// 1-based column of the sequence name.
    pub(in crate::tracks) sequence: i32,
    /// 1-based column of the begin coordinate.
    pub(in crate::tracks) begin: i32,
    /// 1-based column of the end coordinate; 0 when there is none.
    pub(in crate::tracks) end: i32,
}

impl RemoteVariantIndex {
    pub(in crate::tracks) fn query(
        &self,
        reference: usize,
        interval: noodles::core::region::Interval,
    ) -> std::io::Result<Vec<Chunk>> {
        match self {
            Self::Tabix(index) => index.query(reference, interval),
            Self::Csi(index) => index.query(reference, interval),
        }
    }

    fn reference_count(&self) -> usize {
        match self {
            Self::Tabix(index) => index.reference_sequences().len(),
            Self::Csi(index) => index.reference_sequences().len(),
        }
    }

    /// Largest 1-based position the binning scheme can address.
    pub(in crate::tracks) fn maximum_position(&self) -> u64 {
        let (min_shift, depth) = match self {
            Self::Tabix(index) => (index.min_shift(), index.depth()),
            Self::Csi(index) => (index.min_shift(), index.depth()),
        };
        1_u64
            .checked_shl(u32::from(min_shift) + 3 * u32::from(depth))
            .map_or(u64::MAX, |limit| limit - 1)
    }
}

pub(super) struct RemoteVariantFile {
    reader: bgzf::io::Reader<HttpRangeReader>,
    format: VariantFileFormat,
    index: RemoteVariantIndex,
    /// VCF only: sequence names in index order; a region's reference id is its position here.
    /// A BCF region's reference id is the header's contig id.
    names: Vec<Vec<u8>>,
    /// Header text. For a VCF it ends in the `#CHROM` line and declares index-only contigs as
    /// `HTSlib` declares them for a local tabix-indexed file; for a BCF it is the verbatim
    /// `l_text` payload, so dictionary indices in the records keep their meaning.
    header_text: Vec<u8>,
    /// Virtual offset of the first record.
    data_start: VirtualPosition,
    raw_header: bcf::header::HeaderView,
    index_stats: RemoteIoStats,
    id_scan_byte_limit: u64,
}

/// A parsed batch: the reader borrows nothing but must not outlive its backing file.
pub(super) struct ParsedVariantBatch {
    pub(super) reader: bcf::Reader,
    _file: tempfile::NamedTempFile,
}

enum RegionTarget {
    /// VCF: the `CHROM` column.
    Name(Vec<u8>),
    /// BCF: the header contig id stored in each record.
    Id(i32),
}

/// Position of one region query inside the remote file.
pub(super) struct RegionCursor {
    target: RegionTarget,
    /// `HTSlib` yields a line when its tabix end is beyond this 0-based start ...
    start: i64,
    /// ... and stops at the first line whose 0-based position is beyond this one.
    last: i64,
    chunks: std::vec::IntoIter<Chunk>,
    chunk_end: Option<VirtualPosition>,
    high_water: VirtualPosition,
    finished: bool,
}

/// Sequential scan position used by the bounded ID-index build.
pub(super) struct ScanCursor {
    baseline_response_bytes: u64,
    finished: bool,
    pub(super) budget_exhausted: bool,
}

fn backend(error: impl std::fmt::Display) -> TrackError {
    TrackError::Backend(error.to_string())
}

pub(in crate::tracks) fn combine_stats(left: RemoteIoStats, right: RemoteIoStats) -> RemoteIoStats {
    RemoteIoStats {
        requests: left.requests.saturating_add(right.requests),
        response_bytes: left.response_bytes.saturating_add(right.response_bytes),
        cache_hits: left.cache_hits.saturating_add(right.cache_hits),
        cache_misses: left.cache_misses.saturating_add(right.cache_misses),
        evictions: left.evictions.saturating_add(right.evictions),
        retries: left.retries.saturating_add(right.retries),
        cancellations: left.cancellations.saturating_add(right.cancellations),
    }
}

impl RemoteVariantFile {
    pub(super) fn open(
        url: &str,
        index_url: Option<&str>,
        config: HttpRangeConfig,
    ) -> Result<Self, TrackError> {
        let source = HttpRangeReader::open(url, config.clone()).map_err(backend)?;
        let mut reader = bgzf::io::Reader::new(source);
        let format = sniff_format(&mut reader)?;
        let (index, index_names, index_stats) = open_index(url, index_url, format, config)?;
        let (header_text, raw_header, names) = match format {
            VariantFileFormat::Vcf => {
                let raw_text = read_vcf_header_text(&mut reader)?;
                let declared = parse_batch(format, &raw_text, &[])?;
                let text = declare_index_contigs(&raw_text, declared.reader.header(), &index_names);
                let header = parse_batch(format, &text, &[])?.reader.header().clone();
                (text, header, index_names)
            }
            VariantFileFormat::Bcf => {
                let text = read_bcf_header_text(&mut reader)?;
                let header = parse_batch(format, &text, &[])?.reader.header().clone();
                (text, header, Vec::new())
            }
        };
        let data_start = reader.virtual_position();
        Ok(Self {
            reader,
            format,
            index,
            names,
            header_text,
            data_start,
            raw_header,
            index_stats,
            id_scan_byte_limit: MAX_REMOTE_VARIANT_ID_SCAN_BYTES,
        })
    }

    pub(super) const fn raw_header(&self) -> &bcf::header::HeaderView {
        &self.raw_header
    }

    pub(super) fn stats(&self) -> RemoteIoStats {
        combine_stats(self.index_stats, self.reader.get_ref().stats())
    }

    pub(super) fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.reader.get_mut().set_cancellation(cancellation);
    }

    pub(super) fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        self.reader.get_mut().set_transfer_budget(budget);
    }

    #[cfg(test)]
    pub(super) const fn set_id_scan_byte_limit(&mut self, limit: u64) {
        self.id_scan_byte_limit = limit;
    }

    /// Parses `records` (whole newline-terminated VCF lines, or whole BCF records) under this
    /// file's header.
    pub(super) fn parse(&self, records: &[u8]) -> Result<ParsedVariantBatch, TrackError> {
        parse_batch(self.format, &self.header_text, records)
    }

    /// Starts a region query with the bounds `LocalVariantSource` gives `HTSlib`: 0-based
    /// `start`, exclusive `end`.
    pub(super) fn region(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<RegionCursor, TrackError> {
        let (reference, target) = match self.format {
            VariantFileFormat::Vcf => {
                let reference = self
                    .names
                    .iter()
                    .position(|name| name == contig.as_bytes())
                    .ok_or_else(|| {
                        TrackError::Backend(format!(
                            "contig {contig} is not present in the VCF index"
                        ))
                    })?;
                (reference, RegionTarget::Name(contig.as_bytes().to_vec()))
            }
            VariantFileFormat::Bcf => {
                let id = self
                    .raw_header
                    .name2rid(contig.as_bytes())
                    .map_err(backend)?;
                let reference = usize::try_from(id).map_err(|_| TrackError::Coordinate)?;
                if reference >= self.index.reference_count() {
                    // `HTSlib` cannot build an iterator for a contig the index does not hold.
                    return Err(TrackError::Backend(format!(
                        "contig {contig} is not present in the BCF index"
                    )));
                }
                (
                    reference,
                    RegionTarget::Id(i32::try_from(id).map_err(|_| TrackError::Coordinate)?),
                )
            }
        };
        let last = end.saturating_sub(1);
        let maximum = self.index.maximum_position();
        // The index is asked for every bin touching either bound; a line satisfying the exact
        // predicate in `next_batch` overlaps at least one of them.
        let low = start.min(last).saturating_add(1);
        let high = start.max(last).saturating_add(1).min(maximum);
        let chunks = if low > maximum {
            Vec::new()
        } else {
            let low = usize::try_from(low)
                .ok()
                .and_then(Position::new)
                .ok_or(TrackError::Coordinate)?;
            let high = usize::try_from(high)
                .ok()
                .and_then(Position::new)
                .ok_or(TrackError::Coordinate)?;
            self.index.query(reference, (low..=high).into())?
        };
        // Fetch what the index says this region needs, not a fixed read-ahead window.
        self.reader.get_mut().set_bgzf_read_plan(
            chunks
                .iter()
                .map(|chunk| (u64::from(chunk.start()), u64::from(chunk.end()))),
        );
        Ok(RegionCursor {
            target,
            start: i64::try_from(start).unwrap_or(i64::MAX),
            last: i64::try_from(last).unwrap_or(i64::MAX),
            chunks: chunks.into_iter(),
            chunk_end: None,
            high_water: VirtualPosition::from(0),
            finished: false,
        })
    }

    /// Returns the next records of the region, at most about `maximum_bytes` of them (always
    /// at least one record when any remain). An empty result means the region is exhausted.
    pub(super) fn next_region_batch(
        &mut self,
        cursor: &mut RegionCursor,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, TrackError> {
        use noodles::bgzf::io::Seek as _;

        let mut batch = Vec::new();
        let mut record = Vec::new();
        while !cursor.finished && batch.len() < maximum_bytes {
            let Some(chunk_end) = cursor.chunk_end else {
                let Some(chunk) = cursor.chunks.next() else {
                    cursor.finished = true;
                    break;
                };
                if chunk.end() <= cursor.high_water {
                    continue;
                }
                self.reader
                    .seek_to_virtual_position(chunk.start().max(cursor.high_water))?;
                cursor.chunk_end = Some(chunk.end());
                continue;
            };
            if self.reader.virtual_position() >= chunk_end
                || !read_record(&mut self.reader, self.format, &mut record)?
            {
                cursor.chunk_end = None;
                continue;
            }
            cursor.high_water = self.reader.virtual_position();
            if record.is_empty() {
                continue;
            }
            let (same_sequence, position, end) = match &cursor.target {
                RegionTarget::Name(name) => {
                    let line = record.strip_suffix(b"\n").unwrap_or(&record);
                    let interval = tabix_vcf_interval(line).ok_or_else(|| {
                        TrackError::Backend(
                            "remote VCF line has no valid CHROM/POS columns".to_owned(),
                        )
                    })?;
                    (
                        interval.name == name.as_slice(),
                        interval.position,
                        interval.end,
                    )
                }
                RegionTarget::Id(id) => {
                    let (record_id, position, length) = bcf_record_location(&record);
                    (
                        record_id == *id,
                        i64::from(position),
                        i64::from(position).saturating_add(i64::from(length)),
                    )
                }
            };
            // `hts_itr_next` ends the iteration at another sequence; rust-htslib ends it at the
            // first position beyond the requested end (comparing as unsigned, so a negative
            // position ends it too). A record ending at or before the start is skipped.
            if !same_sequence || position > cursor.last || position < 0 {
                cursor.finished = true;
                break;
            }
            if end <= cursor.start {
                continue;
            }
            batch.extend_from_slice(&record);
        }
        Ok(batch)
    }

    /// Rewinds to the first record for a sequential, budgeted scan of the whole file.
    pub(super) fn scan(&mut self) -> Result<ScanCursor, TrackError> {
        use noodles::bgzf::io::Seek as _;

        // A whole-file scan is sequential: the block cache's read-ahead is the right shape.
        self.reader.get_mut().clear_read_plan();
        self.reader.seek_to_virtual_position(self.data_start)?;
        Ok(ScanCursor {
            baseline_response_bytes: self.reader.get_ref().stats().response_bytes,
            finished: false,
            budget_exhausted: false,
        })
    }

    /// Returns the next records of a sequential scan. An empty result means the file ended or
    /// the scan's own transfer allowance ran out (`cursor.budget_exhausted`).
    pub(super) fn next_scan_batch(
        &mut self,
        cursor: &mut ScanCursor,
        maximum_bytes: usize,
        maximum_records: usize,
    ) -> Result<Vec<u8>, TrackError> {
        let mut batch = Vec::new();
        let mut records = 0_usize;
        let mut record = Vec::new();
        while !cursor.finished && batch.len() < maximum_bytes && records < maximum_records {
            let transferred = self
                .reader
                .get_ref()
                .stats()
                .response_bytes
                .saturating_sub(cursor.baseline_response_bytes);
            if transferred > self.id_scan_byte_limit {
                cursor.finished = true;
                cursor.budget_exhausted = true;
                break;
            }
            if !read_record(&mut self.reader, self.format, &mut record)? {
                cursor.finished = true;
                break;
            }
            if record.is_empty() {
                continue;
            }
            batch.extend_from_slice(&record);
            records += 1;
        }
        Ok(batch)
    }
}

/// Reads the next record exactly as it must be handed to `HTSlib`: a newline-terminated VCF
/// line or a whole BCF record. Returns `false` at the end of the data; a blank or comment VCF
/// line leaves `record` empty.
fn read_record(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
    format: VariantFileFormat,
    record: &mut Vec<u8>,
) -> Result<bool, TrackError> {
    match format {
        VariantFileFormat::Vcf => {
            if read_line(reader, record)? == 0 {
                return Ok(false);
            }
            if record.first() == Some(&b'#') {
                record.clear();
            }
            if !record.is_empty() {
                record.push(b'\n');
            }
            Ok(true)
        }
        VariantFileFormat::Bcf => read_bcf_record(reader, record),
    }
}

fn read_bcf_record(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
    record: &mut Vec<u8>,
) -> Result<bool, TrackError> {
    let malformed = || TrackError::Backend("truncated or malformed remote BCF record".to_owned());
    record.clear();
    let mut prefix = [0_u8; BCF_RECORD_PREFIX_BYTES];
    let mut filled = 0;
    while filled < prefix.len() {
        let count = reader.read(&mut prefix[filled..])?;
        if count == 0 {
            break;
        }
        filled += count;
    }
    if filled == 0 {
        return Ok(false);
    }
    if filled < prefix.len() {
        return Err(malformed());
    }
    let shared = u32::from_le_bytes(prefix[..4].try_into().unwrap()) as usize;
    let individual = u32::from_le_bytes(prefix[4..].try_into().unwrap()) as usize;
    if shared < BCF_MINIMUM_SHARED_BYTES {
        return Err(malformed());
    }
    let body = shared.saturating_add(individual);
    if body > MAX_REMOTE_VARIANT_LINE_BYTES {
        return Err(TrackError::ResourceLimit(
            "remote BCF record exceeds the supported length".to_owned(),
        ));
    }
    record.extend_from_slice(&prefix);
    record.resize(BCF_RECORD_PREFIX_BYTES + body, 0);
    reader
        .read_exact(&mut record[BCF_RECORD_PREFIX_BYTES..])
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                malformed()
            } else {
                TrackError::Io(error)
            }
        })?;
    Ok(true)
}

/// `CHROM`, `POS` and `rlen` of a record accepted by [`read_bcf_record`].
fn bcf_record_location(record: &[u8]) -> (i32, i32, i32) {
    let field = |offset: usize| {
        let start = BCF_RECORD_PREFIX_BYTES + offset;
        i32::from_le_bytes(record[start..start + 4].try_into().unwrap())
    };
    (field(0), field(4), field(8))
}

fn sniff_format(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
) -> Result<VariantFileFormat, TrackError> {
    let start = reader.fill_buf().map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            TrackError::Backend(format!(
                "remote variant file is not bgzip-compressed: {error}"
            ))
        } else {
            TrackError::Io(error)
        }
    })?;
    if start.starts_with(&BCF_MAGIC[..4]) {
        Ok(VariantFileFormat::Bcf)
    } else if start.starts_with(b"##fileformat=VCF") {
        Ok(VariantFileFormat::Vcf)
    } else {
        Err(TrackError::Backend(
            "remote object is not a bgzip-compressed VCF or BCF".to_owned(),
        ))
    }
}

fn read_bcf_header_text(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
) -> Result<Vec<u8>, TrackError> {
    let malformed = |error: std::io::Error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            TrackError::Backend("truncated remote BCF header".to_owned())
        } else {
            TrackError::Io(error)
        }
    };
    let mut prefix = [0_u8; 9];
    reader.read_exact(&mut prefix).map_err(malformed)?;
    if &prefix[..5] != BCF_MAGIC {
        return Err(TrackError::Backend(
            "remote BCF is not version 2.2".to_owned(),
        ));
    }
    let length = u32::from_le_bytes(prefix[5..].try_into().unwrap()) as usize;
    if length > MAX_REMOTE_VARIANT_HEADER_BYTES {
        return Err(TrackError::ResourceLimit(
            "remote BCF header exceeds the supported size".to_owned(),
        ));
    }
    let mut text = vec![0_u8; length];
    reader.read_exact(&mut text).map_err(malformed)?;
    Ok(text)
}

/// Reads one line without its terminator; returns the bytes consumed (0 at end of file).
pub(in crate::tracks) fn read_line(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
    line: &mut Vec<u8>,
) -> Result<usize, TrackError> {
    line.clear();
    let mut consumed = 0_usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let (take, complete) = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((available.len(), false), |newline| (newline + 1, true));
        if line.len().saturating_add(take) > MAX_REMOTE_VARIANT_LINE_BYTES {
            return Err(TrackError::ResourceLimit(
                "remote VCF line exceeds the supported length".to_owned(),
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        consumed += take;
        if complete {
            break;
        }
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(consumed)
}

fn read_vcf_header_text(
    reader: &mut bgzf::io::Reader<HttpRangeReader>,
) -> Result<Vec<u8>, TrackError> {
    let mut header = Vec::new();
    let mut line = Vec::new();
    loop {
        // Peeking leaves the reader at the first data line, which is where scans start.
        if reader.fill_buf()?.first() != Some(&b'#') {
            break;
        }
        read_line(reader, &mut line)?;
        if header.len().saturating_add(line.len()) > MAX_REMOTE_VARIANT_HEADER_BYTES {
            return Err(TrackError::ResourceLimit(
                "remote VCF header exceeds the supported size".to_owned(),
            ));
        }
        header.extend_from_slice(&line);
        header.push(b'\n');
    }
    Ok(header)
}

/// `HTSlib` adds every tabix sequence name the header does not declare as `##contig=<ID=name>`
/// when it opens a local indexed VCF; the same declarations are inserted before `#CHROM`.
fn declare_index_contigs(
    header_text: &[u8],
    header: &bcf::header::HeaderView,
    names: &[Vec<u8>],
) -> Vec<u8> {
    let missing = names
        .iter()
        .filter(|name| header.name2rid(name).is_err())
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return header_text.to_vec();
    }
    let column_line = header_text
        .windows(6)
        .rposition(|window| window == b"#CHROM")
        .filter(|offset| *offset == 0 || header_text[offset - 1] == b'\n')
        .unwrap_or(header_text.len());
    let mut augmented = header_text[..column_line].to_vec();
    for name in missing {
        augmented.extend_from_slice(b"##contig=<ID=");
        augmented.extend_from_slice(name);
        augmented.extend_from_slice(b">\n");
    }
    augmented.extend_from_slice(&header_text[column_line..]);
    augmented
}

fn parse_batch(
    format: VariantFileFormat,
    header_text: &[u8],
    records: &[u8],
) -> Result<ParsedVariantBatch, TrackError> {
    let mut file = tempfile::Builder::new()
        .prefix("rgv-remote-variants-")
        .suffix(match format {
            VariantFileFormat::Vcf => ".vcf",
            VariantFileFormat::Bcf => ".bcf",
        })
        .tempfile()?;
    match format {
        VariantFileFormat::Vcf => {
            file.write_all(header_text)?;
            file.write_all(records)?;
        }
        VariantFileFormat::Bcf => {
            let length = u32::try_from(header_text.len()).map_err(|_| TrackError::Coordinate)?;
            let mut writer = bgzf::io::Writer::new(file.as_file_mut());
            writer.write_all(BCF_MAGIC)?;
            writer.write_all(&length.to_le_bytes())?;
            writer.write_all(header_text)?;
            writer.write_all(records)?;
            writer.finish()?;
        }
    }
    file.flush()?;
    // Refuses a header `HTSlib` rejects, which `rust-htslib` would return as a null view.
    let reader = super::local::open_reader(file.path())?;
    Ok(ParsedVariantBatch {
        reader,
        _file: file,
    })
}

struct TabixInterval<'a> {
    name: &'a [u8],
    /// 0-based position, before `HTSlib` clamps a negative one to zero.
    position: i64,
    /// Exclusive 0-based end as tabix computes it for a VCF line.
    end: i64,
}

/// `tbx_parse1` for the VCF preset: `POS`, the `REF` length and `INFO/END`.
fn tabix_vcf_interval(line: &[u8]) -> Option<TabixInterval<'_>> {
    let mut name = None;
    let mut position = None;
    let mut begin = 0_i64;
    let mut end = -1_i64;
    for (column, field) in line.split(|byte| *byte == b'\t').enumerate() {
        match column {
            0 => name = Some(field),
            1 => {
                let (value, digits) = strtoll(field);
                if !digits {
                    return None;
                }
                position = Some(value.saturating_sub(1));
                begin = value.saturating_sub(1).max(0);
                end = value.max(1);
            }
            3 => {
                if !field.is_empty() {
                    end = begin.saturating_add(i64::try_from(field.len()).ok()?);
                }
            }
            7 => {
                let value = if field.starts_with(b"END=") {
                    Some(&field[4..])
                } else if find(field, b"END=").is_some() {
                    find(field, b";END=").map(|offset| &field[offset + 5..])
                } else {
                    None
                };
                if let Some(value) = value.filter(|value| value.first() != Some(&b'.')) {
                    let (declared, _) = strtoll(value);
                    if declared > begin {
                        end = declared;
                    }
                }
                break;
            }
            _ => {}
        }
    }
    Some(TabixInterval {
        name: name?,
        position: position?,
        end,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// C `strtoll(text, &end, 0)`: optional whitespace and sign, then a hexadecimal (`0x`), octal
/// (leading `0`) or decimal integer. The flag reports whether any character was consumed as a
/// number, which is what `tbx_parse1` tests.
pub(in crate::tracks) fn strtoll(text: &[u8]) -> (i64, bool) {
    let mut index = text
        .iter()
        .position(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r'))
        .unwrap_or(text.len());
    let negative = match text.get(index) {
        Some(b'-') => {
            index += 1;
            true
        }
        Some(b'+') => {
            index += 1;
            false
        }
        _ => false,
    };
    let digit = |byte: u8| -> Option<u32> { (byte as char).to_digit(36) };
    let radix = if text.get(index) == Some(&b'0')
        && matches!(text.get(index + 1), Some(b'x' | b'X'))
        && text
            .get(index + 2)
            .and_then(|byte| digit(*byte))
            .is_some_and(|value| value < 16)
    {
        index += 2;
        16
    } else if text.get(index) == Some(&b'0') {
        8
    } else {
        10
    };
    let mut value = 0_i64;
    let mut digits = false;
    while let Some(next) = text
        .get(index)
        .and_then(|byte| digit(*byte))
        .filter(|next| *next < radix)
    {
        value = value
            .saturating_mul(i64::from(radix))
            .saturating_add(i64::from(next));
        digits = true;
        index += 1;
    }
    (if negative { -value } else { value }, digits)
}

fn open_index(
    url: &str,
    explicit: Option<&str>,
    format: VariantFileFormat,
    config: HttpRangeConfig,
) -> Result<(RemoteVariantIndex, Vec<Vec<u8>>, RemoteIoStats), TrackError> {
    if let Some(index_url) = explicit {
        let (bytes, stats) =
            read_remote_object_bounded(index_url, config, MAX_REMOTE_VARIANT_INDEX_BYTES)
                .map_err(backend)?;
        let (index, names) = parse_index(&bytes, format)?;
        return Ok((index, names, stats));
    }
    // `HTSlib` prefers `.csi` over `.tbi` when both sit next to a VCF; a BCF has only `.csi`.
    let suffixes: &[&str] = match format {
        VariantFileFormat::Vcf => &[".csi", ".tbi"],
        VariantFileFormat::Bcf => &[".csi"],
    };
    let mut not_found = None;
    for suffix in suffixes {
        let index_url = associated_remote_url(url, suffix).map_err(backend)?;
        match read_remote_object_bounded(&index_url, config.clone(), MAX_REMOTE_VARIANT_INDEX_BYTES)
        {
            Ok((bytes, stats)) => {
                let (index, names) = parse_index(&bytes, format)?;
                return Ok((index, names, stats));
            }
            Err(RemoteError::Http(error))
                if error.status() == Some(reqwest::StatusCode::NOT_FOUND) =>
            {
                not_found = Some(RemoteError::Http(error));
            }
            Err(error) => return Err(backend(error)),
        }
    }
    Err(TrackError::Backend(format!(
        "remote variant file has no {} index: {}",
        suffixes.join(" or "),
        not_found.expect("every index candidate was tried")
    )))
}

fn parse_index(
    bytes: &[u8],
    format: VariantFileFormat,
) -> Result<(RemoteVariantIndex, Vec<Vec<u8>>), TrackError> {
    let mut inflated = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .take(MAX_REMOTE_VARIANT_INDEX_INFLATED_BYTES + 1)
        .read_to_end(&mut inflated)
        .map_err(|error| TrackError::Backend(format!("invalid variant index: {error}")))?;
    if inflated.len() as u64 > MAX_REMOTE_VARIANT_INDEX_INFLATED_BYTES {
        return Err(TrackError::ResourceLimit(
            "remote variant index exceeds the supported size".to_owned(),
        ));
    }
    if inflated.starts_with(TABIX_MAGIC) {
        if format == VariantFileFormat::Bcf {
            return Err(TrackError::Backend(
                "a BCF needs a CSI index, not a tabix index".to_owned(),
            ));
        }
        let (index, names, columns) = parse_tabix(&inflated)?;
        if columns.preset & 0xffff != TABIX_FORMAT_VCF {
            return Err(TrackError::Backend(
                "tabix index was not built for a VCF".to_owned(),
            ));
        }
        return Ok((RemoteVariantIndex::Tabix(index), names));
    }
    drop(inflated);
    let index = csi::io::Reader::new(std::io::Cursor::new(bytes))
        .read_index()
        .map_err(|error| TrackError::Backend(format!("invalid variant index: {error}")))?;
    if format == VariantFileFormat::Bcf {
        // Reference ids of a BCF index are the header's contig ids; no names are stored.
        return Ok((RemoteVariantIndex::Csi(index), Vec::new()));
    }
    let header = index.header().ok_or_else(|| {
        TrackError::Backend("CSI index carries no tabix header for a VCF".to_owned())
    })?;
    if header.format() != TabixFormat::Vcf {
        return Err(TrackError::Backend(
            "CSI index was not built for a VCF".to_owned(),
        ));
    }
    let names = header
        .reference_sequence_names()
        .iter()
        .map(|name| name.to_vec())
        .collect();
    Ok((RemoteVariantIndex::Csi(index), names))
}

struct IndexBytes<'a>(&'a [u8]);

impl IndexBytes<'_> {
    fn malformed() -> TrackError {
        TrackError::Backend("truncated or malformed tabix index".to_owned())
    }

    fn take(&mut self, count: usize) -> Result<&[u8], TrackError> {
        if self.0.len() < count {
            return Err(Self::malformed());
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Ok(head)
    }

    fn i32(&mut self) -> Result<i32, TrackError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, TrackError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, TrackError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// A count whose `width`-byte items must still fit in the remaining bytes, so a hostile
    /// count cannot drive an allocation.
    fn count(&mut self, width: usize) -> Result<usize, TrackError> {
        let count = usize::try_from(self.i32()?).map_err(|_| Self::malformed())?;
        if count.saturating_mul(width) > self.0.len() {
            return Err(Self::malformed());
        }
        Ok(count)
    }
}

/// Parses an inflated `.tbi` (SAM/tabix specification section 3) into the binning-index type
/// `noodles` queries, which is also how `noodles-tabix` represents it.
fn parse_tabix(inflated: &[u8]) -> Result<(TabixIndex, Vec<Vec<u8>>, TabixColumns), TrackError> {
    let mut bytes = IndexBytes(inflated);
    if bytes.take(4)? != TABIX_MAGIC {
        return Err(IndexBytes::malformed());
    }
    let references = bytes.count(4)?;
    let columns = TabixColumns {
        preset: bytes.i32()?,
        sequence: bytes.i32()?,
        begin: bytes.i32()?,
        end: bytes.i32()?,
    };
    // meta, skip: used only when the index is built.
    bytes.take(8)?;
    let names_length = bytes.count(1)?;
    let names = bytes
        .take(names_length)?
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    if names.len() != references {
        return Err(IndexBytes::malformed());
    }
    let metadata_bin = Bin::max_id(TABIX_DEPTH);
    let mut sequences = Vec::with_capacity(references);
    for _ in 0..references {
        let bin_count = bytes.count(8)?;
        let mut bins = Vec::with_capacity(bin_count);
        for _ in 0..bin_count {
            let id = usize::try_from(bytes.u32()?).map_err(|_| IndexBytes::malformed())?;
            let chunk_count = bytes.count(16)?;
            let mut chunks = Vec::with_capacity(chunk_count);
            for _ in 0..chunk_count {
                let start = VirtualPosition::from(bytes.u64()?);
                let end = VirtualPosition::from(bytes.u64()?);
                chunks.push(Chunk::new(start, end));
            }
            // Ids at and beyond the limit are the metadata pseudo-bin, not record locations.
            if id < metadata_bin {
                bins.push((id, Bin::new(chunks)));
            }
        }
        let interval_count = bytes.count(8)?;
        let mut linear = Vec::with_capacity(interval_count);
        for _ in 0..interval_count {
            linear.push(VirtualPosition::from(bytes.u64()?));
        }
        sequences.push(ReferenceSequence::new(
            bins.into_iter().collect(),
            linear,
            None,
        ));
    }
    let index = TabixIndex::builder()
        .set_min_shift(TABIX_MIN_SHIFT)
        .set_depth(TABIX_DEPTH)
        .set_reference_sequences(sequences)
        .build();
    Ok((index, names, columns))
}

/// Parses the tabix (`.tbi`) or CSI index of a bgzip-compressed text file of any tabix format,
/// returning the sequence names in index order and the column layout the index was built
/// with. `bytes` is the index file as stored (bgzip-compressed).
pub(in crate::tracks) fn parse_text_index(
    bytes: &[u8],
) -> Result<(RemoteVariantIndex, Vec<Vec<u8>>, TabixColumns), TrackError> {
    let mut inflated = Vec::new();
    flate2::read::MultiGzDecoder::new(bytes)
        .take(MAX_REMOTE_VARIANT_INDEX_INFLATED_BYTES + 1)
        .read_to_end(&mut inflated)
        .map_err(|error| TrackError::Backend(format!("invalid tabix index: {error}")))?;
    if inflated.len() as u64 > MAX_REMOTE_VARIANT_INDEX_INFLATED_BYTES {
        return Err(TrackError::ResourceLimit(
            "remote tabix index exceeds the supported size".to_owned(),
        ));
    }
    if inflated.starts_with(TABIX_MAGIC) {
        let (index, names, columns) = parse_tabix(&inflated)?;
        return Ok((RemoteVariantIndex::Tabix(index), names, columns));
    }
    drop(inflated);
    let index = csi::io::Reader::new(std::io::Cursor::new(bytes))
        .read_index()
        .map_err(|error| TrackError::Backend(format!("invalid CSI index: {error}")))?;
    let header = index.header().ok_or_else(|| {
        TrackError::Backend("CSI index carries no tabix header for a text file".to_owned())
    })?;
    let column =
        |index: usize| i32::try_from(index.saturating_add(1)).map_err(|_| IndexBytes::malformed());
    let columns = TabixColumns {
        preset: match header.format() {
            TabixFormat::Generic(
                csi::binning_index::index::header::format::CoordinateSystem::Gff,
            ) => 0,
            TabixFormat::Generic(
                csi::binning_index::index::header::format::CoordinateSystem::Bed,
            ) => TABIX_FLAG_ZERO_BASED,
            TabixFormat::Sam => 1,
            TabixFormat::Vcf => TABIX_FORMAT_VCF,
        },
        sequence: column(header.reference_sequence_name_index())?,
        begin: column(header.start_position_index())?,
        end: header.end_position_index().map_or(Ok(0), column)?,
    };
    let names = header
        .reference_sequence_names()
        .iter()
        .map(|name| name.to_vec())
        .collect();
    Ok((RemoteVariantIndex::Csi(index), names, columns))
}

/// Largest compressed tabix/CSI index downloaded for a remote text file.
pub(in crate::tracks) const MAX_REMOTE_TEXT_INDEX_BYTES: usize = MAX_REMOTE_VARIANT_INDEX_BYTES;

/// Fuzz entry: parses an index for either format and queries every sequence it lists.
pub(crate) fn fuzz_variant_index(bytes: &[u8]) -> bool {
    let mut parsed = parse_text_index(bytes).is_ok();
    for format in [VariantFileFormat::Vcf, VariantFileFormat::Bcf] {
        let Ok((index, _)) = parse_index(bytes, format) else {
            continue;
        };
        parsed = true;
        let maximum = usize::try_from(index.maximum_position()).unwrap_or(usize::MAX);
        let (Some(low), Some(high)) = (Position::new(1), Position::new(maximum)) else {
            continue;
        };
        for reference in 0..index.reference_count().min(8) {
            let _ = index.query(reference, (low..=high).into());
        }
    }
    parsed
}

/// Fuzz entry: the tabix position and end of one VCF data line.
pub(crate) fn fuzz_variant_line_interval(line: &[u8]) -> Option<(i64, i64)> {
    tabix_vcf_interval(line).map(|interval| (interval.position, interval.end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabix_interval_matches_htslib_for_ref_length_and_info_end() {
        let interval = tabix_vcf_interval(b"chr1\t100\t.\tACGT\tA\t.\t.\tDP=3").unwrap();
        assert_eq!(
            (interval.name, interval.position, interval.end),
            (&b"chr1"[..], 99, 103)
        );
        let interval =
            tabix_vcf_interval(b"chr1\t100\t.\tN\t<DEL>\t.\t.\tSVTYPE=DEL;END=500").unwrap();
        assert_eq!((interval.position, interval.end), (99, 500));
        let interval = tabix_vcf_interval(b"chr1\t100\t.\tN\t<DEL>\t.\t.\tEND=700;X=1").unwrap();
        assert_eq!(interval.end, 700);
        // `HTSlib` ignores an END that does not exceed the start, a missing END, and a key that
        // merely ends in "END".
        for info in ["END=50", "END=.", "MEND=900", "DP=1;MEND=900"] {
            let line = format!("chr1\t100\t.\tNN\t<DEL>\t.\t.\t{info}");
            assert_eq!(
                tabix_vcf_interval(line.as_bytes()).unwrap().end,
                101,
                "{info}"
            );
        }
        assert!(tabix_vcf_interval(b"chr1\tx\t.\tA\tT").is_none());
        // Fewer than eight columns still yields the REF-length interval.
        assert_eq!(tabix_vcf_interval(b"chr1\t7\t.\tAC").unwrap().end, 8);
    }

    #[test]
    fn strtoll_follows_c_base_detection() {
        assert_eq!(strtoll(b"42;X"), (42, true));
        assert_eq!(strtoll(b"010"), (8, true));
        assert_eq!(strtoll(b"0x1F"), (31, true));
        assert_eq!(strtoll(b" -7"), (-7, true));
        assert_eq!(strtoll(b"abc"), (0, false));
    }

    #[test]
    fn hostile_tabix_counts_are_rejected_without_allocation() {
        let mut bytes = TABIX_MAGIC.to_vec();
        bytes.extend_from_slice(&i32::MAX.to_le_bytes());
        bytes.extend_from_slice(&[0; 64]);
        assert!(parse_tabix(&bytes).is_err());
        assert!(parse_tabix(b"TBI\x01").is_err());
    }
}
