use std::sync::Arc;

use rgv_hts::{
    AlignmentPayloadBatch, AlignmentRecord, AuxField, PackedRecordError, SequenceData, SequenceView,
};
use thiserror::Error;

use crate::{
    AlignmentSourceRevision, AllocationClass, AllocationLease, CanonicalRecordChunk,
    CapabilityRequirement, LedgerError, PayloadContract, SourceChunkId,
};

/// Independently charged optional sequence/quality/tag capability for one canonical core chunk.
#[derive(Debug)]
pub struct PayloadSidecar {
    source_revision: AlignmentSourceRevision,
    chunk_id: SourceChunkId,
    contract: PayloadContract,
    payload: Arc<AlignmentPayloadBatch>,
    allocation: AllocationLease,
}

impl PayloadSidecar {
    pub fn new(
        core: &CanonicalRecordChunk,
        contract: PayloadContract,
        payload: Arc<AlignmentPayloadBatch>,
        allocation: AllocationLease,
    ) -> Result<Self, PayloadSidecarError> {
        if core.batch().len() != payload.len() {
            return Err(PayloadSidecarError::RecordCountMismatch {
                core: core.batch().len(),
                payload: payload.len(),
            });
        }
        for index in 0..core.batch().len() {
            if core.batch().source_position(index) != payload.source_position(index) {
                return Err(PayloadSidecarError::SourcePositionMismatch { record: index });
            }
        }
        if contract.sequence() == CapabilityRequirement::Required
            && !payload.has_complete_sequence_capability()
        {
            return Err(PayloadSidecarError::MissingSequenceCapability);
        }
        if contract.qualities() == CapabilityRequirement::Required
            && !payload.has_complete_quality_capability()
        {
            return Err(PayloadSidecarError::MissingQualityCapability);
        }
        let allocation_snapshot = allocation.snapshot()?;
        if allocation_snapshot.class != AllocationClass::PayloadSidecar {
            return Err(PayloadSidecarError::WrongAllocationClass {
                actual: allocation_snapshot.class,
            });
        }
        let expected = payload.estimated_bytes();
        if allocation_snapshot.charged_capacity != expected {
            return Err(PayloadSidecarError::WrongAllocationCapacity {
                expected,
                actual: allocation_snapshot.charged_capacity,
            });
        }
        Ok(Self {
            source_revision: core.source_revision(),
            chunk_id: core.id(),
            contract,
            payload,
            allocation,
        })
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn chunk_id(&self) -> SourceChunkId {
        self.chunk_id
    }

    #[must_use]
    pub const fn contract(&self) -> &PayloadContract {
        &self.contract
    }

    #[must_use]
    pub const fn allocation(&self) -> &AllocationLease {
        &self.allocation
    }

    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<SequenceView<'_>> {
        self.payload.sequence(index)
    }

    pub fn auxiliary_field(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<AuxField>, PackedRecordError> {
        self.payload.auxiliary_field(index, tag)
    }

    #[must_use]
    pub fn auxiliary_encoded_len(&self, index: usize) -> Option<usize> {
        self.payload.auxiliary_encoded_len(index)
    }

    pub fn auxiliary_value_payload_len(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<usize>, PackedRecordError> {
        self.payload.auxiliary_value_payload_len(index, tag)
    }

    /// Materialises the retained payload together with one core record. Callers requiring a
    /// lossless source record must additionally require `contract.all_auxiliary_tags()`.
    pub fn materialize_retained_record(
        &self,
        core: &CanonicalRecordChunk,
        index: usize,
    ) -> Result<AlignmentRecord, PackedRecordError> {
        if core.source_revision() != self.source_revision || core.id() != self.chunk_id {
            return Err(PackedRecordError::InvalidIndex(index));
        }
        let mut record = core.batch().materialize_record(index)?;
        record.sequence = match self
            .payload
            .sequence(index)
            .ok_or(PackedRecordError::InvalidSequence(index))?
        {
            SequenceView::NotLoaded => SequenceData::NotLoaded,
            SequenceView::Missing => SequenceData::Missing,
            SequenceView::Present { bases, qualities } => SequenceData::Present {
                bases: bases.to_vec(),
                qualities: qualities.map(<[u8]>::to_vec),
            },
        };
        record.auxiliary_fields = self.payload.auxiliary_fields(index)?;
        Ok(record)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum PayloadSidecarError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("payload record count {payload} does not match core record count {core}")]
    RecordCountMismatch { core: usize, payload: usize },
    #[error("payload record {record} does not address the corresponding core source record")]
    SourcePositionMismatch { record: usize },
    #[error("payload contract promises sequence capability which is not retained")]
    MissingSequenceCapability,
    #[error("payload contract promises quality capability which is not retained")]
    MissingQualityCapability,
    #[error("payload sidecar uses allocation class {actual:?} instead of PayloadSidecar")]
    WrongAllocationClass { actual: AllocationClass },
    #[error("payload allocation capacity is {actual} bytes; expected exactly {expected}")]
    WrongAllocationCapacity { expected: usize, actual: usize },
}

#[cfg(test)]
mod tests {
    use rgv_core::{ContigId, GenomicInterval};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentPayloadBatch, AlignmentRecord,
        AlignmentSourcePosition, AuxField, AuxValue, CigarOp, CigarOpKind, SequenceData,
    };

    use super::*;
    use crate::{
        AllocationLedger, CompleteCoverage, ContractFingerprint, FilteredRecordProvenance,
        ReservationKind, RevisionFingerprint, SourceFingerprint,
    };

    fn revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn contract() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::Required,
            CapabilityRequirement::Required,
            [*b"RG"],
            None,
        )
    }

    #[test]
    fn payload_enrichment_preserves_core_handle_and_is_charged_independently() {
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 14,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"payload".to_vec(),
            cigar: vec![CigarOp {
                kind: CigarOpKind::Match,
                len: 4,
            }],
            sequence: SequenceData::Present {
                bases: b"ACGT".to_vec(),
                qualities: Some(vec![30; 4]),
            },
            auxiliary_fields: vec![AuxField {
                tag: *b"RG",
                value: AuxValue::String("group-a".to_owned()),
            }],
        };
        let position = AlignmentSourcePosition::bam_virtual_offset((7 << 16) | 10);
        let mut decoded = AlignmentBatch::default();
        decoded
            .push_with_source_position(&record, position)
            .unwrap();
        let payload = Arc::new(
            AlignmentPayloadBatch::from_batch(&decoded, true, true, Some(&[*b"RG"])).unwrap(),
        );
        let core = Arc::new(decoded.copy_core().unwrap());
        let coverage = CompleteCoverage::new(
            revision(),
            ContractFingerprint::new(9),
            GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
            FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            },
        )
        .unwrap();
        let core = CanonicalRecordChunk::new(
            revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            core,
            coverage.record_contract(),
        )
        .unwrap();
        let handle = core.handles()[0];
        let ledger = AllocationLedger::new(1 << 20, 0);
        let bytes = payload.estimated_bytes();
        let mut reservation = ledger
            .reserve(
                AllocationClass::PayloadSidecar,
                bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let allocation = reservation.register(bytes, 1).unwrap();
        let sidecar = PayloadSidecar::new(&core, contract(), payload, allocation).unwrap();

        assert_eq!(core.handles(), [handle]);
        assert!(core.batch().is_core_only());
        assert_eq!(
            sidecar.sequence(0),
            Some(SequenceView::Present {
                bases: b"ACGT",
                qualities: Some(&[30; 4]),
            })
        );
        assert_eq!(
            sidecar.auxiliary_field(0, *b"RG").unwrap(),
            Some(AuxField {
                tag: *b"RG",
                value: AuxValue::String("group-a".to_owned()),
            })
        );
        assert_eq!(ledger.snapshot().unwrap().charged, bytes);
        drop(sidecar);
        assert_eq!(ledger.snapshot().unwrap().charged, 0);
    }
}
