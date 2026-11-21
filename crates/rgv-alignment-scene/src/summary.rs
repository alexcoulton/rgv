use std::{mem::size_of, sync::Arc};

use rgv_hts::{
    AlignmentMateAlignment, AlignmentMateOverlapCollapser, CigarOpKind, ExactDepthRun,
    ExactDepthTile, ExactDepthTileBuilder,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    CanonicalGenomicTile, CanonicalTileError, ContractFingerprint, DepthSummaryContract,
    EvidenceRevision, EvidenceRoot, EvidenceSelectionError, LedgerError, PinnedAllocationLease,
    ReservationKind, TransientResourceClass,
};

/// Compatibility key for an exact depth tile.
///
/// Evidence publication revision is deliberately absent: a complete tile for the same immutable
/// source revision and record contract has identical records even when a wider evidence root is
/// published later.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactDepthSummaryKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    depth_contract: ContractFingerprint,
    tile: CanonicalGenomicTile,
}

impl ExactDepthSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: DepthSummaryContract,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            depth_contract: contract.fingerprint(),
            tile,
        }
    }
}

/// Independently owned exact aligned-base depth for one canonical genomic tile.
///
/// This allocation contains no evidence-root references and no camera state. It can therefore
/// survive record-geometry eviction and be projected into any display binning without source I/O.
#[derive(Debug)]
pub struct ExactDepthSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    record_contract: ContractFingerprint,
    contract: DepthSummaryContract,
    tile: CanonicalGenomicTile,
    depth: ExactDepthTile,
    eligible_records: usize,
    collapsed_mate_bases: u64,
    allocation: AllocationLease,
}

impl ExactDepthSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: DepthSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactDepthSummaryError> {
        let interval = tile.interval(contig_length)?;
        let selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
        // Only aligned operations inside this tile can open or close a depth run. Counting a
        // long read's whole CIGAR charged every tile it touched for the entire read.
        let aligned_operations = (0..selection.len()).fold(0_usize, |total, index| {
            total.saturating_add(
                selection
                    .record_view(index)
                    .map_or(0, |record| aligned_operations_in_tile(record, interval)),
            )
        });
        let maximum_bytes = maximum_retained_bytes(aligned_operations, interval.len());
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            maximum_bytes,
            reservation_kind,
        )?;
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            maximum_bytes
                .saturating_mul(2)
                .saturating_add(aligned_operations.saturating_mul(size_of::<(u64, u64)>())),
            reservation_kind,
        )?;

        let mut depth_builder = ExactDepthTileBuilder::new(interval);
        let mut collapser = AlignmentMateOverlapCollapser::new(contract.overlapping_mate_policy());
        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            let flags = record.flags();
            let reverse = flags.is_reverse();
            // Depth collapses only where the earlier mate has an aligned base; its deletions and
            // `N` skips leave the later mate's bases counted, as in the direct rgv-hts query.
            let overlap_mask = collapser.mask_for_alignment(
                AlignmentMateAlignment {
                    read_name: record.read_name(),
                    contig: record.contig(),
                    start: record.start(),
                    end: record.end(),
                    flags,
                    mate_contig: record.mate_contig(),
                    mate_start: record.mate_start(),
                },
                record.cigar().iter().copied(),
                |_| true,
            );
            let mut reference_position = record.start();
            let mut aligned_segments = Vec::<(u64, u64)>::new();
            for operation in record.cigar() {
                if reference_position >= interval.end() {
                    // Nothing further along this read can reach the tile.
                    break;
                }
                let end = reference_position.saturating_add(u64::from(operation.len));
                match operation.kind {
                    CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch => {
                        if end <= interval.start() {
                            // Entirely left of the tile: contributes no depth here, so it is
                            // neither retained nor charged.
                        } else if let Some((_, merged_end)) = aligned_segments.last_mut()
                            && reference_position <= *merged_end
                        {
                            *merged_end = (*merged_end).max(end);
                        } else {
                            aligned_segments.push((reference_position, end));
                        }
                        reference_position = end;
                    }
                    CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => {
                        reference_position = end;
                    }
                    CigarOpKind::Insertion
                    | CigarOpKind::SoftClip
                    | CigarOpKind::HardClip
                    | CigarOpKind::Padding => {}
                }
            }
            let mut collapsed = 0_u64;
            for (start, end) in aligned_segments {
                let start = start.max(interval.start());
                let end = end.min(interval.end());
                if start >= end {
                    continue;
                }
                let mut add = |start, end| {
                    depth_builder.add_aligned_segment(
                        start,
                        end,
                        reverse,
                        record.mapping_quality(),
                    );
                };
                if let Some(mask) = &overlap_mask {
                    collapsed =
                        collapsed.saturating_add(mask.add_unmasked_depth_range(start, end, add));
                } else {
                    add(start, end);
                }
            }
            collapser.note_collapsed(collapsed);
        }
        if !collapser.complete() {
            return Err(ExactDepthSummaryError::MateOverlapCapacityExceeded);
        }

        let depth = depth_builder.finish();
        let retained_bytes = size_of::<Self>().saturating_add(depth.owned_heap_bytes());
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        debug_assert_eq!(
            allocation.snapshot()?.charged_capacity,
            retained_bytes,
            "summary allocation must be charged exactly by retained capacity"
        );
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            record_contract: evidence.record_contract(),
            contract,
            tile,
            depth,
            eligible_records: selection.len(),
            collapsed_mate_bases: collapser.collapsed_mate_bases(),
            allocation,
        }))
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn evidence_revision(&self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub fn key(&self) -> ExactDepthSummaryKey {
        ExactDepthSummaryKey::new(
            self.source_revision,
            self.record_contract,
            self.contract,
            self.tile,
        )
    }

    #[must_use]
    pub const fn contract(&self) -> DepthSummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn depth(&self) -> &ExactDepthTile {
        &self.depth
    }

    #[must_use]
    pub const fn eligible_records(&self) -> usize {
        self.eligible_records
    }

    #[must_use]
    pub const fn collapsed_mate_bases(&self) -> u64 {
        self.collapsed_mate_bases
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    pub fn pin(&self) -> Result<PinnedAllocationLease, LedgerError> {
        self.allocation.pin()
    }

    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        size_of::<Self>().saturating_add(self.depth.owned_heap_bytes())
    }
}

/// Aligned (`M`/`=`/`X`) operations of `record` with at least one base inside `tile`.
fn aligned_operations_in_tile(
    record: rgv_hts::AlignmentRecordView<'_>,
    tile: rgv_core::GenomicInterval,
) -> usize {
    let mut count = 0_usize;
    let mut reference_position = record.start();
    for operation in record.cigar() {
        if reference_position >= tile.end() {
            break;
        }
        let end = reference_position.saturating_add(u64::from(operation.len));
        match operation.kind {
            CigarOpKind::Match | CigarOpKind::SequenceMatch | CigarOpKind::SequenceMismatch => {
                if end > tile.start() && reference_position < end {
                    count = count.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::Deletion | CigarOpKind::ReferenceSkip => reference_position = end,
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    count
}

/// Each aligned operation in the tile adds at most two unmasked pieces, hence four run
/// boundaries; a tile can never hold more runs than it has bases.
fn maximum_retained_bytes(aligned_operations: usize, tile_bases: u64) -> usize {
    let maximum_runs = aligned_operations
        .saturating_mul(4)
        .min(usize::try_from(tile_bases).unwrap_or(usize::MAX));
    let run_capacity = maximum_runs
        .checked_next_power_of_two()
        .unwrap_or(usize::MAX);
    let maximum_tree_capacity = if maximum_runs == 0 {
        0
    } else {
        run_capacity.saturating_mul(2)
    };
    size_of::<ExactDepthSummaryTile>()
        .saturating_add(run_capacity.saturating_mul(size_of::<ExactDepthRun>()))
        .saturating_add(maximum_tree_capacity.saturating_mul(size_of::<u32>()))
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactDepthSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("exact count-once depth exceeded the bounded mate-overlap working set")]
    MateOverlapCapacityExceeded,
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU64, NonZeroUsize};

    use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentRecord, AlignmentSourcePosition, CigarOp,
        SequenceData,
    };

    use super::*;
    use crate::{
        AllocationClass, CanonicalRecordChunk, CapabilityRequirement, CompleteCoverage,
        ContractFingerprint, EvidenceAcquisition, EvidenceChunk, EvidenceRootBuilder,
        FilteredRecordProvenance, PayloadContract, RevisionFingerprint, SourceChunkId,
        SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn record(
        name: &[u8],
        start: u64,
        cigar: &[(CigarOpKind, u32)],
        flags: u16,
        mate_start: Option<u64>,
    ) -> AlignmentRecord {
        let end = cigar.iter().fold(start, |position, (kind, length)| {
            if matches!(
                kind,
                CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch
                    | CigarOpKind::Deletion
                    | CigarOpKind::ReferenceSkip
            ) {
                position.saturating_add(u64::from(*length))
            } else {
                position
            }
        });
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: mate_start.map(|_| ContigId::new(0)),
            mate_start,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: cigar
                .iter()
                .map(|(kind, len)| CigarOp {
                    kind: *kind,
                    len: *len,
                })
                .collect(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    fn evidence(records: &[AlignmentRecord], ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let mut batch = AlignmentBatch::default();
        for (ordinal, record) in records.iter().enumerate() {
            batch
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (7 << 16) | u64::try_from(ordinal + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let chunk = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(batch),
            ContractFingerprint::new(9),
        )
        .unwrap();
        let footprint = EvidenceChunk::footprint(&chunk);
        let mut core = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core = core.register(footprint.core_bytes, 1).unwrap();
        let mut overlap = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap = overlap.register(footprint.overlap_index_bytes, 1).unwrap();
        let chunk = Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap());
        let coverage = GenomicInterval::new(ContigId::new(0), 0, 400).unwrap();
        let acquisition = EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(9),
                coverage,
                FilteredRecordProvenance {
                    records_seen: records.len() as u64,
                    eligible_records: records.len() as u64,
                    ..FilteredRecordProvenance::default()
                },
            )
            .unwrap(),
            [chunk],
        )
        .unwrap();
        let payload = PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [],
            None,
        );
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(5, 6)),
            source_revision(),
            ContractFingerprint::new(9),
            payload,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(2).unwrap()
    }

    fn tile(coordinate: u64) -> CanonicalGenomicTile {
        CanonicalGenomicTile::new(ContigId::new(0), coordinate, NonZeroU64::new(100).unwrap())
    }

    #[test]
    fn long_and_spliced_records_are_clipped_without_filling_reference_skips() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(
            &[
                record(b"long", 50, &[(CigarOpKind::Match, 200)], 0, None),
                record(
                    b"spliced",
                    70,
                    &[
                        (CigarOpKind::Match, 20),
                        (CigarOpKind::ReferenceSkip, 120),
                        (CigarOpKind::Match, 20),
                    ],
                    0x10,
                    None,
                ),
            ],
            &ledger,
        );
        let summary = ExactDepthSummaryTile::build(
            &root,
            tile(1),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();

        assert_eq!(
            summary.depth().maximum_depth(summary.depth().interval()),
            Some(1)
        );
        assert_eq!(
            summary
                .depth()
                .maximum_depth(GenomicInterval::new(ContigId::new(0), 100, 190).unwrap()),
            Some(1)
        );
        assert_eq!(summary.eligible_records(), 2);
    }

    #[test]
    fn depth_reservation_counts_only_aligned_operations_inside_the_tile() {
        let ledger = AllocationLedger::new(4 << 20, 0);
        // Forty 4-base exons, ten bases apart: [0,4), [10,14), ... [390,394).
        let mut cigar = Vec::new();
        for exon in 0..40 {
            if exon > 0 {
                cigar.push((CigarOpKind::ReferenceSkip, 6));
            }
            cigar.push((CigarOpKind::Match, 2));
            cigar.push((CigarOpKind::Insertion, 1));
            cigar.push((CigarOpKind::SequenceMatch, 2));
        }
        let root = evidence(&[record(b"exons", 0, &cigar, 0, None)], &ledger);
        let whole = GenomicInterval::new(ContigId::new(0), 0, 400).unwrap();
        let selection = root.select(whole).unwrap();
        let view = selection.record_view(0).unwrap();
        assert_eq!(aligned_operations_in_tile(view, whole), 80);
        assert_eq!(
            aligned_operations_in_tile(
                view,
                GenomicInterval::new(ContigId::new(0), 100, 150).unwrap()
            ),
            10
        );
        // A tile wholly inside one intron reserves for nothing.
        assert_eq!(
            aligned_operations_in_tile(view, GenomicInterval::new(ContigId::new(0), 5, 9).unwrap()),
            0
        );

        for tile_bases in [1_u64, 3, 7, 50, 64, 400] {
            for coordinate in 0..400_u64.div_ceil(tile_bases) {
                let summary = ExactDepthSummaryTile::build(
                    &root,
                    CanonicalGenomicTile::new(
                        ContigId::new(0),
                        coordinate,
                        NonZeroU64::new(tile_bases).unwrap(),
                    ),
                    400,
                    DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
                    &ledger,
                    ReservationKind::Normal,
                    3,
                )
                .unwrap();
                let interval = summary.depth().interval();
                // Reservation >= actual, with the reservation taken from this tile alone.
                let reserved = maximum_retained_bytes(
                    aligned_operations_in_tile(view, interval),
                    interval.len(),
                );
                assert!(summary.estimated_bytes() <= reserved);
                assert!(reserved <= maximum_retained_bytes(80, 400));
                // Clipping the walk to the tile must not change a single base of depth.
                for position in interval.start()..interval.end() {
                    let depth = summary
                        .depth()
                        .runs()
                        .iter()
                        .find(|run| run.start <= position && position < run.end)
                        .map_or(0, |run| run.depth);
                    let expected = u32::from(position < 394 && position % 10 < 4);
                    assert_eq!(depth, expected, "position {position}, tile {tile_bases}");
                }
            }
        }
    }

    #[test]
    fn adjacent_tiles_share_no_depth_storage_and_join_exactly() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(
            &[record(
                b"boundary",
                50,
                &[(CigarOpKind::Match, 200)],
                0,
                None,
            )],
            &ledger,
        );
        let left = ExactDepthSummaryTile::build(
            &root,
            tile(0),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let right = ExactDepthSummaryTile::build(
            &root,
            tile(1),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        assert_eq!(left.depth().runs()[0].end, 100);
        assert_eq!(right.depth().runs()[0].start, 100);
        assert_ne!(left.allocation().id(), right.allocation().id());
    }

    #[test]
    fn count_once_collapses_only_the_mate_overlap() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let first_flags = 0x1 | 0x40;
        let second_flags = 0x1 | 0x80 | 0x10;
        let root = evidence(
            &[
                record(
                    b"pair",
                    100,
                    &[(CigarOpKind::Match, 80)],
                    first_flags,
                    Some(140),
                ),
                record(
                    b"pair",
                    140,
                    &[(CigarOpKind::Match, 80)],
                    second_flags,
                    Some(100),
                ),
            ],
            &ledger,
        );
        let once = ExactDepthSummaryTile::build(
            &root,
            tile(1),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountOnce),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let both = ExactDepthSummaryTile::build(
            &root,
            tile(1),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();
        let overlap = GenomicInterval::new(ContigId::new(0), 140, 180).unwrap();

        assert_eq!(once.depth().maximum_depth(overlap), Some(1));
        assert_eq!(both.depth().maximum_depth(overlap), Some(2));
        assert_eq!(once.collapsed_mate_bases(), 40);
        assert_eq!(both.collapsed_mate_bases(), 0);
    }

    #[test]
    fn summary_is_independently_charged_and_survives_evidence_release() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(
            &[record(
                b"independent",
                10,
                &[(CigarOpKind::Match, 50)],
                0,
                None,
            )],
            &ledger,
        );
        let summary = ExactDepthSummaryTile::build(
            &root,
            tile(0),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let summary_id = summary.allocation().id();
        assert_eq!(
            summary.allocation().snapshot().unwrap().charged_capacity,
            summary.estimated_bytes()
        );
        drop(root);

        let snapshot = ledger.snapshot().unwrap();
        assert!(snapshot.records.iter().any(|record| {
            record.id == summary_id && record.class == AllocationClass::ExactSummary
        }));
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_resources.iter().any(|usage| {
            usage.class == TransientResourceClass::ExactSummaryBuilder
                && usage.current == 0
                && usage.high_water > 0
        }));
        assert_eq!(summary.depth().runs()[0].depth, 1);
    }

    #[test]
    fn uncertified_tile_cannot_publish_a_summary() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(&[], &ledger);
        let outside = CanonicalGenomicTile::new(ContigId::new(0), 4, NonZeroU64::new(100).unwrap());
        let error = ExactDepthSummaryTile::build(
            &root,
            outside,
            1_000,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ExactDepthSummaryError::Evidence(EvidenceSelectionError::UncertifiedInterval(_))
        ));
    }

    #[test]
    fn impossible_budget_leaves_no_summary_allocation() {
        let evidence_ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(
            &[record(b"budget", 10, &[(CigarOpKind::Match, 50)], 0, None)],
            &evidence_ledger,
        );
        let summary_ledger = AllocationLedger::new(NonZeroUsize::new(1).unwrap().get(), 0);
        let error = ExactDepthSummaryTile::build(
            &root,
            tile(0),
            400,
            DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
            &summary_ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ExactDepthSummaryError::Ledger(LedgerError::BudgetExceeded { .. })
        ));
        assert_eq!(summary_ledger.snapshot().unwrap().charged, 0);
        assert_eq!(summary_ledger.snapshot().unwrap().reserved, 0);
    }
}
