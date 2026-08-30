use std::{collections::HashSet, sync::Arc};

use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSourceRevision, CompleteCoverage, ContractFingerprint, EvidenceChunk,
    EvidenceRootBuilder,
};

/// Atomic result of one complete source evaluation.
///
/// Completeness belongs to this group, never to an independently evictable member chunk. The
/// constructor verifies that the exact number of eligible physical records overlapping the
/// certified interval is present once across the group.
#[derive(Debug)]
pub struct EvidenceAcquisition {
    coverage: CompleteCoverage,
    chunks: Arc<[Arc<EvidenceChunk>]>,
}

impl EvidenceAcquisition {
    pub fn new(
        coverage: CompleteCoverage,
        chunks: impl IntoIterator<Item = Arc<EvidenceChunk>>,
    ) -> Result<Self, EvidenceAcquisitionError> {
        let chunks = chunks.into_iter().collect::<Vec<_>>();
        let mut handles = HashSet::<AlignmentHandle>::new();
        let mut overlapping_records = 0_u64;
        for chunk in &chunks {
            if chunk.chunk().source_revision() != coverage.source_revision() {
                return Err(EvidenceAcquisitionError::SourceRevisionMismatch);
            }
            if chunk.chunk().record_contract() != coverage.record_contract() {
                return Err(EvidenceAcquisitionError::RecordContractMismatch);
            }
            for handle in chunk.chunk().handles() {
                if !handles.insert(*handle) {
                    return Err(EvidenceAcquisitionError::DuplicateHandle(*handle));
                }
            }
            overlapping_records = overlapping_records
                .saturating_add(chunk.overlapping_record_indices(coverage.interval()).len() as u64);
        }
        let expected = coverage.provenance().eligible_records;
        if overlapping_records != expected {
            return Err(EvidenceAcquisitionError::EligibleRecordCountMismatch {
                expected,
                actual: overlapping_records,
            });
        }
        Ok(Self {
            coverage,
            chunks: chunks.into(),
        })
    }

    #[must_use]
    pub const fn coverage(&self) -> CompleteCoverage {
        self.coverage
    }

    #[must_use]
    pub fn chunks(&self) -> &[Arc<EvidenceChunk>] {
        &self.chunks
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.coverage.source_revision()
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.coverage.record_contract()
    }

    pub fn insert_into(
        &self,
        builder: &mut EvidenceRootBuilder,
    ) -> Result<(), crate::EvidenceBuildError> {
        builder.insert_acquisition(self)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EvidenceAcquisitionError {
    #[error("acquisition chunk belongs to a different source revision")]
    SourceRevisionMismatch,
    #[error("acquisition chunk belongs to a different record contract")]
    RecordContractMismatch,
    #[error("physical record {0:?} occurs more than once in one acquisition")]
    DuplicateHandle(AlignmentHandle),
    #[error(
        "acquisition certifies {expected} eligible records but contains {actual} overlapping physical records"
    )]
    EligibleRecordCountMismatch { expected: u64, actual: u64 },
}
