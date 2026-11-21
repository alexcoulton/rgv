use std::{mem::size_of, sync::Arc};

use rgv_hts::{
    AlignmentMateAlignment, AlignmentMateOverlapCollapser, CigarOpKind, PileupBin, PileupBuilder,
    SequenceView, aligned_observation, base_observation_is_counted, bisulfite_strand_is_reverse,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AlleleSummaryContract, AlleleSummaryDetail, AllocationClass,
    AllocationLease, AllocationLedger, CanonicalGenomicTile, CanonicalTileError,
    CapabilityRequirement, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, LedgerError, PinnedAllocationLease, ReservationKind,
    TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactAlleleSummaryKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    allele_contract: ContractFingerprint,
    tile: CanonicalGenomicTile,
}

impl ExactAlleleSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: AlleleSummaryContract,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            allele_contract: contract.fingerprint(),
            tile,
        }
    }
}

/// Independently owned exact per-base allele and indel evidence for one canonical tile.
#[derive(Debug)]
pub struct ExactAlleleSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    record_contract: ContractFingerprint,
    contract: AlleleSummaryContract,
    tile: CanonicalGenomicTile,
    interval_start: u64,
    sites: Vec<PileupBin>,
    eligible_records: usize,
    collapsed_mate_bases: u64,
    allocation: AllocationLease,
}

/// How a summary builder walks each overlapping read's CIGAR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CigarWalk {
    /// Visit only what can reach the tile and stop once past it.
    EnterAtTile,
    /// Visit every operation and base of the read. The former behaviour, kept as an oracle.
    #[cfg_attr(not(test), allow(dead_code))]
    Whole,
}

impl ExactAlleleSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: AlleleSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactAlleleSummaryError> {
        Self::build_with_selection(
            None,
            CigarWalk::EnterAtTile,
            evidence,
            tile,
            contig_length,
            contract,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    /// The former builder, which visited every aligned base of every overlapping read whether
    /// or not it lay in the tile. Kept as the oracle for the tile-clipped walk.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_walking_whole_cigars(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: AlleleSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactAlleleSummaryError> {
        Self::build_with_selection(
            None,
            CigarWalk::Whole,
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
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub(crate) fn build_with_selection(
        shared: Option<&crate::EvidenceSelection>,
        walk: CigarWalk,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: AlleleSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactAlleleSummaryError> {
        if contract.detail() == AlleleSummaryDetail::None {
            return Err(ExactAlleleSummaryError::NotRequested);
        }
        if evidence.payload().sequence() != CapabilityRequirement::Required {
            return Err(ExactAlleleSummaryError::MissingSequenceCapability);
        }
        let qualities_required = contract.minimum_base_quality() > 0
            || contract.detail() == AlleleSummaryDetail::QualityWeighted;
        if qualities_required && evidence.payload().qualities() != CapabilityRequirement::Required {
            return Err(ExactAlleleSummaryError::MissingQualityCapability);
        }
        let interval = tile.interval(contig_length)?;
        let bins = usize::try_from(interval.len())
            .map_err(|_| ExactAlleleSummaryError::TileTooWide(interval.len()))?;
        let owned_selection;
        let selection = if let Some(shared) =
            shared.filter(|shared| shared.is_over(evidence, interval))
        {
            shared
        } else {
            owned_selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
            &owned_selection
        };
        let maximum_bytes = size_of::<Self>().saturating_add(
            bins.saturating_mul(2)
                .saturating_mul(size_of::<PileupBin>()),
        );
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            maximum_bytes,
            reservation_kind,
        )?;
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            maximum_bytes.saturating_add(selection.len().saturating_mul(64)),
            reservation_kind,
        )?;
        let mut alleles = PileupBuilder::new(interval, bins, contract.minimum_base_quality());
        let mut collapser = AlignmentMateOverlapCollapser::new(contract.overlapping_mate_policy());

        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            let sequence = selection
                .sequence(index)
                .ok_or(ExactAlleleSummaryError::MissingSequenceCapability)?;
            let (bases, qualities) = match sequence {
                SequenceView::Present { bases, qualities } => (bases, qualities.unwrap_or(&[])),
                SequenceView::Missing => (&[][..], &[][..]),
                SequenceView::NotLoaded => {
                    return Err(ExactAlleleSummaryError::MissingSequenceCapability);
                }
            };
            if qualities_required && !bases.is_empty() && qualities.len() != bases.len() {
                return Err(ExactAlleleSummaryError::MissingQualityCapability);
            }
            let flags = record.flags();
            let reverse = flags.is_reverse();
            let bisulfite_reverse = bisulfite_strand_is_reverse(flags);
            // The mask follows the earlier mate's own CIGAR and base-quality eligibility, exactly
            // as the direct rgv-hts pileup query does: a deletion, `N` skip or rejected base in
            // the earlier mate must not hide the later mate's evidence.
            let minimum_base_quality = contract.minimum_base_quality();
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
                |read_position| {
                    base_observation_is_counted(
                        bases.len(),
                        qualities,
                        read_position,
                        minimum_base_quality,
                    )
                },
            );
            let overlap_mask = overlap_mask.as_ref();
            let mut reference_position = record.start();
            let mut read_position = 0_usize;
            let mut collapsed = 0_u64;
            for operation in record.cigar() {
                // An insertion at the tile's end is still owned by the tile's last base, so
                // only an operation starting beyond the end proves the rest is irrelevant.
                if walk == CigarWalk::EnterAtTile && reference_position > interval.end() {
                    break;
                }
                match operation.kind {
                    CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch => {
                        let end = reference_position.saturating_add(u64::from(operation.len));
                        // Only the bases inside the tile can be counted here; a long read
                        // crossing many tiles must not be re-read base by base for each.
                        let offsets = match walk {
                            CigarWalk::Whole => 0..operation.len as usize,
                            CigarWalk::EnterAtTile => {
                                let first = interval.start().saturating_sub(reference_position);
                                let last =
                                    interval.end().min(end).saturating_sub(reference_position);
                                usize::try_from(first).unwrap_or(usize::MAX)
                                    ..usize::try_from(last).unwrap_or(usize::MAX)
                            }
                        };
                        for offset in offsets {
                            let position = reference_position.saturating_add(offset as u64);
                            if !interval.contains(position)
                                || overlap_mask.is_some_and(|mask| mask.masks_base(position))
                            {
                                continue;
                            }
                            let sequence_index = read_position.saturating_add(offset);
                            let Some((base, quality)) =
                                aligned_observation(bases, qualities, sequence_index)
                            else {
                                continue;
                            };
                            alleles.add_base_with_bisulfite_strand(
                                position,
                                base,
                                quality,
                                reverse,
                                bisulfite_reverse,
                            );
                        }
                        let clipped_start = reference_position.max(interval.start());
                        let clipped_end = end.min(interval.end());
                        if clipped_start < clipped_end {
                            collapsed = collapsed.saturating_add(overlap_mask.map_or(0, |mask| {
                                mask.add_unmasked_depth_range(clipped_start, clipped_end, |_, _| {})
                            }));
                        }
                        read_position = read_position.saturating_add(operation.len as usize);
                        reference_position = end;
                    }
                    CigarOpKind::Deletion => {
                        let end = reference_position.saturating_add(u64::from(operation.len));
                        let clipped_start = reference_position.max(interval.start());
                        let clipped_end = end.min(interval.end());
                        if clipped_start < clipped_end {
                            if let Some(mask) = overlap_mask {
                                collapsed =
                                    collapsed.saturating_add(mask.add_unmasked_deletion_range(
                                        clipped_start,
                                        clipped_end,
                                        |start, end| alleles.add_deletion(start, end, reverse),
                                    ));
                            } else {
                                alleles.add_deletion(clipped_start, clipped_end, reverse);
                            }
                        }
                        reference_position = end;
                    }
                    CigarOpKind::ReferenceSkip => {
                        reference_position =
                            reference_position.saturating_add(u64::from(operation.len));
                    }
                    CigarOpKind::Insertion => {
                        // Pileup insertion evidence is globally owned by the preceding reference
                        // base (position zero owns a leading insertion at zero). Test ownership
                        // against that canonical anchor, not the right-hand CIGAR cursor. Calling
                        // `add_insertion` from both adjacent tiles would otherwise either omit the
                        // event from the left tile or duplicate it through the builder's local
                        // boundary fallback.
                        let insertion_anchor = reference_position.saturating_sub(1);
                        if interval.contains(insertion_anchor)
                            && !overlap_mask
                                .is_some_and(|mask| mask.masks_indel(reference_position))
                        {
                            alleles.add_insertion(reference_position, reverse);
                        }
                        read_position = read_position.saturating_add(operation.len as usize);
                    }
                    CigarOpKind::SoftClip => {
                        read_position = read_position.saturating_add(operation.len as usize);
                    }
                    CigarOpKind::HardClip | CigarOpKind::Padding => {}
                }
            }
            collapser.note_collapsed(collapsed);
        }
        if !collapser.complete() {
            return Err(ExactAlleleSummaryError::MateOverlapCapacityExceeded);
        }
        let sites = alleles.finish_alleles();
        let retained_bytes = size_of::<Self>()
            .saturating_add(sites.capacity().saturating_mul(size_of::<PileupBin>()));
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            record_contract: evidence.record_contract(),
            contract,
            tile,
            interval_start: interval.start(),
            sites,
            eligible_records: selection.len(),
            collapsed_mate_bases: collapser.collapsed_mate_bases(),
            allocation,
        }))
    }

    #[must_use]
    pub fn key(&self) -> ExactAlleleSummaryKey {
        ExactAlleleSummaryKey::new(
            self.source_revision,
            self.record_contract,
            self.contract,
            self.tile,
        )
    }

    #[must_use]
    pub const fn evidence_revision(&self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn contract(&self) -> AlleleSummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub fn sites(&self) -> &[PileupBin] {
        &self.sites
    }

    #[must_use]
    pub fn site_at(&self, position: u64) -> Option<PileupBin> {
        let index = usize::try_from(position.checked_sub(self.interval_start)?).ok()?;
        self.sites.get(index).copied()
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
        size_of::<Self>()
            .saturating_add(self.sites.capacity().saturating_mul(size_of::<PileupBin>()))
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactAlleleSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("allele detail is not requested by this contract")]
    NotRequested,
    #[error("exact allele summary requires a complete sequence payload")]
    MissingSequenceCapability,
    #[error("exact quality-aware allele summary requires complete quality payload")]
    MissingQualityCapability,
    #[error("canonical allele tile width {0} cannot be represented on this platform")]
    TileTooWide(u64),
    #[error("exact count-once alleles exceeded the bounded mate-overlap working set")]
    MateOverlapCapacityExceeded,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use rgv_core::{ContigId, GenomicInterval, OverlappingMatePolicy};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, CigarOp, SequenceData,
    };

    use super::*;
    use crate::{
        CanonicalRecordChunk, CompleteCoverage, EvidenceAcquisition, EvidenceChunk,
        EvidenceRootBuilder, FilteredRecordProvenance, PayloadContract, PayloadSidecar,
        RevisionFingerprint, SourceChunkId, SourceFingerprint,
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
        bases: &[u8],
        qualities: &[u8],
        flags: u16,
        mate_start: Option<u64>,
    ) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end: start.saturating_add(bases.len() as u64),
            flags: AlignmentFlags::from_bits(flags),
            mapping_quality: Some(60),
            mate_contig: mate_start.map(|_| ContigId::new(0)),
            mate_start,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: u32::try_from(bases.len()).unwrap(),
            }],
            sequence: SequenceData::Present {
                bases: bases.to_vec(),
                qualities: Some(qualities.to_vec()),
            },
            auxiliary_fields: Vec::new(),
        }
    }

    fn evidence(records: &[AlignmentRecord], ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let mut decoded = AlignmentBatch::default();
        for (ordinal, record) in records.iter().enumerate() {
            decoded
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (9 << 16) | u64::try_from(ordinal + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let payload =
            Arc::new(AlignmentPayloadBatch::from_batch(&decoded, true, true, Some(&[])).unwrap());
        let core = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 9,
            },
            Arc::new(decoded.copy_core().unwrap()),
            ContractFingerprint::new(7),
        )
        .unwrap();
        let payload_contract = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [],
            None,
        );
        let payload_bytes = payload.estimated_bytes();
        let mut payload_reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let payload_allocation = payload_reservation.register(payload_bytes, 1).unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let mut core_reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core_allocation = core_reservation.register(footprint.core_bytes, 1).unwrap();
        let mut overlap_reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = overlap_reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let acquisition = EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(7),
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
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
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(5, 6)),
            source_revision(),
            ContractFingerprint::new(7),
            payload_contract,
            1,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(2).unwrap()
    }

    fn tile() -> CanonicalGenomicTile {
        CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap())
    }

    #[test]
    fn exact_sites_apply_base_quality_and_are_charged_by_capacity() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let root = evidence(
            &[record(b"alleles", 10, b"ACGT", &[30, 10, 40, 40], 0, None)],
            &ledger,
        );
        let summary = ExactAlleleSummaryTile::build(
            &root,
            tile(),
            100,
            AlleleSummaryContract::new(
                OverlappingMatePolicy::CountBoth,
                20,
                AlleleSummaryDetail::QualityWeighted,
            ),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();

        assert_eq!(summary.site_at(10).unwrap().alleles.a, 1);
        assert_eq!(summary.site_at(11).unwrap().alleles.c, 0);
        assert_eq!(summary.site_at(12).unwrap().alleles.g, 1);
        assert_eq!(
            summary.allocation().snapshot().unwrap().charged_capacity,
            summary.estimated_bytes()
        );
    }

    #[test]
    fn count_once_removes_overlapping_mate_alleles() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let first = record(b"pair", 10, b"AAAA", &[30; 4], 0x1 | 0x40, Some(12));
        let second = record(b"pair", 12, b"AAAA", &[30; 4], 0x1 | 0x80 | 0x10, Some(10));
        let root = evidence(&[first, second], &ledger);
        let once = ExactAlleleSummaryTile::build(
            &root,
            tile(),
            100,
            AlleleSummaryContract::new(
                OverlappingMatePolicy::CountOnce,
                0,
                AlleleSummaryDetail::Counts,
            ),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let both = ExactAlleleSummaryTile::build(
            &root,
            tile(),
            100,
            AlleleSummaryContract::new(
                OverlappingMatePolicy::CountBoth,
                0,
                AlleleSummaryDetail::Counts,
            ),
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        assert_eq!(once.site_at(12).unwrap().alleles.a, 1);
        assert_eq!(both.site_at(12).unwrap().alleles.a, 2);
        assert_eq!(once.collapsed_mate_bases(), 2);
    }

    /// Earlier mate `10: 4M2D4M` (bases at 10..14 and 16..20, deletion over 14..16) whose
    /// read index 4 (reference 16) fails base quality; later mate `12: 10M` (12..22).
    fn overlapping_mates_with_a_deletion_and_a_low_quality_base() -> [AlignmentRecord; 2] {
        let op = |kind, len| CigarOp { kind, len };
        let mut earlier = record(
            b"pair",
            10,
            b"AAAAAAAA",
            &[30, 30, 30, 30, 5, 30, 30, 30],
            0x1 | 0x20 | 0x40,
            Some(12),
        );
        earlier.cigar = vec![
            op(CigarOpKind::Match, 4),
            op(CigarOpKind::Deletion, 2),
            op(CigarOpKind::Match, 4),
        ];
        earlier.end = 20;
        let later = record(
            b"pair",
            12,
            b"CCCCCCCCCC",
            &[30; 10],
            0x1 | 0x10 | 0x80,
            Some(10),
        );
        [earlier, later]
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn count_once_follows_the_earlier_mates_deletion_and_base_quality() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let records = overlapping_mates_with_a_deletion_and_a_low_quality_base();
        let root = evidence(&records, &ledger);
        let alleles = ExactAlleleSummaryTile::build(
            &root,
            tile(),
            100,
            AlleleSummaryContract::new(
                OverlappingMatePolicy::CountOnce,
                10,
                AlleleSummaryDetail::Counts,
            ),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let depth = crate::ExactDepthSummaryTile::build(
            &root,
            tile(),
            100,
            crate::DepthSummaryContract::new(OverlappingMatePolicy::CountOnce),
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        // (position, A, C, deletion, depth), computed by hand:
        //  10,11  only the earlier mate.
        //  12,13  both aligned: the later mate's C and depth collapse.
        //  14,15  the earlier mate's deletion is the template's observation, so the later
        //         mate's C is not an allele; but only the later mate has an aligned base, so
        //         it supplies the depth (the legacy single-range mask reported depth 0 here).
        //  16     the earlier A fails base quality, so the later C is counted; depth collapses.
        //  17..20 both aligned: collapsed onto the earlier mate.
        //  20,21  only the later mate.
        let expected = [
            (10, 1, 0, 0, 1),
            (11, 1, 0, 0, 1),
            (12, 1, 0, 0, 1),
            (13, 1, 0, 0, 1),
            (14, 0, 0, 1, 1),
            (15, 0, 0, 1, 1),
            (16, 0, 1, 0, 1),
            (17, 1, 0, 0, 1),
            (18, 1, 0, 0, 1),
            (19, 1, 0, 0, 1),
            (20, 0, 1, 0, 1),
            (21, 0, 1, 0, 1),
        ];
        let mut scene_depth = vec![0_u32; 100];
        for run in depth.depth().runs() {
            for position in run.start..run.end {
                scene_depth[usize::try_from(position).unwrap()] += run.depth;
            }
        }
        for (position, a, c, deletion, expected_depth) in expected {
            let site = alleles.site_at(position).unwrap().alleles;
            assert_eq!(
                (site.a, site.c, site.deletion),
                (a, c, deletion),
                "alleles at {position}"
            );
            assert_eq!(site.base_observations() + site.deletion, 1, "{position}");
            assert_eq!(
                scene_depth[usize::try_from(position).unwrap()],
                expected_depth,
                "depth at {position}"
            );
        }
        assert_eq!(scene_depth.iter().sum::<u32>(), 12);
        // Aligned in both mates: 12, 13 and 16..20.
        assert_eq!(depth.collapsed_mate_bases(), 6);
        assert_eq!(alleles.collapsed_mate_bases(), 6);

        // The direct rgv-hts query over the same reads must report exactly the same thing.
        let mut batch = AlignmentBatch::default();
        for record in &records {
            batch.push(record).unwrap();
        }
        let whole = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let resident = rgv_hts::AlignmentQuery {
            interval: whole,
            header: Arc::new(rgv_hts::AlignmentHeaderMetadata::default()),
            batch,
            pileup: rgv_hts::PileupSummary::default(),
            junctions: rgv_hts::JunctionSummary::default(),
            pairs: rgv_hts::PairSummary::default(),
            reference: None,
            stats: rgv_hts::QueryStats {
                records_seen: 2,
                records_eligible: 2,
                records_emitted: 2,
                complete_read_limit: 100,
                ..rgv_hts::QueryStats::default()
            },
        };
        let options = rgv_hts::QueryOptions {
            detail: rgv_hts::QueryDetail::Full,
            max_records: std::num::NonZeroUsize::new(100).unwrap(),
            coverage_bins: std::num::NonZeroUsize::new(100).unwrap(),
            collect_alleles: true,
            overlapping_mate_policy: OverlappingMatePolicy::CountOnce,
            minimum_base_quality: 10,
            ..rgv_hts::QueryOptions::default()
        };
        let direct = rgv_hts::derive_alignment_query(
            &resident,
            &rgv_hts::AlignmentOverlapIndex::new(&resident.batch),
            whole,
            &options,
            None,
            &|| true,
        )
        .unwrap();
        assert_eq!(direct.pileup.len(), 100);
        for position in 0..100_u64 {
            let (_, bin) = direct.pileup.bin_at(whole, position).unwrap();
            let site = alleles.site_at(position).unwrap();
            assert_eq!(site.alleles, bin.alleles, "direct alleles at {position}");
            assert_eq!(site.forward_alleles, bin.forward_alleles, "{position}");
            assert_eq!(site.reverse_alleles, bin.reverse_alleles, "{position}");
            assert_eq!(
                scene_depth[usize::try_from(position).unwrap()],
                bin.depth,
                "direct depth at {position}"
            );
        }
        assert_eq!(
            direct.pileup.provenance.collapsed_mate_bases,
            alleles.collapsed_mate_bases()
        );
    }

    #[test]
    fn insertion_is_owned_once_by_its_left_anchor_across_tile_boundaries() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let insertion = AlignmentRecord {
            contig: ContigId::new(0),
            start: 5,
            end: 15,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"boundary-insertion".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
                CigarOp {
                    kind: CigarOpKind::Insertion,
                    len: 2,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 5,
                },
            ],
            sequence: SequenceData::Present {
                bases: b"ACGTACGTACGT".to_vec(),
                qualities: Some(vec![30; 12]),
            },
            auxiliary_fields: Vec::new(),
        };
        let root = evidence(&[insertion], &ledger);
        let contract = AlleleSummaryContract::new(
            OverlappingMatePolicy::CountBoth,
            0,
            AlleleSummaryDetail::Counts,
        );
        let left = ExactAlleleSummaryTile::build(
            &root,
            CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(10).unwrap()),
            100,
            contract,
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let right = ExactAlleleSummaryTile::build(
            &root,
            CanonicalGenomicTile::new(ContigId::new(0), 1, NonZeroU64::new(10).unwrap()),
            100,
            contract,
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        assert_eq!(left.site_at(9).unwrap().alleles.insertion, 1);
        assert!(right.sites().iter().all(|site| site.alleles.insertion == 0));
    }
}
