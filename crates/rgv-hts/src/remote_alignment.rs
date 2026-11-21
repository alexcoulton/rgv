use std::{
    collections::BTreeMap,
    io::{self, Cursor, Read as _, SeekFrom},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use flate2::read::GzDecoder;
use noodles::{
    bam,
    core::{Position, Region},
    cram,
    csi::BinningIndex,
    fasta,
    sam::{self, alignment::record::cigar::op::Kind},
};
use rgv_core::{ContigId, GenomicInterval, ReferenceDictionary};

use crate::{
    AlignmentBatch, AlignmentError, AlignmentHeaderMetadata, AlignmentOpenOptions, AlignmentQuery,
    AlignmentSource, AlignmentSourceFormat, AlignmentSourceStamp, FastaOpenOptions, FastaReference,
    HttpRangeConfig, HttpRangeReader, JunctionSummary, LocalAlignmentSource, PileupSummary,
    PositionedChunkReceipt, QueryOptions, QueryStats, RemoteCancellation, RemoteError,
    RemoteIoStats, RemoteTransferBudget, associated_remote_url,
    backend_record::{AsBackendRecord, RegionQueryAccumulator},
    index_overview::local_alignment_index_candidates,
    read_remote_object_bounded, remote_url_with_extension,
    source_revision::{LocalSourceGuard, digest_bytes, local_source_stamp, remote_source_stamp},
};

mod in_memory;
mod local_bam;
mod positioned_stream;

pub use in_memory::MAX_IN_MEMORY_ALIGNMENT_BYTES;
pub use local_bam::LocalBamAlignmentSource;
use positioned_stream::NoodlesPositionedStream;

/// Remote BAM stream and its index. They are kept apart (instead of in a noodles
/// `IndexedReader`, which boxes the index as a non-`Send` trait object) so the source can be
/// moved onto its worker thread.
type NoodlesReader = noodles::bgzf::io::Reader<HttpRangeReader>;
type NoodlesCramReader = cram::io::IndexedReader<HttpRangeReader>;

/// The BGZF stream the noodles BAM front ends decode: the remote range reader or a local file.
/// The record iteration, positioned streams and resume logic are generic over it, so a local
/// and a remote BAM are read by the same code.
pub(crate) trait BamBlockStream:
    noodles::bgzf::io::BufRead + noodles::bgzf::io::Seek
{
    /// Announces the `(start, end)` virtual-offset ranges about to be read. A remote stream
    /// plans its range requests from them; a local file has nothing to plan.
    fn plan_reads(&mut self, _chunks: Vec<(u64, u64)>) {}
}

impl BamBlockStream for NoodlesReader {
    fn plan_reads(&mut self, chunks: Vec<(u64, u64)>) {
        self.get_mut().set_bgzf_read_plan(chunks);
    }
}

const MAX_REMOTE_ALIGNMENT_INDEX_BYTES: usize = 256 * 1024 * 1024;
const MAX_REMOTE_CRAM_REFERENCE_BASES: usize = 512 * 1024 * 1024;
const MAX_REMOTE_CRAI_COMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_REMOTE_CRAI_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_REMOTE_CRAI_RECORDS: usize = 1_000_000;

/// Header metadata through the one SAM-text parser the `HTSlib` path uses, so read groups and
/// reference sequences are interpreted (and bounded) identically for every backend.
fn alignment_header_metadata_from_noodles(header: &sam::Header) -> AlignmentHeaderMetadata {
    let mut text = Vec::new();
    if sam::io::Writer::new(&mut text)
        .write_header(header)
        .is_err()
    {
        return AlignmentHeaderMetadata::default();
    }
    crate::alignment::alignment_header_metadata_from_sam(&text)
}

/// Production alignment backend selection. Local BAM uses `HTSlib` unless
/// [`AlignmentOpenOptions::local_bam_backend`] asks for noodles; local CRAM and every remote
/// object (BAM or CRAM, whatever its URL looks like) use noodles, remote ones over the
/// observable range reader. `HTSlib` never opens a URL.
pub enum ProductionAlignmentSource {
    Htslib(LocalAlignmentSource),
    LocalBam(Box<LocalBamAlignmentSource>),
    LocalCram(Box<LocalCramAlignmentSource>),
    RemoteBam(Box<RemoteBamAlignmentSource>),
    RemoteCram(Box<RemoteCramAlignmentSource>),
}

impl ProductionAlignmentSource {
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::RemoteBam(_) | Self::RemoteCram(_))
    }

    pub fn open(
        path: impl AsRef<std::path::Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let path = path.as_ref();
        let raw = path.to_string_lossy();
        if raw.starts_with("http://") || raw.starts_with("https://") {
            let kind = url::Url::parse(&raw).ok().and_then(|url| {
                let extension = std::path::Path::new(url.path()).extension()?;
                if extension.eq_ignore_ascii_case("bam") {
                    Some(RemoteAlignmentKind::Bam)
                } else if extension.eq_ignore_ascii_case("cram") {
                    Some(RemoteAlignmentKind::Cram)
                } else {
                    None
                }
            });
            let mut source = HttpRangeReader::open(raw.as_ref(), options.remote_range.clone())?;
            // A URL that does not name its format (an object key, a download endpoint, a
            // signed link) is identified by the object's leading bytes.
            let kind = match kind {
                Some(kind) => kind,
                None => sniff_remote_alignment(&mut source)?,
            };
            return match kind {
                RemoteAlignmentKind::Bam => {
                    RemoteBamAlignmentSource::open_source(raw.as_ref(), source, options)
                        .map(Box::new)
                        .map(Self::RemoteBam)
                }
                RemoteAlignmentKind::Cram => {
                    RemoteCramAlignmentSource::open_source(raw.as_ref(), source, options)
                        .map(Box::new)
                        .map(Self::RemoteCram)
                }
            };
        }
        let has_extension = |expected: &str| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
        };
        if has_extension("cram") {
            LocalCramAlignmentSource::open(path, options)
                .map(Box::new)
                .map(Self::LocalCram)
        } else if has_extension("sam") {
            // Plain-text SAM has no index: it is loaded whole, up to a size cap.
            Self::open_in_memory(path, options)
        } else if options.local_bam_backend == crate::AlignmentBackendPreference::Noodles
            && has_extension("bam")
        {
            // Only a file named `.bam`: anything else keeps `HTSlib`'s format detection.
            LocalBamAlignmentSource::open(path, options)
                .map(Box::new)
                .map(Self::LocalBam)
        } else {
            LocalAlignmentSource::open(path, options).map(Self::Htslib)
        }
    }

    /// Loads a plain-text SAM file, or a BAM file without an index (sorted or not), into
    /// memory and serves it like any indexed alignment source; see
    /// [`LocalBamAlignmentSource::open_in_memory`] for the size cap. [`Self::open`] does this
    /// by itself for a file named `.sam`; an unindexed `.bam` has to be opened here.
    pub fn open_in_memory(
        path: impl AsRef<std::path::Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        LocalBamAlignmentSource::open_in_memory(path, options)
            .map(Box::new)
            .map(Self::LocalBam)
    }

    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        match self {
            Self::Htslib(_) | Self::LocalBam(_) | Self::LocalCram(_) => None,
            Self::RemoteBam(source) => Some(source.remote_io_stats()),
            Self::RemoteCram(source) => Some(source.remote_io_stats()),
        }
    }

    pub fn set_remote_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        match self {
            Self::Htslib(_) | Self::LocalBam(_) | Self::LocalCram(_) => {}
            Self::RemoteBam(source) => source.set_transfer_budget(budget),
            Self::RemoteCram(source) => source.set_transfer_budget(budget),
        }
    }

    pub fn index_overview(&self) -> Option<&crate::AlignmentIndexOverview> {
        match self {
            Self::Htslib(source) => source.index_overview(),
            Self::LocalBam(source) => source.index_overview(),
            Self::LocalCram(_) => None,
            Self::RemoteBam(source) => source.index_overview.as_ref(),
            Self::RemoteCram(_) => None,
        }
    }

    /// Immutable parsed SAM header metadata retained for scene-backed inspection.
    #[must_use]
    pub fn header_metadata(&self) -> &Arc<AlignmentHeaderMetadata> {
        match self {
            Self::Htslib(source) => source.header_metadata(),
            Self::LocalBam(source) => &source.metadata,
            Self::LocalCram(source) => &source.metadata,
            Self::RemoteBam(source) => &source.metadata,
            Self::RemoteCram(source) => &source.metadata,
        }
    }

    /// Exact mapped-record total when the index provides it in constant time.
    #[must_use]
    pub fn normalization_mapped_records(&self) -> Option<u64> {
        match self {
            Self::Htslib(source) => source.normalization_mapped_records(),
            Self::LocalBam(source) => source.normalization_mapped_records,
            Self::RemoteBam(source) => source.normalization_mapped_records,
            Self::LocalCram(_) | Self::RemoteCram(_) => None,
        }
    }

    /// Whether decoding needs a reference sequence supplied at open time.
    #[must_use]
    pub const fn is_cram(&self) -> bool {
        matches!(self, Self::LocalCram(_) | Self::RemoteCram(_))
    }

    /// Installs or clears the cancellation probe consulted by remote range requests.
    pub fn set_remote_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        match self {
            Self::Htslib(_) | Self::LocalBam(_) | Self::LocalCram(_) => {}
            Self::RemoteBam(source) => source.set_cancellation(cancellation),
            Self::RemoteCram(source) => source.set_cancellation(cancellation),
        }
    }

    /// Starts a resumable positioned acquisition; see
    /// [`LocalAlignmentSource::begin_positioned_stream_with_admission`]. Calling this again
    /// retargets by discarding only the unfinished transaction state.
    pub fn begin_positioned_stream_with_admission(
        &mut self,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), AlignmentError> {
        self.begin_positioned_stream_resuming(interval, admission_interval, options, Vec::new())
            .map(|_| ())
    }

    /// Starts a positioned acquisition that does not decode the chunks named by `skip` again;
    /// see [`LocalAlignmentSource::begin_positioned_stream_resuming`] for the contract. Returns
    /// how many receipts were honoured.
    ///
    /// What skipping saves depends on the backend. Remote BAM recognises a skipped chunk by its
    /// start offset before reading it: its bytes are left out of the range plan and are neither
    /// requested nor inflated, except the BGZF block in which the last skipped chunk ends,
    /// which also holds the start of what follows (and any hole in the plan smaller than the
    /// range reader's coalescing gap). CRAM (local and remote) never selects a skipped slice:
    /// it is neither read nor decoded. Local BAM through `HTSlib` reads one record per run of
    /// skipped chunks; local BAM through noodles seeks past them like remote BAM.
    pub fn begin_positioned_stream_resuming(
        &mut self,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<usize, AlignmentError> {
        match self {
            Self::Htslib(source) => {
                source.begin_positioned_stream_resuming(interval, admission_interval, options, skip)
            }
            Self::LocalBam(source) => {
                source.begin_positioned_stream(interval, admission_interval, options, skip)
            }
            Self::LocalCram(source) => {
                source.positioned_stream = None;
                if !source.source_guard.validate()? {
                    return Err(AlignmentError::SourceChanged);
                }
                if source.dictionary.get(interval.contig()).is_none() {
                    return Err(AlignmentError::UnknownContig(interval.contig().get()));
                }
                // Each transaction has its own reference budget; slices fetch only their span.
                source.reference_query_bases.store(0, Ordering::Relaxed);
                source.reader.reference_sequence_repository().clear();
                let (stream, skipped) = NoodlesPositionedStream::begin_cram(
                    &source.reader,
                    interval,
                    admission_interval,
                    options,
                    skip,
                )?;
                source.positioned_stream = Some(stream);
                Ok(skipped)
            }
            Self::RemoteCram(source) => {
                source.positioned_stream = None;
                if source.dictionary.get(interval.contig()).is_none() {
                    return Err(AlignmentError::UnknownContig(interval.contig().get()));
                }
                // Each transaction has its own reference budget; slices fetch only their span.
                source.reference_query_bases.store(0, Ordering::Relaxed);
                source.reader.reference_sequence_repository().clear();
                let (stream, skipped) = NoodlesPositionedStream::begin_cram(
                    &source.reader,
                    interval,
                    admission_interval,
                    options,
                    skip,
                )?;
                source.positioned_stream = Some(stream);
                Ok(skipped)
            }
            Self::RemoteBam(source) => {
                source.positioned_stream = None;
                let region = remote_bam_region(&source.dictionary, interval)?;
                let (stream, skipped) = NoodlesPositionedStream::begin_bam(
                    &source.index,
                    interval,
                    admission_interval,
                    options,
                    &region,
                    skip,
                )?;
                source.positioned_stream = Some(stream);
                Ok(skipped)
            }
        }
    }

    /// The receipt of the block the last checkpoint returned, once; see
    /// [`LocalAlignmentSource::take_positioned_chunk_receipt`]. `None` after a checkpoint that
    /// was not a block, and for the empty progress block of a CRAM container without records.
    pub fn take_positioned_chunk_receipt(&mut self) -> Option<PositionedChunkReceipt> {
        match self {
            Self::Htslib(source) => source.take_positioned_chunk_receipt(),
            Self::LocalBam(source) => source.positioned_stream.as_mut()?.take_receipt(),
            Self::LocalCram(source) => source.positioned_stream.as_mut()?.take_receipt(),
            Self::RemoteBam(source) => source.positioned_stream.as_mut()?.take_receipt(),
            Self::RemoteCram(source) => source.positioned_stream.as_mut()?.take_receipt(),
        }
    }

    /// Abandons the unfinished positioned transaction without invalidating returned blocks.
    pub fn cancel_positioned_stream(&mut self) {
        match self {
            Self::Htslib(source) => source.cancel_positioned_stream(),
            Self::LocalBam(source) => source.positioned_stream = None,
            Self::LocalCram(source) => source.positioned_stream = None,
            Self::RemoteBam(source) => source.positioned_stream = None,
            Self::RemoteCram(source) => source.positioned_stream = None,
        }
    }

    /// Performs at most one physical owner unit of source work.
    pub fn next_positioned_stream_checkpoint(
        &mut self,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<crate::PositionedAlignmentStreamCheckpoint, AlignmentError> {
        match self {
            Self::Htslib(source) => source.next_positioned_stream_checkpoint(keep_going),
            Self::LocalBam(source) => source.next_positioned_stream_checkpoint(),
            Self::LocalCram(source) => positioned_stream::next_cram_checkpoint(
                &mut source.positioned_stream,
                &mut source.reader,
                &source.header,
                &source.reference_slice_span,
            ),
            Self::RemoteCram(source) => positioned_stream::next_cram_checkpoint(
                &mut source.positioned_stream,
                &mut source.reader,
                &source.header,
                &source.reference_slice_span,
            ),
            Self::RemoteBam(source) => positioned_stream::next_bam_checkpoint(
                &mut source.positioned_stream,
                &mut source.reader,
                &source.header,
            ),
        }
    }

    /// Builds coverage-pyramid tiles for `request` from the region's record stream without
    /// retaining any read; see [`crate::CoverageTileRequest`]. Memory is bounded by the bins
    /// requested, the depth boundaries in flight and the count-once window, never by depth.
    ///
    /// `cancellation` is polled every few hundred records and, for a remote object, by every
    /// range request. `transfer_budget` bounds the requests and bytes a remote object may
    /// transfer for this call ([`RemoteError::TransferBudgetExceeded`] when crossed); it is
    /// ignored for a local file and cleared again before returning.
    ///
    /// On the `HTSlib` backend this abandons any unfinished positioned stream of this handle.
    pub fn coverage_tiles(
        &mut self,
        request: &crate::CoverageTileRequest,
        transfer_budget: Option<RemoteTransferBudget>,
        cancellation: &RemoteCancellation,
    ) -> Result<crate::CoverageTileOutcome, AlignmentError> {
        let keep_going = cancellation.as_ref();
        match self {
            Self::Htslib(source) => source.coverage_tiles(request, keep_going),
            Self::LocalBam(source) => source.coverage_tiles(request, keep_going),
            Self::LocalCram(source) => source.coverage_tiles(request, keep_going),
            Self::RemoteBam(source) => {
                source.set_cancellation(Some(Arc::clone(cancellation)));
                source.set_transfer_budget(transfer_budget);
                let result = source.coverage_tiles(request, keep_going);
                source.set_transfer_budget(None);
                cancelled_coverage(result, keep_going)
            }
            Self::RemoteCram(source) => {
                source.set_cancellation(Some(Arc::clone(cancellation)));
                source.set_transfer_budget(transfer_budget);
                let result = source.coverage_tiles(request, keep_going);
                source.set_transfer_budget(None);
                cancelled_coverage(result, keep_going)
            }
        }
    }

    pub fn query_with_cancellation(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        cancellation: &RemoteCancellation,
    ) -> Result<AlignmentQuery, AlignmentError> {
        match self {
            Self::Htslib(source) => source.query(interval, options, cancellation.as_ref()),
            Self::LocalBam(source) => source.query(interval, options, cancellation.as_ref()),
            Self::LocalCram(source) => source.query(interval, options, cancellation.as_ref()),
            Self::RemoteBam(source) => {
                source.set_cancellation(Some(Arc::clone(cancellation)));
                source.query(interval, options, cancellation.as_ref())
            }
            Self::RemoteCram(source) => {
                source.set_cancellation(Some(Arc::clone(cancellation)));
                source.query(interval, options, cancellation.as_ref())
            }
        }
    }
}

/// A remote transfer interrupted by the cancellation probe is a cancelled request, not a
/// failure.
fn cancelled_coverage(
    result: Result<crate::CoverageTileOutcome, AlignmentError>,
    keep_going: &dyn Fn() -> bool,
) -> Result<crate::CoverageTileOutcome, AlignmentError> {
    match result {
        Err(_) if !keep_going() => Ok(crate::CoverageTileOutcome::Cancelled),
        other => other,
    }
}

/// Feeds a noodles record stream into the shared coverage accumulator.
fn coverage_from_noodles_records<R, I>(
    records: I,
    header: &sam::Header,
    mut stream: crate::coverage_stream::CoverageStream,
    keep_going: &dyn Fn() -> bool,
) -> Result<crate::CoverageTileOutcome, AlignmentError>
where
    R: AsBackendRecord,
    I: IntoIterator<Item = Result<(R, Option<crate::AlignmentSourcePosition>), AlignmentError>>,
{
    for result in records {
        let (record, _) = result?;
        if !stream.add(&record.backend_record(header), keep_going)? {
            break;
        }
    }
    stream.finish()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RemoteAlignmentKind {
    Bam,
    Cram,
}

/// Identifies a remote alignment object by its magic bytes: a CRAM file definition starts with
/// `CRAM`; a BAM is a BGZF stream whose first block inflates to `BAM\1`. The reader is left at
/// the start of the object, with the sniffed bytes in its cache for the header read.
fn sniff_remote_alignment(
    source: &mut HttpRangeReader,
) -> Result<RemoteAlignmentKind, AlignmentError> {
    use io::Seek as _;

    const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
    let unrecognised = || {
        AlignmentError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "the remote object is neither BAM nor CRAM (unrecognised leading bytes)",
        ))
    };
    let mut magic = [0_u8; 4];
    source.seek(SeekFrom::Start(0))?;
    let mut filled = 0;
    while filled < magic.len() {
        match source.read(&mut magic[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    source.seek(SeekFrom::Start(0))?;
    let kind = if &magic[..filled] == b"CRAM" {
        RemoteAlignmentKind::Cram
    } else if filled >= 2 && magic[..2] == GZIP_MAGIC {
        let mut inflated = [0_u8; 4];
        noodles::bgzf::io::Reader::new(&mut *source)
            .read_exact(&mut inflated)
            .map_err(|error| match error.kind() {
                // Not a BGZF stream, or one shorter than a BAM magic; anything else (a
                // failed, refused or cancelled transfer) is reported as what it is.
                io::ErrorKind::InvalidData
                | io::ErrorKind::InvalidInput
                | io::ErrorKind::UnexpectedEof => unrecognised(),
                _ => AlignmentError::Io(error),
            })?;
        if &inflated != b"BAM\x01" {
            return Err(unrecognised());
        }
        RemoteAlignmentKind::Bam
    } else {
        return Err(unrecognised());
    };
    source.seek(SeekFrom::Start(0))?;
    Ok(kind)
}

impl From<LocalAlignmentSource> for ProductionAlignmentSource {
    fn from(source: LocalAlignmentSource) -> Self {
        Self::Htslib(source)
    }
}

fn remote_bam_region(
    dictionary: &ReferenceDictionary,
    interval: GenomicInterval,
) -> Result<Region, AlignmentError> {
    let contig = dictionary
        .get(interval.contig())
        .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
    let start = Position::try_from(
        usize::try_from(interval.start().saturating_add(1))
            .map_err(|_| AlignmentError::CoordinateOverflow)?,
    )
    .map_err(|_| AlignmentError::CoordinateOverflow)?;
    let end = Position::try_from(
        usize::try_from(interval.end()).map_err(|_| AlignmentError::CoordinateOverflow)?,
    )
    .map_err(|_| AlignmentError::CoordinateOverflow)?;
    Ok(Region::new(contig.name(), start..=end))
}

impl AlignmentSource for ProductionAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        match self {
            Self::Htslib(source) => source.reference_dictionary(),
            Self::LocalBam(source) => source.reference_dictionary(),
            Self::LocalCram(source) => source.reference_dictionary(),
            Self::RemoteBam(source) => source.reference_dictionary(),
            Self::RemoteCram(source) => source.reference_dictionary(),
        }
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        match self {
            Self::Htslib(source) => source.source_stamp(),
            Self::LocalBam(source) => source.source_stamp(),
            Self::LocalCram(source) => source.source_stamp(),
            Self::RemoteBam(source) => source.source_stamp(),
            Self::RemoteCram(source) => source.source_stamp(),
        }
    }

    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        match self {
            Self::Htslib(source) => source.query(interval, options, keep_going),
            Self::LocalBam(source) => source.query(interval, options, keep_going),
            Self::LocalCram(source) => source.query(interval, options, keep_going),
            Self::RemoteBam(source) => source.query(interval, options, keep_going),
            Self::RemoteCram(source) => source.query(interval, options, keep_going),
        }
    }
}

/// Indexed local CRAM source using the same positioned container/slice decoder as remote CRAM.
pub struct LocalCramAlignmentSource {
    reader: cram::io::IndexedReader<std::fs::File>,
    header: sam::Header,
    dictionary: ReferenceDictionary,
    metadata: Arc<AlignmentHeaderMetadata>,
    reference_query_bases: Arc<AtomicUsize>,
    reference_slice_span: CramSliceSpan,
    source_stamp: AlignmentSourceStamp,
    source_guard: LocalSourceGuard,
    positioned_stream: Option<NoodlesPositionedStream>,
}

impl LocalCramAlignmentSource {
    pub fn open(
        path: impl AsRef<std::path::Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let path = path.as_ref();
        let reference = options
            .reference_path
            .as_ref()
            .map(|path| {
                FastaReference::open_with_options(
                    path,
                    &FastaOpenOptions {
                        remote_index_url: options.remote_reference_index_url.clone(),
                        remote_range: options.remote_range.clone(),
                        ..FastaOpenOptions::default()
                    },
                )
                .map(|reference| Arc::new(Mutex::new(reference)))
            })
            .transpose()?;
        let reference_query_bases = Arc::new(AtomicUsize::new(0));
        let reference_slice_span = CramSliceSpan::default();
        let repository =
            reference
                .as_ref()
                .map_or_else(missing_cram_reference_repository, |reference| {
                    fasta::Repository::new(CramReferenceAdapter {
                        reference: Arc::clone(reference),
                        query_bases: Arc::clone(&reference_query_bases),
                        slice_span: Arc::clone(&reference_slice_span),
                    })
                });
        let mut builder = cram::io::indexed_reader::Builder::default()
            .set_reference_sequence_repository(repository);
        if let Some(index_path) = &options.index_path {
            builder = builder.set_index(cram::crai::fs::read(index_path)?);
        }
        let mut reader = builder.build_from_path(path)?;
        let header = reader.read_header()?;
        let metadata = Arc::new(alignment_header_metadata_from_noodles(&header));
        let dictionary =
            crate::local::header_reference_dictionary(header.reference_sequences().iter().map(
                |(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                },
            ))?;
        let index_path = local_alignment_index_candidates(
            path,
            options.index_path.as_deref(),
            AlignmentSourceFormat::Cram,
        )
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "opened CRAM index could not be resolved for revision capture",
            )
        })?;
        let source_guard = LocalSourceGuard::capture(path, &index_path)?;
        let source_stamp = options.source_stamp_override.map_or_else(
            || {
                local_source_stamp(
                    path,
                    &index_path,
                    AlignmentSourceFormat::Cram,
                    &dictionary,
                    &metadata,
                )
            },
            Ok,
        )?;
        Ok(Self {
            reader,
            header,
            dictionary,
            metadata,
            reference_query_bases,
            reference_slice_span,
            source_stamp,
            source_guard,
            positioned_stream: None,
        })
    }
}

impl AlignmentSource for LocalCramAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        Some(self.source_stamp)
    }

    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        if !self.source_guard.validate()? {
            return Err(AlignmentError::SourceChanged);
        }
        if !keep_going() {
            return Ok(cancelled_query(interval, Arc::clone(&self.metadata)));
        }
        if self.dictionary.get(interval.contig()).is_none() {
            return Err(AlignmentError::UnknownContig(interval.contig().get()));
        }
        self.reference_query_bases.store(0, Ordering::Relaxed);
        self.reader.reference_sequence_repository().clear();
        let result = {
            let records = positioned_cram_records(
                &mut self.reader,
                &self.header,
                interval,
                &self.reference_slice_span,
            )?;
            query_positioned_noodles_records(
                records,
                &self.header,
                interval,
                &options,
                None,
                Arc::clone(&self.metadata),
                keep_going,
            )
        };
        self.reader.reference_sequence_repository().clear();
        result
    }
}

/// Indexed remote BAM source backed by RGV's observable, bounded HTTP range cache.
pub struct RemoteBamAlignmentSource {
    reader: NoodlesReader,
    index: RemoteIndex,
    header: noodles::sam::Header,
    dictionary: ReferenceDictionary,
    metadata: Arc<AlignmentHeaderMetadata>,
    index_stats: RemoteIoStats,
    normalization_mapped_records: Option<u64>,
    index_overview: Option<crate::AlignmentIndexOverview>,
    source_stamp: AlignmentSourceStamp,
    positioned_stream: Option<NoodlesPositionedStream>,
}

impl RemoteBamAlignmentSource {
    pub fn open(url: &str, options: &AlignmentOpenOptions) -> Result<Self, AlignmentError> {
        let source = HttpRangeReader::open(url, options.remote_range.clone())?;
        Self::open_source(url, source, options)
    }

    fn open_source(
        url: &str,
        source: HttpRangeReader,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let source_version = source.object_version();
        let (index, index_stats, index_digest) = open_index(
            url,
            options.remote_index_url.as_deref(),
            options.remote_range.clone(),
        )?;
        let normalization_mapped_records = remote_index_mapped_records(&index);
        let mut reader = noodles::bgzf::io::Reader::new(source);
        let header = bam::io::Reader::from(&mut reader).read_header()?;
        let metadata = Arc::new(alignment_header_metadata_from_noodles(&header));
        let dictionary =
            crate::local::header_reference_dictionary(header.reference_sequences().iter().map(
                |(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                },
            ))?;
        let index_overview = Some(match &index {
            RemoteIndex::Bai(index) => {
                crate::AlignmentIndexOverview::from_binning_index(index, &dictionary)
            }
            RemoteIndex::Csi(index) => {
                crate::AlignmentIndexOverview::from_binning_index(index, &dictionary)
            }
        });
        let source_stamp = options.source_stamp_override.unwrap_or_else(|| {
            remote_source_stamp(
                url,
                &source_version,
                AlignmentSourceFormat::Bam,
                &dictionary,
                &metadata,
                &index_digest,
            )
        });
        Ok(Self {
            reader,
            index,
            header,
            dictionary,
            metadata,
            index_stats,
            normalization_mapped_records,
            index_overview,
            source_stamp,
            positioned_stream: None,
        })
    }

    pub fn remote_io_stats(&self) -> RemoteIoStats {
        combine_stats(self.index_stats, self.reader.get_ref().stats())
    }

    fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.reader.get_mut().set_cancellation(cancellation);
    }

    fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        self.reader.get_mut().set_transfer_budget(budget);
    }
}

/// Indexed remote CRAM source backed by the same observable, bounded range cache as remote BAM.
pub struct RemoteCramAlignmentSource {
    reader: NoodlesCramReader,
    header: sam::Header,
    dictionary: ReferenceDictionary,
    metadata: Arc<AlignmentHeaderMetadata>,
    index_stats: RemoteIoStats,
    reference: Option<Arc<Mutex<FastaReference>>>,
    reference_query_bases: Arc<AtomicUsize>,
    reference_slice_span: CramSliceSpan,
    source_stamp: AlignmentSourceStamp,
    positioned_stream: Option<NoodlesPositionedStream>,
}

impl RemoteCramAlignmentSource {
    pub fn open(url: &str, options: &AlignmentOpenOptions) -> Result<Self, AlignmentError> {
        let source = HttpRangeReader::open(url, options.remote_range.clone())?;
        Self::open_source(url, source, options)
    }

    fn open_source(
        url: &str,
        source: HttpRangeReader,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let source_version = source.object_version();
        let (index, index_stats, index_digest) = open_cram_index(
            url,
            options.remote_index_url.as_deref(),
            options.remote_range.clone(),
            source.len(),
        )?;
        let reference = options
            .reference_path
            .as_ref()
            .map(|path| {
                FastaReference::open_with_options(
                    path,
                    &FastaOpenOptions {
                        remote_index_url: options.remote_reference_index_url.clone(),
                        remote_range: options.remote_range.clone(),
                        ..FastaOpenOptions::default()
                    },
                )
                .map(|reference| Arc::new(Mutex::new(reference)))
            })
            .transpose()?;
        let reference_query_bases = Arc::new(AtomicUsize::new(0));
        let reference_slice_span = CramSliceSpan::default();
        let repository =
            reference
                .as_ref()
                .map_or_else(missing_cram_reference_repository, |reference| {
                    fasta::Repository::new(CramReferenceAdapter {
                        reference: Arc::clone(reference),
                        query_bases: Arc::clone(&reference_query_bases),
                        slice_span: Arc::clone(&reference_slice_span),
                    })
                });
        let mut reader = cram::io::indexed_reader::Builder::default()
            .set_index(index)
            .set_reference_sequence_repository(repository)
            .build_from_reader(source)?;
        let header = reader.read_header()?;
        let metadata = Arc::new(alignment_header_metadata_from_noodles(&header));
        let dictionary =
            crate::local::header_reference_dictionary(header.reference_sequences().iter().map(
                |(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                },
            ))?;
        let source_stamp = options.source_stamp_override.unwrap_or_else(|| {
            remote_source_stamp(
                url,
                &source_version,
                AlignmentSourceFormat::Cram,
                &dictionary,
                &metadata,
                &index_digest,
            )
        });
        Ok(Self {
            reader,
            header,
            dictionary,
            metadata,
            index_stats,
            reference,
            reference_query_bases,
            reference_slice_span,
            source_stamp,
            positioned_stream: None,
        })
    }

    pub fn remote_io_stats(&self) -> RemoteIoStats {
        let source = combine_stats(self.index_stats, self.reader.get_ref().stats());
        self.reference
            .as_ref()
            .and_then(|reference| reference.lock().ok()?.remote_io_stats())
            .map_or(source, |reference| combine_stats(source, reference))
    }

    fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.reader.get_mut().set_cancellation(cancellation.clone());
        if let Some(reference) = &self.reference
            && let Ok(reference) = reference.lock()
        {
            reference.set_remote_cancellation(cancellation);
        }
    }

    fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        let reference_is_remote = self.reference.as_ref().is_some_and(|reference| {
            reference
                .lock()
                .ok()
                .is_some_and(|reference| reference.remote_io_stats().is_some())
        });
        let (alignment_budget, reference_budget) = if reference_is_remote {
            budget.map_or((None, None), |budget| {
                let (alignment, reference) = split_remote_transfer_budget(budget);
                (Some(alignment), Some(reference))
            })
        } else {
            (budget, None)
        };
        self.reader.get_mut().set_transfer_budget(alignment_budget);
        if let Some(reference) = &self.reference
            && let Ok(reference) = reference.lock()
        {
            reference.set_remote_transfer_budget(reference_budget);
        }
    }
}

fn split_remote_transfer_budget(
    budget: RemoteTransferBudget,
) -> (RemoteTransferBudget, RemoteTransferBudget) {
    let reference_requests = budget.maximum_requests / 2;
    let reference_bytes = budget.maximum_response_bytes / 2;
    (
        RemoteTransferBudget {
            maximum_requests: budget.maximum_requests.saturating_sub(reference_requests),
            maximum_response_bytes: budget
                .maximum_response_bytes
                .saturating_sub(reference_bytes),
        },
        RemoteTransferBudget {
            maximum_requests: reference_requests,
            maximum_response_bytes: reference_bytes,
        },
    )
}

/// Zero-based half-open reference spans needed by the CRAM slice about to be decoded, shared
/// between the decoder loop and the reference adapter.
type CramSliceSpan = Arc<CramSliceReference>;

/// Spans by reference sequence name. A multi-reference slice has one per sequence it holds; a
/// sequence without an entry is served whole.
type NamedCramSliceSpans = std::collections::HashMap<Vec<u8>, (u64, u64)>;

/// Decoder/adapter rendezvous for the slice being decoded.
#[derive(Default)]
struct CramSliceReference {
    span: Mutex<NamedCramSliceSpans>,
    /// Set by the adapter when it could not supply reference bases (fetch failure, budget,
    /// absent contig). Such a failure says nothing about the slice's span, so it must not be
    /// answered by fetching the whole sequence.
    reference_unavailable: std::sync::atomic::AtomicBool,
}

impl CramSliceReference {
    fn lock(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, NamedCramSliceSpans>> {
        self.span.lock()
    }
}

/// Whether a slice that failed to decode against its indexed span should be decoded once more
/// against the whole reference sequence. Only a decode failure can mean the index understated
/// the span; when the reference itself was unavailable a wider fetch would repeat the same
/// failure at many times the cost.
const fn retry_slice_against_whole_sequence(
    attempted_span: bool,
    reference_unavailable: bool,
) -> bool {
    attempted_span && !reference_unavailable
}

struct CramReferenceAdapter {
    reference: Arc<Mutex<FastaReference>>,
    query_bases: Arc<AtomicUsize>,
    slice_span: CramSliceSpan,
}

/// Stands in when no reference was supplied. A reference-compressed slice then fails with an
/// actionable error; without an adapter the CRAM decoder would panic on the missing sequence.
/// Slices which embed their reference, or need none, never consult the repository.
struct MissingCramReferenceAdapter;

impl fasta::repository::Adapter for MissingCramReferenceAdapter {
    fn get(&mut self, name: &[u8]) -> Option<io::Result<fasta::Record>> {
        Some(Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "this CRAM is compressed against reference sequence '{}'; load the matching \
                 reference FASTA to display it",
                String::from_utf8_lossy(name)
            ),
        )))
    }
}

fn missing_cram_reference_repository() -> fasta::Repository {
    fasta::Repository::new(MissingCramReferenceAdapter)
}

impl fasta::repository::Adapter for CramReferenceAdapter {
    fn get(&mut self, name: &[u8]) -> Option<io::Result<fasta::Record>> {
        let result = self.get_inner(name);
        if result.is_err() {
            self.slice_span
                .reference_unavailable
                .store(true, Ordering::Relaxed);
        }
        Some(result)
    }
}

impl CramReferenceAdapter {
    fn get_inner(&self, raw_name: &[u8]) -> io::Result<fasta::Record> {
        let name = std::str::from_utf8(raw_name).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 CRAM reference name")
        })?;
        let reference = self
            .reference
            .lock()
            .map_err(|_| io::Error::other("CRAM reference reader lock was poisoned"))?;
        let Some(contig_id) = reference.reference_dictionary().id_by_name(name) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("CRAM reference contig {name} is absent from the FASTA"),
            ));
        };
        let contig = reference
            .reference_dictionary()
            .get(contig_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid FASTA contig ID"))?;
        let length = usize::try_from(contig.length())
            .map_err(|_| io::Error::other("CRAM reference contig length exceeds this platform"))?;
        // A slice needs only the span it covers on each sequence it holds. The decoder addresses
        // the sequence by absolute position, so the span is placed in a zero-filled buffer which
        // the allocator maps lazily: only the fetched span is ever resident or transferred.
        let span = self
            .slice_span
            .lock()
            .map_err(|_| io::Error::other("CRAM reference span lock was poisoned"))?
            .get(raw_name)
            .map(|(start, end)| ((*start).min(contig.length()), (*end).min(contig.length())))
            .filter(|(start, end)| start < end);
        let (fetch_start, fetch_end) = span.unwrap_or((0, contig.length()));
        let fetched = usize::try_from(fetch_end - fetch_start)
            .map_err(|_| io::Error::other("CRAM reference span exceeds this platform"))?;
        self.query_bases
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |loaded| {
                loaded
                    .checked_add(fetched)
                    .filter(|total| *total <= MAX_REMOTE_CRAM_REFERENCE_BASES)
            })
            .map_err(|_| {
                io::Error::other(format!(
                    "CRAM query reference budget exceeded {MAX_REMOTE_CRAM_REFERENCE_BASES} bases"
                ))
            })?;
        let bases = if fetched == 0 {
            Vec::new()
        } else {
            let interval = GenomicInterval::new(contig_id, fetch_start, fetch_end)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            match reference.fetch(interval) {
                Ok(bases) if span.is_none() => bases,
                Ok(bases) => {
                    let start = usize::try_from(fetch_start).map_err(|_| {
                        io::Error::other("CRAM reference span exceeds this platform")
                    })?;
                    let mut padded = vec![0_u8; start.saturating_add(bases.len()).min(length)];
                    let copied = padded.len() - start;
                    padded[start..].copy_from_slice(&bases[..copied]);
                    padded
                }
                Err(error) => {
                    self.query_bases.fetch_sub(fetched, Ordering::Relaxed);
                    return Err(io::Error::other(error));
                }
            }
        };
        Ok(fasta::Record::new(
            fasta::record::Definition::new(raw_name, None),
            fasta::record::Sequence::from(bases),
        ))
    }
}

impl RemoteBamAlignmentSource {
    fn coverage_tiles(
        &mut self,
        request: &crate::CoverageTileRequest,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<crate::CoverageTileOutcome, AlignmentError> {
        if !keep_going() {
            return Ok(crate::CoverageTileOutcome::Cancelled);
        }
        let stream = crate::coverage_stream::CoverageStream::new(request, &self.dictionary)?;
        let interval = stream.interval()?;
        let region = remote_bam_region(&self.dictionary, interval)?;
        positioned_bam_records(&mut self.reader, &self.index, interval, &region)?.coverage(
            &self.header,
            stream,
            keep_going,
        )
    }
}

impl RemoteCramAlignmentSource {
    fn coverage_tiles(
        &mut self,
        request: &crate::CoverageTileRequest,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<crate::CoverageTileOutcome, AlignmentError> {
        if !keep_going() {
            return Ok(crate::CoverageTileOutcome::Cancelled);
        }
        let stream = crate::coverage_stream::CoverageStream::new(request, &self.dictionary)?;
        let interval = stream.interval()?;
        self.reference_query_bases.store(0, Ordering::Relaxed);
        self.reader.reference_sequence_repository().clear();
        let result = positioned_cram_records(
            &mut self.reader,
            &self.header,
            interval,
            &self.reference_slice_span,
        )
        .and_then(|records| {
            coverage_from_noodles_records(records, &self.header, stream, keep_going)
        });
        self.reader.reference_sequence_repository().clear();
        result
    }
}

impl LocalCramAlignmentSource {
    /// Builds coverage-pyramid tiles from the region's record stream; see
    /// [`ProductionAlignmentSource::coverage_tiles`].
    pub fn coverage_tiles(
        &mut self,
        request: &crate::CoverageTileRequest,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<crate::CoverageTileOutcome, AlignmentError> {
        if !self.source_guard.validate()? {
            return Err(AlignmentError::SourceChanged);
        }
        if !keep_going() {
            return Ok(crate::CoverageTileOutcome::Cancelled);
        }
        let stream = crate::coverage_stream::CoverageStream::new(request, &self.dictionary)?;
        let interval = stream.interval()?;
        self.reference_query_bases.store(0, Ordering::Relaxed);
        self.reader.reference_sequence_repository().clear();
        let result = positioned_cram_records(
            &mut self.reader,
            &self.header,
            interval,
            &self.reference_slice_span,
        )
        .and_then(|records| {
            coverage_from_noodles_records(records, &self.header, stream, keep_going)
        });
        self.reader.reference_sequence_repository().clear();
        result
    }
}

impl AlignmentSource for RemoteBamAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        Some(self.source_stamp)
    }

    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        if !keep_going() {
            return Ok(cancelled_query(interval, Arc::clone(&self.metadata)));
        }
        let contig = self
            .dictionary
            .get(interval.contig())
            .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
        let start = Position::try_from(
            usize::try_from(interval.start().saturating_add(1))
                .map_err(|_| AlignmentError::CoordinateOverflow)?,
        )
        .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let end = Position::try_from(
            usize::try_from(interval.end()).map_err(|_| AlignmentError::CoordinateOverflow)?,
        )
        .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let region = Region::new(contig.name(), start..=end);
        positioned_bam_records(&mut self.reader, &self.index, interval, &region)?.query(
            &self.header,
            &options,
            self.normalization_mapped_records,
            Arc::clone(&self.metadata),
            keep_going,
        )
    }
}

/// Streams the records of one indexed BAM region that intersect `interval`, one at a time.
///
/// Nothing is buffered: the consumer applies filters, the record limit and cancellation as each
/// record arrives, so a dense region costs one record of memory instead of every record in it.
struct PositionedBamRecords<'a, B> {
    reader: bam::io::Reader<&'a mut B>,
    interval: GenomicInterval,
    chunks: std::vec::IntoIter<noodles::csi::binning_index::index::reference_sequence::bin::Chunk>,
    /// End of the index chunk being read; `None` before the next chunk is entered.
    chunk_end: Option<noodles::bgzf::VirtualPosition>,
    /// Virtual offset just past the last record read. Chunks are visited in file order, so a
    /// record ending at or before it was already produced by an overlapping chunk.
    high_water: u64,
    /// The record the last successful [`Self::advance`] read.
    record: bam::Record,
}

impl<B: BamBlockStream> PositionedBamRecords<'_, B> {
    /// Reads the next record intersecting the interval into `self.record` and returns where
    /// it ends in the file; `None` once the region is exhausted. The record buffer is reused,
    /// so a query allocates per record only what normalisation keeps.
    fn advance(&mut self) -> Result<Option<crate::AlignmentSourcePosition>, AlignmentError> {
        loop {
            let Some(chunk_end) = self.chunk_end else {
                let Some(chunk) = self.chunks.next() else {
                    return Ok(None);
                };
                self.reader
                    .get_mut()
                    .seek_to_virtual_position(chunk.start())?;
                self.chunk_end = Some(chunk.end());
                continue;
            };
            if self.reader.get_ref().virtual_position() >= chunk_end
                || self.reader.read_record(&mut self.record)? == 0
            {
                self.chunk_end = None;
                continue;
            }
            let end_position = u64::from(self.reader.get_ref().virtual_position());
            if end_position <= self.high_water {
                continue;
            }
            self.high_water = end_position;
            let placement = bam_record_placement(&self.record)?;
            if !bam_placement_intersects(placement, self.interval) {
                if bam_placement_starts_at_or_after_end(placement, self.interval) {
                    // Coordinate order: nothing later in the file can intersect, so the
                    // remaining chunks (records of coarser bins, far ahead) are never fetched.
                    self.chunks = Vec::new().into_iter();
                    self.chunk_end = None;
                    return Ok(None);
                }
                continue;
            }
            return Ok(Some(crate::AlignmentSourcePosition::bam_virtual_offset(
                end_position,
            )));
        }
    }

    /// Folds the region's records into coverage tiles; no record outlives its iteration.
    fn coverage(
        mut self,
        header: &sam::Header,
        mut stream: crate::coverage_stream::CoverageStream,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<crate::CoverageTileOutcome, AlignmentError> {
        while self.advance()?.is_some() {
            let record = self.record.backend_record(header);
            if !stream.add(&record, keep_going)? {
                break;
            }
        }
        stream.finish()
    }

    /// Folds the region's records into a one-shot query result.
    fn query(
        mut self,
        header: &sam::Header,
        options: &QueryOptions,
        normalization_mapped_records: Option<u64>,
        metadata: Arc<AlignmentHeaderMetadata>,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        let interval = self.interval;
        let mut accumulator =
            RegionQueryAccumulator::new(interval, options, normalization_mapped_records);
        while let Some(source_position) = self.advance()? {
            let record = self.record.backend_record(header);
            if !accumulator.add(&record, Some(source_position), keep_going)? {
                break;
            }
        }
        if accumulator.cancelled() {
            return Ok(cancelled_query(interval, metadata));
        }
        accumulator.finish(metadata)
    }
}

fn positioned_bam_records<'a, B: BamBlockStream>(
    reader: &'a mut B,
    index: &RemoteIndex,
    interval: GenomicInterval,
    region: &Region,
) -> Result<PositionedBamRecords<'a, B>, AlignmentError> {
    let reference_sequence_id =
        usize::try_from(interval.contig().get()).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let mut chunks = index.query(reference_sequence_id, region.interval())?;
    chunks.sort_by_key(|chunk| (chunk.start(), chunk.end()));
    // Fetch what the index says this region needs, not a fixed read-ahead window.
    reader.plan_reads(
        chunks
            .iter()
            .map(|chunk| (u64::from(chunk.start()), u64::from(chunk.end())))
            .collect(),
    );
    Ok(PositionedBamRecords {
        reader: bam::io::Reader::from(reader),
        interval,
        chunks: chunks.into_iter(),
        chunk_end: None,
        high_water: 0,
        record: bam::Record::default(),
    })
}

impl AlignmentSource for RemoteCramAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        Some(self.source_stamp)
    }

    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        if !keep_going() {
            return Ok(cancelled_query(interval, Arc::clone(&self.metadata)));
        }
        let contig = self
            .dictionary
            .get(interval.contig())
            .ok_or(AlignmentError::UnknownContig(interval.contig().get()))?;
        let _ = contig;
        self.reference_query_bases.store(0, Ordering::Relaxed);
        self.reader.reference_sequence_repository().clear();
        let result = {
            let records = positioned_cram_records(
                &mut self.reader,
                &self.header,
                interval,
                &self.reference_slice_span,
            )?;
            query_positioned_noodles_records(
                records,
                &self.header,
                interval,
                &options,
                None,
                Arc::clone(&self.metadata),
                keep_going,
            )
        };
        self.reader.reference_sequence_repository().clear();
        result
    }
}

/// Zero-based half-open spans CRAI records for one slice, by reference sequence id. A
/// multi-reference slice has one entry per sequence it holds. A sequence is absent when the
/// index gives no usable span for it; it is then read whole.
type CramSliceSpans = BTreeMap<usize, (u64, u64)>;

/// Slices of one container selected for decoding, by landmark.
type SelectedCramSlices = BTreeMap<u64, CramSliceSpans>;

/// Containers and slices which CRAI says can intersect `interval`, in file order.
fn select_cram_containers<R>(
    reader: &cram::io::IndexedReader<R>,
    interval: GenomicInterval,
) -> Result<BTreeMap<u64, SelectedCramSlices>, AlignmentError>
where
    R: io::Read,
{
    let reference_sequence_id =
        usize::try_from(interval.contig().get()).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let mut selected = BTreeMap::<u64, SelectedCramSlices>::new();
    for index_record in reader.index() {
        if index_record.reference_sequence_id() != Some(reference_sequence_id) {
            continue;
        }
        let Some(start) = index_record.alignment_start() else {
            continue;
        };
        let start = u64::try_from(start.get())
            .map_err(|_| AlignmentError::CoordinateOverflow)?
            .saturating_sub(1);
        let end = start.saturating_add(index_record.alignment_span() as u64);
        if start < interval.end() && interval.start() < end {
            selected
                .entry(index_record.offset())
                .or_default()
                .entry(index_record.landmark())
                .or_default();
        }
    }
    // Every index record of a selected slice contributes the span of its own sequence: a
    // multi-reference slice is listed once per sequence it holds, and decoding it resolves the
    // reference of each of its records, not only of those on the queried sequence.
    let mut unusable = std::collections::BTreeSet::new();
    for index_record in reader.index() {
        let slice = (index_record.offset(), index_record.landmark());
        let Some(spans) = selected
            .get_mut(&slice.0)
            .and_then(|slices| slices.get_mut(&slice.1))
        else {
            continue;
        };
        // Unmapped records need no reference.
        let Some(sequence) = index_record.reference_sequence_id() else {
            continue;
        };
        let record_span = index_record
            .alignment_start()
            .and_then(|start| {
                let start = u64::try_from(start.get()).ok()?.saturating_sub(1);
                Some((
                    start,
                    start.saturating_add(index_record.alignment_span() as u64),
                ))
            })
            .filter(|(start, end)| start < end);
        match record_span {
            Some((start, end)) if !unusable.contains(&(slice, sequence)) => {
                spans
                    .entry(sequence)
                    .and_modify(|span| *span = (span.0.min(start), span.1.max(end)))
                    .or_insert((start, end));
            }
            _ => {
                // No usable span for this sequence: read it whole, whatever else is listed.
                unusable.insert((slice, sequence));
                spans.remove(&sequence);
            }
        }
    }
    Ok(selected)
}

fn reference_sequence_name(header: &sam::Header, reference_sequence_id: usize) -> Option<Vec<u8>> {
    header
        .reference_sequences()
        .get_index(reference_sequence_id)
        .map(|(name, _)| <_ as AsRef<[u8]>>::as_ref(name).to_vec())
}

/// Fails when a mapped record about to be returned lies outside the reference span its slice
/// was decoded against: its bases would come from the zero-filled part of the buffer. `served`
/// is `None` when the whole sequence was supplied.
fn require_record_inside_served_span<R>(
    record: &R,
    (start, end): (u64, u64),
    served: Option<(u64, u64)>,
) -> Result<(), AlignmentError>
where
    R: sam::alignment::Record,
{
    if let Some((served_start, served_end)) = served
        && (start < served_start || end > served_end)
        && !record.flags()?.is_unmapped()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the CRAM index understates the reference span of a slice",
        )
        .into());
    }
    Ok(())
}

/// Reads the container CRAI places at `container_offset`.
fn read_cram_container<R>(
    reader: &mut cram::io::IndexedReader<R>,
    container_offset: u64,
) -> Result<cram::io::reader::Container, AlignmentError>
where
    R: io::Read + io::Seek,
{
    let repository = reader.reference_sequence_repository().clone();
    let raw_reader = reader.get_mut();
    raw_reader.seek(SeekFrom::Start(container_offset))?;
    let mut direct_reader = cram::io::reader::Builder::default()
        .set_reference_sequence_repository(repository)
        .build_from_reader(&mut *raw_reader);
    let mut container = cram::io::reader::Container::default();
    if direct_reader.read_container(&mut container)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "CRAI points beyond the CRAM container stream",
        )
        .into());
    }
    Ok(container)
}

/// Decodes the selected slices of one CRAM container and visits every record intersecting
/// `interval` with a physical address which is stable across independent query boundaries.
fn visit_cram_container_records<R, F>(
    reader: &mut cram::io::IndexedReader<R>,
    header: &sam::Header,
    interval: GenomicInterval,
    container_offset: u64,
    selected_slices: &SelectedCramSlices,
    slice_span: &CramSliceSpan,
    mut visit: F,
) -> Result<(), AlignmentError>
where
    R: io::Read + io::Seek,
    F: FnMut(
        sam::alignment::RecordBuf,
        crate::AlignmentSourcePosition,
    ) -> Result<(), AlignmentError>,
{
    let repository = reader.reference_sequence_repository().clone();
    let container = read_cram_container(reader, container_offset)?;
    let compression_header = container.compression_header()?;
    let landmarks = container.header().landmarks().to_vec();
    let set_span = |spans: NamedCramSliceSpans| -> Result<(), AlignmentError> {
        *slice_span
            .lock()
            .map_err(|_| io::Error::other("CRAM reference span lock was poisoned"))? = spans;
        // The repository caches by name; a cached span must not serve another slice.
        repository.clear();
        Ok(())
    };
    let queried_sequence = usize::try_from(interval.contig().get())
        .ok()
        .and_then(|id| reference_sequence_name(header, id));
    for (slice_index, result) in container.slices().enumerate() {
        let slice = result?;
        let landmark = u64::try_from(
            *landmarks
                .get(slice_index)
                .ok_or_else(|| io::Error::other("CRAM slice has no container landmark"))?,
        )
        .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let Some(spans) = selected_slices.get(&landmark) else {
            continue;
        };
        let (core_data_src, external_data_srcs) = slice.decode_blocks()?;
        // Decode against the spans the index records for this slice, one per sequence it
        // holds. If the index understated a span, the decoder's checksum or bounds check fails
        // or (a multi-reference slice carries no reference checksum) a returned record lies
        // outside the span it was decoded against; the slice is then decoded once more against
        // whole sequences.
        let mut decoded = Vec::new();
        let mut attempt = spans
            .iter()
            .filter_map(|(sequence, span)| {
                Some((reference_sequence_name(header, *sequence)?, *span))
            })
            .collect::<NamedCramSliceSpans>();
        loop {
            set_span(attempt.clone())?;
            let served = queried_sequence
                .as_ref()
                .and_then(|name| attempt.get(name))
                .copied();
            slice_span
                .reference_unavailable
                .store(false, Ordering::Relaxed);
            let outcome = (|| -> Result<(), AlignmentError> {
                let records = slice.records(
                    repository.clone(),
                    header,
                    &compression_header,
                    &core_data_src,
                    &external_data_srcs,
                )?;
                for (record_index, record) in records.iter().enumerate() {
                    // Test placement on the lazy CRAM record: most records of a slice lie
                    // outside a narrow query and never need an owned copy.
                    let Some((start, end)) =
                        noodles_record_span_on(record, header, interval.contig())?
                    else {
                        continue;
                    };
                    if !placement_intersects((start, end), interval) {
                        continue;
                    }
                    require_record_inside_served_span(record, (start, end), served)?;
                    let record_index = u32::try_from(record_index)
                        .map_err(|_| AlignmentError::CoordinateOverflow)?;
                    let record =
                        sam::alignment::RecordBuf::try_from_alignment_record(header, record)?;
                    decoded.push((record, record_index));
                }
                Ok(())
            })();
            match outcome {
                Ok(()) => break,
                Err(_)
                    if retry_slice_against_whole_sequence(
                        !attempt.is_empty(),
                        slice_span.reference_unavailable.load(Ordering::Relaxed),
                    ) =>
                {
                    decoded.clear();
                    attempt.clear();
                }
                Err(error) => {
                    set_span(NamedCramSliceSpans::new())?;
                    return Err(error);
                }
            }
        }
        set_span(NamedCramSliceSpans::new())?;
        for (record, record_index) in decoded {
            visit(
                record,
                crate::AlignmentSourcePosition::cram_record(
                    container_offset,
                    landmark,
                    record_index,
                ),
            )?;
        }
    }
    Ok(())
}

/// Streams records of the CRAM slices CRAI selects for `interval`, decoding one container at
/// a time. Each record keeps a physical address which is stable across independent genomic
/// query boundaries. At most one container's intersecting records are held at once.
struct PositionedCramRecords<'a, R> {
    reader: &'a mut cram::io::IndexedReader<R>,
    header: &'a sam::Header,
    interval: GenomicInterval,
    slice_span: &'a CramSliceSpan,
    containers: std::collections::btree_map::IntoIter<u64, SelectedCramSlices>,
    decoded: std::vec::IntoIter<(sam::alignment::RecordBuf, crate::AlignmentSourcePosition)>,
    finished: bool,
}

impl<R> Iterator for PositionedCramRecords<'_, R>
where
    R: io::Read + io::Seek,
{
    type Item = Result<
        (
            sam::alignment::RecordBuf,
            Option<crate::AlignmentSourcePosition>,
        ),
        AlignmentError,
    >;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((record, position)) = self.decoded.next() {
                return Some(Ok((record, Some(position))));
            }
            if self.finished {
                return None;
            }
            let Some((container_offset, selected_slices)) = self.containers.next() else {
                self.finished = true;
                return None;
            };
            let mut decoded = Vec::new();
            if let Err(error) = visit_cram_container_records(
                self.reader,
                self.header,
                self.interval,
                container_offset,
                &selected_slices,
                self.slice_span,
                |record, position| {
                    decoded.push((record, position));
                    Ok(())
                },
            ) {
                self.finished = true;
                return Some(Err(error));
            }
            self.decoded = decoded.into_iter();
        }
    }
}

fn positioned_cram_records<'a, R>(
    reader: &'a mut cram::io::IndexedReader<R>,
    header: &'a sam::Header,
    interval: GenomicInterval,
    slice_span: &'a CramSliceSpan,
) -> Result<PositionedCramRecords<'a, R>, AlignmentError>
where
    R: io::Read + io::Seek,
{
    let containers = select_cram_containers(reader, interval)?.into_iter();
    Ok(PositionedCramRecords {
        reader,
        header,
        interval,
        slice_span,
        containers,
        decoded: Vec::new().into_iter(),
        finished: false,
    })
}

/// Contig and zero-based half-open reference span of a placed BAM record, read from the
/// record's bytes. It answers what [`noodles_record_span_on`] does for any noodles record,
/// without the boxed CIGAR iteration of the generic record trait.
fn bam_record_placement(
    record: &bam::Record,
) -> Result<Option<(ContigId, u64, u64)>, AlignmentError> {
    let Some(reference_sequence_id) = record.reference_sequence_id().transpose()? else {
        return Ok(None);
    };
    let Some(contig) = u32::try_from(reference_sequence_id).ok().map(ContigId::new) else {
        return Ok(None);
    };
    let Some(start) = record.alignment_start().transpose()? else {
        return Ok(None);
    };
    let start = u64::try_from(start.get())
        .map_err(|_| AlignmentError::CoordinateOverflow)?
        .saturating_sub(1);
    let mut end = start;
    for chunk in record.cigar().as_bytes().chunks_exact(4) {
        let raw = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        match raw & 0xf {
            // M, D, N, = and X consume the reference.
            0 | 2 | 3 | 7 | 8 => end = end.saturating_add(u64::from(raw >> 4)),
            1 | 4 | 5 | 6 => {}
            _ => {
                return Err(AlignmentError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "alignment has an unknown CIGAR operation",
                )));
            }
        }
    }
    Ok(Some((contig, start, end)))
}

/// Whether a BAM record with this placement is returned by a regional query of `interval`.
fn bam_placement_intersects(
    placement: Option<(ContigId, u64, u64)>,
    interval: GenomicInterval,
) -> bool {
    placement.is_some_and(|(contig, start, end)| {
        contig == interval.contig() && placement_intersects((start, end), interval)
    })
}

/// Whether a BAM record placed on the interval's contig starts at or beyond the interval's
/// end. An indexed BAM is coordinate-sorted, so every later record of the file does too.
fn bam_placement_starts_at_or_after_end(
    placement: Option<(ContigId, u64, u64)>,
    interval: GenomicInterval,
) -> bool {
    placement
        .is_some_and(|(contig, start, _)| contig == interval.contig() && start >= interval.end())
}

/// Whether a record placed on `(start, end)` is returned by a regional query of `interval`. A
/// placed record that consumes no reference base (an unmapped mate placed at its partner, or a
/// CIGAR of `*`) occupies one base, as in `HTSlib` and samtools.
fn placement_intersects((start, end): (u64, u64), interval: GenomicInterval) -> bool {
    start < interval.end() && interval.start() < end.max(start.saturating_add(1))
}

/// Zero-based half-open reference span of a record placed on `contig`.
fn noodles_record_span_on<R>(
    record: &R,
    header: &sam::Header,
    contig: ContigId,
) -> Result<Option<(u64, u64)>, AlignmentError>
where
    R: sam::alignment::Record,
{
    let Some(reference_sequence_id) = record.reference_sequence_id(header).transpose()? else {
        return Ok(None);
    };
    if u32::try_from(reference_sequence_id).ok().map(ContigId::new) != Some(contig) {
        return Ok(None);
    }
    let Some(start) = record.alignment_start().transpose()? else {
        return Ok(None);
    };
    let start = u64::try_from(start.get())
        .map_err(|_| AlignmentError::CoordinateOverflow)?
        .saturating_sub(1);
    Ok(Some((start, record_reference_end(record, start)?)))
}

fn query_positioned_noodles_records<R, I>(
    records: I,
    header: &sam::Header,
    interval: GenomicInterval,
    options: &QueryOptions,
    normalization_mapped_records: Option<u64>,
    metadata: Arc<AlignmentHeaderMetadata>,
    keep_going: &dyn Fn() -> bool,
) -> Result<AlignmentQuery, AlignmentError>
where
    R: AsBackendRecord,
    I: IntoIterator<Item = Result<(R, Option<crate::AlignmentSourcePosition>), AlignmentError>>,
{
    let mut accumulator =
        RegionQueryAccumulator::new(interval, options, normalization_mapped_records);
    for result in records {
        let (record, source_position) = result?;
        if !accumulator.add(&record.backend_record(header), source_position, keep_going)? {
            break;
        }
    }
    if accumulator.cancelled() {
        return Ok(cancelled_query(interval, metadata));
    }
    accumulator.finish(metadata)
}

enum RemoteIndex {
    Bai(bam::bai::Index),
    Csi(noodles::csi::Index),
}

impl RemoteIndex {
    fn query(
        &self,
        reference_sequence_id: usize,
        interval: noodles::core::region::Interval,
    ) -> io::Result<Vec<noodles::csi::binning_index::index::reference_sequence::bin::Chunk>> {
        match self {
            Self::Bai(index) => index.query(reference_sequence_id, interval),
            Self::Csi(index) => index.query(reference_sequence_id, interval),
        }
    }
}

fn remote_index_mapped_records(index: &RemoteIndex) -> Option<u64> {
    match index {
        RemoteIndex::Bai(index) => binning_index_mapped_records(index),
        RemoteIndex::Csi(index) => binning_index_mapped_records(index),
    }
}

fn binning_index_mapped_records(index: &impl BinningIndex) -> Option<u64> {
    let mut metadata_present = false;
    let total = BinningIndex::reference_sequences(index).fold(0_u64, |total, reference| {
        reference.metadata().map_or(total, |metadata| {
            metadata_present = true;
            total.saturating_add(metadata.mapped_record_count())
        })
    });
    (metadata_present && total > 0).then_some(total)
}

fn open_index(
    bam_url: &str,
    explicit_url: Option<&str>,
    config: HttpRangeConfig,
) -> Result<(RemoteIndex, RemoteIoStats, [u8; 32]), AlignmentError> {
    if let Some(url) = explicit_url {
        return if url_path_has_extension(url, "csi") {
            read_index(url, config, Some(RemoteIndexKind::Csi))
        } else if url_path_has_extension(url, "bai") {
            read_index(url, config, Some(RemoteIndexKind::Bai))
        } else {
            read_index(url, config, None)
        };
    }
    let candidates = [
        (
            associated_remote_url(bam_url, ".bai")?,
            RemoteIndexKind::Bai,
        ),
        (
            remote_url_with_extension(bam_url, "bai")?,
            RemoteIndexKind::Bai,
        ),
        (
            associated_remote_url(bam_url, ".csi")?,
            RemoteIndexKind::Csi,
        ),
        (
            remote_url_with_extension(bam_url, "csi")?,
            RemoteIndexKind::Csi,
        ),
    ];
    let mut previous_url = None;
    let mut last_not_found = None;
    for (url, kind) in candidates {
        if previous_url.as_deref() == Some(url.as_str()) {
            continue;
        }
        previous_url = Some(url.clone());
        match read_index(&url, config.clone(), Some(kind)) {
            Ok(index) => return Ok(index),
            Err(error) if remote_not_found(&error) => last_not_found = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_not_found.expect("the conventional index candidate list is nonempty"))
}

fn open_cram_index(
    cram_url: &str,
    explicit_url: Option<&str>,
    config: HttpRangeConfig,
    source_length: u64,
) -> Result<(cram::crai::Index, RemoteIoStats, [u8; 32]), AlignmentError> {
    if let Some(url) = explicit_url {
        return read_cram_index(url, config, source_length);
    }
    let candidates = [
        associated_remote_url(cram_url, ".crai")?,
        remote_url_with_extension(cram_url, "crai")?,
    ];
    let mut previous_url = None;
    let mut last_not_found = None;
    for url in candidates {
        if previous_url.as_deref() == Some(url.as_str()) {
            continue;
        }
        previous_url = Some(url.clone());
        match read_cram_index(&url, config.clone(), source_length) {
            Ok(index) => return Ok(index),
            Err(error) if remote_not_found(&error) => last_not_found = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last_not_found.expect("the conventional CRAM index candidate list is nonempty"))
}

fn read_cram_index(
    url: &str,
    config: HttpRangeConfig,
    source_length: u64,
) -> Result<(cram::crai::Index, RemoteIoStats, [u8; 32]), AlignmentError> {
    let (compressed, stats) =
        read_remote_object_bounded(url, config, MAX_REMOTE_CRAI_COMPRESSED_BYTES)?;
    let index = parse_cram_index(&compressed, source_length)?;
    let digest = digest_bytes(&compressed);
    Ok((index, stats, digest))
}

pub(crate) fn parse_cram_index(
    compressed: &[u8],
    source_length: u64,
) -> Result<cram::crai::Index, AlignmentError> {
    let mut decoder = GzDecoder::new(compressed)
        .take((MAX_REMOTE_CRAI_DECOMPRESSED_BYTES as u64).saturating_add(1));
    let mut index_text_bytes = Vec::new();
    decoder.read_to_end(&mut index_text_bytes)?;
    if index_text_bytes.len() > MAX_REMOTE_CRAI_DECOMPRESSED_BYTES {
        return Err(AlignmentError::InvalidCramIndex(format!(
            "decompressed CRAI exceeds {MAX_REMOTE_CRAI_DECOMPRESSED_BYTES} bytes"
        )));
    }
    let text = std::str::from_utf8(&index_text_bytes)
        .map_err(|_| AlignmentError::InvalidCramIndex("CRAI is not UTF-8".to_owned()))?;
    let mut index = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        if index.len() >= MAX_REMOTE_CRAI_RECORDS {
            return Err(AlignmentError::InvalidCramIndex(format!(
                "CRAI exceeds {MAX_REMOTE_CRAI_RECORDS} records"
            )));
        }
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 6 {
            return Err(AlignmentError::InvalidCramIndex(format!(
                "line {} does not contain six fields",
                line_index + 1
            )));
        }
        let invalid = |field: &str| {
            AlignmentError::InvalidCramIndex(format!(
                "line {} has an invalid {field}",
                line_index + 1
            ))
        };
        let raw_reference_id = fields[0]
            .parse::<i64>()
            .map_err(|_| invalid("reference ID"))?;
        let reference_id = match raw_reference_id {
            -1 => None,
            value if value >= 0 => {
                Some(usize::try_from(value).map_err(|_| invalid("reference ID"))?)
            }
            _ => return Err(invalid("reference ID")),
        };
        let raw_start = fields[1]
            .parse::<usize>()
            .map_err(|_| invalid("alignment start"))?;
        let alignment_start = if raw_start == 0 {
            None
        } else {
            Position::new(raw_start)
        };
        if reference_id.is_some() && alignment_start.is_none() {
            return Err(invalid("alignment start"));
        }
        let alignment_span = fields[2]
            .parse::<usize>()
            .map_err(|_| invalid("alignment span"))?;
        let offset = fields[3]
            .parse::<u64>()
            .map_err(|_| invalid("container offset"))?;
        let landmark = fields[4]
            .parse::<u64>()
            .map_err(|_| invalid("slice landmark"))?;
        let slice_length = fields[5]
            .parse::<u64>()
            .map_err(|_| invalid("slice length"))?;
        if offset >= source_length
            || slice_length == 0
            || offset.saturating_add(slice_length) > source_length
        {
            return Err(AlignmentError::InvalidCramIndex(format!(
                "line {} points outside the CRAM object",
                line_index + 1
            )));
        }
        index.push(cram::crai::Record::new(
            reference_id,
            alignment_start,
            alignment_span,
            offset,
            landmark,
            slice_length,
        ));
    }
    if index.is_empty() {
        return Err(AlignmentError::InvalidCramIndex(
            "CRAI contains no records".to_owned(),
        ));
    }
    Ok(index)
}

#[derive(Clone, Copy)]
enum RemoteIndexKind {
    Bai,
    Csi,
}

fn read_index(
    url: &str,
    config: HttpRangeConfig,
    kind: Option<RemoteIndexKind>,
) -> Result<(RemoteIndex, RemoteIoStats, [u8; 32]), AlignmentError> {
    let (bytes, stats) = read_remote_object_bounded(url, config, MAX_REMOTE_ALIGNMENT_INDEX_BYTES)?;
    let parse_bai = || {
        let mut reader = bam::bai::io::Reader::new(Cursor::new(bytes.as_slice()));
        reader.read_index().map(RemoteIndex::Bai)
    };
    let parse_csi = || {
        let mut reader = noodles::csi::io::Reader::new(Cursor::new(bytes.as_slice()));
        reader.read_index().map(RemoteIndex::Csi)
    };
    let index = match kind {
        Some(RemoteIndexKind::Bai) => parse_bai()?,
        Some(RemoteIndexKind::Csi) => parse_csi()?,
        None => parse_bai().or_else(|_| parse_csi())?,
    };
    let digest = digest_bytes(&bytes);
    Ok((index, stats, digest))
}

fn remote_not_found(error: &AlignmentError) -> bool {
    matches!(
        error,
        AlignmentError::Remote(RemoteError::Http(error))
            if error.status() == Some(reqwest::StatusCode::NOT_FOUND)
    )
}

fn url_path_has_extension(url: &str, extension: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| {
        std::path::Path::new(url.path())
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    })
}

fn combine_stats(left: RemoteIoStats, right: RemoteIoStats) -> RemoteIoStats {
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

fn cancelled_query(
    interval: GenomicInterval,
    metadata: Arc<AlignmentHeaderMetadata>,
) -> AlignmentQuery {
    AlignmentQuery {
        interval,
        header: metadata,
        batch: AlignmentBatch::default(),
        pileup: PileupSummary::default(),
        junctions: JunctionSummary::default(),
        pairs: crate::PairSummary::default(),
        reference: None,
        stats: QueryStats {
            cancelled: true,
            ..QueryStats::default()
        },
    }
}

fn record_reference_end(
    record: &dyn sam::alignment::Record,
    start: u64,
) -> Result<u64, AlignmentError> {
    let mut end = start;
    for result in record.cigar().iter() {
        let operation = result?;
        if matches!(
            operation.kind(),
            Kind::Match
                | Kind::Deletion
                | Kind::Skip
                | Kind::SequenceMatch
                | Kind::SequenceMismatch
        ) {
            end = end.saturating_add(operation.len() as u64);
        }
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs::{self, File},
        num::NonZero,
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::Duration,
    };

    use noodles::sam::{
        alignment::{
            RecordBuf,
            io::Write as _,
            record::{
                Flags, MappingQuality,
                cigar::{Op, op::Kind as CigarKind},
            },
            record_buf::{Data, QualityScores, Sequence, data::field::Value as OwnedValue},
        },
        header::record::value::{Map, map::ReferenceSequence},
    };
    use tiny_http::{Header, Response, Server, StatusCode};

    mod coverage;
    mod differential;
    mod in_memory;
    mod resume;
    mod url_routing;

    use super::*;
    use crate::{
        AlignmentRecord, QueryDetail,
        backend_record::{Eligibility, eligibility},
    };

    #[test]
    fn cram_alignment_and_reference_share_one_transfer_budget() {
        let total = RemoteTransferBudget {
            maximum_requests: 65,
            maximum_response_bytes: 33,
        };
        let (alignment, reference) = split_remote_transfer_budget(total);
        assert_eq!(alignment.maximum_requests, 33);
        assert_eq!(reference.maximum_requests, 32);
        assert_eq!(
            alignment
                .maximum_requests
                .saturating_add(reference.maximum_requests),
            total.maximum_requests
        );
        assert_eq!(alignment.maximum_response_bytes, 17);
        assert_eq!(reference.maximum_response_bytes, 16);
        assert_eq!(
            alignment
                .maximum_response_bytes
                .saturating_add(reference.maximum_response_bytes),
            total.maximum_response_bytes
        );
    }

    struct BamFixture {
        bam_url: String,
        index_url: String,
        requested_urls: Arc<Mutex<Vec<String>>>,
        /// Inclusive byte ranges served, with the object path, in request order.
        served_ranges: Arc<Mutex<Vec<(String, usize, usize)>>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl BamFixture {
        fn start(bam: Vec<u8>, index: Vec<u8>) -> Option<Self> {
            Self::start_with_indexes(
                bam,
                vec![
                    ("/custom.bai".to_owned(), index.clone()),
                    ("/fixture.bam.bai".to_owned(), index),
                ],
                "/custom.bai",
            )
        }

        fn start_with_index(bam: Vec<u8>, index: Vec<u8>, index_path: &str) -> Option<Self> {
            Self::start_with_indexes(bam, vec![(index_path.to_owned(), index)], index_path)
        }

        fn start_with_indexes(
            bam: Vec<u8>,
            indexes: Vec<(String, Vec<u8>)>,
            explicit_index_path: &str,
        ) -> Option<Self> {
            let server = Server::http("127.0.0.1:0").ok()?;
            let address = server.server_addr().to_ip()?;
            let mut objects = Vec::with_capacity(indexes.len().saturating_add(1));
            objects.push(("/fixture.bam".to_owned(), Arc::new(bam)));
            objects.extend(
                indexes
                    .into_iter()
                    .map(|(path, bytes)| (path, Arc::new(bytes))),
            );
            let objects = Arc::new(objects);
            let requested_urls = Arc::new(Mutex::new(Vec::new()));
            let worker_requested_urls = Arc::clone(&requested_urls);
            let served_ranges = Arc::new(Mutex::new(Vec::new()));
            let worker_served_ranges = Arc::clone(&served_ranges);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                        continue;
                    };
                    worker_requested_urls
                        .lock()
                        .unwrap()
                        .push(request.url().to_owned());
                    let path = request.url().split('?').next().unwrap_or_default();
                    let Some((_, data)) = objects.iter().find(|(candidate, _)| candidate == path)
                    else {
                        request.respond(Response::empty(StatusCode(404))).unwrap();
                        continue;
                    };
                    let range = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| parse_range(header.value.as_str(), data.len()));
                    let Some((start, end)) = range else {
                        request.respond(Response::empty(StatusCode(416))).unwrap();
                        continue;
                    };
                    worker_served_ranges
                        .lock()
                        .unwrap()
                        .push((path.to_owned(), start, end));
                    let response = Response::from_data(data[start..=end].to_vec())
                        .with_status_code(StatusCode(206))
                        .with_header(header(
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", data.len()),
                        ))
                        .with_header(header("Accept-Ranges", "bytes"));
                    request.respond(response).unwrap();
                }
            });
            Some(Self {
                bam_url: format!("http://{address}/fixture.bam?signature=source"),
                index_url: format!("http://{address}{explicit_index_path}?signature=index"),
                requested_urls,
                served_ranges,
                stop,
                worker: Some(worker),
            })
        }

        fn requested_urls(&self) -> Vec<String> {
            self.requested_urls.lock().unwrap().clone()
        }

        /// Inclusive BAM byte ranges served since the first `skip` of them.
        fn bam_ranges_after(&self, skip: usize) -> Vec<(u64, u64)> {
            self.served_ranges
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _, _)| path == "/fixture.bam")
                .skip(skip)
                .map(|(_, start, end)| (*start as u64, *end as u64))
                .collect()
        }
    }

    impl Drop for BamFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    struct CramFixture {
        cram_url: String,
        fallback_cram_url: String,
        corrupt_cram_url: String,
        index_url: String,
        reference_url: String,
        reference_index_url: String,
        local_cram: std::path::PathBuf,
        local_reference: std::path::PathBuf,
        directory: tempfile::TempDir,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl CramFixture {
        #[allow(clippy::too_many_lines)]
        fn start() -> Option<Self> {
            let directory = tempfile::tempdir().ok()?;
            let reference_bases = (0..400).map(|index| b"ACGT"[index % 4]).collect::<Vec<_>>();
            let local_reference = directory.path().join("fixture.fa");
            let mut fasta_bytes = b">chr1\n".to_vec();
            fasta_bytes.extend_from_slice(&reference_bases);
            fasta_bytes.push(b'\n');
            fs::write(&local_reference, &fasta_bytes).ok()?;
            let fai_bytes = format!(
                "chr1\t{}\t6\t{}\t{}\n",
                reference_bases.len(),
                reference_bases.len(),
                reference_bases.len() + 1
            )
            .into_bytes();
            fs::write(local_reference.with_extension("fa.fai"), &fai_bytes).ok()?;

            let reference_record = fasta::Record::new(
                fasta::record::Definition::new("chr1", None),
                fasta::record::Sequence::from(reference_bases.clone()),
            );
            let sam_header = sam::Header::builder()
                .add_reference_sequence(
                    "chr1",
                    Map::<ReferenceSequence>::new(NonZero::new(reference_bases.len()).unwrap()),
                )
                .build();
            let mut data = Data::default();
            data.insert(
                sam::alignment::record::data::field::Tag::EDIT_DISTANCE,
                OwnedValue::from(2),
            );
            data.insert(
                sam::alignment::record::data::field::Tag::ALIGNMENT_SCORE,
                OwnedValue::from(37),
            );
            let record = RecordBuf::builder()
                .set_name("remote-cram-read")
                .set_flags(Flags::empty())
                .set_reference_sequence_id(0)
                .set_alignment_start(Position::try_from(21).unwrap())
                .set_mapping_quality(MappingQuality::new(48).unwrap())
                .set_cigar(
                    [
                        Op::new(CigarKind::Match, 4),
                        Op::new(CigarKind::Insertion, 1),
                        Op::new(CigarKind::Match, 3),
                        Op::new(CigarKind::Deletion, 1),
                        Op::new(CigarKind::Match, 4),
                    ]
                    .into_iter()
                    .collect(),
                )
                .set_sequence(Sequence::from(b"ACGTGACGACGT".as_slice()))
                .set_quality_scores(QualityScores::from(vec![35; 12]))
                .set_data(data.clone())
                .build();
            let second_record = RecordBuf::builder()
                .set_name("remote-cram-read-2")
                .set_flags(Flags::empty())
                .set_reference_sequence_id(0)
                .set_alignment_start(Position::try_from(41).unwrap())
                .set_mapping_quality(MappingQuality::new(48).unwrap())
                .set_cigar([Op::new(CigarKind::Match, 12)].into_iter().collect())
                .set_sequence(Sequence::from(b"ACGTACGTACGT".as_slice()))
                .set_quality_scores(QualityScores::from(vec![35; 12]))
                .set_data(data)
                .build();
            let local_cram = directory.path().join("fixture.cram");
            let file = File::create(&local_cram).ok()?;
            let repository = fasta::Repository::new(vec![reference_record]);
            let mut writer = cram::io::writer::Builder::default()
                .set_reference_sequence_repository(repository)
                .build_from_writer(file);
            writer.write_header(&sam_header).ok()?;
            writer.write_alignment_record(&sam_header, &record).ok()?;
            writer
                .write_alignment_record(&sam_header, &second_record)
                .ok()?;
            writer.try_finish(&sam_header).ok()?;
            drop(writer);
            let index = cram::fs::index(&local_cram).ok()?;
            let mut index_writer = cram::crai::io::Writer::new(Vec::new());
            index_writer.write_index(&index).ok()?;
            let index_bytes = index_writer.finish().ok()?;
            let local_index = Path::new(&format!("{}.crai", local_cram.display())).to_path_buf();
            fs::write(&local_index, &index_bytes).ok()?;
            let alignment_bytes = fs::read(&local_cram).ok()?;
            let corrupt_alignment_bytes = alignment_bytes[..alignment_bytes.len().min(64)].to_vec();

            let server = Server::http("127.0.0.1:0").ok()?;
            let address = server.server_addr().to_ip()?;
            let objects = Arc::new([
                ("/fixture.cram", Arc::new(alignment_bytes.clone())),
                ("/fallback.cram", Arc::new(alignment_bytes)),
                ("/corrupt.cram", Arc::new(corrupt_alignment_bytes)),
                ("/private-index", Arc::new(index_bytes.clone())),
                ("/fixture.cram.crai", Arc::new(index_bytes.clone())),
                ("/fallback.crai", Arc::new(index_bytes)),
                ("/fixture.fa", Arc::new(fasta_bytes)),
                ("/private-reference-index", Arc::new(fai_bytes)),
            ]);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(20)) else {
                        continue;
                    };
                    let authenticated = request.headers().iter().any(|header| {
                        header.field.equiv("Authorization")
                            && header.value.as_str() == "Bearer remote-cram-secret"
                    });
                    if !authenticated {
                        request.respond(Response::empty(StatusCode(401))).unwrap();
                        continue;
                    }
                    let path = request.url().split('?').next().unwrap_or_default();
                    let Some((_, data)) = objects.iter().find(|(candidate, _)| *candidate == path)
                    else {
                        request.respond(Response::empty(StatusCode(404))).unwrap();
                        continue;
                    };
                    let range = request
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Range"))
                        .and_then(|header| parse_range(header.value.as_str(), data.len()));
                    let Some((start, end)) = range else {
                        request.respond(Response::empty(StatusCode(416))).unwrap();
                        continue;
                    };
                    let response = Response::from_data(data[start..=end].to_vec())
                        .with_status_code(StatusCode(206))
                        .with_header(header(
                            "Content-Range",
                            format!("bytes {start}-{end}/{}", data.len()),
                        ))
                        .with_header(header("Accept-Ranges", "bytes"))
                        .with_header(header("ETag", "\"remote-cram-fixture\""));
                    request.respond(response).unwrap();
                }
            });
            Some(Self {
                cram_url: format!("http://{address}/fixture.cram?signature=source"),
                fallback_cram_url: format!("http://{address}/fallback.cram?signature=source"),
                corrupt_cram_url: format!("http://{address}/corrupt.cram?signature=source"),
                index_url: format!("http://{address}/private-index?signature=index"),
                reference_url: format!("http://{address}/fixture.fa?signature=reference"),
                reference_index_url: format!(
                    "http://{address}/private-reference-index?signature=fai"
                ),
                local_cram,
                local_reference,
                directory,
                stop,
                worker: Some(worker),
            })
        }
    }

    impl Drop for CramFixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(worker) = self.worker.take() {
                worker.join().unwrap();
            }
        }
    }

    fn parse_range(value: &str, length: usize) -> Option<(usize, usize)> {
        let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
        let start = start.parse().ok()?;
        let end = end.parse::<usize>().ok()?.min(length.checked_sub(1)?);
        (start <= end).then_some((start, end))
    }

    fn header(name: &str, value: impl Into<String>) -> Header {
        Header::from_bytes(name.as_bytes(), value.into().as_bytes()).unwrap()
    }

    fn gzip_bytes(value: &[u8]) -> Vec<u8> {
        use std::io::Write as _;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(value).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn placed_unmapped_record_is_ineligible_before_all_visibility_filters() {
        let header = sam::Header::builder()
            .add_reference_sequence(
                "chr1",
                Map::<ReferenceSequence>::new(NonZero::new(1_000).unwrap()),
            )
            .build();
        let record = RecordBuf::builder()
            .set_name("placed-unmapped")
            .set_flags(Flags::from_bits_truncate(0x04 | 0x100 | 0x800))
            .set_reference_sequence_id(0)
            .set_alignment_start(Position::try_from(101).unwrap())
            .set_mapping_quality(MappingQuality::new(60).unwrap())
            .set_cigar(
                [Op::new(CigarKind::SequenceMatch, 20)]
                    .into_iter()
                    .collect(),
            )
            .set_sequence(Sequence::from(b"ACGTACGTACGTACGTACGT".as_slice()))
            .set_quality_scores(QualityScores::from(vec![35; 20]))
            .build();
        let options = QueryOptions {
            include_secondary: true,
            include_supplementary: true,
            ..QueryOptions::default()
        };

        assert_eq!(
            eligibility(
                &record.backend_record(&header),
                crate::AlignmentFlags::from_bits(record.flags().bits()),
                &options
            )
            .unwrap(),
            Eligibility::Unmapped
        );
    }

    fn assert_mismatched_cram_reference_is_rejected(
        fixture: &CramFixture,
        interval: GenomicInterval,
    ) {
        let mismatched_reference = fixture.directory.path().join("mismatched.fa");
        let mut mismatched_reference_bytes = b">chr1\n".to_vec();
        mismatched_reference_bytes.extend(std::iter::repeat_n(b'T', 400));
        mismatched_reference_bytes.push(b'\n');
        fs::write(&mismatched_reference, mismatched_reference_bytes).unwrap();
        fs::write(
            mismatched_reference.with_extension("fa.fai"),
            b"chr1\t400\t6\t400\t401\n",
        )
        .unwrap();
        let result = ProductionAlignmentSource::open(
            &fixture.local_cram,
            &AlignmentOpenOptions {
                reference_path: Some(mismatched_reference),
                ..AlignmentOpenOptions::default()
            },
        )
        .and_then(|mut source| source.query(interval, QueryOptions::default(), &|| true));
        assert!(result.is_err(), "a mismatched CRAM reference was accepted");
    }

    #[test]
    fn cram_index_parser_rejects_unbounded_or_out_of_object_geometry() {
        let outside = gzip_bytes(b"0\t1\t10\t95\t0\t10\n");
        assert!(matches!(
            parse_cram_index(&outside, 100),
            Err(AlignmentError::InvalidCramIndex(message))
                if message.contains("outside")
        ));

        let missing_start = gzip_bytes(b"0\t0\t10\t20\t0\t10\n");
        assert!(matches!(
            parse_cram_index(&missing_start, 100),
            Err(AlignmentError::InvalidCramIndex(message))
                if message.contains("alignment start")
        ));

        let extra_field = gzip_bytes(b"0\t1\t10\t20\t0\t10\textra\n");
        assert!(matches!(
            parse_cram_index(&extra_field, 100),
            Err(AlignmentError::InvalidCramIndex(message))
                if message.contains("six fields")
        ));
    }

    /// Drains a positioned stream and checks its physical-ownership invariants.
    fn drain_positioned_stream(
        source: &mut ProductionAlignmentSource,
        interval: GenomicInterval,
        options: QueryOptions,
    ) -> (
        Vec<(crate::AlignmentSourcePosition, AlignmentRecord)>,
        QueryStats,
        usize,
    ) {
        source
            .begin_positioned_stream_with_admission(interval, interval, options)
            .unwrap();
        let mut records = Vec::new();
        let mut owners = BTreeSet::new();
        let mut positions = BTreeSet::new();
        loop {
            match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
                crate::PositionedAlignmentStreamCheckpoint::Block(block) => {
                    let Some(first) = block.records.first() else {
                        continue;
                    };
                    let owner = owner_of(first.source_position);
                    assert!(owners.insert(owner), "one owner chunk was returned twice");
                    for positioned in block.records {
                        assert_eq!(owner_of(positioned.source_position), owner);
                        assert!(positions.insert(positioned.source_position));
                        records.push((positioned.source_position, positioned.record));
                    }
                }
                crate::PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                    assert_eq!(completion.interval, interval);
                    return (records, completion.stats, owners.len());
                }
                crate::PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                    panic!("an uncancelled stream reported cancellation")
                }
            }
        }
    }

    fn owner_of(position: crate::AlignmentSourcePosition) -> (u64, u64) {
        match position {
            crate::AlignmentSourcePosition::BamVirtualOffset(offset) => (offset >> 16, 0),
            crate::AlignmentSourcePosition::CramRecord {
                container_offset,
                slice_landmark,
                ..
            } => (container_offset, slice_landmark),
        }
    }

    fn assert_stream_matches_query(
        source: &mut ProductionAlignmentSource,
        interval: GenomicInterval,
        options: &QueryOptions,
    ) -> usize {
        let query = source.query(interval, options.clone(), &|| true).unwrap();
        let (mut streamed, stats, owners) =
            drain_positioned_stream(source, interval, options.clone());
        assert_eq!(stats.records_seen, query.stats.records_seen);
        assert_eq!(stats.records_eligible, query.stats.records_eligible);
        assert_eq!(stats.records_emitted, query.stats.records_emitted);
        assert_eq!(stats.individual_reads, query.stats.individual_reads);
        assert_eq!(streamed.len(), query.batch.len());
        streamed.sort_by_key(|(position, _)| *position);
        let mut expected = (0..query.batch.len())
            .map(|index| {
                (
                    query.batch.source_position(index).unwrap(),
                    query.batch.materialize_record(index).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        expected.sort_by_key(|(position, _)| *position);
        assert_eq!(streamed, expected);
        owners
    }

    #[test]
    fn remote_bam_positioned_stream_matches_the_one_shot_query() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let index_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
        if !path.exists() || !index_path.exists() {
            return;
        }
        let Some(fixture) =
            BamFixture::start(fs::read(&path).unwrap(), fs::read(index_path).unwrap())
        else {
            return;
        };
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            ..AlignmentOpenOptions::default()
        };
        let mut source = ProductionAlignmentSource::open(&fixture.bam_url, &options).unwrap();
        assert!(matches!(source, ProductionAlignmentSource::RemoteBam(_)));
        let query_options = QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        };
        let broad = GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap();
        let owners = assert_stream_matches_query(&mut source, broad, &query_options);
        assert!(owners > 1, "the fixture should span several BGZF blocks");

        // A record keeps one physical identity whichever window acquires it.
        let narrow = GenomicInterval::new(ContigId::new(1), 400_000, 600_000).unwrap();
        assert_stream_matches_query(&mut source, narrow, &query_options);
        let (broad_records, _, _) =
            drain_positioned_stream(&mut source, broad, query_options.clone());
        let (narrow_records, _, _) = drain_positioned_stream(&mut source, narrow, query_options);
        assert!(!narrow_records.is_empty());
        for narrow_record in &narrow_records {
            assert!(broad_records.contains(narrow_record));
        }

        // Retargeting abandons the unfinished transaction without corrupting the next one.
        source
            .begin_positioned_stream_with_admission(broad, broad, QueryOptions::default())
            .unwrap();
        let _ = source.next_positioned_stream_checkpoint(&|| true).unwrap();
        source.cancel_positioned_stream();
        assert!(matches!(
            source.next_positioned_stream_checkpoint(&|| true),
            Err(AlignmentError::NoActivePositionedStream)
        ));
    }

    #[test]
    fn remote_bam_positioned_stream_refuses_dense_targets_without_retaining_records() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let index_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
        if !path.exists() || !index_path.exists() {
            return;
        }
        let Some(fixture) =
            BamFixture::start(fs::read(&path).unwrap(), fs::read(index_path).unwrap())
        else {
            return;
        };
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            ..AlignmentOpenOptions::default()
        };
        let mut source = ProductionAlignmentSource::open(&fixture.bam_url, &options).unwrap();
        let interval = GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap();
        let (records, stats, _) = drain_positioned_stream(
            &mut source,
            interval,
            QueryOptions {
                max_records: std::num::NonZeroUsize::new(10).unwrap(),
                ..QueryOptions::default()
            },
        );
        assert_eq!(records.len(), 10);
        assert_eq!(stats.records_emitted, 0);
        assert!(stats.complete_read_records_eligible > 10);
        assert_eq!(
            stats.individual_reads,
            crate::IndividualReadStatus::TooDense { limit: 10 }
        );
    }

    #[test]
    fn reference_compressed_cram_without_a_reference_fails_with_an_actionable_error() {
        let cram = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmark-data/manual-suite/data/alignments/reference-dependent.cram");
        if !cram.exists() {
            return;
        }
        let mut source =
            ProductionAlignmentSource::open(&cram, &AlignmentOpenOptions::default()).unwrap();
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100_000).unwrap();
        let error = source
            .query(interval, QueryOptions::default(), &|| true)
            .unwrap_err();
        assert!(error.to_string().contains("load the matching"), "{error}");
        source
            .begin_positioned_stream_with_admission(interval, interval, QueryOptions::default())
            .unwrap();
        let error = source
            .next_positioned_stream_checkpoint(&|| true)
            .unwrap_err();
        assert!(error.to_string().contains("load the matching"), "{error}");
    }

    #[test]
    fn cram_slices_decode_against_their_own_reference_span_and_match_htslib() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/manual-suite");
        let cram = root.join("data/alignments/reference-dependent.cram");
        let reference = root.join("data/references/manual.fa");
        if !cram.exists() || !reference.exists() {
            return;
        }
        let options = AlignmentOpenOptions {
            reference_path: Some(reference),
            ..AlignmentOpenOptions::default()
        };
        let query_options = QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        };
        let mut source = LocalCramAlignmentSource::open(&cram, &options).unwrap();
        let mut oracle = LocalAlignmentSource::open(&cram, &options).unwrap();
        let mut compared = 0;
        for (index, contig) in source.reference_dictionary().clone().iter().enumerate() {
            let interval =
                GenomicInterval::new(ContigId::new(index as u32), 0, contig.length()).unwrap();
            let decoded = source
                .query(interval, query_options.clone(), &|| true)
                .unwrap();
            if decoded.batch.is_empty() {
                continue;
            }
            // Only the slices' spans were read from the reference, never the whole sequence.
            let loaded = source.reference_query_bases.load(Ordering::Relaxed);
            assert!(loaded > 0, "the fixture is reference-compressed");
            assert!(
                (loaded as u64) < contig.length(),
                "loaded {loaded} reference bases of a {}-base sequence",
                contig.length()
            );
            let expected = oracle
                .query(interval, query_options.clone(), &|| true)
                .unwrap();
            assert_eq!(decoded.batch.len(), expected.batch.len());
            let key = |record: &AlignmentRecord| {
                (
                    record.start,
                    record.end,
                    record.read_name.clone(),
                    record.flags.bits(),
                )
            };
            let mut ours = (0..decoded.batch.len())
                .map(|index| decoded.batch.materialize_record(index).unwrap())
                .collect::<Vec<_>>();
            let mut theirs = (0..expected.batch.len())
                .map(|index| expected.batch.materialize_record(index).unwrap())
                .collect::<Vec<_>>();
            ours.sort_by_key(key);
            theirs.sort_by_key(key);
            for (ours, theirs) in ours.iter().zip(&theirs) {
                assert_eq!(key(ours), key(theirs));
                assert_eq!(ours.cigar, theirs.cigar);
                assert_eq!(ours.sequence, theirs.sequence, "bases and qualities");
                compared += 1;
            }
        }
        assert!(compared > 0);
    }

    /// Three 2000-base contigs and, on each, five 50-base reads at 100, 300, 500, 700 and 900
    /// (one mismatch each), written as one reference-compressed, indexed CRAM slice: a
    /// multi-reference slice.
    fn write_multi_reference_cram(directory: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        const CONTIG_BASES: usize = 2_000;
        let names = ["ctgA", "ctgB", "ctgC"];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let sequences = names.map(|_| {
            (0..CONTIG_BASES)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    b"ACGT"[(state >> 33) as usize % 4]
                })
                .collect::<Vec<u8>>()
        });
        let reference = directory.join("multi.fa");
        let mut fasta_bytes = Vec::new();
        let mut fai = Vec::new();
        for (name, sequence) in names.iter().zip(&sequences) {
            fasta_bytes.extend_from_slice(format!(">{name}\n").as_bytes());
            fai.extend_from_slice(
                format!(
                    "{name}\t{CONTIG_BASES}\t{}\t{CONTIG_BASES}\t{}\n",
                    fasta_bytes.len(),
                    CONTIG_BASES + 1
                )
                .as_bytes(),
            );
            fasta_bytes.extend_from_slice(sequence);
            fasta_bytes.push(b'\n');
        }
        fs::write(&reference, fasta_bytes).unwrap();
        fs::write(directory.join("multi.fa.fai"), fai).unwrap();

        let mut sam_header = sam::Header::builder();
        for name in names {
            sam_header = sam_header.add_reference_sequence(
                name,
                Map::<ReferenceSequence>::new(NonZero::new(CONTIG_BASES).unwrap()),
            );
        }
        let sam_header = sam_header.build();
        let repository = fasta::Repository::new(
            names
                .iter()
                .zip(&sequences)
                .map(|(name, sequence)| {
                    fasta::Record::new(
                        fasta::record::Definition::new(*name, None),
                        fasta::record::Sequence::from(sequence.clone()),
                    )
                })
                .collect::<Vec<_>>(),
        );
        let cram = directory.join("multi.cram");
        let mut writer = cram::io::writer::Builder::default()
            .set_reference_sequence_repository(repository)
            .build_from_writer(File::create(&cram).unwrap());
        writer.write_header(&sam_header).unwrap();
        for (reference_sequence_id, sequence) in sequences.iter().enumerate() {
            for start in [100_usize, 300, 500, 700, 900] {
                let mut bases = sequence[start..start + 50].to_vec();
                bases[25] = if bases[25] == b'A' { b'C' } else { b'A' };
                let record = RecordBuf::builder()
                    .set_name(format!("read-{reference_sequence_id}-{start}"))
                    .set_flags(Flags::empty())
                    .set_reference_sequence_id(reference_sequence_id)
                    .set_alignment_start(Position::try_from(start + 1).unwrap())
                    .set_mapping_quality(MappingQuality::new(60).unwrap())
                    .set_cigar([Op::new(CigarKind::Match, 50)].into_iter().collect())
                    .set_sequence(Sequence::from(bases))
                    .set_quality_scores(QualityScores::from(vec![30; 50]))
                    .build();
                writer.write_alignment_record(&sam_header, &record).unwrap();
            }
        }
        writer.try_finish(&sam_header).unwrap();
        drop(writer);
        // htslib indexes a multi-reference slice from its record placements alone.
        rust_htslib::bam::index::build(&cram, None, rust_htslib::bam::index::Type::Bai, 1).unwrap();
        (cram, reference)
    }

    fn assert_cram_matches_htslib(
        source: &mut LocalCramAlignmentSource,
        oracle: &mut LocalAlignmentSource,
        contig: u32,
    ) {
        let query_options = QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        };
        let interval = GenomicInterval::new(ContigId::new(contig), 0, 2_000).unwrap();
        let decoded = source
            .query(interval, query_options.clone(), &|| true)
            .unwrap();
        let expected = oracle.query(interval, query_options, &|| true).unwrap();
        let records = |query: &AlignmentQuery| {
            let mut records = (0..query.batch.len())
                .map(|index| query.batch.materialize_record(index).unwrap())
                .map(|record| {
                    (
                        record.start,
                        record.read_name,
                        record.cigar,
                        record.sequence,
                    )
                })
                .collect::<Vec<_>>();
            records.sort_by(|left, right| (left.0, &left.1).cmp(&(right.0, &right.1)));
            records
        };
        assert_eq!(decoded.batch.len(), 5, "contig {contig}");
        assert_eq!(records(&decoded), records(&expected), "contig {contig}");
    }

    #[test]
    fn multi_reference_cram_slices_fetch_only_the_spans_they_cover() {
        let directory = tempfile::tempdir().unwrap();
        let (cram, reference) = write_multi_reference_cram(directory.path());
        // The premise: one slice is indexed under all three sequences, each with the span of
        // its own reads, 100..950 (one-based start 101, span 850).
        let index = cram::crai::fs::read(format!("{}.crai", cram.display())).unwrap();
        let slices = index
            .iter()
            .map(|record| (record.offset(), record.landmark()))
            .collect::<BTreeSet<_>>();
        assert_eq!(slices.len(), 1, "htslib wrote one multi-reference slice");
        assert_eq!(
            index
                .iter()
                .map(|record| (
                    record.reference_sequence_id(),
                    record.alignment_start().map(usize::from),
                    record.alignment_span()
                ))
                .collect::<Vec<_>>(),
            [
                (Some(0), Some(101), 850),
                (Some(1), Some(101), 850),
                (Some(2), Some(101), 850)
            ]
        );

        let options = AlignmentOpenOptions {
            reference_path: Some(reference),
            ..AlignmentOpenOptions::default()
        };
        let mut source = LocalCramAlignmentSource::open(&cram, &options).unwrap();
        let mut oracle = LocalAlignmentSource::open(&cram, &options).unwrap();
        for contig in 0..3 {
            assert_cram_matches_htslib(&mut source, &mut oracle, contig);
            // Decoding the slice resolves the reference of all three sequences: 3 x 850 bases,
            // where it used to read all 3 x 2000.
            assert_eq!(
                source.reference_query_bases.load(Ordering::Relaxed),
                2_550,
                "contig {contig}"
            );
        }
    }

    #[test]
    fn an_index_understating_a_multi_reference_span_falls_back_to_whole_sequences() {
        use std::io::Write as _;

        let directory = tempfile::tempdir().unwrap();
        let (cram, reference) = write_multi_reference_cram(directory.path());
        // Rewrite the index so every sequence claims only 300..950: the read at 100 lies
        // outside. A multi-reference slice carries no reference checksum and the read is inside
        // the zero-filled buffer, so nothing but the returned records' own placement can expose
        // the understated span.
        let index = cram::crai::fs::read(format!("{}.crai", cram.display())).unwrap();
        let understated = directory.path().join("understated.crai");
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(&understated).unwrap(),
            flate2::Compression::default(),
        );
        for record in &index {
            writeln!(
                encoder,
                "{}\t301\t650\t{}\t{}\t{}",
                record.reference_sequence_id().unwrap(),
                record.offset(),
                record.landmark(),
                record.slice_length()
            )
            .unwrap();
        }
        encoder.finish().unwrap();

        let options = AlignmentOpenOptions {
            reference_path: Some(reference),
            ..AlignmentOpenOptions::default()
        };
        let mut source = LocalCramAlignmentSource::open(
            &cram,
            &AlignmentOpenOptions {
                index_path: Some(understated),
                ..options.clone()
            },
        )
        .unwrap();
        let mut oracle = LocalAlignmentSource::open(&cram, &options).unwrap();
        for contig in 0..3 {
            // Bases still match htslib: the slice was decoded again against whole sequences
            // (3 x 650 claimed bases, then 3 x 2000).
            assert_cram_matches_htslib(&mut source, &mut oracle, contig);
            assert_eq!(
                source.reference_query_bases.load(Ordering::Relaxed),
                1_950 + 6_000,
                "contig {contig}"
            );
        }
    }

    #[test]
    fn local_cram_positioned_stream_matches_the_one_shot_query() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/manual-suite");
        let cram = root.join("data/alignments/reference-dependent.cram");
        let reference = root.join("data/references/manual.fa");
        if !cram.exists() || !reference.exists() {
            return;
        }
        let options = AlignmentOpenOptions {
            reference_path: Some(reference),
            ..AlignmentOpenOptions::default()
        };
        let mut source = ProductionAlignmentSource::open(&cram, &options).unwrap();
        assert!(matches!(source, ProductionAlignmentSource::LocalCram(_)));
        assert!(source.is_cram());
        let query_options = QueryOptions {
            detail: QueryDetail::Full,
            ..QueryOptions::default()
        };
        let mut total = 0;
        for (index, contig) in source.reference_dictionary().clone().iter().enumerate() {
            let interval =
                GenomicInterval::new(ContigId::new(index as u32), 0, contig.length()).unwrap();
            let before = source
                .query(interval, query_options.clone(), &|| true)
                .unwrap()
                .batch
                .len();
            assert_stream_matches_query(&mut source, interval, &query_options);
            total += before;
        }
        assert!(total > 0, "the CRAM fixture should contain mapped records");
    }

    /// A coordinate-sorted BAM of incompressible 150 bp reads every 100 bp of one contig.
    fn write_dense_bam(path: &Path, records: u32) {
        use rust_htslib::bam::{
            Format, Header, Record, Writer,
            header::HeaderRecord,
            record::{Cigar, CigarString},
        };

        let mut header = Header::new();
        header.push_record(
            HeaderRecord::new(b"HD")
                .push_tag(b"VN", "1.6")
                .push_tag(b"SO", "coordinate"),
        );
        header.push_record(
            HeaderRecord::new(b"SQ")
                .push_tag(b"SN", "chr1")
                .push_tag(b"LN", 16_000_000),
        );
        let mut writer = Writer::from_path(path, &header, Format::Bam).unwrap();
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut random = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u32
        };
        let cigar = CigarString(vec![Cigar::Match(150)]);
        for index in 0..records {
            let sequence = (0..150)
                .map(|_| b"ACGT"[(random() % 4) as usize])
                .collect::<Vec<_>>();
            let qualities = (0..150).map(|_| (random() % 41) as u8).collect::<Vec<_>>();
            let mut record = Record::new();
            record.set(
                format!("read{index}").as_bytes(),
                Some(&cigar),
                &sequence,
                &qualities,
            );
            record.set_tid(0);
            record.set_pos(i64::from(index) * 100);
            record.set_mapq(60);
            record.set_flags(0);
            writer.write(&record).unwrap();
        }
        drop(writer);
        let index = format!("{}.bai", path.display());
        rust_htslib::bam::index::build(
            path,
            Some(Path::new(&index)),
            rust_htslib::bam::index::Type::Bai,
            1,
        )
        .unwrap();
    }

    /// The compressed span of the index chunks of a region of the first contig, and their
    /// number, derived from the BAI independently of the reader under test.
    fn bai_compressed_span(index: &bam::bai::Index, start: usize, end: usize) -> (u64, u64, usize) {
        let interval = noodles::core::Position::try_from(start + 1).unwrap()
            ..=noodles::core::Position::try_from(end).unwrap();
        let chunks = index.query(0, interval.into()).unwrap();
        (
            chunks
                .iter()
                .map(|chunk| chunk.start().compressed())
                .min()
                .unwrap(),
            chunks
                .iter()
                .map(|chunk| chunk.end().compressed())
                .max()
                .unwrap(),
            chunks.len(),
        )
    }

    /// Queries a region remotely, checks it against the local source and against exact
    /// request/byte accounting, and returns the BAM ranges the server sent for it.
    fn query_and_served_ranges(
        fixture: &BamFixture,
        source: &mut RemoteBamAlignmentSource,
        local: &mut LocalAlignmentSource,
        start: u64,
        end: u64,
    ) -> (AlignmentQuery, Vec<(u64, u64)>) {
        let interval = GenomicInterval::new(ContigId::new(0), start, end).unwrap();
        let served = fixture.bam_ranges_after(0).len();
        let before = source.remote_io_stats();
        let query = source
            .query(interval, QueryOptions::default(), &|| true)
            .unwrap();
        let expected = local
            .query(interval, QueryOptions::default(), &|| true)
            .unwrap();
        assert_eq!(query.stats.records_seen, expected.stats.records_seen);
        assert_eq!(query.batch.len(), expected.batch.len());
        assert_eq!(
            query.stats.records_eligible,
            expected.stats.records_eligible
        );
        assert!(query.stats.records_eligible > 0);
        let ranges = fixture.bam_ranges_after(served);
        let after = source.remote_io_stats();
        assert_eq!(after.requests - before.requests, ranges.len() as u64);
        assert_eq!(
            after.response_bytes - before.response_bytes,
            ranges
                .iter()
                .map(|(start, end)| end - start + 1)
                .sum::<u64>()
        );
        (query, ranges)
    }

    #[test]
    fn remote_bam_fetches_index_chunks_instead_of_a_fixed_read_ahead_window() {
        const BGZF_BLOCK: u64 = 64 * 1024;
        const LEGACY_WINDOW: u64 = 8 * 256 * 1024;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dense.bam");
        write_dense_bam(&path, 120_000);
        let bam = fs::read(&path).unwrap();
        let bam_length = bam.len() as u64;
        assert!(
            bam_length > 3 * LEGACY_WINDOW,
            "fixture is only {bam_length} bytes"
        );
        let index_path = format!("{}.bai", path.display());
        let index = bam::bai::io::Reader::new(File::open(&index_path).unwrap())
            .read_index()
            .unwrap();
        let Some(fixture) = BamFixture::start(bam, fs::read(&index_path).unwrap()) else {
            return;
        };
        // The production defaults: 8 x 256 KiB read-ahead for anything unplanned.
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            ..AlignmentOpenOptions::default()
        };
        let mut source = RemoteBamAlignmentSource::open(&fixture.bam_url, &options).unwrap();
        let mut local =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let implied_span = |start, end| bai_compressed_span(&index, start, end);
        let mut run = |source: &mut RemoteBamAlignmentSource, start: u64, end: u64| {
            query_and_served_ranges(&fixture, source, &mut local, start, end)
        };

        // A 2 kb region deep inside the file: one request starting at the first index chunk
        // and a fraction of the old 2 MiB window.
        let (small, ranges) = run(&mut source, 11_500_000, 11_502_000);
        assert_eq!(small.stats.records_seen, 21);
        let (first, last, chunk_count) = implied_span(11_500_000, 11_502_000);
        assert!(chunk_count >= 1);
        assert_eq!(ranges.len(), 1, "{ranges:?}");
        assert_eq!(ranges[0].0, first);
        let transferred = ranges[0].1 - ranges[0].0 + 1;
        assert!(
            transferred <= (last - first) + BGZF_BLOCK,
            "transferred {transferred} for an index span of {}",
            last - first
        );
        assert!(
            transferred < LEGACY_WINDOW / 4,
            "a small region transferred {transferred} bytes"
        );

        // Revisiting it costs nothing.
        let (_, ranges) = run(&mut source, 11_500_500, 11_501_500);
        assert!(ranges.is_empty(), "{ranges:?}");

        // A 9 Mb region spanning dozens of 256 KiB cache blocks, beyond the bytes the header
        // read already cached: contiguous ranges that start at the first chunk, double from
        // 2 MiB to 8 MiB and stop within one BGZF block of the last record needed, instead of
        // one request per block or per 2 MiB window.
        let (deep, ranges) = run(&mut source, 2_000_000, 11_000_000);
        assert_eq!(deep.stats.records_seen, 90_001);
        let (first, last, _) = implied_span(2_000_000, 11_000_000);
        assert!(first > LEGACY_WINDOW);
        let blocks = (last - first) / (256 * 1024);
        assert!(blocks > 32, "the region spans only {blocks} cache blocks");
        assert_eq!(ranges.len(), 4, "{blocks} blocks: {ranges:?}");
        assert!((ranges.len() as u64) * 2 <= (last - first).div_ceil(LEGACY_WINDOW));
        assert_eq!(ranges[0], (first, first + LEGACY_WINDOW - 1));
        assert_eq!(
            ranges[1],
            (ranges[0].1 + 1, ranges[0].1 + 2 * LEGACY_WINDOW)
        );
        assert_eq!(
            ranges[2],
            (ranges[1].1 + 1, ranges[1].1 + 4 * LEGACY_WINDOW)
        );
        assert_eq!(ranges[3].0, ranges[2].1 + 1);
        assert!(ranges[3].1 < bam_length);
        let transferred = ranges
            .iter()
            .map(|(start, end)| end - start + 1)
            .sum::<u64>();
        assert!(
            transferred <= (last - first) + BGZF_BLOCK,
            "transferred {transferred} for an index span of {}",
            last - first
        );
    }

    #[test]
    fn remote_bam_query_stops_fetching_once_cancelled_mid_stream() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let index_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
        if !path.exists() || !index_path.exists() {
            return;
        }
        let Some(fixture) =
            BamFixture::start(fs::read(&path).unwrap(), fs::read(index_path).unwrap())
        else {
            return;
        };
        // Small uncoalesced ranges make the bytes transferred track the records read.
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            remote_range: HttpRangeConfig {
                chunk_size: std::num::NonZeroUsize::new(64 * 1024).unwrap(),
                minimum_coalesced_chunks: std::num::NonZeroUsize::MIN,
                maximum_coalesced_chunks: std::num::NonZeroUsize::MIN,
                ..HttpRangeConfig::default()
            },
            ..AlignmentOpenOptions::default()
        };
        let interval = GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap();
        let query_options = QueryOptions {
            cancellation_chunk: std::num::NonZeroUsize::new(8).unwrap(),
            ..QueryOptions::default()
        };
        let transferred = |keep_going: &dyn Fn() -> bool| {
            let mut source = RemoteBamAlignmentSource::open(&fixture.bam_url, &options).unwrap();
            let opened = source.remote_io_stats().response_bytes;
            let query = source
                .query(interval, query_options.clone(), keep_going)
                .unwrap();
            (query, source.remote_io_stats().response_bytes - opened)
        };

        let (complete, complete_bytes) = transferred(&|| true);
        assert_eq!(complete.stats.records_seen, 2_000);
        assert!(!complete.stats.cancelled);
        assert!(
            complete_bytes > 8 * 64 * 1024,
            "the region must span many ranges for this test to mean anything ({complete_bytes})"
        );

        // Proceed at the entry check and the first two record checkpoints, then cancel.
        let checks = std::sync::atomic::AtomicUsize::new(0);
        let (cancelled, cancelled_bytes) =
            transferred(&|| checks.fetch_add(1, Ordering::Relaxed) < 3);
        assert!(cancelled.stats.cancelled);
        assert!(cancelled.batch.is_empty());
        // The records were never fetched, rather than fetched and then discarded.
        assert!(
            cancelled_bytes * 4 < complete_bytes,
            "cancelled query transferred {cancelled_bytes} of {complete_bytes} bytes"
        );
    }

    #[test]
    fn unavailable_cram_reference_never_escalates_to_a_whole_sequence_fetch() {
        use fasta::repository::Adapter as _;

        // Only a decode failure against an indexed span justifies decoding once more against
        // the whole sequence.
        assert!(retry_slice_against_whole_sequence(true, false));
        assert!(!retry_slice_against_whole_sequence(true, true));
        assert!(!retry_slice_against_whole_sequence(false, false));
        assert!(!retry_slice_against_whole_sequence(false, true));

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("genome.2bit");
        fs::write(
            &path,
            crate::two_bit::tests::encode_two_bit(&[("chrA", "ACGTACGTACGTACGT")], false),
        )
        .unwrap();
        let slice_span = CramSliceSpan::default();
        let query_bases = Arc::new(AtomicUsize::new(0));
        let mut adapter = CramReferenceAdapter {
            reference: Arc::new(Mutex::new(FastaReference::open(&path).unwrap())),
            query_bases: Arc::clone(&query_bases),
            slice_span: Arc::clone(&slice_span),
        };
        let unavailable = || slice_span.reference_unavailable.load(Ordering::Relaxed);

        // A served span leaves the flag clear: a later decode error is about the slice.
        slice_span.lock().unwrap().insert(b"chrA".to_vec(), (4, 12));
        let record = adapter.get(b"chrA").unwrap().unwrap();
        assert_eq!(&record.sequence().as_ref()[4..], b"ACGTACGT");
        assert_eq!(query_bases.load(Ordering::Relaxed), 8);
        assert!(!unavailable());

        // A contig the reference lacks, and an exhausted base budget, are reference failures.
        assert!(adapter.get(b"chrMissing").unwrap().is_err());
        assert!(unavailable());
        slice_span
            .reference_unavailable
            .store(false, Ordering::Relaxed);
        query_bases.store(MAX_REMOTE_CRAM_REFERENCE_BASES, Ordering::Relaxed);
        let error = adapter.get(b"chrA").unwrap().unwrap_err();
        assert!(error.to_string().contains("reference budget exceeded"));
        assert!(unavailable());
    }

    #[test]
    fn remote_bam_uses_custom_index_and_preserves_complex_records() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let index_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
        if !path.exists() || !index_path.exists() {
            return;
        }
        let Some(fixture) =
            BamFixture::start(fs::read(&path).unwrap(), fs::read(index_path).unwrap())
        else {
            return;
        };
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            ..AlignmentOpenOptions::default()
        };
        let mut source = RemoteBamAlignmentSource::open(&fixture.bam_url, &options).unwrap();
        let remote_stamp = source
            .source_stamp()
            .expect("remote BAM has a validator stamp");
        let query = source
            .query(
                GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap(),
                QueryOptions {
                    detail: QueryDetail::Layout,
                    collect_modifications: true,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        assert_eq!(query.stats.records_seen, 2_000);
        assert!(query.batch.validate());
        assert!((0..query.batch.len()).any(|index| {
            query
                .batch
                .flags(index)
                .expect("validated flags")
                .is_supplementary()
        }));
        assert!((0..query.batch.len()).any(|index| query.batch.has_auxiliary_tag(index, *b"MM")));
        let mut local =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        assert!(local.source_stamp().is_some());
        let mirrored = LocalAlignmentSource::open(
            &path,
            &AlignmentOpenOptions {
                source_stamp_override: Some(remote_stamp),
                ..AlignmentOpenOptions::default()
            },
        )
        .unwrap();
        assert_eq!(mirrored.source_stamp(), Some(remote_stamp));
        let local_query = local
            .query(
                query.interval,
                QueryOptions {
                    detail: QueryDetail::Full,
                    collect_modifications: true,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        assert_eq!(
            (0..query.batch.len())
                .map(|index| query.batch.source_position(index))
                .collect::<Vec<_>>(),
            (0..local_query.batch.len())
                .map(|index| local_query.batch.source_position(index))
                .collect::<Vec<_>>()
        );
        assert_eq!(query.pileup, local_query.pileup);
        let pair_interval = GenomicInterval::new(ContigId::new(0), 0, 200_000).unwrap();
        let remote_pairs = source
            .query(pair_interval, QueryOptions::default(), &|| true)
            .unwrap();
        let local_pairs = local
            .query(pair_interval, QueryOptions::default(), &|| true)
            .unwrap();
        assert_eq!(remote_pairs.pairs, local_pairs.pairs);
        assert_eq!(remote_pairs.pairs.median_template_length, Some(166));
        let refused_options = QueryOptions {
            max_records: std::num::NonZeroUsize::new(1).unwrap(),
            ..QueryOptions::default()
        };
        let remote_refused = source
            .query(pair_interval, refused_options.clone(), &|| true)
            .unwrap();
        let local_refused = local
            .query(pair_interval, refused_options, &|| true)
            .unwrap();
        assert_eq!(
            remote_refused.stats.records_eligible,
            local_refused.stats.records_eligible
        );
        assert_eq!(remote_refused.stats.records_emitted, 0);
        assert_eq!(remote_refused.stats.complete_read_limit, 1);
        assert_eq!(
            remote_refused.stats.individual_reads,
            crate::IndividualReadStatus::TooDense { limit: 1 }
        );
        assert!(remote_refused.batch.is_empty());
        assert_eq!(remote_refused.pileup, local_refused.pileup);
        assert_eq!(remote_refused.pairs, local_refused.pairs);
        let junction_interval =
            GenomicInterval::new(ContigId::new(0), 2_100_000, 2_301_100).unwrap();
        let remote_junctions = source
            .query(junction_interval, QueryOptions::default(), &|| true)
            .unwrap();
        let local_junctions = local
            .query(junction_interval, QueryOptions::default(), &|| true)
            .unwrap();
        assert_eq!(remote_junctions.junctions, local_junctions.junctions);
        assert_eq!(remote_junctions.junctions.junctions.len(), 10_000);
        let stats = source.remote_io_stats();
        assert!(stats.requests > 0);
        assert!(stats.response_bytes < fs::metadata(path).unwrap().len());

        let conventional =
            ProductionAlignmentSource::open(&fixture.bam_url, &AlignmentOpenOptions::default())
                .unwrap();
        assert_eq!(
            conventional.reference_dictionary(),
            source.reference_dictionary()
        );
        assert!(conventional.remote_io_stats().is_some());
    }

    #[test]
    fn remote_bam_qualifies_all_conventional_and_custom_bai_csi_names() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let bai_path = Path::new(&format!("{}.bai", path.display())).to_path_buf();
        if !path.exists() || !bai_path.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let csi_path = directory.path().join("mixed.csi");
        rust_htslib::bam::index::build(
            &path,
            Some(&csi_path),
            rust_htslib::bam::index::Type::Csi(14),
            1,
        )
        .unwrap();
        let bam = fs::read(&path).unwrap();
        let bai = fs::read(bai_path).unwrap();
        let csi = fs::read(csi_path).unwrap();
        let cases = [
            ("/fixture.bam.bai", false, bai.clone()),
            ("/fixture.bai", false, bai),
            ("/fixture.bam.csi", false, csi.clone()),
            ("/fixture.csi", false, csi.clone()),
            ("/private-index", true, csi),
        ];

        for (index_path, explicit, index) in cases {
            let Some(fixture) = BamFixture::start_with_index(bam.clone(), index, index_path) else {
                return;
            };
            let options = AlignmentOpenOptions {
                remote_index_url: explicit.then(|| fixture.index_url.clone()),
                ..AlignmentOpenOptions::default()
            };
            let mut source = ProductionAlignmentSource::open(&fixture.bam_url, &options).unwrap();
            assert!(matches!(source, ProductionAlignmentSource::RemoteBam(_)));
            let query = source
                .query(
                    GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap(),
                    QueryOptions::default(),
                    &|| true,
                )
                .unwrap();
            assert_eq!(query.stats.records_seen, 2_000, "index path {index_path}");
            assert!(query.batch.validate());
            let signature = if explicit { "index" } else { "source" };
            assert!(
                fixture
                    .requested_urls()
                    .iter()
                    .any(|url| url == &format!("{index_path}?signature={signature}")),
                "index path {index_path} was not requested with its signed query"
            );
        }
    }

    #[test]
    fn corrupt_remote_bam_index_fails_explicitly_without_opening_a_source() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let Some(fixture) = BamFixture::start_with_index(
            fs::read(path).unwrap(),
            b"not a BAI".to_vec(),
            "/bad.bai",
        ) else {
            return;
        };
        let options = AlignmentOpenOptions {
            remote_index_url: Some(fixture.index_url.clone()),
            ..AlignmentOpenOptions::default()
        };
        let Err(error) = RemoteBamAlignmentSource::open(&fixture.bam_url, &options) else {
            panic!("a corrupt remote BAI unexpectedly opened");
        };
        assert!(matches!(error, AlignmentError::Io(_)));
        assert!(error.to_string().contains("invalid"));
    }

    #[test]
    fn remote_cram_is_bounded_observable_authenticated_and_semantically_equivalent() {
        let Some(fixture) = CramFixture::start() else {
            return;
        };
        let remote_range = HttpRangeConfig {
            chunk_size: std::num::NonZeroUsize::new(64).unwrap(),
            maximum_cache_bytes: std::num::NonZeroUsize::new(4 * 1024).unwrap(),
            authentication: crate::RemoteAuthentication::Bearer("remote-cram-secret".to_owned()),
            ..HttpRangeConfig::default()
        };
        let options = AlignmentOpenOptions {
            reference_path: Some(fixture.reference_url.clone().into()),
            remote_index_url: Some(fixture.index_url.clone()),
            remote_reference_index_url: Some(fixture.reference_index_url.clone()),
            remote_range,
            ..AlignmentOpenOptions::default()
        };
        let mut remote = ProductionAlignmentSource::open(&fixture.cram_url, &options).unwrap();
        assert!(matches!(remote, ProductionAlignmentSource::RemoteCram(_)));
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let query_options = QueryOptions {
            detail: QueryDetail::Full,
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let remote_query = remote
            .query(interval, query_options.clone(), &|| true)
            .unwrap();
        assert_eq!(remote_query.stats.records_seen, 2);
        assert_eq!(remote_query.stats.records_emitted, 2);
        assert_eq!(remote_query.batch.len(), 2);
        let broad_positions = (0..remote_query.batch.len())
            .map(|index| remote_query.batch.source_position(index).unwrap())
            .collect::<Vec<_>>();
        assert_ne!(broad_positions[0], broad_positions[1]);
        assert_eq!(
            remote_query.batch.materialize_record(0).unwrap().read_name,
            b"remote-cram-read"
        );
        assert!(remote_query.batch.has_auxiliary_tag(0, *b"NM"));
        let stats = remote.remote_io_stats().unwrap();
        assert!(
            stats.requests >= 8,
            "expected CRAM, CRAI, FASTA, and FAI probes and reads"
        );
        assert!(stats.response_bytes > 0);
        assert!(stats.cache_misses > 0);

        let local_options = AlignmentOpenOptions {
            reference_path: Some(fixture.local_reference.clone()),
            ..AlignmentOpenOptions::default()
        };
        let mut local =
            ProductionAlignmentSource::open(&fixture.local_cram, &local_options).unwrap();
        let local_query = local.query(interval, query_options, &|| true).unwrap();
        assert!(matches!(local, ProductionAlignmentSource::LocalCram(_)));
        assert_eq!(
            (0..remote_query.batch.len())
                .map(|index| remote_query.batch.source_position(index))
                .collect::<Vec<_>>(),
            (0..local_query.batch.len())
                .map(|index| local_query.batch.source_position(index))
                .collect::<Vec<_>>()
        );
        let mut remote_record = remote_query.batch.materialize_record(0).unwrap();
        let mut local_record = local_query.batch.materialize_record(0).unwrap();
        // HTSlib synthesizes an MD tag while decoding CRAM; Noodles exposes only the stored
        // auxiliary fields. Core alignment, sequence, quality, CIGAR, and pileup semantics must
        // still agree exactly.
        remote_record.auxiliary_fields.clear();
        local_record.auxiliary_fields.clear();
        assert_eq!(remote_record, local_record);
        assert_eq!(remote_query.pileup, local_query.pileup);
        assert_eq!(remote_query.junctions, local_query.junctions);
        assert_eq!(remote_query.pairs, local_query.pairs);
        let narrow = remote
            .query(
                GenomicInterval::new(ContigId::new(0), 20, 35).unwrap(),
                QueryOptions::default(),
                &|| true,
            )
            .unwrap();
        assert_eq!(narrow.batch.len(), 1);
        assert_eq!(narrow.batch.source_position(0), Some(broad_positions[0]));
        let refusal_options = QueryOptions {
            max_records: std::num::NonZeroUsize::new(1).unwrap(),
            ..QueryOptions::default()
        };
        let remote_refused = remote
            .query(interval, refusal_options.clone(), &|| true)
            .unwrap();
        let local_refused = local.query(interval, refusal_options, &|| true).unwrap();
        assert_eq!(remote_refused.stats.records_eligible, 2);
        assert_eq!(remote_refused.stats.records_emitted, 0);
        assert_eq!(
            remote_refused.stats.individual_reads,
            crate::IndividualReadStatus::TooDense { limit: 1 }
        );
        assert!(remote_refused.batch.is_empty());
        assert_eq!(remote_refused.pileup, local_refused.pileup);
        assert_eq!(remote_refused.junctions, local_refused.junctions);
        assert_eq!(remote_refused.pairs, local_refused.pairs);

        let conventional_options = AlignmentOpenOptions {
            remote_index_url: None,
            ..options.clone()
        };
        let mut conventional =
            ProductionAlignmentSource::open(&fixture.cram_url, &conventional_options).unwrap();
        let conventional_query = conventional
            .query(interval, QueryOptions::default(), &|| true)
            .unwrap();
        assert_eq!(conventional_query.stats.records_emitted, 2);

        let mut fallback =
            ProductionAlignmentSource::open(&fixture.fallback_cram_url, &conventional_options)
                .unwrap();
        let fallback_query = fallback
            .query(interval, QueryOptions::default(), &|| true)
            .unwrap();
        assert_eq!(fallback_query.stats.records_emitted, 2);

        assert_mismatched_cram_reference_is_rejected(&fixture, interval);

        let Err(error) = ProductionAlignmentSource::open(&fixture.corrupt_cram_url, &options)
        else {
            panic!("a truncated remote CRAM unexpectedly opened");
        };
        assert!(
            matches!(
                error,
                AlignmentError::InvalidCramIndex(_) | AlignmentError::Io(_)
            ),
            "unexpected truncated CRAM error: {error}"
        );
    }
}
