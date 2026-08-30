use std::{
    collections::{BTreeMap, BTreeSet},
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
    sam::{
        self,
        alignment::record::{
            cigar::op::Kind,
            data::field::{Value, value::Array},
        },
    },
};
use rgv_core::{ContigId, GenomicInterval, ReferenceDictionary};

use crate::{
    AlignmentBatch, AlignmentError, AlignmentFlags, AlignmentHeaderMetadata, AlignmentOpenOptions,
    AlignmentQuery, AlignmentReadGroup, AlignmentRecord, AlignmentSource, AlignmentSourceFormat,
    AlignmentSourceStamp, AuxComparison, AuxField, AuxValue, CigarOp, CigarOpKind,
    FastaOpenOptions, FastaReference, HttpRangeConfig, HttpRangeReader, JunctionSummary,
    LocalAlignmentSource, MAX_ALIGNMENT_READ_GROUPS, PileupProvenance, PileupSummary, QueryDetail,
    QueryOptions, QueryStats, RemoteCancellation, RemoteError, RemoteIoStats, RemoteTransferBudget,
    SequenceData,
    alignment::FilteredRecordReason,
    associated_remote_url, bisulfite_strand_is_reverse,
    index_overview::local_alignment_index_candidates,
    junction::{
        JunctionStrand, JunctionStrandInput, JunctionSummaryBuilder, RecordJunctionCollector,
    },
    pileup::{
        OverlapCollapser, OverlapMask, PileupBuilder, add_unmasked_range, aligned_observation,
    },
    read_remote_object_bounded, remote_url_with_extension,
    source_revision::{LocalSourceGuard, digest_bytes, local_source_stamp, remote_source_stamp},
};

type NoodlesReader = bam::io::IndexedReader<noodles::bgzf::io::Reader<HttpRangeReader>>;
type NoodlesCramReader = cram::io::IndexedReader<HttpRangeReader>;
const MAX_REMOTE_ALIGNMENT_INDEX_BYTES: usize = 256 * 1024 * 1024;
const MAX_REMOTE_CRAM_REFERENCE_BASES: usize = 512 * 1024 * 1024;
const MAX_REMOTE_CRAI_COMPRESSED_BYTES: usize = 16 * 1024 * 1024;
const MAX_REMOTE_CRAI_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;
const MAX_REMOTE_CRAI_RECORDS: usize = 1_000_000;

fn alignment_header_metadata_from_noodles(header: &sam::Header) -> AlignmentHeaderMetadata {
    use sam::header::record::value::map::{read_group::tag, tag::Other};

    let mut metadata = AlignmentHeaderMetadata::default();
    let maximum_homopolymer_tag =
        Other::<tag::Standard>::try_from(*b"mc").expect("mc is a valid SAM header tag");
    let ultima_orientation_tag =
        Other::<tag::Standard>::try_from(*b"tp").expect("tp is a valid SAM header tag");
    for (id, read_group) in header.read_groups() {
        let id: &[u8] = id.as_ref();
        if id.is_empty() || id.len() > crate::alignment::MAX_ALIGNMENT_READ_GROUP_ID_BYTES {
            continue;
        }
        if metadata.read_groups.len() == MAX_ALIGNMENT_READ_GROUPS {
            metadata.read_groups_omitted = metadata.read_groups_omitted.saturating_add(1);
            continue;
        }
        let platform = read_group
            .other_fields()
            .get(&tag::PLATFORM)
            .and_then(|value| std::str::from_utf8(value.as_ref()).ok())
            .map(ToOwned::to_owned);
        let maximum_homopolymer_length = read_group
            .other_fields()
            .get(&maximum_homopolymer_tag)
            .and_then(|value| std::str::from_utf8(value.as_ref()).ok())
            .and_then(|value| value.parse::<u16>().ok())
            .filter(|value| *value > 0);
        let ultima_tag_orientation = read_group
            .other_fields()
            .get(&ultima_orientation_tag)
            .and_then(|value| {
                let value: &[u8] = value.as_ref();
                match value {
                    b"reference" => Some(crate::UltimaTagOrientation::Reference),
                    b"synthesis" => Some(crate::UltimaTagOrientation::Synthesis),
                    _ => None,
                }
            });
        metadata.read_groups.push(AlignmentReadGroup {
            id: id.to_vec(),
            platform,
            maximum_homopolymer_length,
            ultima_tag_orientation,
        });
    }
    metadata
}

/// Production alignment backend selection. Local files and CRAM use `HTSlib`; remote BAM uses the
/// observable pure-Rust range path.
pub enum ProductionAlignmentSource {
    Htslib(LocalAlignmentSource),
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
        let remote_extension = (raw.starts_with("http://") || raw.starts_with("https://"))
            .then(|| url::Url::parse(&raw).ok())
            .flatten()
            .and_then(|url| {
                std::path::Path::new(url.path())
                    .extension()
                    .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            });
        if remote_extension.as_deref() == Some("bam") {
            RemoteBamAlignmentSource::open(raw.as_ref(), options)
                .map(Box::new)
                .map(Self::RemoteBam)
        } else if remote_extension.as_deref() == Some("cram") {
            RemoteCramAlignmentSource::open(raw.as_ref(), options)
                .map(Box::new)
                .map(Self::RemoteCram)
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cram"))
        {
            LocalCramAlignmentSource::open(path, options)
                .map(Box::new)
                .map(Self::LocalCram)
        } else {
            LocalAlignmentSource::open(path, options).map(Self::Htslib)
        }
    }

    pub fn remote_io_stats(&self) -> Option<RemoteIoStats> {
        match self {
            Self::Htslib(_) | Self::LocalCram(_) => None,
            Self::RemoteBam(source) => Some(source.remote_io_stats()),
            Self::RemoteCram(source) => Some(source.remote_io_stats()),
        }
    }

    pub fn set_remote_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        match self {
            Self::Htslib(_) | Self::LocalCram(_) => {}
            Self::RemoteBam(source) => source.set_transfer_budget(budget),
            Self::RemoteCram(source) => source.set_transfer_budget(budget),
        }
    }

    pub fn index_overview(&self) -> Option<&crate::AlignmentIndexOverview> {
        match self {
            Self::Htslib(source) => source.index_overview(),
            Self::LocalCram(_) => None,
            Self::RemoteBam(source) => source.index_overview.as_ref(),
            Self::RemoteCram(_) => None,
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

impl AlignmentSource for ProductionAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        match self {
            Self::Htslib(source) => source.reference_dictionary(),
            Self::LocalCram(source) => source.reference_dictionary(),
            Self::RemoteBam(source) => source.reference_dictionary(),
            Self::RemoteCram(source) => source.reference_dictionary(),
        }
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        match self {
            Self::Htslib(source) => source.source_stamp(),
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
    source_stamp: AlignmentSourceStamp,
    source_guard: LocalSourceGuard,
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
                    },
                )
                .map(|reference| Arc::new(Mutex::new(reference)))
            })
            .transpose()?;
        let reference_query_bases = Arc::new(AtomicUsize::new(0));
        let repository = reference
            .as_ref()
            .map_or_else(fasta::Repository::default, |reference| {
                fasta::Repository::new(CramReferenceAdapter {
                    reference: Arc::clone(reference),
                    query_bases: Arc::clone(&reference_query_bases),
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
        let dictionary = ReferenceDictionary::from_contigs(
            header
                .reference_sequences()
                .iter()
                .map(|(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                })
                .collect::<Vec<_>>(),
        );
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
            source_stamp,
            source_guard,
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
            let records = positioned_cram_records(&mut self.reader, &self.header, interval)?;
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
    header: noodles::sam::Header,
    dictionary: ReferenceDictionary,
    metadata: Arc<AlignmentHeaderMetadata>,
    index_stats: RemoteIoStats,
    normalization_mapped_records: Option<u64>,
    index_overview: Option<crate::AlignmentIndexOverview>,
    source_stamp: AlignmentSourceStamp,
}

impl RemoteBamAlignmentSource {
    pub fn open(url: &str, options: &AlignmentOpenOptions) -> Result<Self, AlignmentError> {
        let source = HttpRangeReader::open(url, options.remote_range.clone())?;
        let source_version = source.object_version();
        let (index, index_stats, index_digest) = open_index(
            url,
            options.remote_index_url.as_deref(),
            options.remote_range.clone(),
        )?;
        let normalization_mapped_records = remote_index_mapped_records(&index);
        let mut reader = match index {
            RemoteIndex::Bai(index) => bam::io::IndexedReader::new(source, index),
            RemoteIndex::Csi(index) => bam::io::IndexedReader::new(source, index),
        };
        let header = reader.read_header()?;
        let metadata = Arc::new(alignment_header_metadata_from_noodles(&header));
        let dictionary = ReferenceDictionary::from_contigs(
            header
                .reference_sequences()
                .iter()
                .map(|(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                })
                .collect::<Vec<_>>(),
        );
        let index_overview = Some(crate::AlignmentIndexOverview::from_binning_index(
            reader.index(),
            &dictionary,
        ));
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
            header,
            dictionary,
            metadata,
            index_stats,
            normalization_mapped_records,
            index_overview,
            source_stamp,
        })
    }

    pub fn remote_io_stats(&self) -> RemoteIoStats {
        combine_stats(self.index_stats, self.reader.get_ref().get_ref().stats())
    }

    fn set_cancellation(&mut self, cancellation: Option<RemoteCancellation>) {
        self.reader
            .get_mut()
            .get_mut()
            .set_cancellation(cancellation);
    }

    fn set_transfer_budget(&mut self, budget: Option<RemoteTransferBudget>) {
        self.reader.get_mut().get_mut().set_transfer_budget(budget);
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
    source_stamp: AlignmentSourceStamp,
}

impl RemoteCramAlignmentSource {
    pub fn open(url: &str, options: &AlignmentOpenOptions) -> Result<Self, AlignmentError> {
        let source = HttpRangeReader::open(url, options.remote_range.clone())?;
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
                    },
                )
                .map(|reference| Arc::new(Mutex::new(reference)))
            })
            .transpose()?;
        let reference_query_bases = Arc::new(AtomicUsize::new(0));
        let repository = reference
            .as_ref()
            .map_or_else(fasta::Repository::default, |reference| {
                fasta::Repository::new(CramReferenceAdapter {
                    reference: Arc::clone(reference),
                    query_bases: Arc::clone(&reference_query_bases),
                })
            });
        let mut reader = cram::io::indexed_reader::Builder::default()
            .set_index(index)
            .set_reference_sequence_repository(repository)
            .build_from_reader(source)?;
        let header = reader.read_header()?;
        let metadata = Arc::new(alignment_header_metadata_from_noodles(&header));
        let dictionary = ReferenceDictionary::from_contigs(
            header
                .reference_sequences()
                .iter()
                .map(|(name, reference)| {
                    (
                        String::from_utf8_lossy(name.as_ref()).into_owned(),
                        reference.length().get() as u64,
                    )
                })
                .collect::<Vec<_>>(),
        );
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
            source_stamp,
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

struct CramReferenceAdapter {
    reference: Arc<Mutex<FastaReference>>,
    query_bases: Arc<AtomicUsize>,
}

impl fasta::repository::Adapter for CramReferenceAdapter {
    fn get(&mut self, name: &[u8]) -> Option<io::Result<fasta::Record>> {
        Some(self.get_inner(name))
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
        self.query_bases
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |loaded| {
                loaded
                    .checked_add(length)
                    .filter(|total| *total <= MAX_REMOTE_CRAM_REFERENCE_BASES)
            })
            .map_err(|_| {
                io::Error::other(format!(
                    "CRAM query reference budget exceeded {MAX_REMOTE_CRAM_REFERENCE_BASES} bases"
                ))
            })?;
        let bases = if length == 0 {
            Vec::new()
        } else {
            let interval = GenomicInterval::new(contig_id, 0, contig.length())
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            match reference.fetch(interval) {
                Ok(bases) => bases,
                Err(error) => {
                    self.query_bases.fetch_sub(length, Ordering::Relaxed);
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
        let records = positioned_bam_records(&mut self.reader, &self.header, interval, &region)?;
        query_positioned_noodles_records(
            records,
            &self.header,
            interval,
            &options,
            self.normalization_mapped_records,
            Arc::clone(&self.metadata),
            keep_going,
        )
    }
}

fn positioned_bam_records(
    reader: &mut NoodlesReader,
    header: &sam::Header,
    interval: GenomicInterval,
    region: &Region,
) -> Result<Vec<io::Result<(bam::Record, Option<crate::AlignmentSourcePosition>)>>, AlignmentError>
{
    use noodles::bgzf::io::Seek as _;

    let reference_sequence_id =
        usize::try_from(interval.contig().get()).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let chunks = reader
        .index()
        .query(reference_sequence_id, region.interval())?;
    let bgzf_reader = reader.get_mut();
    let mut positioned = Vec::new();
    let mut seen = BTreeSet::new();
    for chunk in chunks {
        bgzf_reader.seek_to_virtual_position(chunk.start())?;
        let mut direct_reader = bam::io::Reader::from(&mut *bgzf_reader);
        let mut record = bam::Record::default();
        while direct_reader.get_ref().virtual_position() < chunk.end() {
            if direct_reader.read_record(&mut record)? == 0 {
                break;
            }
            let end_position = u64::from(direct_reader.get_ref().virtual_position());
            if !seen.insert(end_position) || !noodles_record_intersects(&record, header, interval)?
            {
                continue;
            }
            positioned.push(Ok((
                record.clone(),
                Some(crate::AlignmentSourcePosition::bam_virtual_offset(
                    end_position,
                )),
            )));
        }
    }
    Ok(positioned)
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
            let records = positioned_cram_records(&mut self.reader, &self.header, interval)?;
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

/// Reads exact CRAM slices selected by CRAI and retains a physical record address which is stable
/// across independent genomic query boundaries.
fn positioned_cram_records<R>(
    reader: &mut cram::io::IndexedReader<R>,
    header: &sam::Header,
    interval: GenomicInterval,
) -> Result<
    Vec<
        io::Result<(
            sam::alignment::RecordBuf,
            Option<crate::AlignmentSourcePosition>,
        )>,
    >,
    AlignmentError,
>
where
    R: io::Read + io::Seek,
{
    let reference_sequence_id =
        usize::try_from(interval.contig().get()).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let mut selected = BTreeMap::<u64, BTreeSet<u64>>::new();
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
                .insert(index_record.landmark());
        }
    }

    let repository = reader.reference_sequence_repository().clone();
    let raw_reader = reader.get_mut();
    let mut positioned = Vec::new();
    for (container_offset, selected_landmarks) in selected {
        raw_reader.seek(SeekFrom::Start(container_offset))?;
        let mut direct_reader = cram::io::reader::Builder::default()
            .set_reference_sequence_repository(repository.clone())
            .build_from_reader(&mut *raw_reader);
        let mut container = cram::io::reader::Container::default();
        if direct_reader.read_container(&mut container)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "CRAI points beyond the CRAM container stream",
            )
            .into());
        }
        let compression_header = container.compression_header()?;
        let landmarks = container.header().landmarks().to_vec();
        for (slice_index, result) in container.slices().enumerate() {
            let slice = result?;
            let landmark = u64::try_from(
                *landmarks
                    .get(slice_index)
                    .ok_or_else(|| io::Error::other("CRAM slice has no container landmark"))?,
            )
            .map_err(|_| AlignmentError::CoordinateOverflow)?;
            if !selected_landmarks.contains(&landmark) {
                continue;
            }
            let (core_data_src, external_data_srcs) = slice.decode_blocks()?;
            let records = slice.records(
                repository.clone(),
                header,
                &compression_header,
                &core_data_src,
                &external_data_srcs,
            )?;
            for (record_index, record) in records.iter().enumerate() {
                let record_index =
                    u32::try_from(record_index).map_err(|_| AlignmentError::CoordinateOverflow)?;
                let record = sam::alignment::RecordBuf::try_from_alignment_record(header, record)?;
                if !noodles_record_intersects(&record, header, interval)? {
                    continue;
                }
                positioned.push(Ok((
                    record,
                    Some(crate::AlignmentSourcePosition::cram_record(
                        container_offset,
                        landmark,
                        record_index,
                    )),
                )));
            }
        }
    }
    Ok(positioned)
}

fn noodles_record_intersects<R>(
    record: &R,
    header: &sam::Header,
    interval: GenomicInterval,
) -> Result<bool, AlignmentError>
where
    R: sam::alignment::Record,
{
    let Some(reference_sequence_id) = record.reference_sequence_id(header).transpose()? else {
        return Ok(false);
    };
    if u32::try_from(reference_sequence_id).ok().map(ContigId::new) != Some(interval.contig()) {
        return Ok(false);
    }
    let Some(start) = record.alignment_start().transpose()? else {
        return Ok(false);
    };
    let start = u64::try_from(start.get())
        .map_err(|_| AlignmentError::CoordinateOverflow)?
        .saturating_sub(1);
    let end = record_reference_end(record, start)?;
    Ok(start < interval.end() && interval.start() < end)
}

#[allow(clippy::too_many_lines)]
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
    R: sam::alignment::Record,
    I: IntoIterator<Item = io::Result<(R, Option<crate::AlignmentSourcePosition>)>>,
{
    let mut pileup = PileupBuilder::new(
        interval,
        options.coverage_bins.get(),
        options.minimum_base_quality,
    );
    let mut modifications = options.collect_modifications.then(|| {
        crate::modification_summary::ModificationSummaryBuilder::new(
            interval,
            options.coverage_bins.get(),
        )
    });
    let mut junctions = JunctionSummaryBuilder::new(
        interval,
        options.minimum_junction_flank,
        normalization_mapped_records,
    );
    let mut pairs = crate::pair::PairSummaryBuilder::new();
    let mut overlap_collapser = OverlapCollapser::new(options.overlapping_mate_policy);
    let maximum_records = options.max_records.get();
    let mut retained = Vec::new();
    let mut filtered_records = AlignmentBatch::default();
    let mut too_dense = false;
    let mut stats = QueryStats {
        complete_read_limit: maximum_records,
        ..QueryStats::default()
    };
    for result in records {
        let (record, source_position) = result?;
        stats.records_seen = stats.records_seen.saturating_add(1);
        if stats
            .records_seen
            .is_multiple_of(options.cancellation_chunk.get())
            && !keep_going()
        {
            stats.cancelled = true;
            break;
        }
        let record_span = record
            .alignment_start()
            .transpose()
            .ok()
            .flatten()
            .map(|position| position.get() as u64 - 1)
            .and_then(|start| {
                record_reference_end(&record, start)
                    .ok()
                    .filter(|end| *end > start)
                    .map(|end| (start, end))
            });
        let mut note_filtered = |reason| {
            if let Some((start, end)) = record_span {
                filtered_records.push_filtered_record(start, end, reason);
            } else {
                filtered_records.mark_filtered_record_spans_incomplete();
            }
        };
        match eligibility(&record, header, options)? {
            Eligibility::Eligible => {}
            Eligibility::Unmapped => {
                stats.records_filtered_unmapped = stats.records_filtered_unmapped.saturating_add(1);
                note_filtered(FilteredRecordReason::Unmapped);
                continue;
            }
            Eligibility::Secondary => {
                stats.records_filtered_secondary =
                    stats.records_filtered_secondary.saturating_add(1);
                note_filtered(FilteredRecordReason::Secondary);
                continue;
            }
            Eligibility::Supplementary => {
                stats.records_filtered_supplementary =
                    stats.records_filtered_supplementary.saturating_add(1);
                note_filtered(FilteredRecordReason::Supplementary);
                continue;
            }
            Eligibility::UserFilter {
                duplicate,
                qc_failed,
            } => {
                stats.records_filtered_user = stats.records_filtered_user.saturating_add(1);
                stats.records_filtered_duplicate = stats
                    .records_filtered_duplicate
                    .saturating_add(usize::from(duplicate));
                stats.records_filtered_qc_failed = stats
                    .records_filtered_qc_failed
                    .saturating_add(usize::from(qc_failed));
                note_filtered(FilteredRecordReason::User {
                    duplicate,
                    qc_failed,
                });
                continue;
            }
        }
        stats.records_eligible = stats.records_eligible.saturating_add(1);
        if stats.records_eligible > maximum_records && !too_dense {
            retained.clear();
            retained.shrink_to_fit();
            too_dense = true;
        }
        let flags = AlignmentFlags::from_bits(record.flags()?.bits());
        let record_contig = record
            .reference_sequence_id(header)
            .transpose()?
            .map_or(interval.contig(), |value| ContigId::new(value as u32));
        let mate_contig = record
            .mate_reference_sequence_id(header)
            .transpose()?
            .map(|value| ContigId::new(value as u32));
        let mate_start = record
            .mate_alignment_start()
            .transpose()?
            .map(|position| position.get() as u64 - 1);
        pairs.add_fields(
            record.name().map_or(&[][..], AsRef::<[u8]>::as_ref),
            record_contig,
            flags,
            mate_contig,
            i64::from(record.template_length()?),
        );
        let record_start = record
            .alignment_start()
            .transpose()?
            .map_or(0, |position| position.get() as u64 - 1);
        let record_end = record_reference_end(&record, record_start)?;
        let overlap_mask = overlap_collapser.mask_for(
            record.name().map_or(&[][..], AsRef::<[u8]>::as_ref),
            record_contig,
            record_start,
            record_end,
            flags,
            mate_contig,
            mate_start,
        );
        overlap_collapser.note_collapsed(add_pileup(
            &record,
            &mut pileup,
            &mut junctions,
            options.collect_alleles,
            overlap_mask,
        )?);
        let normalized_for_modifications = if let Some(builder) = &mut modifications {
            let normalized = normalize_record(&record, header, QueryDetail::Full, &[])?;
            builder.add_record(&normalized);
            Some(normalized)
        } else {
            None
        };
        if !too_dense {
            let normalized = match normalized_for_modifications {
                // Keep the record used for modification aggregation so
                // inspectors and automation cannot contradict the complete
                // rendered/exported modification evidence.
                Some(normalized) => normalized,
                None => normalize_record(
                    &record,
                    header,
                    options.detail,
                    &options.retained_auxiliary_tags,
                )?,
            };
            retained.push((normalized, source_position));
        }
    }
    if stats.cancelled {
        return Ok(cancelled_query(interval, metadata));
    }

    retained.sort_by(|left, right| {
        (left.0.start, left.0.end, &left.0.read_name).cmp(&(
            right.0.start,
            right.0.end,
            &right.0.read_name,
        ))
    });
    let mut batch = AlignmentBatch::default();
    for (record, source_position) in &retained {
        if let Some(source_position) = source_position {
            batch
                .push_with_source_position(record, *source_position)
                .map_err(AlignmentError::BatchOverflow)?;
        } else {
            batch.push(record).map_err(AlignmentError::BatchOverflow)?;
        }
    }
    batch.absorb_filtered_records(filtered_records);
    stats.records_emitted = retained.len();
    stats.truncated = too_dense;
    stats.individual_reads = if too_dense {
        crate::IndividualReadStatus::TooDense {
            limit: maximum_records,
        }
    } else {
        crate::IndividualReadStatus::Complete
    };
    let mut pileup = pileup.finish(PileupProvenance {
        eligible_records: stats.records_eligible,
        rendered_records: stats.records_emitted,
        all_eligible_records: !stats.cancelled,
        allele_observations_available: options.collect_alleles,
        overlapping_mate_policy: overlap_collapser.policy(),
        overlap_collapse_complete: overlap_collapser.complete(),
        collapsed_mate_bases: overlap_collapser.collapsed_mate_bases(),
        ..PileupProvenance::default()
    });
    if let Some(modifications) = modifications {
        pileup.modifications = modifications.finish(stats.records_eligible, !stats.cancelled);
    }
    debug_assert!(batch.validate());
    Ok(AlignmentQuery {
        interval,
        header: metadata,
        batch,
        pileup,
        junctions: junctions.finish(stats.records_eligible, !stats.cancelled),
        pairs: pairs.finish(!stats.cancelled),
        reference: None,
        stats,
    })
}

enum RemoteIndex {
    Bai(bam::bai::Index),
    Csi(noodles::csi::Index),
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

fn parse_cram_index(
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Eligibility {
    Eligible,
    Unmapped,
    Secondary,
    Supplementary,
    UserFilter { duplicate: bool, qc_failed: bool },
}

fn eligibility(
    record: &dyn sam::alignment::Record,
    _header: &sam::Header,
    options: &QueryOptions,
) -> Result<Eligibility, AlignmentError> {
    let flags = record.flags()?.bits();
    if flags & 0x4 != 0 {
        return Ok(Eligibility::Unmapped);
    }
    if !options.include_secondary && flags & 0x100 != 0 {
        return Ok(Eligibility::Secondary);
    }
    if !options.include_supplementary && flags & 0x800 != 0 {
        return Ok(Eligibility::Supplementary);
    }
    let filtered = || Eligibility::UserFilter {
        duplicate: flags & 0x400 != 0,
        qc_failed: flags & 0x200 != 0,
    };
    let filter = &options.filter;
    if flags & filter.required_flags != filter.required_flags || flags & filter.excluded_flags != 0
    {
        return Ok(filtered());
    }
    let mapping_quality = record
        .mapping_quality()
        .transpose()?
        .map(|value| value.get());
    if let Some(minimum) = filter.minimum_mapping_quality
        && mapping_quality.is_none_or(|value| value < minimum)
    {
        return Ok(filtered());
    }
    let data = record.data();
    for predicate in &filter.auxiliary {
        let tag = sam::alignment::record::data::field::Tag::new(predicate.tag[0], predicate.tag[1]);
        let value = data.get(&tag).transpose()?.map(normalize_aux).transpose()?;
        if predicate.comparison == AuxComparison::Exists {
            if value.is_none() {
                return Ok(filtered());
            }
        } else if !value
            .as_ref()
            .is_some_and(|value| predicate.matches_value(value))
        {
            return Ok(filtered());
        }
    }
    Ok(Eligibility::Eligible)
}

fn add_pileup(
    record: &dyn sam::alignment::Record,
    pileup: &mut PileupBuilder,
    junctions: &mut JunctionSummaryBuilder,
    collect_alleles: bool,
    overlap_mask: Option<OverlapMask>,
) -> Result<u64, AlignmentError> {
    let Some(start) = record.alignment_start().transpose()? else {
        return Ok(0);
    };
    let mut reference_position =
        u64::try_from(start.get() - 1).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let mut read_position = 0_usize;
    let sequence = collect_alleles.then(|| record.sequence().iter().collect::<Vec<_>>());
    let qualities = collect_alleles
        .then(|| {
            record
                .quality_scores()
                .iter()
                .collect::<io::Result<Vec<_>>>()
        })
        .transpose()?;
    let flags = AlignmentFlags::from_bits(record.flags()?.bits());
    let reverse = flags.is_reverse();
    let bisulfite_reverse = bisulfite_strand_is_reverse(flags);
    let mapping_quality = record
        .mapping_quality()
        .transpose()?
        .map(|value| value.get());
    let mut collapsed_bases = 0_u64;
    let mut aligned_segments = Vec::new();
    let mut record_junctions = RecordJunctionCollector::default();
    for result in record.cigar().iter() {
        let operation = result?;
        let length = operation.len();
        match operation.kind() {
            Kind::Match | Kind::SequenceMatch | Kind::SequenceMismatch => {
                record_junctions.add_aligned(u32::try_from(length).unwrap_or(u32::MAX));
                let end = reference_position.saturating_add(length as u64);
                collapsed_bases = collapsed_bases.saturating_add(add_unmasked_range(
                    reference_position,
                    end,
                    overlap_mask,
                    |start, end| aligned_segments.push((start, end)),
                ));
                if let (Some(sequence), Some(qualities)) = (&sequence, &qualities) {
                    for offset in 0..length {
                        let position = reference_position.saturating_add(offset as u64);
                        if overlap_mask.is_some_and(|mask| mask.contains(position)) {
                            continue;
                        }
                        let sequence_index = read_position.saturating_add(offset);
                        let Some((base, quality)) =
                            aligned_observation(sequence, qualities, sequence_index)
                        else {
                            continue;
                        };
                        pileup.add_base_with_bisulfite_strand(
                            position,
                            base,
                            quality,
                            reverse,
                            bisulfite_reverse,
                        );
                    }
                }
                read_position = read_position.saturating_add(length);
                reference_position = end;
            }
            Kind::Deletion => {
                let end = reference_position.saturating_add(length as u64);
                collapsed_bases = collapsed_bases.saturating_add(add_unmasked_range(
                    reference_position,
                    end,
                    overlap_mask,
                    |start, end| pileup.add_deletion(start, end, reverse),
                ));
                reference_position = end;
            }
            Kind::Skip => {
                let end = reference_position.saturating_add(length as u64);
                record_junctions.add_skip(reference_position, end);
                reference_position = end;
            }
            Kind::Insertion => {
                if !overlap_mask.is_some_and(|mask| mask.contains(reference_position)) {
                    pileup.add_insertion(reference_position, reverse);
                }
                read_position = read_position.saturating_add(length);
            }
            Kind::SoftClip => {
                read_position = read_position.saturating_add(length);
            }
            Kind::HardClip | Kind::Pad => {}
        }
    }
    pileup.add_aligned_record(aligned_segments, reverse, mapping_quality);
    let (record_junctions, dropped_record_junctions) = record_junctions.finish();
    junctions.add_record_junctions(
        &record_junctions,
        dropped_record_junctions,
        reverse,
        noodles_junction_strand(record)?,
    );
    Ok(collapsed_bases)
}

fn noodles_junction_strand(
    record: &dyn sam::alignment::Record,
) -> Result<JunctionStrandInput, AlignmentError> {
    for result in record.data().iter() {
        let (tag, value) = result?;
        if tag.as_ref() != b"XS" {
            continue;
        }
        return Ok(match value {
            Value::Character(b'+') => JunctionStrandInput::Tagged(JunctionStrand::Positive),
            Value::Character(b'-') => JunctionStrandInput::Tagged(JunctionStrand::Negative),
            Value::String(value) if AsRef::<[u8]>::as_ref(value) == b"+" => {
                JunctionStrandInput::Tagged(JunctionStrand::Positive)
            }
            Value::String(value) if AsRef::<[u8]>::as_ref(value) == b"-" => {
                JunctionStrandInput::Tagged(JunctionStrand::Negative)
            }
            _ => JunctionStrandInput::Invalid,
        });
    }
    Ok(JunctionStrandInput::Missing)
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

#[allow(clippy::too_many_lines)]
fn normalize_record(
    record: &dyn sam::alignment::Record,
    header: &sam::Header,
    detail: QueryDetail,
    retained_auxiliary_tags: &[[u8; 2]],
) -> Result<AlignmentRecord, AlignmentError> {
    let contig = record
        .reference_sequence_id(header)
        .transpose()?
        .ok_or(AlignmentError::UnmappedRecord)?;
    let start = record
        .alignment_start()
        .transpose()?
        .ok_or(AlignmentError::UnmappedRecord)?
        .get()
        - 1;
    let cigar = record
        .cigar()
        .iter()
        .map(|result| {
            let operation = result?;
            Ok(CigarOp {
                kind: match operation.kind() {
                    Kind::Match => CigarOpKind::Match,
                    Kind::Insertion => CigarOpKind::Insertion,
                    Kind::Deletion => CigarOpKind::Deletion,
                    Kind::Skip => CigarOpKind::ReferenceSkip,
                    Kind::SoftClip => CigarOpKind::SoftClip,
                    Kind::HardClip => CigarOpKind::HardClip,
                    Kind::Pad => CigarOpKind::Padding,
                    Kind::SequenceMatch => CigarOpKind::SequenceMatch,
                    Kind::SequenceMismatch => CigarOpKind::SequenceMismatch,
                },
                len: u32::try_from(operation.len())
                    .map_err(|_| AlignmentError::CoordinateOverflow)?,
            })
        })
        .collect::<Result<Vec<_>, AlignmentError>>()?;
    let reference_length = cigar
        .iter()
        .filter(|operation| {
            matches!(
                operation.kind,
                CigarOpKind::Match
                    | CigarOpKind::Deletion
                    | CigarOpKind::ReferenceSkip
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
            )
        })
        .fold(0_u64, |length, operation| {
            length.saturating_add(u64::from(operation.len))
        });
    let sequence = if detail == QueryDetail::Layout {
        SequenceData::NotLoaded
    } else if record.sequence().is_empty() {
        SequenceData::Missing
    } else {
        let qualities = record
            .quality_scores()
            .iter()
            .collect::<io::Result<Vec<_>>>()?;
        SequenceData::Present {
            bases: record.sequence().iter().collect(),
            qualities: (!qualities.iter().all(|quality| *quality == u8::MAX)).then_some(qualities),
        }
    };
    let auxiliary_fields = if detail == QueryDetail::Full || !retained_auxiliary_tags.is_empty() {
        record
            .data()
            .iter()
            .map(|result| {
                let (tag, value) = result?;
                Ok((*tag.as_ref(), value))
            })
            .filter_map(|result| match result {
                Ok((tag, value))
                    if detail == QueryDetail::Full || retained_auxiliary_tags.contains(&tag) =>
                {
                    Some(normalize_aux(value).map(|value| AuxField { tag, value }))
                }
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>, AlignmentError>>()?
    } else {
        Vec::new()
    };
    let mate_contig = record
        .mate_reference_sequence_id(header)
        .transpose()?
        .map(|value| ContigId::new(value as u32));
    let mate_start = record
        .mate_alignment_start()
        .transpose()?
        .map(|value| value.get() as u64 - 1);
    Ok(AlignmentRecord {
        contig: ContigId::new(
            u32::try_from(contig).map_err(|_| AlignmentError::CoordinateOverflow)?,
        ),
        start: start as u64,
        end: (start as u64)
            .saturating_add(reference_length)
            .max((start as u64).saturating_add(1)),
        flags: AlignmentFlags::from_bits(record.flags()?.bits()),
        mapping_quality: record
            .mapping_quality()
            .transpose()?
            .map(|value| value.get()),
        mate_contig,
        mate_start,
        template_length: i64::from(record.template_length()?),
        read_name: record
            .name()
            .map_or_else(Vec::new, |name| name.iter().copied().collect()),
        cigar,
        sequence,
        auxiliary_fields,
    })
}

fn normalize_aux(value: Value<'_>) -> Result<AuxValue, AlignmentError> {
    Ok(match value {
        Value::Character(value) => AuxValue::Character(value),
        Value::Int8(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt8(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Int16(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt16(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Int32(value) => AuxValue::Signed(i64::from(value)),
        Value::UInt32(value) => AuxValue::Unsigned(u64::from(value)),
        Value::Float(value) => AuxValue::Float(value),
        Value::String(value) => AuxValue::String(String::from_utf8_lossy(value).into_owned()),
        Value::Hex(value) => AuxValue::HexByteArray(String::from_utf8_lossy(value).into_owned()),
        Value::Array(array) => normalize_array(array)?,
    })
}

fn normalize_array(array: Array<'_>) -> Result<AuxValue, AlignmentError> {
    Ok(match array {
        Array::Int8(values) => AuxValue::SignedArray(
            values
                .iter()
                .map(|value| value.map(i64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::UInt8(values) => AuxValue::UnsignedArray(
            values
                .iter()
                .map(|value| value.map(u64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::Int16(values) => AuxValue::SignedArray(
            values
                .iter()
                .map(|value| value.map(i64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::UInt16(values) => AuxValue::UnsignedArray(
            values
                .iter()
                .map(|value| value.map(u64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::Int32(values) => AuxValue::SignedArray(
            values
                .iter()
                .map(|value| value.map(i64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::UInt32(values) => AuxValue::UnsignedArray(
            values
                .iter()
                .map(|value| value.map(u64::from))
                .collect::<io::Result<_>>()?,
        ),
        Array::Float(values) => AuxValue::FloatArray(values.iter().collect::<io::Result<_>>()?),
    })
}

#[cfg(test)]
mod tests {
    use std::{
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

    use super::*;

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
                stop,
                worker: Some(worker),
            })
        }

        fn requested_urls(&self) -> Vec<String> {
            self.requested_urls.lock().unwrap().clone()
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
            eligibility(&record, &header, &options).unwrap(),
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
