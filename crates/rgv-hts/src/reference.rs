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
/// Largest `.gzi` (BGZF block offset table) read for a remote bgzip-compressed FASTA: sixteen
/// bytes per block, so enough for a FASTA of several hundred gigabases.
const MAX_REMOTE_GZI_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug, Default)]
pub struct FastaOpenOptions {
    pub remote_index_url: Option<String>,
    pub remote_range: HttpRangeConfig,
    /// Optional UCSC `chrom.sizes` table (local path or URL) for a `.2bit` reference. It supplies
    /// every sequence length in one read; without it each length is read from its own record,
    /// which is slow for a remote file with many sequences.
    pub chrom_sizes: Option<String>,
    /// URL of the `.gzi` block index of a remote bgzip-compressed FASTA. Without it the index
    /// is expected at `<url>.gzi`, where `samtools faidx` writes it.
    pub remote_gzi_url: Option<String>,
    /// Report the soft-masking of a `.2bit` reference: masked bases are returned in lower
    /// case, as a soft-masked FASTA returns them. Off by default, in which case every base of
    /// a `.2bit` reference is upper case. It costs reading each fetched sequence's mask table
    /// once. FASTA references always return the bases as the file stores them.
    pub two_bit_soft_masking: bool,
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
pub(crate) struct FastaIndexEntry {
    name: String,
    length: u64,
    offset: u64,
    line_bases: u64,
    line_width: u64,
}

struct RemoteFastaReference {
    reader: Mutex<RemoteFastaStream>,
    entries: Vec<FastaIndexEntry>,
    index_stats: RemoteIoStats,
}

/// The bytes of a remote FASTA, addressed by offset in the uncompressed text.
enum RemoteFastaStream {
    Plain(HttpRangeReader),
    /// bgzip-compressed (`.fa.gz` with `.fai` and `.gzi`): the `.gzi` maps an uncompressed
    /// offset to the BGZF block holding it.
    Bgzf {
        reader: noodles::bgzf::io::Reader<HttpRangeReader>,
        index: noodles::bgzf::gzi::Index,
    },
}

impl RemoteFastaStream {
    fn http(&self) -> &HttpRangeReader {
        match self {
            Self::Plain(reader) => reader,
            Self::Bgzf { reader, .. } => reader.get_ref(),
        }
    }

    fn http_mut(&mut self) -> &mut HttpRangeReader {
        match self {
            Self::Plain(reader) => reader,
            Self::Bgzf { reader, .. } => reader.get_mut(),
        }
    }

    fn seek_to(&mut self, offset: u64) -> std::io::Result<()> {
        match self {
            Self::Plain(reader) => reader.seek(SeekFrom::Start(offset)).map(|_| ()),
            Self::Bgzf { reader, index } => reader
                .seek_by_uncompressed_position(index, offset)
                .map(|_| ()),
        }
    }

    fn read_exact(&mut self, buffer: &mut [u8]) -> std::io::Result<()> {
        match self {
            Self::Plain(reader) => reader.read_exact(buffer),
            Self::Bgzf { reader, .. } => reader.read_exact(buffer),
        }
    }

    /// Whether skipping a few bytes is better done by reading them than by seeking: a seek in
    /// a BGZF stream inflates the target block again.
    const fn reads_through_gaps(&self) -> bool {
        matches!(self, Self::Bgzf { .. })
    }
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
        if let Some(scheme) = crate::local::unsupported_url_scheme(&raw) {
            // Refused by name: `HTSlib` would hand `s3://`, `gs://` or `ftp://` to libcurl.
            return Err(crate::RemoteError::UnsupportedScheme(scheme.to_owned()).into());
        }
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
        let two_bit = if options.two_bit_soft_masking {
            TwoBitReference::open_soft_masked(reader)?
        } else {
            TwoBitReference::open(reader)?
        };
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
            reader.http_mut().set_cancellation(cancellation);
        }
    }

    pub fn set_remote_transfer_budget(&self, budget: Option<RemoteTransferBudget>) {
        if let FastaReader::Remote(remote) = &self.reader
            && let Ok(mut reader) = remote.reader.lock()
        {
            reader.http_mut().set_transfer_budget(budget);
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
        let block_compressed = options.remote_gzi_url.is_some()
            || location_has_extension(url, true, "gz")
            || location_has_extension(url, true, "bgz");
        if block_compressed {
            let gzi_url = options
                .remote_gzi_url
                .clone()
                .map_or_else(|| associated_remote_url(url, ".gzi"), Ok)?;
            let (gzi, gzi_stats) = read_remote_object_bounded(
                &gzi_url,
                options.remote_range.clone(),
                MAX_REMOTE_GZI_BYTES,
            )
            .map_err(|error| {
                AlignmentError::InvalidFastaIndex(format!(
                    "a remote bgzip-compressed FASTA needs its .gzi block index beside the \
                     .fai (`samtools faidx` writes both): {error}"
                ))
            })?;
            let block_index = noodles::bgzf::gzi::io::Reader::new(gzi.as_slice())
                .read_index()
                .map_err(|error| {
                    AlignmentError::InvalidFastaIndex(format!("invalid .gzi block index: {error}"))
                })?;
            let reader = HttpRangeReader::open(url, options.remote_range.clone())?;
            return Ok(Self {
                reader: Mutex::new(RemoteFastaStream::Bgzf {
                    reader: noodles::bgzf::io::Reader::new(reader),
                    index: block_index,
                }),
                entries,
                index_stats: combine_remote_stats(index_stats, gzi_stats),
            });
        }
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
            reader: Mutex::new(RemoteFastaStream::Plain(reader)),
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
        // Line terminators between two lines of a fetch are read through rather than seeked
        // over where a seek is expensive.
        let terminator = entry.line_width - entry.line_bases;
        let mut skipped = [0_u8; 2];
        let read_through = reader.reads_through_gaps() && terminator as usize <= skipped.len();
        let mut positioned = false;
        while position < end {
            let line = position / entry.line_bases;
            let column = position % entry.line_bases;
            let count = (entry.line_bases - column).min(end - position);
            if !(positioned && read_through) {
                let byte_offset = entry
                    .offset
                    .checked_add(line.saturating_mul(entry.line_width))
                    .and_then(|offset| offset.checked_add(column))
                    .ok_or(AlignmentError::CoordinateOverflow)?;
                reader.seek_to(byte_offset)?;
                positioned = true;
            }
            let old_len = bases.len();
            bases.resize(old_len.saturating_add(count as usize), 0);
            reader.read_exact(&mut bases[old_len..])?;
            position = position.saturating_add(count);
            if read_through && position < end {
                reader.read_exact(&mut skipped[..terminator as usize])?;
            }
        }
        Ok(bases)
    }

    fn remote_io_stats(&self) -> RemoteIoStats {
        self.reader.lock().map_or(self.index_stats, |reader| {
            combine_remote_stats(self.index_stats, reader.http().stats())
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

pub(crate) fn parse_fasta_index(bytes: &[u8]) -> Result<Vec<FastaIndexEntry>, AlignmentError> {
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

    /// Serves byte ranges of named objects until dropped.
    struct ObjectServer {
        base: String,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl ObjectServer {
        fn start(objects: Vec<(&'static str, Vec<u8>)>) -> Self {
            let server = Server::http("127.0.0.1:0").unwrap();
            let address = server.server_addr().to_ip().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                        continue;
                    };
                    let path = request.url().split('?').next().unwrap_or_default();
                    let found = objects.iter().find(|(name, _)| *name == path);
                    let range = found.and_then(|(_, data)| {
                        request
                            .headers()
                            .iter()
                            .find(|header| header.field.equiv("Range"))
                            .and_then(|header| test_range(header.value.as_str(), data.len()))
                    });
                    let (Some((_, data)), Some(range)) = (found, range) else {
                        request.respond(Response::empty(StatusCode(404))).unwrap();
                        continue;
                    };
                    request
                        .respond(
                            Response::from_data(data[range.0..=range.1].to_vec())
                                .with_status_code(StatusCode(206))
                                .with_header(test_header(
                                    "Content-Range",
                                    format!("bytes {}-{}/{}", range.0, range.1, data.len()),
                                ))
                                .with_header(test_header("ETag", "\"bgzip-fasta\"")),
                        )
                        .unwrap();
                }
            });
            Self {
                base: format!("http://{address}"),
                stop,
                worker: Some(worker),
            }
        }
    }

    impl Drop for ObjectServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    /// A FASTA of three sequences with 60-base lines, soft-masked runs and a run of `N`: one
    /// long, one ending in a partial line, one that is exactly one line.
    fn soft_masked_fasta() -> (Vec<u8>, Vec<(&'static str, usize)>) {
        let contigs = vec![("chr1", 25_000), ("chr2", 3_217), ("chrM", 60)];
        let mut text = Vec::new();
        for (name, length) in &contigs {
            text.extend_from_slice(format!(">{name} test sequence\n").as_bytes());
            for index in 0..*length {
                let mut base = b"ACGT"[(index * 7 + index / 13) % 4];
                if (2_000..2_100).contains(&index) {
                    base = b'N';
                }
                if (index / 500) % 3 == 1 {
                    base = base.to_ascii_lowercase();
                }
                text.push(base);
                if index % 60 == 59 || index + 1 == *length {
                    text.push(b'\n');
                }
            }
        }
        (text, contigs)
    }

    /// bgzip-compressed FASTA (`.fa.gz` + `.fai` + `.gzi`), local and remote, returns exactly
    /// what the uncompressed FASTA does, soft-masking included, for intervals that span line
    /// ends and BGZF blocks.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn bgzip_compressed_fasta_equals_the_uncompressed_fasta_locally_and_remotely() {
        use std::io::Write as _;

        let directory = tempdir().unwrap();
        let (text, contigs) = soft_masked_fasta();
        let plain_path = directory.path().join("genome.fa");
        fs::write(&plain_path, &text).unwrap();
        faidx::build(&plain_path).unwrap();

        // BGZF blocks of 1,999 uncompressed bytes: block edges fall inside lines.
        let compressed_path = directory.path().join("genome.fa.gz");
        let mut writer =
            noodles::bgzf::io::Writer::new(fs::File::create(&compressed_path).unwrap());
        for block in text.chunks(1_999) {
            writer.write_all(block).unwrap();
            writer.flush().unwrap();
        }
        writer.finish().unwrap();
        // `HTSlib` writes the `.fai` and the `.gzi` block index, as `samtools faidx` does.
        faidx::build(&compressed_path).unwrap();
        let fai = fs::read(format!("{}.fai", compressed_path.display())).unwrap();
        let gzi = fs::read(format!("{}.gzi", compressed_path.display())).unwrap();
        assert_eq!(
            fai,
            fs::read(format!("{}.fai", plain_path.display())).unwrap(),
            "the index of a bgzip-compressed FASTA addresses the uncompressed text"
        );
        // Twelve blocks beyond the first are indexed.
        assert!(gzi.len() >= 8 + 16 * 12, "{}", gzi.len());

        let server = ObjectServer::start(vec![
            ("/genome.fa.gz", fs::read(&compressed_path).unwrap()),
            ("/genome.fa.gz.fai", fai.clone()),
            ("/genome.fa.gz.gzi", gzi.clone()),
            (
                "/elsewhere/genome.fasta",
                fs::read(&compressed_path).unwrap(),
            ),
            ("/elsewhere/genome.fasta.fai", fai),
            ("/indexes/blocks.gzi", gzi),
            ("/orphan/genome.fa.gz", fs::read(&compressed_path).unwrap()),
            (
                "/orphan/genome.fa.gz.fai",
                fs::read(format!("{}.fai", plain_path.display())).unwrap(),
            ),
        ]);
        let mut options = FastaOpenOptions::default();
        options.remote_range.chunk_size = std::num::NonZeroUsize::new(1_024).unwrap();
        options.remote_range.maximum_cache_bytes = std::num::NonZeroUsize::new(8 * 1_024).unwrap();
        options.remote_range.maximum_retries = 0;

        let plain = FastaReference::open(&plain_path).unwrap();
        let local = FastaReference::open(&compressed_path).unwrap();
        let remote =
            FastaReference::open_with_options(format!("{}/genome.fa.gz", server.base), &options)
                .unwrap();
        // The block index somewhere else, named explicitly.
        let explicit = FastaReference::open_with_options(
            format!("{}/elsewhere/genome.fasta", server.base),
            &FastaOpenOptions {
                remote_gzi_url: Some(format!("{}/indexes/blocks.gzi", server.base)),
                ..options.clone()
            },
        )
        .unwrap();
        for reference in [&local, &remote, &explicit] {
            assert_eq!(
                reference.reference_dictionary(),
                plain.reference_dictionary()
            );
        }
        assert!(local.remote_io_stats().is_none());

        let mut compared = 0;
        let mut lower_case_seen = false;
        for (contig, (_, length)) in contigs.iter().enumerate() {
            let length = *length as u64;
            let starts = [
                0, 1, 59, 60, 61, 499, 500, 1_938, 1_998, 1_999, 2_050, 12_345,
            ];
            let lengths = [1, 2, 59, 60, 61, 120, 1_999, 2_000, 4_100, 25_000];
            for start in starts.into_iter().filter(|start| *start < length) {
                for span in lengths {
                    let end = (start + span).min(length);
                    let interval =
                        GenomicInterval::new(ContigId::new(contig as u32), start, end).unwrap();
                    let expected = plain.fetch(interval).unwrap();
                    assert_eq!(expected.len() as u64, end - start);
                    lower_case_seen |= expected.iter().any(u8::is_ascii_lowercase);
                    for (name, reference) in [
                        ("local", &local),
                        ("remote", &remote),
                        ("explicit", &explicit),
                    ] {
                        assert!(
                            reference.fetch(interval).unwrap() == expected,
                            "{name} bgzip FASTA differs at {interval:?}"
                        );
                    }
                    compared += 1;
                }
            }
        }
        assert!(compared > 200);
        assert!(lower_case_seen, "the fixture has no soft-masked base");
        // Soft-masking survives: this stretch is lower case in the file.
        let masked = GenomicInterval::new(ContigId::new(0), 500, 510).unwrap();
        let bases = remote.fetch(masked).unwrap();
        assert!(bases.iter().all(u8::is_ascii_lowercase), "{bases:?}");

        // Ranges were read, not the whole object, and the transfer budget is honoured.
        let stats = remote.remote_io_stats().unwrap();
        assert!(stats.requests > 3);
        let fresh =
            FastaReference::open_with_options(format!("{}/genome.fa.gz", server.base), &options)
                .unwrap();
        fresh.set_remote_transfer_budget(Some(RemoteTransferBudget {
            maximum_requests: 0,
            maximum_response_bytes: 0,
        }));
        let far = GenomicInterval::new(ContigId::new(0), 20_000, 20_100).unwrap();
        assert!(fresh.fetch(far).is_err());
        fresh.set_remote_transfer_budget(None);
        assert_eq!(fresh.fetch(far).unwrap(), plain.fetch(far).unwrap());

        // Without the block index the open fails and says what is missing.
        let Err(error) = FastaReference::open_with_options(
            format!("{}/orphan/genome.fa.gz", server.base),
            &options,
        ) else {
            panic!("a bgzip-compressed FASTA opened without its .gzi");
        };
        assert!(error.to_string().contains(".gzi"), "{error}");
    }

    #[test]
    fn references_at_unsupported_url_schemes_are_refused_by_name() {
        for location in [
            "s3://bucket/genome.fa",
            "gs://bucket/genome.fa.gz",
            "ftp://ftp.example.org/pub/genome.fa",
            "S3://bucket/genome.2bit",
        ] {
            let Err(error) = FastaReference::open(location) else {
                panic!("{location} opened");
            };
            assert!(
                matches!(
                    &error,
                    AlignmentError::Remote(crate::RemoteError::UnsupportedScheme(scheme))
                        if location.to_ascii_lowercase().starts_with(&scheme.to_ascii_lowercase())
                ),
                "{location}: {error}"
            );
            assert!(
                error.to_string().contains("unsupported remote URL scheme"),
                "{error}"
            );
        }
        // A path that merely contains a colon is still a path.
        let Err(error) = FastaReference::open("/nonexistent/odd:name/genome.fa") else {
            panic!("a missing file opened");
        };
        assert!(!error.to_string().contains("scheme"), "{error}");
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
