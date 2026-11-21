use std::{mem::size_of, sync::Arc};

use rgv_model::{
    AuxField, AuxValue, MAX_MODIFICATION_SERIES, ModificationBin, ModificationSeries,
    ModificationSummary, ModificationSummaryBuilder, SequenceView,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    CanonicalGenomicTile, CanonicalTileError, CapabilityRequirement, ContractFingerprint,
    EvidenceRevision, EvidenceRoot, EvidenceSelectionError, LedgerError,
    ModificationSummaryContract, PinnedAllocationLease, ReservationKind, TransientResourceClass,
};

/// `MN` is required alongside `MM`/`ML`: it decides whether `MM` still describes the stored
/// sequence, most importantly for hard-clipped supplementary alignments.
const REQUIRED_MODIFICATION_TAGS: [[u8; 2]; 6] = [*b"HP", *b"ML", *b"MM", *b"MN", *b"Ml", *b"Mm"];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ExactModificationSummaryKey {
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    modification_contract: ContractFingerprint,
    tile: CanonicalGenomicTile,
}

impl ExactModificationSummaryKey {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        contract: ModificationSummaryContract,
        tile: CanonicalGenomicTile,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            modification_contract: contract.fingerprint(),
            tile,
        }
    }
}

/// Exact per-base modified-base series for one canonical tile.
#[derive(Debug)]
pub struct ExactModificationSummaryTile {
    source_revision: AlignmentSourceRevision,
    evidence_revision: EvidenceRevision,
    record_contract: ContractFingerprint,
    contract: ModificationSummaryContract,
    tile: CanonicalGenomicTile,
    summary: ModificationSummary,
    allocation: AllocationLease,
}

impl ExactModificationSummaryTile {
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: ModificationSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactModificationSummaryError> {
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
        contract: ModificationSummaryContract,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, ExactModificationSummaryError> {
        if evidence.payload().sequence() != CapabilityRequirement::Required {
            return Err(ExactModificationSummaryError::MissingSequenceCapability);
        }
        if !evidence.payload().all_auxiliary_tags()
            && !REQUIRED_MODIFICATION_TAGS.iter().all(|tag| {
                evidence
                    .payload()
                    .auxiliary_tags()
                    .binary_search(tag)
                    .is_ok()
            })
        {
            return Err(ExactModificationSummaryError::MissingAuxiliaryCapability);
        }
        let interval = tile.interval(contig_length)?;
        let bins = usize::try_from(interval.len())
            .map_err(|_| ExactModificationSummaryError::TileTooWide(interval.len()))?;
        let owned_selection;
        let selection = if let Some(shared) =
            shared.filter(|shared| shared.is_over(evidence, interval))
        {
            shared
        } else {
            owned_selection = evidence.select_with_reservation_kind(interval, reservation_kind)?;
            &owned_selection
        };
        // Every series owns one dense bin per tile base, so reserve for the series the selected
        // records can actually open rather than always for the declared maximum.
        let maximum_series = maximum_series(selection)?;
        let maximum_bytes = maximum_retained_bytes(maximum_series, bins);
        let mut reservation = ledger.reserve(
            AllocationClass::ExactSummary,
            maximum_bytes,
            reservation_kind,
        )?;
        let _scratch = ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            maximum_bytes.saturating_mul(2),
            reservation_kind,
        )?;
        let mut builder = ModificationSummaryBuilder::new(interval, bins);
        for index in 0..selection.len() {
            let record = selection
                .record_view(index)
                .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
            let sequence = match selection
                .sequence(index)
                .ok_or(ExactModificationSummaryError::MissingSequenceCapability)?
            {
                SequenceView::Present { bases, .. } => Some(bases),
                SequenceView::Missing => None,
                SequenceView::NotLoaded => {
                    return Err(ExactModificationSummaryError::MissingSequenceCapability);
                }
            };
            let mm_upper = selection.auxiliary_field(index, *b"MM")?;
            let mm_lower = selection.auxiliary_field(index, *b"Mm")?;
            let ml_upper = selection.auxiliary_field(index, *b"ML")?;
            let ml_lower = selection.auxiliary_field(index, *b"Ml")?;
            let mn = selection.auxiliary_field(index, *b"MN")?;
            let haplotype = selection.auxiliary_field(index, *b"HP")?;
            builder.add_record_parts_checked(
                record.start(),
                record.cigar(),
                record.flags(),
                sequence,
                auxiliary_value(mm_upper.as_ref().or(mm_lower.as_ref())),
                auxiliary_value(ml_upper.as_ref().or(ml_lower.as_ref())),
                auxiliary_value(mn.as_ref()),
                auxiliary_value(haplotype.as_ref()),
            );
        }
        let mut summary = builder.finish(selection.len(), true);
        if summary.provenance.dropped_series > 0 {
            return Err(ExactModificationSummaryError::SeriesCapacityExceeded);
        }
        for series in &mut summary.series {
            series.bins.shrink_to_fit();
        }
        summary.series.shrink_to_fit();
        let retained_bytes = size_of::<Self>().saturating_add(summary_retained_bytes(&summary));
        let allocation = reservation.register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            source_revision: evidence.source_revision(),
            evidence_revision: evidence.revision(),
            record_contract: evidence.record_contract(),
            contract,
            tile,
            summary,
            allocation,
        }))
    }

    #[must_use]
    pub fn key(&self) -> ExactModificationSummaryKey {
        ExactModificationSummaryKey::new(
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
    pub const fn contract(&self) -> ModificationSummaryContract {
        self.contract
    }

    #[must_use]
    pub const fn tile(&self) -> CanonicalGenomicTile {
        self.tile
    }

    #[must_use]
    pub const fn summary(&self) -> &ModificationSummary {
        &self.summary
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
        size_of::<Self>().saturating_add(summary_retained_bytes(&self.summary))
    }
}

/// Retained bytes for `series` dense series of `bins` bins, with one doubling of slack for the
/// series vector while it grows.
fn maximum_retained_bytes(series: usize, bins: usize) -> usize {
    let per_series = size_of::<ModificationSeries>()
        .saturating_add(bins.saturating_mul(size_of::<ModificationBin>()));
    size_of::<ExactModificationSummaryTile>()
        .saturating_add(series.saturating_mul(2).saturating_mul(per_series))
}

/// Upper bound on the distinct modification series the selected records can open, read from the
/// `MM` group headers and `HP` haplotype without locating a single call. The builder itself never
/// retains more than [`MAX_MODIFICATION_SERIES`], so the scan stops once that many are possible.
fn maximum_series(
    selection: &crate::EvidenceSelection,
) -> Result<usize, ExactModificationSummaryError> {
    // Keys are deliberately finer than the builder's (exact header text, undecoded haplotype
    // value): over-counting keeps the bound safe, merging distinct series would not.
    let mut groups = std::collections::BTreeSet::<(String, Option<String>)>::new();
    let mut series = 0_usize;
    for index in 0..selection.len() {
        let mm_upper = selection.auxiliary_field(index, *b"MM")?;
        let mm_lower = selection.auxiliary_field(index, *b"Mm")?;
        let Some(AuxValue::String(mm)) = auxiliary_value(mm_upper.as_ref().or(mm_lower.as_ref()))
        else {
            continue;
        };
        let haplotype = selection
            .auxiliary_field(index, *b"HP")?
            .map(|field| format!("{:?}", field.value));
        for group in mm.split(';').filter(|group| !group.is_empty()) {
            let header = group.split_once(',').map_or(group, |(header, _)| header);
            if groups.insert((header.to_owned(), haplotype.clone())) {
                // One series per modification code; every header byte after the canonical base
                // and strand is at most one code.
                series = series.saturating_add(header.len().saturating_sub(2).max(1));
                if series >= MAX_MODIFICATION_SERIES {
                    return Ok(MAX_MODIFICATION_SERIES);
                }
            }
        }
    }
    Ok(series)
}

fn auxiliary_value(field: Option<&AuxField>) -> Option<&AuxValue> {
    field.map(|field| &field.value)
}

fn summary_retained_bytes(summary: &ModificationSummary) -> usize {
    summary
        .series
        .capacity()
        .saturating_mul(size_of::<ModificationSeries>())
        .saturating_add(summary.series.iter().fold(0_usize, |total, series| {
            total.saturating_add(
                series
                    .bins
                    .capacity()
                    .saturating_mul(size_of::<ModificationBin>()),
            )
        }))
}

#[derive(Debug, Error)]
pub enum ExactModificationSummaryError {
    #[error(transparent)]
    Tile(#[from] CanonicalTileError),
    #[error(transparent)]
    Evidence(#[from] EvidenceSelectionError),
    #[error(transparent)]
    Packed(#[from] rgv_model::PackedRecordError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("exact modification summary requires complete sequence payload")]
    MissingSequenceCapability,
    #[error("exact modification summary requires MM/Mm, ML/Ml, and HP tag capabilities")]
    MissingAuxiliaryCapability,
    #[error("canonical modification tile width {0} cannot be represented on this platform")]
    TileTooWide(u64),
    #[error("modification series exceed the declared exact capability")]
    SeriesCapacityExceeded,
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use rgv_core::{ContigId, GenomicInterval};
    use rgv_model::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, CigarOp, CigarOpKind, ImplicitModificationMode, ModificationCode,
        SequenceData,
    };

    use super::*;
    use crate::{
        AllocationClass, CanonicalRecordChunk, CompleteCoverage, EvidenceAcquisition,
        EvidenceChunk, EvidenceRootBuilder, FilteredRecordProvenance, PayloadContract,
        PayloadSidecar, RevisionFingerprint, SourceChunkId, SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn evidence(ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 14,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"modified".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"CCCC".to_vec(),
                qualities: None,
            },
            auxiliary_fields: vec![
                AuxField {
                    tag: *b"MM",
                    value: AuxValue::String("C+m,0;".to_owned()),
                },
                AuxField {
                    tag: *b"ML",
                    value: AuxValue::UnsignedArray(vec![255]),
                },
                AuxField {
                    tag: *b"HP",
                    value: AuxValue::Unsigned(2),
                },
            ],
        };
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((13 << 16) | 1),
            )
            .unwrap();
        let payload = Arc::new(
            AlignmentPayloadBatch::from_batch(
                &decoded,
                true,
                false,
                Some(&REQUIRED_MODIFICATION_TAGS),
            )
            .unwrap(),
        );
        let core = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 13,
            },
            Arc::new(decoded.copy_core().unwrap()),
            ContractFingerprint::new(7),
        )
        .unwrap();
        let payload_contract = PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::NotRequired,
            REQUIRED_MODIFICATION_TAGS,
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
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(7),
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
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

    #[test]
    fn packed_modification_payload_builds_exact_per_base_series() {
        let ledger = AllocationLedger::new(8 << 20, 0);
        let root = evidence(&ledger);
        let summary = ExactModificationSummaryTile::build(
            &root,
            CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap()),
            100,
            ModificationSummaryContract,
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();

        assert_eq!(summary.summary().series.len(), 1);
        let series = &summary.summary().series[0];
        assert_eq!(series.key.code, ModificationCode::Letter(b'm'));
        assert_eq!(
            series.key.implicit_mode,
            ImplicitModificationMode::Canonical
        );
        assert_eq!(series.key.haplotype, Some(2));
        assert_eq!(series.bins[10].modified_calls, 1);
        assert_eq!(series.bins[10].probability_sum, 255);
        assert_eq!(
            summary.allocation().snapshot().unwrap().charged_capacity,
            summary.estimated_bytes()
        );
    }

    #[test]
    fn reservation_follows_the_series_the_selected_records_can_open() {
        // Reserving the declared maximum (32 series, doubled twice) asked for ~0.4 MiB retained
        // and ~0.8 MiB of scratch for this single-series, 100-base tile.
        let ledger = AllocationLedger::new_with_transient_budget(8 << 20, 0, 64 << 10);
        let root = evidence(&ledger);
        let tile = CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap());
        let selection = root.select(tile.interval(100).unwrap()).unwrap();
        assert_eq!(maximum_series(&selection).unwrap(), 1);
        drop(selection);
        let summary = ExactModificationSummaryTile::build(
            &root,
            tile,
            100,
            ModificationSummaryContract,
            &ledger,
            ReservationKind::Normal,
            3,
        )
        .unwrap();

        // Reservation >= actual, and no longer sized for series nothing can open.
        let reserved = maximum_retained_bytes(1, 100);
        assert!(summary.estimated_bytes() <= reserved);
        assert!(reserved * 8 < maximum_retained_bytes(MAX_MODIFICATION_SERIES, 100));
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.transient, 0);
        assert!(snapshot.transient_high_water <= 64 << 10);
    }
}
