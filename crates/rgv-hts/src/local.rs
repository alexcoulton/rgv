use std::{
    num::{NonZeroU32, NonZeroUsize},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

use rgv_core::{GenomicInterval, OverlappingMatePolicy, ReferenceDictionary};
use rust_htslib::{
    bam::{self, Read as _},
    tpool::ThreadPool,
};
use thiserror::Error;

use crate::{
    AlignmentBatch, AlignmentFilter, AlignmentHeaderMetadata, AlignmentOverlapIndex,
    AlignmentReadGroup, AlignmentRecord, AlignmentRecordView, AlignmentSourceFormat,
    AlignmentSourceStamp, AuxValue, HttpRangeConfig, JunctionSummary, PairSummary,
    PileupProvenance, PileupSummary, PositionedChunkReceipt, ReferenceSlice, RemoteError,
    SequenceData, SequenceView,
    alignment::BatchOverflow,
    alignment::alignment_header_metadata_from_sam,
    backend_record::{
        PileupAlignment, PositionedAdmission, PositionedBlockBuilder, RegionQueryAccumulator,
        add_alignment_pileup, junction_strand_input,
    },
    index_overview::{load_local_alignment_index_overview, local_alignment_index_candidates},
    junction::JunctionSummaryBuilder,
    pileup::{
        MateAlignment, MateOverlapMask, OverlapCollapser, PileupBuilder,
        base_observation_is_counted,
    },
    positioned_resume::{ChunkCompletion, ChunkExtent},
    source_revision::{LocalSourceGuard, local_source_stamp},
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum QueryDetail {
    Layout,
    /// Layout columns plus sequence/qualities for fixed-position base sorting,
    /// without retaining unrelated auxiliary payload.
    Sequence,
    Full,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq)]
pub struct QueryOptions {
    pub detail: QueryDetail,
    /// Auxiliary tags retained by layout-only queries because the active row
    /// policy uses them. Full-detail queries retain every auxiliary field.
    pub retained_auxiliary_tags: Vec<[u8; 2]>,
    /// The active row policy needs sequence (currently fixed-position base
    /// sorting), so a layout-horizon query cannot omit it.
    pub layout_requires_sequence: bool,
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
            retained_auxiliary_tags: Vec::new(),
            layout_requires_sequence: false,
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
        for tag in &self.retained_auxiliary_tags {
            write(tag);
        }
        write(&[0xff]);
        write(&[u8::from(self.layout_requires_sequence)]);
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

/// A bounded count of `HTSlib` decompression threads shared by every holder of a lease.
#[derive(Debug)]
struct DecompressionThreadAllowance {
    leased: AtomicU32,
}

/// The allowance all open local alignment sources of the process draw from.
static DECOMPRESSION_THREADS: DecompressionThreadAllowance = DecompressionThreadAllowance {
    leased: AtomicU32::new(0),
};

/// Most `HTSlib` decompression threads all open local alignment sources may hold together:
/// the machine's parallelism, kept between 2 and 16.
fn decompression_thread_limit() -> u32 {
    static LIMIT: OnceLock<u32> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::thread::available_parallelism()
            .map_or(4, |threads| {
                u32::try_from(threads.get()).unwrap_or(u32::MAX)
            })
            .clamp(2, 16)
    })
}

/// A share of a decompression thread allowance, returned when dropped.
///
/// ADR 0002 calls for one shared `HTSlib` pool. rust-htslib 1.0.1 cannot provide that
/// soundly: its `ThreadPool` is an `Rc<RefCell<_>>` (neither `Send` nor `Sync`, so it cannot
/// live in a `static`), and every reader keeps a clone of that `Rc` while being declared
/// `Send`, so one pool attached to readers owned by different track threads would race on a
/// non-atomic reference count. Each source therefore still owns a private pool, but all of
/// them draw its size from one bounded process-wide allowance: opening many sources never
/// raises the process's decompression thread count beyond [`decompression_thread_limit`]. A
/// source that finds the allowance exhausted decompresses on its own thread.
#[derive(Debug)]
struct DecompressionThreadLease {
    allowance: &'static DecompressionThreadAllowance,
    threads: u32,
}

impl DecompressionThreadAllowance {
    /// Leases up to `requested` threads, as many as `limit` still leaves.
    fn lease(&'static self, requested: u32, limit: u32) -> DecompressionThreadLease {
        let mut threads = 0;
        let _ = self
            .leased
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |leased| {
                threads = requested.min(limit.saturating_sub(leased));
                Some(leased.saturating_add(threads))
            });
        DecompressionThreadLease {
            allowance: self,
            threads,
        }
    }
}

impl Drop for DecompressionThreadLease {
    fn drop(&mut self) {
        self.allowance
            .leased
            .fetch_sub(self.threads, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug)]
pub struct AlignmentOpenOptions {
    pub index_path: Option<PathBuf>,
    pub reference_path: Option<PathBuf>,
    /// Most `HTSlib` decompression threads a local source asks for. The grant is capped by a
    /// process-wide allowance sized from the machine's parallelism, so the total over every
    /// open source stays bounded however many are open.
    pub decompression_threads: NonZeroU32,
    pub remote_index_url: Option<String>,
    pub remote_reference_index_url: Option<String>,
    pub remote_range: HttpRangeConfig,
    /// Trusted identity override for a known immutable copy of the same source.
    ///
    /// Normal opens derive a conservative path/URL-specific stamp. Resource bundles may persist
    /// and reuse a stamp when moving the exact BAM/CRAM and index between local and HTTP storage.
    pub source_stamp_override: Option<AlignmentSourceStamp>,
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
            source_stamp_override: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IndividualReadStatus {
    #[default]
    Complete,
    TooDense {
        limit: usize,
    },
}

impl IndividualReadStatus {
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
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
    /// Eligible records overlapping the exact foreground admission interval. This equals
    /// `records_eligible` for ordinary exact queries, but excludes offscreen records acquired
    /// only for a tiled or speculative browsing horizon.
    pub complete_read_records_eligible: usize,
    /// Records loaded for individual layout/inspection. This equals
    /// `records_eligible` for a complete result and is zero for Too dense.
    pub records_emitted: usize,
    /// Legacy compatibility flag. A true value now means the individual-read
    /// result is Too dense and the committed batch is empty; it never denotes
    /// a retained partial batch.
    pub truncated: bool,
    /// Actual foreground/horizon admission limit applied to this query after
    /// workspace allocation.
    pub complete_read_limit: usize,
    pub individual_reads: IndividualReadStatus,
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
    #[must_use]
    pub const fn individual_reads_complete(&self) -> bool {
        self.stats.individual_reads.is_complete()
    }

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

    /// Bytes newly allocated by this query beyond any Arc-backed packed storage it references.
    pub fn incremental_estimated_bytes(&self) -> usize {
        self.batch
            .incremental_estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    /// Per-query evidence and projection bytes when its packed arenas are charged elsewhere.
    pub fn shared_view_estimated_bytes(&self) -> usize {
        self.batch
            .shared_view_estimated_bytes()
            .saturating_add(self.pileup.estimated_bytes())
            .saturating_add(self.junctions.estimated_bytes())
            .saturating_add(self.pairs.estimated_bytes())
            .saturating_add(
                self.reference
                    .as_ref()
                    .map_or(0, |reference| reference.bases.len()),
            )
    }

    /// Expected packed/evidence bytes if this logical population were decoded
    /// into its own resident interval. Unlike `estimated_bytes`, this does not
    /// charge unrelated records retained by a zero-copy backing arena.
    pub fn logical_decoded_estimated_bytes(&self) -> usize {
        self.batch
            .logical_decoded_estimated_bytes()
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

/// Rebuilds an exact target query from a complete compatible resident query.
/// All interval-specific summaries are regenerated; the broad query's
/// aggregates are never relabelled as target evidence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentDerivationMetrics {
    pub lookup_micros: u64,
    pub projection_micros: u64,
    pub summary_micros: u64,
    pub summary_tile_hits: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentDerivationPhase {
    SelectingResidentReads,
    BuildingSummaries,
}

pub fn derive_alignment_query(
    resident: &AlignmentQuery,
    overlap_index: &AlignmentOverlapIndex,
    interval: GenomicInterval,
    options: &QueryOptions,
    reference: Option<ReferenceSlice>,
    keep_going: &dyn Fn() -> bool,
) -> Result<AlignmentQuery, AlignmentError> {
    derive_alignment_query_with_metrics(
        resident,
        overlap_index,
        interval,
        options,
        reference,
        keep_going,
    )
    .map(|(query, _)| query)
}

pub fn derive_alignment_query_with_metrics(
    resident: &AlignmentQuery,
    overlap_index: &AlignmentOverlapIndex,
    interval: GenomicInterval,
    options: &QueryOptions,
    reference: Option<ReferenceSlice>,
    keep_going: &dyn Fn() -> bool,
) -> Result<(AlignmentQuery, AlignmentDerivationMetrics), AlignmentError> {
    derive_alignment_query_with_metrics_and_phase(
        resident,
        overlap_index,
        interval,
        options,
        reference,
        keep_going,
        &mut |_| {},
    )
}

pub fn derive_alignment_query_with_metrics_and_phase(
    resident: &AlignmentQuery,
    overlap_index: &AlignmentOverlapIndex,
    interval: GenomicInterval,
    options: &QueryOptions,
    reference: Option<ReferenceSlice>,
    keep_going: &dyn Fn() -> bool,
    on_phase: &mut dyn FnMut(AlignmentDerivationPhase),
) -> Result<(AlignmentQuery, AlignmentDerivationMetrics), AlignmentError> {
    derive_alignment_query_with_summary_index_and_phase(
        resident,
        overlap_index,
        None,
        interval,
        options,
        reference,
        keep_going,
        on_phase,
    )
}

pub fn derive_alignment_query_with_summary_index_and_phase(
    resident: &AlignmentQuery,
    overlap_index: &AlignmentOverlapIndex,
    summary_index: Option<&crate::AlignmentSummaryIndex>,
    interval: GenomicInterval,
    options: &QueryOptions,
    reference: Option<ReferenceSlice>,
    keep_going: &dyn Fn() -> bool,
    on_phase: &mut dyn FnMut(AlignmentDerivationPhase),
) -> Result<(AlignmentQuery, AlignmentDerivationMetrics), AlignmentError> {
    if !resident.individual_reads_complete()
        || resident.interval.contig() != interval.contig()
        || resident.interval.start() > interval.start()
        || resident.interval.end() < interval.end()
        || !resident.batch.filtered_record_spans_complete()
    {
        return Err(AlignmentError::IncompatibleResidentQuery);
    }

    on_phase(AlignmentDerivationPhase::SelectingResidentReads);
    let lookup_started = Instant::now();
    let selected = overlap_index.select(&resident.batch, interval);
    let lookup_micros = duration_micros(lookup_started.elapsed());
    let projection_started = Instant::now();
    let maximum_records = options.max_records.get();
    let too_dense = selected.len() > maximum_records;
    if !keep_going() {
        return Ok((
            cancelled_query(interval, Arc::clone(&resident.header)),
            AlignmentDerivationMetrics {
                lookup_micros,
                ..AlignmentDerivationMetrics::default()
            },
        ));
    }
    let mut batch = if too_dense {
        AlignmentBatch::default()
    } else {
        resident
            .batch
            .project(&selected)
            .map_err(AlignmentError::BatchOverflow)?
    };
    let mut stats = resident
        .batch
        .copy_filtered_records_within(interval, &mut batch);
    stats.records_seen = stats.records_seen.saturating_add(selected.len());
    stats.records_eligible = selected.len();
    stats.complete_read_records_eligible = selected.len();
    stats.complete_read_limit = maximum_records;
    stats.records_emitted = if too_dense { 0 } else { selected.len() };
    stats.truncated = too_dense;
    stats.individual_reads = if too_dense {
        IndividualReadStatus::TooDense {
            limit: maximum_records,
        }
    } else {
        IndividualReadStatus::Complete
    };
    if !keep_going() {
        return Ok((
            cancelled_query(interval, Arc::clone(&resident.header)),
            AlignmentDerivationMetrics {
                lookup_micros,
                projection_micros: duration_micros(projection_started.elapsed()),
                ..AlignmentDerivationMetrics::default()
            },
        ));
    }
    let projection_micros = duration_micros(projection_started.elapsed());

    on_phase(AlignmentDerivationPhase::BuildingSummaries);
    let summary_started = Instant::now();
    let indexed = summary_index.filter(|index| {
        !options.collect_alleles
            && index.supports(
                interval,
                resident.batch.len(),
                options.overlapping_mate_policy,
            )
    });
    let mut pileup = indexed.is_none().then(|| {
        PileupBuilder::new(
            interval,
            options.coverage_bins.get(),
            options.minimum_base_quality,
        )
    });
    let indexed_modifications_absent = indexed
        .is_some_and(|index| options.collect_modifications && !index.modification_tags_present());
    let mut modifications =
        (options.collect_modifications && !indexed_modifications_absent).then(|| {
            crate::modification_summary::ModificationSummaryBuilder::new(
                interval,
                options.coverage_bins.get(),
            )
        });
    let mut junctions = JunctionSummaryBuilder::new(
        interval,
        options.minimum_junction_flank,
        resident.junctions.provenance.normalization_mapped_records,
    );
    let mut pairs = indexed.is_none().then(crate::pair::PairSummaryBuilder::new);
    let mut overlap_collapser = indexed
        .is_none()
        .then(|| OverlapCollapser::new(options.overlapping_mate_policy));
    if let Some(indexed) = indexed {
        indexed.add_selected_junctions(&selected, &mut junctions);
        if let Some(modifications) = &mut modifications {
            for (ordinal, index) in selected.iter().copied().enumerate() {
                if ordinal.is_multiple_of(options.cancellation_chunk.get()) && !keep_going() {
                    return Ok((
                        cancelled_query(interval, Arc::clone(&resident.header)),
                        AlignmentDerivationMetrics {
                            lookup_micros,
                            projection_micros,
                            summary_micros: duration_micros(summary_started.elapsed()),
                            ..AlignmentDerivationMetrics::default()
                        },
                    ));
                }
                let record = resident
                    .batch
                    .record_view(index)
                    .ok_or(crate::PackedRecordError::InvalidIndex(index))?;
                if record.has_auxiliary_tag(*b"MM") || record.has_auxiliary_tag(*b"Mm") {
                    modifications.add_record(&record.materialize()?);
                }
            }
        }
    } else {
        for (ordinal, index) in selected.iter().copied().enumerate() {
            if ordinal.is_multiple_of(options.cancellation_chunk.get()) && !keep_going() {
                return Ok((
                    cancelled_query(interval, Arc::clone(&resident.header)),
                    AlignmentDerivationMetrics {
                        lookup_micros,
                        projection_micros,
                        summary_micros: duration_micros(summary_started.elapsed()),
                        ..AlignmentDerivationMetrics::default()
                    },
                ));
            }
            let record = resident
                .batch
                .record_view(index)
                .ok_or(crate::PackedRecordError::InvalidIndex(index))?;
            let pairs = pairs.as_mut().expect("unindexed pair builder exists");
            pairs.add_fields(
                record.read_name(),
                record.contig(),
                record.flags(),
                record.mate_contig(),
                record.template_length(),
            );
            let overlap_collapser = overlap_collapser
                .as_mut()
                .expect("unindexed overlap collapser exists");
            let overlap_mask = overlap_collapser.mask_for_alignment(
                MateAlignment {
                    read_name: record.read_name(),
                    contig: record.contig(),
                    start: record.start(),
                    end: record.end(),
                    flags: record.flags(),
                    mate_contig: record.mate_contig(),
                    mate_start: record.mate_start(),
                },
                record.cigar().iter().copied(),
                view_base_observation_filter(
                    record,
                    options.collect_alleles,
                    options.minimum_base_quality,
                ),
            );
            let collapsed = add_record_view_pileup(
                record,
                pileup.as_mut().expect("unindexed pileup builder exists"),
                &mut junctions,
                options.collect_alleles,
                overlap_mask.as_ref(),
            )?;
            overlap_collapser.note_collapsed(collapsed);
            if let Some(modifications) = &mut modifications
                && (record.has_auxiliary_tag(*b"MM") || record.has_auxiliary_tag(*b"Mm"))
            {
                modifications.add_record(&record.materialize()?);
            }
        }
    }

    let mut pileup = if let Some(indexed) = indexed {
        indexed.pileup(
            interval,
            options.coverage_bins.get(),
            options.minimum_base_quality,
            &selected,
            stats.records_emitted,
        )
    } else {
        let overlap_collapser = overlap_collapser.expect("unindexed overlap collapser exists");
        pileup
            .expect("unindexed pileup builder exists")
            .finish(PileupProvenance {
                eligible_records: stats.records_eligible,
                rendered_records: stats.records_emitted,
                all_eligible_records: true,
                allele_observations_available: options.collect_alleles,
                overlapping_mate_policy: overlap_collapser.policy(),
                overlap_collapse_complete: overlap_collapser.complete(),
                collapsed_mate_bases: overlap_collapser.collapsed_mate_bases(),
                ..PileupProvenance::default()
            })
    };
    if options.collect_modifications {
        pileup.modifications = modifications.map_or_else(
            || {
                crate::modification_summary::ModificationSummaryBuilder::new(
                    interval,
                    options.coverage_bins.get(),
                )
                .finish(stats.records_eligible, true)
            },
            |modifications| modifications.finish(stats.records_eligible, true),
        );
    }
    debug_assert!(batch.validate());
    let query = AlignmentQuery {
        interval,
        header: Arc::clone(&resident.header),
        batch,
        pileup,
        junctions: junctions.finish(stats.records_eligible, true),
        pairs: indexed.map_or_else(
            || pairs.expect("unindexed pair builder exists").finish(true),
            |indexed| indexed.pair_summary(&selected),
        ),
        reference,
        stats,
    };
    Ok((
        query,
        AlignmentDerivationMetrics {
            lookup_micros,
            projection_micros,
            summary_micros: duration_micros(summary_started.elapsed()),
            summary_tile_hits: indexed.map_or(0, |index| index.tile_count_for(interval)),
        },
    ))
}

/// Builds the dictionary of an alignment or reference header.
///
/// A contig name that occurs twice is an error rather than a silently shadowed entry: records
/// and regions are resolved by name, so the second contig could never be reached by its name
/// and everything addressed to it would be attributed to the first.
pub(crate) fn header_reference_dictionary(
    contigs: impl IntoIterator<Item = (String, u64)>,
) -> Result<ReferenceDictionary, AlignmentError> {
    Ok(ReferenceDictionary::try_from_contigs(contigs)?)
}

fn duration_micros(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

/// Reports which `SEQ` indices of a packed record are counted as base observations, mirroring
/// the eligibility applied by [`add_record_view_pileup`].
fn view_base_observation_filter(
    record: AlignmentRecordView<'_>,
    collect_alleles: bool,
    minimum_base_quality: u8,
) -> impl FnMut(usize) -> bool {
    let (base_count, qualities) = match record.sequence() {
        SequenceView::Present { bases, qualities } => (bases.len(), qualities.unwrap_or(&[])),
        SequenceView::NotLoaded | SequenceView::Missing => (0, &[][..]),
    };
    move |read_position| {
        !collect_alleles
            || base_observation_is_counted(
                base_count,
                qualities,
                read_position,
                minimum_base_quality,
            )
    }
}

fn add_record_view_pileup(
    record: AlignmentRecordView<'_>,
    pileup: &mut PileupBuilder,
    junctions: &mut JunctionSummaryBuilder,
    collect_alleles: bool,
    overlap_mask: Option<&MateOverlapMask>,
) -> Result<u64, AlignmentError> {
    let observations = collect_alleles.then(|| match record.sequence() {
        SequenceView::Present { bases, qualities } => (bases, qualities.unwrap_or(&[])),
        SequenceView::NotLoaded | SequenceView::Missing => (&[][..], &[][..]),
    });
    let strand = record.auxiliary_field(*b"XS")?;
    Ok(add_alignment_pileup(
        &PileupAlignment {
            start: record.start(),
            cigar: record.cigar(),
            flags: record.flags(),
            mapping_quality: record.mapping_quality(),
            observations,
            junction_strand: junction_strand_input(strand.as_ref().map(|field| &field.value)),
        },
        pileup,
        junctions,
        overlap_mask,
    ))
}

pub trait AlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary;

    /// Validated source generation used by immutable scene handles.
    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        None
    }

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

#[derive(Clone, Debug, PartialEq)]
pub struct PositionedAlignmentRecord {
    pub record: AlignmentRecord,
    pub source_position: crate::AlignmentSourcePosition,
}

/// One bounded BGZF owner unit from a resumable indexed BAM transaction. The records are exactly
/// the eligible records observed by this regional transaction in the physical unit; transaction
/// completeness is reported separately and must never be inferred from this fragment alone.
#[derive(Clone, Debug, PartialEq)]
pub struct PositionedAlignmentBlock {
    pub compressed_offset: u64,
    pub records: Vec<PositionedAlignmentRecord>,
    pub genomic_span: Option<GenomicInterval>,
}

impl PositionedAlignmentBlock {
    /// Logical heap capacity retained by this completed decoder owner block. Source adapters use
    /// this measured block size to reserve the duplicate packing/copy working set before creating
    /// canonical scene allocations. HTSlib's internal decoder buffers remain separately reported
    /// process overhead because they are allocated below this Rust ownership boundary.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.records
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PositionedAlignmentRecord>()),
            )
            .saturating_add(self.records.iter().fold(0_usize, |total, positioned| {
                total.saturating_add(alignment_record_heap_bytes(&positioned.record))
            }))
    }
}

fn alignment_record_heap_bytes(record: &AlignmentRecord) -> usize {
    let sequence_bytes = match &record.sequence {
        SequenceData::Present { bases, qualities } => bases.capacity().saturating_add(
            qualities
                .as_ref()
                .map_or(0, |qualities| qualities.capacity()),
        ),
        SequenceData::NotLoaded | SequenceData::Missing => 0,
    };
    record
        .read_name
        .capacity()
        .saturating_add(
            record
                .cigar
                .capacity()
                .saturating_mul(std::mem::size_of::<crate::CigarOp>()),
        )
        .saturating_add(sequence_bytes)
        .saturating_add(
            record
                .auxiliary_fields
                .capacity()
                .saturating_mul(std::mem::size_of::<crate::AuxField>()),
        )
        .saturating_add(
            record
                .auxiliary_fields
                .iter()
                .fold(0_usize, |total, field| {
                    total.saturating_add(auxiliary_value_heap_bytes(&field.value))
                }),
        )
}

fn auxiliary_value_heap_bytes(value: &crate::AuxValue) -> usize {
    match value {
        crate::AuxValue::String(value) | crate::AuxValue::HexByteArray(value) => value.capacity(),
        crate::AuxValue::SignedArray(value) => {
            value.capacity().saturating_mul(std::mem::size_of::<i64>())
        }
        crate::AuxValue::UnsignedArray(value) => {
            value.capacity().saturating_mul(std::mem::size_of::<u64>())
        }
        crate::AuxValue::FloatArray(value) => {
            value.capacity().saturating_mul(std::mem::size_of::<f32>())
        }
        crate::AuxValue::Character(_)
        | crate::AuxValue::Signed(_)
        | crate::AuxValue::Unsigned(_)
        | crate::AuxValue::Float(_)
        | crate::AuxValue::Double(_) => 0,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PositionedAlignmentStreamCompletion {
    pub interval: GenomicInterval,
    pub stats: QueryStats,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PositionedAlignmentStreamCheckpoint {
    Block(PositionedAlignmentBlock),
    Complete(PositionedAlignmentStreamCompletion),
    Cancelled(PositionedAlignmentStreamCompletion),
}

struct LocalPositionedStreamState {
    admission: PositionedAdmission,
    current_block: Option<u64>,
    block: PositionedBlockBuilder,
    pending: Option<(bam::Record, crate::AlignmentSourcePosition)>,
    reached_end: bool,
    cancelled: bool,
    /// Virtual offset just past the last record processed into the current block.
    block_end: u64,
    /// A record of the target beyond the current block was read (or skipped).
    later_record_seen: bool,
    /// Receipt of the block the last checkpoint returned.
    receipt: Option<PositionedChunkReceipt>,
}

pub struct LocalAlignmentSource {
    reader: bam::IndexedReader,
    encoding: LocalAlignmentEncoding,
    dictionary: ReferenceDictionary,
    header: Arc<AlignmentHeaderMetadata>,
    normalization_mapped_records: Option<u64>,
    index_overview: Option<crate::AlignmentIndexOverview>,
    source_stamp: Option<AlignmentSourceStamp>,
    source_guard: Option<LocalSourceGuard>,
    positioned_stream: Option<LocalPositionedStreamState>,
    /// Declared after `reader`: the reader (and its thread pool) is dropped before its
    /// threads are handed back to the process-wide allowance.
    decompression_threads: DecompressionThreadLease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalAlignmentEncoding {
    Bam,
    Cram,
    Unknown,
}

impl LocalAlignmentSource {
    /// Immutable parsed SAM header metadata retained for scene-backed inspection.
    #[must_use]
    pub fn header_metadata(&self) -> &Arc<AlignmentHeaderMetadata> {
        &self.header
    }

    /// `HTSlib` decompression threads granted to this source from the process-wide allowance:
    /// at most the requested [`AlignmentOpenOptions::decompression_threads`], and zero when
    /// other open sources already hold the whole allowance.
    #[must_use]
    pub const fn decompression_threads(&self) -> u32 {
        self.decompression_threads.threads
    }

    /// Exact mapped-record total obtained from constant-time local BAM index statistics when it
    /// is available. Scene-owned CPM summaries carry this as an explicit source dependency.
    #[must_use]
    pub const fn normalization_mapped_records(&self) -> Option<u64> {
        self.normalization_mapped_records
    }

    pub fn open(
        path: impl AsRef<Path>,
        options: &AlignmentOpenOptions,
    ) -> Result<Self, AlignmentError> {
        let path = path.as_ref();
        let raw_path = path.to_string_lossy();
        let encoding = match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("bam") => LocalAlignmentEncoding::Bam,
            Some("cram") => LocalAlignmentEncoding::Cram,
            _ => LocalAlignmentEncoding::Unknown,
        };
        if has_url_scheme(&raw_path) {
            // Remote objects are read by the pure-Rust range reader, which accounts, bounds,
            // authenticates and cancels every request. `HTSlib` never opens a URL, whatever
            // its scheme (it would hand `http`, `ftp`, `s3` and `gs` alike to libcurl).
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "HTSlib does not open alignment URLs; http and https URLs are opened through \
                 ProductionAlignmentSource and other schemes are not supported",
            )
            .into());
        }
        let mut reader = if let Some(index_path) = &options.index_path {
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
        let decompression_threads = DECOMPRESSION_THREADS.lease(
            options.decompression_threads.get(),
            decompression_thread_limit(),
        );
        if decompression_threads.threads > 0 {
            let pool = ThreadPool::new(decompression_threads.threads)?;
            reader.set_thread_pool(&pool)?;
        }

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
            header_reference_dictionary(header.target_names().into_iter().enumerate().map(
                |(index, name)| {
                    let length = header.target_len(index as u32).unwrap_or_default();
                    (String::from_utf8_lossy(name).into_owned(), length)
                },
            ))?;
        let index_overview =
            load_local_alignment_index_overview(path, options.index_path.as_deref(), &dictionary);
        let format = match encoding {
            LocalAlignmentEncoding::Bam => Some(AlignmentSourceFormat::Bam),
            LocalAlignmentEncoding::Cram => Some(AlignmentSourceFormat::Cram),
            LocalAlignmentEncoding::Unknown => None,
        };
        let (source_stamp, source_guard) = if let Some(format) = format {
            let index_path =
                local_alignment_index_candidates(path, options.index_path.as_deref(), format)
                    .into_iter()
                    .find(|candidate| candidate.is_file())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "opened alignment index could not be resolved for revision capture",
                        )
                    })?;
            let guard = LocalSourceGuard::capture(path, &index_path)?;
            let stamp = options.source_stamp_override.map_or_else(
                || local_source_stamp(path, &index_path, format, &dictionary, &metadata),
                Ok,
            )?;
            (Some(stamp), Some(guard))
        } else {
            (options.source_stamp_override, None)
        };
        Ok(Self {
            reader,
            encoding,
            dictionary,
            header: metadata,
            normalization_mapped_records,
            index_overview,
            source_stamp,
            source_guard,
            positioned_stream: None,
            decompression_threads,
        })
    }

    /// Starts a resumable indexed BAM transaction. Calling this again retargets by discarding only
    /// the unfinished transaction state; already returned blocks remain independently usable.
    pub fn begin_positioned_stream(
        &mut self,
        interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), AlignmentError> {
        self.begin_positioned_stream_with_admission(interval, interval, options)
    }

    /// Starts a resumable physical acquisition while applying Complete-read admission only to
    /// the contained exact foreground interval. This keeps adjacent cache population out of the
    /// user-visible viewport limit without weakening completeness of the acquired evidence.
    pub fn begin_positioned_stream_with_admission(
        &mut self,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
    ) -> Result<(), AlignmentError> {
        if self.encoding != LocalAlignmentEncoding::Bam {
            return Err(AlignmentError::PositionedStreamUnsupported);
        }
        let admission = PositionedAdmission::new(interval, admission_interval, options)?;
        if let Some(guard) = &self.source_guard
            && !guard.validate()?
        {
            return Err(AlignmentError::SourceChanged);
        }
        let tid = i32::try_from(interval.contig().get())
            .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let start =
            i64::try_from(interval.start()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        let end = i64::try_from(interval.end()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        self.reader.fetch((tid, start, end))?;
        self.positioned_stream = Some(LocalPositionedStreamState {
            admission,
            current_block: None,
            block: PositionedBlockBuilder::default(),
            pending: None,
            reached_end: false,
            cancelled: false,
            block_end: 0,
            later_record_seen: false,
            receipt: None,
        });
        Ok(())
    }

    /// Starts a positioned stream like [`Self::begin_positioned_stream_with_admission`] that
    /// does not decode the chunks named by `skip` again.
    ///
    /// `skip` holds receipts of chunks an earlier stream of this source returned and whose
    /// records the caller still has. A receipt is honoured when
    /// [`PositionedChunkReceipt::can_resume`] holds for `interval` and `options`; any other is
    /// ignored and its chunk is decoded as usual. The number honoured is returned.
    ///
    /// The stream then returns exactly the blocks a fresh stream would, in the same order,
    /// except the skipped ones, and completes with the statistics a fresh stream would report:
    /// the skipped chunks' records are accounted (seen, filtered, eligible, admitted) from
    /// their receipts, restricted to `interval` and `admission_interval`.
    ///
    /// `HTSlib` learns which block a record belongs to only by reading it, so the first record
    /// of a run of consecutive skipped chunks is read and dropped before the reader seeks past
    /// the run; the rest of the run is neither inflated nor parsed.
    pub fn begin_positioned_stream_resuming(
        &mut self,
        interval: GenomicInterval,
        admission_interval: GenomicInterval,
        options: QueryOptions,
        skip: Vec<PositionedChunkReceipt>,
    ) -> Result<usize, AlignmentError> {
        self.begin_positioned_stream_with_admission(interval, admission_interval, options)?;
        Ok(self
            .positioned_stream
            .as_mut()
            .expect("the positioned stream was just started")
            .admission
            .set_skipped(skip, PositionedChunkReceipt::is_bam))
    }

    /// The receipt of the block the last checkpoint returned, once. Keep it with the block's
    /// records to let a later [`Self::begin_positioned_stream_resuming`] skip the chunk.
    pub fn take_positioned_chunk_receipt(&mut self) -> Option<PositionedChunkReceipt> {
        self.positioned_stream.as_mut()?.receipt.take()
    }

    /// Abandons the unfinished positioned transaction without invalidating any blocks already
    /// returned to its owner.
    pub fn cancel_positioned_stream(&mut self) {
        self.positioned_stream = None;
    }

    /// Performs at most one physical BAM owner unit of source work.
    #[allow(clippy::too_many_lines)]
    pub fn next_positioned_stream_checkpoint(
        &mut self,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
        if self.positioned_stream.is_none() {
            return Err(AlignmentError::NoActivePositionedStream);
        }
        loop {
            let reached_end = self
                .positioned_stream
                .as_ref()
                .is_some_and(|state| state.reached_end);
            if reached_end {
                if self
                    .positioned_stream
                    .as_ref()
                    .is_some_and(|state| state.current_block.is_some())
                {
                    return self.finish_positioned_block();
                }
                let state = self
                    .positioned_stream
                    .take()
                    .expect("active positioned stream was checked");
                let cancelled = state.cancelled;
                let (interval, mut totals) = state.admission.finish();
                totals.cancelled = cancelled;
                let completion = PositionedAlignmentStreamCompletion {
                    interval,
                    stats: totals,
                };
                return Ok(if completion.stats.cancelled {
                    PositionedAlignmentStreamCheckpoint::Cancelled(completion)
                } else {
                    PositionedAlignmentStreamCheckpoint::Complete(completion)
                });
            }

            let pending = self
                .positioned_stream
                .as_mut()
                .and_then(|state| state.pending.take());
            let next = if let Some(pending) = pending {
                Some(Ok(pending))
            } else {
                let mut record = bam::Record::new();
                self.reader.read(&mut record).map(|result| {
                    result.and_then(|()| {
                        let position = u64::try_from(self.reader.tell())
                            .map_err(|_| rust_htslib::errors::Error::FileSeek)?;
                        Ok((
                            record,
                            crate::AlignmentSourcePosition::bam_virtual_offset(position),
                        ))
                    })
                })
            };
            let Some(next) = next else {
                self.positioned_stream
                    .as_mut()
                    .expect("active positioned stream was checked")
                    .reached_end = true;
                continue;
            };
            let (record, source_position) = next?;
            let compressed_offset = match source_position {
                crate::AlignmentSourcePosition::BamVirtualOffset(offset) => offset >> 16,
                crate::AlignmentSourcePosition::CramRecord { .. } => {
                    return Err(AlignmentError::PositionedStreamUnsupported);
                }
            };
            let has_completed_block = self
                .positioned_stream
                .as_ref()
                .and_then(|state| state.current_block)
                .is_some_and(|current| current != compressed_offset);
            let record_end = match source_position {
                crate::AlignmentSourcePosition::BamVirtualOffset(offset) => offset,
                crate::AlignmentSourcePosition::CramRecord { .. } => 0,
            };
            let state = self
                .positioned_stream
                .as_mut()
                .expect("active positioned stream was checked");
            if state.admission.resume().has_skipped()
                && let Some(resume_at) = state
                    .admission
                    .skip_from_owner(compressed_offset, record_end)
            {
                // The record belongs to a chunk the caller already holds: drop it and seek
                // past that chunk and the skipped chunks directly following it.
                state.later_record_seen = true;
                self.reader.seek(
                    i64::try_from(resume_at).map_err(|_| AlignmentError::CoordinateOverflow)?,
                )?;
                continue;
            }
            if has_completed_block {
                state.pending = Some((record, source_position));
                state.later_record_seen = true;
                return self.finish_positioned_block();
            }
            if state.admission.note_seen() && !keep_going() {
                state.cancelled = true;
                state.reached_end = true;
                state.block.mark_incomplete();
                continue;
            }
            if state.current_block.is_none() {
                state.later_record_seen = false;
            }
            state.current_block = Some(compressed_offset);
            state.block_end = record_end;
            state.admission.process(
                &record,
                source_position,
                compressed_offset,
                &mut state.block,
            )?;
        }
    }

    fn finish_positioned_block(
        &mut self,
    ) -> Result<PositionedAlignmentStreamCheckpoint, AlignmentError> {
        let state = self
            .positioned_stream
            .as_mut()
            .ok_or(AlignmentError::NoActivePositionedStream)?;
        let compressed_offset = state
            .current_block
            .take()
            .ok_or(AlignmentError::NoActivePositionedStream)?;
        let interval = state.admission.interval;
        let block = state.block.finish(compressed_offset, interval);
        state.receipt = Some(state.admission.issue_receipt(
            &mut state.block,
            ChunkCompletion {
                owner: compressed_offset,
                cram_slice_landmark: None,
                // HTSlib's region iterator does not expose where a record starts.
                extent: Some(ChunkExtent {
                    start: None,
                    end: state.block_end,
                }),
                left_complete: None,
                right_complete: std::mem::take(&mut state.later_record_seen),
            },
        ));
        Ok(PositionedAlignmentStreamCheckpoint::Block(block))
    }
}

/// Whether `value` is a URL (`scheme://...`) rather than a filesystem path.
fn has_url_scheme(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|first: char| first.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "+.-".contains(character))
    })
}

impl AlignmentSource for LocalAlignmentSource {
    fn reference_dictionary(&self) -> &ReferenceDictionary {
        &self.dictionary
    }

    fn source_stamp(&self) -> Option<AlignmentSourceStamp> {
        self.source_stamp
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
        self.positioned_stream = None;
        if let Some(guard) = &self.source_guard
            && !guard.validate()?
        {
            return Err(AlignmentError::SourceChanged);
        }
        if !keep_going() {
            return Ok(cancelled_query(interval, Arc::clone(&self.header)));
        }
        let tid = i32::try_from(interval.contig().get())
            .map_err(|_| AlignmentError::CoordinateOverflow)?;
        let start =
            i64::try_from(interval.start()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        let end = i64::try_from(interval.end()).map_err(|_| AlignmentError::CoordinateOverflow)?;
        self.reader.fetch((tid, start, end))?;

        let mut accumulator =
            RegionQueryAccumulator::new(interval, &options, self.normalization_mapped_records);
        let mut record = bam::Record::new();
        while let Some(result) = self.reader.read(&mut record) {
            result?;
            let source_position = (self.encoding == LocalAlignmentEncoding::Bam)
                .then(|| u64::try_from(self.reader.tell()).ok())
                .flatten()
                .map(crate::AlignmentSourcePosition::bam_virtual_offset);
            if !accumulator.add(&record, source_position, keep_going)? {
                break;
            }
        }
        accumulator.finish(Arc::clone(&self.header))
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

#[cfg(test)]
fn normalize_record(
    record: &bam::Record,
    detail: QueryDetail,
    retained_auxiliary_tags: &[[u8; 2]],
) -> Result<AlignmentRecord, AlignmentError> {
    let mut cigar = Vec::new();
    crate::backend_record::BackendRecord::cigar_into(record, &mut cigar)?;
    crate::backend_record::normalize_record(record, &cigar, detail, retained_auxiliary_tags)
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
    #[error("alignment source or its index changed after it was opened")]
    SourceChanged,
    #[error("resumable physical-unit streaming is available only for positioned BAM sources")]
    PositionedStreamUnsupported,
    #[error("no resumable positioned alignment transaction is active")]
    NoActivePositionedStream,
    #[error("alignment batch exceeded its packed 32-bit arena offsets")]
    BatchOverflow(BatchOverflow),
    #[error(transparent)]
    PackedRecord(#[from] crate::PackedRecordError),
    #[error("resident alignment data cannot authoritatively answer this target")]
    IncompatibleResidentQuery,
    #[error("Complete-read admission interval {admission:?} is outside source query {query:?}")]
    AdmissionIntervalOutsideQuery {
        query: GenomicInterval,
        admission: GenomicInterval,
    },
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
    #[error("the header names contig {} more than once", .0.name)]
    DuplicateContig(#[from] rgv_core::DuplicateContigError),
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

    use rgv_core::ContigId;
    use rust_htslib::bam::record::Aux;

    use super::*;

    #[test]
    fn a_header_naming_a_contig_twice_is_rejected() {
        let duplicated = [
            ("chr1".to_owned(), 1_000),
            ("chr2".to_owned(), 500),
            ("chr1".to_owned(), 2_000),
        ];
        let error = header_reference_dictionary(duplicated).unwrap_err();
        assert!(
            matches!(&error, AlignmentError::DuplicateContig(duplicate) if duplicate.name == "chr1"),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "the header names contig chr1 more than once"
        );

        let distinct =
            header_reference_dictionary([("chr1".to_owned(), 1_000), ("chr2".to_owned(), 500)])
                .unwrap();
        assert_eq!(distinct.len(), 2);
        assert_eq!(distinct.id_by_name("chr2"), Some(ContigId::new(1)));
    }

    #[test]
    fn opening_a_bam_whose_reference_list_repeats_a_name_fails() {
        use std::io::Write as _;

        // A BAM with no header text and a binary reference list `chr1` (1000), `chr1` (2000).
        let mut header = b"BAM\x01".to_vec();
        header.extend_from_slice(&0_u32.to_le_bytes());
        header.extend_from_slice(&2_u32.to_le_bytes());
        for length in [1_000_u32, 2_000] {
            header.extend_from_slice(&5_u32.to_le_bytes());
            header.extend_from_slice(b"chr1\0");
            header.extend_from_slice(&length.to_le_bytes());
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("duplicate.bam");
        let mut writer = rust_htslib::bgzf::Writer::from_path(&path).unwrap();
        writer.write_all(&header).unwrap();
        drop(writer);
        let index = PathBuf::from(format!("{}.bai", path.display()));
        bam::index::build(&path, Some(&index), bam::index::Type::Bai, 1).unwrap();

        let error = LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default())
            .err()
            .unwrap();
        assert!(
            matches!(&error, AlignmentError::DuplicateContig(duplicate) if duplicate.name == "chr1"),
            "{error:?}"
        );
    }

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
    fn decompression_thread_leases_never_exceed_their_allowance_and_are_returned() {
        static ALLOWANCE: DecompressionThreadAllowance = DecompressionThreadAllowance {
            leased: AtomicU32::new(0),
        };
        let first = ALLOWANCE.lease(4, 10);
        let second = ALLOWANCE.lease(4, 10);
        let third = ALLOWANCE.lease(4, 10);
        let fourth = ALLOWANCE.lease(4, 10);
        assert_eq!(
            [first.threads, second.threads, third.threads, fourth.threads],
            [4, 4, 2, 0]
        );
        assert_eq!(ALLOWANCE.leased.load(Ordering::Acquire), 10);
        drop(second);
        assert_eq!(ALLOWANCE.leased.load(Ordering::Acquire), 6);
        assert_eq!(ALLOWANCE.lease(8, 10).threads, 4);
        drop((first, third, fourth));
        assert_eq!(ALLOWANCE.leased.load(Ordering::Acquire), 0);

        // Leases taken from many threads at once still respect the limit.
        let leases = std::thread::scope(|scope| {
            let workers = (0..16)
                .map(|_| scope.spawn(|| ALLOWANCE.lease(3, 10)))
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(leases.iter().map(|lease| lease.threads).sum::<u32>(), 10);
        drop(leases);
        assert_eq!(ALLOWANCE.leased.load(Ordering::Acquire), 0);
    }

    #[test]
    fn opening_many_local_sources_does_not_grow_the_decompression_thread_count() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("many.bam");
        write_positioned_stream_fixture(&path, 400);
        let limit = decompression_thread_limit();
        assert!((2..=16).contains(&limit));
        let options = AlignmentOpenOptions::default();
        let requested = options.decompression_threads.get();
        let count = 24_u32;
        assert!(count * requested > limit);
        let mut sources = (0..count)
            .map(|_| {
                let source = LocalAlignmentSource::open(&path, &options).unwrap();
                // Other tests of this process lease from the same allowance concurrently; the
                // bound holds for everyone together.
                assert!(DECOMPRESSION_THREADS.leased.load(Ordering::Acquire) <= limit);
                source
            })
            .collect::<Vec<_>>();
        // 24 sources asking for 4 threads each hold at most the allowance, not 96 threads.
        let granted = sources
            .iter()
            .map(LocalAlignmentSource::decompression_threads)
            .sum::<u32>();
        assert!(granted <= limit, "{granted} threads for a limit of {limit}");
        let unthreaded = sources
            .iter()
            .filter(|source| source.decompression_threads() == 0)
            .count();
        assert!(unthreaded >= (count - limit) as usize);

        // A source without pool threads returns exactly what a threaded one does.
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100_000).unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for source in &mut sources {
            let query = source
                .query(interval, QueryOptions::default(), &|| true)
                .unwrap();
            assert!(query.stats.records_seen > 0);
            seen.insert((query.stats.records_seen, query.batch.len()));
        }
        assert_eq!(seen.len(), 1);
    }

    fn write_positioned_stream_fixture(path: &Path, record_count: usize) {
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
                .push_tag(b"LN", 1_000_000),
        );
        let mut writer = Writer::from_path(path, &header, Format::Bam).unwrap();
        for index in 0..record_count {
            let name = format!("positioned-{index:08}");
            let mut record = Record::new();
            record.set(
                name.as_bytes(),
                Some(&CigarString(vec![Cigar::Equal(75)])),
                &[b'A'; 75],
                &[35; 75],
            );
            record.set_tid(0);
            record.set_pos(i64::try_from(index.saturating_mul(3)).unwrap());
            record.set_mapq(60);
            if index.is_multiple_of(101) {
                record.set_flags(0x100);
            } else if index.is_multiple_of(137) {
                record.set_flags(0x800);
            }
            writer.write(&record).unwrap();
        }
        drop(writer);
        let index = PathBuf::from(format!("{}.bai", path.display()));
        bam::index::build(path, Some(&index), bam::index::Type::Bai, 1).unwrap();
    }

    fn collect_positioned_stream(
        source: &mut LocalAlignmentSource,
        interval: GenomicInterval,
        options: QueryOptions,
    ) -> (
        Vec<PositionedAlignmentBlock>,
        PositionedAlignmentStreamCompletion,
    ) {
        source.begin_positioned_stream(interval, options).unwrap();
        let mut blocks = Vec::new();
        loop {
            match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
                PositionedAlignmentStreamCheckpoint::Block(block) => blocks.push(block),
                PositionedAlignmentStreamCheckpoint::Complete(completion) => {
                    return (blocks, completion);
                }
                PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                    panic!("an uncancelled positioned stream was cancelled")
                }
            }
        }
    }

    /// Two overlapping pairs with hand-computed pileups.
    ///
    /// Pair `del`: first mate at 100 with `4M2D4M` (bases `A` at 100..104 and 106..110,
    /// deletion 104..106; the base at 106 has quality 5), second mate at 102 with `10M`
    /// (bases `C` at 102..112).
    /// Pair `skip`: first mate at 200 with `3M4N3M` (bases `G` at 200..203 and 207..210),
    /// second mate at 203 with `5M` (bases `T` at 203..208).
    fn write_overlapping_mate_fixture(path: &Path) {
        use rust_htslib::bam::{
            Format, Header, Record, Writer,
            header::HeaderRecord,
            record::{Cigar, CigarString},
        };
        type Mate<'a> = (&'a [u8], i64, Vec<Cigar>, &'a [u8], &'a [u8], u16, i64);

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
        let first = 0x1 | 0x20 | 0x40;
        let second = 0x1 | 0x10 | 0x80;
        let records: [Mate<'_>; 4] = [
            (
                b"del",
                100,
                vec![Cigar::Match(4), Cigar::Del(2), Cigar::Match(4)],
                b"AAAAAAAA",
                &[30, 30, 30, 30, 5, 30, 30, 30],
                first,
                102,
            ),
            (
                b"del",
                102,
                vec![Cigar::Match(10)],
                b"CCCCCCCCCC",
                &[30; 10],
                second,
                100,
            ),
            (
                b"skip",
                200,
                vec![Cigar::Match(3), Cigar::RefSkip(4), Cigar::Match(3)],
                b"GGGGGG",
                &[30; 6],
                first,
                203,
            ),
            (
                b"skip",
                203,
                vec![Cigar::Match(5)],
                b"TTTTT",
                &[30; 5],
                second,
                200,
            ),
        ];
        for (name, position, cigar, sequence, qualities, flags, mate_position) in records {
            let mut record = Record::new();
            record.set(name, Some(&CigarString(cigar)), sequence, qualities);
            record.set_tid(0);
            record.set_pos(position);
            record.set_mapq(60);
            record.set_flags(flags);
            record.set_mtid(0);
            record.set_mpos(mate_position);
            writer.write(&record).unwrap();
        }
        drop(writer);
        let index = PathBuf::from(format!("{}.bai", path.display()));
        bam::index::build(path, Some(&index), bam::index::Type::Bai, 1).unwrap();
    }

    fn overlapping_mate_pileup(
        path: &Path,
        policy: OverlappingMatePolicy,
        collect_alleles: bool,
    ) -> crate::PileupSummary {
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        source
            .query(
                GenomicInterval::new(ContigId::new(0), 95, 215).unwrap(),
                QueryOptions {
                    coverage_bins: NonZeroUsize::new(120).unwrap(),
                    collect_alleles,
                    minimum_base_quality: 10,
                    overlapping_mate_policy: policy,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap()
            .pileup
    }

    #[test]
    fn count_once_masks_only_what_the_earlier_mate_contributed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("overlapping-mates.bam");
        write_overlapping_mate_fixture(&path);
        let pileup = overlapping_mate_pileup(&path, OverlappingMatePolicy::CountOnce, true);
        let bin = |position: u64| pileup.bins[usize::try_from(position - 95).unwrap()];
        let depth = |range: std::ops::Range<u64>| range.map(|p| bin(p).depth).collect::<Vec<_>>();

        // Pair `del`: every template position 100..112 has exactly one aligned base. 104/105
        // are aligned only in the second mate (the first has a deletion there).
        assert_eq!(
            depth(98..114),
            [0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0]
        );
        for position in [100, 101, 102, 103, 107, 108, 109] {
            assert_eq!((bin(position).alleles.a, bin(position).alleles.c), (1, 0));
            assert_eq!(bin(position).alleles.deletion, 0);
        }
        // The first mate's deletion is the template's single observation at 104/105.
        for position in [104, 105] {
            assert_eq!(bin(position).alleles.deletion, 1);
            assert_eq!(bin(position).alleles.base_observations(), 0);
        }
        // The first mate's base at 106 fails quality, so the second mate's `C` is counted.
        assert_eq!((bin(106).alleles.a, bin(106).alleles.c), (0, 1));
        assert_eq!(bin(106).reverse_alleles.c, 1);
        for position in [110, 111] {
            assert_eq!((bin(position).alleles.a, bin(position).alleles.c), (0, 1));
        }
        // Pair `skip`: the second mate fills the first mate's `N` gap 203..207.
        assert_eq!(depth(199..211), [0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0]);
        for position in [200, 201, 202, 207, 208, 209] {
            assert_eq!((bin(position).alleles.g, bin(position).alleles.t), (1, 0));
        }
        for position in [203, 204, 205, 206] {
            assert_eq!((bin(position).alleles.g, bin(position).alleles.t), (0, 1));
        }
        // Collapsed aligned bases: 102,103,106..110 of `del` and 207 of `skip`.
        assert_eq!(pileup.provenance.collapsed_mate_bases, 7);
        assert!(pileup.provenance.overlap_collapse_complete);

        let both = overlapping_mate_pileup(&path, OverlappingMatePolicy::CountBoth, true);
        let bin = |position: u64| both.bins[usize::try_from(position - 95).unwrap()];
        assert_eq!(
            (100..112).map(|p| bin(p).depth).collect::<Vec<_>>(),
            [1, 1, 2, 2, 1, 1, 2, 2, 2, 2, 1, 1]
        );
        assert_eq!((bin(104).alleles.deletion, bin(104).alleles.c), (1, 1));
        assert_eq!(both.provenance.collapsed_mate_bases, 0);
    }

    #[test]
    fn resident_derivations_collapse_overlapping_mates_like_a_direct_query() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("overlapping-mates.bam");
        write_overlapping_mate_fixture(&path);
        let interval = GenomicInterval::new(ContigId::new(0), 95, 215).unwrap();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        for collect_alleles in [true, false] {
            let options = QueryOptions {
                detail: QueryDetail::Full,
                coverage_bins: NonZeroUsize::new(120).unwrap(),
                collect_alleles,
                minimum_base_quality: 10,
                ..QueryOptions::default()
            };
            let direct = source.query(interval, options.clone(), &|| true).unwrap();
            assert_eq!(direct.pileup.provenance.collapsed_mate_bases, 7);
            let overlap = AlignmentOverlapIndex::new(&direct.batch);
            // The packed-record path applies the same CIGAR- and quality-aware mask.
            let derived =
                derive_alignment_query(&direct, &overlap, interval, &options, None, &|| true)
                    .unwrap();
            assert_eq!(derived.pileup, direct.pileup, "alleles: {collect_alleles}");
            if !collect_alleles {
                // So does the precomputed depth/indel summary index.
                let summaries = crate::AlignmentSummaryIndex::build(
                    &direct.batch,
                    interval,
                    options.overlapping_mate_policy,
                )
                .unwrap();
                let indexed = derive_alignment_query_with_summary_index_and_phase(
                    &direct,
                    &overlap,
                    Some(&summaries),
                    interval,
                    &options,
                    None,
                    &|| true,
                    &mut |_| {},
                )
                .unwrap()
                .0;
                assert_eq!(indexed.pileup, direct.pileup);
            }
        }
    }

    #[test]
    fn depth_only_queries_match_allele_queries_without_decoding_bases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("overlapping-mates.bam");
        write_overlapping_mate_fixture(&path);
        let with_alleles = overlapping_mate_pileup(&path, OverlappingMatePolicy::CountOnce, true);
        let depth_only = overlapping_mate_pileup(&path, OverlappingMatePolicy::CountOnce, false);
        assert!(!depth_only.provenance.allele_observations_available);
        for (depth_only, with_alleles) in depth_only.bins.iter().zip(&with_alleles.bins) {
            assert_eq!(depth_only.depth, with_alleles.depth);
            assert_eq!(depth_only.forward_depth, with_alleles.forward_depth);
            assert_eq!(depth_only.alleles.deletion, with_alleles.alleles.deletion);
            assert_eq!(depth_only.alleles.base_observations(), 0);
        }
        assert_eq!(depth_only.provenance.collapsed_mate_bases, 7);
    }

    /// Mirrors `fuzz/fuzz_targets/alignment_file.rs`: the seed is an uncompressed BAM payload
    /// which the harness BGZF-frames and indexes. If this stops decoding a record, the fuzz
    /// target has stopped reaching record decode.
    #[test]
    fn alignment_fuzz_seed_reaches_record_decode_once_framed_and_indexed() {
        use std::io::Write as _;

        let seed = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/seeds/alignment_file/one-record.bam-payload");
        if !seed.exists() {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("framed.bam");
        // Without an index the open is refused before any record is read.
        let mut writer = rust_htslib::bgzf::Writer::from_path(&path).unwrap();
        writer.write_all(&fs::read(seed).unwrap()).unwrap();
        drop(writer);
        assert!(LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).is_err());

        bam::index::build(&path, None, bam::index::Type::Bai, 1).unwrap();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let query = source
            .query(
                GenomicInterval::new(ContigId::new(0), 0, 1_000).unwrap(),
                QueryOptions {
                    detail: QueryDetail::Full,
                    collect_alleles: true,
                    collect_modifications: true,
                    ..QueryOptions::default()
                },
                &|| true,
            )
            .unwrap();
        assert_eq!(query.batch.len(), 1);
        let record = query.batch.materialize_record(0).unwrap();
        assert_eq!((record.start, record.end), (100, 108));
        assert_eq!(
            record.sequence,
            SequenceData::Present {
                bases: b"ACGTACGT".to_vec(),
                qualities: Some(vec![30; 8]),
            }
        );
        // `C+m,0;` with ML 200: the first C of ACGTACGT, read index 1, reference 101.
        let calls = crate::decode_modified_bases(&record).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            (calls[0].sequencer_position, calls[0].probability),
            (1, Some(200))
        );
        assert_eq!(query.pileup.modifications.provenance.decoded_calls, 1);
    }

    #[test]
    fn open_local_source_rejects_an_index_changed_during_the_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("changed-index.bam");
        write_placed_unmapped_fixture(&path);
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        assert!(source.source_stamp().is_some());

        let index_path = PathBuf::from(format!("{}.bai", path.display()));
        let mut bytes = fs::read(&index_path).unwrap();
        bytes.push(0);
        fs::write(index_path, bytes).unwrap();

        let result = source.query(
            GenomicInterval::new(ContigId::new(0), 0, 200).unwrap(),
            QueryOptions::default(),
            &|| true,
        );
        assert!(matches!(result, Err(AlignmentError::SourceChanged)));
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
    fn layout_detail_retains_only_row_policy_payload() {
        use rust_htslib::bam::record::{Cigar, CigarString};

        let mut record = bam::Record::new();
        record.set(
            b"layout-policy",
            Some(&CigarString(vec![Cigar::Equal(4)])),
            b"ACGT",
            &[30; 4],
        );
        record.set_tid(0);
        record.set_pos(10);
        record.push_aux(b"RG", Aux::String("group-a")).unwrap();
        record.push_aux(b"ZZ", Aux::String("unrelated")).unwrap();

        let layout = normalize_record(&record, QueryDetail::Layout, &[*b"RG"]).unwrap();
        assert_eq!(layout.sequence, SequenceData::NotLoaded);
        assert_eq!(layout.auxiliary_fields.len(), 1);
        assert_eq!(layout.auxiliary_fields[0].tag, *b"RG");

        let sequence = normalize_record(&record, QueryDetail::Sequence, &[*b"RG"]).unwrap();
        assert!(matches!(sequence.sequence, SequenceData::Present { .. }));
        assert_eq!(sequence.auxiliary_fields.len(), 1);

        let full = normalize_record(&record, QueryDetail::Full, &[]).unwrap();
        assert!(matches!(full.sequence, SequenceData::Present { .. }));
        assert_eq!(full.auxiliary_fields.len(), 2);

        let tagged_options = QueryOptions {
            retained_auxiliary_tags: vec![*b"RG"],
            ..QueryOptions::default()
        };
        assert_ne!(
            QueryOptions::default().fingerprint(),
            tagged_options.fingerprint()
        );
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
    fn cancellation_after_refusal_never_exposes_provisional_reads_or_complete_aggregates() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let mut source =
            LocalAlignmentSource::open(path, &AlignmentOpenOptions::default()).unwrap();
        let cancellation_checks = AtomicUsize::new(0);
        let query = source
            .query(
                GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap(),
                QueryOptions {
                    max_records: NonZeroUsize::new(1).unwrap(),
                    cancellation_chunk: NonZeroUsize::new(1).unwrap(),
                    ..QueryOptions::default()
                },
                &|| cancellation_checks.fetch_add(1, Ordering::SeqCst) < 3,
            )
            .unwrap();

        assert!(query.stats.cancelled);
        assert_eq!(query.stats.records_eligible, 2);
        assert_eq!(query.stats.records_emitted, 0);
        assert_eq!(
            query.stats.individual_reads,
            IndividualReadStatus::TooDense { limit: 1 }
        );
        assert!(query.batch.is_empty());
        assert!(!query.pileup.provenance.all_eligible_records);
        assert!(!query.junctions.provenance.all_eligible_records);
        assert!(!query.pairs.all_eligible_records);
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
    fn contained_projection_matches_an_independent_indexed_query() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let broad = GenomicInterval::new(ContigId::new(0), 9_950, 10_050).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            collect_modifications: true,
            ..QueryOptions::default()
        };
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let resident = source.query(broad, options.clone(), &|| true).unwrap();
        let index = AlignmentOverlapIndex::new(&resident.batch);
        let derived =
            derive_alignment_query(&resident, &index, target, &options, None, &|| true).unwrap();
        let exact = source.query(target, options, &|| true).unwrap();

        assert_eq!(derived.stats, exact.stats);
        assert_eq!(derived.batch, exact.batch);
        assert_eq!(derived.pileup, exact.pileup);
        assert_eq!(derived.junctions, exact.junctions);
        assert_eq!(derived.pairs, exact.pairs);
    }

    #[test]
    fn local_bam_positions_are_stable_across_independent_query_boundaries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let broad = GenomicInterval::new(ContigId::new(0), 9_950, 10_050).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let options = QueryOptions {
            max_records: NonZeroUsize::new(1_000).unwrap(),
            ..QueryOptions::default()
        };
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let resident = source.query(broad, options.clone(), &|| true).unwrap();
        let exact = source.query(target, options, &|| true).unwrap();
        let resident_positions = (0..resident.batch.len())
            .map(|index| resident.batch.source_position(index).unwrap())
            .collect::<std::collections::HashSet<_>>();
        let exact_positions = (0..exact.batch.len())
            .map(|index| exact.batch.source_position(index).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            exact_positions.len(),
            exact_positions
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
        assert!(
            exact_positions
                .iter()
                .all(|position| resident_positions.contains(position))
        );
    }

    #[test]
    fn positioned_stream_is_bounded_by_physical_owner_and_matches_fresh_query() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("positioned-stream.bam");
        write_positioned_stream_fixture(&path, 4_000);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(10_000).unwrap(),
            ..QueryOptions::default()
        };
        let mut exact_source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let exact = exact_source
            .query(interval, options.clone(), &|| true)
            .unwrap();
        let mut stream_source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let (blocks, completion) = collect_positioned_stream(&mut stream_source, interval, options);

        assert!(blocks.len() > 1, "fixture must cross multiple BGZF owners");
        assert_eq!(completion.stats, exact.stats);
        let streamed = blocks
            .iter()
            .flat_map(|block| {
                block.records.iter().map(|record| {
                    let crate::AlignmentSourcePosition::BamVirtualOffset(offset) =
                        record.source_position
                    else {
                        panic!("a local BAM stream returned a CRAM source position")
                    };
                    assert_eq!(offset >> 16, block.compressed_offset);
                    (record.source_position, record.record.clone())
                })
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(streamed.len(), exact.batch.len());
        for index in 0..exact.batch.len() {
            let position = exact.batch.source_position(index).unwrap();
            assert_eq!(
                streamed.get(&position),
                Some(&exact.batch.materialize_record(index).unwrap())
            );
        }
    }

    #[test]
    fn positioned_stream_applies_complete_limit_to_exact_admission_interval_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("positioned-admission.bam");
        write_positioned_stream_fixture(&path, 4_000);
        let query = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
        let admission = GenomicInterval::new(ContigId::new(0), 19_000, 19_001).unwrap();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        source
            .begin_positioned_stream_with_admission(
                query,
                admission,
                QueryOptions {
                    detail: QueryDetail::Full,
                    max_records: NonZeroUsize::new(1).unwrap(),
                    ..QueryOptions::default()
                },
            )
            .unwrap();
        let mut emitted = 0_usize;
        let completion = loop {
            match source.next_positioned_stream_checkpoint(&|| true).unwrap() {
                PositionedAlignmentStreamCheckpoint::Block(block) => {
                    emitted = emitted.saturating_add(block.records.len());
                }
                PositionedAlignmentStreamCheckpoint::Complete(completion) => break completion,
                PositionedAlignmentStreamCheckpoint::Cancelled(_) => {
                    panic!("an uncancelled positioned stream was cancelled")
                }
            }
        };

        assert!(completion.stats.records_eligible > 1);
        assert_eq!(completion.stats.complete_read_records_eligible, 0);
        assert_eq!(
            completion.stats.individual_reads,
            IndividualReadStatus::Complete
        );
        assert_eq!(completion.stats.records_emitted, emitted);
        assert_eq!(
            completion.stats.records_emitted,
            completion.stats.records_eligible
        );
    }

    #[test]
    fn positioned_stream_cancellation_never_reports_complete_coverage() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("positioned-cancel.bam");
        write_positioned_stream_fixture(&path, 1_000);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 10_000).unwrap();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        source
            .begin_positioned_stream(
                interval,
                QueryOptions {
                    cancellation_chunk: NonZeroUsize::new(1).unwrap(),
                    ..QueryOptions::default()
                },
            )
            .unwrap();

        loop {
            match source.next_positioned_stream_checkpoint(&|| false).unwrap() {
                PositionedAlignmentStreamCheckpoint::Block(_) => {}
                PositionedAlignmentStreamCheckpoint::Cancelled(completion) => {
                    assert!(completion.stats.cancelled);
                    assert_eq!(completion.interval, interval);
                    break;
                }
                PositionedAlignmentStreamCheckpoint::Complete(_) => {
                    panic!("cancelled positioned work certified complete coverage")
                }
            }
        }
    }

    #[test]
    fn beginning_a_new_positioned_stream_discards_only_unfinished_transaction_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("positioned-retarget.bam");
        write_positioned_stream_fixture(&path, 4_000);
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(10_000).unwrap(),
            ..QueryOptions::default()
        };
        let first = GenomicInterval::new(ContigId::new(0), 0, 5_000).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 8_000, 12_500).unwrap();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        source
            .begin_positioned_stream(first, options.clone())
            .unwrap();
        assert!(matches!(
            source.next_positioned_stream_checkpoint(&|| true).unwrap(),
            PositionedAlignmentStreamCheckpoint::Block(_)
        ));
        let (blocks, completion) = collect_positioned_stream(&mut source, target, options.clone());
        let mut exact_source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let exact = exact_source.query(target, options, &|| true).unwrap();

        assert_eq!(completion.stats, exact.stats);
        let positions = blocks
            .iter()
            .flat_map(|block| block.records.iter().map(|record| record.source_position))
            .collect::<std::collections::HashSet<_>>();
        let exact_positions = (0..exact.batch.len())
            .map(|index| exact.batch.source_position(index).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(positions, exact_positions);
    }

    #[test]
    fn positioned_stream_stops_retaining_records_after_explicit_complete_limit_refusal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("positioned-refusal.bam");
        write_positioned_stream_fixture(&path, 4_000);
        let interval = GenomicInterval::new(ContigId::new(0), 0, 20_000).unwrap();
        let limit = 10;
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let (blocks, completion) = collect_positioned_stream(
            &mut source,
            interval,
            QueryOptions {
                detail: QueryDetail::Full,
                max_records: NonZeroUsize::new(limit).unwrap(),
                ..QueryOptions::default()
            },
        );
        let retained = blocks
            .iter()
            .map(|block| block.records.len())
            .sum::<usize>();

        assert_eq!(retained, limit);
        assert!(completion.stats.records_eligible > limit);
        assert_eq!(completion.stats.records_emitted, 0);
        assert_eq!(
            completion.stats.individual_reads,
            IndividualReadStatus::TooDense { limit }
        );
    }

    #[test]
    fn resident_summary_index_matches_an_independent_regional_query() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let broad = GenomicInterval::new(ContigId::new(0), 9_950, 10_050).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: false,
            collect_modifications: true,
            ..QueryOptions::default()
        };
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let resident = source.query(broad, options.clone(), &|| true).unwrap();
        let overlap = AlignmentOverlapIndex::new(&resident.batch);
        let summaries = crate::AlignmentSummaryIndex::build(
            &resident.batch,
            resident.interval,
            options.overlapping_mate_policy,
        )
        .unwrap();
        let indexed = derive_alignment_query_with_summary_index_and_phase(
            &resident,
            &overlap,
            Some(&summaries),
            target,
            &options,
            None,
            &|| true,
            &mut |_| {},
        )
        .unwrap()
        .0;
        let exact = source.query(target, options, &|| true).unwrap();

        assert_eq!(indexed, exact);
    }

    #[test]
    fn contained_projection_refuses_reads_over_the_target_limit() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmark-data/variants.bam");
        if !path.exists() {
            return;
        }
        let broad = GenomicInterval::new(ContigId::new(0), 9_950, 10_050).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 9_999, 10_001).unwrap();
        let resident_options = QueryOptions {
            detail: QueryDetail::Full,
            max_records: NonZeroUsize::new(1_000).unwrap(),
            coverage_bins: NonZeroUsize::new(2).unwrap(),
            collect_alleles: true,
            ..QueryOptions::default()
        };
        let target_options = QueryOptions {
            max_records: NonZeroUsize::new(1).unwrap(),
            ..resident_options.clone()
        };
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let resident = source.query(broad, resident_options, &|| true).unwrap();
        let derived = derive_alignment_query(
            &resident,
            &AlignmentOverlapIndex::new(&resident.batch),
            target,
            &target_options,
            None,
            &|| true,
        )
        .unwrap();
        let exact = source.query(target, target_options, &|| true).unwrap();

        assert_eq!(derived.stats, exact.stats);
        assert!(matches!(
            derived.stats.individual_reads,
            IndividualReadStatus::TooDense { limit: 1 }
        ));
        assert!(derived.batch.is_empty());
        assert_eq!(derived.pileup, exact.pileup);
        assert_eq!(derived.junctions, exact.junctions);
        assert_eq!(derived.pairs, exact.pairs);
    }

    #[test]
    fn contained_projection_rebuilds_filtered_record_counts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("placed-unmapped.bam");
        write_placed_unmapped_fixture(&path);
        let options = QueryOptions::default();
        let mut source =
            LocalAlignmentSource::open(&path, &AlignmentOpenOptions::default()).unwrap();
        let resident = source
            .query(
                GenomicInterval::new(ContigId::new(0), 90, 150).unwrap(),
                options.clone(),
                &|| true,
            )
            .unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 108, 135).unwrap();
        let derived = derive_alignment_query(
            &resident,
            &AlignmentOverlapIndex::new(&resident.batch),
            target,
            &options,
            None,
            &|| true,
        )
        .unwrap();
        let exact = source.query(target, options, &|| true).unwrap();

        assert_eq!(derived.stats, exact.stats);
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
    fn pair_summary_remains_complete_when_individual_reads_are_refused() {
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
        let refused = query(1);
        let complete = query(50_000);
        assert_eq!(refused.batch.len(), 0);
        assert_eq!(refused.stats.records_emitted, 0);
        assert!(matches!(
            refused.stats.individual_reads,
            IndividualReadStatus::TooDense { limit: 1 }
        ));
        assert_eq!(refused.pairs, complete.pairs);
        assert!(refused.pairs.eligible_proper_templates >= 100);
        assert_eq!(refused.pairs.median_template_length, Some(166));
        assert_eq!(
            refused.pairs.inferred_bounds,
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
    fn overlapping_mate_policy_collapses_the_right_end_before_read_admission() {
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
    fn modification_summary_remains_complete_when_individual_reads_are_refused() {
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
        let refused = query(1);
        let complete = query(50_000);
        assert!(refused.stats.truncated);
        assert!(refused.batch.is_empty());
        assert_eq!(refused.pileup.modifications, complete.pileup.modifications);
        assert_eq!(
            refused.pileup.modifications.provenance.eligible_records,
            refused.stats.records_eligible
        );
    }

    #[test]
    fn junction_summary_remains_complete_when_individual_reads_are_refused() {
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
        let refused = query(1);
        let complete = query(50_000);
        assert!(refused.stats.truncated);
        assert!(refused.batch.is_empty());
        assert_eq!(refused.junctions, complete.junctions);
        assert_eq!(refused.junctions.junctions.len(), 10_000);
        assert_eq!(refused.junctions.provenance.spliced_records, 10_000);
        assert_eq!(
            refused.junctions.provenance.supporting_spliced_records,
            10_000
        );
        assert_eq!(
            refused.junctions.provenance.junction_observations_retained,
            10_000
        );
        assert_eq!(
            refused.junctions.provenance.normalization_mapped_records,
            Some(282_017)
        );
        assert_eq!(refused.junctions.provenance.xs_tagged_observations, 5_000);
        assert_eq!(
            refused
                .junctions
                .provenance
                .alignment_strand_fallback_observations,
            2_500
        );
        assert_eq!(refused.junctions.provenance.invalid_xs_observations, 2_500);
        assert_eq!(
            refused
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.positive_depth)
                .sum::<u32>(),
            5_000
        );
        assert_eq!(
            refused
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.negative_depth)
                .sum::<u32>(),
            2_500
        );
        assert_eq!(
            refused
                .junctions
                .junctions
                .iter()
                .map(|junction| junction.unknown_depth)
                .sum::<u32>(),
            2_500
        );
        assert!(refused.junctions.provenance.all_eligible_records);

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
    fn variant_truth_pileup_is_exact_when_individual_reads_are_refused() {
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

        let refused = query(1);
        let complete = query(1_000);
        assert_eq!(refused.stats.records_eligible, 13);
        assert_eq!(refused.stats.records_emitted, 0);
        assert!(refused.stats.truncated);
        assert!(refused.batch.is_empty());
        assert_eq!(refused.pileup.bins, complete.pileup.bins);
        assert_eq!(refused.pileup.provenance.eligible_records, 13);
        assert_eq!(refused.pileup.provenance.rendered_records, 0);
        assert!(refused.pileup.provenance.all_eligible_records);

        let (_, insertion_anchor) = refused.pileup.bin_at(interval, 9_999).unwrap();
        assert_eq!(insertion_anchor.alleles.insertion, 4);
        let (_, variant) = refused.pileup.bin_at(interval, 10_000).unwrap();
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
    fn complete_read_limit_is_all_or_nothing_at_the_exact_boundary() {
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
                        max_records: NonZeroUsize::new(maximum_records).unwrap(),
                        coverage_bins: NonZeroUsize::new(2).unwrap(),
                        collect_alleles: true,
                        ..QueryOptions::default()
                    },
                    &|| true,
                )
                .unwrap()
        };

        let exact = query(13);
        assert_eq!(exact.stats.records_eligible, 13);
        assert_eq!(exact.stats.records_emitted, 13);
        assert_eq!(exact.stats.complete_read_limit, 13);
        assert_eq!(exact.stats.individual_reads, IndividualReadStatus::Complete);
        assert_eq!(exact.batch.len(), 13);
        assert!(!exact.stats.truncated);

        let over = query(12);
        assert_eq!(over.stats.records_eligible, 13);
        assert_eq!(over.stats.records_emitted, 0);
        assert_eq!(over.stats.complete_read_limit, 12);
        assert_eq!(
            over.stats.individual_reads,
            IndividualReadStatus::TooDense { limit: 12 }
        );
        assert!(over.batch.is_empty());
        assert!(over.stats.truncated);
        assert_eq!(over.pileup.bins, exact.pileup.bins);
        assert!(over.pileup.provenance.all_eligible_records);
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
