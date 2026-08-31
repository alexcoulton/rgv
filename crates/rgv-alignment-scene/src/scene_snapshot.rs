use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    num::NonZeroU64,
    sync::Arc,
};

use rgv_core::GenomicInterval;
use rgv_hts::AlignmentRecord;
use thiserror::Error;

use crate::summary_page::required_summary_page_keys;
use crate::{
    AlignmentExportError, AlignmentExportRecord, AlignmentHandle, AlignmentPresentationScene,
    AllocationClass, AllocationLease, AllocationLedger, CanonicalGenomicTile,
    ExactAlleleSiteEvidence, ExactPileupSiteEvidence, ExactSummaryPins, ExactSummaryRoot,
    JunctionAnnotationIndex, JunctionAnnotationPins, LedgerError, PinnedAllocationLease,
    ReservationKind, SceneRevision, SummaryPageFamily, SummaryPageKey, SummaryRevision,
    SummarySemanticComponent, SummarySemanticPage,
};

/// Scene-backed allele observation used by inspection and automation consumers. The base counts
/// and tile provenance come from the exact summary root; the optional reference base comes from
/// the immutable, reference-revision-keyed semantic page presented in the same snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentSceneAlleleSite {
    pub evidence: ExactAlleleSiteEvidence,
    pub reference_base: Option<u8>,
}

/// Scene-backed coverage/pileup fact. Exact depth is always present; allele observations and their
/// same-snapshot reference interpretation remain explicit optional capabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AlignmentScenePileupSite {
    pub evidence: ExactPileupSiteEvidence,
    pub reference_base: Option<u8>,
}

/// The complete immutable semantic authority for one alignment track revision.
///
/// Read geometry and every enabled exact derived lane enter and leave publication together. A
/// renderer, picker, inspector, or exporter should retain this object rather than independently
/// retaining a read scene and a summary root that could belong to different evidence revisions.
#[derive(Debug)]
pub struct AlignmentSceneSnapshot {
    revision: SceneRevision,
    reads: Arc<AlignmentPresentationScene>,
    summaries: Arc<ExactSummaryRoot>,
    annotation: Option<Arc<JunctionAnnotationIndex>>,
    summary_pages: Arc<[Arc<SummarySemanticPage>]>,
    summary_tile_index: HashMap<(SummaryPageFamily, CanonicalGenomicTile), usize>,
    summary_tile_bases: Option<NonZeroU64>,
    metadata_allocation: AllocationLease,
    _pins: AlignmentSceneSnapshotPins,
}

#[derive(Debug)]
struct AlignmentSceneSnapshotPins {
    _summaries: ExactSummaryPins,
    _annotation: Option<JunctionAnnotationPins>,
    _summary_pages: Vec<PinnedAllocationLease>,
    _metadata: PinnedAllocationLease,
}

impl AlignmentSceneSnapshot {
    pub fn new(
        revision: SceneRevision,
        reads: Arc<AlignmentPresentationScene>,
        summaries: Arc<ExactSummaryRoot>,
        summary_pages: Vec<Arc<SummarySemanticPage>>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentSceneSnapshotError> {
        Self::new_with_annotation(
            revision,
            reads,
            summaries,
            None,
            summary_pages,
            ledger,
            reservation_kind,
            publication_revision,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_annotation(
        revision: SceneRevision,
        reads: Arc<AlignmentPresentationScene>,
        summaries: Arc<ExactSummaryRoot>,
        annotation: Option<Arc<JunctionAnnotationIndex>>,
        mut summary_pages: Vec<Arc<SummarySemanticPage>>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentSceneSnapshotError> {
        validate_authority(
            revision,
            &reads,
            &summaries,
            annotation.as_deref(),
            &summary_pages,
        )?;
        summary_pages.sort_unstable_by_key(|page| page.key());
        let summary_tile_bases = summary_pages.first().map(|page| page.key().tile().bases());
        let mut summary_tile_index = HashMap::with_capacity(summary_pages.len());
        for (index, page) in summary_pages.iter().enumerate() {
            let key = page.key();
            assert!(
                summary_tile_index
                    .insert((key.family(), key.tile()), index)
                    .is_none(),
                "validated summary pages have unique family/tile identities"
            );
        }
        summary_tile_index.shrink_to_fit();
        let metadata_bytes =
            snapshot_metadata_bytes(summary_pages.len(), summary_tile_index.capacity());
        let mut reservation = ledger.reserve(
            AllocationClass::PresentationScene,
            metadata_bytes,
            reservation_kind,
        )?;
        summary_pages.shrink_to_fit();
        let metadata_allocation = reservation.register(metadata_bytes, publication_revision)?;
        let summary_pins = summaries.pin()?;
        let annotation_pins = annotation
            .as_ref()
            .map(|annotation| annotation.pin())
            .transpose()?;
        let summary_page_pins = summary_pages
            .iter()
            .map(|page| page.allocation().pin())
            .collect::<Result<Vec<_>, _>>()?;
        let metadata_pin = metadata_allocation.pin()?;
        Ok(Arc::new(Self {
            revision,
            reads,
            summaries,
            annotation,
            summary_pages: summary_pages.into(),
            summary_tile_index,
            summary_tile_bases,
            metadata_allocation,
            _pins: AlignmentSceneSnapshotPins {
                _summaries: summary_pins,
                _annotation: annotation_pins,
                _summary_pages: summary_page_pins,
                _metadata: metadata_pin,
            },
        }))
    }

    #[must_use]
    pub const fn revision(&self) -> SceneRevision {
        self.revision
    }

    #[must_use]
    pub const fn reads(&self) -> &Arc<AlignmentPresentationScene> {
        &self.reads
    }

    #[must_use]
    pub const fn summaries(&self) -> &Arc<ExactSummaryRoot> {
        &self.summaries
    }

    #[must_use]
    pub const fn annotation(&self) -> Option<&Arc<JunctionAnnotationIndex>> {
        self.annotation.as_ref()
    }

    #[must_use]
    pub fn summary_pages(&self) -> &[Arc<SummarySemanticPage>] {
        &self.summary_pages
    }

    /// Resolves one exact summary page without scanning the snapshot. This supports bounded GPU
    /// warm-up cursors whose authority was minted by an earlier atomic replacement.
    #[must_use]
    pub fn summary_page_for_key(&self, key: SummaryPageKey) -> Option<&Arc<SummarySemanticPage>> {
        self.summary_pages
            .binary_search_by_key(&key, |page| page.key())
            .ok()
            .map(|index| &self.summary_pages[index])
    }

    /// Selects pages by their complete semantic index rather than only their owner tile. A
    /// returned junction page can therefore begin before `interval` while owning an overlapping
    /// long junction.
    #[must_use]
    pub fn summary_pages_for(
        &self,
        family: SummaryPageFamily,
        interval: GenomicInterval,
    ) -> Vec<&Arc<SummarySemanticPage>> {
        let Some(interval) = interval.intersection(self.prepared_interval()) else {
            return Vec::new();
        };
        // Junction and pair dependencies can own facts whose indexed interval begins before
        // their canonical tile. Those sparse specialist families retain the general scan. The
        // high-frequency depth/allele/modification camera path uses direct canonical tile lookup.
        if matches!(
            family,
            SummaryPageFamily::Junctions | SummaryPageFamily::Pairs
        ) {
            return self
                .summary_pages
                .iter()
                .filter(|page| {
                    page.key().family() == family && page.indexed_interval().overlaps(interval)
                })
                .collect();
        }
        let Some(tile_bases) = self.summary_tile_bases else {
            return Vec::new();
        };
        let first = interval.start() / tile_bases.get();
        let last = interval.end().saturating_sub(1) / tile_bases.get();
        let capacity = usize::try_from(last.saturating_sub(first).saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(self.summary_pages.len());
        let mut pages = Vec::with_capacity(capacity);
        for coordinate in first..=last {
            let tile = CanonicalGenomicTile::new(interval.contig(), coordinate, tile_bases);
            if let Some(index) = self.summary_tile_index.get(&(family, tile)).copied() {
                let page = &self.summary_pages[index];
                if page.indexed_interval().overlaps(interval) {
                    pages.push(page);
                }
            }
        }
        pages.sort_unstable_by_key(|page| page.key());
        pages
    }

    /// Resolves the exact allele fact and its same-snapshot reference interpretation at one base.
    /// This method never constructs or consults a mutable viewport query.
    #[must_use]
    pub fn allele_site_at(&self, position: u64) -> Option<AlignmentSceneAlleleSite> {
        let evidence = self.summaries.allele_site_evidence_at(position)?;
        let (bin, reference_base) = self.allele_semantic_at(position)?;
        debug_assert_eq!(bin, evidence.bin);
        Some(AlignmentSceneAlleleSite {
            evidence,
            reference_base,
        })
    }

    /// Resolves exact coverage at one base even when the scene did not request sequence detail.
    /// If allele detail exists, its counts and reference interpretation come from the same
    /// immutable snapshot and are merged into the returned pileup fact.
    #[must_use]
    pub fn pileup_site_at(&self, position: u64) -> Option<AlignmentScenePileupSite> {
        let evidence = self.summaries.pileup_site_evidence_at(position)?;
        let reference_base = self
            .allele_semantic_at(position)
            .map(|(_, reference_base)| reference_base)
            .unwrap_or(None);
        Some(AlignmentScenePileupSite {
            evidence,
            reference_base,
        })
    }

    fn allele_semantic_at(&self, position: u64) -> Option<(rgv_hts::PileupBin, Option<u8>)> {
        let end = position.checked_add(1)?;
        let interval =
            GenomicInterval::new(self.prepared_interval().contig(), position, end).ok()?;
        let page = self
            .summary_pages_for(SummaryPageFamily::Alleles, interval)
            .into_iter()
            .next()?;
        let offset = position.checked_sub(page.interval().start())?;
        page.primitives().iter().find_map(|primitive| {
            (primitive.start_offset <= offset && offset < primitive.end_offset)
                .then_some(primitive.component)
                .and_then(|component| match component {
                    SummarySemanticComponent::Alleles {
                        bin,
                        reference_base,
                    } => Some((bin, reference_base)),
                    _ => None,
                })
        })
    }

    /// Maximum canonical summary tiles any camera with this genomic span can expose inside the
    /// prepared authority, including a boundary crossing. Used to size frame uniforms without
    /// treating all retained summary pages as simultaneously visible.
    #[must_use]
    pub fn summary_tile_envelope(&self, genomic_bases: u64) -> usize {
        let Some(tile_bases) = self.summary_tile_bases else {
            return 0;
        };
        if genomic_bases == 0 {
            return 0;
        }
        let tile_bases = tile_bases.get();
        let prepared = self.prepared_interval();
        let first = prepared.start() / tile_bases;
        let last = prepared.end().saturating_sub(1) / tile_bases;
        let prepared_tiles = last.saturating_sub(first).saturating_add(1);
        let camera_tiles = genomic_bases
            .saturating_sub(1)
            .checked_div(tile_bases)
            .unwrap_or(0)
            .saturating_add(2)
            .min(prepared_tiles);
        usize::try_from(camera_tiles)
            .unwrap_or(usize::MAX)
            .min(self.summary_pages.len())
    }

    #[must_use]
    pub fn summary_revision(&self) -> SummaryRevision {
        self.summaries.revision()
    }

    #[must_use]
    pub fn prepared_interval(&self) -> GenomicInterval {
        self.reads.prepared_interval()
    }

    #[must_use]
    pub const fn metadata_allocation(&self) -> &AllocationLease {
        &self.metadata_allocation
    }

    #[must_use]
    pub fn covers(&self, interval: GenomicInterval, rows: std::ops::Range<u32>) -> bool {
        self.reads.covers(interval, rows)
            && self
                .summaries
                .prepared_interval()
                .contains_interval(interval)
    }

    pub fn inspect_retained(
        &self,
        handle: AlignmentHandle,
    ) -> Result<Option<AlignmentRecord>, AlignmentExportError> {
        self.reads.inspect_retained(handle)
    }

    pub fn export_full(
        &self,
        interval: GenomicInterval,
    ) -> Result<Vec<AlignmentExportRecord>, AlignmentExportError> {
        self.reads.export_full(interval)
    }
}

fn validate_authority(
    revision: SceneRevision,
    reads: &AlignmentPresentationScene,
    summaries: &ExactSummaryRoot,
    annotation: Option<&JunctionAnnotationIndex>,
    pages: &[Arc<SummarySemanticPage>],
) -> Result<(), AlignmentSceneSnapshotError> {
    if revision != reads.revision()
        || reads.evidence().revision() != summaries.evidence_revision()
        || reads.evidence().source_revision() != summaries.source_revision()
        || reads.evidence().record_contract() != summaries.record_contract()
    {
        return Err(AlignmentSceneSnapshotError::AuthorityMismatch);
    }
    if reads.presentation().annotation != annotation.map(JunctionAnnotationIndex::revision) {
        return Err(AlignmentSceneSnapshotError::AnnotationAuthorityMismatch);
    }
    if !summaries
        .prepared_interval()
        .contains_interval(reads.prepared_interval())
    {
        return Err(AlignmentSceneSnapshotError::SummaryCoverageMismatch);
    }

    let mut actual = HashSet::<SummaryPageKey>::with_capacity(pages.len());
    for page in pages {
        let key = page.key();
        if !actual.insert(key) {
            return Err(AlignmentSceneSnapshotError::DuplicateSummaryPage(key));
        }
        if key
            != SummaryPageKey::new_with_dependencies(
                summaries,
                key.tile(),
                key.family(),
                reads.presentation().reference,
                annotation,
            )
        {
            return Err(AlignmentSceneSnapshotError::SummaryPageAuthorityMismatch(
                key,
            ));
        }
    }
    for expected in required_summary_page_keys(
        summaries,
        reads.prepared_interval(),
        reads.presentation().reference,
        annotation,
    ) {
        if !actual.contains(&expected) {
            return Err(AlignmentSceneSnapshotError::MissingMandatorySummaryPage(
                expected,
            ));
        }
    }
    Ok(())
}

const fn snapshot_metadata_bytes(summary_pages: usize, summary_index_capacity: usize) -> usize {
    size_of::<AlignmentSceneSnapshot>()
        .saturating_add(summary_pages.saturating_mul(size_of::<Arc<SummarySemanticPage>>()))
        .saturating_add(
            summary_index_capacity
                .saturating_mul(size_of::<((SummaryPageFamily, CanonicalGenomicTile), usize)>())
                .saturating_mul(2),
        )
}

#[derive(Debug, Error)]
pub enum AlignmentSceneSnapshotError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("read geometry and exact summaries do not share one scene authority")]
    AuthorityMismatch,
    #[error("exact summaries do not cover the complete prepared read interval")]
    SummaryCoverageMismatch,
    #[error("scene annotation revision does not match the read presentation contract")]
    AnnotationAuthorityMismatch,
    #[error("exact summary page {0:?} occurs more than once in one snapshot")]
    DuplicateSummaryPage(SummaryPageKey),
    #[error("exact summary page {0:?} belongs to another summary authority")]
    SummaryPageAuthorityMismatch(SummaryPageKey),
    #[error("mandatory exact summary page {0:?} is absent")]
    MissingMandatorySummaryPage(SummaryPageKey),
}
