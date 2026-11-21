//! Remote bgzip-compressed, tabix- or CSI-indexed text (BED, GFF, GTF, ...) over
//! [`HttpRangeReader`] (HTS-16).
//!
//! The index is held in memory and only the BGZF blocks a region needs are requested, through
//! RGV's observable range reader (authentication, cancellation, transfer budgets, byte
//! accounting). Line selection follows `HTSlib`'s tabix iterator (`tbx_parse1` for the generic
//! format, then `hts_itr_next`), so a remote region yields the lines a local one does.

use std::io::BufRead as _;

use noodles::{
    bgzf::{self, VirtualPosition, io::Seek as _},
    core::Position,
    csi::binning_index::index::reference_sequence::bin::Chunk,
};

use super::{
    TrackError,
    variants::{
        MAX_REMOTE_TEXT_INDEX_BYTES, RemoteTabixIndex, TABIX_FLAG_ZERO_BASED, TabixColumns,
        combine_remote_stats, parse_text_index, read_bgzf_line, strtoll,
    },
};
use crate::{
    HttpRangeConfig, HttpRangeReader, RemoteCancellation, RemoteError, RemoteIoStats,
    RemoteTransferBudget, associated_remote_url, read_remote_object_bounded,
};

fn backend(error: impl std::fmt::Display) -> TrackError {
    TrackError::Backend(error.to_string())
}

pub(super) struct RemoteTabixText {
    reader: bgzf::io::Reader<HttpRangeReader>,
    index: RemoteTabixIndex,
    /// Sequence names in index order; a region's reference id is its position here.
    names: Vec<Vec<u8>>,
    columns: TabixColumns,
    index_stats: RemoteIoStats,
}

/// Position of one region query inside the remote file.
pub(super) struct TabixRegionCursor {
    name: Vec<u8>,
    /// 0-based start and exclusive end of the region, as given to `hts_itr_query`.
    start: i64,
    end: i64,
    chunks: std::vec::IntoIter<Chunk>,
    chunk_end: Option<VirtualPosition>,
    high_water: VirtualPosition,
    finished: bool,
}

/// Sequence name and 0-based half-open interval of one line: `tbx_parse1` for the generic
/// tabix format. `None` is the parse failure on which `HTSlib` abandons the iteration.
fn generic_interval<'a>(line: &'a [u8], columns: &TabixColumns) -> Option<(&'a [u8], i64, i64)> {
    let zero_based = columns.preset & TABIX_FLAG_ZERO_BASED != 0;
    let mut name = None;
    let mut begin = -1_i64;
    let mut end = -1_i64;
    for (index, field) in line.split(|byte| *byte == b'\t').enumerate() {
        let column = i32::try_from(index + 1).unwrap_or(i32::MAX);
        if column == columns.sequence {
            name = Some(field);
        } else if column == columns.begin {
            let (value, digits) = strtoll(field);
            begin = value;
            if columns.begin <= columns.end {
                end = begin;
            }
            if !digits {
                return None;
            }
            if !zero_based {
                begin = begin.saturating_sub(1);
            } else if columns.begin <= columns.end {
                end = end.saturating_add(1);
            }
            begin = begin.max(0);
            end = end.max(1);
        } else if column == columns.end {
            let (value, digits) = strtoll(field);
            if !digits {
                return None;
            }
            end = value;
        }
    }
    (begin >= 0 && end >= 0).then_some((name?, begin, end))
}

impl RemoteTabixText {
    /// Opens `url` with its index. With no `index_url`, `<url>.csi` and `<url>.tbi` are tried
    /// (the order `HTSlib` prefers); `Ok(None)` means the server has neither, which is the
    /// caller's cue to read the file whole. Any other failure is an error: an index that
    /// exists but cannot be fetched or parsed is not silently replaced by a full download.
    pub(super) fn open(
        url: &str,
        index_url: Option<&str>,
        config: &HttpRangeConfig,
    ) -> Result<Option<Self>, TrackError> {
        let fetch = |index_url: &str| {
            read_remote_object_bounded(index_url, config.clone(), MAX_REMOTE_TEXT_INDEX_BYTES)
        };
        let (bytes, index_stats) = if let Some(index_url) = index_url {
            fetch(index_url).map_err(backend)?
        } else {
            let mut found = None;
            for suffix in [".csi", ".tbi"] {
                let candidate = associated_remote_url(url, suffix).map_err(backend)?;
                match fetch(&candidate) {
                    Ok(index) => {
                        found = Some(index);
                        break;
                    }
                    // Absent. Object stores answer 403 for a key that does not exist when
                    // the caller may not list the bucket.
                    Err(RemoteError::Http(error))
                        if matches!(
                            error.status(),
                            Some(
                                reqwest::StatusCode::NOT_FOUND
                                    | reqwest::StatusCode::FORBIDDEN
                                    | reqwest::StatusCode::GONE
                            )
                        ) => {}
                    Err(error) => return Err(backend(error)),
                }
            }
            let Some(found) = found else {
                return Ok(None);
            };
            found
        };
        let (index, names, columns) = parse_text_index(&bytes)?;
        if columns.preset & 0xffff != 0 {
            return Err(TrackError::Backend(
                "the tabix index was built with the SAM or VCF preset, not for a BED, GFF or \
                 other generic text file"
                    .to_owned(),
            ));
        }
        let source = HttpRangeReader::open(url, config.clone()).map_err(backend)?;
        let mut reader = bgzf::io::Reader::new(source);
        reader.fill_buf().map_err(|error| {
            if error.kind() == std::io::ErrorKind::InvalidData {
                TrackError::Backend(format!(
                    "remote indexed text file is not bgzip-compressed: {error}"
                ))
            } else {
                TrackError::Io(error)
            }
        })?;
        Ok(Some(Self {
            reader,
            index,
            names,
            columns,
            index_stats,
        }))
    }

    pub(super) fn sequence_names(&self) -> Vec<String> {
        self.names
            .iter()
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect()
    }

    pub(super) fn stats(&self) -> RemoteIoStats {
        combine_remote_stats(self.index_stats, self.reader.get_ref().stats())
    }

    pub(super) fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.reader.get_mut().set_cancellation(cancellation);
    }

    pub(super) fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        self.reader.get_mut().set_transfer_budget(budget);
    }

    /// Starts a query of the 0-based half-open region `[start, end)` of `contig`, which must
    /// be one of [`Self::sequence_names`].
    pub(super) fn region(
        &mut self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> Result<TabixRegionCursor, TrackError> {
        let reference = self
            .names
            .iter()
            .position(|name| name == contig.as_bytes())
            .ok_or_else(|| {
                TrackError::Backend(format!("contig {contig} is not present in the tabix index"))
            })?;
        let maximum = self.index.maximum_position();
        // 1-based inclusive bounds of the bins to visit.
        let low = start.saturating_add(1);
        let high = end.max(low).min(maximum);
        let chunks = if low > maximum {
            Vec::new()
        } else {
            let position = |value: u64| {
                usize::try_from(value)
                    .ok()
                    .and_then(Position::new)
                    .ok_or(TrackError::Coordinate)
            };
            let mut chunks = self
                .index
                .query(reference, (position(low)?..=position(high)?).into())?;
            chunks.sort_by_key(|chunk| (chunk.start(), chunk.end()));
            chunks
        };
        // Fetch what the index says this region needs, not a fixed read-ahead window.
        self.reader.get_mut().set_bgzf_read_plan(
            chunks
                .iter()
                .map(|chunk| (u64::from(chunk.start()), u64::from(chunk.end()))),
        );
        Ok(TabixRegionCursor {
            name: contig.as_bytes().to_vec(),
            start: i64::try_from(start).unwrap_or(i64::MAX),
            end: i64::try_from(end).unwrap_or(i64::MAX),
            chunks: chunks.into_iter(),
            chunk_end: None,
            high_water: VirtualPosition::from(0),
            finished: false,
        })
    }

    /// Reads the next line of the region into `line` (without its terminator). `false` means
    /// the region is exhausted.
    pub(super) fn next_region_line(
        &mut self,
        cursor: &mut TabixRegionCursor,
        line: &mut Vec<u8>,
    ) -> Result<bool, TrackError> {
        while !cursor.finished {
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
                || read_bgzf_line(&mut self.reader, line)? == 0
            {
                cursor.chunk_end = None;
                continue;
            }
            cursor.high_water = self.reader.virtual_position();
            // `tbx_readrec`: a line the index columns cannot be read from, or naming a
            // sequence the index does not hold, ends the iteration with an error.
            let (name, begin, end) = generic_interval(line, &self.columns).ok_or_else(|| {
                TrackError::Backend(
                    "a line of the remote indexed text file has no valid coordinates in the \
                     indexed columns"
                        .to_owned(),
                )
            })?;
            if !self.names.iter().any(|known| known == name) {
                return Err(TrackError::Backend(
                    "a line of the remote indexed text file names a sequence its index does \
                     not hold"
                        .to_owned(),
                ));
            }
            // `hts_itr_next`: another sequence, or a begin at or beyond the region's end,
            // ends the iteration; a line ending at or before the region's start is skipped.
            if name != cursor.name.as_slice() || begin >= cursor.end {
                cursor.finished = true;
                break;
            }
            if end > cursor.start {
                return Ok(true);
            }
        }
        line.clear();
        Ok(false)
    }

    /// Rewinds to the start of the file for a sequential scan of every line and returns the
    /// response bytes transferred so far, the baseline of the scan's own allowance.
    pub(super) fn start_scan(&mut self) -> Result<u64, TrackError> {
        // A whole-file scan is sequential: the block cache's read-ahead is the right shape.
        self.reader.get_mut().clear_read_plan();
        self.reader
            .seek_to_virtual_position(VirtualPosition::from(0))?;
        Ok(self.reader.get_ref().stats().response_bytes)
    }

    /// Reads the next line of a sequential scan; `false` at the end of the file.
    pub(super) fn next_scan_line(&mut self, line: &mut Vec<u8>) -> Result<bool, TrackError> {
        Ok(read_bgzf_line(&mut self.reader, line)? != 0)
    }

    pub(super) fn response_bytes(&self) -> u64 {
        self.reader.get_ref().stats().response_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BED: TabixColumns = TabixColumns {
        preset: TABIX_FLAG_ZERO_BASED,
        sequence: 1,
        begin: 2,
        end: 3,
    };
    const GFF: TabixColumns = TabixColumns {
        preset: 0,
        sequence: 1,
        begin: 4,
        end: 5,
    };

    #[test]
    fn generic_interval_follows_tbx_parse1() {
        assert_eq!(
            generic_interval(b"chr1\t100\t250\tname", &BED),
            Some((&b"chr1"[..], 100, 250))
        );
        assert_eq!(
            generic_interval(b"chr2\tsrc\texon\t101\t250\t.\t+\t.\tID=x", &GFF),
            Some((&b"chr2"[..], 100, 250))
        );
        // A one-based begin of zero is clamped, as `HTSlib` clamps it.
        assert_eq!(
            generic_interval(b"chr2\tsrc\texon\t0\t9", &GFF),
            Some((&b"chr2"[..], 0, 9))
        );
        // Comment and separator lines, and lines without numeric coordinates, do not parse.
        assert_eq!(generic_interval(b"###", &GFF), None);
        assert_eq!(generic_interval(b"chr1\tx\t5", &BED), None);
        assert_eq!(generic_interval(b"chr1\t5\tx", &BED), None);
        // Without an end column the interval is the begin position itself.
        let point = TabixColumns { end: 2, ..BED };
        assert_eq!(
            generic_interval(b"chr1\t7", &point),
            Some((&b"chr1"[..], 7, 8))
        );
    }
}
