//! Local-file front end of the `noodles` BAM decoder (ADR 0002, "Backend convergence", step 1).
//!
//! The record iteration, one-shot fold, positioned streams and resume logic are the ones the
//! remote BAM source uses; only the BGZF stream differs: a file read by `noodles`'
//! multithreaded BGZF reader (or its plain reader when no decompression thread is granted)
//! instead of the HTTP range reader. A SAM file or an unindexed BAM is served the same way from
//! a sorted, indexed BAM held in memory (see `in_memory`).

use std::{
    fs::File,
    io::{self, BufRead as _, Cursor},
    num::NonZero,
    path::Path,
    sync::Arc,
};

use noodles::{
    bam,
    bgzf::{self, VirtualPosition, io::MultithreadedReader},
    sam,
};
use rgv_core::{GenomicInterval, ReferenceDictionary};

use super::{
    BamBlockStream, NoodlesPositionedStream, RemoteIndex, alignment_header_metadata_from_noodles,
    cancelled_query, positioned_bam_records, positioned_stream, remote_bam_region,
    remote_index_mapped_records,
};
use crate::{
    AlignmentError, AlignmentHeaderMetadata, AlignmentOpenOptions, AlignmentQuery, AlignmentSource,
    AlignmentSourceFormat, AlignmentSourceStamp, PositionedAlignmentStreamCheckpoint,
    PositionedChunkReceipt, QueryOptions,
    index_overview::local_alignment_index_candidates,
    local::{DECOMPRESSION_THREADS, DecompressionThreadLease, decompression_thread_limit},
    source_revision::{LocalSourceGuard, local_source_stamp},
};

/// A local BGZF stream: a file inflated on this thread or by a pool of worker threads, or a
/// BAM held in memory.
pub(super) enum LocalBgzf {
    Single(bgzf::io::Reader<File>),
    Memory(bgzf::io::Reader<Cursor<Vec<u8>>>),
    Multi {
        reader: MultithreadedReader<File>,
        /// Length of the file when it was opened.
        file_length: u64,
        /// Set by a seek to or beyond the end of the file: the stream is at its end there.
        /// (The multithreaded reader would keep serving its previous block instead.)
        at_end: Option<VirtualPosition>,
    },
}

impl LocalBgzf {
    fn new(file: File, file_length: u64, worker_threads: u32) -> Self {
        match NonZero::new(worker_threads as usize) {
            Some(workers) => Self::Multi {
                reader: MultithreadedReader::with_worker_count(workers, file),
                file_length,
                at_end: None,
            },
            None => Self::Single(bgzf::io::Reader::new(file)),
        }
    }
}

/// The multithreaded reader reports a block it could not read (an I/O error, or a file cut
/// short inside a block) as the end of the stream. Its compressed position then lies before
/// the end of the file, which is how that is told from a real end.
fn incomplete_stream(position: u64, file_length: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!(
            "the BGZF stream ended at byte {position} of {file_length}: the file is truncated \
             or a block could not be read"
        ),
    )
}

impl io::Read for LocalBgzf {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Single(reader) => reader.read(buffer),
            Self::Memory(reader) => reader.read(buffer),
            Self::Multi { .. } => {
                let mut available = self.fill_buf()?;
                let read = available.read(buffer)?;
                self.consume(read);
                Ok(read)
            }
        }
    }
}

impl io::BufRead for LocalBgzf {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        match self {
            Self::Single(reader) => reader.fill_buf(),
            Self::Memory(reader) => reader.fill_buf(),
            Self::Multi {
                reader,
                file_length,
                at_end,
            } => {
                if at_end.is_some() {
                    return Ok(&[]);
                }
                if reader.fill_buf()?.is_empty() && reader.position() < *file_length {
                    return Err(incomplete_stream(reader.position(), *file_length));
                }
                reader.fill_buf()
            }
        }
    }

    fn consume(&mut self, amount: usize) {
        match self {
            Self::Single(reader) => reader.consume(amount),
            Self::Memory(reader) => reader.consume(amount),
            Self::Multi { reader, at_end, .. } => {
                if at_end.is_none() {
                    reader.consume(amount);
                }
            }
        }
    }
}

impl bgzf::io::Read for LocalBgzf {
    fn virtual_position(&self) -> VirtualPosition {
        match self {
            Self::Single(reader) => reader.virtual_position(),
            Self::Memory(reader) => reader.virtual_position(),
            Self::Multi { reader, at_end, .. } => {
                at_end.unwrap_or_else(|| reader.virtual_position())
            }
        }
    }
}

impl bgzf::io::BufRead for LocalBgzf {}

impl bgzf::io::Seek for LocalBgzf {
    fn seek_to_virtual_position(
        &mut self,
        position: VirtualPosition,
    ) -> io::Result<VirtualPosition> {
        match self {
            Self::Single(reader) => reader.seek_to_virtual_position(position),
            Self::Memory(reader) => reader.seek_to_virtual_position(position),
            Self::Multi {
                reader,
                file_length,
                at_end,
            } => {
                *at_end = None;
                let compressed = position.compressed();
                if compressed >= *file_length {
                    *at_end = Some(position);
                    return Ok(position);
                }
                reader.seek_to_virtual_position(position)?;
                if reader.position() == compressed {
                    // No block arrived although the file continues here.
                    return Err(incomplete_stream(compressed, *file_length));
                }
                Ok(position)
            }
        }
    }

    fn seek_with_index(
        &mut self,
        _index: &bgzf::gzi::Index,
        _position: io::SeekFrom,
    ) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a BAM stream is addressed by virtual position",
        ))
    }
}

impl BamBlockStream for LocalBgzf {}

fn read_local_index(path: &Path) -> io::Result<RemoteIndex> {
    let extension = path
        .extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase());
    match extension.as_deref() {
        Some("csi") => noodles::csi::fs::read(path).map(RemoteIndex::Csi),
        Some("bai") => bam::bai::fs::read(path).map(RemoteIndex::Bai),
        _ => bam::bai::fs::read(path)
            .map(RemoteIndex::Bai)
            .or_else(|_| noodles::csi::fs::read(path).map(RemoteIndex::Csi)),
    }
}

/// Indexed local BAM source decoded by `noodles`: the decoder of [`RemoteBamAlignmentSource`]
/// (the same record iteration, positioned streams and resume logic) over a local file.
///
/// Opened through [`ProductionAlignmentSource::open`] when
/// [`AlignmentOpenOptions::local_bam_backend`] asks for it; the default local BAM decoder is
/// still [`LocalAlignmentSource`]. [`Self::open_in_memory`] serves a SAM file or an unindexed
/// BAM through the same decoder.
///
/// [`RemoteBamAlignmentSource`]: crate::RemoteBamAlignmentSource
/// [`ProductionAlignmentSource::open`]: crate::ProductionAlignmentSource::open
/// [`LocalAlignmentSource`]: crate::LocalAlignmentSource
pub struct LocalBamAlignmentSource {
    pub(super) reader: LocalBgzf,
    pub(super) index: RemoteIndex,
    pub(super) header: sam::Header,
    pub(super) dictionary: ReferenceDictionary,
    pub(super) metadata: Arc<AlignmentHeaderMetadata>,
    pub(super) normalization_mapped_records: Option<u64>,
    pub(super) index_overview: Option<crate::AlignmentIndexOverview>,
    source_stamp: AlignmentSourceStamp,
    source_guard: LocalSourceGuard,
    pub(super) positioned_stream: Option<NoodlesPositionedStream>,
    /// Declared after `reader`: the reader's workers stop before their threads are handed
    /// back to the process-wide allowance.
    decompression_threads: DecompressionThreadLease,
}

impl LocalBamAlignmentSource {
    /// Opens an indexed BAM file. The index is `options.index_path`, or the first of
    /// `<file>.bai`, `<stem>.bai`, `<file>.csi`, `<stem>.csi` that exists.
    pub fn open(
        path: impl AsRef<Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let lease = DECOMPRESSION_THREADS.lease(
            options.decompression_threads.get(),
            decompression_thread_limit(),
        );
        Self::open_with_lease(path.as_ref(), options, lease)
    }

    /// Opens the file with BGZF workers for the threads of `decompression_threads`.
    pub(crate) fn open_with_lease(
        path: &Path,
        options: &AlignmentOpenOptions,
        decompression_threads: DecompressionThreadLease,
    ) -> Result<Self, AlignmentError> {
        if crate::local::has_url_scheme(&path.to_string_lossy()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a local BAM source does not open URLs; http and https URLs are opened \
                 through ProductionAlignmentSource and other schemes are not supported",
            )
            .into());
        }
        let file = File::open(path)?;
        let file_length = file.metadata()?.len();
        let index_path = local_alignment_index_candidates(
            path,
            options.index_path.as_deref(),
            AlignmentSourceFormat::Bam,
        )
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no BAM index (.bai or .csi) was found for the alignment file",
            )
        })?;
        let index = read_local_index(&index_path)?;
        let reader = LocalBgzf::new(file, file_length, decompression_threads.threads);
        Self::assemble(
            reader,
            index,
            path,
            &index_path,
            options,
            decompression_threads,
        )
    }

    /// Loads a plain-text SAM file, or a BAM file that has no index (coordinate-sorted or
    /// not), into memory and serves it like an indexed BAM. The records are sorted by
    /// position and indexed in memory; nothing is written to disk.
    ///
    /// The file is read whole, so it may be at most [`MAX_IN_MEMORY_ALIGNMENT_BYTES`] on
    /// disk, and its records at most that much once encoded as BAM. A larger file fails with
    /// [`std::io::ErrorKind::FileTooLarge`] and a message telling the user to sort and index
    /// it. `options.index_path` is ignored.
    ///
    /// [`MAX_IN_MEMORY_ALIGNMENT_BYTES`]: crate::MAX_IN_MEMORY_ALIGNMENT_BYTES
    pub fn open_in_memory(
        path: impl AsRef<Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        Self::open_in_memory_with_cap(
            path.as_ref(),
            options,
            super::in_memory::MAX_IN_MEMORY_ALIGNMENT_BYTES,
        )
    }

    pub(crate) fn open_in_memory_with_cap(
        path: &Path,
        options: &AlignmentOpenOptions,
        cap: u64,
    ) -> Result<Self, AlignmentError> {
        if crate::local::has_url_scheme(&path.to_string_lossy()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only a local SAM or BAM file can be loaded into memory",
            )
            .into());
        }
        let (bytes, index) = super::in_memory::load_sorted_bam(path, cap)?;
        let reader = LocalBgzf::Memory(bgzf::io::Reader::new(Cursor::new(bytes)));
        // The file is its own index for revision purposes: any change to it is a new source.
        Self::assemble(
            reader,
            RemoteIndex::Bai(index),
            path,
            path,
            options,
            DECOMPRESSION_THREADS.lease(0, decompression_thread_limit()),
        )
    }

    /// Reads the header from `reader` and derives everything a source holds from it and the
    /// index. `revision_path` is the file whose content, with `path`, identifies the source.
    fn assemble(
        mut reader: LocalBgzf,
        index: RemoteIndex,
        path: &Path,
        revision_path: &Path,
        options: &AlignmentOpenOptions,
        decompression_threads: DecompressionThreadLease,
    ) -> Result<Self, AlignmentError> {
        let normalization_mapped_records = remote_index_mapped_records(&index);
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
        let source_guard = LocalSourceGuard::capture(path, revision_path)?;
        let source_stamp = options.source_stamp_override.map_or_else(
            || {
                local_source_stamp(
                    path,
                    revision_path,
                    AlignmentSourceFormat::Bam,
                    &dictionary,
                    &metadata,
                )
            },
            Ok,
        )?;
        Ok(Self {
            reader,
            index,
            header,
            dictionary,
            metadata,
            normalization_mapped_records,
            index_overview,
            source_stamp,
            source_guard,
            positioned_stream: None,
            decompression_threads,
        })
    }

    /// Whether the records are served from memory (a SAM file or an unindexed BAM).
    #[must_use]
    pub const fn is_in_memory(&self) -> bool {
        matches!(self.reader, LocalBgzf::Memory(_))
    }

    /// Immutable parsed SAM header metadata.
    #[must_use]
    pub const fn header_metadata(&self) -> &Arc<AlignmentHeaderMetadata> {
        &self.metadata
    }

    /// Navigation density map derived from the index.
    #[must_use]
    pub const fn index_overview(&self) -> Option<&crate::AlignmentIndexOverview> {
        self.index_overview.as_ref()
    }

    /// Exact mapped-record total from the index metadata, when it has any.
    #[must_use]
    pub const fn normalization_mapped_records(&self) -> Option<u64> {
        self.normalization_mapped_records
    }

    /// BGZF worker threads granted to this source from the process-wide allowance; zero means
    /// blocks are inflated on the calling thread.
    #[must_use]
    pub const fn decompression_threads(&self) -> u32 {
        self.decompression_threads.threads
    }

    pub(super) fn begin_positioned_stream(
        &mut self,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<usize, AlignmentError> {
        self.positioned_stream = None;
        if !self.source_guard.validate()? {
            return Err(AlignmentError::SourceChanged);
        }
        let region = remote_bam_region(&self.dictionary, interval)?;
        let (stream, skipped) = NoodlesPositionedStream::begin_bam(
            &self.index,
            interval,
            admission_interval,
            options,
            &region,
            skip,
        )?;
        self.positioned_stream = Some(stream);
        Ok(skipped)
    }

    pub(super) fn next_positioned_stream_checkpoint(
        &mut self,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
        positioned_stream::next_bam_checkpoint(
            &mut self.positioned_stream,
            &mut self.reader,
            &self.header,
        )
    }
}

impl AlignmentSource for LocalBamAlignmentSource {
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
        let region = remote_bam_region(&self.dictionary, interval)?;
        positioned_bam_records(&mut self.reader, &self.index, interval, &region)?.query(
            &self.header,
            &options,
            self.normalization_mapped_records,
            Arc::clone(&self.metadata),
            keep_going,
        )
    }
}
