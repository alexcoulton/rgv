use std::{collections::HashSet, mem::size_of, sync::Arc};

use rgv_hts::{AlignmentBatch, AlignmentSourcePosition};
use thiserror::Error;

use crate::{AlignmentHandle, AlignmentSourceRevision, ContractFingerprint};

/// Canonical source-owned decode unit. Genomic render tiles never own records.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SourceChunkId {
    BamBlock {
        compressed_offset: u64,
    },
    CramSlice {
        container_offset: u64,
        slice_landmark: u64,
    },
}

impl SourceChunkId {
    #[must_use]
    pub const fn for_position(position: AlignmentSourcePosition) -> Self {
        match position {
            AlignmentSourcePosition::BamVirtualOffset(offset) => Self::BamBlock {
                compressed_offset: offset >> 16,
            },
            AlignmentSourcePosition::CramRecord {
                container_offset,
                slice_landmark,
                ..
            } => Self::CramSlice {
                container_offset,
                slice_landmark,
            },
        }
    }
}

/// One immutable packed record allocation owned by a physical source chunk.
///
/// Completeness belongs to an atomic [`crate::EvidenceAcquisition`] dependency set so evicting a
/// sibling chunk can never leave behind a false per-chunk coverage claim.
#[derive(Clone, Debug)]
pub struct CanonicalRecordChunk {
    source_revision: AlignmentSourceRevision,
    id: SourceChunkId,
    record_contract: ContractFingerprint,
    batch: Arc<AlignmentBatch>,
    handles: Arc<[AlignmentHandle]>,
}

impl CanonicalRecordChunk {
    pub fn new(
        source_revision: AlignmentSourceRevision,
        id: SourceChunkId,
        batch: Arc<AlignmentBatch>,
        record_contract: ContractFingerprint,
    ) -> Result<Self, ChunkError> {
        if !batch.is_core_only() {
            return Err(ChunkError::OptionalPayloadInCoreChunk);
        }
        let mut unique = HashSet::with_capacity(batch.len());
        let mut handles = Vec::with_capacity(batch.len());
        for index in 0..batch.len() {
            let position = batch
                .source_position(index)
                .ok_or(ChunkError::MissingSourcePosition { record: index })?;
            let actual = SourceChunkId::for_position(position);
            if actual != id {
                return Err(ChunkError::WrongOwnerChunk {
                    record: index,
                    expected: id,
                    actual,
                });
            }
            let handle = AlignmentHandle::new(source_revision, position);
            if !unique.insert(handle) {
                return Err(ChunkError::DuplicateSourcePosition { record: index });
            }
            handles.push(handle);
        }
        Ok(Self {
            source_revision,
            id,
            record_contract,
            batch,
            handles: handles.into(),
        })
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn id(&self) -> SourceChunkId {
        self.id
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub fn batch(&self) -> &Arc<AlignmentBatch> {
        &self.batch
    }

    #[must_use]
    pub fn handles(&self) -> &[AlignmentHandle] {
        &self.handles
    }

    /// Resolves a stable source-position handle without scanning any other source chunk. Chunks
    /// are deliberately small physical decode units; their packed records may be reordered by
    /// genomic position, so lookup stays local rather than assuming physical-order storage.
    #[must_use]
    pub fn record_index(&self, handle: AlignmentHandle) -> Option<usize> {
        (handle.source_revision() == self.source_revision
            && SourceChunkId::for_position(handle.source_position()) == self.id)
            .then(|| {
                self.handles
                    .iter()
                    .position(|candidate| *candidate == handle)
            })
            .flatten()
    }

    /// Retained packed-record and stable-handle capacity owned by this chunk.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.batch.estimated_bytes().saturating_add(
            self.handles
                .len()
                .saturating_mul(size_of::<AlignmentHandle>()),
        )
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ChunkError {
    #[error("canonical core chunk retains sequence, quality, or auxiliary payload")]
    OptionalPayloadInCoreChunk,
    #[error("record {record} has no stable physical source position")]
    MissingSourcePosition { record: usize },
    #[error("record {record} belongs to {actual:?}, not declared owner {expected:?}")]
    WrongOwnerChunk {
        record: usize,
        expected: SourceChunkId,
        actual: SourceChunkId,
    },
    #[error("record {record} repeats an existing physical source position")]
    DuplicateSourcePosition { record: usize },
}

#[cfg(test)]
mod tests {
    use rgv_core::ContigId;
    use rgv_hts::{AlignmentFlags, AlignmentRecord, SequenceData};

    use super::*;
    use crate::{ContractFingerprint, RevisionFingerprint, SourceFingerprint};

    fn revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn record() -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start: 10,
            end: 20,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"duplicate".to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    #[test]
    fn true_duplicates_keep_distinct_handles() {
        let mut batch = AlignmentBatch::default();
        batch
            .push_with_source_position(
                &record(),
                AlignmentSourcePosition::bam_virtual_offset((7 << 16) | 10),
            )
            .unwrap();
        batch
            .push_with_source_position(
                &record(),
                AlignmentSourcePosition::bam_virtual_offset((7 << 16) | 20),
            )
            .unwrap();
        let chunk = CanonicalRecordChunk::new(
            revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(batch),
            ContractFingerprint::new(5),
        )
        .unwrap();

        assert_ne!(chunk.handles()[0], chunk.handles()[1]);
    }

    #[test]
    fn unpositioned_compatibility_records_cannot_enter_scene_store() {
        let mut batch = AlignmentBatch::default();
        batch.push(&record()).unwrap();
        let error = CanonicalRecordChunk::new(
            revision(),
            SourceChunkId::BamBlock {
                compressed_offset: 7,
            },
            Arc::new(batch),
            ContractFingerprint::new(5),
        )
        .unwrap_err();

        assert_eq!(error, ChunkError::MissingSourcePosition { record: 0 });
    }
}
