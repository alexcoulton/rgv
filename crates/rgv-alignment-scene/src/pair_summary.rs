use std::{mem::size_of, sync::Arc};

use rgv_hts::{PairSummary, PairSummaryBuilder, template_id};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    CanonicalGenomicTile, CanonicalTileError, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, LedgerError, PairSummaryContract, PinnedAllocationLease,
    ReservationKind, TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactPairSummaryKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    pair_contract: ContractFingerprint,
    tile: CanonicalGenomicTile,
}

impl ExactPairSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: PairSummaryContract,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            pair_contract: contract.fingerprint(),
            tile,
        }
    }
}

/// One deterministic proper-template candidate owned by its first record's alignment start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairCandidateEvent {
    pub handle: AlignmentHandle,
    pub record_start: u64,
    pub record_end: u64,
    pub score: u64,
    pub template_length: u64,
}

/// Canonically owned pair-summary candidates, composable without reparsing source records.
#[derive(Debug)]
pub struct ExactPairSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    record_contract: ContractFingerprint,
    contract: PairSummaryContract,
    tile: CanonicalGenomicTile,
    candidates: Vec<PairCandidateEvent>,
    summary: PairSummary,
    maximum_record_end: Option<u64>,
    allocation: AllocationLease,
}

impl ExactPairSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: PairSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactPairSummaryError> {
        let interval = tile.interval(contig_length)?;
        let selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
        let maximum_capacity = selection
            .len()
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX);
        let maximum_bytes = size_of::<Self>()
            .saturating_add(maximum_capacity.saturating_mul(size_of::<PairCandidateEvent>()));
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            maximum_bytes,
            reservation_kind,
        )?;
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            maximum_bytes.saturating_mul(3),
            reservation_kind,
        )?;
        let mut candidates = Vec::new();
        for (index, selected) in selection.records().iter().enumerate() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            let flags = record.flags();
            if !interval.contains(record.start())
                || record.read_name().is_empty()
                || !flags.is_paired()
                || !flags.is_proper_pair()
                || flags.is_unmapped()
                || flags.is_mate_unmapped()
                || flags.is_secondary()
                || flags.is_supplementary()
                || !flags.is_first_in_template()
                || record.mate_contig() != Some(record.contig())
                || record.template_length() == 0
            {
                continue;
            }
            candidates.push(PairCandidateEvent {
                handle: selected.handle,
                record_start: record.start(),
                record_end: record.end(),
                score: template_id(record.read_name()).get(),
                template_length: record.template_length().unsigned_abs(),
            });
        }
        candidates.sort_unstable_by_key(|candidate| (candidate.score, candidate.handle));
        candidates.shrink_to_fit();
        let summary = summarize_pair_candidates(&candidates, true);
        let maximum_record_end = candidates
            .iter()
            .map(|candidate| candidate.record_end)
            .max();
        let retained_bytes = size_of::<Self>().saturating_add(
            candidates
                .capacity()
                .saturating_mul(size_of::<PairCandidateEvent>()),
        );
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            record_contract: evidence.record_contract(),
            contract,
            tile,
            candidates,
            summary,
            maximum_record_end,
            allocation,
        }))
    }

    #[must_use]
    pub fn key(&self) -> ExactPairSummaryKey {
        ExactPairSummaryKey::new(
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
    pub const fn contract(&self) -> PairSummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub fn candidates(&self) -> &[PairCandidateEvent] {
        &self.candidates
    }

    #[must_use]
    pub const fn summary(&self) -> PairSummary {
        self.summary
    }

    #[must_use]
    pub const fn maximum_record_end(&self) -> Option<u64> {
        self.maximum_record_end
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
        size_of::<Self>().saturating_add(
            self.candidates
                .capacity()
                .saturating_mul(size_of::<PairCandidateEvent>()),
        )
    }
}

#[must_use]
pub fn summarize_pair_candidates(
    candidates: &[PairCandidateEvent],
    all_eligible_records: bool,
) -> PairSummary {
    summarize_pair_candidate_iter(candidates.iter().copied(), all_eligible_records)
}

pub(crate) fn summarize_pair_candidate_iter(
    candidates: impl IntoIterator<Item = PairCandidateEvent>,
    all_eligible_records: bool,
) -> PairSummary {
    let mut builder = PairSummaryBuilder::new();
    let mut count = 0_usize;
    for candidate in candidates {
        count = count.saturating_add(1);
        builder.add_precomputed_sample(candidate.score, candidate.template_length);
    }
    builder.add_precomputed_eligible_count(count);
    builder.finish(all_eligible_records)
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ExactPairSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use rgv_core::{ContigId, GenomicInterval};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentRecord, AlignmentSourcePosition, SequenceData,
    };

    use super::*;
    use crate::{
        AllocationClass, CanonicalRecordChunk, CapabilityRequirement, CompleteCoverage,
        EvidenceAcquisition, EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance,
        PayloadContract, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn record(name: &[u8], start: u64, length: i64) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end: start + 25,
            flags: AlignmentFlags::from_bits(0x1 | 0x2 | 0x40),
            mapping_quality: Some(60),
            mate_contig: Some(ContigId::new(0)),
            mate_start: Some(start + 100),
            template_length: length,
            read_name: name.to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    fn evidence(ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let records = [
            record(b"left", 80, 200),
            record(b"right", 120, 300),
            record(b"duplicate", 120, 400),
        ];
        let mut batch = AlignmentBatch::default();
        for (index, record) in records.iter().enumerate() {
            batch
                .push_with_source_position(
                    record,
                    AlignmentSourcePosition::bam_virtual_offset(
                        (12 << 16) | u64::try_from(index + 1).unwrap(),
                    ),
                )
                .unwrap();
        }
        let chunk = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 12,
            },
            Arc::new(batch),
            ContractFingerprint::new(7),
        )
        .unwrap();
        let footprint = EvidenceChunk::footprint(&chunk);
        let mut reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core = reservation.register(footprint.core_bytes, 1).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap = reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap());
        let acquisition = EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(7),
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                FilteredRecordProvenance {
                    records_seen: 3,
                    eligible_records: 3,
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
            PayloadContract::new(
                CapabilityRequirement::NotRequired,
                CapabilityRequirement::NotRequired,
                [],
                None,
            ),
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
    fn pair_candidates_are_owned_by_start_and_remain_composable() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(&ledger);
        let left = ExactPairSummaryTile::build(
            &root,
            tile(0),
            300,
            PairSummaryContract,
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let right = ExactPairSummaryTile::build(
            &root,
            tile(1),
            300,
            PairSummaryContract,
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        assert_eq!(left.candidates().len(), 1);
        assert_eq!(right.candidates().len(), 2);
        assert_eq!(left.summary().median_template_length, Some(200));
        assert_eq!(right.summary().median_template_length, Some(300));
        assert_ne!(right.candidates()[0].handle, right.candidates()[1].handle);
        let combined = left
            .candidates()
            .iter()
            .chain(right.candidates())
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(
            summarize_pair_candidates(&combined, true).median_template_length,
            Some(300)
        );
        assert_eq!(
            right.allocation().snapshot().unwrap().charged_capacity,
            right.estimated_bytes()
        );
    }
}
