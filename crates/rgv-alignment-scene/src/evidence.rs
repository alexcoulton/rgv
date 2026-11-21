use std::{collections::HashSet, mem::size_of, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::{AlignmentRecordView, AuxField, PackedRecordError, SequenceView};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentSourceRevision, AllocationClass, AllocationLease, AllocationLedger,
    AllocationReservation, CanonicalRecordChunk, CertifiedCoverageSet, ContractFingerprint,
    CoverageError, EvidenceRevision, LedgerError, PayloadContract, PayloadSidecar,
    PinnedAllocationLease, ReservationKind, SourceChunkId, TransientResourceClass,
    TransientResourceReservation,
};

/// Charged immutable source chunk plus its genomic overlap index.
#[derive(Debug)]
pub struct EvidenceChunk {
    chunk: CanonicalRecordChunk,
    core_allocation: AllocationLease,
    overlap_allocation: AllocationLease,
    payload: Option<Arc<PayloadSidecar>>,
}

impl EvidenceChunk {
    #[must_use]
    pub fn footprint(chunk: &CanonicalRecordChunk) -> EvidenceChunkFootprint {
        // The index is built once here and kept by the chunk, so admission reuses it.
        EvidenceChunkFootprint {
            core_bytes: chunk.estimated_bytes(),
            overlap_index_bytes: chunk.overlap_index().estimated_bytes(),
        }
    }

    pub fn new(
        chunk: CanonicalRecordChunk,
        core_allocation: AllocationLease,
        overlap_allocation: AllocationLease,
    ) -> Result<Self, EvidenceChunkError> {
        Self::build(chunk, core_allocation, overlap_allocation, None)
    }

    pub fn new_with_payload(
        chunk: CanonicalRecordChunk,
        core_allocation: AllocationLease,
        overlap_allocation: AllocationLease,
        payload: Arc<PayloadSidecar>,
    ) -> Result<Self, EvidenceChunkError> {
        Self::build(chunk, core_allocation, overlap_allocation, Some(payload))
    }

    fn build(
        chunk: CanonicalRecordChunk,
        core_allocation: AllocationLease,
        overlap_allocation: AllocationLease,
        payload: Option<Arc<PayloadSidecar>>,
    ) -> Result<Self, EvidenceChunkError> {
        if let Some(payload) = &payload
            && (payload.source_revision() != chunk.source_revision()
                || payload.chunk_id() != chunk.id())
        {
            return Err(EvidenceChunkError::PayloadOwnerMismatch);
        }
        validate_allocation(
            &core_allocation,
            AllocationClass::CoreChunk,
            chunk.estimated_bytes(),
        )?;
        validate_allocation(
            &overlap_allocation,
            AllocationClass::OverlapIndex,
            chunk.overlap_index().estimated_bytes(),
        )?;
        Ok(Self {
            chunk,
            core_allocation,
            overlap_allocation,
            payload,
        })
    }

    #[must_use]
    pub const fn chunk(&self) -> &CanonicalRecordChunk {
        &self.chunk
    }

    #[must_use]
    pub const fn core_allocation(&self) -> &AllocationLease {
        &self.core_allocation
    }

    #[must_use]
    pub const fn overlap_allocation(&self) -> &AllocationLease {
        &self.overlap_allocation
    }

    #[must_use]
    pub fn payload(&self) -> Option<&Arc<PayloadSidecar>> {
        self.payload.as_ref()
    }

    pub(crate) fn overlapping_record_indices(&self, interval: GenomicInterval) -> Vec<usize> {
        self.chunk
            .overlap_index()
            .select(self.chunk.batch(), interval)
    }

    pub(crate) fn overlapping_record_count(&self, interval: GenomicInterval) -> usize {
        self.chunk
            .overlap_index()
            .count(self.chunk.batch(), interval)
    }
}

fn validate_allocation(
    allocation: &AllocationLease,
    expected_class: AllocationClass,
    expected_bytes: usize,
) -> Result<(), EvidenceChunkError> {
    let snapshot = allocation.snapshot()?;
    if snapshot.class != expected_class {
        return Err(EvidenceChunkError::WrongAllocationClass {
            expected: expected_class,
            actual: snapshot.class,
        });
    }
    if snapshot.charged_capacity != expected_bytes {
        return Err(EvidenceChunkError::WrongAllocationCapacity {
            class: expected_class,
            expected: expected_bytes,
            actual: snapshot.charged_capacity,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceChunkFootprint {
    pub core_bytes: usize,
    pub overlap_index_bytes: usize,
}

/// Builder-private mutable transaction. Published evidence roots never mutate.
pub struct EvidenceRootBuilder {
    revision: EvidenceRevision,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    payload: PayloadContract,
    chunks: Vec<Arc<EvidenceChunk>>,
    coverage: CertifiedCoverageSet,
    metadata_reservation: AllocationReservation,
}

impl EvidenceRootBuilder {
    pub fn new(
        revision: EvidenceRevision,
        source_revision: AlignmentSourceRevision,
        record_contract: ContractFingerprint,
        payload: PayloadContract,
        expected_chunks: usize,
        expected_certificates: usize,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
    ) -> Result<Self, EvidenceBuildError> {
        let maximum_metadata_bytes = expected_chunks
            .saturating_mul(size_of::<Arc<EvidenceChunk>>())
            .saturating_add(expected_certificates.saturating_mul(
                size_of::<GenomicInterval>().saturating_add(size_of::<crate::CompleteCoverage>()),
            ));
        let metadata_reservation = ledger.reserve(
            AllocationClass::EvidenceRootMetadata,
            maximum_metadata_bytes,
            reservation_kind,
        )?;
        Ok(Self {
            revision,
            source_revision,
            record_contract,
            payload,
            chunks: Vec::with_capacity(expected_chunks),
            coverage: CertifiedCoverageSet::new(source_revision, record_contract),
            metadata_reservation,
        })
    }

    pub fn insert_acquisition(
        &mut self,
        acquisition: &crate::EvidenceAcquisition,
    ) -> Result<(), EvidenceBuildError> {
        if acquisition.source_revision() != self.source_revision {
            return Err(EvidenceBuildError::SourceRevisionMismatch);
        }
        if acquisition.record_contract() != self.record_contract {
            return Err(EvidenceBuildError::RecordContractMismatch);
        }
        let no_payload_required = self.payload.sequence()
            == crate::CapabilityRequirement::NotRequired
            && self.payload.qualities() == crate::CapabilityRequirement::NotRequired
            && !self.payload.all_auxiliary_tags()
            && self.payload.auxiliary_tags().is_empty()
            && self.payload.decode_reference().is_none();
        for chunk in acquisition.chunks() {
            let payload_satisfies = chunk
                .payload
                .as_ref()
                .is_some_and(|payload| payload.contract().satisfies(&self.payload));
            if !payload_satisfies && !no_payload_required {
                return Err(EvidenceBuildError::PayloadContractUnsatisfied);
            }
        }
        let mut next_coverage = self.coverage.clone();
        next_coverage.insert(acquisition.coverage())?;
        for chunk in acquisition.chunks() {
            self.chunks.push(Arc::clone(chunk));
        }
        self.coverage = next_coverage;
        Ok(())
    }

    pub fn build(
        mut self,
        publication_revision: u64,
    ) -> Result<Arc<EvidenceRoot>, EvidenceBuildError> {
        self.chunks.sort_by_key(|chunk| chunk.chunk.id());
        // A root may retain the same canonical record through more than one acquired chunk, so
        // summing chunk lengths is not exact. Compute the unique count once while this root is
        // still builder-owned; every published root is immutable and can thereafter expose the
        // value without rebuilding a million-entry set on UI or diagnostic paths.
        // A handle names its owning source chunk, and handles are unique within one chunk, so
        // only fragments sharing a chunk id can repeat a record. Deduplicating inside those
        // (rare) groups keeps the count exact without hashing every handle of every root.
        let record_count = self
            .chunks
            .chunk_by(|left, right| left.chunk.id() == right.chunk.id())
            .fold(0_usize, |total, fragments| {
                total.saturating_add(match fragments {
                    [only] => only.chunk.handles().len(),
                    fragments => fragments
                        .iter()
                        .flat_map(|chunk| chunk.chunk.handles().iter().copied())
                        .collect::<HashSet<_>>()
                        .len(),
                })
            });
        let retained_metadata_bytes = self
            .chunks
            .len()
            .saturating_mul(size_of::<Arc<EvidenceChunk>>())
            .saturating_add(
                self.coverage
                    .intervals()
                    .len()
                    .saturating_mul(size_of::<GenomicInterval>()),
            )
            .saturating_add(
                self.coverage
                    .certificates()
                    .len()
                    .saturating_mul(size_of::<crate::CompleteCoverage>()),
            );
        let metadata_allocation = self
            .metadata_reservation
            .register(retained_metadata_bytes, publication_revision)?;
        let coverage = Arc::new(self.coverage);
        Ok(Arc::new(EvidenceRoot {
            revision: self.revision,
            source_revision: self.source_revision,
            record_contract: self.record_contract,
            payload: self.payload,
            chunks: self.chunks.into(),
            record_count,
            coverage: Arc::clone(&coverage),
            eligible_coverage: coverage,
            view_filter: EvidenceViewFilter::unfiltered(record_count),
            allowed_handles: None,
            metadata_allocation,
        }))
    }
}

/// Immutable complete alignment evidence root.
#[derive(Debug)]
pub struct EvidenceRoot {
    revision: EvidenceRevision,
    source_revision: AlignmentSourceRevision,
    record_contract: ContractFingerprint,
    payload: PayloadContract,
    chunks: Arc<[Arc<EvidenceChunk>]>,
    record_count: usize,
    coverage: Arc<CertifiedCoverageSet>,
    /// Broad acquisition provenance retained beside a derived view. This remains immutable and
    /// lets explicit full-eligible export tell the truth without consulting a filtered summary.
    eligible_coverage: Arc<CertifiedCoverageSet>,
    view_filter: EvidenceViewFilter,
    allowed_handles: Option<Arc<[AlignmentHandle]>>,
    metadata_allocation: AllocationLease,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceViewFilter {
    fingerprint: ContractFingerprint,
    eligible_records: usize,
    matching_records: usize,
    predicate_matches: Arc<[usize]>,
}

impl EvidenceViewFilter {
    fn unfiltered(records: usize) -> Self {
        Self {
            fingerprint: ContractFingerprint::new(0),
            eligible_records: records,
            matching_records: records,
            predicate_matches: Arc::new([]),
        }
    }

    #[must_use]
    pub const fn fingerprint(&self) -> ContractFingerprint {
        self.fingerprint
    }

    #[must_use]
    pub const fn eligible_records(&self) -> usize {
        self.eligible_records
    }

    #[must_use]
    pub const fn matching_records(&self) -> usize {
        self.matching_records
    }

    #[must_use]
    pub fn predicate_matches(&self) -> &[usize] {
        &self.predicate_matches
    }
}

impl EvidenceRoot {
    #[must_use]
    pub const fn revision(&self) -> EvidenceRevision {
        self.revision
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    #[must_use]
    pub const fn record_contract(&self) -> ContractFingerprint {
        self.record_contract
    }

    #[must_use]
    pub const fn payload(&self) -> &PayloadContract {
        &self.payload
    }

    #[must_use]
    pub fn chunks(&self) -> &[Arc<EvidenceChunk>] {
        &self.chunks
    }

    #[must_use]
    pub fn coverage(&self) -> &CertifiedCoverageSet {
        &self.coverage
    }

    #[must_use]
    pub fn eligible_coverage(&self) -> &CertifiedCoverageSet {
        &self.eligible_coverage
    }

    /// Allocation owned directly by this root. Exposed only to the crate's move-only semantic
    /// retirement planner; callers must still prove exclusive `Arc` ownership before claiming it.
    pub(crate) const fn metadata_allocation(&self) -> &AllocationLease {
        &self.metadata_allocation
    }

    #[must_use]
    pub fn exact_filter_provenance(
        &self,
        interval: GenomicInterval,
    ) -> Option<crate::FilteredRecordProvenance> {
        self.coverage.exact_provenance(interval)
    }

    #[must_use]
    pub const fn record_count(&self) -> usize {
        self.record_count
    }

    #[must_use]
    pub const fn view_filter(&self) -> &EvidenceViewFilter {
        &self.view_filter
    }

    /// Whether the handle belongs to this exact derived view, not merely to one of the broad
    /// canonical chunks retained behind it.
    #[must_use]
    pub fn contains_handle(&self, handle: AlignmentHandle) -> bool {
        self.allowed_handles
            .as_ref()
            .is_none_or(|allowed| allowed.binary_search(&handle).is_ok())
            && self.record_ref(handle).is_some()
    }

    /// Conservative number of physical records retained by the root without allocating a
    /// temporary deduplication set. Builders use this to reserve before selecting records.
    #[must_use]
    pub fn record_count_upper_bound(&self) -> usize {
        if let Some(handles) = &self.allowed_handles {
            return handles.len();
        }
        self.chunks.iter().fold(0_usize, |total, chunk| {
            total.saturating_add(chunk.chunk.handles().len())
        })
    }

    pub fn pin(&self) -> Result<EvidencePins, LedgerError> {
        let mut allocations =
            Vec::with_capacity(self.chunks.len().saturating_mul(2).saturating_add(1));
        allocations.push(self.metadata_allocation.pin()?);
        for chunk in self.chunks.iter() {
            allocations.push(chunk.core_allocation.pin()?);
            allocations.push(chunk.overlap_allocation.pin()?);
            if let Some(payload) = &chunk.payload {
                allocations.push(payload.allocation().pin()?);
            }
        }
        Ok(EvidencePins { allocations })
    }

    pub fn select(
        self: &Arc<Self>,
        interval: GenomicInterval,
    ) -> Result<EvidenceSelection, EvidenceSelectionError> {
        self.select_with_reservation_kind(interval, ReservationKind::Normal)
    }

    pub fn select_with_reservation_kind(
        self: &Arc<Self>,
        interval: GenomicInterval,
        reservation_kind: ReservationKind,
    ) -> Result<EvidenceSelection, EvidenceSelectionError> {
        self.select_impl(interval, reservation_kind, true)
    }

    /// Selects the complete broad acquisition population, ignoring this root's derived view
    /// selection. This is deliberately separate from ordinary scene consumers and is used only
    /// for explicitly labelled full-eligible exports.
    pub fn select_eligible(
        self: &Arc<Self>,
        interval: GenomicInterval,
    ) -> Result<EvidenceSelection, EvidenceSelectionError> {
        self.select_impl(interval, ReservationKind::Normal, false)
    }

    fn select_impl(
        self: &Arc<Self>,
        interval: GenomicInterval,
        reservation_kind: ReservationKind,
        apply_view_filter: bool,
    ) -> Result<EvidenceSelection, EvidenceSelectionError> {
        // A derived filtered root deliberately has two authorities: `coverage` certifies the
        // interval for which its allowed-handle set is complete, while `eligible_coverage`
        // certifies the broader canonical population retained behind that view. Normal consumers
        // must stay inside the filtered certificate. The explicit `select_eligible` path is the
        // inverse: it ignores the allowed-handle set so it must validate against the eligible
        // certificate. Checking filtered coverage for both paths falsely rejects a cache-backed
        // zoom-out even though every canonical record is still resident.
        let certified_coverage = if apply_view_filter {
            &self.coverage
        } else {
            &self.eligible_coverage
        };
        if !certified_coverage.covers(interval) {
            return Err(EvidenceSelectionError::UncertifiedInterval(interval));
        }
        let (maximum_records, maximum_chunk_records) =
            self.chunks.iter().fold((0_usize, 0_usize), |state, chunk| {
                let count = chunk.overlapping_record_count(interval);
                (state.0.saturating_add(count), state.1.max(count))
            });
        let maximum_scratch =
            evidence_selection_scratch_bytes(maximum_records, maximum_chunk_records);
        let mut transient = self.metadata_allocation.ledger().reserve_transient(
            TransientResourceClass::EvidenceSelection,
            maximum_scratch,
            reservation_kind,
        )?;
        // Sort keys are read once per record. Evaluating them inside the comparator, and
        // deduplicating through a hash set, dominated selection at deep loci.
        let mut keyed = Vec::<(u64, u64, EvidenceRecordRef)>::with_capacity(maximum_records);
        for (chunk_index, chunk) in self.chunks.iter().enumerate() {
            let chunk_index_u32 =
                u32::try_from(chunk_index).map_err(|_| EvidenceSelectionError::IndexOverflow)?;
            let batch = chunk.chunk.batch();
            for record_index in chunk.chunk.overlap_index().select(batch, interval) {
                let record_index_u32 = u32::try_from(record_index)
                    .map_err(|_| EvidenceSelectionError::IndexOverflow)?;
                let handle = *chunk
                    .chunk
                    .handles()
                    .get(record_index)
                    .ok_or(EvidenceSelectionError::InvalidChunkIndex)?;
                if apply_view_filter
                    && self
                        .allowed_handles
                        .as_ref()
                        .is_some_and(|handles| handles.binary_search(&handle).is_err())
                {
                    continue;
                }
                keyed.push((
                    batch.start(record_index).unwrap_or_default(),
                    batch.end(record_index).unwrap_or_default(),
                    EvidenceRecordRef {
                        chunk: chunk_index_u32,
                        record: record_index_u32,
                        handle,
                    },
                ));
            }
        }
        // Stable: among copies of one physical record the first chunk wins, as before. Copies
        // share start, end and handle, so they are adjacent after sorting.
        keyed.sort_by_key(|(start, end, record)| (*start, *end, record.handle));
        keyed.dedup_by_key(|(_, _, record)| record.handle);
        let mut records = keyed
            .into_iter()
            .map(|(_, _, record)| record)
            .collect::<Vec<_>>();
        records.shrink_to_fit();
        let retained_scratch = evidence_selection_retained_bytes(records.capacity());
        let records: Arc<[EvidenceRecordRef]> = records.into();
        transient.shrink_to(retained_scratch)?;
        Ok(EvidenceSelection {
            root: Arc::clone(self),
            interval,
            records,
            _transient: transient,
        })
    }

    /// Selects only the stable handles already chosen by a row-atlas page.
    ///
    /// Unlike an interval selection, this does not enumerate every record at a deep locus. A
    /// handle identifies its small physical source chunk, so page construction is proportional
    /// to the rows being materialized rather than total coverage in the genomic tile.
    pub fn select_handles_with_reservation_kind<I>(
        self: &Arc<Self>,
        interval: GenomicInterval,
        handles: I,
        reservation_kind: ReservationKind,
    ) -> Result<EvidenceSelection, EvidenceSelectionError>
    where
        I: IntoIterator<Item = AlignmentHandle>,
        I::IntoIter: ExactSizeIterator,
    {
        if !self.coverage.covers(interval) {
            return Err(EvidenceSelectionError::UncertifiedInterval(interval));
        }
        let handles = handles.into_iter();
        let retained_bytes = evidence_selection_retained_bytes(handles.len());
        let mut transient = self.metadata_allocation.ledger().reserve_transient(
            TransientResourceClass::EvidenceSelection,
            retained_bytes,
            reservation_kind,
        )?;
        let mut records = Vec::with_capacity(handles.len());
        for handle in handles {
            if self
                .allowed_handles
                .as_ref()
                .is_some_and(|allowed| allowed.binary_search(&handle).is_err())
            {
                return Err(EvidenceSelectionError::MissingHandle(handle));
            }
            records.push(
                self.record_ref(handle)
                    .ok_or(EvidenceSelectionError::MissingHandle(handle))?,
            );
        }
        records.shrink_to_fit();
        let retained_bytes = evidence_selection_retained_bytes(records.capacity());
        let records: Arc<[EvidenceRecordRef]> = records.into();
        transient.shrink_to(retained_bytes)?;
        Ok(EvidenceSelection {
            root: Arc::clone(self),
            interval,
            records,
            _transient: transient,
        })
    }

    fn record_ref(&self, handle: AlignmentHandle) -> Option<EvidenceRecordRef> {
        if handle.source_revision() != self.source_revision {
            return None;
        }
        let owner = SourceChunkId::for_position(handle.source_position());
        let first = self
            .chunks
            .partition_point(|chunk| chunk.chunk.id() < owner);
        for (chunk_index, chunk) in self.chunks[first..].iter().enumerate() {
            if chunk.chunk.id() != owner {
                break;
            }
            let Some(record_index) = chunk.chunk.record_index(handle) else {
                continue;
            };
            return Some(EvidenceRecordRef {
                chunk: u32::try_from(first.saturating_add(chunk_index)).ok()?,
                record: u32::try_from(record_index).ok()?,
                handle,
            });
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn derive_filtered(
        self: &Arc<Self>,
        revision: EvidenceRevision,
        target: GenomicInterval,
        fingerprint: ContractFingerprint,
        mut handles: Vec<AlignmentHandle>,
        predicate_matches: Vec<usize>,
        eligible_records: usize,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, EvidenceBuildError> {
        handles.sort_unstable();
        handles.dedup();
        let excluded = eligible_records.saturating_sub(handles.len());
        let source_provenance = self.coverage.exact_provenance(target);
        let provenance = source_provenance.map_or(
            crate::FilteredRecordProvenance {
                records_seen: eligible_records as u64,
                eligible_records: handles.len() as u64,
                user_filter: excluded as u64,
                ..crate::FilteredRecordProvenance::default()
            },
            |source| crate::FilteredRecordProvenance {
                records_seen: source.records_seen,
                eligible_records: handles.len() as u64,
                unmapped: source.unmapped,
                secondary: source.secondary,
                supplementary: source.supplementary,
                user_filter: source.user_filter.saturating_add(excluded as u64),
            },
        );
        let mut coverage = CertifiedCoverageSet::new(self.source_revision, self.record_contract);
        coverage.insert(crate::CompleteCoverage::new(
            self.source_revision,
            self.record_contract,
            target,
            provenance,
        )?)?;
        let retained_bytes = self
            .chunks
            .len()
            .saturating_mul(size_of::<Arc<EvidenceChunk>>())
            .saturating_add(handles.len().saturating_mul(size_of::<AlignmentHandle>()))
            .saturating_add(predicate_matches.len().saturating_mul(size_of::<usize>()))
            .saturating_add(size_of::<GenomicInterval>())
            .saturating_add(size_of::<crate::CompleteCoverage>());
        let allocation = ledger
            .reserve(
                AllocationClass::EvidenceRootMetadata,
                retained_bytes,
                reservation_kind,
            )?
            .register(retained_bytes, publication_revision)?;
        let matching_records = handles.len();
        Ok(Arc::new(Self {
            revision,
            source_revision: self.source_revision,
            record_contract: self.record_contract,
            payload: self.payload.clone(),
            chunks: Arc::clone(&self.chunks),
            record_count: matching_records,
            coverage: Arc::new(coverage),
            eligible_coverage: Arc::clone(&self.eligible_coverage),
            view_filter: EvidenceViewFilter {
                fingerprint,
                eligible_records,
                matching_records,
                predicate_matches: predicate_matches.into(),
            },
            allowed_handles: Some(handles.into()),
            metadata_allocation: allocation,
        }))
    }

    pub(crate) fn derive_unfiltered(
        self: &Arc<Self>,
        revision: EvidenceRevision,
        target: GenomicInterval,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, EvidenceBuildError> {
        let record_count = self.select_eligible(target)?.len();
        let retained_bytes = self
            .chunks
            .len()
            .saturating_mul(size_of::<Arc<EvidenceChunk>>())
            .saturating_add(size_of::<Arc<CertifiedCoverageSet>>() * 2)
            .saturating_add(size_of::<GenomicInterval>());
        let allocation = ledger
            .reserve(
                AllocationClass::EvidenceRootMetadata,
                retained_bytes,
                reservation_kind,
            )?
            .register(retained_bytes, publication_revision)?;
        Ok(Arc::new(Self {
            revision,
            source_revision: self.source_revision,
            record_contract: self.record_contract,
            payload: self.payload.clone(),
            chunks: Arc::clone(&self.chunks),
            record_count,
            coverage: Arc::clone(&self.eligible_coverage),
            eligible_coverage: Arc::clone(&self.eligible_coverage),
            view_filter: EvidenceViewFilter::unfiltered(record_count),
            allowed_handles: None,
            metadata_allocation: allocation,
        }))
    }
}

/// Peak scratch of [`EvidenceRoot::select_impl`]: the keyed sort vector (two cached `u64` sort
/// keys beside each record reference), the retained record vector collected from it, and the
/// largest per-chunk overlap result. Selection deduplicates by sorting, so there is no hash set
/// to charge for.
fn evidence_selection_scratch_bytes(records: usize, maximum_chunk_records: usize) -> usize {
    evidence_selection_retained_bytes(records)
        .saturating_mul(2)
        .saturating_add(records.saturating_mul(size_of::<(u64, u64)>()))
        .saturating_add(maximum_chunk_records.saturating_mul(size_of::<usize>()))
}

fn evidence_selection_retained_bytes(records: usize) -> usize {
    records
        .saturating_mul(size_of::<EvidenceRecordRef>())
        .saturating_add(size_of::<usize>().saturating_mul(2))
}

/// Pins retained by a published/current evidence snapshot.
#[derive(Debug)]
pub struct EvidencePins {
    allocations: Vec<PinnedAllocationLease>,
}

impl EvidencePins {
    #[must_use]
    pub fn len(&self) -> usize {
        self.allocations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.allocations.is_empty()
    }

    pub(crate) fn allocations(&self) -> &[PinnedAllocationLease] {
        &self.allocations
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct EvidenceRecordRef {
    pub chunk: u32,
    pub record: u32,
    pub handle: AlignmentHandle,
}

/// Zero-copy logical record selection over immutable packed chunks.
#[derive(Debug)]
pub struct EvidenceSelection {
    root: Arc<EvidenceRoot>,
    interval: GenomicInterval,
    records: Arc<[EvidenceRecordRef]>,
    _transient: TransientResourceReservation,
}

impl EvidenceSelection {
    #[must_use]
    pub const fn interval(&self) -> GenomicInterval {
        self.interval
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    #[must_use]
    pub fn records(&self) -> &[EvidenceRecordRef] {
        &self.records
    }

    #[must_use]
    pub fn record_view(&self, index: usize) -> Option<AlignmentRecordView<'_>> {
        let record = self.records.get(index)?;
        self.root
            .chunks
            .get(record.chunk as usize)?
            .chunk
            .batch()
            .record_view(record.record as usize)
    }

    #[must_use]
    pub fn sequence(&self, index: usize) -> Option<SequenceView<'_>> {
        let record = self.records.get(index)?;
        self.root
            .chunks
            .get(record.chunk as usize)?
            .payload
            .as_ref()?
            .sequence(record.record as usize)
    }

    pub fn auxiliary_field(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<AuxField>, PackedRecordError> {
        let record = self
            .records
            .get(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let payload = self
            .root
            .chunks
            .get(record.chunk as usize)
            .and_then(|chunk| chunk.payload.as_ref())
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        payload.auxiliary_field(record.record as usize, tag)
    }

    #[must_use]
    pub fn auxiliary_encoded_len(&self, index: usize) -> Option<usize> {
        let record = self.records.get(index)?;
        self.root
            .chunks
            .get(record.chunk as usize)?
            .payload
            .as_ref()?
            .auxiliary_encoded_len(record.record as usize)
    }

    pub fn auxiliary_value_payload_len(
        &self,
        index: usize,
        tag: [u8; 2],
    ) -> Result<Option<usize>, PackedRecordError> {
        let record = self
            .records
            .get(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let payload = self
            .root
            .chunks
            .get(record.chunk as usize)
            .and_then(|chunk| chunk.payload.as_ref())
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        payload.auxiliary_value_payload_len(record.record as usize, tag)
    }

    /// Materialises one retained record only for API and specialist-analysis boundaries. Normal
    /// row/page construction continues to use zero-allocation packed views.
    pub fn materialize_retained_record(
        &self,
        index: usize,
    ) -> Result<rgv_hts::AlignmentRecord, PackedRecordError> {
        let record = self
            .records
            .get(index)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        let chunk = self
            .root
            .chunks
            .get(record.chunk as usize)
            .ok_or(PackedRecordError::InvalidIndex(index))?;
        if let Some(payload) = &chunk.payload {
            payload.materialize_retained_record(&chunk.chunk, record.record as usize)
        } else {
            chunk
                .chunk
                .batch()
                .materialize_record(record.record as usize)
        }
    }
}

#[derive(Debug, Error)]
pub enum EvidenceBuildError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Coverage(#[from] CoverageError),
    #[error(transparent)]
    Selection(#[from] EvidenceSelectionError),
    #[error("evidence chunk belongs to a different source revision")]
    SourceRevisionMismatch,
    #[error("evidence chunk belongs to a different record contract")]
    RecordContractMismatch,
    #[error("evidence chunk does not satisfy the root payload contract")]
    PayloadContractUnsatisfied,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EvidenceChunkError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("evidence allocation class mismatch: expected {expected:?}, received {actual:?}")]
    WrongAllocationClass {
        expected: AllocationClass,
        actual: AllocationClass,
    },
    #[error(
        "evidence {class:?} allocation capacity mismatch: expected {expected} bytes, received {actual}"
    )]
    WrongAllocationCapacity {
        class: AllocationClass,
        expected: usize,
        actual: usize,
    },
    #[error("payload sidecar belongs to a different source chunk")]
    PayloadOwnerMismatch,
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum EvidenceSelectionError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("requested interval {0:?} is not covered by completeness certificates")]
    UncertifiedInterval(GenomicInterval),
    #[error("evidence index exceeds the scene's compact index representation")]
    IndexOverflow,
    #[error("evidence chunk index is internally inconsistent")]
    InvalidChunkIndex,
    #[error("stable alignment handle {0:?} is absent from the immutable evidence root")]
    MissingHandle(AlignmentHandle),
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rgv_core::{ContigId, GenomicInterval};
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentRecord, AlignmentSourcePosition, SequenceData,
    };

    use super::*;
    use crate::{
        CapabilityRequirement, CompleteCoverage, FilteredRecordProvenance, RevisionFingerprint,
        SourceChunkId, SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [],
            None,
        )
    }

    fn record(name: &[u8], start: u64, end: u64) -> AlignmentRecord {
        AlignmentRecord {
            contig: ContigId::new(0),
            start,
            end,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: name.to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        }
    }

    fn evidence_chunk(
        ledger: &AllocationLedger,
        block: u64,
        records: &[(&[u8], u64, u64, u64)],
    ) -> Arc<EvidenceChunk> {
        let mut batch = AlignmentBatch::default();
        for (name, start, end, in_block) in records {
            batch
                .push_with_source_position(
                    &record(name, *start, *end),
                    AlignmentSourcePosition::bam_virtual_offset((block << 16) | in_block),
                )
                .unwrap();
        }
        let chunk = CanonicalRecordChunk::new(
            source_revision(),
            SourceChunkId::BamBlock {
                compressed_offset: block,
            },
            Arc::new(batch),
            ContractFingerprint::new(9),
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
        let core_allocation = reservation.register(footprint.core_bytes, block).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap_allocation = reservation
            .register(footprint.overlap_index_bytes, block)
            .unwrap();
        Arc::new(EvidenceChunk::new(chunk, core_allocation, overlap_allocation).unwrap())
    }

    fn root(ledger: &AllocationLedger) -> Arc<EvidenceRoot> {
        let coverage = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let first = evidence_chunk(ledger, 1, &[(b"first", 5, 15, 1), (b"long", 10, 80, 2)]);
        let second = evidence_chunk(ledger, 2, &[(b"second", 40, 50, 1)]);
        let mut builder = EvidenceRootBuilder::new(
            EvidenceRevision::new(RevisionFingerprint::new(10, 11)),
            source_revision(),
            ContractFingerprint::new(9),
            payload(),
            2,
            1,
            ledger,
            ReservationKind::Normal,
        )
        .unwrap();
        let acquisition = crate::EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(9),
                coverage,
                FilteredRecordProvenance {
                    records_seen: 3,
                    eligible_records: 3,
                    ..FilteredRecordProvenance::default()
                },
            )
            .unwrap(),
            [first, second],
        )
        .unwrap();
        builder.insert_acquisition(&acquisition).unwrap();
        builder.build(12).unwrap()
    }

    #[test]
    fn selection_is_zero_copy_complete_and_cross_chunk() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        let selection = root
            .select(GenomicInterval::new(ContigId::new(0), 45, 46).unwrap())
            .unwrap();

        assert_eq!(selection.len(), 2);
        assert_eq!(selection.record_view(0).unwrap().read_name(), b"long");
        assert_eq!(selection.record_view(1).unwrap().read_name(), b"second");
        assert!(
            selection
                .records()
                .iter()
                .all(|record| record.handle.source_revision() == source_revision())
        );
    }

    #[test]
    fn immutable_root_caches_its_exact_unique_record_count() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        let expected = root
            .chunks()
            .iter()
            .flat_map(|chunk| chunk.chunk().handles().iter().copied())
            .collect::<HashSet<_>>()
            .len();

        assert_eq!(root.record_count(), expected);
        assert_eq!(root.record_count(), 3);
    }

    #[test]
    fn stable_handle_selection_resolves_only_requested_row_records() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        let first = root.chunks()[0].chunk().handles()[0];
        let second = root.chunks()[1].chunk().handles()[0];
        let interval = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let selection = root
            .select_handles_with_reservation_kind(
                interval,
                [second, first],
                ReservationKind::Normal,
            )
            .unwrap();

        assert_eq!(selection.len(), 2);
        assert_eq!(selection.records()[0].handle, second);
        assert_eq!(selection.record_view(0).unwrap().read_name(), b"second");
        assert_eq!(selection.records()[1].handle, first);
        assert_eq!(selection.record_view(1).unwrap().read_name(), b"first");
    }

    #[test]
    fn uncertified_selection_is_refused_instead_of_returning_partial_evidence() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        let interval = GenomicInterval::new(ContigId::new(0), 100, 101).unwrap();
        assert_eq!(
            root.select(interval).unwrap_err(),
            EvidenceSelectionError::UncertifiedInterval(interval)
        );
    }

    #[test]
    fn filtered_root_selects_broader_eligible_evidence_only_through_explicit_boundary() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let canonical = root(&ledger);
        let broad = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let narrow = GenomicInterval::new(ContigId::new(0), 40, 50).unwrap();
        let narrow_selection = canonical.select(narrow).unwrap();
        let retained = narrow_selection
            .records()
            .iter()
            .map(|record| record.handle)
            .take(1)
            .collect::<Vec<_>>();
        let eligible_records = narrow_selection.len();
        drop(narrow_selection);
        let filtered = canonical
            .derive_filtered(
                EvidenceRevision::new(RevisionFingerprint::new(10, 12)),
                narrow,
                ContractFingerprint::new(77),
                retained,
                vec![1],
                eligible_records,
                &ledger,
                ReservationKind::Normal,
                13,
            )
            .unwrap();

        assert!(filtered.coverage().covers(narrow));
        assert!(!filtered.coverage().covers(broad));
        assert!(filtered.eligible_coverage().covers(broad));
        assert_eq!(
            filtered.select(broad).unwrap_err(),
            EvidenceSelectionError::UncertifiedInterval(broad)
        );
        assert_eq!(filtered.select_eligible(broad).unwrap().len(), 3);
    }

    #[test]
    fn incomplete_multi_chunk_acquisition_cannot_publish_a_complete_certificate() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let first = evidence_chunk(&ledger, 1, &[(b"only", 5, 15, 1)]);
        let error = crate::EvidenceAcquisition::new(
            CompleteCoverage::new(
                source_revision(),
                ContractFingerprint::new(9),
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
                FilteredRecordProvenance {
                    records_seen: 2,
                    eligible_records: 2,
                    ..FilteredRecordProvenance::default()
                },
            )
            .unwrap(),
            [first],
        )
        .unwrap_err();
        assert_eq!(
            error,
            crate::EvidenceAcquisitionError::EligibleRecordCountMismatch {
                expected: 2,
                actual: 1,
            }
        );
    }

    #[test]
    fn root_and_chunk_allocations_remain_charged_through_selection_and_pins() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
        let selection = root
            .select(GenomicInterval::new(ContigId::new(0), 45, 46).unwrap())
            .unwrap();
        let selection_snapshot = ledger.snapshot().unwrap();
        assert!(selection_snapshot.transient > 0);
        assert!(
            selection_snapshot
                .transient_resources
                .iter()
                .any(
                    |usage| usage.class == TransientResourceClass::EvidenceSelection
                        && usage.current > 0
                )
        );
        let pins = root.pin().unwrap();
        assert_eq!(pins.len(), 5);
        let charged = ledger.snapshot().unwrap().charged;
        drop(root);
        assert_eq!(ledger.snapshot().unwrap().charged, charged);
        drop(selection);
        assert_eq!(ledger.snapshot().unwrap().transient, 0);
        drop(pins);
        assert_eq!(ledger.snapshot().unwrap().charged, 0);
    }

    #[test]
    fn selection_reserves_before_allocating_and_refuses_cleanly_at_the_budget() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let root = root(&ledger);
        let charged = ledger.snapshot().unwrap().charged;
        ledger.set_transient_budget(0).unwrap();
        let error = root
            .select(GenomicInterval::new(ContigId::new(0), 45, 46).unwrap())
            .unwrap_err();
        assert!(matches!(
            error,
            EvidenceSelectionError::Ledger(LedgerError::TransientBudgetExceeded { .. })
        ));
        let snapshot = ledger.snapshot().unwrap();
        assert_eq!(snapshot.charged, charged);
        assert_eq!(snapshot.transient, 0);
    }

    #[test]
    fn admission_policy_does_not_change_evidence_identity() {
        let _policy = crate::AdmissionPolicy::new(
            NonZeroUsize::new(100).unwrap(),
            NonZeroUsize::new(1 << 20).unwrap(),
            400,
        );
        let ledger = AllocationLedger::new(1 << 20, 0);
        assert_eq!(root(&ledger).record_contract(), ContractFingerprint::new(9));
    }
}
