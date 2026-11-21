//! UCSC `.2bit` reference sequences, local or over HTTP range requests.
//!
//! The file is a small header, a name-to-offset index, and one record per sequence holding its
//! length, the runs of unknown bases, the soft-mask runs, and the bases packed four to a byte.
//! Only the header and index are read at open; a sequence's run tables are read the first time
//! that sequence is fetched. Soft-masking is reported only by a reference opened with
//! [`TwoBitReference::open_soft_masked`], which returns masked bases in lower case as a
//! soft-masked FASTA does; otherwise bases are always upper case and the mask tables are
//! never read.

use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};

use crate::AlignmentError;

const SIGNATURE: u32 = 0x1A41_2743;
const MAX_SEQUENCES: u32 = 4_000_000;
const MAX_RUNS: u32 = 64_000_000;
const BASES: [u8; 4] = *b"TCAG";

pub(crate) trait ReadSeek: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeek for T {}

struct SequenceEntry {
    name: String,
    offset: u64,
}

#[derive(Clone)]
struct SequenceLayout {
    length: u64,
    /// Sorted, non-overlapping half-open runs of unknown (`N`) bases.
    unknown_runs: Vec<(u64, u64)>,
    /// Sorted half-open runs of soft-masked bases; empty unless soft-masking is reported.
    masked_runs: Vec<(u64, u64)>,
    packed_offset: u64,
}

struct State {
    reader: Box<dyn ReadSeek>,
    layouts: HashMap<usize, SequenceLayout>,
}

/// Smallest index entry: a name-length byte, a one-byte name and a 32-bit offset.
const MINIMUM_INDEX_ENTRY_BYTES: u64 = 6;
const HEADER_BYTES: u64 = 16;

pub(crate) struct TwoBitReference {
    state: Mutex<State>,
    entries: Vec<SequenceEntry>,
    by_name: HashMap<String, usize>,
    swapped: bool,
    /// Whether masked bases are returned in lower case.
    soft_masking: bool,
    /// Total size of the file. Every count read from the file is checked against it before
    /// memory is reserved, so a hostile header cannot demand more than the file could hold.
    file_length: u64,
}

fn invalid(message: impl Into<String>) -> AlignmentError {
    AlignmentError::InvalidFastaIndex(format!("2bit: {}", message.into()))
}

fn read_u32(reader: &mut dyn ReadSeek, swapped: bool) -> Result<u32, AlignmentError> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(if swapped {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

fn read_u64(reader: &mut dyn ReadSeek, swapped: bool) -> Result<u64, AlignmentError> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(if swapped {
        u64::from_be_bytes(bytes)
    } else {
        u64::from_le_bytes(bytes)
    })
}

fn read_u32_table(
    reader: &mut dyn ReadSeek,
    count: usize,
    swapped: bool,
) -> Result<Vec<u32>, AlignmentError> {
    let mut bytes = vec![0_u8; count.saturating_mul(4)];
    reader.read_exact(&mut bytes)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| {
            let chunk = [chunk[0], chunk[1], chunk[2], chunk[3]];
            if swapped {
                u32::from_be_bytes(chunk)
            } else {
                u32::from_le_bytes(chunk)
            }
        })
        .collect())
}

impl TwoBitReference {
    /// Opens a reference whose bases are always upper case.
    pub(crate) fn open(reader: Box<dyn ReadSeek>) -> Result<Self, AlignmentError> {
        Self::open_with(reader, false)
    }

    /// Opens a reference that returns soft-masked bases in lower case (`acgtn`).
    pub(crate) fn open_soft_masked(reader: Box<dyn ReadSeek>) -> Result<Self, AlignmentError> {
        Self::open_with(reader, true)
    }

    fn open_with(
        mut reader: Box<dyn ReadSeek>,
        soft_masking: bool,
    ) -> Result<Self, AlignmentError> {
        let file_length = reader.seek(SeekFrom::End(0))?;
        reader.seek(SeekFrom::Start(0))?;
        let mut signature = [0_u8; 4];
        reader.read_exact(&mut signature)?;
        let swapped = if u32::from_le_bytes(signature) == SIGNATURE {
            false
        } else if u32::from_be_bytes(signature) == SIGNATURE {
            true
        } else {
            return Err(invalid("the file does not start with the 2bit signature"));
        };
        let version = read_u32(reader.as_mut(), swapped)?;
        // Version 1 differs only in using 64-bit sequence offsets for files beyond 4 GiB.
        let wide_offsets = match version {
            0 => false,
            1 => true,
            other => return Err(invalid(format!("unsupported version {other}"))),
        };
        let sequence_count = read_u32(reader.as_mut(), swapped)?;
        let _reserved = read_u32(reader.as_mut(), swapped)?;
        if sequence_count > MAX_SEQUENCES {
            return Err(invalid(format!(
                "{sequence_count} sequences exceeds the supported maximum"
            )));
        }
        if u64::from(sequence_count) * MINIMUM_INDEX_ENTRY_BYTES
            > file_length.saturating_sub(HEADER_BYTES)
        {
            return Err(invalid(format!(
                "the header declares {sequence_count} sequences, more than a {file_length}-byte \
                 file can index"
            )));
        }
        let mut entries = Vec::with_capacity(sequence_count as usize);
        let mut by_name = HashMap::with_capacity(sequence_count as usize);
        for index in 0..sequence_count as usize {
            let mut name_length = [0_u8; 1];
            reader.read_exact(&mut name_length)?;
            let mut name = vec![0_u8; usize::from(name_length[0])];
            reader.read_exact(&mut name)?;
            let name = String::from_utf8(name)
                .map_err(|_| invalid("a sequence name is not valid UTF-8"))?;
            let offset = if wide_offsets {
                read_u64(reader.as_mut(), swapped)?
            } else {
                u64::from(read_u32(reader.as_mut(), swapped)?)
            };
            if name.is_empty() || by_name.insert(name.clone(), index).is_some() {
                return Err(invalid(format!(
                    "empty or duplicate sequence name '{name}'"
                )));
            }
            entries.push(SequenceEntry { name, offset });
        }
        Ok(Self {
            state: Mutex::new(State {
                reader,
                layouts: HashMap::new(),
            }),
            entries,
            by_name,
            swapped,
            soft_masking,
            file_length,
        })
    }

    pub(crate) fn sequence_names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    /// Reads one sequence's length directly from its record. Prefer a chromosome-sizes table for
    /// remote files with many sequences: this costs one small read per sequence.
    pub(crate) fn sequence_length(&self, name: &str) -> Result<u64, AlignmentError> {
        let index = *self
            .by_name
            .get(name)
            .ok_or_else(|| invalid(format!("missing sequence {name}")))?;
        let mut state = self.lock()?;
        if let Some(layout) = state.layouts.get(&index) {
            return Ok(layout.length);
        }
        state
            .reader
            .seek(SeekFrom::Start(self.entries[index].offset))?;
        Ok(u64::from(read_u32(state.reader.as_mut(), self.swapped)?))
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, AlignmentError> {
        self.state
            .lock()
            .map_err(|_| invalid("the reader lock was poisoned"))
    }

    fn layout(&self, state: &mut State, index: usize) -> Result<SequenceLayout, AlignmentError> {
        if let Some(layout) = state.layouts.get(&index) {
            return Ok(layout.clone());
        }
        let swapped = self.swapped;
        let record_offset = self.entries[index].offset;
        let reader = state.reader.as_mut();
        reader.seek(SeekFrom::Start(record_offset))?;
        let length = u64::from(read_u32(reader, swapped)?);
        let unknown_count = read_u32(reader, swapped)?;
        // Two 4-byte tables follow; they must fit in what is left of the file. Without this a
        // forged count reserves up to two 256 MiB buffers before the short read is noticed.
        let remaining = self.file_length.saturating_sub(record_offset);
        if unknown_count > MAX_RUNS || u64::from(unknown_count) * 8 > remaining {
            return Err(invalid("unknown-base run table is implausibly large"));
        }
        let starts = read_u32_table(reader, unknown_count as usize, swapped)?;
        let sizes = read_u32_table(reader, unknown_count as usize, swapped)?;
        let mask_count = read_u32(reader, swapped)?;
        if mask_count > MAX_RUNS || u64::from(mask_count) * 8 > remaining {
            return Err(invalid("soft-mask run table is implausibly large"));
        }
        // Unless soft-masking is reported, the mask tables are skipped without being read (or
        // downloaded). The reserved word follows them.
        let masked_runs = if self.soft_masking {
            let starts = read_u32_table(reader, mask_count as usize, swapped)?;
            let sizes = read_u32_table(reader, mask_count as usize, swapped)?;
            let mut runs = starts
                .into_iter()
                .zip(sizes)
                .map(|(start, size)| {
                    let start = u64::from(start);
                    (start, start.saturating_add(u64::from(size)).min(length))
                })
                .filter(|(start, end)| start < end)
                .collect::<Vec<_>>();
            runs.sort_unstable();
            runs
        } else {
            Vec::new()
        };
        let packed_offset = record_offset
            .checked_add(4 + 4)
            .and_then(|offset| offset.checked_add(u64::from(unknown_count) * 8))
            .and_then(|offset| offset.checked_add(4))
            .and_then(|offset| offset.checked_add(u64::from(mask_count) * 8))
            .and_then(|offset| offset.checked_add(4))
            .ok_or(AlignmentError::CoordinateOverflow)?;
        // The packed bases (four to a byte) must lie inside the file too, so a forged length
        // cannot make `fetch` reserve a buffer the file cannot fill.
        if packed_offset
            .checked_add(length.div_ceil(4))
            .is_none_or(|packed_end| packed_end > self.file_length)
        {
            return Err(invalid("a sequence extends beyond the end of the file"));
        }
        let mut unknown_runs = starts
            .into_iter()
            .zip(sizes)
            .map(|(start, size)| {
                let start = u64::from(start);
                (start, start.saturating_add(u64::from(size)).min(length))
            })
            .filter(|(start, end)| start < end)
            .collect::<Vec<_>>();
        unknown_runs.sort_unstable();
        // A valid file's runs are disjoint. Overlapping ones (a forged or damaged table) are
        // merged, because `fetch` finds the first run by bisecting on the run ends, which
        // only disjoint runs keep in order.
        unknown_runs.dedup_by(|run, merged| {
            let overlaps = run.0 <= merged.1;
            if overlaps {
                merged.1 = merged.1.max(run.1);
            }
            overlaps
        });
        let layout = SequenceLayout {
            length,
            unknown_runs,
            masked_runs,
            packed_offset,
        };
        state.layouts.insert(index, layout.clone());
        Ok(layout)
    }

    pub(crate) fn fetch(
        &self,
        name: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, AlignmentError> {
        let index = *self
            .by_name
            .get(name)
            .ok_or_else(|| invalid(format!("missing sequence {name}")))?;
        let mut state = self.lock()?;
        let layout = self.layout(&mut state, index)?;
        if start >= end || end > layout.length {
            return Err(AlignmentError::ReferenceIntervalOutOfBounds {
                contig: name.to_owned(),
                start,
                end,
                length: layout.length,
            });
        }
        let first_byte = start / 4;
        let last_byte = (end - 1) / 4;
        let mut packed = vec![
            0_u8;
            usize::try_from(last_byte - first_byte + 1)
                .map_err(|_| AlignmentError::CoordinateOverflow)?
        ];
        state.reader.seek(SeekFrom::Start(
            layout
                .packed_offset
                .checked_add(first_byte)
                .ok_or(AlignmentError::CoordinateOverflow)?,
        ))?;
        state.reader.read_exact(&mut packed)?;
        drop(state);

        let count = usize::try_from(end - start).map_err(|_| AlignmentError::CoordinateOverflow)?;
        let mut bases = Vec::with_capacity(count);
        for position in start..end {
            let byte = packed[(position / 4 - first_byte) as usize];
            let shift = 6 - 2 * (position % 4);
            bases.push(BASES[usize::from((byte >> shift) & 0b11)]);
        }
        // Overwrite the unknown runs which intersect the request.
        let first_run = layout
            .unknown_runs
            .partition_point(|(_, run_end)| *run_end <= start);
        for (run_start, run_end) in &layout.unknown_runs[first_run..] {
            if *run_start >= end {
                break;
            }
            let from = (run_start.max(&start) - start) as usize;
            let to = (run_end.min(&end) - start) as usize;
            bases[from..to].fill(b'N');
        }
        // Soft-masked runs may overlap each other; lower-casing twice is harmless. A run that
        // starts before the request can still reach into it, so every run starting before the
        // request's end is considered.
        let masked_before_end = layout
            .masked_runs
            .partition_point(|(run_start, _)| *run_start < end);
        for (run_start, run_end) in &layout.masked_runs[..masked_before_end] {
            if *run_end <= start {
                continue;
            }
            let from = (run_start.max(&start) - start) as usize;
            let to = (run_end.min(&end) - start) as usize;
            bases[from..to].make_ascii_lowercase();
        }
        Ok(bases)
    }
}

/// Parses a UCSC `chrom.sizes` table (`name<TAB>length` per line).
pub(crate) fn parse_chrom_sizes(bytes: &[u8]) -> Result<Vec<(String, u64)>, AlignmentError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("chrom sizes is not UTF-8"))?;
    let mut sizes = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let name = fields.next().unwrap_or_default();
        let length = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| invalid(format!("chrom sizes line {} has no length", line_index + 1)))?;
        sizes.push((name.to_owned(), length));
    }
    Ok(sizes)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use super::*;

    /// Encodes sequences as a little- or big-endian version-0 2bit file.
    pub(crate) fn encode_two_bit(sequences: &[(&str, &str)], big_endian: bool) -> Vec<u8> {
        let word = |value: u32| {
            if big_endian {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        let mut records = Vec::new();
        for (_, sequence) in sequences {
            let bytes = sequence.as_bytes();
            let mut record = Vec::new();
            record.extend(word(bytes.len() as u32));
            let mut runs = Vec::<(u32, u32)>::new();
            let mut masks = Vec::<(u32, u32)>::new();
            for (index, base) in bytes.iter().enumerate() {
                let index = index as u32;
                if base.eq_ignore_ascii_case(&b'N') {
                    match runs.last_mut() {
                        Some((start, size)) if *start + *size == index => *size += 1,
                        _ => runs.push((index, 1)),
                    }
                }
                if base.is_ascii_lowercase() {
                    match masks.last_mut() {
                        Some((start, size)) if *start + *size == index => *size += 1,
                        _ => masks.push((index, 1)),
                    }
                }
            }
            for table in [&runs, &masks] {
                record.extend(word(table.len() as u32));
                for (start, _) in table {
                    record.extend(word(*start));
                }
                for (_, size) in table {
                    record.extend(word(*size));
                }
            }
            record.extend(word(0));
            for chunk in bytes.chunks(4) {
                let mut packed = 0_u8;
                for (slot, base) in chunk.iter().enumerate() {
                    let code = match base.to_ascii_uppercase() {
                        b'C' => 1,
                        b'A' => 2,
                        b'G' => 3,
                        _ => 0,
                    };
                    packed |= code << (6 - 2 * slot);
                }
                record.push(packed);
            }
            records.push(record);
        }
        let index_bytes = sequences
            .iter()
            .map(|(name, _)| 1 + name.len() + 4)
            .sum::<usize>();
        let mut file = Vec::new();
        file.extend(word(SIGNATURE));
        file.extend(word(0));
        file.extend(word(sequences.len() as u32));
        file.extend(word(0));
        let mut offset = 16 + index_bytes;
        for ((name, _), record) in sequences.iter().zip(&records) {
            file.push(name.len() as u8);
            file.extend(name.as_bytes());
            file.extend(word(offset as u32));
            offset += record.len();
        }
        for record in records {
            file.extend(record);
        }
        file
    }

    const FIRST: &str = "ACGTNNNNacgtACGTTTGACCANNGATTACAGATTACAnnnnC";
    const SECOND: &str = "GGGCCCTTTAAAC";

    #[test]
    fn forged_counts_are_rejected_against_the_file_size_before_allocation() {
        let valid = encode_two_bit(&[("chrA", "ACGTACGT")], false);
        // Header: signature, version, sequence count, reserved. One index entry (1 + 4 + 4
        // bytes) puts the record at byte 25: length, unknown-run count, ...
        let record = 16 + 1 + 4 + 4;
        let message = |result: Result<Vec<u8>, AlignmentError>| result.unwrap_err().to_string();
        let patched = |offset: usize, value: u32| {
            let mut file = valid.clone();
            file[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            file
        };

        // Four million sequences cannot be indexed by a 43-byte file.
        let error = TwoBitReference::open(Box::new(Cursor::new(patched(8, MAX_SEQUENCES))))
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("more than a 43-byte file can index"),
            "{error}"
        );

        // 60 million unknown runs would be two 240 MB tables.
        let forged_runs =
            TwoBitReference::open(Box::new(Cursor::new(patched(record + 4, 60_000_000)))).unwrap();
        assert!(message(forged_runs.fetch("chrA", 0, 4)).contains("unknown-base run table"));

        // A 4 Gb sequence in a 43-byte file: the packed bases are not there to fetch.
        let forged_length =
            TwoBitReference::open(Box::new(Cursor::new(patched(record, u32::MAX)))).unwrap();
        assert!(
            message(forged_length.fetch("chrA", 0, 1_000_000_000))
                .contains("extends beyond the end of the file")
        );

        let intact = TwoBitReference::open(Box::new(Cursor::new(valid.clone()))).unwrap();
        assert_eq!(valid.len(), 43);
        assert_eq!(intact.fetch("chrA", 0, 8).unwrap(), b"ACGTACGT");
    }

    /// Found by the mutation sweep in `lib.rs` (2bit, seed 0x165667B19E3779F9, iteration
    /// 179), minimised here. Three unknown-base runs of which the middle one is forged to
    /// cover the rest of the sequence: the runs were then no longer ordered by their ends,
    /// the bisection in `fetch` started at a run ending before the request, and the slice
    /// bounds underflowed.
    #[test]
    fn overlapping_unknown_runs_are_merged_instead_of_underflowing() {
        let mut file = encode_two_bit(&[("chrA", "NANACNACGTACGTAC")], false);
        // Record at byte 25: length, run count (3), three starts (0, 2, 5), three sizes.
        let sizes = 25 + 4 + 4 + 12;
        assert_eq!(file[sizes + 4..sizes + 8], 1_u32.to_le_bytes());
        file[sizes + 4..sizes + 8].copy_from_slice(&0x7fff_ffff_u32.to_le_bytes());
        assert_eq!(
            file,
            [
                0x43, 0x27, 0x41, 0x1a, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 4, b'c', b'h', b'r',
                b'A', 25, 0, 0, 0, 16, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 5, 0, 0, 0, 1,
                0, 0, 0, 0xff, 0xff, 0xff, 0x7f, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x22, 0x49,
                0xc9, 0xc9,
            ]
        );
        for soft_masked in [false, true] {
            let reader = Box::new(Cursor::new(file.clone()));
            let reference = if soft_masked {
                TwoBitReference::open_soft_masked(reader)
            } else {
                TwoBitReference::open(reader)
            }
            .unwrap();
            // The forged run reaches the end of the sequence: unknown from base 2 onwards.
            assert_eq!(reference.fetch("chrA", 8, 16).unwrap(), b"NNNNNNNN");
            assert_eq!(reference.fetch("chrA", 0, 16).unwrap(), b"NANNNNNNNNNNNNNN");
            assert_eq!(reference.fetch("chrA", 1, 2).unwrap(), b"A");
        }
    }

    #[test]
    fn every_subrange_matches_the_encoded_sequence_in_both_byte_orders() {
        for big_endian in [false, true] {
            let file = encode_two_bit(&[("chrA", FIRST), ("chrB", SECOND)], big_endian);
            let reference = TwoBitReference::open(Box::new(Cursor::new(file))).unwrap();
            assert_eq!(
                reference.sequence_names().collect::<Vec<_>>(),
                ["chrA", "chrB"]
            );
            for (name, sequence) in [("chrA", FIRST), ("chrB", SECOND)] {
                let expected = sequence.to_ascii_uppercase();
                assert_eq!(
                    reference.sequence_length(name).unwrap(),
                    expected.len() as u64
                );
                for start in 0..expected.len() {
                    for end in start + 1..=expected.len() {
                        assert_eq!(
                            reference.fetch(name, start as u64, end as u64).unwrap(),
                            expected.as_bytes()[start..end],
                            "{name}:{start}-{end}"
                        );
                    }
                }
            }
        }
    }

    /// Soft-masking is reported on request: every subrange then equals the FASTA text the
    /// file was encoded from, lower-case runs (including masked `n`) preserved.
    #[test]
    fn soft_masking_is_reported_in_lower_case_when_asked_for() {
        const MASKED: &str = "acgtACGTnnNNacgtNNnnACGTTTGACCAgattacaGATTACAnnnnCa";
        for big_endian in [false, true] {
            let file = encode_two_bit(&[("chrA", MASKED), ("chrB", SECOND)], big_endian);
            let masked =
                TwoBitReference::open_soft_masked(Box::new(Cursor::new(file.clone()))).unwrap();
            let plain = TwoBitReference::open(Box::new(Cursor::new(file))).unwrap();
            for (name, sequence) in [("chrA", MASKED), ("chrB", SECOND)] {
                for start in 0..sequence.len() {
                    for end in start + 1..=sequence.len() {
                        let fetched = masked.fetch(name, start as u64, end as u64).unwrap();
                        assert_eq!(
                            fetched,
                            sequence.as_bytes()[start..end],
                            "{name}:{start}-{end}"
                        );
                        // The default is unchanged: the same bases, upper case.
                        assert_eq!(
                            plain.fetch(name, start as u64, end as u64).unwrap(),
                            fetched.to_ascii_uppercase(),
                            "{name}:{start}-{end}"
                        );
                    }
                }
            }
        }
        // A forged mask count is rejected before its tables are reserved.
        let mut forged = encode_two_bit(&[("chrA", "ACGTACGT")], false);
        // Record at byte 25: length, unknown-run count (0), then the mask-run count.
        forged[25 + 8..25 + 12].copy_from_slice(&60_000_000_u32.to_le_bytes());
        let reference = TwoBitReference::open_soft_masked(Box::new(Cursor::new(forged))).unwrap();
        assert!(
            reference
                .fetch("chrA", 0, 4)
                .unwrap_err()
                .to_string()
                .contains("soft-mask run table")
        );
    }

    #[test]
    fn invalid_files_and_ranges_are_rejected() {
        assert!(
            TwoBitReference::open(Box::new(Cursor::new(b"not a 2bit file!".to_vec()))).is_err()
        );
        let file = encode_two_bit(&[("chrA", FIRST)], false);
        let reference = TwoBitReference::open(Box::new(Cursor::new(file))).unwrap();
        assert!(reference.fetch("chrA", 0, FIRST.len() as u64 + 1).is_err());
        assert!(reference.fetch("chrA", 5, 5).is_err());
        assert!(reference.fetch("chrZ", 0, 1).is_err());
    }

    #[test]
    fn chrom_sizes_tables_parse() {
        assert_eq!(
            parse_chrom_sizes(b"chr1\t248956422\n# note\nchr2 242193529\n").unwrap(),
            [
                ("chr1".to_owned(), 248_956_422),
                ("chr2".to_owned(), 242_193_529)
            ]
        );
        assert!(parse_chrom_sizes(b"chr1\n").is_err());
    }
}
