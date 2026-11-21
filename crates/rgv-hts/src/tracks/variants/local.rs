//! Opening a local VCF/BCF without ever handing `HTSlib` a file it mishandles.
//!
//! `rust-htslib` 1.0.1 does not check what `HTSlib` returns when it opens a variant file:
//!
//! * `bcf::IndexedReader::new` treats every return value of `bcf_sr_add_reader` as success
//!   (`>= 0`; the function returns 0 on failure) and then duplicates the header of the first
//!   reader. For a file that is not BGZF-compressed, has no loadable index or has a header
//!   `HTSlib` rejects, that header (or the reader array) is null and the process dies with
//!   SIGSEGV. A BCF whose index is missing survives the open and dies in the first `fetch`.
//! * `bcf::Reader::new` wraps the result of `bcf_hdr_read` without a null check, so a header
//!   `HTSlib` rejects is dereferenced by the first header accessor.
//!
//! Nothing here can make those calls safe after the fact, so every precondition is established
//! first, in Rust: [`open`] sniffs the compression and validates the header text, opens the
//! file through [`open_reader`] (which detects a null header), and only builds an indexed
//! reader when the file is BGZF-compressed and its index is known to load. Every other
//! readable VCF/BCF is loaded into memory by [`InMemoryVariantFile`].

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader, Read},
    path::Path,
};

use flate2::read::MultiGzDecoder;
use noodles::csi;
use rust_htslib::bcf::{self, Read as _};

use crate::tracks::TrackError;

/// Largest unindexed VCF/BCF loaded into memory, in bytes. The limit applies to the file on
/// disk and, for a compressed file, separately to its decompressed size. A larger file has to
/// be compressed with `bgzip` and indexed (`tabix -p vcf` or `bcftools index`).
pub const MAX_IN_MEMORY_VARIANT_BYTES: u64 = 128 * 1024 * 1024;

/// Largest number of records of an unindexed VCF/BCF loaded into memory.
pub const MAX_IN_MEMORY_VARIANT_RECORDS: usize = 2_000_000;

/// Largest VCF/BCF header validated before the file is handed to `HTSlib`.
const MAX_LOCAL_VARIANT_HEADER_BYTES: usize = 64 * 1024 * 1024;

/// Largest CSI index read to check that `HTSlib` will be able to load it.
const MAX_LOCAL_VARIANT_INDEX_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Compression {
    None,
    Gzip,
    Bgzf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Format {
    Vcf,
    Bcf,
}

/// A local variant file, opened the way its compression and index allow.
pub(super) enum LocalVariantFile {
    Indexed(bcf::IndexedReader),
    Memory(Box<InMemoryVariantFile>),
}

fn backend(error: impl std::fmt::Display) -> TrackError {
    TrackError::Backend(error.to_string())
}

/// Opens `path` for region queries: through its index when it is BGZF-compressed and has one
/// `HTSlib` can load, from memory otherwise.
pub(super) fn open(path: &Path) -> Result<LocalVariantFile, TrackError> {
    let (compression, format) = preflight(path)?;
    // Proves that `HTSlib` recognises the file as VCF or BCF and parses its header, which the
    // indexed reader assumes without checking.
    let reader = open_reader(path)?;
    if compression == Compression::Bgzf && index_is_loadable(path, format) {
        drop(reader);
        return bcf::IndexedReader::from_path(path)
            .map(LocalVariantFile::Indexed)
            .map_err(backend);
    }
    InMemoryVariantFile::load(
        path,
        reader,
        compression,
        MAX_IN_MEMORY_VARIANT_BYTES,
        MAX_IN_MEMORY_VARIANT_RECORDS,
    )
    .map(|file| LocalVariantFile::Memory(Box::new(file)))
}

/// Opens a VCF/BCF for sequential reading, refusing a file whose header `HTSlib` could not
/// read. `rust-htslib` returns such a reader as if it were usable.
pub(super) fn open_reader(path: &Path) -> Result<bcf::Reader, TrackError> {
    let reader = bcf::Reader::from_path(path).map_err(backend)?;
    if crate::htslib_guard::wraps_null_pointer(reader.header()) {
        return Err(TrackError::Backend(format!(
            "{} does not have a readable VCF/BCF header",
            path.display()
        )));
    }
    Ok(reader)
}

/// The contig a record lies on, by the header the record was read with. `HTSlib` adds a
/// contig no header line declares to that header as it meets it, so a copy of the header
/// taken earlier does not know it (and `HeaderView::rid2name` reads one entry past the end of
/// the dictionary for the first such id).
pub(super) fn record_contig(record: &bcf::Record) -> Option<String> {
    let rid = record.rid()?;
    let header = record.header();
    if rid >= header.contig_count() {
        return None;
    }
    header
        .rid2name(rid)
        .ok()
        .map(|name| String::from_utf8_lossy(name).into_owned())
}

/// Establishes what the readers assume: `path` is a readable regular file whose header is
/// bounded and valid UTF-8 (`rust-htslib` unwraps the conversion of every header value).
/// Returns the compression as `HTSlib` detects it and the format the content announces.
fn preflight(path: &Path) -> Result<(Compression, Format), TrackError> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(TrackError::Backend(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let mut prefix = Vec::with_capacity(18);
    File::open(path)?.take(18).read_to_end(&mut prefix)?;
    // The test of `hts_detect_format`.
    let compression = if prefix.starts_with(&[0x1f, 0x8b]) {
        if prefix.len() >= 18 && prefix[3] & 4 != 0 && &prefix[12..16] == b"BC\x02\x00" {
            Compression::Bgzf
        } else {
            Compression::Gzip
        }
    } else {
        Compression::None
    };
    let file = File::open(path)?;
    let mut reader: Box<dyn BufRead> = match compression {
        Compression::None => Box::new(BufReader::new(file)),
        Compression::Gzip | Compression::Bgzf => {
            Box::new(BufReader::new(MultiGzDecoder::new(file)))
        }
    };
    let not_a_variant_file =
        || TrackError::Backend(format!("{} is not a VCF or BCF file", path.display()));
    let unreadable =
        |error: io::Error| TrackError::Backend(format!("cannot read {}: {error}", path.display()));
    let mut magic = [0_u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|_| not_a_variant_file())?;
    let too_large = || {
        TrackError::ResourceLimit(format!(
            "the header of {} exceeds the supported size",
            path.display()
        ))
    };
    let (format, header) = if &magic == b"BCF\x02" {
        let mut rest = [0_u8; 5];
        reader
            .read_exact(&mut rest)
            .map_err(|_| not_a_variant_file())?;
        let length = u32::from_le_bytes([rest[1], rest[2], rest[3], rest[4]]) as usize;
        if length > MAX_LOCAL_VARIANT_HEADER_BYTES {
            return Err(too_large());
        }
        let mut text = Vec::new();
        reader
            .take(length as u64)
            .read_to_end(&mut text)
            .map_err(unreadable)?;
        if text.len() != length {
            return Err(not_a_variant_file());
        }
        (Format::Bcf, text)
    } else if magic[0] == b'#' {
        let mut text = magic.to_vec();
        loop {
            let limit = (MAX_LOCAL_VARIANT_HEADER_BYTES + 1).saturating_sub(text.len()) as u64;
            (&mut reader)
                .take(limit)
                .read_until(b'\n', &mut text)
                .map_err(unreadable)?;
            if text.len() > MAX_LOCAL_VARIANT_HEADER_BYTES {
                return Err(too_large());
            }
            if reader.fill_buf().map_err(unreadable)?.first() != Some(&b'#') {
                break;
            }
        }
        (Format::Vcf, text)
    } else {
        return Err(not_a_variant_file());
    };
    if std::str::from_utf8(&header).is_err() {
        return Err(TrackError::Backend(format!(
            "the header of {} is not valid UTF-8",
            path.display()
        )));
    }
    Ok((compression, format))
}

/// Whether `HTSlib` will load an index for the BGZF-compressed `path`. It must not be asked
/// to open the file through the index otherwise: see the module documentation.
fn index_is_loadable(path: &Path, format: Format) -> bool {
    match format {
        // `tbx::Reader` loads the index with the call the indexed reader makes
        // (`tbx_index_load`) and checks its result. It unwraps the UTF-8 conversion of the
        // leading comment lines, which `preflight` has validated. It is not asked when no
        // index file exists, because it leaks its file handle when the load fails.
        Format::Vcf => {
            index_candidates(path, &["csi", "tbi"])
                .iter()
                .any(|candidate| std::fs::metadata(candidate).is_ok())
                && rust_htslib::tbx::Reader::from_path(path).is_ok()
        }
        Format::Bcf => bcf_index_path(path).is_some_and(|index| {
            let mut bytes = Vec::new();
            File::open(index)
                .and_then(|file| {
                    file.take(MAX_LOCAL_VARIANT_INDEX_BYTES + 1)
                        .read_to_end(&mut bytes)
                })
                .is_ok_and(|read| read as u64 <= MAX_LOCAL_VARIANT_INDEX_BYTES)
                && csi::io::Reader::new(io::Cursor::new(bytes))
                    .read_index()
                    .is_ok()
        }),
    }
}

/// Where `hts_idx_check_local` looks for an index of each extension, in its order:
/// `<path>.<extension>`, then the path with everything after its last `.` replaced.
fn index_candidates(path: &Path, extensions: &[&str]) -> Vec<String> {
    let Some(raw) = path.to_str() else {
        return Vec::new();
    };
    let stem = raw.rfind('.').filter(|dot| *dot > 0).map(|dot| &raw[..dot]);
    extensions
        .iter()
        .flat_map(|extension| {
            std::iter::once(format!("{raw}.{extension}"))
                .chain(stem.map(|stem| format!("{stem}.{extension}")))
        })
        .collect()
}

/// The index `bcf_index_load` picks: the first CSI candidate that exists. `HTSlib` does not
/// fall back to the next one when that file is unreadable.
fn bcf_index_path(path: &Path) -> Option<String> {
    index_candidates(path, &["csi"])
        .into_iter()
        .find(|candidate| std::fs::metadata(candidate).is_ok())
}

fn too_large(path: &Path, reason: &str) -> TrackError {
    TrackError::Io(io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!(
            "{} has no usable index and {reason}, more than an unindexed VCF/BCF may have to \
             be loaded into memory; compress it with bgzip and index it (tabix -p vcf, or \
             bcftools index)",
            path.display()
        ),
    ))
}

struct StoredRecord {
    /// 0-based start.
    start: i64,
    /// 0-based exclusive end, beyond `start`.
    end: i64,
    record: bcf::Record,
}

#[derive(Default)]
struct ContigRecords {
    /// Sorted by `start`; records with the same start keep their file order.
    records: Vec<StoredRecord>,
    /// `max_end[i]` is the largest `end` among `records[..=i]`.
    max_end: Vec<i64>,
}

/// A whole VCF/BCF held in memory, sorted by position, for a file that cannot be queried
/// through an index: plain or gzip-compressed text, or a BGZF file without a loadable index.
/// The records need not be sorted in the file, nor their contigs declared in the header.
pub(super) struct InMemoryVariantFile {
    /// Kept for its header, which every stored record shares.
    reader: bcf::Reader,
    contigs: Vec<ContigRecords>,
    by_name: HashMap<String, usize>,
}

impl InMemoryVariantFile {
    fn load(
        path: &Path,
        mut reader: bcf::Reader,
        compression: Compression,
        maximum_bytes: u64,
        maximum_records: usize,
    ) -> Result<Self, TrackError> {
        let megabytes = maximum_bytes / (1024 * 1024);
        if std::fs::metadata(path)?.len() > maximum_bytes {
            return Err(too_large(path, &format!("is larger than {megabytes} MiB")));
        }
        if compression != Compression::None {
            let inflated = io::copy(
                &mut MultiGzDecoder::new(File::open(path)?).take(maximum_bytes.saturating_add(1)),
                &mut io::sink(),
            )
            .map_err(|error| {
                TrackError::Backend(format!("cannot decompress {}: {error}", path.display()))
            })?;
            if inflated > maximum_bytes {
                return Err(too_large(
                    path,
                    &format!("decompresses to more than {megabytes} MiB"),
                ));
            }
        }
        let mut contigs = Vec::<ContigRecords>::new();
        let mut by_name = HashMap::<String, usize>::new();
        for (loaded, result) in reader.records().enumerate() {
            let record = result.map_err(|error| {
                TrackError::Backend(format!(
                    "{}: record {} cannot be read: {error}",
                    path.display(),
                    loaded.saturating_add(1)
                ))
            })?;
            if loaded == maximum_records {
                return Err(too_large(
                    path,
                    &format!("holds more than {maximum_records} records"),
                ));
            }
            let Some(contig) = record_contig(&record) else {
                continue;
            };
            let start = record.pos();
            let end = record.end().max(start.saturating_add(1));
            let next = contigs.len();
            let slot = *by_name.entry(contig).or_insert(next);
            if slot == next {
                contigs.push(ContigRecords::default());
            }
            contigs[slot]
                .records
                .push(StoredRecord { start, end, record });
        }
        for contig in &mut contigs {
            contig.records.sort_by_key(|stored| stored.start);
            let mut furthest = i64::MIN;
            contig.max_end = contig
                .records
                .iter()
                .map(|stored| {
                    furthest = furthest.max(stored.end);
                    furthest
                })
                .collect();
        }
        Ok(Self {
            reader,
            contigs,
            by_name,
        })
    }

    pub(super) fn header(&self) -> &bcf::header::HeaderView {
        self.reader.header()
    }

    /// Records on `contig` overlapping the 0-based half-open `[start, end)`, by position. A
    /// contig the file does not mention has none.
    pub(super) fn overlapping(
        &self,
        contig: &str,
        start: u64,
        end: u64,
    ) -> impl Iterator<Item = &bcf::Record> {
        let start = i64::try_from(start).unwrap_or(i64::MAX);
        let end = i64::try_from(end).unwrap_or(i64::MAX);
        let records = self
            .by_name
            .get(contig)
            .filter(|_| start < end)
            .map_or(&[][..], |slot| {
                let contig = &self.contigs[*slot];
                let last = contig.records.partition_point(|stored| stored.start < end);
                let first = contig.max_end[..last].partition_point(|furthest| *furthest <= start);
                &contig.records[first..last]
            });
        records
            .iter()
            .filter(move |stored| stored.end > start)
            .map(|stored| &stored.record)
    }

    /// Every record, contig by contig in order of first appearance, by position within one.
    pub(super) fn records(&self) -> impl Iterator<Item = &bcf::Record> {
        self.contigs
            .iter()
            .flat_map(|contig| contig.records.iter().map(|stored| &stored.record))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    const VCF: &str = "##fileformat=VCFv4.2\n\
        #CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n\
        chr1\t100\ta\tA\tT\t.\t.\t.\n\
        chr1\t200\tb\tA\tT\t.\t.\t.\n\
        chr1\t300\tc\tA\tT\t.\t.\t.\n";

    fn load(
        path: &Path,
        maximum_bytes: u64,
        maximum_records: usize,
    ) -> Result<InMemoryVariantFile, TrackError> {
        let (compression, _) = preflight(path)?;
        InMemoryVariantFile::load(
            path,
            open_reader(path)?,
            compression,
            maximum_bytes,
            maximum_records,
        )
    }

    fn is_too_large(result: Result<InMemoryVariantFile, TrackError>) -> bool {
        match result {
            Err(TrackError::Io(error)) => {
                assert!(error.to_string().contains("bgzip"), "{error}");
                error.kind() == io::ErrorKind::FileTooLarge
            }
            _ => false,
        }
    }

    #[test]
    fn a_readable_header_is_not_reported_missing() {
        // Guards the `Debug` rendering `wraps_null_pointer` reads: were it to change, every
        // variant file would be refused and this fails.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ok.vcf");
        std::fs::write(&path, VCF).unwrap();
        let reader = bcf::Reader::from_path(&path).unwrap();
        assert!(!crate::htslib_guard::wraps_null_pointer(reader.header()));
    }

    #[test]
    fn a_header_htslib_rejects_is_reported_missing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("no-column-line.vcf");
        std::fs::write(&path, "##fileformat=VCFv4.2\n##contig=<ID=chr1>\n").unwrap();
        let reader = bcf::Reader::from_path(&path).unwrap();
        assert!(crate::htslib_guard::wraps_null_pointer(reader.header()));
        assert!(open_reader(&path).is_err());
    }

    #[test]
    fn an_indexed_bgzf_file_is_queried_through_its_index_and_any_other_from_memory() {
        let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data");
        for name in [
            "manual-suite/data/variants/cohort.vcf.gz",
            "manual-suite/data/variants/cohort.bcf",
            "demo-tracks/variants.bcf",
        ] {
            assert!(
                matches!(open(&data.join(name)), Ok(LocalVariantFile::Indexed(_))),
                "{name}"
            );
        }
        // The same bytes without their index.
        let directory = tempfile::tempdir().unwrap();
        for name in ["cohort.vcf.gz", "cohort.bcf"] {
            let copy = directory.path().join(name);
            std::fs::copy(data.join("manual-suite/data/variants").join(name), &copy).unwrap();
            assert!(
                matches!(open(&copy), Ok(LocalVariantFile::Memory(_))),
                "{name}"
            );
        }
        let plain = directory.path().join("plain.vcf");
        std::fs::write(&plain, VCF).unwrap();
        assert!(matches!(open(&plain), Ok(LocalVariantFile::Memory(_))));
    }

    #[test]
    fn files_beyond_the_in_memory_limits_are_refused_with_advice() {
        let directory = tempfile::tempdir().unwrap();
        let plain = directory.path().join("plain.vcf");
        std::fs::write(&plain, VCF).unwrap();
        let size = VCF.len() as u64;
        assert!(load(&plain, size, 3).is_ok());
        assert!(is_too_large(load(&plain, size - 1, 3)));
        assert!(is_too_large(load(&plain, size, 2)));

        // A compressed file is measured by what it inflates to.
        let padded = format!("{VCF}{}", "chr1\t400\td\tA\tT\t.\t.\t.\n".repeat(2000));
        let gzip = directory.path().join("padded.vcf.gz");
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(padded.as_bytes()).unwrap();
        std::fs::write(&gzip, encoder.finish().unwrap()).unwrap();
        let compressed = std::fs::metadata(&gzip).unwrap().len();
        assert!(compressed * 4 < padded.len() as u64);
        assert!(is_too_large(load(&gzip, compressed * 2, usize::MAX)));
        assert!(load(&gzip, padded.len() as u64, usize::MAX).is_ok());
    }

    #[test]
    fn overlap_selection_finds_long_records_behind_short_ones() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("spans.vcf");
        std::fs::write(
            &path,
            "##fileformat=VCFv4.2\n\
             ##INFO=<ID=END,Number=1,Type=Integer,Description=\"End\">\n\
             #CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n\
             chr1\t300\tshort\tA\tT\t.\t.\t.\n\
             chr1\t100\tlong\tN\t<DEL>\t.\t.\tEND=1000\n\
             chr1\t200\tmid\tACGT\tA\t.\t.\t.\n",
        )
        .unwrap();
        let file = load(&path, u64::MAX, usize::MAX).unwrap();
        let ids = |start, end| {
            file.overlapping("chr1", start, end)
                .map(|record| String::from_utf8(record.id()).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(0, 99), Vec::<String>::new());
        assert_eq!(ids(0, 100), ["long"]);
        assert_eq!(ids(0, 2000), ["long", "mid", "short"]);
        assert_eq!(ids(203, 299), ["long"]);
        assert_eq!(ids(202, 203), ["long", "mid"]);
        assert_eq!(ids(999, 1000), ["long"]);
        assert_eq!(ids(1000, 2000), Vec::<String>::new());
        assert_eq!(ids(500, 500), Vec::<String>::new());
        assert_eq!(file.overlapping("chr2", 0, 2000).count(), 0);
        assert_eq!(file.records().count(), 3);
    }

    #[test]
    fn bcf_index_lookup_follows_htslib() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("calls.bcf");
        assert_eq!(bcf_index_path(&path), None);
        let replaced = directory.path().join("calls.csi");
        std::fs::write(&replaced, b"").unwrap();
        assert_eq!(bcf_index_path(&path).as_deref(), replaced.to_str());
        let appended = directory.path().join("calls.bcf.csi");
        std::fs::write(&appended, b"").unwrap();
        assert_eq!(bcf_index_path(&path).as_deref(), appended.to_str());
    }
}
