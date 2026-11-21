use std::{mem::size_of, sync::Arc};

use rgv_model::{
    AlignmentMateAlignment, AlignmentMateOverlapCollapser, CigarOpKind, ExactDepthRun,
    ExactDepthTile, ExactDepthTileBuilder, add_counted_depth_segments,
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
        Self::build_with_selection(
            None,
            evidence,
            tile,
            contig_length,
            contract,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    /// [`Self::build`] over a selection of the tile already made for another summary family.
    /// One tile's families all read the same complete selection, so the store selects once per
    /// tile. A selection of another root or interval is ignored and the tile selects its own.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_with_selection(
        shared: Option<&crate::EvidenceSelection>,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: DepthSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactDepthSummaryError> {
        let interval = tile.interval(contig_length)?;
        let owned_selection;
        let selection = if let Some(shared) =
            shared.filter(|shared| shared.is_over(evidence, interval))
        {
            shared
        } else {
            owned_selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
            &owned_selection
        };
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
        let mut aligned_segments = Vec::<(u64, u64)>::new();
        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            let flags = record.flags();
            let reverse = flags.is_reverse();
            // The count-once depth rule is the one the streaming coverage pyramid uses.
            add_counted_depth_segments(
                &mut collapser,
                AlignmentMateAlignment {
                    read_name: record.read_name(),
                    contig: record.contig(),
                    start: record.start(),
                    end: record.end(),
                    flags,
                    mate_contig: record.mate_contig(),
                    mate_start: record.mate_start(),
                },
                record.cigar(),
                interval.start(),
                interval.end(),
                &mut aligned_segments,
                |start, end| {
                    depth_builder.add_aligned_segment(
                        start,
                        end,
                        reverse,
                        record.mapping_quality(),
                    );
                },
            );
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
    record: rgv_model::AlignmentRecordView<'_>,
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
    use rgv_model::{
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

    /// The streaming coverage pyramid and the read-derived exact depth summary agree wherever
    /// both exist: a level-0 pyramid tile equals the exact tiles' runs aggregated to its bins.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn the_coverage_pyramid_equals_exact_read_derived_depth_aggregated_to_bins() {
        use rgv_model::{
            AlignmentMateAlignment, CoverageAccumulator, CoveragePyramidParameters, CoverageTile,
        };

        use crate::{DepthPyramidStore, DepthPyramidView};

        const CONTIG_LENGTH: u64 = 400;
        // 4-base bins, 64-base tiles: the 400-base contig has seven tiles, the last partial.
        let parameters = CoveragePyramidParameters::new(4, 4, 16).unwrap();
        let mut state = 0x5eed_u64;
        let mut random = move |bound: u64| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) % bound
        };
        for policy in [
            OverlappingMatePolicy::CountOnce,
            OverlappingMatePolicy::CountBoth,
        ] {
            for round in 0..6 {
                let mut records = Vec::new();
                for template in 0..120 {
                    let name = format!("t{round}-{template}").into_bytes();
                    let cigar = |random: &mut dyn FnMut(u64) -> u64| {
                        let mut cigar = vec![(CigarOpKind::Match, 1 + random(25) as u32)];
                        match random(4) {
                            0 => cigar.push((CigarOpKind::Deletion, 1 + random(6) as u32)),
                            1 => cigar.push((CigarOpKind::ReferenceSkip, 1 + random(40) as u32)),
                            2 => cigar.push((CigarOpKind::Insertion, 2)),
                            _ => return cigar,
                        }
                        cigar.push((CigarOpKind::SequenceMatch, 1 + random(20) as u32));
                        cigar
                    };
                    let start = random(300);
                    let first = cigar(&mut random);
                    if random(3) == 0 {
                        records.push(record(&name, start, &first, 0, None));
                        continue;
                    }
                    let mate_start = start + random(30);
                    let second = cigar(&mut random);
                    records.push(record(&name, start, &first, 0x1 | 0x40, Some(mate_start)));
                    records.push(record(
                        &name,
                        mate_start,
                        &second,
                        0x1 | 0x80 | 0x10,
                        Some(start),
                    ));
                }
                records.sort_by_key(|record| record.start);
                assert!(records.iter().all(|record| record.end <= CONTIG_LENGTH));

                // Read-level authority: exact depth tiles derived from resident evidence.
                let ledger = AllocationLedger::new(64 << 20, 0);
                let root = evidence(&records, &ledger);
                let exact = (0..CONTIG_LENGTH.div_ceil(64))
                    .map(|coordinate| {
                        ExactDepthSummaryTile::build(
                            &root,
                            CanonicalGenomicTile::new(
                                ContigId::new(0),
                                coordinate,
                                NonZeroU64::new(64).unwrap(),
                            ),
                            CONTIG_LENGTH,
                            DepthSummaryContract::new(policy),
                            &ledger,
                            ReservationKind::Normal,
                            3,
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(exact.len(), 7);
                if policy == OverlappingMatePolicy::CountOnce {
                    assert!(exact.iter().any(|tile| tile.collapsed_mate_bases() > 0));
                }

                // The pyramid: the same records streamed once, nothing retained.
                let tiles = parameters.tile_range(0, 0, CONTIG_LENGTH, CONTIG_LENGTH);
                let mut accumulator = CoverageAccumulator::new(
                    parameters,
                    ContigId::new(0),
                    CONTIG_LENGTH,
                    0,
                    tiles,
                    policy,
                )
                .unwrap();
                for record in &records {
                    accumulator
                        .add_record(
                            AlignmentMateAlignment {
                                read_name: &record.read_name,
                                contig: record.contig,
                                start: record.start,
                                end: record.end,
                                flags: record.flags,
                                mate_contig: record.mate_contig,
                                mate_start: record.mate_start,
                            },
                            &record.cigar,
                        )
                        .unwrap();
                }
                let pyramid = accumulator.finish().unwrap();
                assert!(pyramid.overlap_collapse_complete);
                assert_eq!(pyramid.tiles.len(), exact.len());

                let mut store = DepthPyramidStore::new(parameters, ledger.clone(), 8 << 20);
                for (tile, exact_tile) in pyramid.tiles.iter().zip(&exact) {
                    assert_eq!(
                        tile.span(),
                        (
                            exact_tile.depth().interval().start(),
                            exact_tile.depth().interval().end()
                        )
                    );
                    let aggregated = CoverageTile::from_depth_runs(
                        tile.id(),
                        parameters,
                        CONTIG_LENGTH,
                        exact_tile.depth().runs(),
                    )
                    .unwrap();
                    assert_eq!(tile, &aggregated, "{policy:?}, round {round}");
                    store.insert(tile.clone()).unwrap();
                }
                // And through the lane product: any range's maximum equals the exact one.
                let lane = store.lane(&DepthPyramidView {
                    contig: ContigId::new(0),
                    contig_length: CONTIG_LENGTH,
                    start: 0,
                    end: CONTIG_LENGTH,
                    bases_per_pixel: 1.0,
                });
                assert!(lane.is_exact_at_target());
                for exact_tile in &exact {
                    let interval = exact_tile.depth().interval();
                    assert_eq!(
                        Some(lane.maximum_depth(interval.start(), interval.end())),
                        exact_tile.depth().maximum_depth(interval)
                    );
                }
            }
        }
    }
}
