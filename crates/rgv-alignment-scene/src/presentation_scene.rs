use std::{
    collections::{BTreeSet, HashMap, HashSet},
    mem::size_of,
    num::{NonZeroU32, NonZeroU64},
    ops::Range,
    sync::Arc,
};

use rgv_core::GenomicInterval;
use rgv_hts::{AlignmentRecord, ModificationCode, PackedRecordError, decode_modified_bases};
use thiserror::Error;

use crate::{
    AlignmentHandle, AlignmentPageFamily, AlignmentPageKey, AlignmentRowAtlas,
    AlignmentRowAtlasPins, AlignmentSemanticPage, AllocationClass, AllocationLease,
    AllocationLedger, CanonicalGenomicTile, CanonicalRowBlock, CapabilityRequirement, EvidencePins,
    EvidenceRoot, EvidenceSelectionError, LedgerError, PinnedAllocationLease, PresentationContract,
    ReservationKind, SceneRevision,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct AlignmentPageSpatialKey {
    tile: CanonicalGenomicTile,
    row_block: CanonicalRowBlock,
    family: AlignmentPageFamily,
}

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
    page_keys: Arc<[AlignmentPageKey]>,
    drawable_page_keys: Arc<[AlignmentPageKey]>,
    page_index: HashMap<AlignmentPageSpatialKey, usize>,
    page_tile_bases: NonZeroU64,
    page_row_block_rows: NonZeroU32,
    maximum_handle_primitives_per_page: usize,
    maximum_non_label_primitives_per_page: usize,
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
        let first_key = pages
            .first()
            .expect("validated interaction-ready scenes contain semantic pages")
            .key();
        let page_tile_bases = first_key.genomic_tile().bases();
        let page_row_block_rows = first_key.row_block().rows();
        let mut page_index = HashMap::with_capacity(pages.len());
        for (index, page) in pages.iter().enumerate() {
            let key = page.key();
            let spatial = AlignmentPageSpatialKey {
                tile: key.genomic_tile(),
                row_block: key.row_block(),
                family: key.family(),
            };
            assert!(
                page_index.insert(spatial, index).is_none(),
                "validated presentation pages have unique spatial identities"
            );
        }
        page_index.shrink_to_fit();
        let mut page_keys = pages.iter().map(|page| page.key()).collect::<Vec<_>>();
        let mut drawable_page_keys = pages
            .iter()
            .filter(|page| page.has_drawable_primitives())
            .map(|page| page.key())
            .collect::<Vec<_>>();
        // Warm one horizontal browsing band at a time. Visible pages are admitted atomically by
        // the foreground replacement; this ordering then fills every prepared genomic tile for
        // the nearest/top row block before proceeding deeper down the stack.
        let warm_order = |key: &AlignmentPageKey| {
            (
                key.row_block(),
                key.genomic_tile(),
                key.family(),
                key.lod(),
                *key,
            )
        };
        page_keys.sort_unstable_by_key(warm_order);
        drawable_page_keys.sort_unstable_by_key(warm_order);
        let page_keys = page_keys.into();
        let drawable_page_keys: Arc<[AlignmentPageKey]> = drawable_page_keys.into();
        let maximum_handle_primitives_per_page = pages
            .iter()
            .map(|page| page_maximum_handle_primitives(page))
            .max()
            .unwrap_or(0);
        let maximum_non_label_primitives_per_page = pages
            .iter()
            .map(|page| {
                page.primitives()
                    .iter()
                    .filter(|primitive| {
                        !matches!(primitive.component, crate::AlignmentComponent::Label { .. })
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);
        let metadata_bytes =
            scene_metadata_bytes(pages.len(), drawable_page_keys.len(), page_index.capacity());
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
            page_keys,
            drawable_page_keys,
            page_index,
            page_tile_bases,
            page_row_block_rows,
            maximum_handle_primitives_per_page,
            maximum_non_label_primitives_per_page,
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

    /// Exact sorted immutable page authority, constructed on the scene worker rather than during
    /// a UI-frame replacement.
    #[must_use]
    pub const fn page_keys(&self) -> &Arc<[AlignmentPageKey]> {
        &self.page_keys
    }

    /// Exact worker-built authority for semantic pages containing GPU-drawable primitives.
    /// Label-only and otherwise empty pages remain in `pages`/`page_keys` for CPU projection,
    /// picking, inspection, export, and vertical semantic completeness, but never acquire fake
    /// zero-byte GPU residency.
    #[must_use]
    pub const fn drawable_page_keys(&self) -> &Arc<[AlignmentPageKey]> {
        &self.drawable_page_keys
    }

    /// Largest non-label primitive count owned by one stable handle in one canonical page. The
    /// renderer combines this worker-built scalar with its exact camera-page envelope, avoiding a
    /// complete primitive scan and avoiding buffers sized for pages that cannot be visible at once.
    #[must_use]
    pub const fn maximum_handle_primitives_per_page(&self) -> usize {
        self.maximum_handle_primitives_per_page
    }

    /// Largest drawable primitive population in one canonical page. A highlight predicate may
    /// match every handle visible to a camera, so the renderer uses this worker-built bound to
    /// provision a viewport-sized overlay ring without scanning or retaining the whole scene as
    /// duplicate GPU geometry.
    #[must_use]
    pub const fn maximum_non_label_primitives_per_page(&self) -> usize {
        self.maximum_non_label_primitives_per_page
    }

    /// Conservative number of canonical pages which any camera of the supplied genomic/row span
    /// can expose inside this scene. Boundary crossings account for one additional tile/block;
    /// the result is clamped to the prepared authority.
    #[must_use]
    pub fn camera_page_envelope(&self, genomic_bases: u64, visible_rows: u32) -> usize {
        if genomic_bases == 0 || visible_rows == 0 || self.prepared_rows.is_empty() {
            return 0;
        }
        let tile_bases = self.page_tile_bases.get();
        let prepared_first_tile = self.prepared_interval.start() / tile_bases;
        let prepared_last_tile = self.prepared_interval.end().saturating_sub(1) / tile_bases;
        let prepared_tiles = prepared_last_tile
            .saturating_sub(prepared_first_tile)
            .saturating_add(1);
        let camera_tiles = genomic_bases
            .saturating_sub(1)
            .checked_div(tile_bases)
            .unwrap_or(0)
            .saturating_add(2)
            .min(prepared_tiles);

        let block_rows = self.page_row_block_rows.get();
        let prepared_first_block = self.prepared_rows.start / block_rows;
        let prepared_last_block = self.prepared_rows.end.saturating_sub(1) / block_rows;
        let prepared_blocks = prepared_last_block
            .saturating_sub(prepared_first_block)
            .saturating_add(1);
        let camera_blocks = visible_rows
            .saturating_sub(1)
            .checked_div(block_rows)
            .unwrap_or(0)
            .saturating_add(2)
            .min(prepared_blocks);
        usize::try_from(camera_tiles)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(camera_blocks).unwrap_or(usize::MAX))
            .saturating_mul(self.presentation.required_page_families().len())
            .min(self.pages.len())
    }

    /// Exact primitive count for the current stable-handle interaction set. A supplementary chain
    /// may contain more than three handles, so replacement planning takes the maximum of this
    /// indexed result and the baseline envelope. Cost is proportional to selected handles and
    /// the canonical pages those handles occupy, never the complete scene.
    #[must_use]
    pub fn interaction_overlay_instances(
        &self,
        interaction: &crate::AlignmentInteractionState,
    ) -> usize {
        let mut handles =
            HashSet::with_capacity(interaction.selected_chain().len().saturating_add(2));
        handles.extend(interaction.hovered());
        handles.extend(interaction.selected());
        handles.extend(interaction.selected_chain().iter().copied());
        handles.into_iter().fold(0_usize, |total, handle| {
            let Some(hit) = self.atlas.record(handle) else {
                return total;
            };
            let record = hit.record;
            let Ok(interval) = GenomicInterval::new(
                self.prepared_interval.contig(),
                record.start(),
                record.end().max(record.start().saturating_add(1)),
            ) else {
                return total;
            };
            let count = self
                .visible_pages(interval, record.row()..record.row().saturating_add(1))
                .into_iter()
                .flat_map(|page| page.primitives())
                .filter(|primitive| {
                    primitive.handle == handle
                        && !matches!(primitive.component, crate::AlignmentComponent::Label { .. })
                })
                .count();
            total.saturating_add(count)
        })
    }

    /// Resolves one immutable semantic page by its complete content identity. Scene construction
    /// keeps the page table sorted, so redraw-time cursors can advance through a previously
    /// minted authority without rebuilding or hashing the complete page manifest.
    #[must_use]
    pub fn page_for_key(&self, key: AlignmentPageKey) -> Option<&Arc<AlignmentSemanticPage>> {
        self.pages
            .binary_search_by_key(&key, |page| page.key())
            .ok()
            .map(|index| &self.pages[index])
    }

    #[must_use]
    pub const fn metadata_allocation(&self) -> &AllocationLease {
        &self.metadata_allocation
    }

    /// The metadata pin is exposed only inside this crate so the semantic epoch retirement
    /// planner can prove that it owns every ledger reference before granting replacement credit.
    /// Ordinary presentation consumers must retain the scene rather than manipulating its pins.
    pub(crate) const fn metadata_pin(&self) -> &PinnedAllocationLease {
        &self._pins._metadata
    }

    pub(crate) fn evidence_pins(&self) -> &[PinnedAllocationLease] {
        self._pins._evidence.allocations()
    }

    pub(crate) fn atlas_pins(&self) -> &[PinnedAllocationLease] {
        self._pins._atlas.allocations()
    }

    pub(crate) fn page_pins(&self) -> &[PinnedAllocationLease] {
        &self._pins._pages
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
        let Some(interval) = interval.intersection(self.prepared_interval) else {
            return Vec::new();
        };
        let rows = rows.start.max(self.prepared_rows.start)..rows.end.min(self.prepared_rows.end);
        if rows.start >= rows.end {
            return Vec::new();
        }
        let first_tile = interval.start() / self.page_tile_bases.get();
        let last_tile = interval.end().saturating_sub(1) / self.page_tile_bases.get();
        let first_row_block = rows.start / self.page_row_block_rows.get();
        let last_row_block = rows.end.saturating_sub(1) / self.page_row_block_rows.get();
        let families = self.presentation.required_page_families();
        let capacity = usize::try_from(last_tile.saturating_sub(first_tile).saturating_add(1))
            .unwrap_or(usize::MAX)
            .saturating_mul(
                usize::try_from(
                    last_row_block
                        .saturating_sub(first_row_block)
                        .saturating_add(1),
                )
                .unwrap_or(usize::MAX),
            )
            .saturating_mul(families.len());
        let mut visible = Vec::with_capacity(capacity.min(self.pages.len()));
        for tile_coordinate in first_tile..=last_tile {
            let tile =
                CanonicalGenomicTile::new(interval.contig(), tile_coordinate, self.page_tile_bases);
            for row_coordinate in first_row_block..=last_row_block {
                let row_block = CanonicalRowBlock::new(row_coordinate, self.page_row_block_rows);
                for family in families.iter().copied() {
                    let spatial = AlignmentPageSpatialKey {
                        tile,
                        row_block,
                        family,
                    };
                    if let Some(index) = self.page_index.get(&spatial).copied() {
                        visible.push(&self.pages[index]);
                    }
                }
            }
        }
        visible.sort_unstable_by_key(|page| page.key());
        visible
    }

    /// Materialises the payload retained by this immutable scene for one stable handle. Hover and
    /// selection use handles directly; this allocation-heavy path is reserved for inspection.
    pub fn inspect_retained(
        &self,
        handle: AlignmentHandle,
    ) -> Result<Option<AlignmentRecord>, AlignmentExportError> {
        if !self.evidence.contains_handle(handle) {
            return Ok(None);
        }
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
        let mut records = Vec::new();
        self.for_each_full(interval, |record| {
            records.push(record);
            true
        })?;
        Ok(records)
    }

    /// Visits the lossless retained population in exact source order without constructing a
    /// second all-record vector. This is the action-query path for complete automation scans and
    /// streaming exports over very deep scenes.
    pub fn for_each_full(
        &self,
        interval: GenomicInterval,
        visit: impl FnMut(AlignmentExportRecord) -> bool,
    ) -> Result<(), AlignmentExportError> {
        self.for_each_full_selection(interval, false, visit)
    }

    /// Visits the complete acquisition-eligible population even when the committed scene has a
    /// view filter. Callers must label this choice explicitly; ordinary export follows the
    /// committed filtered view.
    pub fn for_each_eligible_full(
        &self,
        interval: GenomicInterval,
        visit: impl FnMut(AlignmentExportRecord) -> bool,
    ) -> Result<(), AlignmentExportError> {
        self.for_each_full_selection(interval, true, visit)
    }

    fn for_each_full_selection(
        &self,
        interval: GenomicInterval,
        eligible: bool,
        mut visit: impl FnMut(AlignmentExportRecord) -> bool,
    ) -> Result<(), AlignmentExportError> {
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
        let selection = if eligible {
            self.evidence.select_eligible(interval)?
        } else {
            self.evidence.select(interval)?
        };
        for reference in selection.records() {
            let chunk = self
                .evidence
                .chunks()
                .get(reference.chunk as usize)
                .ok_or(AlignmentExportError::InvalidEvidenceRecord)?;
            let payload = chunk
                .payload()
                .ok_or(AlignmentExportError::FullPayloadUnavailable)?;
            if !visit(AlignmentExportRecord {
                handle: reference.handle,
                record: payload
                    .materialize_retained_record(chunk.chunk(), reference.record as usize)?,
            }) {
                break;
            }
        }
        Ok(())
    }

    /// Visits stable handle, read name, and flags without materializing sequence/quality/tag
    /// payload. Callers use this bounded view to prepare name/chain counts before one complete
    /// streaming record pass.
    pub fn for_each_identity(
        &self,
        interval: GenomicInterval,
        mut visit: impl FnMut(AlignmentHandle, &[u8], rgv_hts::AlignmentFlags),
    ) -> Result<(), AlignmentExportError> {
        if !self.prepared_interval.contains_interval(interval) {
            return Err(AlignmentExportError::UnpreparedInterval(interval));
        }
        let selection = self.evidence.select(interval)?;
        for (index, reference) in selection.records().iter().enumerate() {
            let record = selection
                .record_view(index)
                .ok_or(AlignmentExportError::InvalidEvidenceRecord)?;
            visit(reference.handle, record.read_name(), record.flags());
        }
        Ok(())
    }

    /// Discovers modified-base codes from a bounded prefix of the exact retained population.
    /// This action query exists for settings/property surfaces and never participates in frame
    /// rendering. Malformed individual tags are skipped, matching the legacy settings behavior.
    pub fn modification_codes(
        &self,
        maximum_records: usize,
        maximum_codes: usize,
    ) -> Result<Vec<ModificationCode>, AlignmentExportError> {
        if maximum_records == 0 || maximum_codes == 0 {
            return Ok(Vec::new());
        }
        let mut codes = BTreeSet::new();
        let selection = self.evidence.select(self.prepared_interval)?;
        for index in 0..selection.len().min(maximum_records) {
            let record = selection.materialize_retained_record(index)?;
            let Ok(calls) = decode_modified_bases(&record) else {
                continue;
            };
            for call in calls {
                codes.insert(call.code);
                if codes.len() == maximum_codes {
                    return Ok(codes.into_iter().collect());
                }
            }
        }
        Ok(codes.into_iter().collect())
    }

    #[must_use]
    pub fn contains_handle(&self, handle: AlignmentHandle) -> bool {
        self.evidence.contains_handle(handle)
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
        let ordered = self.ordered_handles();
        let index = ordered.iter().position(|candidate| *candidate == handle)?;
        match direction {
            AlignmentTraversalDirection::Previous => index
                .checked_sub(1)
                .and_then(|index| ordered.get(index).copied()),
            AlignmentTraversalDirection::Next => ordered.get(index.saturating_add(1)).copied(),
        }
    }

    /// First or last handle in the immutable row-atlas traversal order. This is the stable-handle
    /// equivalent of selecting the edge item in a query-local display list.
    #[must_use]
    pub fn edge_handle(&self, direction: AlignmentTraversalDirection) -> Option<AlignmentHandle> {
        let ordered = self.ordered_handles();
        match direction {
            AlignmentTraversalDirection::Previous => ordered.last().copied(),
            AlignmentTraversalDirection::Next => ordered.first().copied(),
        }
    }

    /// Resolves all physical records with a read name from the scene's exact evidence root.
    /// True duplicates remain separate stable handles. This action-oriented lookup is used for
    /// cross-panel read restoration and never becomes a render-page identity.
    #[must_use]
    pub fn handles_for_read_name(&self, read_name: &[u8]) -> Vec<AlignmentHandle> {
        self.matching_read_name_handles(read_name)
            .into_iter()
            .map(|(handle, _)| handle)
            .collect()
    }

    fn ordered_handles(&self) -> Vec<AlignmentHandle> {
        let mut ordered = self
            .atlas
            .records_for_rows(self.prepared_rows.clone(), self.prepared_interval)
            .into_iter()
            .map(|hit| hit.record.handle())
            .collect::<Vec<_>>();
        ordered.dedup();
        ordered
    }

    fn read_name_and_flags(
        &self,
        handle: AlignmentHandle,
    ) -> Option<(Vec<u8>, rgv_hts::AlignmentFlags)> {
        if !self.evidence.contains_handle(handle) {
            return None;
        }
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
                if self.evidence.contains_handle(handle)
                    && batch.read_name(index) == Some(read_name)
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
    if !atlas.shares_stable_record_authority(evidence) {
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
    let first_block = if prepared_rows.is_empty() {
        prepared_rows.start.saturating_sub(1) / block_rows
    } else {
        prepared_rows.start / block_rows
    };
    let last_block = prepared_rows.end.saturating_sub(1) / block_rows;
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

const fn scene_metadata_bytes(
    pages: usize,
    drawable_pages: usize,
    page_index_capacity: usize,
) -> usize {
    size_of::<AlignmentPresentationScene>()
        .saturating_add(pages.saturating_mul(size_of::<Arc<AlignmentSemanticPage>>()))
        .saturating_add(pages.saturating_mul(size_of::<AlignmentPageKey>()))
        .saturating_add(drawable_pages.saturating_mul(size_of::<AlignmentPageKey>()))
        // Hash-table bucket/control overhead is implementation-specific. Charge two key/value
        // widths per reserved bucket so the index is conservatively inside the scene budget.
        .saturating_add(
            page_index_capacity
                .saturating_mul(size_of::<(AlignmentPageSpatialKey, usize)>())
                .saturating_mul(2),
        )
}

fn page_maximum_handle_primitives(page: &AlignmentSemanticPage) -> usize {
    let mut by_handle = HashMap::<AlignmentHandle, usize>::new();
    for primitive in page
        .primitives()
        .iter()
        .filter(|primitive| !matches!(primitive.component, crate::AlignmentComponent::Label { .. }))
    {
        *by_handle.entry(primitive.handle).or_default() += 1;
    }
    by_handle.into_values().max().unwrap_or(0)
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
