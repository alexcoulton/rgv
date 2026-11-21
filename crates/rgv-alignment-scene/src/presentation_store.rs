use std::{
    collections::{BTreeSet, HashMap, HashSet},
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
    sync::Arc,
};

use rgv_core::GenomicInterval;
use rgv_hts::ReferenceSlice;
use thiserror::Error;

use crate::page_identity::{page_reads_reference, reference_slice_stamp};
use crate::summary_page::required_summary_page_keys;
use crate::{
    AlignmentCacheResidencyClass, AlignmentCacheResidencyInventory, AlignmentPageIdentity,
    AlignmentPageKey, AlignmentPresentationScene, AlignmentPresentationSceneError,
    AlignmentRowAtlas, AlignmentSceneSnapshot, AlignmentSceneSnapshotError, AlignmentSemanticPage,
    AllocationId, AllocationLedger, AllocationOwner, AllocationRecordSnapshot, BoundedCacheTrim,
    CanonicalGenomicTile, CanonicalRowBlock, ExactSummaryRoot, JunctionAnnotationIndex,
    LedgerError, PresentationContract, ReferenceRevision, ReservationKind, RevisionFingerprint,
    SceneRevision, SemanticPageError, SummaryPageFamily, SummaryPageKey, SummarySemanticPage,
    SummarySemanticPageError,
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

/// Latest visible demand for one presentation owner. The store combines every owner before
/// assigning cache priority, so independent panels retain independent cameras without creating
/// independent cache policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlignmentPresentationOwnerDemand {
    pub owner: AlignmentPresentationOwner,
    pub visible_interval: GenomicInterval,
    pub visible_rows: Range<u32>,
}

#[derive(Debug)]
struct CachedReadPage {
    residency: AlignmentCacheResidencyClass,
    last_used_revision: u64,
    page: Arc<AlignmentSemanticPage>,
}

#[derive(Debug)]
struct CachedSummaryPage {
    residency: AlignmentCacheResidencyClass,
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
    /// Ordered LRU ownership directory. Page maps remain the identity lookup; this index makes
    /// bounded reclaim select its next legal entries without sorting or rescanning the cache.
    eviction_index: BTreeSet<(
        AlignmentCacheResidencyClass,
        u64,
        AllocationId,
        CachedPageLocation,
    )>,
    /// Source-worker preparation authority. These snapshots are reusable private candidates;
    /// they are never the desktop's observable presented epoch.
    prepared_by_owner: HashMap<AlignmentPresentationOwner, Arc<AlignmentSceneSnapshot>>,
    demanded_read_residency: HashMap<AlignmentPageKey, AlignmentCacheResidencyClass>,
    demanded_summary_residency: HashMap<SummaryPageKey, AlignmentCacheResidencyClass>,
    /// Content directory over `read_pages`: identity with its reference digest cleared, to
    /// every resident page with those records and rows as `(reference digest, page key)`.
    read_identities: HashMap<AlignmentPageIdentity, Vec<(RevisionFingerprint, AlignmentPageKey)>>,
    /// Requests already resolved for one pair of immutable roots: the key a request would
    /// build under, to the whole-reference-slice stamp it was resolved with and the key of the
    /// resident page that satisfies it. This is a memo of a completed identity proof; an entry
    /// whose page has left the cache is ignored.
    resolved_read_keys: HashMap<AlignmentPageKey, (RevisionFingerprint, AlignmentPageKey)>,
    read_page_reuse: AlignmentReadPageReuseStats,
    next_revision: u64,
}

/// Cumulative read-page resolution counters for one presentation store.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AlignmentReadPageReuseStats {
    /// Read pages requested by `prepare*` and `warm_read_row_block`.
    pub requests: u64,
    /// Requests answered by a page built under the same evidence and row-atlas revisions.
    pub same_revision_hits: u64,
    /// Requests answered by a page built under earlier revisions, after proving that these
    /// roots hand the builder identical inputs.
    pub cross_revision_reuses: u64,
    /// Requests which had to build a page.
    pub builds: u64,
}

impl AlignmentPresentationStore {
    #[must_use]
    pub fn new(ledger: AllocationLedger) -> Self {
        Self {
            ledger,
            read_pages: HashMap::new(),
            summary_pages: HashMap::new(),
            eviction_index: BTreeSet::new(),
            prepared_by_owner: HashMap::new(),
            demanded_read_residency: HashMap::new(),
            demanded_summary_residency: HashMap::new(),
            read_identities: HashMap::new(),
            resolved_read_keys: HashMap::new(),
            read_page_reuse: AlignmentReadPageReuseStats::default(),
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
        let page_capacity = genomic_tiles
            .len()
            .saturating_mul(row_blocks.len())
            .saturating_mul(read_families.len());
        let mut read_pages = Vec::with_capacity(page_capacity);
        let mut read_identities = Vec::with_capacity(page_capacity);
        let reference_stamp = reference_slice_stamp(reference);
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
                    let page = self.read_page(
                        key,
                        evidence,
                        atlas,
                        presentation,
                        contig_length,
                        reference,
                        reference_stamp,
                        AlignmentCacheResidencyClass::ForegroundVisible,
                        reservation_kind,
                        revision_number,
                    )?;
                    // The page was either built from these roots or resolved by reproducing
                    // its identity from them; the scene re-checks that binding per page.
                    read_identities.push(page.identity());
                    read_pages.push(page);
                }
            }
        }
        let reads = AlignmentPresentationScene::new_with_page_identities(
            revision,
            Arc::clone(evidence),
            Arc::clone(atlas),
            presentation,
            prepared_interval,
            prepared_rows,
            read_pages,
            Some(&read_identities),
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
                    AlignmentCacheResidencyClass::ForegroundVisible,
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
        let reference_stamp = reference_slice_stamp(reference);
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
                let builds_before = self.read_page_reuse.builds;
                self.read_page(
                    key,
                    evidence,
                    atlas,
                    presentation,
                    contig_length,
                    reference,
                    reference_stamp,
                    AlignmentCacheResidencyClass::DisposableSpeculation,
                    reservation_kind,
                    revision_number,
                )?;
                cache_misses = cache_misses
                    .saturating_add(usize::from(self.read_page_reuse.builds != builds_before));
            }
        }
        Ok(AlignmentReadPageWarm {
            row_block,
            page_occurrences,
            cache_misses,
        })
    }

    /// Returns whether every semantic page needed to present one canonical row block is still
    /// owned by this cache. This is the exact residency predicate used after partial reclaim: a
    /// missing page invalidates only its row block, never the whole lane directory.
    #[allow(clippy::too_many_arguments)]
    pub fn read_row_block_is_fully_cached(
        &self,
        evidence: &crate::EvidenceRoot,
        atlas: &AlignmentRowAtlas,
        presentation: PresentationContract,
        prepared_interval: GenomicInterval,
        row_block: CanonicalRowBlock,
        contig_length: u64,
        geometry_tile_bases: NonZeroU64,
    ) -> Result<bool, AlignmentPresentationStoreError> {
        validate_read_roots(evidence, atlas, prepared_interval)?;
        row_block.range(atlas.row_count())?;
        let genomic_tiles =
            CanonicalGenomicTile::covering(prepared_interval, geometry_tile_bases, contig_length)?;
        for genomic_tile in genomic_tiles {
            for family in presentation.required_page_families().iter().copied() {
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
                if self.resolved_read_page(key).is_some() {
                    continue;
                }
                // Not resolved for these exact roots yet: a page built from earlier roots
                // still counts when these roots reproduce its records and rows.
                let resident = AlignmentSemanticPage::probe_identity(
                    key,
                    evidence,
                    atlas,
                    presentation,
                    contig_length,
                )
                .is_ok_and(|identity| {
                    self.read_identities
                        .get(&identity)
                        .is_some_and(|pages| !pages.is_empty())
                });
                if !resident {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Cumulative read-page resolution counters: how many requests were answered from the
    /// cache, how many of those crossed an evidence or row-atlas revision, and how many built.
    #[must_use]
    pub const fn read_page_reuse_stats(&self) -> AlignmentReadPageReuseStats {
        self.read_page_reuse
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

    /// Exact distinct byte inventory for cache policy and diagnostics. This is intentionally a
    /// control/trace operation rather than a per-frame rendering query.
    pub fn cache_residency_inventory(
        &self,
    ) -> Result<AlignmentCacheResidencyInventory, AlignmentPresentationStoreError> {
        self.residency_inventory()
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
        self.rebuild_eviction_index();
        self.rebuild_read_identities();
        PresentationCacheDiscard {
            read_pages: read_pages_before.saturating_sub(self.read_pages.len()),
            summary_pages: summary_pages_before.saturating_sub(self.summary_pages.len()),
        }
    }

    /// Demotes semantic entries outside every active working-set island without discarding them.
    /// They remain useful as a return cache, but sort ahead of active-locus buffers during the
    /// next bounded reclaim pass.
    pub fn demote_cache_outside(
        &mut self,
        active: &[GenomicInterval],
    ) -> PresentationCacheDemotion {
        let mut read_pages = 0_usize;
        let mut summary_pages = 0_usize;
        for cached in self.read_pages.values_mut() {
            if !active
                .iter()
                .any(|target| cached.page.interval().overlaps(*target))
                && cached.residency != AlignmentCacheResidencyClass::RecentLocusReturnCache
            {
                cached.residency = AlignmentCacheResidencyClass::RecentLocusReturnCache;
                read_pages = read_pages.saturating_add(1);
            }
        }
        for cached in self.summary_pages.values_mut() {
            if !active
                .iter()
                .any(|target| cached.page.indexed_interval().overlaps(*target))
                && cached.residency != AlignmentCacheResidencyClass::RecentLocusReturnCache
            {
                cached.residency = AlignmentCacheResidencyClass::RecentLocusReturnCache;
                summary_pages = summary_pages.saturating_add(1);
            }
        }
        self.rebuild_eviction_index();
        PresentationCacheDemotion {
            read_pages,
            summary_pages,
        }
    }

    /// Reclassifies semantic cache ownership from the exact latest lane demands. Prepared scene
    /// pages form the nearby buffer; the subset intersecting each camera is foreground-visible.
    /// Same-island leftovers become cold, disjoint pages become return cache, and optional warmed
    /// pages remain disposable until an actual demand promotes them.
    pub fn reclassify_for_demands(
        &mut self,
        active_intervals: &[GenomicInterval],
        demands: &[AlignmentPresentationOwnerDemand],
        reclassify_unowned: bool,
    ) {
        let mut demanded_reads = HashMap::<AlignmentPageKey, AlignmentCacheResidencyClass>::new();
        let mut demanded_summaries = HashMap::<SummaryPageKey, AlignmentCacheResidencyClass>::new();
        for demand in demands {
            let Some(scene) = self.prepared_by_owner.get(&demand.owner) else {
                continue;
            };
            for page in scene.reads().pages() {
                let key = page.key();
                let residency = if page.interval().overlaps(demand.visible_interval)
                    && ranges_overlap(page.row_range(), demand.visible_rows.clone())
                {
                    AlignmentCacheResidencyClass::ForegroundVisible
                } else {
                    AlignmentCacheResidencyClass::ActiveNearBuffer
                };
                demanded_reads
                    .entry(key)
                    .and_modify(|current| *current = (*current).max(residency))
                    .or_insert(residency);
            }
            for page in scene.summary_pages() {
                let key = page.key();
                let residency = if page.indexed_interval().overlaps(demand.visible_interval) {
                    AlignmentCacheResidencyClass::ForegroundVisible
                } else {
                    AlignmentCacheResidencyClass::ActiveNearBuffer
                };
                demanded_summaries
                    .entry(key)
                    .and_modify(|current| *current = (*current).max(residency))
                    .or_insert(residency);
            }
        }

        if reclassify_unowned {
            for (key, cached) in &mut self.read_pages {
                cached.residency = demanded_reads.get(key).copied().unwrap_or_else(|| {
                    if active_intervals
                        .iter()
                        .any(|interval| cached.page.interval().overlaps(*interval))
                    {
                        if cached.residency == AlignmentCacheResidencyClass::DisposableSpeculation {
                            AlignmentCacheResidencyClass::DisposableSpeculation
                        } else {
                            AlignmentCacheResidencyClass::ActiveLocusCold
                        }
                    } else {
                        AlignmentCacheResidencyClass::RecentLocusReturnCache
                    }
                });
            }
            for (key, cached) in &mut self.summary_pages {
                cached.residency = demanded_summaries.get(key).copied().unwrap_or_else(|| {
                    if active_intervals
                        .iter()
                        .any(|interval| cached.page.indexed_interval().overlaps(*interval))
                    {
                        AlignmentCacheResidencyClass::ActiveLocusCold
                    } else {
                        AlignmentCacheResidencyClass::RecentLocusReturnCache
                    }
                });
            }
            self.rebuild_eviction_index();
        } else {
            let affected_reads = self
                .demanded_read_residency
                .keys()
                .chain(demanded_reads.keys())
                .copied()
                .collect::<HashSet<_>>();
            for key in affected_reads {
                let Some(cached) = self.read_pages.get_mut(&key) else {
                    continue;
                };
                let old_index = (
                    cached.residency,
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Read(key),
                );
                let next = demanded_reads.get(&key).copied().unwrap_or_else(|| {
                    if cached.residency == AlignmentCacheResidencyClass::DisposableSpeculation {
                        AlignmentCacheResidencyClass::DisposableSpeculation
                    } else {
                        AlignmentCacheResidencyClass::ActiveLocusCold
                    }
                });
                if next != cached.residency {
                    self.eviction_index.remove(&old_index);
                    cached.residency = next;
                    self.eviction_index.insert((
                        next,
                        cached.last_used_revision,
                        cached.page.allocation().id(),
                        CachedPageLocation::Read(key),
                    ));
                }
            }
            let affected_summaries = self
                .demanded_summary_residency
                .keys()
                .chain(demanded_summaries.keys())
                .copied()
                .collect::<HashSet<_>>();
            for key in affected_summaries {
                let Some(cached) = self.summary_pages.get_mut(&key) else {
                    continue;
                };
                let old_index = (
                    cached.residency,
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Summary(key),
                );
                let next = demanded_summaries
                    .get(&key)
                    .copied()
                    .unwrap_or(AlignmentCacheResidencyClass::ActiveLocusCold);
                if next != cached.residency {
                    self.eviction_index.remove(&old_index);
                    cached.residency = next;
                    self.eviction_index.insert((
                        next,
                        cached.last_used_revision,
                        cached.page.allocation().id(),
                        CachedPageLocation::Summary(key),
                    ));
                }
            }
        }
        self.demanded_read_residency = demanded_reads;
        self.demanded_summary_residency = demanded_summaries;
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
        let pressure = self.evict_inactive_until(budget)?;
        // Under `Bounded` the new total also shrinks this owner's share.
        self.trim_to_owner_budget(usize::MAX)?;
        Ok(pressure)
    }

    /// Keeps the semantic-page cache inside the presentation owner's share under
    /// [`AlignmentMemoryPolicy::Bounded`](crate::AlignmentMemoryPolicy) by releasing
    /// least-recently-used pages. It is a no-op under `Complete`.
    ///
    /// Never released: a page of any owner's prepared scene. Pages the cameras require but no
    /// scene has staged yet are the last candidates. `pressure` is set only when the content
    /// which cannot be released alone exceeds the owner's hard cap.
    #[allow(clippy::result_large_err)]
    pub fn trim_to_owner_budget(
        &mut self,
        maximum_entries: usize,
    ) -> Result<BoundedCacheTrim, AlignmentPresentationStoreError> {
        let mut trim = BoundedCacheTrim {
            complete: true,
            ..BoundedCacheTrim::default()
        };
        if !self.ledger.is_bounded() {
            return Ok(trim);
        }
        let owner = AllocationOwner::Presentation;
        let usage = self.ledger.owner_usage(owner)?;
        if usage.overage() == 0 {
            return Ok(trim);
        }
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
        let candidates = self
            .eviction_index
            .iter()
            .filter(|(_, _, _, candidate)| match candidate {
                CachedPageLocation::Read(key) => !protected_read_pages.contains(key),
                CachedPageLocation::Summary(key) => !protected_summary_pages.contains(key),
            })
            .take(maximum_entries.saturating_add(1))
            .copied()
            .collect::<Vec<_>>();
        let mut exhausted = true;
        for indexed @ (_, _, _, candidate) in candidates {
            if self.ledger.owner_usage(owner)?.overage() == 0 {
                break;
            }
            if trim.evicted_entries >= maximum_entries {
                exhausted = false;
                break;
            }
            self.eviction_index.remove(&indexed);
            match candidate {
                CachedPageLocation::Read(key) => {
                    self.forget_read_page(key);
                    trim.evicted_read_pages = trim.evicted_read_pages.saturating_add(1);
                }
                CachedPageLocation::Summary(key) => {
                    self.summary_pages.remove(&key);
                }
            }
            trim.evicted_entries = trim.evicted_entries.saturating_add(1);
        }
        let usage = self.ledger.owner_usage(owner)?;
        trim.complete = usage.overage() == 0 || exhausted;
        if trim.complete && usage.charged > usage.hard_cap {
            // What the prepared scenes need, plus whatever else survived the trim unpinned. A
            // scene another holder still presents is pinned too, but it retires when its
            // successor is presented and is not held against that successor.
            let prepared = self.prepared_by_owner.values().flat_map(|snapshot| {
                snapshot
                    .reads()
                    .pages()
                    .iter()
                    .map(|page| page.allocation())
                    .chain(
                        snapshot
                            .summary_pages()
                            .iter()
                            .map(|page| page.allocation()),
                    )
            });
            let cached = self
                .read_pages
                .values()
                .map(|cached| cached.page.allocation())
                .chain(
                    self.summary_pages
                        .values()
                        .map(|cached| cached.page.allocation()),
                );
            let mut seen = HashSet::<AllocationId>::new();
            let mut unevictable = 0_usize;
            for (needed, allocation) in prepared
                .map(|allocation| (true, allocation))
                .chain(cached.map(|allocation| (false, allocation)))
            {
                if !seen.insert(allocation.id()) {
                    continue;
                }
                let record = allocation.snapshot()?;
                if needed || record.pins == 0 {
                    unevictable = unevictable.saturating_add(record.charged_capacity);
                }
            }
            trim.pressure = self.ledger.owner_cap_pressure(owner, unevictable)?;
            trim.pressure_owner = trim.pressure.map(|_| owner);
        }
        Ok(trim)
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
        self.evict_inactive_batch(target_charged, usize::MAX)
    }

    /// Relinquishes at most `maximum_entries` inactive semantic cache entries. Candidate
    /// selection is one ordered pass per quantum, replacing the former full-cache scan for every
    /// removed page. The bounded form lets the scene worker interleave foreground requests with
    /// maintenance instead of disappearing into one uninterruptible reclaim loop.
    pub fn evict_inactive_batch(
        &mut self,
        target_charged: usize,
        maximum_entries: usize,
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
        let mut candidates = Vec::new();
        let mut candidates_inspected = 0_usize;
        for indexed @ (_, _, _, candidate) in &self.eviction_index {
            if candidates.len() >= maximum_entries {
                break;
            }
            candidates_inspected = candidates_inspected.saturating_add(1);
            let protected = match candidate {
                CachedPageLocation::Read(key) => protected_read_pages.contains(key),
                CachedPageLocation::Summary(key) => protected_summary_pages.contains(key),
            };
            if !protected {
                candidates.push(*indexed);
            }
        }
        for indexed @ (_, _, _, candidate) in candidates {
            if self.ledger.progress()?.charged <= target_charged {
                break;
            }
            self.eviction_index.remove(&indexed);
            match candidate {
                CachedPageLocation::Read(key) => {
                    self.forget_read_page(key);
                    evicted_read_pages = evicted_read_pages.saturating_add(1);
                }
                CachedPageLocation::Summary(key) => {
                    self.summary_pages.remove(&key);
                    evicted_summary_pages = evicted_summary_pages.saturating_add(1);
                }
            }
        }

        let progress = self.ledger.progress()?;
        let reclaim_complete = progress.charged <= target_charged
            || evicted_read_pages.saturating_add(evicted_summary_pages) < maximum_entries;
        let residency = if reclaim_complete && maximum_entries == usize::MAX {
            self.residency_inventory()?
        } else {
            AlignmentCacheResidencyInventory::default()
        };
        Ok(PresentationStorePressure {
            budget: target_charged,
            evicted_read_pages,
            evicted_summary_pages,
            candidates_inspected,
            reclaim_complete,
            residency,
            charged: progress.charged,
            pinned: progress.pinned,
            pinned_overage: progress.pinned_overage,
            remaining_overage: progress.charged.saturating_sub(target_charged),
        })
    }

    /// The resident page a request for `key` was last resolved to, if it is still cached.
    fn resolved_read_page(
        &self,
        key: AlignmentPageKey,
    ) -> Option<(RevisionFingerprint, AlignmentPageKey)> {
        self.resolved_read_keys
            .get(&key)
            .copied()
            .filter(|(_, canonical)| self.read_pages.contains_key(canonical))
    }

    fn touch_read_page(
        &mut self,
        canonical: AlignmentPageKey,
        requested_residency: AlignmentCacheResidencyClass,
        revision: u64,
    ) -> Result<Option<Arc<AlignmentSemanticPage>>, AlignmentPresentationStoreError> {
        let Some(cached) = self.read_pages.get_mut(&canonical) else {
            return Ok(None);
        };
        let previous = (
            cached.residency,
            cached.last_used_revision,
            cached.page.allocation().id(),
            CachedPageLocation::Read(canonical),
        );
        cached.last_used_revision = revision;
        cached.residency = cached.residency.max(requested_residency);
        self.ledger.touch(cached.page.allocation().id(), revision)?;
        self.eviction_index.remove(&previous);
        self.eviction_index.insert((
            cached.residency,
            revision,
            cached.page.allocation().id(),
            CachedPageLocation::Read(canonical),
        ));
        Ok(Some(Arc::clone(&cached.page)))
    }

    /// Drops one read page from the lookup maps and the content directory. The eviction index
    /// entry is the caller's to remove.
    fn forget_read_page(&mut self, key: AlignmentPageKey) -> Option<CachedReadPage> {
        let cached = self.read_pages.remove(&key)?;
        let identity = cached.page.identity().without_reference();
        if let Some(pages) = self.read_identities.get_mut(&identity) {
            pages.retain(|(_, resident)| *resident != key);
            if pages.is_empty() {
                self.read_identities.remove(&identity);
            }
        }
        Some(cached)
    }

    fn rebuild_read_identities(&mut self) {
        self.read_identities.clear();
        for (key, cached) in &self.read_pages {
            let identity = cached.page.identity();
            self.read_identities
                .entry(identity.without_reference())
                .or_default()
                .push((identity.content().reference(), *key));
        }
        let read_pages = &self.read_pages;
        self.resolved_read_keys
            .retain(|_, (_, canonical)| read_pages.contains_key(canonical));
    }

    fn remember_resolution(
        &mut self,
        key: AlignmentPageKey,
        reference_stamp: RevisionFingerprint,
        canonical: AlignmentPageKey,
    ) {
        self.resolved_read_keys
            .insert(key, (reference_stamp, canonical));
        // Every publication mints new request keys, so entries for superseded roots are never
        // asked for again. Bound the memo by the cache it describes; losing an entry only costs
        // one identity derivation.
        let limit = self
            .read_pages
            .len()
            .saturating_mul(8)
            .saturating_add(1_024);
        if self.resolved_read_keys.len() > limit {
            let read_pages = &self.read_pages;
            self.resolved_read_keys
                .retain(|_, (_, canonical)| read_pages.contains_key(canonical));
            if self.resolved_read_keys.len() > limit / 2 {
                self.resolved_read_keys.clear();
                self.resolved_read_keys
                    .insert(key, (reference_stamp, canonical));
            }
        }
    }

    /// Resolves one read page for these roots: a page already resolved for them, else a
    /// resident page from earlier roots whose content identity these roots reproduce, else a
    /// fresh build.
    #[allow(clippy::too_many_arguments)]
    fn read_page(
        &mut self,
        key: AlignmentPageKey,
        evidence: &Arc<crate::EvidenceRoot>,
        atlas: &Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        contig_length: u64,
        reference: Option<&ReferenceSlice>,
        reference_stamp: RevisionFingerprint,
        requested_residency: AlignmentCacheResidencyClass,
        reservation_kind: ReservationKind,
        revision: u64,
    ) -> Result<Arc<AlignmentSemanticPage>, AlignmentPresentationStoreError> {
        self.read_page_reuse.requests = self.read_page_reuse.requests.saturating_add(1);
        // Same immutable roots (the key names both revisions) and, for a page that reads the
        // reference, the same slice: the inputs are those of the earlier resolution.
        let reads_reference = page_reads_reference(key.family(), key.lod(), presentation.style);
        if let Some((stamp, canonical)) = self.resolved_read_page(key)
            && (!reads_reference || stamp == reference_stamp)
            && let Some(page) = self.touch_read_page(canonical, requested_residency, revision)?
        {
            self.count_read_page_hit(key, canonical);
            return Ok(page);
        }
        let inputs = AlignmentSemanticPage::collect_inputs(
            key,
            evidence,
            atlas,
            presentation,
            contig_length,
            reference,
            &self.ledger,
            reservation_kind,
        )?;
        let identity = inputs.identity();
        let resident = self
            .read_identities
            .get(&identity.without_reference())
            .and_then(|pages| {
                pages
                    .iter()
                    .find(|(reference, _)| *reference == identity.content().reference())
            })
            .map(|(_, canonical)| *canonical);
        if let Some(canonical) = resident
            && let Some(page) = self.touch_read_page(canonical, requested_residency, revision)?
        {
            debug_assert_eq!(page.identity(), identity);
            drop(inputs);
            self.remember_resolution(key, reference_stamp, canonical);
            self.count_read_page_hit(key, canonical);
            return Ok(page);
        }
        let inputs = inputs.keyed_by_reference();
        let request = key;
        let key = inputs.key();
        let page = AlignmentSemanticPage::build_from_inputs(
            inputs,
            evidence,
            atlas,
            presentation,
            reference,
            &self.ledger,
            reservation_kind,
            revision,
        )?;
        self.read_page_reuse.builds = self.read_page_reuse.builds.saturating_add(1);
        // A resident page under this exact key would have matched the identity above; replace
        // defensively rather than leave two directory entries for one key.
        if let Some(replaced) = self.forget_read_page(key) {
            self.eviction_index.remove(&(
                replaced.residency,
                replaced.last_used_revision,
                replaced.page.allocation().id(),
                CachedPageLocation::Read(key),
            ));
        }
        self.read_pages.insert(
            key,
            CachedReadPage {
                residency: requested_residency,
                last_used_revision: revision,
                page: Arc::clone(&page),
            },
        );
        self.read_identities
            .entry(identity.without_reference())
            .or_default()
            .push((identity.content().reference(), key));
        self.eviction_index.insert((
            requested_residency,
            revision,
            page.allocation().id(),
            CachedPageLocation::Read(key),
        ));
        self.remember_resolution(request, reference_stamp, key);
        Ok(page)
    }

    fn count_read_page_hit(&mut self, key: AlignmentPageKey, canonical: AlignmentPageKey) {
        let counter = if key.evidence_revision() == canonical.evidence_revision()
            && key.row_atlas_revision() == canonical.row_atlas_revision()
        {
            &mut self.read_page_reuse.same_revision_hits
        } else {
            &mut self.read_page_reuse.cross_revision_reuses
        };
        *counter = counter.saturating_add(1);
    }

    fn summary_page(
        &mut self,
        key: SummaryPageKey,
        summaries: &Arc<ExactSummaryRoot>,
        reference: Option<(ReferenceRevision, &ReferenceSlice, u64)>,
        annotation: Option<&JunctionAnnotationIndex>,
        requested_residency: AlignmentCacheResidencyClass,
        reservation_kind: ReservationKind,
        revision: u64,
    ) -> Result<Arc<SummarySemanticPage>, AlignmentPresentationStoreError> {
        if let Some(cached) = self.summary_pages.get_mut(&key) {
            let previous = (
                cached.residency,
                cached.last_used_revision,
                cached.page.allocation().id(),
                CachedPageLocation::Summary(key),
            );
            cached.last_used_revision = revision;
            cached.residency = cached.residency.max(requested_residency);
            self.ledger.touch(cached.page.allocation().id(), revision)?;
            self.eviction_index.remove(&previous);
            self.eviction_index.insert((
                cached.residency,
                revision,
                cached.page.allocation().id(),
                CachedPageLocation::Summary(key),
            ));
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
                residency: requested_residency,
                last_used_revision: revision,
                page: Arc::clone(&page),
            },
        );
        self.eviction_index.insert((
            requested_residency,
            revision,
            page.allocation().id(),
            CachedPageLocation::Summary(key),
        ));
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

    fn rebuild_eviction_index(&mut self) {
        self.eviction_index.clear();
        self.eviction_index
            .extend(self.read_pages.iter().map(|(key, cached)| {
                (
                    cached.residency,
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Read(*key),
                )
            }));
        self.eviction_index
            .extend(self.summary_pages.iter().map(|(key, cached)| {
                (
                    cached.residency,
                    cached.last_used_revision,
                    cached.page.allocation().id(),
                    CachedPageLocation::Summary(*key),
                )
            }));
    }

    fn residency_inventory(
        &self,
    ) -> Result<AlignmentCacheResidencyInventory, AlignmentPresentationStoreError> {
        let mut inventory = AlignmentCacheResidencyInventory::default();
        let mut allocations =
            HashMap::<AllocationId, (AlignmentCacheResidencyClass, AllocationRecordSnapshot)>::new(
            );
        for cached in self.read_pages.values() {
            inventory.observe_entry(cached.residency);
            let id = cached.page.allocation().id();
            if let Some((residency, _)) = allocations.get_mut(&id) {
                *residency = (*residency).max(cached.residency);
            } else {
                allocations.insert(id, (cached.residency, cached.page.allocation().snapshot()?));
            }
        }
        for cached in self.summary_pages.values() {
            inventory.observe_entry(cached.residency);
            let id = cached.page.allocation().id();
            if let Some((residency, _)) = allocations.get_mut(&id) {
                *residency = (*residency).max(cached.residency);
            } else {
                allocations.insert(id, (cached.residency, cached.page.allocation().snapshot()?));
            }
        }
        for (_, (residency, allocation)) in allocations {
            inventory.observe_distinct_allocation(residency, allocation);
        }
        Ok(inventory)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CachedPageLocation {
    Read(AlignmentPageKey),
    Summary(SummaryPageKey),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PresentationCacheDiscard {
    pub read_pages: usize,
    pub summary_pages: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PresentationCacheDemotion {
    pub read_pages: usize,
    pub summary_pages: usize,
}

impl PresentationCacheDemotion {
    #[must_use]
    pub const fn total(self) -> usize {
        self.read_pages.saturating_add(self.summary_pages)
    }
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
    /// Legal inactive candidates inspected when constructing this bounded reclaim quantum.
    pub candidates_inspected: usize,
    /// The target was reached or this store exhausted its legal candidates before the quantum
    /// limit. `false` means the caller should yield and resume another bounded quantum later.
    pub reclaim_complete: bool,
    pub residency: AlignmentCacheResidencyInventory,
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
