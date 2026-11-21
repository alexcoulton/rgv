use std::{mem::size_of, sync::Arc};

use rgv_hts::{
    AuxField, AuxValue, CigarOpKind, JunctionEvidence, JunctionStrand, JunctionStrandInput,
    JunctionSummary, JunctionSummaryBuilder, RecordJunctionCollector,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    CanonicalGenomicTile, CanonicalTileError, ContractFingerprint, EvidenceRevision, EvidenceRoot,
    EvidenceSelectionError, JunctionSummaryContract, LedgerError, PinnedAllocationLease,
    ReservationKind, TransientResourceClass,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactJunctionSummaryKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    junction_contract: ContractFingerprint,
    normalization_mapped_records: Option<u64>,
    tile: CanonicalGenomicTile,
}

impl ExactJunctionSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: JunctionSummaryContract,
        normalization_mapped_records: Option<u64>,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            junction_contract: contract.fingerprint(),
            normalization_mapped_records,
            tile,
        }
    }
}

/// Exact splice-junction evidence owned by junction start coordinate.
#[derive(Debug)]
pub struct ExactJunctionSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    record_contract: ContractFingerprint,
    contract: JunctionSummaryContract,
    normalization_mapped_records: Option<u64>,
    tile: CanonicalGenomicTile,
    summary: JunctionSummary,
    /// Junctions which start left of this tile and reach into it. Every read supporting one
    /// spans the tile's first base, so the counts are exact from this tile's own selection.
    /// They are owned by an earlier tile whenever that tile is prepared; a summary root reads
    /// them only from its first tile, where no owner exists.
    entering: Vec<JunctionEvidence>,
    maximum_end: Option<u64>,
    allocation: AllocationLease,
}

impl ExactJunctionSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: JunctionSummaryContract,
        normalization_mapped_records: Option<u64>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactJunctionSummaryError> {
        Self::build_with_selection(
            None,
            crate::allele_summary::CigarWalk::EnterAtTile,
            evidence,
            tile,
            contig_length,
            contract,
            normalization_mapped_records,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    /// The former builder, which collected every junction of every overlapping read before
    /// discarding those the tile does not retain. Kept as the oracle for the gated walk.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_walking_whole_cigars(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: JunctionSummaryContract,
        normalization_mapped_records: Option<u64>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactJunctionSummaryError> {
        Self::build_with_selection(
            None,
            crate::allele_summary::CigarWalk::Whole,
            evidence,
            tile,
            contig_length,
            contract,
            normalization_mapped_records,
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
        walk: crate::allele_summary::CigarWalk,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: JunctionSummaryContract,
        normalization_mapped_records: Option<u64>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactJunctionSummaryError> {
        if !evidence.payload().all_auxiliary_tags()
            && evidence
                .payload()
                .auxiliary_tags()
                .binary_search(b"XS")
                .is_err()
        {
            return Err(ExactJunctionSummaryError::MissingXsCapability);
        }
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
        // Only skips this tile can retain are counted: those it owns (start inside the tile)
        // and those entering across its left edge.
        let (maximum_owned, maximum_entering) =
            (0..selection.len()).fold((0_usize, 0_usize), |(owned, entering), index| {
                let (record_owned, record_entering) = selection
                    .record_view(index)
                    .map_or((0, 0), |record| retained_skip_counts(record, interval));
                (
                    owned.saturating_add(record_owned),
                    entering.saturating_add(record_entering),
                )
            });
        let junction_capacity = maximum_owned
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX)
            .saturating_add(
                maximum_entering
                    .checked_next_power_of_two()
                    .unwrap_or(usize::MAX),
            );
        let maximum_bytes = size_of::<Self>()
            .saturating_add(junction_capacity.saturating_mul(size_of::<JunctionEvidence>()));
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
        let mut builder = JunctionSummaryBuilder::exact(
            interval,
            contract.minimum_flank(),
            normalization_mapped_records,
        );
        let mut entering_builder = JunctionSummaryBuilder::exact(
            interval,
            contract.minimum_flank(),
            normalization_mapped_records,
        );
        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            // Flank lengths need the read's whole CIGAR, but only for a read with a skip this
            // tile retains. The count stops at the tile's end, so an unspliced read (or one
            // spliced only elsewhere) costs a walk to the tile and no collection at all.
            if walk == crate::allele_summary::CigarWalk::EnterAtTile
                && retained_skip_counts(record, interval) == (0, 0)
            {
                continue;
            }
            let mut reference_position = record.start();
            let mut collector = RecordJunctionCollector::exact();
            for operation in record.cigar() {
                match operation.kind {
                    CigarOpKind::Match
                    | CigarOpKind::SequenceMatch
                    | CigarOpKind::SequenceMismatch => {
                        collector.add_aligned(operation.len);
                        reference_position =
                            reference_position.saturating_add(u64::from(operation.len));
                    }
                    CigarOpKind::Deletion => {
                        reference_position =
                            reference_position.saturating_add(u64::from(operation.len));
                    }
                    CigarOpKind::ReferenceSkip => {
                        let end = reference_position.saturating_add(u64::from(operation.len));
                        collector.add_skip(reference_position, end);
                        reference_position = end;
                    }
                    CigarOpKind::Insertion
                    | CigarOpKind::SoftClip
                    | CigarOpKind::HardClip
                    | CigarOpKind::Padding => {}
                }
            }
            let (mut observations, dropped) = collector.finish();
            debug_assert_eq!(dropped, 0);
            let entering_observations = observations
                .iter()
                .filter(|observation| {
                    observation.start < interval.start() && observation.end > interval.start()
                })
                .copied()
                .collect::<Vec<_>>();
            observations.retain(|observation| interval.contains(observation.start));
            if observations.is_empty() && entering_observations.is_empty() {
                continue;
            }
            let strand = junction_strand(selection.auxiliary_field(index, *b"XS")?);
            if !observations.is_empty() {
                builder.add_record_junctions(
                    &observations,
                    dropped,
                    record.flags().is_reverse(),
                    strand,
                );
            }
            if !entering_observations.is_empty() {
                entering_builder.add_record_junctions(
                    &entering_observations,
                    0,
                    record.flags().is_reverse(),
                    strand,
                );
            }
        }
        let mut summary = builder.finish(selection.len(), true);
        let entering_summary = entering_builder.finish(selection.len(), true);
        if summary.provenance.dropped_record_junction_observations > 0
            || summary.provenance.dropped_distinct_junctions > 0
            || entering_summary
                .provenance
                .dropped_record_junction_observations
                > 0
            || entering_summary.provenance.dropped_distinct_junctions > 0
        {
            return Err(ExactJunctionSummaryError::IncompleteAggregation);
        }
        summary.junctions.shrink_to_fit();
        let mut entering = entering_summary.junctions;
        entering.shrink_to_fit();
        let maximum_end = summary.junctions.iter().map(|junction| junction.end).max();
        let retained_bytes = size_of::<Self>()
            .saturating_add(summary.estimated_bytes())
            .saturating_add(entering_bytes(&entering));
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            record_contract: evidence.record_contract(),
            contract,
            normalization_mapped_records,
            tile,
            summary,
            entering,
            maximum_end,
            allocation,
        }))
    }

    #[must_use]
    pub fn key(&self) -> ExactJunctionSummaryKey {
        ExactJunctionSummaryKey::new(
            self.source_revision,
            self.record_contract,
            self.contract,
            self.normalization_mapped_records,
            self.tile,
        )
    }

    #[must_use]
    pub const fn evidence_revision(&self) -> EvidenceRevision {
        self.evidence_revision
    }

    #[must_use]
    pub const fn contract(&self) -> JunctionSummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn summary(&self) -> &JunctionSummary {
        &self.summary
    }

    /// Exact evidence for junctions which start before this tile and end after its first base.
    ///
    /// These are not part of [`Self::summary`]: the tile containing a junction's start owns it,
    /// so that summing tiles never counts a junction twice. A consumer may use this list only
    /// for the leftmost tile it has prepared, where the owning tile is absent.
    #[must_use]
    pub fn entering_junctions(&self) -> &[JunctionEvidence] {
        &self.entering
    }

    #[must_use]
    pub const fn maximum_end(&self) -> Option<u64> {
        self.maximum_end
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
            .saturating_add(self.summary.estimated_bytes())
            .saturating_add(entering_bytes(&self.entering))
    }
}

fn entering_bytes(entering: &Vec<JunctionEvidence>) -> usize {
    entering
        .capacity()
        .saturating_mul(size_of::<JunctionEvidence>())
}

/// Reference skips of `record` this tile retains: `(owned, entering)`.
fn retained_skip_counts(
    record: rgv_hts::AlignmentRecordView<'_>,
    tile: rgv_core::GenomicInterval,
) -> (usize, usize) {
    let mut owned = 0_usize;
    let mut entering = 0_usize;
    let mut reference_position = record.start();
    for operation in record.cigar() {
        if reference_position >= tile.end() {
            break;
        }
        match operation.kind {
            CigarOpKind::Match
            | CigarOpKind::SequenceMatch
            | CigarOpKind::SequenceMismatch
            | CigarOpKind::Deletion => {
                reference_position = reference_position.saturating_add(u64::from(operation.len));
            }
            CigarOpKind::ReferenceSkip => {
                let end = reference_position.saturating_add(u64::from(operation.len));
                if tile.contains(reference_position) {
                    owned = owned.saturating_add(1);
                } else if end > tile.start() {
                    entering = entering.saturating_add(1);
                }
                reference_position = end;
            }
            CigarOpKind::Insertion
            | CigarOpKind::SoftClip
            | CigarOpKind::HardClip
            | CigarOpKind::Padding => {}
        }
    }
    (owned, entering)
}

fn junction_strand(field: Option<AuxField>) -> JunctionStrandInput {
    match field.map(|field| field.value) {
        Some(AuxValue::Character(b'+')) => JunctionStrandInput::Tagged(JunctionStrand::Positive),
        Some(AuxValue::Character(b'-')) => JunctionStrandInput::Tagged(JunctionStrand::Negative),
        Some(AuxValue::String(value)) if value == "+" => {
            JunctionStrandInput::Tagged(JunctionStrand::Positive)
        }
        Some(AuxValue::String(value)) if value == "-" => {
            JunctionStrandInput::Tagged(JunctionStrand::Negative)
        }
        Some(_) => JunctionStrandInput::Invalid,
        None => JunctionStrandInput::Missing,
    }
}

#[derive(Debug, Error)]
pub enum ExactJunctionSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    Packed(#[from] rgv_hts::PackedRecordError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("exact junction summary requires the XS auxiliary-tag capability")]
    MissingXsCapability,
    #[error("exact junction aggregation unexpectedly dropped source observations")]
    IncompleteAggregation,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use rgv_core::{ContigId, GenomicInterval};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, CigarOp, SequenceData,
    };

    use super::*;
    use crate::{
        AllocationClass, CanonicalRecordChunk, CapabilityRequirement, EvidenceAcquisition,
        EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance, PayloadContract,
        PayloadSidecar, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn evidence(ledger: &AllocationLedger, retain_xs: bool) -> Arc<EvidenceRoot> {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 80,
            end: 230,
            flags: AlignmentFlags::from_bits(0x10),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"spliced".to_vec(),
            cigar: vec![
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 10,
                },
                CigarOp {
                    kind: CigarOpKind::ReferenceSkip,
                    len: 120,
                },
                CigarOp {
                    kind: CigarOpKind::Match,
                    len: 20,
                },
            ],
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: vec![AuxField {
                tag: *b"XS",
                value: AuxValue::Character(b'+'),
            }],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((11 << 16) | 1),
            )
            .unwrap();
        let xs_tag = [*b"XS"];
        let retained_tags = retain_xs.then_some(xs_tag.as_slice()).unwrap_or(&[]);
        let payload = Arc::new(
            AlignmentPayloadBatch::from_batch(&decoded, false, false, Some(retained_tags)).unwrap(),
        );
        let core = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 11,
            },
            Arc::new(decoded.copy_core().unwrap()),
            ContractFingerprint::new(7),
        )
        .unwrap();
        let payload_contract = PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            retained_tags.iter().copied(),
            None,
        );
        let payload_bytes = payload.estimated_bytes();
        let mut reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                payload_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let payload_allocation = reservation.register(payload_bytes, 1).unwrap();
        let payload = Arc::new(
            PayloadSidecar::new(&core, payload_contract.clone(), payload, payload_allocation)
                .unwrap(),
        );
        let footprint = EvidenceChunk::footprint(&core);
        let mut reservation = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let core_allocation = reservation.register(footprint.core_bytes, 1).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = reservation
            .register(footprint.overlap_index_bytes, 1)
            .unwrap();
        let chunk = Arc::new(
            EvidenceChunk::new_with_payload(core, core_allocation, overlap_allocation, payload)
                .unwrap(),
        );
        let acquisition = EvidenceAcquisition::new(
            crate::CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(7),
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                FilteredRecordProvenance {
                    records_seen: 1,
                    eligible_records: 1,
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

    fn tile(coordinate: u64) -> CanonicalGenomicTile {
        CanonicalGenomicTile::new(ContigId::new(0), coordinate, NonZeroU64::new(100).unwrap())
    }

    #[test]
    fn junction_is_owned_once_by_its_start_tile_with_exact_flanks_and_strand() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(&ledger, true);
        let left = ExactJunctionSummaryTile::build(
            &root,
            tile(0),
            300,
            JunctionSummaryContract::new(5),
            Some(1_000),
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();
        let middle = ExactJunctionSummaryTile::build(
            &root,
            tile(1),
            300,
            JunctionSummaryContract::new(5),
            Some(1_000),
            &ledger,
            ReservationKind::Normal,
            4,
        )
        .unwrap();

        assert_eq!(left.summary().junctions.len(), 1);
        let junction = left.summary().junctions[0];
        assert_eq!((junction.start, junction.end), (90, 210));
        assert_eq!(
            (junction.minimum_left_flank, junction.minimum_right_flank),
            (10, 20)
        );
        assert_eq!(junction.tagged_positive_depth, 1);
        assert!(middle.summary().junctions.is_empty());
        assert_eq!(left.maximum_end(), Some(210));
        assert_eq!(
            left.allocation().snapshot().unwrap().charged_capacity,
            left.estimated_bytes()
        );
    }

    #[test]
    fn junctions_entering_a_tile_are_exact_but_stay_out_of_its_owned_summary() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(&ledger, true);
        let build = |coordinate: u64| {
            ExactJunctionSummaryTile::build(
                &root,
                tile(coordinate),
                300,
                JunctionSummaryContract::new(5),
                Some(1_000),
                &ledger,
                ReservationKind::Normal,
                3 + coordinate,
            )
            .unwrap()
        };
        let owner = build(0);
        assert!(owner.entering_junctions().is_empty());
        assert_eq!(owner.summary().junctions.len(), 1);
        // The junction (90, 210) crosses the left edge of both later tiles. The read supporting
        // it is resident in each, so each can report it exactly without owning it.
        for coordinate in [1, 2] {
            let later = build(coordinate);
            assert!(later.summary().junctions.is_empty());
            assert_eq!(
                later.entering_junctions(),
                owner.summary().junctions.as_slice()
            );
            assert_eq!(
                later.allocation().snapshot().unwrap().charged_capacity,
                later.estimated_bytes()
            );
        }
    }

    #[test]
    fn missing_xs_payload_is_an_explicit_capability_error() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = evidence(&ledger, false);
        let error = ExactJunctionSummaryTile::build(
            &root,
            tile(0),
            300,
            JunctionSummaryContract::new(0),
            None,
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ExactJunctionSummaryError::MissingXsCapability
        ));
    }
}
