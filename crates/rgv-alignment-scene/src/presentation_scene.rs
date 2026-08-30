use std::{collections::HashSet, mem::size_of, ops::Range, sync::Arc};

use rgv_core::GenomicInterval;
use rgv_hts::{AlignmentRecord, PackedRecordError};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentPageKey, AlignmentRowAtlas,
    AlignmentRowAtlasPins, AlignmentSemanticPage, AllocationClass, AllocationLease,
    AllocationLedger, CapabilityRequirement, EvidencePins, EvidenceRoot, EvidenceSelectionError,
    LedgerError, PinnedAllocationLease, PresentationContract, ReservationKind, SceneRevision,
};

/// One immutable, interaction-ready alignment presentation authority. Evidence, rows, mandatory
/// CPU semantic pages, and their pins enter and leave publication together.
#[derive(Debug)]
pub struct AlignmentPresentationScene {
    revision: SceneRevision,
    evidence: Arc<EvidenceRoot>,
    atlas: Arc<AlignmentRowAtlas>,
    presentation: PresentationContract,
    prepared_interval: GenomicInterval,
    prepared_rows: Range<u32>,
    pages: Arc<[Arc<AlignmentSemanticPage>]>,
    metadata_allocation: AllocationLease,
    _pins: AlignmentPresentationScenePins,
}

#[derive(Debug)]
struct AlignmentPresentationScenePins {
    _evidence: EvidencePins,
    _atlas: AlignmentRowAtlasPins,
    _pages: Vec<PinnedAllocationLease>,
    _metadata: PinnedAllocationLease,
}

impl AlignmentPresentationScene {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        revision: SceneRevision,
        evidence: Arc<EvidenceRoot>,
        atlas: Arc<AlignmentRowAtlas>,
        presentation: PresentationContract,
        prepared_interval: GenomicInterval,
        prepared_rows: Range<u32>,
        mut pages: Vec<Arc<AlignmentSemanticPage>>,
        ledger: &AllocationLedger,
        reservation_kind: ReservationKind,
        publication_revision: u64,
    ) -> Result<Arc<Self>, AlignmentPresentationSceneError> {
        validate_scene_dependencies(
            &evidence,
            &atlas,
            presentation,
            prepared_interval,
            &prepared_rows,
            &pages,
        )?;
        pages.sort_unstable_by_key(|page| page.key());
        let metadata_bytes = scene_metadata_bytes(pages.len());
        let mut reservation = ledger.reserve(
            AllocationClass::PresentationScene,
            metadata_bytes,
            reservation_kind,
        )?;
        pages.shrink_to_fit();
        let metadata_allocation = reservation.register(metadata_bytes, publication_revision)?;
        let evidence_pins = evidence.pin()?;
        let atlas_pins = atlas.pin()?;
        let page_pins = pages
            .iter()
            .map(|page| page.allocation().pin())
            .collect::<Result<Vec<_>, _>>()?;
        let metadata_pin = metadata_allocation.pin()?;
        Ok(Arc::new(Self {
            revision,
            evidence,
            atlas,
            presentation,
            prepared_interval,
            prepared_rows,
            pages: pages.into(),
            metadata_allocation,
            _pins: AlignmentPresentationScenePins {
                _evidence: evidence_pins,
                _atlas: atlas_pins,
                _pages: page_pins,
                _metadata: metadata_pin,
            },
        }))
    }

    #[must_use]
    pub const fn revision(&self) -> SceneRevision {
        self.revision
    }

    #[must_use]
    pub const fn evidence(&self) -> &Arc<EvidenceRoot> {
        &self.evidence
    }

    #[must_use]
    pub const fn atlas(&self) -> &Arc<AlignmentRowAtlas> {
        &self.atlas
    }

    #[must_use]
    pub const fn presentation(&self) -> PresentationContract {
        self.presentation
    }

    #[must_use]
    pub const fn prepared_interval(&self) -> GenomicInterval {
        self.prepared_interval
    }

    #[must_use]
    pub fn prepared_rows(&self) -> Range<u32> {
        self.prepared_rows.clone()
    }

    #[must_use]
    pub fn pages(&self) -> &[Arc<AlignmentSemanticPage>] {
        &self.pages
    }

    #[must_use]
    pub const fn metadata_allocation(&self) -> &AllocationLease {
        &self.metadata_allocation
    }

    #[must_use]
    pub fn covers(&self, interval: GenomicInterval, rows: Range<u32>) -> bool {
        self.prepared_interval.contains_interval(interval)
            && self.prepared_rows.start <= rows.start
            && self.prepared_rows.end >= rows.end
    }

    #[must_use]
    pub fn visible_pages(
        &self,
        interval: GenomicInterval,
        rows: Range<u32>,
    ) -> Vec<&Arc<AlignmentSemanticPage>> {
        self.pages
            .iter()
            .filter(|page| page.interval().overlaps(interval))
            .filter(|page| ranges_overlap(page.row_range(), rows.clone()))
            .collect()
    }

    /// Materialises the payload retained by this immutable scene for one stable handle. Hover and
    /// selection use handles directly; this allocation-heavy path is reserved for inspection.
    pub fn inspect_retained(
        &self,
        handle: AlignmentHandle,
    ) -> Result<Option<AlignmentRecord>, AlignmentExportError> {
        for chunk in self.evidence.chunks() {
            let Some(index) = chunk
                .chunk()
                .handles()
                .iter()
                .position(|candidate| *candidate == handle)
            else {
                continue;
            };
            return chunk.payload().map_or_else(
                || Ok(Some(chunk.chunk().batch().materialize_record(index)?)),
                |payload| {
                    Ok(Some(
                        payload.materialize_retained_record(chunk.chunk(), index)?,
                    ))
                },
            );
        }
        Ok(None)
    }

    /// Exports a lossless, exactly ordered population from the same evidence root used by
    /// rendering and picking. A tag-filtered sidecar is refused rather than called complete.
    pub fn export_full(
        &self,
        interval: GenomicInterval,
    ) -> Result<Vec<AlignmentExportRecord>, AlignmentExportError> {
        if !self.prepared_interval.contains_interval(interval) {
            return Err(AlignmentExportError::UnpreparedInterval(interval));
        }
        let payload = self.evidence.payload();
        if payload.sequence() != CapabilityRequirement::Required
            || payload.qualities() != CapabilityRequirement::Required
            || !payload.all_auxiliary_tags()
        {
            return Err(AlignmentExportError::FullPayloadUnavailable);
        }
        let selection = self.evidence.select(interval)?;
        let mut records = Vec::with_capacity(selection.len());
        for reference in selection.records() {
            let chunk = self
                .evidence
                .chunks()
                .get(reference.chunk as usize)
                .ok_or(AlignmentExportError::InvalidEvidenceRecord)?;
            let payload = chunk
                .payload()
                .ok_or(AlignmentExportError::FullPayloadUnavailable)?;
            records.push(AlignmentExportRecord {
                handle: reference.handle,
                record: payload
                    .materialize_retained_record(chunk.chunk(), reference.record as usize)?,
            });
        }
        Ok(records)
    }

    #[must_use]
    pub fn contains_handle(&self, handle: AlignmentHandle) -> bool {
        handle.source_revision() == self.evidence.source_revision()
            && self
                .evidence
                .chunks()
                .iter()
                .any(|chunk| chunk.chunk().handles().contains(&handle))
    }

    /// Resolves the primary/supplementary chain from the immutable evidence root. This runs only
    /// on selection/navigation actions; ordinary rendering continues to use page-local handles.
    #[must_use]
    pub fn supplementary_chain_handles(&self, handle: AlignmentHandle) -> Vec<AlignmentHandle> {
        let Some((read_name, _)) = self.read_name_and_flags(handle) else {
            return Vec::new();
        };
        let mut handles = self
            .matching_read_name_handles(&read_name)
            .into_iter()
            .filter(|(candidate, flags)| *candidate == handle || !flags.is_secondary())
            .map(|(candidate, _)| candidate)
            .collect::<Vec<_>>();
        handles.sort_unstable();
        handles.dedup();
        handles
    }

    /// Resolves physical mate records without using a query-local record index.
    #[must_use]
    pub fn mate_handles(&self, handle: AlignmentHandle) -> Vec<AlignmentHandle> {
        let Some((read_name, selected_flags)) = self.read_name_and_flags(handle) else {
            return Vec::new();
        };
        if !selected_flags.is_paired() {
            return Vec::new();
        }
        let mut handles = self
            .matching_read_name_handles(&read_name)
            .into_iter()
            .filter(|(candidate, flags)| {
                *candidate != handle
                    && flags.is_paired()
                    && !flags.is_secondary()
                    && match (
                        selected_flags.is_first_in_template(),
                        selected_flags.is_last_in_template(),
                    ) {
                        (true, false) => flags.is_last_in_template(),
                        (false, true) => flags.is_first_in_template(),
                        _ => true,
                    }
            })
            .map(|(candidate, _)| candidate)
            .collect::<Vec<_>>();
        handles.sort_unstable();
        handles.dedup();
        handles
    }

    #[must_use]
    pub fn adjacent_handle(
        &self,
        handle: AlignmentHandle,
        direction: AlignmentTraversalDirection,
    ) -> Option<AlignmentHandle> {
        let mut ordered = self
            .atlas
            .records_for_rows(self.prepared_rows.clone(), self.prepared_interval)
            .into_iter()
            .map(|hit| hit.record.handle())
            .collect::<Vec<_>>();
        ordered.dedup();
        let index = ordered.iter().position(|candidate| *candidate == handle)?;
        match direction {
            AlignmentTraversalDirection::Previous => index
                .checked_sub(1)
                .and_then(|index| ordered.get(index).copied()),
            AlignmentTraversalDirection::Next => ordered.get(index.saturating_add(1)).copied(),
        }
    }

    fn read_name_and_flags(
        &self,
        handle: AlignmentHandle,
    ) -> Option<(Vec<u8>, rgv_hts::AlignmentFlags)> {
        for chunk in self.evidence.chunks() {
            let Some(index) = chunk
                .chunk()
                .handles()
                .iter()
                .position(|candidate| *candidate == handle)
            else {
                continue;
            };
            let batch = chunk.chunk().batch();
            return Some((batch.read_name(index)?.to_vec(), batch.flags(index)?));
        }
        None
    }

    fn matching_read_name_handles(
        &self,
        read_name: &[u8],
    ) -> Vec<(AlignmentHandle, rgv_hts::AlignmentFlags)> {
        let mut output = Vec::new();
        for chunk in self.evidence.chunks() {
            let batch = chunk.chunk().batch();
            for (index, handle) in chunk.chunk().handles().iter().copied().enumerate() {
                if batch.read_name(index) == Some(read_name)
                    && let Some(flags) = batch.flags(index)
                {
                    output.push((handle, flags));
                }
            }
        }
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlignmentTraversalDirection {
    Previous,
    Next,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AlignmentExportRecord {
    pub handle: AlignmentHandle,
    pub record: AlignmentRecord,
}

fn validate_scene_dependencies(
    evidence: &EvidenceRoot,
    atlas: &AlignmentRowAtlas,
    presentation: PresentationContract,
    prepared_interval: GenomicInterval,
    prepared_rows: &Range<u32>,
    pages: &[Arc<AlignmentSemanticPage>],
) -> Result<(), AlignmentPresentationSceneError> {
    if atlas.source_revision() != evidence.source_revision()
        || atlas.record_contract() != evidence.record_contract()
    {
        return Err(AlignmentPresentationSceneError::AuthorityMismatch);
    }
    if !evidence.coverage().covers(prepared_interval)
        || !atlas.interval().contains_interval(prepared_interval)
    {
        return Err(AlignmentPresentationSceneError::UnpreparedEvidence);
    }
    let row_count = u32::try_from(atlas.row_count())
        .map_err(|_| AlignmentPresentationSceneError::RowCoordinateOverflow)?;
    if prepared_rows.start > prepared_rows.end || prepared_rows.end > row_count {
        return Err(AlignmentPresentationSceneError::InvalidPreparedRows {
            start: prepared_rows.start,
            end: prepared_rows.end,
            row_count,
        });
    }
    if pages.is_empty() {
        return Err(AlignmentPresentationSceneError::MissingMandatoryPages);
    }
    let mut keys = HashSet::<AlignmentPageKey>::with_capacity(pages.len());
    let required_families = presentation.required_page_families();
    let first = pages[0].key();
    for page in pages {
        let key = page.key();
        if !keys.insert(key) {
            return Err(AlignmentPresentationSceneError::DuplicatePage(key));
        }
        if key.source_revision() != evidence.source_revision()
            || key.evidence_revision() != evidence.revision()
            || key.row_atlas_revision() != atlas.revision()
            || key.layout() != atlas.layout_contract().fingerprint()
            || key.presentation() != presentation.page_fingerprint(key.family())
            || key.lod() != presentation.geometry_lod
            || !required_families.contains(&key.family())
        {
            return Err(AlignmentPresentationSceneError::PageContractMismatch(key));
        }
        if key.genomic_tile().bases() != first.genomic_tile().bases()
            || key.row_block().rows() != first.row_block().rows()
        {
            return Err(AlignmentPresentationSceneError::MixedPageSchemes);
        }
    }
    for family in required_families {
        validate_mandatory_coverage(prepared_interval, prepared_rows, pages, first, family)?;
    }
    Ok(())
}

fn validate_mandatory_coverage(
    prepared_interval: GenomicInterval,
    prepared_rows: &Range<u32>,
    pages: &[Arc<AlignmentSemanticPage>],
    first: AlignmentPageKey,
    family: AlignmentPageFamily,
) -> Result<(), AlignmentPresentationSceneError> {
    let block_rows = first.row_block().rows().get();
    let first_block = prepared_rows.start / block_rows;
    let last_block = if prepared_rows.is_empty() {
        first_block
    } else {
        prepared_rows.end.saturating_sub(1) / block_rows
    };
    for block in first_block..=last_block {
        let mut intervals = pages
            .iter()
            .filter(|page| page.key().row_block().coordinate() == block)
            .filter(|page| page.key().family() == family)
            .map(|page| page.interval())
            .filter(|interval| interval.contig() == prepared_interval.contig())
            .collect::<Vec<_>>();
        intervals.sort_unstable_by_key(|interval| (interval.start(), interval.end()));
        let mut cursor = prepared_interval.start();
        for interval in intervals {
            if interval.end() <= cursor || interval.start() > cursor {
                continue;
            }
            cursor = cursor.max(interval.end());
            if cursor >= prepared_interval.end() {
                break;
            }
        }
        if cursor < prepared_interval.end() {
            return Err(AlignmentPresentationSceneError::MissingMandatoryBlock {
                family,
                row_block: block,
                first_missing_base: cursor,
            });
        }
    }
    Ok(())
}

const fn ranges_overlap(left: Range<u32>, right: Range<u32>) -> bool {
    left.start < right.end && left.end > right.start
}

const fn scene_metadata_bytes(pages: usize) -> usize {
    size_of::<AlignmentPresentationScene>()
        .saturating_add(pages.saturating_mul(size_of::<Arc<AlignmentSemanticPage>>()))
}

#[derive(Debug, Error)]
pub enum AlignmentPresentationSceneError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("presentation evidence and row atlas do not share one authority")]
    AuthorityMismatch,
    #[error("presentation target is not covered by both evidence and row atlas")]
    UnpreparedEvidence,
    #[error("presentation row coordinate cannot be represented")]
    RowCoordinateOverflow,
    #[error("prepared row range {start}..{end} is outside an atlas with {row_count} rows")]
    InvalidPreparedRows {
        start: u32,
        end: u32,
        row_count: u32,
    },
    #[error("an interaction-ready scene requires at least one mandatory semantic page")]
    MissingMandatoryPages,
    #[error("semantic page {0:?} occurs more than once in one presentation scene")]
    DuplicatePage(AlignmentPageKey),
    #[error("semantic page {0:?} has a stale or incompatible authority contract")]
    PageContractMismatch(AlignmentPageKey),
    #[error("one presentation scene cannot mix genomic or row page schemes")]
    MixedPageSchemes,
    #[error(
        "mandatory {family:?} row block {row_block} has no semantic page covering base {first_missing_base}"
    )]
    MissingMandatoryBlock {
        family: AlignmentPageFamily,
        row_block: u32,
        first_missing_base: u64,
    },
}

#[derive(Debug, Error)]
pub enum AlignmentExportError {
    #[error("export interval {0:?} is outside the immutable prepared scene")]
    UnpreparedInterval(GenomicInterval),
    #[error("lossless export requires sequence, qualities, and all auxiliary tags")]
    FullPayloadUnavailable,
    #[error("scene evidence contains an invalid record reference")]
    InvalidEvidenceRecord,
    #[error(transparent)]
    EvidenceSelection(#[from] EvidenceSelectionError),
    #[error(transparent)]
    PackedRecord(#[from] PackedRecordError),
}
