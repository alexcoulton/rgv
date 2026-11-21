//! Loads a plain-text SAM file, or a BAM file without an index (sorted or not), into an
//! in-memory coordinate-sorted BAM with a BAI built alongside (PROD-10).
//!
//! The result is read by the same decoder as every other BAM, so regional queries, positioned
//! streams, resume and the index overview behave as they do for an indexed file. Nothing is
//! written to disk. The whole file has to be read and sorted first, so its size is capped.

use std::{
    fs::File,
    io::{self, BufReader, Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
};

use noodles::{
    bam, bgzf,
    core::Position,
    csi::binning_index::{Indexer, index::reference_sequence::bin::Chunk},
    sam::{self, alignment::io::Write as _},
};

use super::bam_record_placement;
use crate::AlignmentError;

/// Largest SAM or unindexed BAM file loaded into memory, in bytes. The limit applies to the
/// file on disk and, separately, to the BAM-encoded size of its records (a BAM file inflates
/// to several times its size). Anything larger has to be sorted and indexed.
pub const MAX_IN_MEMORY_ALIGNMENT_BYTES: u64 = 256 * 1024 * 1024;

/// Uncompressed bytes per BGZF block of the in-memory BAM. A record is not split across
/// blocks unless it is larger than a block, as in files written by `HTSlib`.
const BLOCK_BYTES: usize = 0xff00;

fn too_large(path: &Path, what: &str, bytes: u64, cap: u64) -> AlignmentError {
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy());
    AlignmentError::Io(io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "{name} {what} {bytes} bytes; a SAM or unindexed BAM file is loaded into memory \
             only up to {cap} bytes ({} MiB). Sort and index it instead (`samtools sort -o \
             sorted.bam {name}`, then `samtools index sorted.bam`) and open sorted.bam",
            cap / (1024 * 1024)
        ),
    ))
}

/// BAM-encoded size of a record: the fixed fields, the name and the variable-length fields.
fn encoded_bytes(record: &bam::Record) -> u64 {
    let name = record.name().map_or(1, |name| name.len());
    (36 + name
        + 1
        + record.cigar().as_bytes().len()
        + record.sequence().as_bytes().len()
        + record.quality_scores().as_bytes().len()
        + record.data().as_bytes().len()) as u64
}

struct Loaded {
    header: sam::Header,
    records: Vec<bam::Record>,
}

fn read_bam(file: File, path: &Path, cap: u64) -> Result<Loaded, AlignmentError> {
    let mut reader = bam::io::Reader::new(file);
    let header = reader.read_header()?;
    let mut records = Vec::new();
    let mut bytes = 0_u64;
    let mut record = bam::Record::default();
    while reader.read_record(&mut record)? != 0 {
        bytes = bytes.saturating_add(encoded_bytes(&record));
        if bytes > cap {
            return Err(too_large(path, "holds more than", cap, cap));
        }
        records.push(std::mem::take(&mut record));
    }
    Ok(Loaded { header, records })
}

fn read_sam(file: File, path: &Path, cap: u64) -> Result<Loaded, AlignmentError> {
    let mut reader = sam::io::Reader::new(BufReader::new(file));
    let header = reader.read_header()?;
    let mut records = Vec::new();
    let mut bytes = 0_u64;
    let mut encoded = Vec::new();
    for result in reader.record_bufs(&header) {
        let parsed = result?;
        // Through the BAM encoding, so a record from SAM text is held (and later decoded)
        // exactly as the same record read from a BAM file.
        encoded.clear();
        bam::io::Writer::from(&mut encoded).write_alignment_record(&header, &parsed)?;
        bytes = bytes.saturating_add(encoded.len() as u64);
        if bytes > cap {
            return Err(too_large(path, "holds more than", cap, cap));
        }
        let mut record = bam::Record::default();
        bam::io::Reader::from(encoded.as_slice()).read_record(&mut record)?;
        records.push(record);
    }
    Ok(Loaded { header, records })
}

/// Reads `path` (SAM text or BAM, told apart by the leading bytes) and returns the bytes of a
/// coordinate-sorted BAM holding its records, with the index of those bytes.
///
/// Records are ordered by reference sequence and start; records with the same start keep the
/// order they had in the file. Unplaced records follow all placed ones.
pub(super) fn load_sorted_bam(
    path: &Path,
    cap: u64,
) -> Result<(Vec<u8>, bam::bai::Index), AlignmentError> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    if length > cap {
        return Err(too_large(path, "is", length, cap));
    }
    let mut magic = [0_u8; 4];
    let mut filled = 0;
    while filled < magic.len() {
        match file.read(&mut magic[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    file.seek(SeekFrom::Start(0))?;
    let Loaded { header, records } = if &magic[..filled] == b"CRAM" {
        return Err(AlignmentError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a CRAM file is not loaded into memory; index it (`samtools index`) and open it \
             with its .crai",
        )));
    } else if filled >= 2 && magic[..2] == [0x1f, 0x8b] {
        read_bam(file, path, cap)?
    } else {
        read_sam(file, path, cap)?
    };

    let mut placed = Vec::with_capacity(records.len());
    for record in records {
        let placement = bam_record_placement(&record)?;
        // Unplaced records sort after every reference sequence.
        let key = placement.map_or((u32::MAX, 0), |(contig, start, _)| (contig.get(), start));
        placed.push((key, placement, record));
    }
    placed.sort_by_key(|(key, _, _)| *key);

    let mut bgzf_writer = bgzf::io::writer::Builder::default()
        .set_compression_level(bgzf::io::writer::CompressionLevel::FAST)
        .build_from_writer(Vec::new());
    bam::io::Writer::from(&mut bgzf_writer).write_header(&header)?;
    bgzf_writer.flush()?;
    let mut indexer = Indexer::default();
    for (_, placement, record) in &placed {
        let staged = usize::from(bgzf_writer.virtual_position().uncompressed());
        let size = usize::try_from(encoded_bytes(record)).unwrap_or(usize::MAX);
        if staged > 0 && staged.saturating_add(size) > BLOCK_BYTES {
            bgzf_writer.flush()?;
        }
        let start = bgzf_writer.virtual_position();
        bam::io::Writer::from(&mut bgzf_writer).write_record(&header, record)?;
        let end = bgzf_writer.virtual_position();
        let context = placement
            .map(|(contig, start, end)| {
                let position = |zero_based: u64| {
                    usize::try_from(zero_based)
                        .ok()
                        .and_then(|value| Position::new(value.saturating_add(1)))
                        .ok_or(AlignmentError::CoordinateOverflow)
                };
                // One-based inclusive; a record consuming no reference base occupies one.
                Ok::<_, AlignmentError>((
                    contig.get() as usize,
                    position(start)?,
                    position(end.saturating_sub(1).max(start))?,
                    !record.flags().is_unmapped(),
                ))
            })
            .transpose()?;
        indexer.add_record(context, Chunk::new(start, end))?;
    }
    let bytes = bgzf_writer.finish()?;
    let index = indexer.build(header.reference_sequences().len());
    Ok((bytes, index))
}
