use std::{mem::size_of, num::NonZeroU64, sync::Arc};

use rgv_core::GenomicInterval;
use thiserror::Error;

use crate::{
    AlignmentRowAtlas, AlignmentRowAtlasError, AlignmentRowAtlasPins, AlignmentSourceRevision,
    AlleleSummaryContract, AllocationLedger, CanonicalGenomicTile, DepthSummaryContract,
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
    TransientResourceClass,
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
    current: Option<PublishedEvidence>,
    next_revision: u64,
}

#[derive(Debug)]
struct CachedAcquisition {
    admitted_revision: u64,
    acquisition: Arc<EvidenceAcquisition>,
}

#[derive(Debug)]
struct CachedEvidenceChunk {
    admitted_revision: u64,
    chunk: Arc<EvidenceChunk>,
}

#[derive(Debug)]
struct CachedDepthSummary {
    admitted_revision: u64,
    key: ExactDepthSummaryKey,
    summary: Arc<ExactDepthSummaryTile>,
}

#[derive(Debug)]
struct CachedAlleleSummary {
    admitted_revision: u64,
    key: ExactAlleleSummaryKey,
    summary: Arc<ExactAlleleSummaryTile>,
}

#[derive(Debug)]
struct CachedJunctionSummary {
    admitted_revision: u64,
    key: ExactJunctionSummaryKey,
    summary: Arc<ExactJunctionSummaryTile>,
}

#[derive(Debug)]
struct CachedPairSummary {
    admitted_revision: u64,
    key: ExactPairSummaryKey,
    summary: Arc<ExactPairSummaryTile>,
}

#[derive(Debug)]
struct CachedModificationSummary {
    admitted_revision: u64,
    key: ExactModificationSummaryKey,
    summary: Arc<ExactModificationSummaryTile>,
}

#[derive(Debug)]
struct CachedFilterSummary {
    admitted_revision: u64,
    key: ExactFilterSummaryKey,
    summary: Arc<ExactFilterSummaryTile>,
}

#[derive(Debug)]
struct CachedRowAtlas {
    admitted_revision: u64,
    atlas: Arc<AlignmentRowAtlas>,
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
            current: None,
            next_revision: 1,
        }
    }

    #[must_use]
    pub const fn source_revision(&self) -> AlignmentSourceRevision {
        self.source_revision
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
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, cached)| cached.chunk.chunk().id() == chunk.chunk().id())
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
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
            self.chunks[index] = CachedEvidenceChunk {
                admitted_revision: revision,
                chunk: Arc::clone(&chunk),
            };
            return Ok(chunk);
        }
        self.chunks.push(CachedEvidenceChunk {
            admitted_revision: revision,
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
        if row_atlas.root.source_revision() != evidence.root.source_revision()
            || row_atlas.root.record_contract() != evidence.root.record_contract()
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
        if let Some(cached) = self
            .depth_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.depth_summaries.push(CachedDepthSummary {
            admitted_revision: revision,
            key,
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
        if let Some(cached) = self
            .allele_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.allele_summaries.push(CachedAlleleSummary {
            admitted_revision: revision,
            key,
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
        if let Some(cached) = self
            .junction_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.junction_summaries.push(CachedJunctionSummary {
            admitted_revision: revision,
            key,
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
        if let Some(cached) = self
            .pair_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.pair_summaries.push(CachedPairSummary {
            admitted_revision: revision,
            key,
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
        if let Some(cached) = self
            .modification_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.modification_summaries.push(CachedModificationSummary {
            admitted_revision: revision,
            key,
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
        if let Some(cached) = self
            .filter_summaries
            .iter_mut()
            .find(|cached| cached.key == key)
        {
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
        self.filter_summaries.push(CachedFilterSummary {
            admitted_revision: revision,
            key,
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

        let containing = self
            .row_atlases
            .iter()
            .enumerate()
            .filter(|(_, cached)| {
                atlas_compatible(&cached.atlas, evidence, layout_contract)
                    && cached.atlas.interval().contains_interval(target)
            })
            .min_by_key(|(_, cached)| cached.atlas.interval().len())
            .map(|(index, _)| index);
        if let Some(index) = containing {
            let cached = &mut self.row_atlases[index];
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
        before_acquisitions
            .saturating_sub(self.acquisitions.len())
            .saturating_add(before_chunks.saturating_sub(self.chunks.len()))
            .saturating_add(before_summaries.saturating_sub(self.exact_summary_count()))
            .saturating_add(before_atlases.saturating_sub(self.row_atlases.len()))
    }

    /// Applies a smaller budget and releases derived summaries before oldest source acquisitions.
    /// Pinned current evidence is never destroyed and remains visible as explicit pinned overage.
    pub fn reduce_budget(&mut self, budget: usize) -> Result<StorePressure, EvidenceStoreError> {
        self.ledger.set_budget(budget)?;
        self.acquisitions
            .sort_by_key(|cached| cached.admitted_revision);
        self.chunks.sort_by_key(|cached| cached.admitted_revision);
        self.depth_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.allele_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.junction_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.pair_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.filter_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.modification_summaries
            .sort_by_key(|cached| cached.admitted_revision);
        self.row_atlases
            .sort_by_key(|cached| cached.admitted_revision);
        let mut evicted_acquisitions = 0;
        let mut evicted_chunks = 0;
        let mut evicted_exact_summaries = 0;
        let mut evicted_row_atlases = 0;
        while self.ledger.snapshot()?.charged > budget {
            if !self.row_atlases.is_empty() {
                self.row_atlases.remove(0);
                evicted_row_atlases += 1;
            } else if self.evict_oldest_exact_summary() {
                evicted_exact_summaries += 1;
            } else if !self.chunks.is_empty() {
                self.chunks.remove(0);
                evicted_chunks += 1;
            } else if !self.acquisitions.is_empty() {
                self.acquisitions.remove(0);
                evicted_acquisitions += 1;
            } else {
                break;
            }
        }
        let snapshot = self.ledger.snapshot()?;
        Ok(StorePressure {
            evicted_acquisitions,
            evicted_chunks,
            evicted_exact_summaries,
            evicted_row_atlases,
            charged: snapshot.charged,
            pinned: snapshot.pinned,
            pinned_overage: snapshot.pinned_overage,
        })
    }

    fn take_revision(&mut self) -> Result<u64, EvidenceStoreError> {
        let revision = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(EvidenceStoreError::RevisionOverflow)?;
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

    fn evict_oldest_exact_summary(&mut self) -> bool {
        let candidates = [
            self.depth_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 0_u8)),
            self.allele_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 1)),
            self.junction_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 2)),
            self.pair_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 3)),
            self.modification_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 4)),
            self.filter_summaries
                .first()
                .map(|cached| (cached.admitted_revision, 5)),
        ];
        let Some((_, kind)) = candidates.into_iter().flatten().min() else {
            return false;
        };
        match kind {
            0 => {
                self.depth_summaries.remove(0);
            }
            1 => {
                self.allele_summaries.remove(0);
            }
            2 => {
                self.junction_summaries.remove(0);
            }
            3 => {
                self.pair_summaries.remove(0);
            }
            4 => {
                self.modification_summaries.remove(0);
            }
            5 => {
                self.filter_summaries.remove(0);
            }
            _ => unreachable!("summary family is bounded by the candidate table"),
        }
        true
    }
}

fn atlas_compatible(
    atlas: &AlignmentRowAtlas,
    evidence: &EvidenceRoot,
    layout_contract: LayoutContract,
) -> bool {
    atlas.source_revision() == evidence.source_revision()
        && atlas.record_contract() == evidence.record_contract()
        && atlas.layout_contract() == layout_contract
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
    use std::num::{NonZeroU64, NonZeroUsize};

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
        AllocationClass, CanonicalRecordChunk, CapabilityRequirement, CompleteCoverage,
        ContractFingerprint, EvidenceChunk, FilteredRecordProvenance, SourceChunkId,
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
            row_scale_class: 0,
            stacking: ReadStackingMode::Stable,
            grouping: AlignmentGroupMode::None,
            group_order: AlignmentOrderDirection::Natural,
            sorting: AlignmentSortMode::Start,
            sort_order: AlignmentOrderDirection::Natural,
            sort_anchor: None,
            view_as_pairs: false,
            pair_length_bounds: TemplateLengthBounds::default(),
            promoted_template: None,
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

        assert_eq!(pressure.evicted_acquisitions, 1);
        assert!(pressure.charged > 0);
        assert_eq!(pressure.pinned, pressure.charged);
        assert_eq!(pressure.pinned_overage, pressure.charged);
        assert_eq!(store.acquisition_count(), 0);
        assert_eq!(
            current
                .select(GenomicInterval::new(ContigId::new(0), 1, 2).unwrap())
                .unwrap()
                .len(),
            1
        );
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
        assert_eq!(summaries.root().pin().unwrap().len(), 9);
        assert_eq!(summaries.root().filter_tiles().len(), 2);
        assert_eq!(summaries.root().filter_provenance_certificates().count(), 2);
        assert_eq!(
            summaries
                .root()
                .filter_provenance(GenomicInterval::new(ContigId::new(0), 0, 200).unwrap())
                .unwrap(),
            FilteredRecordProvenance::default()
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
