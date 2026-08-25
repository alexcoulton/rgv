use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf},
    sync::Arc,
};

use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy, ReferenceDictionary};
use rust_htslib::{
    bam::{self, Read as _, record::Aux},
    tpool::ThreadPool,
};
use thiserror::Error;

use crate::{
    AlignmentBatch, AlignmentFilter, AlignmentFlags, AlignmentHeaderMetadata, AlignmentReadGroup,
    AlignmentRecord, AuxField, AuxValue, CigarOp, CigarOpKind, HttpRangeConfig, JunctionSummary,
    PairSummary, PileupProvenance, PileupSummary, ReferenceSlice, RemoteError, SequenceData,
    alignment::BatchOverflow,
    alignment::alignment_header_metadata_from_sam,
    index_overview::load_local_alignment_index_overview,
    junction::{
        JunctionStrand, JunctionStrandInput, JunctionSummaryBuilder, RecordJunctionCollector,
    },
    pileup::{
        OverlapCollapser, OverlapMask, PileupBuilder, add_unmasked_range, aligned_observation,
    },
    sampling::BoundedSampler,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryDetail {
    Layout,
    Full,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct QueryOptions {
    pub detail: QueryDetail,
    pub max_records: NonZeroUsize,
    pub cancellation_chunk: NonZeroUsize,
    pub coverage_bins: NonZeroUsize,
    pub collect_alleles: bool,
    pub collect_modifications: bool,
    pub overlapping_mate_policy: OverlappingMatePolicy,
    pub minimum_base_quality: u8,
    pub minimum_junction_flank: u32,
    pub include_secondary: bool,
    pub include_supplementary: bool,
    pub include_reference: bool,
    pub filter: AlignmentFilter,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            detail: QueryDetail::Layout,
            max_records: NonZeroUsize::new(50_000).unwrap(),
            cancellation_chunk: NonZeroUsize::new(256).unwrap(),
            coverage_bins: NonZeroUsize::new(1_024).unwrap(),
            collect_alleles: false,
            collect_modifications: false,
            overlapping_mate_policy: OverlappingMatePolicy::default(),
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            include_secondary: false,
            include_supplementary: true,
            include_reference: false,
            filter: AlignmentFilter::default(),
        }
    }
}

impl QueryOptions {
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut write = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        write(&[self.detail as u8]);
        write(&self.max_records.get().to_le_bytes());
        write(&self.coverage_bins.get().to_le_bytes());
        write(&[
            u8::from(self.collect_alleles),
            u8::from(self.collect_modifications),
            self.overlapping_mate_policy as u8,
            self.minimum_base_quality,
        ]);
        write(&self.minimum_junction_flank.to_le_bytes());
        write(&[
            u8::from(self.include_secondary),
            u8::from(self.include_supplementary),
            u8::from(self.include_reference),
        ]);
        write(&self.filter.fingerprint().to_le_bytes());
        hash
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentOpenOptions {
    pub index_path: Option<PathBuf>,
    pub reference_path: Option<PathBuf>,
    pub decompression_threads: NonZeroU32,
    pub remote_index_url: Option<String>,
    pub remote_reference_index_url: Option<String>,
    pub remote_range: HttpRangeConfig,
}

impl Default for AlignmentOpenOptions {
    fn default() -> Self {
        Self {
            index_path: None,
            reference_path: None,
            decompression_threads: NonZeroU32::new(4).unwrap(),
            remote_index_url: None,
            remote_reference_index_url: None,
            remote_range: HttpRangeConfig {
                minimum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
                maximum_coalesced_chunks: NonZeroUsize::new(8).unwrap(),
                ..HttpRangeConfig::default()
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryStats {
    pub records_seen: usize,
    /// Unmapped records ignored even when an index returns a placed record.
    pub records_filtered_unmapped: usize,
    /// Secondary records excluded by the dedicated visibility option.
    pub records_filtered_secondary: usize,
    /// Supplementary records excluded by the dedicated visibility option.
    pub records_filtered_supplementary: usize,
    /// Records rejected by the user-configured flag, quality, or tag filter.
    pub records_filtered_user: usize,
    /// User-filtered records carrying the duplicate flag. This is a subset of
    /// `records_filtered_user` and is therefore not additive with it.
    pub records_filtered_duplicate: usize,
    /// User-filtered records carrying the QC-failed flag. This is a subset of
    /// `records_filtered_user` and is therefore not additive with it.
    pub records_filtered_qc_failed: usize,
    /// Records that passed flags and user filters in the backing query interval.
    pub records_eligible: usize,
    /// Records loaded for individual layout/inspection, equal to `records_eligible`
    /// unless the explicit query safety bound was exceeded.
    pub records_emitted: usize,
    /// The explicit query safety bound omitted otherwise eligible records.
    pub truncated: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentQuery {
    pub interval: GenomicInterval,
    pub header: Arc<AlignmentHeaderMetadata>,
    pub batch: AlignmentBatch,
    pub pileup: PileupSummary,
    pub junctions: JunctionSummary,
    pub pairs: PairSummary,
    pub reference: Option<ReferenceSlice>,
    pub stats: QueryStats,
}

impl AlignmentQuery {
    pub fn estimated_bytes(&self) -> usize {
        self.batch
            .estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    pub fn materialize_record(
        &self,
        index: usize,
    ) -> Result<AlignmentRecord, crate::PackedRecordError> {
        self.batch.materialize_record(index)
    }

    pub fn read_group(&self, index: usize) -> Option<&AlignmentReadGroup> {
        let field = self.batch.auxiliary_field(index, *b"RG").ok()??;
        let AuxValue::String(id) = field.value else {
            return None;
        };
        self.header.read_group(id.as_bytes())
    }
}

pub trait AlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary;

    fn index_overview(&self) -> Option<&crate::AlignmentIndexOverview> {
        None
    }

    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError>;
}

pub struct LocalAlignmentSource {
    reader: bam::IndexedReader,
    dictionary: ReferenceDictionary,
    header: Arc<AlignmentHeaderMetadata>,
    normalization_mapped_records: Option<u64>,
    index_overview: Option<crate::AlignmentIndexOverview>,
}

impl LocalAlignmentSource {
    pub fn open(
        path: impl AsRef<Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let path = path.as_ref();
        let raw_path = path.to_string_lossy();
        let remote_url = is_remote_url(&raw_path)
            .then(|| url::Url::parse(&raw_path))
            .transpose()?;
        let mut reader = if let Some(url) = remote_url {
            if options.index_path.is_some() {
                return Err(AlignmentError::RemoteIndexOverrideUnsupported);
            }
            bam::IndexedReader::from_url(&url)?
        } else if let Some(index_path) = &options.index_path {
            bam::IndexedReader::from_path_and_index(path, index_path)?
        } else {
            bam::IndexedReader::from_path(path)?
        };
        if reader.header().inner_ptr().is_null() {
            return Err(AlignmentError::InvalidAlignmentHeader);
        }
        if let Some(reference_path) = &options.reference_path {
            reader.set_reference(reference_path)?;
        }
        let pool = ThreadPool::new(options.decompression_threads.get())?;
        reader.set_thread_pool(&pool)?;

        // `index_stats` is constant-time for indexed BAM, but scans CRAM/SAM. Only use the exact
        // BAM index metadata here so opening a CRAM never acquires a hidden whole-file cost.
        let normalization_mapped_records = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bam"))
            .then(|| reader.index_stats().ok())
            .flatten()
            .map(|stats| {
                stats
                    .into_iter()
                    .filter(|(target_id, _, _, _)| *target_id >= 0)
                    .fold(0_u64, |total, (_, _, mapped, _)| {
                        total.saturating_add(mapped)
                    })
            })
            .filter(|mapped| *mapped > 0);

        let header = reader.header();
        let metadata = Arc::new(alignment_header_metadata_from_sam(header.as_bytes()));
        let dictionary =
            ReferenceDictionary::from_contigs(header.target_names().into_iter().enumerate().map(
                |(index, name)| {
                    let length = header.target_len(index as u32).unwrap_or_default();
                    (String::from_utf8_lossy(name).into_owned(), length)
                },
            ));
        let index_overview =
            load_local_alignment_index_overview(path, options.index_path.as_deref(), &dictionary);
        Ok(Self {
            reader,
            dictionary,
            header: metadata,
            normalization_mapped_records,
            index_overview,
        })
    }
}

fn is_remote_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

impl AlignmentSource for LocalAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    fn index_overview(&self) -> Option<&crate::AlignmentIndexOverview> {
        self.index_overview.as_ref()
    }

    #[allow(clippy::too_many_lines)]
    fn query(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<AlignmentQuery, AlignmentError> {
        if !keep_going() {
            return Ok(cancelled_query(interval, Arc::clone(&self.header)));
        }
        let tid = i32::try_from(interval.contig().get())
            .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let start =
            i64::try_from(interval.start()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        let end = i64::try_from(interval.end()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        self.reader.fetch((tid, start, end))?;

        let mut stats = QueryStats::default();
        let mut samples = BoundedSampler::new(options.max_records.get());
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
            self.normalization_mapped_records,
        );
        let mut pairs = crate::pair::PairSummaryBuilder::new();
        let mut overlap_collapser = OverlapCollapser::new(options.overlapping_mate_policy);
        let mut record = bam::Record::new();
        while let Some(result) = self.reader.read(&mut record) {
            result?;
            stats.records_seen += 1;
            if stats
                .records_seen
                .is_multiple_of(options.cancellation_chunk.get())
                && !keep_going()
            {
                stats.cancelled = true;
                break;
            }
            let flags = AlignmentFlags::from_bits(record.flags());
            if flags.is_unmapped() {
                stats.records_filtered_unmapped += 1;
                continue;
            }
            if !options.include_secondary && flags.is_secondary() {
                stats.records_filtered_secondary += 1;
                continue;
            }
            if !options.include_supplementary && flags.is_supplementary() {
                stats.records_filtered_supplementary += 1;
                continue;
            }
            if !options.filter.matches(&record)? {
                stats.records_filtered_user += 1;
                stats.records_filtered_duplicate += usize::from(flags.is_duplicate());
                stats.records_filtered_qc_failed += usize::from(flags.is_qc_failed());
                continue;
            }
            stats.records_eligible += 1;
            pairs.add_fields(
                record.qname(),
                ContigId::new(u32::try_from(record.tid()).unwrap_or_default()),
                flags,
                u32::try_from(record.mtid()).ok().map(ContigId::new),
                record.insert_size(),
            );
            let record_start = u64::try_from(record.pos()).unwrap_or_default();
            let record_end = u64::try_from(record.cigar().end_pos()).unwrap_or(record_start);
            let overlap_mask = overlap_collapser.mask_for(
                record.qname(),
                ContigId::new(u32::try_from(record.tid()).unwrap_or_default()),
                record_start,
                record_end,
                flags,
                u32::try_from(record.mtid()).ok().map(ContigId::new),
                u64::try_from(record.mpos()).ok(),
            );
            overlap_collapser.note_collapsed(add_record_pileup(
                &record,
                &mut pileup,
                &mut junctions,
                options.collect_alleles,
                overlap_mask,
            ));
            let normalized_for_modifications = if let Some(builder) = &mut modifications {
                let normalized = normalize_record(&record, QueryDetail::Full)?;
                builder.add_record(&normalized);
                Some(normalized)
            } else {
                None
            };

            let score = sampling_score(&record);
            if samples.would_retain(score, stats.records_seen) {
                samples.insert(
                    score,
                    stats.records_seen,
                    match normalized_for_modifications {
                        // Modification rendering, export, inspection, and MCP
                        // discovery must all observe the same retained record.
                        // The summary already paid the cost to normalize the
                        // full record, so do not discard its sequence/tags at
                        // Regional or Overview detail.
                        Some(normalized) => normalized,
                        None => normalize_record(&record, options.detail)?,
                    },
                );
            }
        }

        let mut retained = samples.into_values();
        retained.sort_by(|left, right| {
            (left.start, left.end, &left.read_name).cmp(&(right.start, right.end, &right.read_name))
        });
        let mut batch = AlignmentBatch::default();
        for record in &retained {
            batch.push(record).map_err(AlignmentError::BatchOverflow)?;
        }
        stats.records_emitted = retained.len();
        stats.truncated = stats.records_eligible > stats.records_emitted;
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
            header: Arc::clone(&self.header),
            batch,
            pileup,
            junctions: junctions.finish(stats.records_eligible, !stats.cancelled),
            pairs: pairs.finish(!stats.cancelled),
            reference: None,
            stats,
        })
    }
}

fn cancelled_query(
    interval: GenomicInterval,
    header: Arc<AlignmentHeaderMetadata>,
) -> AlignmentQuery {
    AlignmentQuery {
        interval,
        header,
        batch: AlignmentBatch::default(),
        pileup: PileupSummary::default(),
        junctions: JunctionSummary::default(),
        pairs: PairSummary::default(),
        reference: None,
        stats: QueryStats {
            cancelled: true,
            ..QueryStats::default()
        },
    }
}

fn sampling_score(record: &bam::Record) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in record
        .qname()
        .iter()
        .copied()
        .chain(record.tid().to_le_bytes())
        .chain(record.pos().to_le_bytes())
        .chain(record.flags().to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn add_record_pileup(
    record: &bam::Record,
    pileup: &mut PileupBuilder,
    junctions: &mut JunctionSummaryBuilder,
    collect_alleles: bool,
    overlap_mask: Option<OverlapMask>,
) -> u64 {
    use rust_htslib::bam::record::Cigar;

    let Ok(mut reference_position) = u64::try_from(record.pos()) else {
        return 0;
    };
    let mut read_position = 0_usize;
    let sequence = record.seq().as_bytes();
    let qualities = record.qual();
    let reverse = record.is_reverse();
    let mapping_quality = (record.mapq() != u8::MAX).then_some(record.mapq());
    let mut collapsed_bases = 0_u64;
    let mut aligned_segments = Vec::new();
    let mut record_junctions = RecordJunctionCollector::default();
    for operation in &record.cigar() {
        match operation {
            Cigar::Match(len) | Cigar::Equal(len) | Cigar::Diff(len) => {
                record_junctions.add_aligned(*len);
                let end = reference_position.saturating_add(u64::from(*len));
                collapsed_bases = collapsed_bases.saturating_add(add_unmasked_range(
                    reference_position,
                    end,
                    overlap_mask,
                    |start, end| aligned_segments.push((start, end)),
                ));
                if collect_alleles {
                    for offset in 0..*len as usize {
                        let position = reference_position.saturating_add(offset as u64);
                        if overlap_mask.is_some_and(|mask| mask.contains(position)) {
                            continue;
                        }
                        let sequence_index = read_position.saturating_add(offset);
                        let Some((base, quality)) =
                            aligned_observation(&sequence, qualities, sequence_index)
                        else {
                            continue;
                        };
                        pileup.add_base(position, base, quality, reverse);
                    }
                }
                read_position = read_position.saturating_add(*len as usize);
                reference_position = end;
            }
            Cigar::Del(len) => {
                let end = reference_position.saturating_add(u64::from(*len));
                collapsed_bases = collapsed_bases.saturating_add(add_unmasked_range(
                    reference_position,
                    end,
                    overlap_mask,
                    |start, end| pileup.add_deletion(start, end, reverse),
                ));
                reference_position = end;
            }
            Cigar::RefSkip(len) => {
                let end = reference_position.saturating_add(u64::from(*len));
                record_junctions.add_skip(reference_position, end);
                reference_position = end;
            }
            Cigar::Ins(len) => {
                if !overlap_mask.is_some_and(|mask| mask.contains(reference_position)) {
                    pileup.add_insertion(reference_position, reverse);
                }
                read_position = read_position.saturating_add(*len as usize);
            }
            Cigar::SoftClip(len) => {
                read_position = read_position.saturating_add(*len as usize);
            }
            Cigar::HardClip(_) | Cigar::Pad(_) => {}
        }
    }
    pileup.add_aligned_record(aligned_segments, reverse, mapping_quality);
    let (record_junctions, dropped_record_junctions) = record_junctions.finish();
    junctions.add_record_junctions(
        &record_junctions,
        dropped_record_junctions,
        reverse,
        htslib_junction_strand(record),
    );
    collapsed_bases
}

fn htslib_junction_strand(record: &bam::Record) -> JunctionStrandInput {
    match record.aux(b"XS") {
        Ok(Aux::Char(b'+') | Aux::String("+")) => {
            JunctionStrandInput::Tagged(JunctionStrand::Positive)
        }
        Ok(Aux::Char(b'-') | Aux::String("-")) => {
            JunctionStrandInput::Tagged(JunctionStrand::Negative)
        }
        Ok(_) => JunctionStrandInput::Invalid,
        Err(_) => JunctionStrandInput::Missing,
    }
}

fn normalize_record(
    record: &bam::Record,
    detail: QueryDetail,
) -> Result<AlignmentRecord, AlignmentError> {
    use rust_htslib::bam::record::Cigar;

    let tid = u32::try_from(record.tid()).map_err(|_| AlignmentError::UnmappedRecord)?;
    let start = u64::try_from(record.pos()).map_err(|_| AlignmentError::UnmappedRecord)?;
    let cigar_view = record.cigar();
    let end =
        u64::try_from(cigar_view.end_pos()).map_err(|_| AlignmentError::CoordinateOverflow)?;
    let cigar = cigar_view
        .iter()
        .map(|operation| CigarOp {
            kind: match operation {
                Cigar::Match(_) => CigarOpKind::Match,
                Cigar::Ins(_) => CigarOpKind::Insertion,
                Cigar::Del(_) => CigarOpKind::Deletion,
                Cigar::RefSkip(_) => CigarOpKind::ReferenceSkip,
                Cigar::SoftClip(_) => CigarOpKind::SoftClip,
                Cigar::HardClip(_) => CigarOpKind::HardClip,
                Cigar::Pad(_) => CigarOpKind::Padding,
                Cigar::Equal(_) => CigarOpKind::SequenceMatch,
                Cigar::Diff(_) => CigarOpKind::SequenceMismatch,
            },
            len: operation.len(),
        })
        .collect();
    let sequence = if detail == QueryDetail::Layout {
        SequenceData::NotLoaded
    } else if record.seq_len() == 0 {
        SequenceData::Missing
    } else {
        let qualities = (!record.qual().iter().all(|quality| *quality == u8::MAX))
            .then(|| record.qual().to_vec());
        SequenceData::Present {
            bases: record.seq().as_bytes(),
            qualities,
        }
    };
    let auxiliary_fields = if detail == QueryDetail::Full {
        record
            .aux_iter()
            .map(|result| {
                let (tag, value) = result?;
                let tag: [u8; 2] = tag.try_into().map_err(|_| AlignmentError::InvalidAuxTag)?;
                Ok(AuxField {
                    tag,
                    value: normalize_aux(value),
                })
            })
            .collect::<Result<Vec<_>, AlignmentError>>()?
    } else {
        Vec::new()
    };

    Ok(AlignmentRecord {
        contig: ContigId::new(tid),
        start,
        end: end.max(start.saturating_add(1)),
        flags: AlignmentFlags::from_bits(record.flags()),
        mapping_quality: (record.mapq() != u8::MAX).then_some(record.mapq()),
        mate_contig: u32::try_from(record.mtid()).ok().map(ContigId::new),
        mate_start: u64::try_from(record.mpos()).ok(),
        template_length: record.insert_size(),
        read_name: record.qname().to_vec(),
        cigar,
        sequence,
        auxiliary_fields,
    })
}

fn normalize_aux(value: Aux<'_>) -> AuxValue {
    match value {
        Aux::Char(value) => AuxValue::Character(value),
        Aux::I8(value) => AuxValue::Signed(i64::from(value)),
        Aux::U8(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::I16(value) => AuxValue::Signed(i64::from(value)),
        Aux::U16(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::I32(value) => AuxValue::Signed(i64::from(value)),
        Aux::U32(value) => AuxValue::Unsigned(u64::from(value)),
        Aux::Float(value) => AuxValue::Float(value),
        Aux::Double(value) => AuxValue::Double(value),
        Aux::String(value) => AuxValue::String(value.to_owned()),
        Aux::HexByteArray(value) => AuxValue::HexByteArray(value.to_owned()),
        Aux::ArrayI8(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU8(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayI16(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU16(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayI32(values) => AuxValue::SignedArray(values.iter().map(i64::from).collect()),
        Aux::ArrayU32(values) => AuxValue::UnsignedArray(values.iter().map(u64::from).collect()),
        Aux::ArrayFloat(values) => AuxValue::FloatArray(values.iter().collect()),
    }
}

#[derive(Debug, Error)]
pub enum AlignmentError {
    #[error(transparent)]
    Htslib(#[from] rust_htslib::errors::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Remote(#[from] RemoteError),
    #[error("alignment coordinate cannot be represented by the backend")]
    CoordinateOverflow,
    #[error("an indexed regional query unexpectedly returned an unmapped record")]
    UnmappedRecord,
    #[error("alignment header is missing or invalid")]
    InvalidAlignmentHeader,
    #[error("alignment batch exceeded its packed 32-bit arena offsets")]
    BatchOverflow(BatchOverflow),
    #[error("auxiliary tag was not exactly two bytes")]
    InvalidAuxTag,
    #[error("reference dictionary has no contig id {0}")]
    UnknownContig(u32),
    #[error(transparent)]
    Url(#[from] url::ParseError),
    #[error("custom remote index URLs are not yet supported; use the conventional .bai/.crai URL")]
    RemoteIndexOverrideUnsupported,
    #[error("invalid FASTA index: {0}")]
    InvalidFastaIndex(String),
    #[error("invalid CRAM index: {0}")]
    InvalidCramIndex(String),
    #[error("reference interval {contig}:{start}-{end} exceeds contig length {length}")]
    ReferenceIntervalOutOfBounds {
        contig: String,
        start: u64,
        end: u64,
        length: u64,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn write_placed_unmapped_fixture(path: &Path) {
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
                .push_tag(b"LN", 1_000),
        );
        let mut writer = Writer::from_path(path, &header, Format::Bam).unwrap();
        for (name, position, flags) in [
            (b"mapped".as_slice(), 100, 0),
            (b"placed-unmapped", 110, 0x4),
        ] {
            let mut record = Record::new();
            record.set(
                name,
                Some(&CigarString(vec![Cigar::Equal(20)])),
                b"ACGTACGTACGTACGTACGT",
                &[35; 20],
            );
            record.set_tid(0);
            record.set_pos(position);
            record.set_mapq(60);
            record.set_flags(flags);
            writer.write(&record).unwrap();
        }
        drop(writer);
        let index = PathBuf::from(format!("{}.bai", path.display()));
        bam::index::build(path, Some(&index), bam::index::Type::Bai, 1).unwrap();
    }

    #[test]
    fn modification_collection_participates_in_query_cache_identity() {
        let baseline = QueryOptions::default();
        let with_modifications = QueryOptions {
            collect_modifications: true,
            ..QueryOptions::default()
        };
        assert_ne!(baseline.fingerprint(), with_modifications.fingerprint());
        let count_both = QueryOptions {
            overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
            ..QueryOptions::default()
        };
        assert_ne!(baseline.fingerprint(), count_both.fingerprint());
        let filtered_junctions = QueryOptions {
            minimum_junction_flank: 12,
            ..QueryOptions::default()
        };
        assert_ne!(baseline.fingerprint(), filtered_junctions.fingerprint());
    }

    #[test]
    fn query_can_be_cancelled_before_io() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let query = source
            .query(
                GenomicInterval::new(ContigId::new(0), 900_000, 920_000).unwrap(),
                QueryOptions::default(),
                &|| false,
            )
            .unwrap();
        assert!(query.stats.cancelled);
        assert!(query.batch.is_empty());
    }

    #[test]
    fn placed_unmapped_records_do_not_contribute_genomic_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("placed-unmapped.bam");
        write_placed_unmapped_fixture(&path);
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let query = source
            .query(
                GenomicInterval::new(ContigId::new(0), 90, 150).unwrap(),
                QueryOptions {
                    coverage_bins: NonZeroUsize::new(60).unwrap(),
                    collect_alleles: true,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();

        assert_eq!(query.stats.records_seen, 2);
        assert_eq!(query.stats.records_filtered_unmapped, 1);
        assert_eq!(query.stats.records_eligible, 1);
        assert_eq!(query.stats.records_emitted, 1);
        assert_eq!(query.batch.len(), 1);
        assert_eq!(
            query.batch.materialize_record(0).unwrap().read_name,
            b"mapped"
        );
        assert_eq!(query.pileup.maximum_depth(), 1);
        assert!(query.junctions.junctions.is_empty());
    }

    #[test]
    fn benchmark_corpus_preserves_complex_records() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
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
        assert!(query.batch.validate());
        assert!(query.stats.records_emitted >= 1_500);
        assert!((0..query.batch.len()).any(|index| query.batch.has_auxiliary_tag(index, *b"MM")));
        assert!((0..query.batch.len()).any(|index| query.batch.has_auxiliary_tag(index, *b"SA")));
        assert!((0..query.batch.len()).any(|index| {
            query
                .materialize_record(index)
                .is_ok_and(|record| matches!(record.sequence, SequenceData::Present { .. }))
        }));
        assert!((0..query.batch.len()).any(|index| {
            query.materialize_record(index).is_ok_and(|record| {
                crate::decode_modified_bases(&record).is_ok_and(|calls| !calls.is_empty())
            })
        }));
        assert!(!query.pileup.modifications.is_empty());
        assert_eq!(
            query
                .pileup
                .modifications
                .provenance
                .records_with_modification_tags,
            1_499
        );
        assert_eq!(query.pileup.modifications.provenance.malformed_records, 3);
        assert_eq!(query.pileup.modifications.provenance.decoded_calls, 2_992);
    }

    #[test]
    fn pair_summary_uses_all_eligible_templates_before_display_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 0, 200_000).unwrap();
        let query = |maximum_records| {
            let mut source =
                LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
            source
                .query(
                    interval,
                    QueryOptions {
                        max_records: NonZeroUsize::new(maximum_records).unwrap(),
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };
        let sampled = query(1);
        let complete = query(50_000);
        assert!(sampled.stats.truncated);
        assert_eq!(sampled.pairs, complete.pairs);
        assert!(sampled.pairs.eligible_proper_templates >= 100);
        assert_eq!(sampled.pairs.median_template_length, Some(166));
        assert_eq!(
            sampled.pairs.inferred_bounds,
            Some(crate::TemplateLengthBounds {
                minimum: Some(166),
                maximum: Some(166),
            })
        );
    }

    #[test]
    fn structural_pair_fixture_exposes_every_declared_evidence_pattern() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 2_590_000, 2_650_000).unwrap();
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let query = source
            .query(
                interval,
                QueryOptions {
                    detail: QueryDetail::Full,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        let bounds = crate::TemplateLengthBounds {
            minimum: Some(100),
            maximum: Some(500),
        };
        let evidence = (0..query.batch.len())
            .filter_map(|index| query.materialize_record(index).ok())
            .filter(|record| record.read_name.starts_with(b"PairSv-"))
            .map(|record| crate::pair_evidence(&record, interval, bounds))
            .collect::<Vec<_>>();
        assert_eq!(evidence.len(), 10);
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| pair.classification
                    == crate::PairClassification::DiscordantTemplateLengthLong)
                .count(),
            2
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| pair.classification
                    == crate::PairClassification::DiscordantTemplateLengthShort)
                .count(),
            2
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| {
                    pair.orientation == crate::PairOrientation::SameStrand
                        && pair.classification == crate::PairClassification::DiscordantOrientation
                })
                .count(),
            2
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| pair.orientation == crate::PairOrientation::Outward)
                .count(),
            2
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| pair.classification == crate::PairClassification::InterContig)
                .count(),
            1
        );
        assert_eq!(
            evidence
                .iter()
                .filter(|pair| pair.classification == crate::PairClassification::MateUnmapped)
                .count(),
            1
        );
    }

    #[test]
    fn overlapping_mate_policy_collapses_the_right_end_before_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 1_000, 1_200).unwrap();
        let query = |policy| {
            let mut source =
                LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
            source
                .query(
                    interval,
                    QueryOptions {
                        coverage_bins: NonZeroUsize::new(200).unwrap(),
                        collect_alleles: true,
                        overlapping_mate_policy: policy,
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };
        let collapsed = query(OverlappingMatePolicy::CountOnce);
        let both = query(OverlappingMatePolicy::CountBoth);
        assert!(collapsed.pileup.provenance.overlap_collapse_complete);
        assert!(collapsed.pileup.provenance.collapsed_mate_bases > 0);
        assert_eq!(both.pileup.provenance.collapsed_mate_bases, 0);
        assert!(collapsed.pileup.maximum_depth() < both.pileup.maximum_depth());
        assert!(
            collapsed
                .pileup
                .bins
                .iter()
                .zip(&both.pileup.bins)
                .all(|(collapsed, both)| collapsed.depth <= both.depth)
        );
    }

    #[test]
    fn modification_summary_is_independent_of_visible_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap();
        let query = |maximum_records| {
            let mut source =
                LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
            source
                .query(
                    interval,
                    QueryOptions {
                        detail: QueryDetail::Full,
                        max_records: NonZeroUsize::new(maximum_records).unwrap(),
                        collect_modifications: true,
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };
        let sampled = query(1);
        let complete = query(50_000);
        assert!(sampled.stats.truncated);
        assert_eq!(sampled.pileup.modifications, complete.pileup.modifications);
        assert_eq!(
            sampled.pileup.modifications.provenance.eligible_records,
            sampled.stats.records_eligible
        );
    }

    #[test]
    fn junction_summary_is_complete_and_independent_of_visible_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 2_100_000, 2_301_100).unwrap();
        let query = |maximum_records| {
            let mut source =
                LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
            source
                .query(
                    interval,
                    QueryOptions {
                        max_records: NonZeroUsize::new(maximum_records).unwrap(),
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };
        let sampled = query(1);
        let complete = query(50_000);
        assert!(sampled.stats.truncated);
        assert_eq!(sampled.junctions, complete.junctions);
        assert_eq!(sampled.junctions.junctions.len(), 10_000);
        assert_eq!(sampled.junctions.provenance.spliced_records, 10_000);
        assert_eq!(
            sampled.junctions.provenance.supporting_spliced_records,
            10_000
        );
        assert_eq!(
            sampled.junctions.provenance.junction_observations_retained,
            10_000
        );
        assert_eq!(
            sampled.junctions.provenance.normalization_mapped_records,
            Some(282_017)
        );
        assert_eq!(sampled.junctions.provenance.xs_tagged_observations, 5_000);
        assert_eq!(
            sampled
                .junctions
                .provenance
                .alignment_strand_fallback_observations,
            2_500
        );
        assert_eq!(sampled.junctions.provenance.invalid_xs_observations, 2_500);
        assert_eq!(
            sampled
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.positive_depth)
                .sum::<u32>(),
            5_000
        );
        assert_eq!(
            sampled
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.negative_depth)
                .sum::<u32>(),
            2_500
        );
        assert_eq!(
            sampled
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.unknown_depth)
                .sum::<u32>(),
            2_500
        );
        assert!(sampled.junctions.provenance.all_eligible_records);

        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let filtered = source
            .query(
                interval,
                QueryOptions {
                    minimum_junction_flank: 76,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        assert!(filtered.junctions.junctions.is_empty());
        assert_eq!(filtered.junctions.provenance.spliced_records, 10_000);
        assert_eq!(filtered.junctions.provenance.supporting_spliced_records, 0);
        assert_eq!(
            filtered
                .junctions
                .provenance
                .junction_observations_filtered_by_flank,
            10_000
        );
    }

    #[test]
    fn variant_truth_pileup_is_exact_and_independent_of_visible_sampling() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let query = |maximum_records| {
            let mut source =
                LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
            source
                .query(
                    interval,
                    QueryOptions {
                        detail: QueryDetail::Full,
                        max_records: NonZeroUsize::new(maximum_records).unwrap(),
                        coverage_bins: NonZeroUsize::new(2).unwrap(),
                        collect_alleles: true,
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };

        let sampled = query(1);
        let complete = query(1_000);
        assert_eq!(sampled.stats.records_eligible, 13);
        assert_eq!(sampled.stats.records_emitted, 1);
        assert!(sampled.stats.truncated);
        assert_eq!(sampled.pileup.bins, complete.pileup.bins);
        assert_eq!(sampled.pileup.provenance.eligible_records, 13);
        assert_eq!(sampled.pileup.provenance.rendered_records, 1);
        assert!(sampled.pileup.provenance.all_eligible_records);

        let (_, insertion_anchor) = sampled.pileup.bin_at(interval, 9_999).unwrap();
        assert_eq!(insertion_anchor.alleles.insertion, 4);
        let (_, variant) = sampled.pileup.bin_at(interval, 10_000).unwrap();
        assert_eq!(variant.depth, 8);
        assert_eq!(variant.alleles.a, 7);
        assert_eq!(variant.alleles.c, 1);
        assert_eq!(variant.forward_alleles.a, 7);
        assert_eq!(variant.forward_alleles.c, 1);
        assert_eq!(variant.reverse_alleles.base_observations(), 0);
        assert_eq!(variant.alleles.deletion, 5);
        assert_eq!(variant.forward_depth, 8);
        assert_eq!(variant.reverse_depth, 0);
        assert_eq!(variant.mean_base_quality(), Some(40.0));
        assert_eq!(variant.mean_mapping_quality(), Some(60.0));
    }

    #[test]
    fn minimum_base_quality_filters_alleles_without_changing_depth() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let interval = GenomicInterval::new(ContigId::new(0), 10_000, 10_001).unwrap();
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let query = source
            .query(
                interval,
                QueryOptions {
                    coverage_bins: NonZeroUsize::new(1).unwrap(),
                    collect_alleles: true,
                    minimum_base_quality: 41,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        let bin = query.pileup.bins[0];
        assert_eq!(bin.depth, 8);
        assert_eq!(bin.alleles.base_observations(), 0);
        assert_eq!(bin.alleles.deletion, 5);
    }

    #[test]
    fn malformed_alignment_is_reported_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt.bam");
        fs::write(&path, b"not a BAM file").unwrap();
        assert!(LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).is_err());
    }

    #[test]
    fn indexed_open_rejects_a_bam_without_its_index() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !source.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unindexed.bam");
        fs::copy(source, &path).unwrap();
        assert!(LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).is_err());
    }

    #[test]
    fn indexed_open_accepts_an_explicit_csi_for_local_bam() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !source.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let index = directory.path().join("mixed.csi");
        bam::index::build(&source, Some(&index), bam::index::Type::Csi(14), 1).unwrap();
        let mut alignment = LocalAlignmentSource::open(
            &source,
            &AlignmentOpenOptions {
                index_path: Some(index),
                ..AlignmentOpenOptions::default()
            },
        )
        .unwrap();
        let query = alignment
            .query(
                GenomicInterval::new(ContigId::new(1), 0, 2_000_000).unwrap(),
                QueryOptions::default(),
                &|| true,
            )
            .unwrap();
        assert_eq!(query.stats.records_seen, 2_000);
        assert!(query.batch.validate());
    }

    #[test]
    fn indexed_open_rejects_a_corrupt_bam_index() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        if !source.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("corrupt-index.bam");
        let index = PathBuf::from(format!("{}.bai", path.display()));
        fs::copy(source, &path).unwrap();
        fs::write(index, b"not a BAI").unwrap();
        assert!(LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).is_err());
    }

    #[test]
    fn indexed_open_rejects_a_truncated_bam_even_with_an_index() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/mixed.bam");
        let source_index = PathBuf::from(format!("{}.bai", source.display()));
        if !source.exists() || !source_index.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("truncated.bam");
        let index = PathBuf::from(format!("{}.bai", path.display()));
        let bytes = fs::read(source).unwrap();
        fs::write(&path, &bytes[..bytes.len().min(64)]).unwrap();
        fs::copy(source_index, index).unwrap();
        assert!(LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).is_err());
    }
}
