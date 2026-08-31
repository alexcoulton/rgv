use std::{
    collections::{HashMap, HashSet},
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
    sync::Arc,
};

use rgv_core::GenomicInterval;
use rgv_hts::ReferenceSlice;
use thiserror::Error;

use crate::summary_page::required_summary_page_keys;
use crate::{
    AlignmentPageKey, AlignmentPresentationScene, AlignmentPresentationSceneError,
    AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSceneSnapshotError, AlignmentSemanticPage,
    AllocationLedger, CanonicalGenomicTile, CanonicalRowBlock, ExactSummaryRoot,
    JunctionAnnotationIndex, LedgerError, PresentationContract, ReferenceRevision, ReservationKind,
    RevisionFingerprint, SceneRevision, SemanticPageError, SummaryPageFamily, SummaryPageKey,
    SummarySemanticPage, SummarySemanticPageError,
};

/// Stable publication slot within one source-owned presentation repository. Multiple panels can
/// retain independent current snapshots while sharing immutable semantic-page cache entries.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlignmentPresentationOwner(u64);

impl AlignmentPresentationOwner {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
struct CachedReadPage {
    last_used_revision: u64,
    page: Arc<AlignmentSemanticPage>,
}

#[derive(Debug)]
struct CachedSummaryPage {
    last_used_revision: u64,
    page: Arc<SummarySemanticPage>,
}

/// Headless current/candidate publication and semantic-page cache.
///
/// This store has no source I/O and never mutates a published snapshot. Failed candidates may
/// leave independently valid pages in the cache, but cannot alter `current`.
#[derive(Debug)]
pub struct AlignmentPresentationStore {
    ledger: AllocationLedger,
    read_pages: HashMap<AlignmentPageKey, CachedReadPage>,
    summary_pages: HashMap<SummaryPageKey, CachedSummaryPage>,
    /// Source-worker preparation authority. These snapshots are reusable private candidates;
    /// they are never the desktop's observable presented epoch.
    prepared_by_owner: HashMap<AlignmentPresentationOwner, Arc<AlignmentSceneSnapshot>>,
    next_revision: u64,
}

impl AlignmentPresentationStore {
    #[must_use]
    pub fn new(ledger: AllocationLedger) -> Self {
        Self {
            ledger,
            read_pages: HashMap::new(),
            summary_pages: HashMap::new(),
            prepared_by_owner: HashMap::new(),
            next_revision: 1,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        &mut self,
        evidence: &Arc<crate::EvidenceRoot>,
        summaries: &Arc<ExactSummaryRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        prepared_interval: GenomicInterval,
        prepared_rows: Range<u32>,
        contig_length: u64,
        geometry_tile_bases: NonZeroU64,
        row_block_rows: NonZeroU32,
        reference: Option<&ReferenceSlice>,
        reservation_kind: ReservationKind,
    ) -> Result<AlignmentSceneCandidate, AlignmentPresentationStoreError> {
        self.prepare_with_annotation(
            evidence,
            summaries,
            atlas,
            presentation,
            prepared_interval,
            prepared_rows,
            contig_length,
            geometry_tile_bases,
            row_block_rows,
            reference,
            None,
            reservation_kind,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_with_annotation(
        &mut self,
        evidence: &Arc<crate::EvidenceRoot>,
        summaries: &Arc<ExactSummaryRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        prepared_interval: GenomicInterval,
        prepared_rows: Range<u32>,
        contig_length: u64,
        geometry_tile_bases: NonZeroU64,
        row_block_rows: NonZeroU32,
        reference: Option<&ReferenceSlice>,
        annotation: Option<&Arc<JunctionAnnotationIndex>>,
        reservation_kind: ReservationKind,
    ) -> Result<AlignmentSceneCandidate, AlignmentPresentationStoreError> {
        validate_roots(
            evidence,
            summaries,
            atlas,
            prepared_interval,
            &prepared_rows,
        )?;
        if presentation.annotation
            != annotation
                .map(Arc::as_ref)
                .map(JunctionAnnotationIndex::revision)
        {
            return Err(AlignmentPresentationStoreError::AnnotationAuthorityMismatch);
        }
        let revision_number = self.take_revision()?;
        if let Some(annotation) = annotation {
            annotation.touch(&self.ledger, revision_number)?;
        }
        let revision = SceneRevision::new(RevisionFingerprint::new(0, revision_number));
        let genomic_tiles =
            CanonicalGenomicTile::covering(prepared_interval, geometry_tile_bases, contig_length)?;
        let row_blocks = covering_row_blocks(prepared_rows.clone(), row_block_rows);
        let read_families = presentation.required_page_families();
        let mut read_pages = Vec::with_capacity(
            genomic_tiles
                .len()
                .saturating_mul(row_blocks.len())
                .saturating_mul(read_families.len()),
        );
        for row_block in row_blocks {
            for genomic_tile in genomic_tiles.iter().copied() {
                for family in read_families.iter().copied() {
                    let key = AlignmentPageKey::new(
                        evidence.source_revision(),
                        evidence.revision(),
                        atlas.revision(),
                        atlas.layout_contract().fingerprint(),
                        presentation.page_fingerprint(family),
                        genomic_tile,
                        row_block,
                        family,
                        presentation.geometry_lod,
                    );
                    read_pages.push(self.read_page(
                        key,
                        evidence,
                        atlas,
                        presentation,
                        contig_length,
                        reference,
                        reservation_kind,
                        revision_number,
                    )?);
                }
            }
        }
        let reads = AlignmentPresentationScene::new(
            revision,
            Arc::clone(evidence),
            Arc::clone(atlas),
            presentation,
            prepared_interval,
            prepared_rows,
            read_pages,
            &self.ledger,
            reservation_kind,
            revision_number,
        )?;

        let summary_keys = required_summary_page_keys(
            summaries,
            prepared_interval,
            presentation.reference,
            annotation.map(Arc::as_ref),
        );
        let mut summary_pages = Vec::with_capacity(summary_keys.len());
        for key in summary_keys {
            summary_pages.push(
                self.summary_page(
                    key,
                    summaries,
                    presentation
                        .reference
                        .zip(reference)
                        .map(|(revision, reference)| (revision, reference, contig_length))
                        .filter(|_| key.family() == SummaryPageFamily::Alleles),
                    annotation.map(Arc::as_ref),
                    reservation_kind,
                    revision_number,
                )?,
            );
        }
        let snapshot = AlignmentSceneSnapshot::new_with_annotation(
            revision,
            reads,
            Arc::clone(summaries),
            annotation.cloned(),
            summary_pages,
            &self.ledger,
            reservation_kind,
            revision_number,
        )?;
        Ok(AlignmentSceneCandidate {
            prepared_interval,
            prepared_rows: snapshot.reads().prepared_rows(),
            snapshot,
        })
    }

    /// Materializes one canonical row block into the semantic-page cache without constructing or
    /// publishing a cumulative scene snapshot. This is the unit of optional vertical warm-up:
    /// foreground cameras later assemble their bounded snapshot from these independently keyed
    /// pages, while the currently presented scene remains byte-for-byte unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn warm_read_row_block(
        &mut self,
        evidence: &Arc<crate::EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        prepared_interval: GenomicInterval,
        row_block: CanonicalRowBlock,
        contig_length: u64,
        geometry_tile_bases: NonZeroU64,
        reference: Option<&ReferenceSlice>,
        reservation_kind: ReservationKind,
    ) -> Result<AlignmentReadPageWarm, AlignmentPresentationStoreError> {
        validate_read_roots(evidence, atlas, prepared_interval)?;
        row_block.range(atlas.row_count())?;
        let revision_number = self.take_revision()?;
        let genomic_tiles =
            CanonicalGenomicTile::covering(prepared_interval, geometry_tile_bases, contig_length)?;
        let read_families = presentation.required_page_families();
        let page_occurrences = genomic_tiles.len().saturating_mul(read_families.len());
        let mut cache_misses = 0_usize;
        for genomic_tile in genomic_tiles {
            for family in read_families.iter().copied() {
                let key = AlignmentPageKey::new(
                    evidence.source_revision(),
                    evidence.revision(),
                    atlas.revision(),
                    atlas.layout_contract().fingerprint(),
                    presentation.page_fingerprint(family),
                    genomic_tile,
                    row_block,
                    family,
                    presentation.geometry_lod,
                );
                cache_misses =
                    cache_misses.saturating_add(usize::from(!self.read_pages.contains_key(&key)));
                self.read_page(
                    key,
                    evidence,
                    atlas,
                    presentation,
                    contig_length,
                    reference,
                    reservation_kind,
                    revision_number,
                )?;
            }
        }
        Ok(AlignmentReadPageWarm {
            row_block,
            page_occurrences,
            cache_misses,
        })
    }

    pub fn stage(&mut self, candidate: AlignmentSceneCandidate) -> Arc<AlignmentSceneSnapshot> {
        self.stage_for(AlignmentPresentationOwner::default(), candidate)
    }

    /// Replaces one source worker's private prepared candidate without disturbing another panel.
    /// Product presentation remains a separate CPU/GPU transaction owned by the desktop.
    pub fn stage_for(
        &mut self,
        owner: AlignmentPresentationOwner,
        candidate: AlignmentSceneCandidate,
    ) -> Arc<AlignmentSceneSnapshot> {
        let snapshot = Arc::clone(&candidate.snapshot);
        self.prepared_by_owner.insert(owner, candidate.snapshot);
        snapshot
    }

    #[must_use]
    pub fn prepared(&self) -> Option<&Arc<AlignmentSceneSnapshot>> {
        self.prepared_for(AlignmentPresentationOwner::default())
    }

    #[must_use]
    pub fn prepared_for(
        &self,
        owner: AlignmentPresentationOwner,
    ) -> Option<&Arc<AlignmentSceneSnapshot>> {
        self.prepared_by_owner.get(&owner)
    }

    #[must_use]
    pub fn prepared_covers(&self, interval: GenomicInterval, rows: Range<u32>) -> bool {
        self.prepared_covers_for(AlignmentPresentationOwner::default(), interval, rows)
    }

    #[must_use]
    pub fn prepared_covers_for(
        &self,
        owner: AlignmentPresentationOwner,
        interval: GenomicInterval,
        rows: Range<u32>,
    ) -> bool {
        self.prepared_by_owner
            .get(&owner)
            .is_some_and(|current| current.covers(interval, rows))
    }

    /// Removes one publication slot. Existing external frame leases remain valid and charged.
    pub fn remove_owner(
        &mut self,
        owner: AlignmentPresentationOwner,
    ) -> Option<Arc<AlignmentSceneSnapshot>> {
        self.prepared_by_owner.remove(&owner)
    }

    #[must_use]
    pub fn owner_count(&self) -> usize {
        self.prepared_by_owner.len()
    }

    #[must_use]
    pub fn read_page_count(&self) -> usize {
        self.read_pages.len()
    }

    #[must_use]
    pub fn summary_page_count(&self) -> usize {
        self.summary_pages.len()
    }

    #[must_use]
    pub fn ledger(&self) -> AllocationLedger {
        self.ledger.clone()
    }

    /// Drops cached semantic pages which cannot contribute to a disjoint target. Published
    /// snapshots retain their own leases and pins, so removing a lookup entry can never make the
    /// current scene disappear or pretend that its live allocation was reclaimed.
    pub fn discard_disjoint_cache(
        &mut self,
        target: GenomicInterval,
        rows: Range<u32>,
    ) -> PresentationCacheDiscard {
        let read_pages_before = self.read_pages.len();
        let summary_pages_before = self.summary_pages.len();
        self.read_pages.retain(|_, cached| {
            cached.page.interval().overlaps(target)
                && ranges_overlap(cached.page.row_range(), rows.clone())
        });
        self.summary_pages
            .retain(|_, cached| cached.page.indexed_interval().overlaps(target));
        PresentationCacheDiscard {
            read_pages: read_pages_before.saturating_sub(self.read_pages.len()),
            summary_pages: summary_pages_before.saturating_sub(self.summary_pages.len()),
        }
    }

    /// Applies a lower shared allocation budget and relinquishes semantic-page cache ownership in
    /// true least-recently-used order. Staged worker scenes and committed desktop epochs retain
    /// their own `Arc`s and pins, so removing a lookup entry never destroys either authority. A page
    /// which remains pinned may release no bytes immediately, but relinquishing the cache's last
    /// outside `Arc` lets an atomic epoch replacement prove that it will retire on commit.
    pub fn reduce_budget(
        &mut self,
        budget: usize,
    ) -> Result<PresentationStorePressure, AlignmentPresentationStoreError> {
        self.ledger.set_budget(budget)?;
        self.evict_inactive_until(budget)
    }

    /// Releases inactive semantic cache ownership toward a shared-ledger headroom target without
    /// changing the user policy. This is the owner-local half of aggregate foreground reclaim:
    /// another owner may have encountered pressure even though this store still owns the oldest
    /// legal cache reference. Cache entries are removable even while an external scene pins their
    /// allocation; the scene remains exact and the ownership change can unlock retirement credit
    /// on the subsequent aggregate admission retry.
    pub fn evict_inactive_until(
        &mut self,
        target_charged: usize,
    ) -> Result<PresentationStorePressure, AlignmentPresentationStoreError> {
        let mut evicted_read_pages = 0_usize;
        let mut evicted_summary_pages = 0_usize;
        let protected_read_pages = self
            .prepared_by_owner
            .values()
            .flat_map(|snapshot| snapshot.reads().pages().iter().map(|page| page.key()))
            .collect::<HashSet<_>>();
        let protected_summary_pages = self
            .prepared_by_owner
            .values()
            .flat_map(|snapshot| snapshot.summary_pages().iter().map(|page| page.key()))
            .collect::<HashSet<_>>();

        loop {
            if self.ledger.progress()?.charged <= target_charged {
                break;
            }
            let Some(candidate) =
                self.oldest_cached_page(&protected_read_pages, &protected_summary_pages)
            else {
                break;
            };
            match candidate {
                CachedPageLocation::Read(key) => {
                    self.read_pages.remove(&key);
                    evicted_read_pages = evicted_read_pages.saturating_add(1);
                }
                CachedPageLocation::Summary(key) => {
                    self.summary_pages.remove(&key);
                    evicted_summary_pages = evicted_summary_pages.saturating_add(1);
                }
            }
        }

        let snapshot = self.ledger.snapshot()?;
        Ok(PresentationStorePressure {
            budget: target_charged,
            evicted_read_pages,
            evicted_summary_pages,
            charged: snapshot.charged,
            pinned: snapshot.pinned,
            pinned_overage: snapshot.pinned_overage,
            remaining_overage: snapshot.charged.saturating_sub(target_charged),
        })
    }

    fn read_page(
        &mut self,
        key: AlignmentPageKey,
        evidence: &Arc<crate::EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        contig_length: u64,
        reference: Option<&ReferenceSlice>,
        reservation_kind: ReservationKind,
        revision: u64,
    ) -> Result<Arc<AlignmentSemanticPage>, AlignmentPresentationStoreError> {
        if let Some(cached) = self.read_pages.get_mut(&key) {
            cached.last_used_revision = revision;
            self.ledger.touch(cached.page.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.page));
        }
        let page = AlignmentSemanticPage::build(
            key,
            evidence,
            atlas,
            presentation,
            contig_length,
            reference,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        self.read_pages.insert(
            key,
            CachedReadPage {
                last_used_revision: revision,
                page: Arc::clone(&page),
            },
        );
        Ok(page)
    }

    fn summary_page(
        &mut self,
        key: SummaryPageKey,
        summaries: &Arc<ExactSummaryRoot>,
        reference: Option<(ReferenceRevision, &ReferenceSlice, u64)>,
        annotation: Option<&JunctionAnnotationIndex>,
        reservation_kind: ReservationKind,
        revision: u64,
    ) -> Result<Arc<SummarySemanticPage>, AlignmentPresentationStoreError> {
        if let Some(cached) = self.summary_pages.get_mut(&key) {
            cached.last_used_revision = revision;
            self.ledger.touch(cached.page.allocation().id(), revision)?;
            return Ok(Arc::clone(&cached.page));
        }
        let page = SummarySemanticPage::build_with_dependencies(
            key,
            summaries,
            reference,
            annotation,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        self.summary_pages.insert(
            key,
            CachedSummaryPage {
                last_used_revision: revision,
                page: Arc::clone(&page),
            },
        );
        Ok(page)
    }

    fn take_revision(&mut self) -> Result<u64, AlignmentPresentationStoreError> {
        let revision = self.next_revision;
        self.next_revision = self
            .next_revision
            .checked_add(1)
            .ok_or(AlignmentPresentationStoreError::RevisionOverflow)?;
        Ok(revision)
    }

    fn oldest_cached_page(
        &self,
        protected_read_pages: &HashSet<AlignmentPageKey>,
        protected_summary_pages: &HashSet<SummaryPageKey>,
    ) -> Option<CachedPageLocation> {
        let read = self
            .read_pages
            .iter()
            .filter(|(key, _)| !protected_read_pages.contains(key))
            .min_by_key(|(_, cached)| (cached.last_used_revision, cached.page.allocation().id()))
            .map(|(key, cached)| {
                (
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Read(*key),
                )
            });
        let summary = self
            .summary_pages
            .iter()
            .filter(|(key, _)| !protected_summary_pages.contains(key))
            .min_by_key(|(_, cached)| (cached.last_used_revision, cached.page.allocation().id()))
            .map(|(key, cached)| {
                (
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Summary(*key),
                )
            });
        match (read, summary) {
            (Some(read), Some(summary)) => Some(if (read.0, read.1) <= (summary.0, summary.1) {
                read.2
            } else {
                summary.2
            }),
            (Some(read), None) => Some(read.2),
            (None, Some(summary)) => Some(summary.2),
            (None, None) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CachedPageLocation {
    Read(AlignmentPageKey),
    Summary(SummaryPageKey),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PresentationCacheDiscard {
    pub read_pages: usize,
    pub summary_pages: usize,
}

impl PresentationCacheDiscard {
    #[must_use]
    pub const fn total(self) -> usize {
        self.read_pages.saturating_add(self.summary_pages)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentationStorePressure {
    pub budget: usize,
    pub evicted_read_pages: usize,
    pub evicted_summary_pages: usize,
    pub charged: usize,
    pub pinned: usize,
    pub pinned_overage: usize,
    /// Charged memory still above the requested ceiling. This may exceed `pinned_overage` when a
    /// caller holds an unpinned page lease after its cache key has been removed.
    pub remaining_overage: usize,
}

impl PresentationStorePressure {
    #[must_use]
    pub const fn evicted_pages(self) -> usize {
        self.evicted_read_pages
            .saturating_add(self.evicted_summary_pages)
    }
}

#[derive(Debug)]
pub struct AlignmentSceneCandidate {
    prepared_interval: GenomicInterval,
    prepared_rows: Range<u32>,
    snapshot: Arc<AlignmentSceneSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentReadPageWarm {
    pub row_block: CanonicalRowBlock,
    /// Number of stable semantic page keys covered by this row block and horizontal horizon.
    pub page_occurrences: usize,
    /// Keys which required new semantic-page construction rather than a cache touch.
    pub cache_misses: usize,
}

impl AlignmentSceneCandidate {
    #[must_use]
    pub const fn prepared_interval(&self) -> GenomicInterval {
        self.prepared_interval
    }

    #[must_use]
    pub fn prepared_rows(&self) -> Range<u32> {
        self.prepared_rows.clone()
    }

    #[must_use]
    pub const fn snapshot(&self) -> &Arc<AlignmentSceneSnapshot> {
        &self.snapshot
    }
}

fn validate_roots(
    evidence: &crate::EvidenceRoot,
    summaries: &ExactSummaryRoot,
    atlas: &AlignmentRowAtlas,
    interval: GenomicInterval,
    rows: &Range<u32>,
) -> Result<(), AlignmentPresentationStoreError> {
    if summaries.evidence_revision() != evidence.revision()
        || summaries.source_revision() != evidence.source_revision()
        || summaries.record_contract() != evidence.record_contract()
        || !atlas.shares_stable_record_authority(evidence)
    {
        return Err(AlignmentPresentationStoreError::AuthorityMismatch);
    }
    if !evidence.coverage().covers(interval)
        || !summaries.prepared_interval().contains_interval(interval)
        || !atlas.interval().contains_interval(interval)
    {
        return Err(AlignmentPresentationStoreError::UnpreparedInterval(
            interval,
        ));
    }
    let row_count = u32::try_from(atlas.row_count())
        .map_err(|_| AlignmentPresentationStoreError::RowCoordinateOverflow)?;
    if rows.start > rows.end || rows.end > row_count {
        return Err(AlignmentPresentationStoreError::UnpreparedRows {
            start: rows.start,
            end: rows.end,
            row_count,
        });
    }
    Ok(())
}

fn validate_read_roots(
    evidence: &crate::EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    interval: GenomicInterval,
) -> Result<(), AlignmentPresentationStoreError> {
    // Row assignments are keyed by source-stable alignment handles, so a containing atlas may be
    // reused across compatible evidence publications. Requiring the atlas's historical evidence
    // revision to equal the current root contradicted `EvidenceStore::prepare_row_atlas` and made
    // every optional vertical warm-up fail after an otherwise valid contained-target reuse.
    if !atlas.shares_stable_record_authority(evidence) {
        return Err(AlignmentPresentationStoreError::AuthorityMismatch);
    }
    if !evidence.coverage().covers(interval) || !atlas.interval().contains_interval(interval) {
        return Err(AlignmentPresentationStoreError::UnpreparedInterval(
            interval,
        ));
    }
    Ok(())
}

fn covering_row_blocks(rows: Range<u32>, block_rows: NonZeroU32) -> Vec<CanonicalRowBlock> {
    if rows.is_empty() {
        // Empty camera ranges can occur exactly at the bottom boundary while scroll state is
        // being clamped to a newly published row extent. The presentation validator still asks
        // for one canonical block, so retain the block immediately before that boundary (or zero
        // at the top). Choosing block zero unconditionally made a legitimate deep-row camera fail
        // with `MissingMandatoryBlock`, while choosing `start / rows` can point beyond a perfectly
        // block-aligned atlas.
        return vec![CanonicalRowBlock::new(
            rows.start.saturating_sub(1) / block_rows.get(),
            block_rows,
        )];
    }
    let first = rows.start / block_rows.get();
    let last = rows.end.saturating_sub(1) / block_rows.get();
    (first..=last)
        .map(|coordinate| CanonicalRowBlock::new(coordinate, block_rows))
        .collect()
}

fn ranges_overlap(left: Range<u32>, right: Range<u32>) -> bool {
    left.start < right.end && right.start < left.end
}

#[derive(Debug, Error)]
pub enum AlignmentPresentationStoreError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Tile(#[from] crate::CanonicalTileError),
    #[error(transparent)]
    SemanticPage(#[from] SemanticPageError),
    #[error(transparent)]
    SummaryPage(#[from] SummarySemanticPageError),
    #[error(transparent)]
    ReadScene(#[from] AlignmentPresentationSceneError),
    #[error(transparent)]
    Snapshot(#[from] AlignmentSceneSnapshotError),
    #[error("evidence, exact summaries, and row atlas do not share one authority")]
    AuthorityMismatch,
    #[error("presentation annotation revision does not match the supplied annotation index")]
    AnnotationAuthorityMismatch,
    #[error("prepared interval {0:?} is not completely available from every authority root")]
    UnpreparedInterval(GenomicInterval),
    #[error("prepared row coordinates cannot be represented")]
    RowCoordinateOverflow,
    #[error("prepared rows {start}..{end} exceed the atlas row count {row_count}")]
    UnpreparedRows {
        start: u32,
        end: u32,
        row_count: u32,
    },
    #[error("presentation revision space exhausted")]
    RevisionOverflow,
}
