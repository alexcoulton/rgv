use std::{
    cmp::Reverse,
    collections::{HashMap, HashSet},
    mem::size_of,
    num::NonZeroU64,
    sync::Arc,
};

use rgv_core::GenomicInterval;
use thiserror::Error;

use crate::{
    AlignmentCacheResidencyClass, AlignmentCacheResidencyInventory, AlignmentFilterEvaluationError,
    AlignmentRowAtlas, AlignmentRowAtlasError, AlignmentRowAtlasPins, AlignmentSourceRevision,
    AlignmentViewFilterContract, AlleleSummaryContract, AllocationId, AllocationLease,
    AllocationLedger, AllocationRecordSnapshot, CanonicalGenomicTile, DepthSummaryContract,
    EvidenceAcquisition, EvidenceBuildError, EvidenceChunk, EvidencePins, EvidenceRevision,
    EvidenceRoot, EvidenceRootBuilder, ExactAlleleSummaryError, ExactAlleleSummaryKey,
    ExactAlleleSummaryTile, ExactDepthSummaryError, ExactDepthSummaryKey, ExactDepthSummaryTile,
    ExactFilterSummaryError, ExactFilterSummaryKey, ExactFilterSummaryTile,
    ExactJunctionSummaryError, ExactJunctionSummaryKey, ExactJunctionSummaryTile,
    ExactModificationSummaryError, ExactModificationSummaryKey, ExactModificationSummaryTile,
    ExactPairSummaryError, ExactPairSummaryKey, ExactPairSummaryTile, ExactSummaryPins,
    ExactSummaryRoot, ExactSummaryRootError, JunctionSummaryContract, LayoutContract, LedgerError,
    ModificationSummaryContract, PairSummaryContract, PayloadContract, ReservationKind,
    RevisionFingerprint, RowAtlasRevision, SourceChunkId, SummaryContract, SummaryRevision,
    TransientResourceClass, evaluate_alignment_filter,
};

/// Mutable cache authority intended to be owned by exactly one source actor.
///
/// All values it publishes are immutable. Preparing or dropping a candidate cannot mutate the
/// currently published evidence root.
#[derive(Debug)]
pub struct EvidenceStore {
    source_revision: AlignmentSourceRevision,
    record_contract: crate::ContractFingerprint,
    ledger: AllocationLedger,
    chunks: Vec<CachedEvidenceChunk>,
    acquisitions: Vec<CachedAcquisition>,
    depth_summaries: Vec<CachedDepthSummary>,
    allele_summaries: Vec<CachedAlleleSummary>,
    junction_summaries: Vec<CachedJunctionSummary>,
    pair_summaries: Vec<CachedPairSummary>,
    modification_summaries: Vec<CachedModificationSummary>,
    filter_summaries: Vec<CachedFilterSummary>,
    row_atlases: Vec<CachedRowAtlas>,
    /// Keyed positions into the cache vectors above. The vectors stay the eviction-ordered
    /// owners; these only replace the linear scan every lookup used to make.
    chunk_index: CachePositionIndex<SourceChunkId>,
    depth_index: CachePositionIndex<(ExactDepthSummaryKey, crate::ContractFingerprint)>,
    allele_index: CachePositionIndex<(ExactAlleleSummaryKey, crate::ContractFingerprint)>,
    junction_index: CachePositionIndex<(ExactJunctionSummaryKey, crate::ContractFingerprint)>,
    pair_index: CachePositionIndex<(ExactPairSummaryKey, crate::ContractFingerprint)>,
    modification_index:
        CachePositionIndex<(ExactModificationSummaryKey, crate::ContractFingerprint)>,
    filter_index: CachePositionIndex<(ExactFilterSummaryKey, crate::ContractFingerprint)>,
    current: Option<PublishedEvidence>,
    next_revision: u64,
    eviction_order_dirty: bool,
}

/// Lazily maintained `key -> positions` index over one cache vector.
///
/// Appending keeps the index current. Anything which reorders or removes entries (eviction
/// sorting, `retain`) calls [`Self::invalidate`]; the next lookup rebuilds it in one pass. A
/// burst of lookups, such as preparing every summary family for every tile of a horizon, is
/// therefore one rebuild plus constant-time probes instead of a scan per probe.
#[derive(Debug)]
struct CachePositionIndex<K> {
    positions: HashMap<K, Vec<usize>>,
    valid: bool,
}

impl<K: Copy + Eq + std::hash::Hash> CachePositionIndex<K> {
    fn new() -> Self {
        Self {
            positions: HashMap::new(),
            valid: true,
        }
    }

    fn invalidate(&mut self) {
        self.valid = false;
    }

    /// Positions of every entry stored under `key`, in vector order.
    fn positions<T>(&mut self, entries: &[T], key_of: impl Fn(&T) -> K, key: K) -> &[usize] {
        if !self.valid {
            self.positions.clear();
            for (position, entry) in entries.iter().enumerate() {
                self.positions
                    .entry(key_of(entry))
                    .or_default()
                    .push(position);
            }
            self.valid = true;
        }
        self.positions.get(&key).map_or(&[], Vec::as_slice)
    }

    /// Records an entry appended at `position`.
    fn appended(&mut self, key: K, position: usize) {
        if self.valid {
            self.positions.entry(key).or_default().push(position);
        }
    }
}

/// Cache entries set aside during one eviction pass because they back the published authority.
#[derive(Default)]
struct ProtectedCacheEntries {
    acquisitions: Vec<CachedAcquisition>,
    chunks: Vec<CachedEvidenceChunk>,
    depth_summaries: Vec<CachedDepthSummary>,
    allele_summaries: Vec<CachedAlleleSummary>,
    junction_summaries: Vec<CachedJunctionSummary>,
    pair_summaries: Vec<CachedPairSummary>,
    modification_summaries: Vec<CachedModificationSummary>,
    filter_summaries: Vec<CachedFilterSummary>,
    row_atlases: Vec<CachedRowAtlas>,
}

#[derive(Debug)]
struct CachedAcquisition {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    acquisition: Arc<EvidenceAcquisition>,
}

#[derive(Debug)]
struct CachedEvidenceChunk {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    /// Conservative genomic span calculated once at admission. `None` means the physical source
    /// unit crossed contigs or contained no positioned record and cannot be classified as
    /// disjoint without reopening its complete batch.
    interval: Option<GenomicInterval>,
    chunk: Arc<EvidenceChunk>,
}

#[derive(Debug)]
struct CachedDepthSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactDepthSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactDepthSummaryTile>,
}

#[derive(Debug)]
struct CachedAlleleSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactAlleleSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactAlleleSummaryTile>,
}

#[derive(Debug)]
struct CachedJunctionSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactJunctionSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactJunctionSummaryTile>,
}

#[derive(Debug)]
struct CachedPairSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactPairSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactPairSummaryTile>,
}

#[derive(Debug)]
struct CachedModificationSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactModificationSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactModificationSummaryTile>,
}

#[derive(Debug)]
struct CachedFilterSummary {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    key: ExactFilterSummaryKey,
    view_filter: crate::ContractFingerprint,
    summary: Arc<ExactFilterSummaryTile>,
}

#[derive(Debug)]
struct CachedRowAtlas {
    residency: AlignmentCacheResidencyClass,
    admitted_revision: u64,
    atlas: Arc<AlignmentRowAtlas>,
}

/// One reloadable evidence-cache owner. Variant order is the tie-break policy after LRU age:
/// derived products yield before decoded source material when several disjoint entries were
/// demoted to the same return-cache revision.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EvidenceCacheLocation {
    RowAtlas,
    DepthSummary,
    AlleleSummary,
    JunctionSummary,
    PairSummary,
    ModificationSummary,
    FilterSummary,
    Chunk,
    Acquisition,
}

impl EvidenceStore {
    #[must_use]
    pub fn new(
        source_revision: AlignmentSourceRevision,
        record_contract: crate::ContractFingerprint,
        ledger: AllocationLedger,
    ) -> Self {
        Self {
            source_revision,
            record_contract,
            ledger,
            chunks: Vec::new(),
            acquisitions: Vec::new(),
            depth_summaries: Vec::new(),
            allele_summaries: Vec::new(),
            junction_summaries: Vec::new(),
            pair_summaries: Vec::new(),
            modification_summaries: Vec::new(),
            filter_summaries: Vec::new(),
            row_atlases: Vec::new(),
            chunk_index: CachePositionIndex::new(),
            depth_index: CachePositionIndex::new(),
            allele_index: CachePositionIndex::new(),
            junction_index: CachePositionIndex::new(),
            pair_index: CachePositionIndex::new(),
            modification_index: CachePositionIndex::new(),
            filter_index: CachePositionIndex::new(),
            current: None,
            next_revision: 1,
            eviction_order_dirty: false,
        }
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
    }

    /// Exact distinct byte inventory for aggregate cache policy and diagnostics. Shared chunk
    /// allocations referenced by acquisitions and chunk lookup entries are counted once.
    pub fn cache_residency_inventory(
        &self,
    ) -> Result<AlignmentCacheResidencyInventory, EvidenceStoreError> {
        self.residency_inventory()
    }

    #[must_use]
    pub const fn record_contract(&self) -> crate::ContractFingerprint {
        self.record_contract
    }

    /// Allocation-free completeness probe used by the scene coordinator before attempting a
    /// candidate build. It evaluates the union of compatible transaction certificates; cached
    /// physical chunks alone never imply coverage.
    #[must_use]
    pub fn cached_covers(&self, target: GenomicInterval, payload: &PayloadContract) -> bool {
        if self.current.as_ref().is_some_and(|current| {
            current.root.payload().satisfies(payload)
                && current.root.eligible_coverage().covers(target)
        }) {
            return true;
        }
        let mut cursor = target.start();
        loop {
            let next = self
                .acquisitions
                .iter()
                .filter(|cached| {
                    let interval = cached.acquisition.coverage().interval();
                    interval.contig() == target.contig()
                        && interval.start() <= cursor
                        && interval.end() > cursor
                })
                .filter(|cached| {
                    cached
                        .acquisition
                        .chunks()
                        .iter()
                        .all(|chunk| chunk_satisfies_payload(chunk, payload))
                })
                .map(|cached| cached.acquisition.coverage().interval().end())
                .max()
                .unwrap_or(cursor);
            if next == cursor {
                return false;
            }
            cursor = next;
            if cursor >= target.end() {
                return true;
            }
        }
    }

    /// Returns one contiguous uncovered portion of `target`. Direction selects which edge is
    /// acquired first when a zoom exposes gaps on both sides. Only complete compatible
    /// acquisition certificates participate; salvaged chunks cannot manufacture coverage.
    #[must_use]
    pub fn cached_missing_interval(
        &self,
        target: GenomicInterval,
        payload: &PayloadContract,
        prefer_right: bool,
    ) -> Option<GenomicInterval> {
        if self.current.as_ref().is_some_and(|current| {
            current.root.payload().satisfies(payload)
                && current.root.eligible_coverage().covers(target)
        }) {
            return None;
        }
        if prefer_right {
            let mut cursor = target.end();
            loop {
                let containing_start = self
                    .acquisitions
                    .iter()
                    .filter(|cached| acquisition_satisfies(cached, target.contig(), payload))
                    .map(|cached| cached.acquisition.coverage().interval())
                    .filter(|interval| interval.start() < cursor && interval.end() >= cursor)
                    .map(|interval| interval.start())
                    .min();
                if let Some(start) = containing_start {
                    cursor = cursor.min(start);
                    if cursor <= target.start() {
                        return None;
                    }
                    continue;
                }
                let gap_start = self
                    .acquisitions
                    .iter()
                    .filter(|cached| acquisition_satisfies(cached, target.contig(), payload))
                    .map(|cached| cached.acquisition.coverage().interval())
                    .filter(|interval| interval.end() < cursor)
                    .map(|interval| interval.end())
                    .max()
                    .unwrap_or(target.start())
                    .max(target.start());
                return GenomicInterval::new(target.contig(), gap_start, cursor).ok();
            }
        }

        let mut cursor = target.start();
        loop {
            let containing_end = self
                .acquisitions
                .iter()
                .filter(|cached| acquisition_satisfies(cached, target.contig(), payload))
                .map(|cached| cached.acquisition.coverage().interval())
                .filter(|interval| interval.start() <= cursor && interval.end() > cursor)
                .map(|interval| interval.end())
                .max();
            if let Some(end) = containing_end {
                cursor = cursor.max(end);
                if cursor >= target.end() {
                    return None;
                }
                continue;
            }
            let gap_end = self
                .acquisitions
                .iter()
                .filter(|cached| acquisition_satisfies(cached, target.contig(), payload))
                .map(|cached| cached.acquisition.coverage().interval())
                .filter(|interval| interval.start() > cursor)
                .map(|interval| interval.start())
                .min()
                .unwrap_or(target.end())
                .min(target.end());
            return GenomicInterval::new(target.contig(), cursor, gap_end).ok();
        }
    }

    pub fn admit(
        &mut self,
        acquisition: Arc<EvidenceAcquisition>,
    ) -> Result<u64, EvidenceStoreError> {
        if acquisition.source_revision() != self.source_revision {
            return Err(EvidenceStoreError::SourceRevisionMismatch);
        }
        if acquisition.record_contract() != self.record_contract {
            return Err(EvidenceStoreError::RecordContractMismatch);
        }
        for chunk in acquisition.chunks() {
            self.admit_chunk(Arc::clone(chunk))?;
        }
        let revision = self.take_revision()?;
        self.acquisitions.push(CachedAcquisition {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            acquisition,
        });
        Ok(revision)
    }

    /// Salvages one fully decoded physical source unit before transaction completeness is known.
    /// A later complete acquisition may reuse this allocation; a cancelled intent never promotes
    /// the chunk itself into a false completeness certificate.
    pub fn admit_chunk(
        &mut self,
        chunk: Arc<EvidenceChunk>,
    ) -> Result<Arc<EvidenceChunk>, EvidenceStoreError> {
        if chunk.chunk().source_revision() != self.source_revision {
            return Err(EvidenceStoreError::SourceRevisionMismatch);
        }
        if chunk.chunk().record_contract() != self.record_contract {
            return Err(EvidenceStoreError::RecordContractMismatch);
        }
        let revision = self.take_revision()?;
        let matching = self
            .chunk_index
            .positions(
                &self.chunks,
                |cached| cached.chunk.chunk().id(),
                chunk.chunk().id(),
            )
            .to_vec();
        for index in matching.iter().copied() {
            if self.chunks[index].chunk.chunk().handles() != chunk.chunk().handles() {
                // Indexed transactions may enter or leave one physical owner unit at different
                // record boundaries. Completeness remains transaction-scoped, so retain the
                // disjoint immutable fragment rather than pretending either fragment is the
                // complete owner population.
                continue;
            }
            if payload_satisfies(&self.chunks[index].chunk, &chunk) {
                let cached = &mut self.chunks[index];
                cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
                cached.admitted_revision = revision;
                self.ledger
                    .touch(cached.chunk.core_allocation().id(), revision)?;
                self.ledger
                    .touch(cached.chunk.overlap_allocation().id(), revision)?;
                if let Some(payload) = cached.chunk.payload() {
                    self.ledger.touch(payload.allocation().id(), revision)?;
                }
                return Ok(Arc::clone(&cached.chunk));
            }
        }
        if let Some(index) = matching.iter().copied().find(|index| {
            self.chunks[*index].chunk.chunk().handles() == chunk.chunk().handles()
                && payload_satisfies(&chunk, &self.chunks[*index].chunk)
        }) {
            let interval = evidence_chunk_interval(&chunk);
            self.chunks[index] = CachedEvidenceChunk {
                residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
                admitted_revision: revision,
                interval,
                chunk: Arc::clone(&chunk),
            };
            return Ok(chunk);
        }
        let interval = evidence_chunk_interval(&chunk);
        self.chunk_index
            .appended(chunk.chunk().id(), self.chunks.len());
        self.chunks.push(CachedEvidenceChunk {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            interval,
            chunk: Arc::clone(&chunk),
        });
        Ok(chunk)
    }

    #[must_use]
    pub fn cached_chunk_fragment(
        &self,
        id: SourceChunkId,
        handles: &[crate::AlignmentHandle],
        required_payload: &PayloadContract,
    ) -> Option<&Arc<EvidenceChunk>> {
        self.chunks
            .iter()
            .filter(|cached| cached.chunk.chunk().id() == id)
            .filter(|cached| cached.chunk.chunk().handles() == handles)
            .find(|cached| chunk_satisfies_payload(&cached.chunk, required_payload))
            .map(|cached| &cached.chunk)
    }

    #[must_use]
    pub fn ledger(&self) -> AllocationLedger {
        self.ledger.clone()
    }

    pub fn prepare(
        &mut self,
        target: GenomicInterval,
        payload: PayloadContract,
        reservation_kind: ReservationKind,
    ) -> Result<EvidenceCandidate, EvidenceStoreError> {
        let retained_filtered = self.current.as_ref().and_then(|current| {
            (current.root.view_filter().fingerprint() != crate::ContractFingerprint::new(0)
                && current.root.payload().satisfies(&payload)
                && current.root.eligible_coverage().covers(target))
            .then(|| Arc::clone(&current.root))
        });
        if let Some(current) = retained_filtered {
            let revision_number = self.take_revision()?;
            let revision = EvidenceRevision::new(RevisionFingerprint::new(0, revision_number));
            let root = current.derive_unfiltered(
                revision,
                target,
                &self.ledger,
                reservation_kind,
                revision_number,
            )?;
            let pins = root.pin()?;
            return Ok(EvidenceCandidate {
                target,
                root,
                _pins: pins,
            });
        }
        let no_payload_required = payload.sequence() == crate::CapabilityRequirement::NotRequired
            && payload.qualities() == crate::CapabilityRequirement::NotRequired
            && !payload.all_auxiliary_tags()
            && payload.auxiliary_tags().is_empty()
            && payload.decode_reference().is_none();
        let selected = self
            .acquisitions
            .iter()
            .filter(|cached| cached.acquisition.coverage().interval().overlaps(target))
            .filter(|cached| {
                no_payload_required
                    || cached.acquisition.chunks().iter().all(|chunk| {
                        chunk
                            .payload()
                            .is_some_and(|available| available.contract().satisfies(&payload))
                    })
            })
            .map(|cached| Arc::clone(&cached.acquisition))
            .collect::<Vec<_>>();
        let expected_chunks = selected.iter().fold(0_usize, |total, cached| {
            total.saturating_add(cached.chunks().len())
        });
        let revision_number = self.take_revision()?;
        let revision = EvidenceRevision::new(RevisionFingerprint::new(0, revision_number));
        let mut builder = EvidenceRootBuilder::new(
            revision,
            self.source_revision,
            self.record_contract,
            payload,
            expected_chunks,
            selected.len(),
            &self.ledger,
            reservation_kind,
        )?;
        for acquisition in selected {
            builder.insert_acquisition(&acquisition)?;
        }
        let root = builder.build(revision_number)?;
        if !root.coverage().covers(target) {
            return Err(EvidenceStoreError::UncoveredTarget(target));
        }
        let pins = root.pin()?;
        Ok(EvidenceCandidate {
            target,
            root,
            _pins: pins,
        })
    }

    /// Derives a filtered immutable evidence root while retaining the candidate's canonical
    /// chunks. The broad acquisition remains cached, so clearing or changing the filter does not
    /// require source I/O. The input candidate is never published independently of its filtered
    /// summaries and row atlas.
    pub fn prepare_filtered(
        &mut self,
        candidate: EvidenceCandidate,
        target: GenomicInterval,
        contract: &AlignmentViewFilterContract,
        evaluation_fingerprint: crate::ContractFingerprint,
        reference: Option<&rgv_hts::ReferenceSlice>,
        reservation_kind: ReservationKind,
    ) -> Result<EvidenceCandidate, EvidenceStoreError> {
        if contract.is_empty() {
            return Ok(candidate);
        }
        let evaluation = evaluate_alignment_filter(candidate.root(), target, contract, reference)?;
        let (handles, predicate_matches, eligible_records) = evaluation.into_parts();
        let revision_number = self.take_revision()?;
        let revision = EvidenceRevision::new(RevisionFingerprint::new(0, revision_number));
        let root = candidate.root.derive_filtered(
            revision,
            target,
            evaluation_fingerprint,
            handles,
            predicate_matches,
            eligible_records,
            &self.ledger,
            reservation_kind,
            revision_number,
        )?;
        let pins = root.pin()?;
        Ok(EvidenceCandidate {
            target,
            root,
            _pins: pins,
        })
    }

    pub fn publish(&mut self, candidate: EvidenceCandidate) -> Arc<EvidenceRoot> {
        let root = Arc::clone(&candidate.root);
        self.current = Some(PublishedEvidence {
            target: candidate.target,
            root: candidate.root,
            _pins: candidate._pins,
            summaries: None,
            row_atlas: None,
        });
        root
    }

    /// Atomically publishes evidence and all required exact summary families from the same
    /// candidate revision.
    pub fn publish_with_summaries(
        &mut self,
        evidence: EvidenceCandidate,
        summaries: ExactSummaryCandidate,
    ) -> Result<(Arc<EvidenceRoot>, Arc<ExactSummaryRoot>), EvidenceStoreError> {
        if summaries.root.evidence_revision() != evidence.root.revision()
            || !summaries
                .root
                .prepared_interval()
                .contains_interval(evidence.target)
            || !summaries
                .root
                .prepared_interval()
                .contains_interval(summaries.target)
        {
            return Err(EvidenceStoreError::SummaryEvidenceMismatch);
        }
        let evidence_root = Arc::clone(&evidence.root);
        let summary_root = Arc::clone(&summaries.root);
        self.current = Some(PublishedEvidence {
            target: evidence.target,
            root: evidence.root,
            _pins: evidence._pins,
            summaries: Some(PublishedSummaries {
                target: summaries.target,
                root: summaries.root,
                _pins: summaries._pins,
            }),
            row_atlas: None,
        });
        Ok((evidence_root, summary_root))
    }

    /// Atomically publishes evidence, exact summaries, and the immutable row authority. A cached
    /// atlas may have been derived from an older compatible evidence publication because its
    /// physical handles are source-stable; it must cover the full candidate target under the same
    /// source and record contract.
    pub fn publish_complete_authority(
        &mut self,
        evidence: EvidenceCandidate,
        summaries: ExactSummaryCandidate,
        row_atlas: RowAtlasCandidate,
    ) -> Result<
        (
            Arc<EvidenceRoot>,
            Arc<ExactSummaryRoot>,
            Arc<AlignmentRowAtlas>,
        ),
        EvidenceStoreError,
    > {
        if summaries.root.evidence_revision() != evidence.root.revision()
            || !summaries
                .root
                .prepared_interval()
                .contains_interval(evidence.target)
            || !summaries
                .root
                .prepared_interval()
                .contains_interval(summaries.target)
        {
            return Err(EvidenceStoreError::SummaryEvidenceMismatch);
        }
        if !row_atlas
            .root
            .shares_stable_record_authority(&evidence.root)
            || !row_atlas.root.interval().contains_interval(evidence.target)
            || !row_atlas
                .root
                .interval()
                .contains_interval(row_atlas.target)
        {
            return Err(EvidenceStoreError::RowAtlasEvidenceMismatch);
        }
        let evidence_root = Arc::clone(&evidence.root);
        let summary_root = Arc::clone(&summaries.root);
        let atlas_root = Arc::clone(&row_atlas.root);
        self.current = Some(PublishedEvidence {
            target: evidence.target,
            root: evidence.root,
            _pins: evidence._pins,
            summaries: Some(PublishedSummaries {
                target: summaries.target,
                root: summaries.root,
                _pins: summaries._pins,
            }),
            row_atlas: Some(PublishedRowAtlas {
                target: row_atlas.target,
                root: row_atlas.root,
                _pins: row_atlas._pins,
            }),
        });
        Ok((evidence_root, summary_root, atlas_root))
    }

    /// Returns or builds an exact canonical depth tile from a complete immutable evidence root.
    /// Compatible tiles survive evidence-root extension because their key excludes publication
    /// revision while retaining source and record-contract identity.
    #[allow(clippy::too_many_arguments)]
    pub fn exact_depth_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: DepthSummaryContract,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactDepthSummaryTile>, EvidenceStoreError> {
        if evidence.source_revision() != self.source_revision {
            return Err(EvidenceStoreError::SourceRevisionMismatch);
        }
        if evidence.record_contract() != self.record_contract {
            return Err(EvidenceStoreError::RecordContractMismatch);
        }
        let key =
            ExactDepthSummaryKey::new(self.source_revision, self.record_contract, contract, tile);
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .depth_index
            .positions(
                &self.depth_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) = cached_position.map(|position| &mut self.depth_summaries[position]) {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactDepthSummaryTile::build(
            evidence,
            tile,
            contig_length,
            contract,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.depth_index
            .appended((key, view_filter), self.depth_summaries.len());
        self.depth_summaries.push(CachedDepthSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exact_allele_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: AlleleSummaryContract,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactAlleleSummaryTile>, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let key =
            ExactAlleleSummaryKey::new(self.source_revision, self.record_contract, contract, tile);
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .allele_index
            .positions(
                &self.allele_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) = cached_position.map(|position| &mut self.allele_summaries[position]) {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactAlleleSummaryTile::build(
            evidence,
            tile,
            contig_length,
            contract,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.allele_index
            .appended((key, view_filter), self.allele_summaries.len());
        self.allele_summaries.push(CachedAlleleSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exact_junction_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: JunctionSummaryContract,
        normalization_mapped_records: Option<u64>,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactJunctionSummaryTile>, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let key = ExactJunctionSummaryKey::new(
            self.source_revision,
            self.record_contract,
            contract,
            normalization_mapped_records,
            tile,
        );
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .junction_index
            .positions(
                &self.junction_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) = cached_position.map(|position| &mut self.junction_summaries[position])
        {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactJunctionSummaryTile::build(
            evidence,
            tile,
            contig_length,
            contract,
            normalization_mapped_records,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.junction_index
            .appended((key, view_filter), self.junction_summaries.len());
        self.junction_summaries.push(CachedJunctionSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exact_pair_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: PairSummaryContract,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactPairSummaryTile>, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let key =
            ExactPairSummaryKey::new(self.source_revision, self.record_contract, contract, tile);
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .pair_index
            .positions(
                &self.pair_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) = cached_position.map(|position| &mut self.pair_summaries[position]) {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactPairSummaryTile::build(
            evidence,
            tile,
            contig_length,
            contract,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.pair_index
            .appended((key, view_filter), self.pair_summaries.len());
        self.pair_summaries.push(CachedPairSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exact_modification_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        contract: ModificationSummaryContract,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactModificationSummaryTile>, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let key = ExactModificationSummaryKey::new(
            self.source_revision,
            self.record_contract,
            contract,
            tile,
        );
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .modification_index
            .positions(
                &self.modification_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) =
            cached_position.map(|position| &mut self.modification_summaries[position])
        {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactModificationSummaryTile::build(
            evidence,
            tile,
            contig_length,
            contract,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.modification_index
            .appended((key, view_filter), self.modification_summaries.len());
        self.modification_summaries.push(CachedModificationSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn exact_filter_summary(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        tile: CanonicalGenomicTile,
        contig_length: u64,
        provenance_scope: GenomicInterval,
        contract: crate::FilterSummaryContract,
        reservation_kind: ReservationKind,
    ) -> Result<Arc<ExactFilterSummaryTile>, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let key = ExactFilterSummaryKey::new(
            self.source_revision,
            crate::filter_summary::filter_provenance_fingerprint(evidence, provenance_scope),
            self.record_contract,
            contract,
            provenance_scope,
            tile,
        );
        let revision = self.take_revision()?;
        let view_filter = evidence.view_filter().fingerprint();
        let cached_position = self
            .filter_index
            .positions(
                &self.filter_summaries,
                |cached| (cached.key, cached.view_filter),
                (key, view_filter),
            )
            .first()
            .copied();
        if let Some(cached) = cached_position.map(|position| &mut self.filter_summaries[position]) {
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision;
            self.ledger
                .touch(cached.summary.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.summary));
        }
        let summary = ExactFilterSummaryTile::build(
            evidence,
            tile,
            contig_length,
            provenance_scope,
            contract,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        debug_assert_eq!(summary.key(), key);
        self.filter_index
            .appended((key, view_filter), self.filter_summaries.len());
        self.filter_summaries.push(CachedFilterSummary {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision,
            key,
            view_filter,
            summary: Arc::clone(&summary),
        });
        Ok(summary)
    }

    /// Prepares every exact summary family required by one factorised summary contract. The
    /// canonical horizon is expanded to whole measurement-selected tiles; the evidence root must
    /// certify that full horizon before any summary candidate can publish.
    #[allow(clippy::too_many_arguments)]
    pub fn prepare_exact_summaries(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        target: GenomicInterval,
        tile_bases: NonZeroU64,
        contig_length: u64,
        contract: SummaryContract,
        normalization_mapped_records: Option<u64>,
        reservation_kind: ReservationKind,
    ) -> Result<ExactSummaryCandidate, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let width = tile_bases.get();
        let first_coordinate = target.start() / width;
        let last_coordinate = target.end().saturating_sub(1) / width;
        let tile_count = usize::try_from(
            last_coordinate
                .saturating_sub(first_coordinate)
                .saturating_add(1),
        )
        .unwrap_or(usize::MAX);
        let family_count = 4_usize
            .saturating_add(usize::from(
                contract.allele_detail != crate::AlleleSummaryDetail::None,
            ))
            .saturating_add(usize::from(contract.collect_modifications));
        let staging_bytes = tile_count
            .saturating_mul(size_of::<CanonicalGenomicTile>())
            .saturating_add(
                tile_count
                    .saturating_mul(family_count)
                    .saturating_mul(size_of::<Arc<ExactDepthSummaryTile>>())
                    .saturating_mul(4),
            );
        let _staging = self.ledger.reserve_transient(
            TransientResourceClass::ExactSummaryBuilder,
            staging_bytes,
            reservation_kind,
        )?;
        let tiles = CanonicalGenomicTile::covering(target, tile_bases, contig_length)
            .map_err(crate::ExactDepthSummaryError::from)?;
        let first_interval = tiles
            .first()
            .expect("a non-empty target has at least one canonical tile")
            .interval(contig_length)
            .map_err(crate::ExactDepthSummaryError::from)?;
        let last_interval = tiles
            .last()
            .expect("a non-empty target has at least one canonical tile")
            .interval(contig_length)
            .map_err(crate::ExactDepthSummaryError::from)?;
        let provenance_scope = GenomicInterval::new(
            first_interval.contig(),
            first_interval.start(),
            last_interval.end(),
        )
        .expect("ordered canonical tiles form a non-empty interval");
        let mut depth = Vec::with_capacity(tiles.len());
        let mut alleles = Vec::with_capacity(tiles.len().saturating_mul(usize::from(
            contract.allele_detail != crate::AlleleSummaryDetail::None,
        )));
        let mut junctions = Vec::with_capacity(tiles.len());
        let mut pairs = Vec::with_capacity(tiles.len());
        let mut filters = Vec::with_capacity(tiles.len());
        let mut modifications = Vec::with_capacity(
            tiles
                .len()
                .saturating_mul(usize::from(contract.collect_modifications)),
        );
        for tile in tiles {
            depth.push(self.exact_depth_summary(
                evidence,
                tile,
                contig_length,
                contract.depth(),
                reservation_kind,
            )?);
            if contract.allele_detail != crate::AlleleSummaryDetail::None {
                alleles.push(self.exact_allele_summary(
                    evidence,
                    tile,
                    contig_length,
                    contract.alleles(),
                    reservation_kind,
                )?);
            }
            junctions.push(self.exact_junction_summary(
                evidence,
                tile,
                contig_length,
                contract.junctions(),
                normalization_mapped_records,
                reservation_kind,
            )?);
            pairs.push(self.exact_pair_summary(
                evidence,
                tile,
                contig_length,
                contract.pairs(),
                reservation_kind,
            )?);
            filters.push(self.exact_filter_summary(
                evidence,
                tile,
                contig_length,
                provenance_scope,
                contract.filters(),
                reservation_kind,
            )?);
            if let Some(modification_contract) = contract.modifications() {
                modifications.push(self.exact_modification_summary(
                    evidence,
                    tile,
                    contig_length,
                    modification_contract,
                    reservation_kind,
                )?);
            }
        }
        let revision_number = self.take_revision()?;
        let revision = SummaryRevision::new(RevisionFingerprint::new(0, revision_number));
        let root = ExactSummaryRoot::build(
            revision,
            evidence.revision(),
            self.source_revision,
            self.record_contract,
            contract,
            target,
            depth,
            alleles,
            junctions,
            pairs,
            filters,
            modifications,
            &self.ledger,
            reservation_kind,
            revision_number,
        )?;
        let pins = root.pin()?;
        Ok(ExactSummaryCandidate {
            target,
            root,
            _pins: pins,
        })
    }

    /// Reuses a containing immutable atlas for camera containment, incrementally extends the
    /// largest compatible contained atlas for edge acquisition, or builds a clean canonical root.
    /// No exact-camera row projection is registered in any of these paths.
    pub fn prepare_row_atlas(
        &mut self,
        evidence: &Arc<EvidenceRoot>,
        target: GenomicInterval,
        layout_contract: LayoutContract,
        reservation_kind: ReservationKind,
    ) -> Result<RowAtlasCandidate, EvidenceStoreError> {
        self.validate_summary_evidence(evidence)?;
        let revision_number = self.take_revision()?;
        let current_atlas = self.current_row_atlas().cloned();

        let containing = self
            .row_atlases
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                atlas_compatible(&cached.atlas, evidence, layout_contract)
                    && cached.atlas.interval().contains_interval(target)
            })
            // Prefer the published atlas, then the widest, then the newest. Choosing the
            // smallest container flipped a contained camera back to the superseded atlas (and
            // its row space and page keys) straight after an extension had published a wider one.
            .max_by_key(|(_, cached)| {
                (
                    current_atlas
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &cached.atlas)),
                    cached.atlas.interval().len(),
                    cached.admitted_revision,
                )
            })
            .map(|(index, _)| index);
        if let Some(index) = containing {
            let cached = &mut self.row_atlases[index];
            cached.residency = AlignmentCacheResidencyClass::ActiveNearBuffer;
            cached.admitted_revision = revision_number;
            cached.atlas.touch(&self.ledger, revision_number)?;
            let pins = cached.atlas.pin()?;
            return Ok(RowAtlasCandidate {
                target,
                root: Arc::clone(&cached.atlas),
                _pins: pins,
            });
        }

        let extendable = self
            .row_atlases
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                atlas_compatible(&cached.atlas, evidence, layout_contract)
                    && target.contains_interval(cached.atlas.interval())
            })
            .max_by_key(|(_, cached)| cached.atlas.interval().len())
            .map(|(index, _)| index);
        let revision = RowAtlasRevision::new(RevisionFingerprint::new(0, revision_number));
        let atlas = if let Some(index) = extendable {
            self.row_atlases[index].atlas.extend(
                revision,
                evidence,
                target,
                &self.ledger,
                reservation_kind,
                revision_number,
            )?
        } else {
            AlignmentRowAtlas::build(
                revision,
                evidence,
                target,
                layout_contract,
                &self.ledger,
                reservation_kind,
                revision_number,
            )?
        };
        let pins = atlas.pin()?;
        self.row_atlases.push(CachedRowAtlas {
            residency: AlignmentCacheResidencyClass::ActiveNearBuffer,
            admitted_revision: revision_number,
            atlas: Arc::clone(&atlas),
        });
        Ok(RowAtlasCandidate {
            target,
            root: atlas,
            _pins: pins,
        })
    }

    #[must_use]
    pub fn current(&self) -> Option<&Arc<EvidenceRoot>> {
        self.current.as_ref().map(|current| &current.root)
    }

    #[must_use]
    pub fn current_target(&self) -> Option<GenomicInterval> {
        self.current.as_ref().map(|current| current.target)
    }

    #[must_use]
    pub fn current_summaries(&self) -> Option<&Arc<ExactSummaryRoot>> {
        self.current
            .as_ref()
            .and_then(|current| current.summaries.as_ref())
            .map(|summaries| &summaries.root)
    }

    #[must_use]
    pub fn current_summary_target(&self) -> Option<GenomicInterval> {
        self.current
            .as_ref()
            .and_then(|current| current.summaries.as_ref())
            .map(|summaries| summaries.target)
    }

    #[must_use]
    pub fn current_row_atlas(&self) -> Option<&Arc<AlignmentRowAtlas>> {
        self.current
            .as_ref()
            .and_then(|current| current.row_atlas.as_ref())
            .map(|atlas| &atlas.root)
    }

    #[must_use]
    pub fn current_row_atlas_target(&self) -> Option<GenomicInterval> {
        self.current
            .as_ref()
            .and_then(|current| current.row_atlas.as_ref())
            .map(|atlas| atlas.target)
    }

    #[must_use]
    pub fn acquisition_count(&self) -> usize {
        self.acquisitions.len()
    }

    #[must_use]
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    #[must_use]
    pub fn depth_summary_count(&self) -> usize {
        self.depth_summaries.len()
    }

    #[must_use]
    pub fn exact_summary_count(&self) -> usize {
        self.depth_summaries
            .len()
            .saturating_add(self.allele_summaries.len())
            .saturating_add(self.junction_summaries.len())
            .saturating_add(self.pair_summaries.len())
            .saturating_add(self.filter_summaries.len())
            .saturating_add(self.modification_summaries.len())
    }

    #[must_use]
    pub fn row_atlas_count(&self) -> usize {
        self.row_atlases.len()
    }

    /// Drops cache ownership outside a disjoint target. Current/candidate roots keep their exact
    /// allocations charged until their final leases disappear.
    pub fn discard_disjoint_cache(&mut self, target: GenomicInterval) -> usize {
        let before_acquisitions = self.acquisitions.len();
        let before_chunks = self.chunks.len();
        let before_summaries = self.exact_summary_count();
        let before_atlases = self.row_atlases.len();
        self.depth_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.allele_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.junction_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.pair_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.filter_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.modification_summaries
            .retain(|cached| cached.summary.tile().overlaps(target));
        self.row_atlases
            .retain(|cached| cached.atlas.interval().overlaps(target));
        self.chunks
            .retain(|cached| evidence_chunk_overlaps(&cached.chunk, target));
        self.acquisitions
            .retain(|cached| cached.acquisition.coverage().interval().overlaps(target));
        self.invalidate_cache_indices();
        before_acquisitions
            .saturating_sub(self.acquisitions.len())
            .saturating_add(before_chunks.saturating_sub(self.chunks.len()))
            .saturating_add(before_summaries.saturating_sub(self.exact_summary_count()))
            .saturating_add(before_atlases.saturating_sub(self.row_atlases.len()))
    }

    /// Marks cache entries outside every active locus as explicit return-cache material. Their
    /// immutable values remain reusable, but they yield before active buffers when the shared
    /// ledger next needs headroom.
    pub fn demote_cache_outside(&mut self, active: &[GenomicInterval]) -> usize {
        let mut demoted = 0_usize;
        macro_rules! demote {
            ($entries:expr, $interval:expr) => {
                for cached in $entries {
                    if !active
                        .iter()
                        .any(|target| ($interval)(cached).overlaps(*target))
                        && cached.residency != AlignmentCacheResidencyClass::RecentLocusReturnCache
                    {
                        cached.residency = AlignmentCacheResidencyClass::RecentLocusReturnCache;
                        demoted = demoted.saturating_add(1);
                    }
                }
            };
        }
        demote!(&mut self.depth_summaries, |cached: &CachedDepthSummary| {
            cached.summary.tile()
        });
        demote!(
            &mut self.allele_summaries,
            |cached: &CachedAlleleSummary| cached.summary.tile()
        );
        demote!(
            &mut self.junction_summaries,
            |cached: &CachedJunctionSummary| cached.summary.tile()
        );
        demote!(&mut self.pair_summaries, |cached: &CachedPairSummary| {
            cached.summary.tile()
        });
        demote!(
            &mut self.filter_summaries,
            |cached: &CachedFilterSummary| cached.summary.tile()
        );
        demote!(
            &mut self.modification_summaries,
            |cached: &CachedModificationSummary| cached.summary.tile()
        );
        demote!(&mut self.row_atlases, |cached: &CachedRowAtlas| cached
            .atlas
            .interval());
        for cached in &mut self.chunks {
            if cached
                .interval
                .is_some_and(|interval| !active.iter().any(|target| interval.overlaps(*target)))
                && cached.residency != AlignmentCacheResidencyClass::RecentLocusReturnCache
            {
                cached.residency = AlignmentCacheResidencyClass::RecentLocusReturnCache;
                demoted = demoted.saturating_add(1);
            }
        }
        demote!(&mut self.acquisitions, |cached: &CachedAcquisition| cached
            .acquisition
            .coverage()
            .interval());
        if demoted > 0 {
            self.eviction_order_dirty = true;
        }
        demoted
    }

    /// Demotes only exact summary tiles outside the live source horizons. This narrower operation
    /// is used when a zoom or overlapping pan contracts the derived working set: decoded source
    /// evidence remains a useful browsing buffer, while dense per-base summary variants outside
    /// that buffer should yield first during a foreground replacement.
    pub fn demote_exact_summary_cache_outside(&mut self, active: &[GenomicInterval]) -> usize {
        let mut demoted = 0_usize;
        macro_rules! demote {
            ($entries:expr) => {
                for cached in $entries {
                    if !active
                        .iter()
                        .any(|target| cached.summary.tile().overlaps(*target))
                        && cached.residency != AlignmentCacheResidencyClass::RecentLocusReturnCache
                    {
                        cached.residency = AlignmentCacheResidencyClass::RecentLocusReturnCache;
                        demoted = demoted.saturating_add(1);
                    }
                }
            };
        }
        demote!(&mut self.depth_summaries);
        demote!(&mut self.allele_summaries);
        demote!(&mut self.junction_summaries);
        demote!(&mut self.pair_summaries);
        demote!(&mut self.filter_summaries);
        demote!(&mut self.modification_summaries);
        if demoted > 0 {
            self.eviction_order_dirty = true;
        }
        demoted
    }

    /// Reclassifies exact-summary lookup ownership from the immutable roots currently prepared
    /// for presentation. Contract variants from an earlier zoom, filter, or display mode are
    /// reproducible derivatives: they may remain useful as cold/return cache, but must not retain
    /// active-near priority merely because their canonical tile overlaps the current locus.
    ///
    /// Published snapshots keep independent pins, so lowering cache priority cannot invalidate
    /// visible data. It only lets cooperative reclaim relinquish obsolete summary ownership
    /// before decoded BAM evidence when the shared budget next needs headroom.
    pub fn reclassify_exact_summary_cache(
        &mut self,
        active: &[GenomicInterval],
        prepared: &[Arc<ExactSummaryRoot>],
    ) -> usize {
        let mut demanded = HashSet::<AllocationId>::new();
        for root in prepared {
            demanded.extend(root.depth_tiles().iter().map(|tile| tile.allocation().id()));
            demanded.extend(
                root.allele_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            demanded.extend(
                root.junction_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            demanded.extend(root.pair_tiles().iter().map(|tile| tile.allocation().id()));
            demanded.extend(
                root.filter_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            demanded.extend(
                root.modification_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
        }

        let classify = |current: AlignmentCacheResidencyClass,
                        allocation: AllocationId,
                        tile: CanonicalGenomicTile| {
            if demanded.contains(&allocation) {
                AlignmentCacheResidencyClass::ActiveNearBuffer
            } else if current == AlignmentCacheResidencyClass::DisposableSpeculation {
                current
            } else if active.iter().any(|interval| tile.overlaps(*interval)) {
                AlignmentCacheResidencyClass::ActiveLocusCold
            } else {
                AlignmentCacheResidencyClass::RecentLocusReturnCache
            }
        };
        let mut changed = 0_usize;
        macro_rules! reclassify {
            ($entries:expr) => {
                for cached in $entries {
                    let next = classify(
                        cached.residency,
                        cached.summary.allocation().id(),
                        cached.summary.tile(),
                    );
                    if next != cached.residency {
                        cached.residency = next;
                        changed = changed.saturating_add(1);
                    }
                }
            };
        }
        reclassify!(&mut self.depth_summaries);
        reclassify!(&mut self.allele_summaries);
        reclassify!(&mut self.junction_summaries);
        reclassify!(&mut self.pair_summaries);
        reclassify!(&mut self.filter_summaries);
        reclassify!(&mut self.modification_summaries);
        if changed > 0 {
            self.eviction_order_dirty = true;
        }
        changed
    }

    /// Applies a smaller budget and releases derived summaries before oldest source acquisitions.
    /// Pinned current evidence is never destroyed and remains visible as explicit pinned overage.
    pub fn reduce_budget(&mut self, budget: usize) -> Result<StorePressure, EvidenceStoreError> {
        self.ledger.set_budget(budget)?;
        self.evict_inactive_until(budget)
    }

    /// Releases reloadable inactive source/summary/layout cache ownership without changing the
    /// configured ledger policy. Current evidence remains protected by its published pins.
    pub fn evict_inactive_until(
        &mut self,
        target_charged: usize,
    ) -> Result<StorePressure, EvidenceStoreError> {
        self.evict_inactive_batch(target_charged, usize::MAX)
    }

    /// Releases a bounded number of reloadable evidence entries. Collections are ordered with
    /// their oldest entry at the tail so removal is `Vec::pop`, avoiding the repeated whole-vector
    /// shifts previously caused by `remove(0)` at dense loci.
    #[allow(clippy::too_many_lines)]
    pub fn evict_inactive_batch(
        &mut self,
        target_charged: usize,
        maximum_entries: usize,
    ) -> Result<StorePressure, EvidenceStoreError> {
        if self.eviction_order_dirty {
            self.acquisitions
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.chunks
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.depth_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.allele_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.junction_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.pair_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.filter_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.modification_summaries
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.row_atlases
                .sort_by_key(|cached| Reverse((cached.residency, cached.admitted_revision)));
            self.eviction_order_dirty = false;
            self.invalidate_cache_indices();
        }
        // Entries backing the published authority are set aside, not dropped: releasing the
        // cache's reference to them frees nothing while the published root pins them, and it
        // strips the lookup entries the next extension of the visible locus needs.
        let protected = if self.ledger.progress()?.charged > target_charged {
            self.published_allocations()
        } else {
            HashSet::new()
        };
        let mut held = ProtectedCacheEntries::default();
        let mut candidates_inspected = 0_usize;
        let mut evicted_acquisitions = 0;
        let mut evicted_chunks = 0;
        let mut evicted_exact_summaries = 0;
        let mut evicted_row_atlases = 0;
        let mut removed = 0_usize;
        while removed < maximum_entries && self.ledger.progress()?.charged > target_charged {
            let (candidate, inspected) = self.oldest_cache_entry();
            candidates_inspected = candidates_inspected.saturating_add(inspected);
            let Some(candidate) = candidate else {
                break;
            };
            // Pops the tail entry of one family. A protected entry moves to `held` and the loop
            // looks at the next-oldest candidate without spending the eviction quantum.
            macro_rules! relinquish {
                ($entries:ident, $evicted:ident, |$cached:ident| $protected:expr) => {{
                    let Some(entry) = self.$entries.pop() else {
                        break;
                    };
                    let $cached = &entry;
                    if $protected {
                        held.$entries.push(entry);
                        continue;
                    }
                    $evicted += 1;
                }};
            }
            match candidate {
                EvidenceCacheLocation::RowAtlas => {
                    relinquish!(row_atlases, evicted_row_atlases, |cached| protected
                        .contains(&cached.atlas.metadata_allocation().id()));
                }
                EvidenceCacheLocation::DepthSummary => {
                    relinquish!(depth_summaries, evicted_exact_summaries, |cached| protected
                        .contains(&cached.summary.allocation().id()));
                }
                EvidenceCacheLocation::AlleleSummary => {
                    relinquish!(allele_summaries, evicted_exact_summaries, |cached| {
                        protected.contains(&cached.summary.allocation().id())
                    });
                }
                EvidenceCacheLocation::JunctionSummary => {
                    relinquish!(junction_summaries, evicted_exact_summaries, |cached| {
                        protected.contains(&cached.summary.allocation().id())
                    });
                }
                EvidenceCacheLocation::PairSummary => {
                    relinquish!(pair_summaries, evicted_exact_summaries, |cached| protected
                        .contains(&cached.summary.allocation().id()));
                }
                EvidenceCacheLocation::ModificationSummary => {
                    relinquish!(modification_summaries, evicted_exact_summaries, |cached| {
                        protected.contains(&cached.summary.allocation().id())
                    });
                }
                EvidenceCacheLocation::FilterSummary => {
                    relinquish!(filter_summaries, evicted_exact_summaries, |cached| {
                        protected.contains(&cached.summary.allocation().id())
                    });
                }
                EvidenceCacheLocation::Chunk => {
                    relinquish!(chunks, evicted_chunks, |cached| protected
                        .contains(&cached.chunk.core_allocation().id()));
                }
                // A certificate is what lets the visible locus be extended from cache; it backs
                // the published root as soon as any of its chunks does.
                EvidenceCacheLocation::Acquisition => {
                    relinquish!(acquisitions, evicted_acquisitions, |cached| cached
                        .acquisition
                        .chunks()
                        .iter()
                        .any(|chunk| protected.contains(&chunk.core_allocation().id())));
                }
            }
            removed = removed.saturating_add(1);
        }
        // Held entries left their tails oldest-first, so pushing them back newest-first restores
        // the exact eviction order.
        macro_rules! restore {
            ($($entries:ident),+) => {
                $(self.$entries.extend(held.$entries.into_iter().rev());)+
            };
        }
        restore!(
            acquisitions,
            chunks,
            depth_summaries,
            allele_summaries,
            junction_summaries,
            pair_summaries,
            modification_summaries,
            filter_summaries,
            row_atlases
        );
        if removed > 0 {
            self.invalidate_cache_indices();
        }
        let progress = self.ledger.progress()?;
        let reclaim_complete = progress.charged <= target_charged || removed < maximum_entries;
        let residency = if reclaim_complete && maximum_entries == usize::MAX {
            self.residency_inventory()?
        } else {
            AlignmentCacheResidencyInventory::default()
        };
        Ok(StorePressure {
            evicted_acquisitions,
            evicted_chunks,
            evicted_exact_summaries,
            evicted_row_atlases,
            candidates_inspected,
            reclaim_complete,
            residency,
            charged: progress.charged,
            pinned: progress.pinned,
            pinned_overage: progress.pinned_overage,
        })
    }

    fn invalidate_cache_indices(&mut self) {
        self.chunk_index.invalidate();
        self.depth_index.invalidate();
        self.allele_index.invalidate();
        self.junction_index.invalidate();
        self.pair_index.invalidate();
        self.modification_index.invalidate();
        self.filter_index.invalidate();
    }

    /// Allocations owned by the currently published evidence, summaries and row atlas. Cache
    /// entries holding one of these mirror the presentation store's protected page set.
    fn published_allocations(&self) -> HashSet<AllocationId> {
        let mut protected = HashSet::new();
        let Some(current) = &self.current else {
            return protected;
        };
        protected.extend(
            current
                .root
                .chunks()
                .iter()
                .map(|chunk| chunk.core_allocation().id()),
        );
        if let Some(summaries) = &current.summaries {
            let root = &summaries.root;
            protected.extend(root.depth_tiles().iter().map(|tile| tile.allocation().id()));
            protected.extend(
                root.allele_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            protected.extend(
                root.junction_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            protected.extend(root.pair_tiles().iter().map(|tile| tile.allocation().id()));
            protected.extend(
                root.filter_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
            protected.extend(
                root.modification_tiles()
                    .iter()
                    .map(|tile| tile.allocation().id()),
            );
        }
        if let Some(atlas) = &current.row_atlas {
            protected.insert(atlas.root.metadata_allocation().id());
        }
        protected
    }

    fn take_revision(&mut self) -> Result<u64, EvidenceStoreError> {
        let revision = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(EvidenceStoreError::RevisionOverflow)?;
        self.eviction_order_dirty = true;
        Ok(revision)
    }

    fn validate_summary_evidence(&self, evidence: &EvidenceRoot) -> Result<(), EvidenceStoreError> {
        if evidence.source_revision() != self.source_revision {
            return Err(EvidenceStoreError::SourceRevisionMismatch);
        }
        if evidence.record_contract() != self.record_contract {
            return Err(EvidenceStoreError::RecordContractMismatch);
        }
        Ok(())
    }

    /// Returns the highest-priority legal eviction owner using only the tail of each
    /// already-ordered collection. Return-cache material yields before every active entry;
    /// within one residency tier, reproducible derived products yield before source evidence and
    /// each family remains LRU. Selection therefore stays constant-sized.
    fn oldest_cache_entry(&self) -> (Option<EvidenceCacheLocation>, usize) {
        let key = |residency, revision, location| (residency, location, revision);
        let candidates = [
            self.row_atlases.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::RowAtlas,
                )
            }),
            self.depth_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::DepthSummary,
                )
            }),
            self.allele_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::AlleleSummary,
                )
            }),
            self.junction_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::JunctionSummary,
                )
            }),
            self.pair_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::PairSummary,
                )
            }),
            self.modification_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::ModificationSummary,
                )
            }),
            self.filter_summaries.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::FilterSummary,
                )
            }),
            self.chunks.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::Chunk,
                )
            }),
            self.acquisitions.last().map(|cached| {
                key(
                    cached.residency,
                    cached.admitted_revision,
                    EvidenceCacheLocation::Acquisition,
                )
            }),
        ];
        let inspected = candidates.iter().flatten().count();
        (
            candidates
                .into_iter()
                .flatten()
                .min()
                .map(|(_, location, _)| location),
            inspected,
        )
    }

    fn residency_inventory(&self) -> Result<AlignmentCacheResidencyInventory, EvidenceStoreError> {
        let mut inventory = AlignmentCacheResidencyInventory::default();
        let mut allocations =
            HashMap::<AllocationId, (AlignmentCacheResidencyClass, AllocationRecordSnapshot)>::new(
            );
        macro_rules! observe_entries {
            ($entries:expr) => {
                for cached in $entries {
                    inventory.observe_entry(cached.residency);
                }
            };
        }
        observe_entries!(&self.acquisitions);
        observe_entries!(&self.chunks);
        observe_entries!(&self.depth_summaries);
        observe_entries!(&self.allele_summaries);
        observe_entries!(&self.junction_summaries);
        observe_entries!(&self.pair_summaries);
        observe_entries!(&self.modification_summaries);
        observe_entries!(&self.filter_summaries);
        observe_entries!(&self.row_atlases);

        for cached in &self.acquisitions {
            for chunk in cached.acquisition.chunks() {
                observe_cache_allocation(
                    &mut allocations,
                    cached.residency,
                    chunk.core_allocation(),
                )?;
                observe_cache_allocation(
                    &mut allocations,
                    cached.residency,
                    chunk.overlap_allocation(),
                )?;
                if let Some(payload) = chunk.payload() {
                    observe_cache_allocation(
                        &mut allocations,
                        cached.residency,
                        payload.allocation(),
                    )?;
                }
            }
        }
        for cached in &self.chunks {
            observe_cache_allocation(
                &mut allocations,
                cached.residency,
                cached.chunk.core_allocation(),
            )?;
            observe_cache_allocation(
                &mut allocations,
                cached.residency,
                cached.chunk.overlap_allocation(),
            )?;
            if let Some(payload) = cached.chunk.payload() {
                observe_cache_allocation(&mut allocations, cached.residency, payload.allocation())?;
            }
        }
        macro_rules! observe_summary_allocations {
            ($entries:expr) => {
                for cached in $entries {
                    observe_cache_allocation(
                        &mut allocations,
                        cached.residency,
                        cached.summary.allocation(),
                    )?;
                }
            };
        }
        observe_summary_allocations!(&self.depth_summaries);
        observe_summary_allocations!(&self.allele_summaries);
        observe_summary_allocations!(&self.junction_summaries);
        observe_summary_allocations!(&self.pair_summaries);
        observe_summary_allocations!(&self.modification_summaries);
        observe_summary_allocations!(&self.filter_summaries);
        for cached in &self.row_atlases {
            observe_cache_allocation(
                &mut allocations,
                cached.residency,
                cached.atlas.metadata_allocation(),
            )?;
            for layer in cached.atlas.layers() {
                observe_cache_allocation(&mut allocations, cached.residency, layer.allocation())?;
            }
        }
        for (_, (residency, allocation)) in allocations {
            inventory.observe_distinct_allocation(residency, allocation);
        }
        Ok(inventory)
    }
}

fn observe_cache_allocation(
    allocations: &mut HashMap<
        AllocationId,
        (AlignmentCacheResidencyClass, AllocationRecordSnapshot),
    >,
    residency: AlignmentCacheResidencyClass,
    allocation: &AllocationLease,
) -> Result<(), LedgerError> {
    if let Some((current, _)) = allocations.get_mut(&allocation.id()) {
        *current = (*current).max(residency);
        return Ok(());
    }
    let snapshot = allocation.snapshot()?;
    allocations.insert(snapshot.id, (residency, snapshot));
    Ok(())
}

fn atlas_compatible(
    atlas: &AlignmentRowAtlas,
    evidence: &EvidenceRoot,
    layout_contract: LayoutContract,
) -> bool {
    atlas.shares_stable_record_authority(evidence) && atlas.layout_contract() == layout_contract
}

fn chunk_satisfies_payload(chunk: &EvidenceChunk, required: &PayloadContract) -> bool {
    let no_payload_required = required.sequence() == crate::CapabilityRequirement::NotRequired
        && required.qualities() == crate::CapabilityRequirement::NotRequired
        && !required.all_auxiliary_tags()
        && required.auxiliary_tags().is_empty()
        && required.decode_reference().is_none();
    no_payload_required
        || chunk
            .payload()
            .is_some_and(|payload| payload.contract().satisfies(required))
}

fn acquisition_satisfies(
    cached: &CachedAcquisition,
    contig: rgv_core::ContigId,
    payload: &PayloadContract,
) -> bool {
    cached.acquisition.coverage().interval().contig() == contig
        && cached
            .acquisition
            .chunks()
            .iter()
            .all(|chunk| chunk_satisfies_payload(chunk, payload))
}

fn payload_satisfies(available: &EvidenceChunk, requested: &EvidenceChunk) -> bool {
    requested.payload().map_or(true, |required| {
        available
            .payload()
            .is_some_and(|payload| payload.contract().satisfies(required.contract()))
    })
}

fn evidence_chunk_interval(chunk: &EvidenceChunk) -> Option<GenomicInterval> {
    let batch = chunk.chunk().batch();
    let mut contig = None;
    let mut start = u64::MAX;
    let mut end = 0_u64;
    for index in 0..batch.len() {
        let (Some(record_contig), Some(record_start), Some(record_end)) =
            (batch.contig(index), batch.start(index), batch.end(index))
        else {
            continue;
        };
        if contig.is_some_and(|existing| existing != record_contig) {
            return None;
        }
        contig = Some(record_contig);
        start = start.min(record_start);
        end = end.max(record_end);
    }
    GenomicInterval::new(contig?, start, end).ok()
}

fn evidence_chunk_overlaps(chunk: &EvidenceChunk, target: GenomicInterval) -> bool {
    let batch = chunk.chunk().batch();
    (0..batch.len()).any(|index| {
        batch.contig(index) == Some(target.contig())
            && batch.start(index).is_some_and(|start| start < target.end())
            && batch.end(index).is_some_and(|end| end > target.start())
    })
}

#[derive(Debug)]
pub struct EvidenceCandidate {
    target: GenomicInterval,
    root: Arc<EvidenceRoot>,
    _pins: EvidencePins,
}

#[derive(Debug)]
pub struct ExactSummaryCandidate {
    target: GenomicInterval,
    root: Arc<ExactSummaryRoot>,
    _pins: ExactSummaryPins,
}

#[derive(Debug)]
pub struct RowAtlasCandidate {
    target: GenomicInterval,
    root: Arc<AlignmentRowAtlas>,
    _pins: AlignmentRowAtlasPins,
}

impl RowAtlasCandidate {
    #[must_use]
    pub const fn target(&self) -> GenomicInterval {
        self.target
    }

    #[must_use]
    pub const fn root(&self) -> &Arc<AlignmentRowAtlas> {
        &self.root
    }
}

impl ExactSummaryCandidate {
    #[must_use]
    pub const fn target(&self) -> GenomicInterval {
        self.target
    }

    #[must_use]
    pub const fn root(&self) -> &Arc<ExactSummaryRoot> {
        &self.root
    }
}

impl EvidenceCandidate {
    #[must_use]
    pub const fn target(&self) -> GenomicInterval {
        self.target
    }

    #[must_use]
    pub const fn root(&self) -> &Arc<EvidenceRoot> {
        &self.root
    }
}

#[derive(Debug)]
struct PublishedEvidence {
    target: GenomicInterval,
    root: Arc<EvidenceRoot>,
    _pins: EvidencePins,
    summaries: Option<PublishedSummaries>,
    row_atlas: Option<PublishedRowAtlas>,
}

#[derive(Debug)]
struct PublishedSummaries {
    target: GenomicInterval,
    root: Arc<ExactSummaryRoot>,
    _pins: ExactSummaryPins,
}

#[derive(Debug)]
struct PublishedRowAtlas {
    target: GenomicInterval,
    root: Arc<AlignmentRowAtlas>,
    _pins: AlignmentRowAtlasPins,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorePressure {
    pub evicted_acquisitions: usize,
    pub evicted_chunks: usize,
    pub evicted_exact_summaries: usize,
    pub evicted_row_atlases: usize,
    pub candidates_inspected: usize,
    pub reclaim_complete: bool,
    pub residency: AlignmentCacheResidencyInventory,
    pub charged: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
}

#[derive(Debug, Error)]
pub enum EvidenceStoreError {
    #[error(transparent)]
    Build(#[from] EvidenceBuildError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    ExactDepthSummary(#[from] ExactDepthSummaryError),
    #[error(transparent)]
    ExactAlleleSummary(#[from] ExactAlleleSummaryError),
    #[error(transparent)]
    ExactJunctionSummary(#[from] ExactJunctionSummaryError),
    #[error(transparent)]
    ExactPairSummary(#[from] ExactPairSummaryError),
    #[error(transparent)]
    ExactModificationSummary(#[from] ExactModificationSummaryError),
    #[error(transparent)]
    ExactFilterSummary(#[from] ExactFilterSummaryError),
    #[error(transparent)]
    ExactSummaryRoot(#[from] ExactSummaryRootError),
    #[error(transparent)]
    RowAtlas(#[from] AlignmentRowAtlasError),
    #[error(transparent)]
    ViewFilter(#[from] AlignmentFilterEvaluationError),
    #[error("acquisition belongs to a different source revision")]
    SourceRevisionMismatch,
    #[error("acquisition belongs to a different record contract")]
    RecordContractMismatch,
    #[error("exact summary candidate was not derived from its evidence candidate")]
    SummaryEvidenceMismatch,
    #[error("row-atlas candidate is incompatible with its evidence candidate")]
    RowAtlasEvidenceMismatch,
    #[error("cached evidence does not completely cover target {0:?}")]
    UncoveredTarget(GenomicInterval),
    #[error("evidence revision space exhausted")]
    RevisionOverflow,
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};

    use rgv_core::{
        AlignmentGroupMode, AlignmentOrderDirection, AlignmentSortMode, ContigId, GenomicInterval,
        OverlappingMatePolicy, ReadStackingMode,
    };
    use rgv_hts::{
        AlignmentBatch, AlignmentFlags, AlignmentRecord, AlignmentSourcePosition, SequenceData,
        TemplateLengthBounds,
    };

    use super::*;
    use crate::{
        AlignmentPresentationStore, AlignmentStyleContract, AllocationClass, CanonicalRecordChunk,
        CanonicalRowBlock, CapabilityRequirement, CompleteCoverage, ContractFingerprint,
        EvidenceChunk, FilteredRecordProvenance, GeometryLod, PresentationContract, SourceChunkId,
        SourceFingerprint,
    };

    fn source_revision() -> AlignmentSourceRevision {
        AlignmentSourceRevision::new(
            SourceFingerprint::new(RevisionFingerprint::new(1, 2)),
            RevisionFingerprint::new(3, 4),
        )
    }

    fn no_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [],
            None,
        )
    }

    fn junction_payload() -> PayloadContract {
        PayloadContract::new(
            CapabilityRequirement::NotRequired,
            CapabilityRequirement::NotRequired,
            [*b"XS"],
            None,
        )
    }

    fn summary_contract() -> SummaryContract {
        SummaryContract {
            overlapping_mate_policy: OverlappingMatePolicy::CountBoth,
            minimum_base_quality: 0,
            minimum_junction_flank: 0,
            allele_detail: crate::AlleleSummaryDetail::None,
            collect_modifications: false,
        }
    }

    fn layout_contract() -> LayoutContract {
        LayoutContract {
            stacking: ReadStackingMode::Stable,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
            sampling: crate::ReadSampling::Complete,
        }
    }

    fn empty_acquisition(start: u64, end: u64) -> Arc<EvidenceAcquisition> {
        Arc::new(
            EvidenceAcquisition::new(
                CompleteCoverage::new(
                    source_revision(),
                    ContractFingerprint::new(9),
                    GenomicInterval::new(ContigId::new(0), start, end).unwrap(),
                    FilteredRecordProvenance::default(),
                )
                .unwrap(),
                [],
            )
            .unwrap(),
        )
    }

    fn one_record_acquisition(
        ledger: &AllocationLedger,
        start: u64,
        end: u64,
        block: u64,
    ) -> Arc<EvidenceAcquisition> {
        let interval = GenomicInterval::new(ContigId::new(0), start, end).unwrap();
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: start.saturating_add(1),
            end: start.saturating_add(5).min(end),
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"resident".to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((block << 16) | 1),
            )
            .unwrap();
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
        let core = reservation.register(footprint.core_bytes, block).unwrap();
        let mut reservation = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap();
        let overlap = reservation
            .register(footprint.overlap_index_bytes, block)
            .unwrap();
        let chunk = Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap());
        Arc::new(
            EvidenceAcquisition::new(
                CompleteCoverage::new(
                    source_revision(),
                    ContractFingerprint::new(9),
                    interval,
                    FilteredRecordProvenance {
                        records_seen: 1,
                        eligible_records: 1,
                        ..FilteredRecordProvenance::default()
                    },
                )
                .unwrap(),
                [chunk],
            )
            .unwrap(),
        )
    }

    fn boundary_spanning_acquisition(
        ledger: &AllocationLedger,
        coverage_start: u64,
        coverage_end: u64,
        block: u64,
    ) -> Arc<EvidenceAcquisition> {
        let interval =
            GenomicInterval::new(ContigId::new(0), coverage_start, coverage_end).unwrap();
        let record = AlignmentRecord {
            contig: ContigId::new(0),
            start: 40,
            end: 60,
            flags: AlignmentFlags::from_bits(0),
            mapping_quality: Some(60),
            mate_contig: None,
            mate_start: None,
            template_length: 0,
            read_name: b"crosses-certificate-boundary".to_vec(),
            cigar: Vec::new(),
            sequence: SequenceData::NotLoaded,
            auxiliary_fields: Vec::new(),
        };
        let mut batch = AlignmentBatch::default();
        batch
            .push_with_source_position(
                &record,
                AlignmentSourcePosition::bam_virtual_offset((block << 16) | 1),
            )
            .unwrap();
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
        let core = ledger
            .reserve(
                AllocationClass::CoreChunk,
                footprint.core_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.core_bytes, block)
            .unwrap();
        let overlap = ledger
            .reserve(
                AllocationClass::OverlapIndex,
                footprint.overlap_index_bytes,
                ReservationKind::Normal,
            )
            .unwrap()
            .register(footprint.overlap_index_bytes, block)
            .unwrap();
        let chunk = Arc::new(EvidenceChunk::new(chunk, core, overlap).unwrap());
        Arc::new(
            EvidenceAcquisition::new(
                CompleteCoverage::new(
                    source_revision(),
                    ContractFingerprint::new(9),
                    interval,
                    FilteredRecordProvenance {
                        records_seen: 1,
                        eligible_records: 1,
                        ..FilteredRecordProvenance::default()
                    },
                )
                .unwrap(),
                [chunk],
            )
            .unwrap(),
        )
    }

    #[test]
    fn candidate_failure_and_cancellation_leave_current_byte_identical() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 10);
        let mut store = EvidenceStore::new(source_revision(), ContractFingerprint::new(9), ledger);
        store.admit(empty_acquisition(0, 100)).unwrap();
        let first = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 10, 20).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.publish(first);
        let current_pointer = Arc::as_ptr(&current);

        let error = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 200, 220).unwrap(),
                no_payload(),
                ReservationKind::Transition,
            )
            .unwrap_err();
        assert!(matches!(error, EvidenceStoreError::UncoveredTarget(_)));
        assert_eq!(Arc::as_ptr(store.current().unwrap()), current_pointer);

        store.admit(empty_acquisition(100, 300)).unwrap();
        let cancelled = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 200, 220).unwrap(),
                no_payload(),
                ReservationKind::Transition,
            )
            .unwrap();
        drop(cancelled);
        assert_eq!(Arc::as_ptr(store.current().unwrap()), current_pointer);
    }

    #[test]
    fn adjacent_certificates_form_one_complete_candidate() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(source_revision(), ContractFingerprint::new(9), ledger);
        store.admit(empty_acquisition(0, 50)).unwrap();
        store.admit(empty_acquisition(50, 100)).unwrap();
        let candidate = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 25, 75).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        assert!(candidate.root().coverage().covers(candidate.target()));
    }

    #[test]
    fn adjacent_query_provenance_does_not_double_count_a_boundary_spanning_record() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        // This is the production trace shape: both adjacent indexed queries truthfully observe
        // the same physical alignment because it overlaps their shared genomic boundary.
        store
            .admit(boundary_spanning_acquisition(&ledger, 0, 50, 17))
            .unwrap();
        store
            .admit(boundary_spanning_acquisition(&ledger, 50, 100, 17))
            .unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let canonical = store
            .prepare(target, no_payload(), ReservationKind::Normal)
            .unwrap();

        assert!(canonical.root().coverage().covers(target));
        assert_eq!(canonical.root().select(target).unwrap().len(), 1);
        assert_eq!(canonical.root().exact_filter_provenance(target), None);

        let filter = crate::AlignmentViewFilterContract::new(
            1,
            [crate::AlignmentFilterPredicate::MappingQuality {
                minimum: Some(0),
                maximum: None,
            }],
        );
        let filtered = store
            .prepare_filtered(
                canonical,
                target,
                &filter,
                filter.evaluation_fingerprint(None),
                None,
                ReservationKind::Transition,
            )
            .unwrap();

        assert_eq!(filtered.root().view_filter().eligible_records(), 1);
        assert_eq!(filtered.root().view_filter().matching_records(), 1);
        assert_eq!(
            filtered.root().exact_filter_provenance(target),
            Some(FilteredRecordProvenance {
                records_seen: 1,
                eligible_records: 1,
                ..FilteredRecordProvenance::default()
            })
        );
    }

    #[test]
    fn filtered_candidate_shares_canonical_chunks_and_clears_without_source_io() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 17))
            .unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let canonical = store
            .prepare(target, no_payload(), ReservationKind::Normal)
            .unwrap();
        let canonical_chunk = Arc::clone(&canonical.root().chunks()[0]);
        let retained_handle = canonical_chunk.chunk().handles()[0];
        let filter = crate::AlignmentViewFilterContract::new(
            1,
            [crate::AlignmentFilterPredicate::MappingQuality {
                minimum: Some(61),
                maximum: None,
            }],
        );
        let filtered = store
            .prepare_filtered(
                canonical,
                target,
                &filter,
                filter.evaluation_fingerprint(None),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        assert_eq!(filtered.root().record_count(), 0);
        assert_eq!(filtered.root().view_filter().eligible_records(), 1);
        assert_eq!(filtered.root().view_filter().matching_records(), 0);
        assert!(Arc::ptr_eq(&canonical_chunk, &filtered.root().chunks()[0]));
        assert!(!filtered.root().contains_handle(retained_handle));
        store.publish(filtered);
        // Eviction keeps the certificate backing the published root, so model its release (a
        // locus change discarding the cache) explicitly.
        let pressure = store.evict_inactive_until(0).unwrap();
        assert_eq!(pressure.evicted_acquisitions, 0);
        assert!(
            store.discard_disjoint_cache(
                GenomicInterval::new(ContigId::new(0), 10_000, 10_100).unwrap()
            ) > 0
        );

        let restored = store
            .prepare(target, no_payload(), ReservationKind::Transition)
            .unwrap();
        assert_eq!(restored.root().record_count(), 1);
        assert!(restored.root().contains_handle(retained_handle));
        assert_eq!(store.acquisition_count(), 0);
        assert!(Arc::ptr_eq(&canonical_chunk, &restored.root().chunks()[0]));
    }

    #[test]
    fn filtered_current_rederives_a_broader_cached_target_from_eligible_authority() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 18))
            .unwrap();
        let broad = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let narrow = GenomicInterval::new(ContigId::new(0), 0, 20).unwrap();
        let canonical = store
            .prepare(broad, no_payload(), ReservationKind::Normal)
            .unwrap();
        let canonical_chunk = Arc::clone(&canonical.root().chunks()[0]);
        let filter = crate::AlignmentViewFilterContract::new(
            1,
            [crate::AlignmentFilterPredicate::MappingQuality {
                minimum: Some(61),
                maximum: None,
            }],
        );
        let filtered = store
            .prepare_filtered(
                canonical,
                narrow,
                &filter,
                filter.evaluation_fingerprint(None),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        assert!(filtered.root().coverage().covers(narrow));
        assert!(!filtered.root().coverage().covers(broad));
        assert!(filtered.root().eligible_coverage().covers(broad));
        store.publish(filtered);

        // Model the state from the production trace: the immutable filtered root is current,
        // while the independently cached acquisition has been released. Its shared canonical
        // chunks and broader eligible certificate are still sufficient for zoom-out. Eviction
        // itself now keeps entries backing the published root, so release it explicitly.
        let pressure = store.evict_inactive_until(0).unwrap();
        assert_eq!(pressure.evicted_acquisitions, 0);
        assert!(
            store.discard_disjoint_cache(
                GenomicInterval::new(ContigId::new(0), 10_000, 10_100).unwrap()
            ) > 0
        );
        assert_eq!(store.acquisition_count(), 0);
        assert!(store.cached_covers(broad, &no_payload()));

        let restored = store
            .prepare(broad, no_payload(), ReservationKind::Transition)
            .unwrap();
        assert!(restored.root().coverage().covers(broad));
        assert_eq!(restored.root().record_count(), 1);
        assert!(Arc::ptr_eq(&canonical_chunk, &restored.root().chunks()[0]));
    }

    #[test]
    fn budget_reduction_evicts_cache_ownership_but_never_current_evidence() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 7))
            .unwrap();
        let candidate = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 0, 20).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.publish(candidate);
        let pressure = store.reduce_budget(0).unwrap();

        // The only cache entries back the published root: dropping them would free nothing
        // (the root pins every byte) and would forget the certificate the next extension needs.
        assert_eq!(pressure.evicted_acquisitions, 0);
        assert_eq!(pressure.evicted_chunks, 0);
        assert!(pressure.reclaim_complete);
        assert!(pressure.charged > 0);
        assert_eq!(pressure.pinned, pressure.charged);
        assert_eq!(pressure.pinned_overage, pressure.charged);
        assert_eq!(store.acquisition_count(), 1);
        assert_eq!(store.chunk_count(), 1);
        assert_eq!(
            current
                .select(GenomicInterval::new(ContigId::new(0), 1, 2).unwrap())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn eviction_skips_entries_backing_the_published_locus_without_spending_its_quantum() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        let visible = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let elsewhere = GenomicInterval::new(ContigId::new(0), 1_000, 1_100).unwrap();
        // The visible locus is admitted first, so its entries are the oldest in every family
        // and sit at the eviction tail.
        store
            .admit(one_record_acquisition(
                &ledger,
                visible.start(),
                visible.end(),
                7,
            ))
            .unwrap();
        store
            .admit(one_record_acquisition(
                &ledger,
                elsewhere.start(),
                elsewhere.end(),
                8,
            ))
            .unwrap();
        let candidate = store
            .prepare(visible, no_payload(), ReservationKind::Normal)
            .unwrap();
        let _current = store.publish(candidate);

        // One entry per quantum: the protected tail entries must not consume it.
        let first = store.evict_inactive_batch(0, 1).unwrap();
        assert_eq!(first.evicted_chunks + first.evicted_acquisitions, 1);
        let rest = store.evict_inactive_until(0).unwrap();
        assert_eq!(first.evicted_chunks + rest.evicted_chunks, 1);
        assert_eq!(first.evicted_acquisitions + rest.evicted_acquisitions, 1);
        assert!(rest.reclaim_complete);

        assert_eq!(store.acquisition_count(), 1);
        assert_eq!(store.chunk_count(), 1);
        // The visible locus can still be re-prepared and extended from cache alone...
        assert!(
            store
                .prepare(visible, no_payload(), ReservationKind::Normal)
                .is_ok()
        );
        // ...while the other locus is gone and needs its source again.
        assert!(matches!(
            store.prepare(elsewhere, no_payload(), ReservationKind::Normal),
            Err(EvidenceStoreError::UncoveredTarget(_))
        ));

        // Protection follows publication, not admission: once another locus is published the
        // old entries are ordinary cache again.
        store
            .admit(one_record_acquisition(
                &ledger,
                elsewhere.start(),
                elsewhere.end(),
                8,
            ))
            .unwrap();
        let candidate = store
            .prepare(elsewhere, no_payload(), ReservationKind::Normal)
            .unwrap();
        let _current = store.publish(candidate);
        let moved = store.evict_inactive_until(0).unwrap();
        assert_eq!(moved.evicted_acquisitions, 1);
        assert_eq!(moved.evicted_chunks, 1);
        assert!(
            store
                .prepare(elsewhere, no_payload(), ReservationKind::Normal)
                .is_ok()
        );
    }

    #[test]
    fn keyed_summary_lookups_stay_correct_across_eviction_reordering() {
        let ledger = AllocationLedger::new(1 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 300, 7))
            .unwrap();
        let evidence = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 0, 300).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        let contract = DepthSummaryContract::new(OverlappingMatePolicy::CountBoth);
        let depth = |store: &mut EvidenceStore, coordinate: u64| {
            store
                .exact_depth_summary(
                    evidence.root(),
                    CanonicalGenomicTile::new(
                        ContigId::new(0),
                        coordinate,
                        NonZeroU64::new(100).unwrap(),
                    ),
                    300,
                    contract,
                    ReservationKind::Normal,
                )
                .unwrap()
        };
        let tiles = (0..3)
            .map(|coordinate| depth(&mut store, coordinate))
            .collect::<Vec<_>>();
        assert_eq!(store.depth_summary_count(), 3);
        // A hit returns the cached tile and adds nothing.
        assert!(Arc::ptr_eq(&depth(&mut store, 1), &tiles[1]));
        assert_eq!(store.depth_summary_count(), 3);

        // Eviction sorts and pops the cache vector; the keyed index must follow. Tile 0 is now
        // the least recently used summary.
        let pressure = store.evict_inactive_batch(0, 1).unwrap();
        assert_eq!(pressure.evicted_exact_summaries, 1);
        assert_eq!(store.depth_summary_count(), 2);
        assert!(Arc::ptr_eq(&depth(&mut store, 2), &tiles[2]));
        assert!(Arc::ptr_eq(&depth(&mut store, 1), &tiles[1]));
        assert!(!Arc::ptr_eq(&depth(&mut store, 0), &tiles[0]));
        assert_eq!(store.depth_summary_count(), 3);
        for (coordinate, tile) in tiles.iter().enumerate() {
            assert_eq!(tile.tile().coordinate(), coordinate as u64);
        }
    }

    #[test]
    fn contained_target_stays_on_the_published_then_widest_row_atlas() {
        let ledger = AllocationLedger::new(4 << 20, 1 << 20);
        let mut store = EvidenceStore::new(source_revision(), ContractFingerprint::new(9), ledger);
        store.admit(empty_acquisition(0, 100)).unwrap();
        store.admit(empty_acquisition(100, 200)).unwrap();
        let narrow = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let wide = GenomicInterval::new(ContigId::new(0), 0, 200).unwrap();
        let contained = GenomicInterval::new(ContigId::new(0), 10, 50).unwrap();
        let authority = |store: &mut EvidenceStore, target: GenomicInterval| {
            let evidence = store
                .prepare(target, junction_payload(), ReservationKind::Normal)
                .unwrap();
            let summaries = store
                .prepare_exact_summaries(
                    evidence.root(),
                    target,
                    NonZeroU64::new(100).unwrap(),
                    200,
                    summary_contract(),
                    None,
                    ReservationKind::Normal,
                )
                .unwrap();
            let rows = store
                .prepare_row_atlas(
                    evidence.root(),
                    target,
                    layout_contract(),
                    ReservationKind::Normal,
                )
                .unwrap();
            (evidence, summaries, rows)
        };
        let contained_atlas = |store: &mut EvidenceStore| {
            let evidence = store
                .prepare(contained, junction_payload(), ReservationKind::Normal)
                .unwrap();
            Arc::clone(
                store
                    .prepare_row_atlas(
                        evidence.root(),
                        contained,
                        layout_contract(),
                        ReservationKind::Normal,
                    )
                    .unwrap()
                    .root(),
            )
        };

        let (evidence, summaries, rows) = authority(&mut store, narrow);
        let (_, _, narrow_atlas) = store
            .publish_complete_authority(evidence, summaries, rows)
            .unwrap();
        // A wider atlas is prepared by extension but not yet published.
        let (evidence, summaries, rows) = authority(&mut store, wide);
        let wide_atlas = Arc::clone(rows.root());
        assert!(!Arc::ptr_eq(&narrow_atlas, &wide_atlas));
        assert_eq!(store.row_atlas_count(), 2);
        // The camera stays on the published authority while the wider one is only a candidate.
        assert!(Arc::ptr_eq(&contained_atlas(&mut store), &narrow_atlas));

        store
            .publish_complete_authority(evidence, summaries, rows)
            .unwrap();
        // After the extension publishes, a contained target must not flip back to the
        // superseded, smaller atlas and its older revision.
        let reused = contained_atlas(&mut store);
        assert!(Arc::ptr_eq(&reused, &wide_atlas));
        assert_eq!(reused.revision(), wide_atlas.revision());
        assert_eq!(store.row_atlas_count(), 2);
    }

    #[test]
    fn bounded_reclaim_evicts_demoted_source_before_an_active_row_atlas() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        let old = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let active = GenomicInterval::new(ContigId::new(0), 1_000, 1_100).unwrap();
        store
            .admit(one_record_acquisition(&ledger, old.start(), old.end(), 7))
            .unwrap();
        store
            .admit(one_record_acquisition(
                &ledger,
                active.start(),
                active.end(),
                8,
            ))
            .unwrap();
        let evidence = store
            .prepare(active, no_payload(), ReservationKind::Normal)
            .unwrap();
        let _atlas = store
            .prepare_row_atlas(
                evidence.root(),
                active,
                layout_contract(),
                ReservationKind::Normal,
            )
            .unwrap();

        assert_eq!(store.demote_cache_outside(&[active]), 2);
        let pressure = store.evict_inactive_batch(0, 1).unwrap();

        assert_eq!(pressure.evicted_chunks, 1);
        assert_eq!(pressure.evicted_row_atlases, 0);
        assert_eq!(store.chunk_count(), 1);
        assert_eq!(store.row_atlas_count(), 1);
        assert!(!pressure.reclaim_complete);
    }

    #[test]
    fn compatible_depth_tile_is_reused_across_wider_evidence_publication() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 7))
            .unwrap();
        let candidate = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.publish(candidate);
        let tile = CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap());
        let contract = DepthSummaryContract::new(OverlappingMatePolicy::CountBoth);
        let first = store
            .exact_depth_summary(&current, tile, 200, contract, ReservationKind::Normal)
            .unwrap();

        store.admit(empty_acquisition(100, 200)).unwrap();
        let candidate = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 0, 150).unwrap(),
                no_payload(),
                ReservationKind::Transition,
            )
            .unwrap();
        let wider = store.publish(candidate);
        let reused = store
            .exact_depth_summary(&wider, tile, 200, contract, ReservationKind::Normal)
            .unwrap();

        assert_eq!(Arc::as_ptr(&first), Arc::as_ptr(&reused));
        assert_eq!(store.depth_summary_count(), 1);
        assert_ne!(first.evidence_revision(), wider.revision());
    }

    #[test]
    fn contained_target_reuses_row_atlas_without_new_allocations_or_repack() {
        let ledger = AllocationLedger::new(2 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 7))
            .unwrap();
        let horizon = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let evidence = store
            .prepare(horizon, no_payload(), ReservationKind::Normal)
            .unwrap();
        let first = store
            .prepare_row_atlas(
                evidence.root(),
                horizon,
                layout_contract(),
                ReservationKind::Normal,
            )
            .unwrap();
        let first_pointer = Arc::as_ptr(first.root());
        let first_fingerprint = first.root().assignment_fingerprint();
        let before = ledger.snapshot().unwrap();
        let contained = GenomicInterval::new(ContigId::new(0), 10, 20).unwrap();
        let second = store
            .prepare_row_atlas(
                evidence.root(),
                contained,
                layout_contract(),
                ReservationKind::Normal,
            )
            .unwrap();
        let after = ledger.snapshot().unwrap();

        assert_eq!(Arc::as_ptr(second.root()), first_pointer);
        assert_eq!(second.root().assignment_fingerprint(), first_fingerprint);
        assert_eq!(store.row_atlas_count(), 1);
        assert_eq!(after.charged, before.charged);
        assert_eq!(after.records.len(), before.records.len());
    }

    #[test]
    fn compatible_older_row_atlas_can_warm_pages_for_newer_evidence() {
        let ledger = AllocationLedger::new(4 << 20, 1 << 20);
        let mut evidence_store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        evidence_store
            .admit(one_record_acquisition(&ledger, 0, 100, 7))
            .unwrap();
        let horizon = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let first_evidence = evidence_store
            .prepare(horizon, no_payload(), ReservationKind::Normal)
            .unwrap();
        let retained_atlas = evidence_store
            .prepare_row_atlas(
                first_evidence.root(),
                horizon,
                layout_contract(),
                ReservationKind::Normal,
            )
            .unwrap();

        let contained = GenomicInterval::new(ContigId::new(0), 0, 50).unwrap();
        let newer_evidence = evidence_store
            .prepare(contained, no_payload(), ReservationKind::Normal)
            .unwrap();
        assert_ne!(
            retained_atlas.root().derived_from_evidence(),
            newer_evidence.root().revision()
        );
        assert!(
            retained_atlas
                .root()
                .shares_stable_record_authority(newer_evidence.root())
        );

        let mut presentation_store = AlignmentPresentationStore::new(ledger);
        let warmed = presentation_store
            .warm_read_row_block(
                newer_evidence.root(),
                retained_atlas.root(),
                PresentationContract {
                    geometry_lod: GeometryLod::Span,
                    style: AlignmentStyleContract::default(),
                    reference: None,
                    annotation: None,
                },
                contained,
                CanonicalRowBlock::new(0, NonZeroU32::new(8).unwrap()),
                100,
                NonZeroU64::new(100).unwrap(),
                None,
                ReservationKind::Normal,
            )
            .unwrap();

        assert!(warmed.page_occurrences > 0);
    }

    #[test]
    fn evidence_summaries_and_rows_publish_as_one_authority() {
        let ledger = AllocationLedger::new(4 << 20, 1 << 20);
        let mut store = EvidenceStore::new(source_revision(), ContractFingerprint::new(9), ledger);
        store.admit(empty_acquisition(0, 100)).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let evidence = store
            .prepare(target, junction_payload(), ReservationKind::Normal)
            .unwrap();
        let summaries = store
            .prepare_exact_summaries(
                evidence.root(),
                target,
                NonZeroU64::new(100).unwrap(),
                100,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        let rows = store
            .prepare_row_atlas(
                evidence.root(),
                target,
                layout_contract(),
                ReservationKind::Transition,
            )
            .unwrap();

        assert!(store.current().is_none());
        let (_, _, published_rows) = store
            .publish_complete_authority(evidence, summaries, rows)
            .unwrap();

        assert_eq!(
            store.current_row_atlas().map(Arc::as_ptr),
            Some(Arc::as_ptr(&published_rows))
        );
        assert_eq!(store.current_row_atlas_target(), Some(target));
        assert!(store.current_summaries().is_some());
    }

    #[test]
    fn budget_pressure_evicts_unpinned_summary_before_source_acquisition() {
        let ledger = AllocationLedger::new(1 << 20, 0);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store
            .admit(one_record_acquisition(&ledger, 0, 100, 7))
            .unwrap();
        let candidate = store
            .prepare(
                GenomicInterval::new(ContigId::new(0), 0, 100).unwrap(),
                no_payload(),
                ReservationKind::Normal,
            )
            .unwrap();
        let current = store.publish(candidate);
        let summary = store
            .exact_depth_summary(
                &current,
                CanonicalGenomicTile::new(ContigId::new(0), 0, NonZeroU64::new(100).unwrap()),
                100,
                DepthSummaryContract::new(OverlappingMatePolicy::CountBoth),
                ReservationKind::Normal,
            )
            .unwrap();
        let target_budget = ledger
            .snapshot()
            .unwrap()
            .charged
            .saturating_sub(summary.estimated_bytes());
        drop(summary);

        let pressure = store.reduce_budget(target_budget).unwrap();

        assert_eq!(pressure.evicted_exact_summaries, 1);
        assert_eq!(pressure.evicted_acquisitions, 0);
        assert_eq!(store.depth_summary_count(), 0);
        assert_eq!(store.acquisition_count(), 1);
        assert!(pressure.charged <= NonZeroUsize::new(target_budget).unwrap().get());
    }

    #[test]
    fn exact_summary_cache_priority_follows_the_prepared_root_not_locus_overlap_alone() {
        let ledger = AllocationLedger::new(4 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        let old = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let active = GenomicInterval::new(ContigId::new(0), 1_000, 1_100).unwrap();
        store
            .admit(empty_acquisition(old.start(), old.end()))
            .unwrap();
        store
            .admit(empty_acquisition(active.start(), active.end()))
            .unwrap();

        let old_evidence = store
            .prepare(old, junction_payload(), ReservationKind::Normal)
            .unwrap();
        let old_summaries = store
            .prepare_exact_summaries(
                old_evidence.root(),
                old,
                NonZeroU64::new(100).unwrap(),
                2_000,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        let active_evidence = store
            .prepare(active, junction_payload(), ReservationKind::Normal)
            .unwrap();
        let active_summaries = store
            .prepare_exact_summaries(
                active_evidence.root(),
                active,
                NonZeroU64::new(100).unwrap(),
                2_000,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();

        let changed =
            store.reclassify_exact_summary_cache(&[active], &[Arc::clone(active_summaries.root())]);
        let inventory = store.cache_residency_inventory().unwrap();

        assert_eq!(changed, 4);
        assert_eq!(store.exact_summary_count(), 8);
        assert_eq!(inventory.active_near_buffer_entries, 6); // four active summaries + two sources
        assert_eq!(inventory.recent_locus_return_cache_entries, 4);
        assert_eq!(inventory.active_locus_cold_entries, 0);
        assert!(inventory.active_near_buffer_bytes > 0);
        assert!(inventory.recent_locus_return_cache_bytes > 0);
        drop(old_summaries);
    }

    #[test]
    fn complete_summary_families_publish_atomically_with_their_evidence() {
        let ledger = AllocationLedger::new(2 << 20, 1 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store.admit(empty_acquisition(0, 100)).unwrap();
        store.admit(empty_acquisition(100, 200)).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 10, 150).unwrap();
        let evidence = store
            .prepare(target, junction_payload(), ReservationKind::Normal)
            .unwrap();
        let summaries = store
            .prepare_exact_summaries(
                evidence.root(),
                target,
                NonZeroU64::new(100).unwrap(),
                200,
                summary_contract(),
                Some(10_000),
                ReservationKind::Transition,
            )
            .unwrap();

        assert!(store.current().is_none());
        assert_eq!(
            summaries.root().prepared_interval(),
            GenomicInterval::new(ContigId::new(0), 0, 200).unwrap()
        );
        assert_eq!(summaries.root().maximum_depth(target).unwrap(), 0);
        assert_eq!(
            summaries
                .root()
                .pair_summary(target)
                .unwrap()
                .sampled_templates,
            0
        );
        assert_eq!(
            summaries.root().prepared_pair_summary(),
            summaries
                .root()
                .pair_summary(summaries.root().prepared_interval())
                .unwrap()
        );
        assert_eq!(summaries.root().pin().unwrap().len(), 9);
        assert_eq!(summaries.root().filter_tiles().len(), 2);
        assert_eq!(summaries.root().filter_provenance_certificates().count(), 2);
        assert_eq!(
            summaries
                .root()
                .filter_provenance(GenomicInterval::new(ContigId::new(0), 0, 200).unwrap())
                .unwrap_err(),
            crate::ExactSummaryQueryError::IndivisibleFilterProvenance
        );
        assert_eq!(
            summaries
                .root()
                .filter_provenance(GenomicInterval::new(ContigId::new(0), 0, 100).unwrap())
                .unwrap_err(),
            crate::ExactSummaryQueryError::IndivisibleFilterProvenance
        );
        let metadata_bytes = summaries.root().estimated_metadata_bytes();
        let (_, published_summaries) = store.publish_with_summaries(evidence, summaries).unwrap();

        assert_eq!(
            store.current_summaries().map(Arc::as_ptr),
            Some(Arc::as_ptr(&published_summaries))
        );
        assert!(ledger.snapshot().unwrap().records.iter().any(|record| {
            record.class == AllocationClass::SummaryRootMetadata
                && record.charged_capacity == metadata_bytes
        }));
    }

    #[test]
    fn stale_summary_candidate_cannot_publish_with_another_evidence_revision() {
        let ledger = AllocationLedger::new(2 << 20, 1 << 20);
        let mut store = EvidenceStore::new(source_revision(), ContractFingerprint::new(9), ledger);
        store.admit(empty_acquisition(0, 100)).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let first = store
            .prepare(target, junction_payload(), ReservationKind::Normal)
            .unwrap();
        let summaries = store
            .prepare_exact_summaries(
                first.root(),
                target,
                NonZeroU64::new(100).unwrap(),
                100,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        let second = store
            .prepare(target, junction_payload(), ReservationKind::Normal)
            .unwrap();

        let error = store.publish_with_summaries(second, summaries).unwrap_err();

        assert!(matches!(error, EvidenceStoreError::SummaryEvidenceMismatch));
        assert!(store.current().is_none());
        drop(first);
    }

    #[test]
    fn repeated_candidate_publication_reaches_a_stable_charged_high_water_mark() {
        let ledger = AllocationLedger::new(4 << 20, 2 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store.admit(empty_acquisition(0, 100)).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let mut charged = Vec::new();
        for _ in 0..32 {
            let evidence = store
                .prepare(target, junction_payload(), ReservationKind::Transition)
                .unwrap();
            let summaries = store
                .prepare_exact_summaries(
                    evidence.root(),
                    target,
                    NonZeroU64::new(100).unwrap(),
                    100,
                    summary_contract(),
                    None,
                    ReservationKind::Transition,
                )
                .unwrap();
            store.publish_with_summaries(evidence, summaries).unwrap();
            charged.push(ledger.snapshot().unwrap().charged);
        }

        assert!(charged.windows(2).all(|values| values[0] == values[1]));
        assert_eq!(store.exact_summary_count(), 4);
    }

    #[test]
    fn two_panel_candidates_share_source_and_summary_allocations_without_double_charge() {
        let ledger = AllocationLedger::new(4 << 20, 2 << 20);
        let mut store = EvidenceStore::new(
            source_revision(),
            ContractFingerprint::new(9),
            ledger.clone(),
        );
        store.admit(empty_acquisition(0, 100)).unwrap();
        let target = GenomicInterval::new(ContigId::new(0), 0, 100).unwrap();
        let first_evidence = store
            .prepare(target, junction_payload(), ReservationKind::Transition)
            .unwrap();
        let first_summaries = store
            .prepare_exact_summaries(
                first_evidence.root(),
                target,
                NonZeroU64::new(100).unwrap(),
                100,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        let second_evidence = store
            .prepare(target, junction_payload(), ReservationKind::Transition)
            .unwrap();
        let second_summaries = store
            .prepare_exact_summaries(
                second_evidence.root(),
                target,
                NonZeroU64::new(100).unwrap(),
                100,
                summary_contract(),
                None,
                ReservationKind::Transition,
            )
            .unwrap();
        let snapshot = ledger.snapshot().unwrap();

        assert_eq!(
            snapshot
                .records
                .iter()
                .filter(|record| record.class == AllocationClass::ExactSummary)
                .count(),
            4
        );
        assert_eq!(store.exact_summary_count(), 4);
        for allocation in snapshot
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::ExactSummary)
        {
            assert_eq!(allocation.pins, 2);
        }
        drop((first_evidence, first_summaries));
        for allocation in ledger
            .snapshot()
            .unwrap()
            .records
            .iter()
            .filter(|record| record.class == AllocationClass::ExactSummary)
        {
            assert_eq!(allocation.pins, 1);
        }
        drop((second_evidence, second_summaries));
    }
}
