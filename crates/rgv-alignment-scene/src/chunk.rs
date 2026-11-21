use std::{
    collections::HashSet,
    mem::size_of,
    sync::{Arc, OnceLock},
};

use rgv_hts::{AlignmentBatch, AlignmentOverlapIndex, AlignmentSourcePosition};
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
    /// Record indices ordered by handle, kept only when `handles` is not already ascending
    /// (packed records may be reordered by genomic position). Either way a handle resolves by
    /// binary search instead of a scan of the chunk.
    handle_order: Option<Arc<[u32]>>,
    /// Genomic overlap index over `batch`, built on first use and shared by every clone. Sizing
    /// a chunk for its ledger reservation and then admitting it used to build the index twice.
    overlap: Arc<OnceLock<AlignmentOverlapIndex>>,
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
        let handle_order = if handles.windows(2).all(|pair| pair[0] < pair[1]) {
            None
        } else {
            let mut order = (0..handles.len())
                .map(|index| {
                    u32::try_from(index).map_err(|_| ChunkError::TooManyRecords { record: index })
                })
                .collect::<Result<Vec<_>, _>>()?;
            order.sort_unstable_by_key(|index| handles[*index as usize]);
            Some(order.into())
        };
        Ok(Self {
            source_revision,
            id,
            record_contract,
            batch,
            handles: handles.into(),
            handle_order,
            overlap: Arc::new(OnceLock::new()),
        })
    }

    /// The chunk's genomic overlap index. It is charged separately from
    /// [`Self::estimated_bytes`] by the owning [`crate::EvidenceChunk`].
    pub(crate) fn overlap_index(&self) -> &AlignmentOverlapIndex {
        self.overlap
            .get_or_init(|| AlignmentOverlapIndex::new(&self.batch))
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
        if handle.source_revision() != self.source_revision
            || SourceChunkId::for_position(handle.source_position()) != self.id
        {
            return None;
        }
        match &self.handle_order {
            None => self.handles.binary_search(&handle).ok(),
            Some(order) => order
                .binary_search_by_key(&handle, |index| self.handles[*index as usize])
                .ok()
                .map(|position| order[position] as usize),
        }
    }

    /// Retained packed-record and stable-handle capacity owned by this chunk.
    #[must_use]
    pub fn estimated_bytes(&self) -> usize {
        self.batch
            .estimated_bytes()
            .saturating_add(
                self.handles
                    .len()
                    .saturating_mul(size_of::<AlignmentHandle>()),
            )
            .saturating_add(
                self.handle_order
                    .as_ref()
                    .map_or(0, |order| order.len().saturating_mul(size_of::<u32>())),
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
    #[error("record {record} exceeds the per-chunk record index range")]
    TooManyRecords { record: usize },
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
    fn handles_resolve_by_search_whatever_order_the_batch_stores_them_in() {
        // Deterministic shuffles of one block's virtual offsets, including the ascending one.
        for (case, step) in [1_u64, 7, 11, 13].into_iter().enumerate() {
            let count = 97_u64;
            let offsets = (0..count)
                .map(|index| (7 << 16) | ((index * step) % count + 1))
                .collect::<Vec<_>>();
            let mut batch = AlignmentBatch::default();
            for offset in &offsets {
                batch
                    .push_with_source_position(
                        &record(),
                        AlignmentSourcePosition::bam_virtual_offset(*offset),
                    )
                    .unwrap();
            }
            let chunk = CanonicalRecordChunk::new(
                revision(),
                SourceChunkId::BamBlock {
                    compressed_offset: 7,
                },
                Arc::new(batch),
                ContractFingerprint::new(5),
            )
            .unwrap();
            assert_eq!(chunk.handle_order.is_none(), step == 1, "case {case}");
            for (index, handle) in chunk.handles().iter().enumerate() {
                assert_eq!(chunk.record_index(*handle), Some(index), "case {case}");
                // The former linear scan is the oracle.
                assert_eq!(
                    chunk.record_index(*handle),
                    chunk
                        .handles()
                        .iter()
                        .position(|candidate| candidate == handle),
                );
            }
            let absent = [
                AlignmentHandle::new(
                    revision(),
                    AlignmentSourcePosition::bam_virtual_offset((7 << 16) | 5_000),
                ),
                AlignmentHandle::new(
                    revision(),
                    AlignmentSourcePosition::bam_virtual_offset((8 << 16) | 1),
                ),
                AlignmentHandle::new(
                    AlignmentSourceRevision::new(
                        SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
                        RevisionFingerprint::new(3, 5),
                    ),
                    AlignmentSourcePosition::bam_virtual_offset((7 << 16) | 1),
                ),
            ];
            for handle in absent {
                assert_eq!(chunk.record_index(handle), None, "case {case}");
            }
        }
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
