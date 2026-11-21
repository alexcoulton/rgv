use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::Mutex,
};

use rgv_core::{GenomicInterval, ReferenceDictionary};
use rust_htslib::faidx;

use crate::{
    AlignmentError, HttpRangeConfig, HttpRangeReader, RemoteCancellation, RemoteIoStats,
    RemoteTransferBudget, associated_remote_url, read_remote_object_bounded,
    two_bit::TwoBitReference,
};

const MAX_REMOTE_FASTA_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_REMOTE_FASTA_INDEX_ENTRIES: usize = 1_000_000;
const MAX_REMOTE_FASTA_INDEX_LINE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct FastaOpenOptions {
    pub remote_index_url: Option<String>,
    pub remote_range: HttpRangeConfig,
    /// Optional UCSC `chrom.sizes` table (local path or URL) for a `.2bit` reference. It supplies
    /// every sequence length in one read; without it each length is read from its own record,
    /// which is slow for a remote file with many sequences.
    pub chrom_sizes: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceSlice {
    pub interval: GenomicInterval,
    pub bases: Vec<u8>,
}

impl ReferenceSlice {
    pub fn base_at(&self, genomic_position: u64) -> Option<u8> {
        if !self.interval.contains(genomic_position) {
            return None;
        }
        let offset = usize::try_from(genomic_position - self.interval.start()).ok()?;
        self.bases.get(offset).copied()
    }
}

pub struct FastaReference {
    reader: FastaReader,
    dictionary: ReferenceDictionary,
}

enum FastaReader {
    Local(faidx::Reader),
    Remote(Box<RemoteFastaReference>),
    TwoBit(Box<TwoBitReference>),
}

#[derive(Clone, Debug)]
struct FastaIndexEntry {
    name: String,
    length: u64,
    offset: u64,
    line_bases: u64,
    line_width: u64,
}

struct RemoteFastaReference {
    reader: Mutex<HttpRangeReader>,
    entries: Vec<FastaIndexEntry>,
    index_stats: RemoteIoStats,
}

impl FastaReference {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AlignmentError> {
        Self::open_with_options(path, &FastaOpenOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: &FastaOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        let is_remote = raw.starts_with("http://") || raw.starts_with("https://");
        if location_has_extension(&raw, is_remote, "2bit") {
            return Self::open_two_bit(&raw, is_remote, options);
        }
        if is_remote {
            let remote = RemoteFastaReference::open(&raw, options)?;
            let dictionary = crate::local::header_reference_dictionary(
                remote
                    .entries
                    .iter()
                    .map(|entry| (entry.name.clone(), entry.length)),
            )?;
            Ok(Self {
                reader: FastaReader::Remote(Box::new(remote)),
                dictionary,
            })
        } else {
            let reader = faidx::Reader::from_path(path)?;
            let dictionary = crate::local::header_reference_dictionary(
                reader
                    .seq_names()?
                    .into_iter()
                    .map(|name| {
                        let length = reader.fetch_seq_len(&name);
                        (name, length)
                    })
                    .collect::<Vec<_>>(),
            )?;
            Ok(Self {
                reader: FastaReader::Local(reader),
                dictionary,
            })
        }
    }

    fn open_two_bit(
        location: &str,
        is_remote: bool,
        options: &FastaOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let reader: Box<dyn crate::two_bit::ReadSeek> = if is_remote {
            Box::new(HttpRangeReader::open(
                location,
                options.remote_range.clone(),
            )?)
        } else {
            Box::new(std::io::BufReader::new(std::fs::File::open(location)?))
        };
        let two_bit = TwoBitReference::open(reader)?;
        // The reference's companion slot (the FASTA index for a FASTA) carries the sizes table
        // for a 2bit, so sessions and launch options need no separate setting.
        let known_sizes = options
            .chrom_sizes
            .as_deref()
            .or(options.remote_index_url.as_deref())
            .and_then(|sizes| {
                // An unreadable or stale table only costs speed: lengths are then read from the
                // sequence records themselves.
                let bytes = if sizes.starts_with("http://") || sizes.starts_with("https://") {
                    read_remote_object_bounded(
                        sizes,
                        options.remote_range.clone(),
                        MAX_REMOTE_FASTA_INDEX_BYTES,
                    )
                    .ok()?
                    .0
                } else {
                    std::fs::read(sizes).ok()?
                };
                crate::two_bit::parse_chrom_sizes(&bytes).ok()
            })
            .map(|sizes| {
                sizes
                    .into_iter()
                    .collect::<std::collections::HashMap<_, _>>()
            });
        let mut contigs = Vec::new();
        for name in two_bit.sequence_names() {
            let length = match known_sizes.as_ref().and_then(|sizes| sizes.get(name)) {
                Some(length) => *length,
                None => two_bit.sequence_length(name)?,
            };
            contigs.push((name.to_owned(), length));
        }
        Ok(Self {
            dictionary: crate::local::header_reference_dictionary(contigs)?,
            reader: FastaReader::TwoBit(Box::new(two_bit)),
        })
    }

    pub fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    pub fn fetch(&self, interval: GenomicInterval) -> Result<Vec<u8>, AlignmentError> {
        let contig = self
            .dictionary
            .get(interval.contig())
            .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
        self.fetch_contig_interval(contig.name(), interval.start(), interval.end())
    }

    pub fn fetch_slice(&self, interval: GenomicInterval) -> Result<ReferenceSlice, AlignmentError> {
        Ok(ReferenceSlice {
            interval,
            bases: self.fetch(interval)?,
        })
    }

    /// Fetches an alignment interval by contig name rather than assuming the
    /// alignment and FASTA dictionaries use identical numeric IDs or order.
    /// A partial FASTA legitimately returns no slice for an absent contig.
    pub fn fetch_slice_by_name(
        &self,
        contig_name: &str,
        interval: GenomicInterval,
    ) -> Result<Option<ReferenceSlice>, AlignmentError> {
        let Some(contig_id) = self.dictionary.id_by_name(contig_name) else {
            return Ok(None);
        };
        let contig = self
            .dictionary
            .get(contig_id)
            .ok_or(AlignmentError::UnknownContig(contig_id.get()))?;
        let bases = self.fetch_contig_interval(contig.name(), interval.start(), interval.end())?;
        Ok(Some(ReferenceSlice { interval, bases }))
    }

    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        match &self.reader {
            FastaReader::Local(_) | FastaReader::TwoBit(_) => None,
            FastaReader::Remote(remote) => Some(remote.remote_io_stats()),
        }
    }

    pub fn set_remote_cancellation(&self, cancellation: Option<RemoteCancellation>) {
        if let FastaReader::Remote(remote) = &self.reader
            && let Ok(mut reader) = remote.reader.lock()
        {
            reader.set_cancellation(cancellation);
        }
    }

    pub fn set_remote_transfer_budget(&self, budget: Option<RemoteTransferBudget>) {
        if let FastaReader::Remote(remote) = &self.reader
            && let Ok(mut reader) = remote.reader.lock()
        {
            reader.set_transfer_budget(budget);
        }
    }

    fn fetch_contig_interval(
        &self,
        contig_name: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, AlignmentError> {
        match &self.reader {
            FastaReader::Local(reader) => {
                let start =
                    usize::try_from(start).map_err(|_| AlignmentError::CoordinateOverflow)?;
                let inclusive_end = usize::try_from(end.saturating_sub(1))
                    .map_err(|_| AlignmentError::CoordinateOverflow)?;
                Ok(reader.fetch_seq(contig_name, start, inclusive_end)?)
            }
            FastaReader::Remote(remote) => remote.fetch(contig_name, start, end),
            FastaReader::TwoBit(two_bit) => two_bit.fetch(contig_name, start, end),
        }
    }
}

/// Whether a local path or URL (ignoring its query string and fragment) ends in `extension`.
fn location_has_extension(location: &str, is_remote: bool, extension: &str) -> bool {
    let path = if is_remote {
        location.split(['?', '#']).next().unwrap_or(location)
    } else {
        location
    };
    Path::new(path)
        .extension()
        .is_some_and(|actual| actual.eq_ignore_ascii_case(extension))
}

impl RemoteFastaReference {
    fn open(url: &str, options: &FastaOpenOptions) -> Result<Self, AlignmentError> {
        let index_url = options
            .remote_index_url
            .clone()
            .map_or_else(|| associated_remote_url(url, ".fai"), Ok)?;
        let (index, index_stats) = read_remote_object_bounded(
            &index_url,
            options.remote_range.clone(),
            MAX_REMOTE_FASTA_INDEX_BYTES,
        )?;
        let entries = parse_fasta_index(&index)?;
        let reader = HttpRangeReader::open(url, options.remote_range.clone())?;
        for entry in &entries {
            if entry.length > 0 && entry.offset >= reader.len() {
                return Err(AlignmentError::InvalidFastaIndex(format!(
                    "{} starts beyond the remote FASTA object",
                    entry.name
                )));
            }
        }
        Ok(Self {
            reader: Mutex::new(reader),
            entries,
            index_stats,
        })
    }

    fn fetch(&self, name: &str, start: u64, end: u64) -> Result<Vec<u8>, AlignmentError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| AlignmentError::InvalidFastaIndex(format!("missing contig {name}")))?;
        if start >= end || end > entry.length {
            return Err(AlignmentError::ReferenceIntervalOutOfBounds {
                contig: name.to_owned(),
                start,
                end,
                length: entry.length,
            });
        }
        let expected =
            usize::try_from(end - start).map_err(|_| AlignmentError::CoordinateOverflow)?;
        let mut bases = Vec::with_capacity(expected);
        let mut position = start;
        let mut reader = self.reader.lock().map_err(|_| {
            AlignmentError::InvalidFastaIndex("remote FASTA cache lock was poisoned".to_owned())
        })?;
        while position < end {
            let line = position / entry.line_bases;
            let column = position % entry.line_bases;
            let count = (entry.line_bases - column).min(end - position);
            let byte_offset = entry
                .offset
                .checked_add(line.saturating_mul(entry.line_width))
                .and_then(|offset| offset.checked_add(column))
                .ok_or(AlignmentError::CoordinateOverflow)?;
            reader.seek(SeekFrom::Start(byte_offset))?;
            let old_len = bases.len();
            bases.resize(old_len.saturating_add(count as usize), 0);
            reader.read_exact(&mut bases[old_len..])?;
            position = position.saturating_add(count);
        }
        Ok(bases)
    }

    fn remote_io_stats(&self) -> RemoteIoStats {
        self.reader.lock().map_or(self.index_stats, |reader| {
            combine_remote_stats(self.index_stats, reader.stats())
        })
    }
}

fn combine_remote_stats(left: RemoteIoStats, right: RemoteIoStats) -> RemoteIoStats {
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

fn parse_fasta_index(bytes: &[u8]) -> Result<Vec<FastaIndexEntry>, AlignmentError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| AlignmentError::InvalidFastaIndex("index is not UTF-8".to_owned()))?;
    let mut entries = Vec::new();
    // Hashed membership: scanning `entries` for every line is quadratic on assemblies with
    // hundreds of thousands of scaffolds.
    let mut names = std::collections::HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_REMOTE_FASTA_INDEX_LINE_BYTES {
            return Err(AlignmentError::InvalidFastaIndex(format!(
                "line {} exceeds the bounded line length",
                line_index + 1
            )));
        }
        if entries.len() == MAX_REMOTE_FASTA_INDEX_ENTRIES {
            return Err(AlignmentError::InvalidFastaIndex(format!(
                "index exceeds {MAX_REMOTE_FASTA_INDEX_ENTRIES} entries"
            )));
        }
        let mut fields = line.split('\t');
        let name = fields.next().unwrap_or_default();
        let parse_u64 = |value: Option<&str>, field: &str| {
            value
                .ok_or_else(|| {
                    AlignmentError::InvalidFastaIndex(format!(
                        "line {} is missing {field}",
                        line_index + 1
                    ))
                })?
                .parse::<u64>()
                .map_err(|_| {
                    AlignmentError::InvalidFastaIndex(format!(
                        "line {} has an invalid {field}",
                        line_index + 1
                    ))
                })
        };
        let length = parse_u64(fields.next(), "sequence length")?;
        let offset = parse_u64(fields.next(), "byte offset")?;
        let line_bases = parse_u64(fields.next(), "line base count")?;
        let line_width = parse_u64(fields.next(), "line byte width")?;
        if name.is_empty() || name.len() > 1024 || line_bases == 0 || line_width < line_bases {
            return Err(AlignmentError::InvalidFastaIndex(format!(
                "line {} has invalid FASTA geometry",
                line_index + 1
            )));
        }
        if !names.insert(name) {
            return Err(AlignmentError::InvalidFastaIndex(format!(
                "duplicate contig {name}"
            )));
        }
        entries.push(FastaIndexEntry {
            name: name.to_owned(),
            length,
            offset,
            line_bases,
            line_width,
        });
    }
    if entries.is_empty() {
        return Err(AlignmentError::InvalidFastaIndex(
            "index contains no sequences".to_owned(),
        ));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    use rgv_core::ContigId;
    use tempfile::tempdir;
    use tiny_http::{Header, Response, Server, StatusCode};

    use super::*;

    #[test]
    fn two_bit_references_open_behind_the_fasta_interface() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("genome.2bit");
        let sequences = [
            ("chr1", "ACGTNNNNACGTACGTTTGACCA"),
            ("chrM", "GATTACAGATTACA"),
        ];
        fs::write(
            &path,
            crate::two_bit::tests::encode_two_bit(&sequences, false),
        )
        .unwrap();

        let reference = FastaReference::open(&path).unwrap();
        let dictionary = reference.reference_dictionary();
        assert_eq!(dictionary.len(), 2);
        assert_eq!(dictionary.get(ContigId::new(0)).unwrap().name(), "chr1");
        assert_eq!(dictionary.get(ContigId::new(0)).unwrap().length(), 23);
        assert_eq!(dictionary.get(ContigId::new(1)).unwrap().length(), 14);
        let interval = GenomicInterval::new(ContigId::new(0), 2, 10).unwrap();
        assert_eq!(reference.fetch(interval).unwrap(), b"GTNNNNAC");
        let slice = reference
            .fetch_slice_by_name(
                "chrM",
                GenomicInterval::new(ContigId::new(1), 0, 7).unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(slice.bases, b"GATTACA");
        assert!(reference.remote_io_stats().is_none());

        // A chromosome-sizes table supplies lengths without touching each sequence record.
        let sizes = directory.path().join("genome.chrom.sizes");
        fs::write(&sizes, "chr1\t23\nchrM\t14\n").unwrap();
        let with_sizes = FastaReference::open_with_options(
            &path,
            &FastaOpenOptions {
                chrom_sizes: Some(sizes.to_string_lossy().into_owned()),
                ..FastaOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            with_sizes.reference_dictionary(),
            reference.reference_dictionary()
        );
        assert_eq!(with_sizes.fetch(interval).unwrap(), b"GTNNNNAC");
    }

    #[test]
    fn named_fetch_preserves_the_alignment_contig_id() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("partial.fa");
        fs::write(&path, b">chr20\nTTTT\n>chr6\nACGTACGT\n").unwrap();
        faidx::build(&path).unwrap();
        let reference = FastaReference::open(&path).unwrap();
        let alignment_interval = GenomicInterval::new(ContigId::new(5), 1, 5).unwrap();

        let slice = reference
            .fetch_slice_by_name("chr6", alignment_interval)
            .unwrap()
            .unwrap();

        assert_eq!(slice.interval, alignment_interval);
        assert_eq!(slice.bases, b"CGTA");
        assert!(
            reference
                .fetch_slice_by_name("chr1", alignment_interval)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn remote_fasta_uses_bounded_authenticated_ranges_and_preserves_query_credentials() {
        let fasta = b">chr1\nACGTACGT\n>chr2\nTTAA\n".to_vec();
        let index = b"chr1\t8\t6\t8\t9\nchr2\t4\t21\t4\t5\n".to_vec();
        let index_len = index.len();
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                    continue;
                };
                assert!(request.url().ends_with("?token=temporary"));
                assert!(request.headers().iter().any(|header| {
                    header.field.equiv("Authorization")
                        && header.value.as_str() == "Bearer reference-secret"
                }));
                let data = if request.url().starts_with("/genome.fa.fai") {
                    &index
                } else {
                    &fasta
                };
                let range = request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Range"))
                    .and_then(|header| test_range(header.value.as_str(), data.len()))
                    .unwrap();
                let body = data[range.0..=range.1].to_vec();
                request
                    .respond(
                        Response::from_data(body)
                            .with_status_code(StatusCode(206))
                            .with_header(test_header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                            ))
                            .with_header(test_header("ETag", "\"reference-fixture\"")),
                    )
                    .unwrap();
            }
        });
        let mut options = FastaOpenOptions::default();
        options.remote_range.chunk_size = std::num::NonZeroUsize::new(8).unwrap();
        options.remote_range.maximum_cache_bytes = std::num::NonZeroUsize::new(64).unwrap();
        options.remote_range.maximum_coalesced_chunks = std::num::NonZeroUsize::new(2).unwrap();
        options.remote_range.authentication =
            crate::RemoteAuthentication::Bearer("reference-secret".to_owned());
        let reference = FastaReference::open_with_options(
            format!("http://{address}/genome.fa?token=temporary"),
            &options,
        )
        .unwrap();

        let interval = GenomicInterval::new(ContigId::new(0), 2, 7).unwrap();
        assert_eq!(reference.fetch(interval).unwrap(), b"GTACG");
        assert_eq!(
            reference
                .fetch_slice_by_name(
                    "chr2",
                    GenomicInterval::new(ContigId::new(9), 0, 4).unwrap(),
                )
                .unwrap()
                .unwrap()
                .bases,
            b"TTAA"
        );
        let stats = reference.remote_io_stats().unwrap();
        assert!(stats.requests >= 4);
        assert!(stats.response_bytes >= (index_len + 2) as u64);

        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
    }

    #[test]
    fn fasta_index_duplicate_check_scales_to_scaffold_level_assemblies() {
        use std::fmt::Write as _;

        // 200,000 scaffolds: a pairwise name comparison would need 2 * 10^10 string compares.
        let mut index = String::new();
        for scaffold in 0..200_000_u64 {
            writeln!(index, "scaffold_{scaffold}\t8\t{}\t8\t9", scaffold * 9 + 12).unwrap();
        }
        let entries = parse_fasta_index(index.as_bytes()).unwrap();
        assert_eq!(entries.len(), 200_000);
        assert_eq!(entries[199_999].name, "scaffold_199999");
        // A repeat of the very first name is still found at the very end.
        index.push_str("scaffold_0\t8\t5\t8\t9\n");
        assert!(matches!(
            parse_fasta_index(index.as_bytes()),
            Err(AlignmentError::InvalidFastaIndex(message)) if message == "duplicate contig scaffold_0"
        ));
    }

    #[test]
    fn fasta_index_parser_rejects_duplicate_and_unbounded_geometry() {
        assert!(parse_fasta_index(b"chr1\t8\t6\t8\t9\nchr1\t4\t20\t4\t5\n").is_err());
        assert!(parse_fasta_index(b"chr1\t8\t6\t0\t0\n").is_err());
        assert!(parse_fasta_index(b"").is_err());
    }

    fn test_range(value: &str, length: usize) -> Option<(usize, usize)> {
        let value = value.strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        let start = start.parse::<usize>().ok()?;
        let end = end.parse::<usize>().ok()?.min(length.checked_sub(1)?);
        (start <= end).then_some((start, end))
    }

    fn test_header(name: &str, value: impl AsRef<str>) -> Header {
        Header::from_bytes(name.as_bytes(), value.as_ref().as_bytes()).unwrap()
    }
}
