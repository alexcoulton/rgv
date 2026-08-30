use std::{collections::HashSet, mem::size_of, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::AlignmentRecord;
use thiserror::Error;

use crate::{
    AlignmentExportError, AlignmentExportRecord, AlignmentHandle, AlignmentPresentationScene,
    AllocationClass, AllocationLease, AllocationLedger, ExactSummaryPins, ExactSummaryRoot,
    JunctionAnnotationIndex, JunctionAnnotationPins, LedgerError, PinnedAllocationLease,
    ReservationKind, SceneRevision, SummaryPageFamily, SummaryPageKey, SummaryRevision,
    SummarySemanticPage,
};

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
        let metadata_bytes = snapshot_metadata_bytes(summary_pages.len());
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

    #[must_use]
    pub fn summary_pages_for(
        &self,
        family: SummaryPageFamily,
        interval: GenomicInterval,
    ) -> Vec<&Arc<SummarySemanticPage>> {
        self.summary_pages
            .iter()
            .filter(|page| page.key().family() == family && page.interval().overlaps(interval))
            .collect()
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

    let required_families = required_summary_families(summaries);
    let mut actual = HashSet::<SummaryPageKey>::with_capacity(pages.len());
    for page in pages {
        let key = page.key();
        if !actual.insert(key) {
            return Err(AlignmentSceneSnapshotError::DuplicateSummaryPage(key));
        }
        if key
            != SummaryPageKey::new_with_annotation(summaries, key.tile(), key.family(), annotation)
        {
            return Err(AlignmentSceneSnapshotError::SummaryPageAuthorityMismatch(
                key,
            ));
        }
    }
    for tile in summaries
        .depth_tiles()
        .iter()
        .filter(|tile| tile.depth().interval().overlaps(reads.prepared_interval()))
        .map(|tile| tile.tile())
    {
        for family in required_families.iter().copied() {
            let expected = SummaryPageKey::new_with_annotation(summaries, tile, family, annotation);
            if !actual.contains(&expected) {
                return Err(AlignmentSceneSnapshotError::MissingMandatorySummaryPage(
                    expected,
                ));
            }
        }
    }
    Ok(())
}

fn required_summary_families(root: &ExactSummaryRoot) -> Vec<SummaryPageFamily> {
    let mut families = vec![
        SummaryPageFamily::Depth,
        SummaryPageFamily::Junctions,
        SummaryPageFamily::Pairs,
        SummaryPageFamily::FilterProvenance,
    ];
    if root.contract().allele_detail != crate::AlleleSummaryDetail::None {
        families.push(SummaryPageFamily::Alleles);
    }
    if root.contract().collect_modifications {
        families.push(SummaryPageFamily::Modifications);
    }
    families
}

const fn snapshot_metadata_bytes(summary_pages: usize) -> usize {
    size_of::<AlignmentSceneSnapshot>()
        .saturating_add(summary_pages.saturating_mul(size_of::<Arc<SummarySemanticPage>>()))
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
