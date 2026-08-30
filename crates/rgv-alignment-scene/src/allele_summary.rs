use std::{mem::size_of, sync::Arc};

use rgv_hts::{
    AlignmentMateOverlapCollapser, CigarOpKind, PileupBin, PileupBuilder, SequenceView,
    add_unmasked_range, aligned_observation, bisulfite_strand_is_reverse,
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

impl ExactAlleleSummaryTile {
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    pub fn build(
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
        let selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
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
            let overlap_mask = collapser.mask_for(
                record.read_name(),
                record.contig(),
                record.start(),
                record.end(),
                flags,
                record.mate_contig(),
                record.mate_start(),
            );
            let mut reference_position = record.start();
            let mut read_position = 0_usize;
            let mut collapsed = 0_u64;
            for operation in record.cigar() {
                match operation.kind {
                    CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch => {
                        let end = reference_position.saturating_add(u64::from(operation.len));
                        for offset in 0..operation.len as usize {
                            let position = reference_position.saturating_add(offset as u64);
                            if !interval.contains(position)
                                || overlap_mask.is_some_and(|mask| mask.contains(position))
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
                            collapsed = collapsed.saturating_add(add_unmasked_range(
                                clipped_start,
                                clipped_end,
                                overlap_mask,
                                |_, _| {},
                            ));
                        }
                        read_position = read_position.saturating_add(operation.len as usize);
                        reference_position = end;
                    }
                    CigarOpKind::Deletion => {
                        let end = reference_position.saturating_add(u64::from(operation.len));
                        let clipped_start = reference_position.max(interval.start());
                        let clipped_end = end.min(interval.end());
                        if clipped_start < clipped_end {
                            collapsed = collapsed.saturating_add(add_unmasked_range(
                                clipped_start,
                                clipped_end,
                                overlap_mask,
                                |start, end| alleles.add_deletion(start, end, reverse),
                            ));
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
                            && !overlap_mask.is_some_and(|mask| mask.contains(reference_position))
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
